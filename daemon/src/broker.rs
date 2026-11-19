//! The broker: one method table, one consent check, one audit entry per capability call.
//!
//! Requests arrive as one JSON object per line ([`crate::protocol`]). Every method that touches a
//! capability or a grant goes through [`Broker::audited`], which writes the audit entry *after* the
//! work and *before* the response — a call the broker cannot record is a call it does not report as
//! done. `ping`, `capabilities`, `consent.list` and `audit.tail` are read-only views and are not
//! audit entries themselves.
//!
//! Every capability follows the same four steps, in this order: validate the parameters (a refusal
//! here touches nothing), check a live grant ([`Broker::require_grant`]), do the work, and only
//! then spend a `once` grant ([`Broker::charge`]). A call that never happened therefore never burns
//! consent.

use crate::VERSION;
use crate::act::{Actor, Chord, FocusedWindow};
use crate::audit::{AuditEntry, AuditLog, DEFAULT_TAIL_LINES, GrantLabel, MAX_TAIL_LINES, Outcome};
use crate::caps;
use crate::capture::{self, Capturer, Target, ToolSet};
use crate::clock::{self, SharedClock};
use crate::error::{ApiError, ErrorCode};
use crate::exec::{CommandRunner, one_line};
use crate::grants::{Grant, GrantMode, Ledger, MAX_TTL_SECONDS, MIN_TTL_SECONDS};
use crate::paths::Paths;
use crate::protocol::{ParseOutcome, Request, Response};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use time::OffsetDateTime;

/// What an `act.type` confirmation says: which window, then the text in full.
///
/// No quotes around the payload: they would be characters the person did not
/// approve, and a text containing a quote would make the summary ambiguous about
/// where it ends. The text goes on its own line, verbatim.
fn type_summary(window: &crate::act::FocusedWindow, text: &str) -> String {
    format!("Type into {}:\n{text}", window_label(window))
}

/// What an `act.clipboard` write says: the text, in full, on its own line.
fn clipboard_summary(text: &str) -> String {
    format!("Copy to the clipboard:\n{text}")
}

/// What a `web.search` confirmation says: the query, in full.
fn search_summary(query: &str) -> String {
    format!("Search the web for: {query}")
}

/// The window a confirmation is about: the app class the audit keeps, and the title
/// the person recognises. Two terminals of the same kind are otherwise identical in
/// the card, and the title is what tells them apart.
fn window_label(window: &crate::act::FocusedWindow) -> String {
    window_label_parts(&window.class, &window.title)
}

/// The same, for the capture path, which carries its own window facts.
fn window_label_parts(class: &str, title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        class.to_string()
    } else {
        format!("{} — \"{}\"", class, crate::exec::one_line(title, 80))
    }
}

/// What a `shell.run` confirmation says: the label the person recognises *and* the
/// program with its arguments, because a label is a wish and the program is the
/// operation.
fn shell_run_summary(action: &str) -> Result<String, ApiError> {
    let (program, args) = crate::act::shell_action_program(action)
        .ok_or_else(|| ApiError::internal(format!("the action {action} has no program")))?;
    Ok(format!(
        "{}: {}",
        crate::act::shell_action_label(action),
        crate::act::program_line(program, args)
    ))
}

/// What a `web.fetch` confirmation says: the whole address. The path and the query
/// are what is fetched; an approval that omitted them would authorise an unknown
/// page. The audit still keeps only the host.
fn fetch_summary(url: &str) -> String {
    format!("Read: {url}")
}

/// The scope trust mints its standing grants with. Nothing else uses it, which
/// is what makes `trust off` able to take back exactly those and nothing more.
const TRUST_SCOPE: &str = "trust";

/// How long a pending confirmation waits for an answer before it is abandoned.
const PENDING_WAIT_SECONDS: u64 = 120;
/// How long a key-press approval lasts, and in which window. This is the
/// ledger's minimum TTL (60 s): short enough to be a task, long enough for
/// Tab/arrows/Enter navigation without a prompt per key.
const KEY_RUN_SECONDS: u64 = 60;
/// How long an unused one-call approval stays on disk.
const APPROVAL_ONCE_TTL_SECONDS: u64 = 300;

/// What an approval mints when the person says yes.
#[derive(Debug, Clone)]
enum AllowPolicy {
    /// Exactly one call with this capability.
    Once,
    /// Key presses in one window for a few seconds.
    Run {
        seconds: u64,
        target: Option<String>,
    },
}

/// One confirmation the card has not answered yet.
#[derive(Debug, Clone)]
struct PendingRequest {
    id: String,
    method: String,
    capability: String,
    summary: String,
    created_unix: i64,
    decision: Option<bool>,
}

/// What the broker needs from the outside world, injected so tests need no
/// compositor, no clock and no tools on the PATH.
pub struct BrokerConfig {
    /// Socket and state locations.
    pub paths: Paths,
    /// Where "now" comes from.
    pub clock: SharedClock,
    /// How external tools are run.
    pub runner: Arc<dyn CommandRunner>,
    /// Which optional tools exist.
    pub tools: ToolSet,
}

impl std::fmt::Debug for BrokerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrokerConfig")
            .field("paths", &self.paths)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

/// The daemon's state: paths, clock, tool runner, the consent ledger and the audit log.
pub struct Broker {
    paths: Paths,
    clock: SharedClock,
    capturer: Capturer,
    actor: Actor,
    audit: AuditLog,
    ledger: Mutex<Ledger>,
    pending: Mutex<BTreeMap<String, PendingRequest>>,
    /// Bumped by `consent.revoke_run`; a decision that crosses a bump cannot mint.
    run_epoch: AtomicU64,
    /// The pictures the last search came with, and when it happened. The card
    /// shows them under the answer; nothing else reads this.
    last_media: Mutex<Option<LastSearch>>,
    pid: u32,
    started_at: OffsetDateTime,
}

/// What the last search showed, and when. In memory only, never audited: a
/// thumbnail URL is content about what the person asked.
#[derive(Debug, Clone)]
struct LastSearch {
    at: i64,
    media: Vec<(String, String)>,
}

impl std::fmt::Debug for Broker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Broker")
            .field("paths", &self.paths)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// The facts an audit entry is built from, filled in as a call proceeds.
///
/// Nothing here is content: counts, sizes, a class and a direction, never the text typed, the
/// clipboard body or the chord.
#[derive(Debug, Default)]
struct Trace {
    capability: Option<String>,
    grant: GrantLabel,
    target: Option<String>,
    app: Option<String>,
    geometry: Option<String>,
    path: Option<String>,
    bytes: Option<u64>,
    chars: Option<u64>,
    key_count: Option<u64>,
    mode: Option<String>,
    action: Option<String>,
}

impl Broker {
    /// Build the broker: create the state tree and load the ledger.
    pub fn new(config: BrokerConfig) -> Result<Self, ApiError> {
        config.paths.ensure_state()?;
        let ledger = Ledger::load(&config.paths.grants_file())?;
        let pending = Mutex::new(BTreeMap::new());
        let started_at = config.clock.now();
        let audit = AuditLog::new(config.paths.audit_dir(), Arc::clone(&config.clock));
        let actor = Actor::new(Arc::clone(&config.runner));
        let capturer = Capturer::new(config.runner, config.tools);
        Ok(Self {
            paths: config.paths,
            clock: config.clock,
            capturer,
            actor,
            audit,
            ledger: Mutex::new(ledger),
            pending,
            run_epoch: AtomicU64::new(0),
            last_media: Mutex::new(None),
            pid: std::process::id(),
            started_at,
        })
    }

    /// Where this broker keeps its files.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The daemon's pid.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Handle one request line and return the response line (no newline).
    pub fn handle_line(&self, line: &[u8]) -> String {
        match crate::protocol::parse_request(line) {
            ParseOutcome::Request(request) => self.handle(request).to_line(),
            ParseOutcome::BadJson(message) => {
                Response::err(None, &ApiError::new(ErrorCode::BadJson, message)).to_line()
            }
            ParseOutcome::BadRequest(message) => {
                Response::err(None, &ApiError::new(ErrorCode::BadRequest, message)).to_line()
            }
        }
    }

    /// Handle one parsed request.
    pub fn handle(&self, request: Request) -> Response {
        let id = request.id.clone();
        let now = self.clock.now();
        // Trusted machine: keep a standing grant for everything, so no call has to
        // ask. Doing it here means flipping the switch takes effect on the next
        // call, with no restart and no stale grant.
        // A failure here is not fatal: the call still goes through normal consent,
        // which is the safe direction.
        let _ = self.ensure_trust(now.unix_timestamp());
        let result = match request.method.as_str() {
            "ping" => self.ping(),
            "capabilities" => self.capabilities(now),
            "observe.screen" => self.observe_screen(&request.params, now),
            "act.type" => self.act_type(&request.params, now),
            "act.key" => self.act_key(&request.params, now),
            "act.pointer" => self.act_pointer(&request.params, now),
            "act.clipboard" => self.act_clipboard(&request.params, now),
            "act.launch" => self.act_launch(&request.params, now),
            "app.list" => self.app_list(),
            "shell.run" => self.shell_run(&request.params, now),
            "shell.exec" => self.shell_exec(&request.params, now),
            "web.search" => self.web_search(&request.params, now),
            "web.last" => self.web_last(),
            "web.fetch" => self.web_fetch(&request.params, now),
            "consent.grant" => self.consent_grant(&request.params, now),
            "consent.revoke" => self.consent_revoke(&request.params, now),
            "consent.list" => self.consent_list(now),
            "consent.pending" => self.consent_pending(now),
            "consent.decide" => self.consent_decide(&request.params, now),
            "consent.revoke_run" => self.consent_revoke_run(now),
            "consent.revoke_all" => self.consent_revoke_all(now),
            "audit.tail" => self.audit_tail(&request.params),
            other => Err(ApiError::new(
                ErrorCode::UnknownMethod,
                format!(
                    "unknown method \"{}\"; this daemon knows ping, capabilities, observe.screen, \
                     act.type, act.key, act.pointer, act.clipboard, act.launch, app.list, \
                     shell.run, shell.exec, web.search, web.fetch, web.last, \
                     consent.grant, consent.revoke, \
                     consent.list, consent.pending, consent.decide, consent.revoke_run, \
                     consent.revoke_all and audit.tail",
                    one_line(other, 64)
                ),
            )),
        };
        match result {
            Ok(value) => Response::ok(id, value),
            Err(err) => Response::err(id, &err),
        }
    }

    fn ping(&self) -> Result<Value, ApiError> {
        Ok(json!({
            "version": VERSION,
            "pid": self.pid,
            "startedAt": clock::rfc3339(self.started_at)?,
        }))
    }

    fn capabilities(&self, now: OffsetDateTime) -> Result<Value, ApiError> {
        let now_unix = now.unix_timestamp();
        self.ensure_trust(now_unix)?;
        let ledger = self.ledger();
        let mut list = Vec::with_capacity(caps::CAPABILITIES.len());
        for capability in caps::CAPABILITIES {
            let grant = match ledger.live(capability.name, now_unix) {
                Some(grant) => json!({
                    "mode": grant.mode.as_str(),
                    "expiresAt": self.stamp(now, grant.expires_unix)?,
                    "scope": grant.scope,
                    "target": grant.target,
                }),
                None => Value::Null,
            };
            list.push(json!({
                "name": capability.name,
                "description": capability.description,
                "grant": grant,
            }));
        }
        Ok(json!({ "capabilities": list, "trusted": self.trusted() }))
    }

    fn observe_screen(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("observe.screen", now, |trace| {
            trace.capability = Some(caps::OBSERVE_SCREEN.name.to_string());
            self.run_observe_screen(params, now, trace)
        })
    }

    fn run_observe_screen(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let target = match params.get("target") {
            None | Some(Value::Null) => Target::Window,
            Some(Value::String(text)) => Target::parse(text)?,
            Some(_) => {
                return Err(ApiError::invalid(
                    "\"target\" must be \"window\" or \"screen\"",
                ));
            }
        };
        trace.target = Some(target.as_str().to_string());

        let interactive = wants_confirmation(params);
        // For an interactive call the facts come first: the confirmation names
        // the window. A plain call keeps the old order, so a refusal reads
        // nothing at all.
        let mut facts = None;
        if interactive {
            facts = Some(self.capture_facts(target, trace)?);
        }
        let summary = match &facts {
            Some((Some(window), _)) => format!(
                "Capture the {} window",
                window_label_parts(&window.class, &window.title)
            ),
            Some((None, Some(monitor))) => format!("Capture the whole screen ({})", monitor.name),
            _ => "Capture the screen".to_string(),
        };
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::OBSERVE_SCREEN,
            "one capture",
            interactive,
            summary,
            "observe.screen",
            None,
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;
        // Read the facts again after the answer: the confirmation named a window and a
        // rectangle, and both can change while the card waits. The fresh read is what gets
        // captured, so the bytes match the window that is actually there now — and if it is not
        // the window that was approved, nothing is captured at all. The audit then keeps naming
        // what the person approved, not what happened to be in front of the camera.
        let approved_app = facts
            .as_ref()
            .and_then(|(window, _)| window.as_ref().map(|window| window.class.clone()));
        let approved_geometry = facts
            .as_ref()
            .and_then(|(window, _)| window.as_ref().map(|window| window.rect.geometry()));
        let (window, monitor) = self.capture_facts(target, trace)?;
        if let Some((approved_window, approved_monitor)) = facts {
            let refusal = match (target, &approved_window, &window) {
                (Target::Window, Some(approved), Some(now)) if !now.same_window(approved) => {
                    Some("the window changed after that approval, so nothing was captured")
                }
                (Target::Screen, _, _) => match &approved_monitor {
                    Some(approved)
                        if monitor.as_ref().map(|now| now.name.as_str())
                            != Some(approved.name.as_str()) =>
                    {
                        Some(
                            "the focused screen changed after that approval, so nothing was \
                             captured",
                        )
                    }
                    _ => None,
                },
                _ => None,
            };
            if let Some(message) = refusal {
                trace.app = approved_app;
                trace.geometry = approved_geometry;
                return Err(focus_moved(message));
            }
        }

        let name = format!(
            "{}-{}.png",
            clock::shot_stamp(now),
            crate::ids::random_hex(6)
        );
        let path = self.paths.shot_path(&name)?;
        match target {
            Target::Window => {
                let window = window.as_ref().ok_or_else(|| {
                    ApiError::internal("the window facts disappeared between check and capture")
                })?;
                self.capturer.grab_window(window, &path)?;
            }
            Target::Screen => {
                let monitor = monitor.as_ref().ok_or_else(|| {
                    ApiError::internal("the monitor facts disappeared between check and capture")
                })?;
                self.capturer.grab_screen(monitor, &path)?;
            }
        }
        let bytes = std::fs::metadata(&path)
            .map_err(|err| {
                ApiError::internal(format!("could not measure {}: {err}", path.display()))
            })?
            .len();
        let (width, height) = capture::png_file_dimensions(&path)?;
        trace.path = Some(path.display().to_string());
        trace.bytes = Some(bytes);

        // Build the payload before spending a one-shot grant: a vision failure
        // must not burn consent that produced nothing usable.
        let vision = self.capturer.vision(&path, &self.paths.shots_dir())?;

        // The capture happened: a `once` grant is spent now, a `standing` grant
        // stays. A concurrent call that already spent it loses here, and its
        // orphan capture is removed rather than handed out.
        if let Err(err) = self.charge(&grant, caps::OBSERVE_SCREEN.name, now_unix) {
            drop(std::fs::remove_file(&path));
            return Err(err);
        }

        let mut result = Map::new();
        result.insert(
            "capturedAt".to_string(),
            Value::String(clock::rfc3339(now)?),
        );
        result.insert(
            "path".to_string(),
            Value::String(path.display().to_string()),
        );
        result.insert("bytes".to_string(), Value::from(bytes));
        result.insert("width".to_string(), Value::from(width));
        result.insert("height".to_string(), Value::from(height));
        result.insert(
            "grant".to_string(),
            Value::String(grant.mode.as_str().to_string()),
        );
        result.insert(
            "target".to_string(),
            Value::String(target.as_str().to_string()),
        );
        if let Some(window) = &window {
            result.insert(
                "window".to_string(),
                json!({ "class": window.class, "title": window.title }),
            );
        }
        if let Some(monitor) = &monitor {
            result.insert(
                "monitor".to_string(),
                json!({ "name": monitor.name, "scale": monitor.scale }),
            );
        }
        result.insert(
            "vision".to_string(),
            json!({
                "mime": vision.mime,
                "width": vision.width,
                "height": vision.height,
                "bytes": vision.bytes,
                "base64": vision.base64,
            }),
        );
        Ok(Value::Object(result))
    }

    /// `act.type`: type text into the focused window. The capability is `act.type`.
    fn act_type(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("act.type", now, |trace| {
            trace.capability = Some(caps::ACT_TYPE.name.to_string());
            self.run_act_type(params, now, trace)
        })
    }

    fn run_act_type(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let text = required_text(params, "act.type", "text")?;
        let chars = crate::act::validate_text(text)?;
        trace.chars = Some(chars as u64);

        let interactive = wants_confirmation(params);
        let mut known = None;
        if interactive {
            known = Some(self.focused_window(trace)?);
        }
        let summary = match &known {
            Some(target) => type_summary(target, text),
            None => String::new(),
        };
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::ACT_TYPE,
            "one action",
            interactive,
            summary,
            "act.type",
            known.as_ref().map(|window| window.class.as_str()),
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;
        let target = match &known {
            Some(target) => target.clone(),
            None => self.focused_window(trace)?,
        };

        // When the card approved this, the approval named a window: the text goes there or
        // nowhere. Without a confirmation nothing waited, so there is nothing to re-check.
        self.actor.type_text(text, known.as_ref())?;
        self.charge(&grant, caps::ACT_TYPE.name, now_unix)?;
        Ok(json!({
            "method": "act.type",
            "chars": chars,
            "target": { "class": target.class, "title": target.title },
        }))
    }

    /// `act.key`: press one validated chord in the focused window. Capability `act.keys`.
    fn act_key(&self, params: &Map<String, Value>, now: OffsetDateTime) -> Result<Value, ApiError> {
        self.audited("act.key", now, |trace| {
            trace.capability = Some(caps::ACT_KEYS.name.to_string());
            self.run_act_key(params, now, trace)
        })
    }

    fn run_act_key(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let chord = Chord::parse(required_text(params, "act.key", "key")?)?;
        // Counted for the audit; the chord itself is never logged.
        trace.key_count = Some(chord.key_count() as u64);

        let interactive = wants_confirmation(params);
        // The window in front of the person right now: what a fresh approval names,
        // and what a running approval is looked up against. A call that is not
        // interactive has no approval to place, so it does not read the window
        // before the consent check — reading it could fail for the wrong reason.
        let current = if interactive {
            Some(self.focused_window(trace)?)
        } else {
            None
        };
        let scope = current.as_ref().map(crate::act::FocusedWindow::scope);
        let summary = match &current {
            Some(target) => format!(
                "Press {} in {} (allows keys in this window for {} s)",
                chord.canonical(),
                window_label(target),
                KEY_RUN_SECONDS
            ),
            None => String::new(),
        };
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::ACT_KEYS,
            "one action",
            interactive,
            summary,
            "act.key",
            scope.as_deref(),
            AllowPolicy::Run {
                seconds: KEY_RUN_SECONDS,
                target: scope.clone(),
            },
            trace,
            now_unix,
        )?;
        let target = match &current {
            Some(target) => target.clone(),
            None => self.focused_window(trace)?,
        };

        // A 60-second key approval is bound to one window — class *and* address, so
        // a second window of the same app is a different approval. What the keys
        // are checked against is the window the grant names, not the one that has
        // focus now: comparing the current window with itself would let a reused
        // grant follow focus into another window of the same class.
        let approved = grant
            .target
            .as_deref()
            .and_then(crate::act::FocusedWindow::from_scope)
            .or_else(|| current.clone());
        self.actor.press(&chord, approved.as_ref())?;
        self.charge(&grant, caps::ACT_KEYS.name, now_unix)?;
        Ok(json!({
            "method": "act.key",
            "chord": chord.canonical(),
            "target": { "class": target.class, "title": target.title },
        }))
    }

    /// `act.pointer`: move the real pointer inside the focused window and click
    /// or scroll. Capability `act.pointer`.
    fn act_pointer(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("act.pointer", now, |trace| {
            trace.capability = Some(caps::ACT_POINTER.name.to_string());
            self.run_act_pointer(params, now, trace)
        })
    }

    fn run_act_pointer(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let action = crate::act::pointer_action(required_text(params, "act.pointer", "action")?)?;
        let nx = required_fraction(params, "x")?;
        let ny = required_fraction(params, "y")?;
        let dx = optional_i32(params, "dx")?;
        let dy = optional_i32(params, "dy")?;
        trace.action = Some(action.to_string());

        let interactive = wants_confirmation(params);
        let known = if interactive {
            Some(self.focused_window(trace)?)
        } else {
            None
        };
        let summary = match (&known, action) {
            (Some(target), "scroll") => format!(
                "Scroll {} at {}%, {}% in {}",
                scroll_words(dx, dy),
                percent(nx),
                percent(ny),
                window_label(target)
            ),
            (Some(target), "move") => format!(
                "Move the pointer to {}%, {}% in {}",
                percent(nx),
                percent(ny),
                target.class
            ),
            (Some(target), other) => format!(
                "{} at {}%, {}% in {}",
                pointer_verb(other),
                percent(nx),
                percent(ny),
                target.class
            ),
            _ => String::new(),
        };
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::ACT_POINTER,
            "one action",
            interactive,
            summary,
            "act.pointer",
            known.as_ref().map(|window| window.class.as_str()),
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;
        let target = match &known {
            Some(target) => target.clone(),
            None => self.focused_window(trace)?,
        };
        trace.app = Some(target.class.clone());
        self.actor.pointer(action, nx, ny, dx, dy, known.as_ref())?;
        self.charge(&grant, caps::ACT_POINTER.name, now_unix)?;
        Ok(json!({
            "method": "act.pointer",
            "action": action,
            "x": nx,
            "y": ny,
            "app": target.class,
        }))
    }

    /// `act.launch`: start one of the fixed launch actions. Capability `act.launch`.
    fn act_launch(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("act.launch", now, |trace| {
            trace.capability = Some(caps::ACT_LAUNCH.name.to_string());
            self.run_act_launch(params, now, trace)
        })
    }

    fn run_act_launch(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        // Two shapes: a fixed verb (`action`), or any app this machine has (`app`, a desktop
        // id). Both are validated before consent, so a refused call starts nothing.
        let requested_app = optional_text(params, "app")?;
        let (action, app, summary) = match requested_app {
            Some(raw) => {
                let id = crate::launch::validate_desktop_id(raw)?;
                if !crate::launch::is_installed(&id) {
                    return Err(ApiError::invalid(format!(
                        "{} is not installed on this machine; app.list shows what is",
                        one_line(&id, 64)
                    )));
                }
                trace.action = Some(id.clone());
                // The id is what `gtk-launch` receives; the name is only what the
                // menu calls it. Show both when they differ.
                let name = crate::launch::display_name(&id);
                let summary = if name == id {
                    format!("Open {id}")
                } else {
                    format!("Open {name} ({id})")
                };
                (None, LaunchApp::Desktop(id), summary)
            }
            None => {
                let action = required_text(params, "act.launch", "action")?;
                let action = crate::act::launch_action(action)?;
                trace.action = Some(action.to_string());
                // Which app this verb opens is decided here, from the person's own settings,
                // before consent is asked: the confirmation should name the app that opens.
                let app = self.launch_app(action)?;
                let summary = match &app {
                    LaunchApp::Command => match crate::act::launch_verb_program(action) {
                        Some((program, args)) => format!(
                            "Open the {action}: {}",
                            crate::act::program_line(program, args)
                        ),
                        None => format!("Open the {action}"),
                    },
                    LaunchApp::Desktop(id) => {
                        let name = crate::launch::display_name(id);
                        if name == *id {
                            format!("Open {id}")
                        } else {
                            format!("Open {name} ({id})")
                        }
                    }
                };
                (Some(action), app, summary)
            }
        };

        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::ACT_LAUNCH,
            "one launch",
            wants_confirmation(params),
            summary,
            "act.launch",
            None,
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;

        match &app {
            LaunchApp::Command => self
                .actor
                .launch(action.ok_or_else(|| ApiError::internal("a launch verb went missing"))?)?,
            LaunchApp::Desktop(id) => {
                self.actor.launch_desktop(id)?;
                // "Open the app" has to mean the window is in front of the person:
                // gtk-launch activates an app that is already running, and a minimized
                // one stays on Omarchy's hidden workspace until someone fetches it.
                self.actor.raise_app(&crate::launch::app_name(id))?;
            }
        }
        self.charge(&grant, caps::ACT_LAUNCH.name, now_unix)?;
        Ok(json!({
            "method": "act.launch",
            "action": action,
            "app": match &app {
                LaunchApp::Command => action.unwrap_or_default(),
                LaunchApp::Desktop(id) => id.as_str(),
            },
        }))
    }

    /// `app.list`: the desktop entries this machine has. Read-only, no grant.
    fn app_list(&self) -> Result<Value, ApiError> {
        let apps: Vec<Value> = crate::launch::installed_apps()
            .into_iter()
            .map(|app| json!({ "id": app.id, "name": app.name }))
            .collect();
        Ok(json!({ "apps": apps }))
    }

    /// `shell.exec`: run one command in the person's own shell. Capability `shell.exec`.
    fn shell_exec(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("shell.exec", now, |trace| {
            trace.capability = Some(caps::SHELL_EXEC.name.to_string());
            self.run_shell_exec(params, now, trace)
        })
    }

    fn run_shell_exec(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let command = required_text(params, "shell.exec", "command")?;
        let command = crate::act::validate_command(command)?;
        // The audit keeps the program, never the whole command line: that line can
        // carry a path or a value the person would not want written down.
        trace.action = Some(crate::act::command_program(&command));
        trace.chars = Some(command.chars().count() as u64);

        let cwd = match optional_text(params, "cwd")? {
            Some(dir) => crate::act::validate_working_dir(dir)?,
            None => crate::act::default_working_dir()?,
        };
        let timeout = match params.get("timeoutSeconds") {
            None | Some(Value::Null) => crate::act::DEFAULT_EXEC_SECONDS,
            Some(value) => value
                .as_u64()
                .filter(|seconds| (1..=crate::act::MAX_EXEC_SECONDS).contains(seconds))
                .ok_or_else(|| {
                    ApiError::invalid(format!(
                        "\"timeoutSeconds\" must be a whole number from 1 to {}",
                        crate::act::MAX_EXEC_SECONDS
                    ))
                })?,
        };

        // No truncation, and the same string the actor hands to `bash -c`: what
        // the card shows is what runs, down to the `cd`.
        let summary = format!("Run: {}", crate::act::command_script(&command, &cwd));
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::SHELL_EXEC,
            "one command",
            wants_confirmation(params),
            summary,
            "shell.exec",
            None,
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;

        let output = self.actor.run_command(&command, &cwd, timeout)?;
        self.charge(&grant, caps::SHELL_EXEC.name, now_unix)?;
        Ok(json!({
            "method": "shell.exec",
            "command": command,
            "cwd": cwd.display().to_string(),
            "exitCode": output.status,
            "timedOut": output.timed_out,
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
        }))
    }

    /// `web.search`: look something up on the internet. Capability `web.search`.
    fn web_search(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("web.search", now, |trace| {
            trace.capability = Some(caps::WEB_SEARCH.name.to_string());
            self.run_web_search(params, now, trace)
        })
    }

    fn run_web_search(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let query = required_text(params, "web.search", "query")?;
        let query = crate::web::validate_query(query)?;
        // The audit keeps the length, never the words: a search can carry a name,
        // an address or a symptom, and writing that down is its own leak.
        trace.action = Some("search".to_string());
        trace.chars = Some(query.chars().count() as u64);

        let summary = search_summary(&query);
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::WEB_SEARCH,
            "one lookup",
            wants_confirmation(params),
            summary,
            "web.search",
            None,
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;

        // One consent, both indexes, in parallel: the person asked a question.
        // A configured provider key is a real web index; without one this is the
        // free news-and-encyclopedia path.
        let config = crate::web::load_config(self.paths.state_dir());
        let (hits, missing) = self.actor.search(config.provider, &config.key, &query)?;
        {
            // Host and picture only: this is shown, not logged.
            let media: Vec<(String, String)> = hits
                .iter()
                .filter(|hit| !hit.image.is_empty())
                .filter_map(|hit| {
                    let host = hit
                        .url
                        .split_once("://")
                        .and_then(|(_, rest)| rest.split('/').next())
                        .unwrap_or("")
                        .to_string();
                    (!host.is_empty()).then(|| (host, hit.image.clone()))
                })
                .take(4)
                .collect();
            if let Ok(mut slot) = self.last_media.lock() {
                *slot = (!media.is_empty()).then(|| LastSearch {
                    at: now.unix_timestamp(),
                    media,
                });
            }
        }
        let empty = hits.is_empty();
        self.charge(&grant, caps::WEB_SEARCH.name, now_unix)?;
        Ok(json!({
            "method": "web.search",
            "query": query,
            "missing": missing,
            "provider": config.provider.name(),
            "results": hits
                .into_iter()
                .map(|hit| json!({
                    "title": hit.title,
                    "url": hit.url,
                    "when": hit.when,
                    "source": hit.source,
                    "snippet": hit.snippet,
                    "image": hit.image,
                }))
                .collect::<Vec<_>>(),
            "hint": if empty {
                "nothing in the index for that query; try fewer or different words, and read a \
                 page with web_fetch if you know the address"
            } else {
                ""
            },
        }))
    }

    /// `web.last`: the pictures the last search came with. Local, read-only, no
    /// capability: it is the card showing what the answer is based on.
    fn web_last(&self) -> Result<Value, ApiError> {
        let slot = self
            .last_media
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(last) = slot.as_ref() else {
            return Ok(json!({ "at": 0, "images": [] }));
        };
        let (at, images) = (last.at, &last.media);
        Ok(json!({
            "at": at,
            "images": images
                .iter()
                .map(|(host, url)| json!({ "host": host, "url": url }))
                .collect::<Vec<_>>(),
        }))
    }

    /// `web.fetch`: read one public page as text. Capability `web.search`.
    fn web_fetch(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("web.fetch", now, |trace| {
            trace.capability = Some(caps::WEB_SEARCH.name.to_string());
            self.run_web_fetch(params, now, trace)
        })
    }

    fn run_web_fetch(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let url = required_text(params, "web.fetch", "url")?;
        let url = crate::web::validate_url(url)?;
        // Same rule as a command: if the card cannot show it whole, it is not
        // approved at all.
        if url.chars().count() > crate::web::MAX_SHOWN_URL_CHARS {
            return Err(ApiError::new(
                ErrorCode::TooLarge,
                format!(
                    "a URL may not be longer than {} characters, because the card shows the whole \
                     address before you approve it",
                    crate::web::MAX_SHOWN_URL_CHARS
                ),
            ));
        }
        // Host only: a path or a query string can name a person or carry a token.
        let host = url
            .split_once("://")
            .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
            .unwrap_or_default()
            .to_string();
        trace.action = Some(host.clone());
        trace.chars = Some(url.chars().count() as u64);

        let summary = fetch_summary(&url);
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::WEB_SEARCH,
            "one page",
            wants_confirmation(params),
            summary,
            "web.fetch",
            None,
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;

        let text = self.actor.fetch_url(&url)?;
        self.charge(&grant, caps::WEB_SEARCH.name, now_unix)?;
        Ok(json!({ "method": "web.fetch", "url": url, "host": host, "text": text }))
    }

    /// `shell.run`: run one of the fixed desktop actions. Capability `shell.run`.
    fn shell_run(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("shell.run", now, |trace| {
            trace.capability = Some(caps::SHELL_RUN.name.to_string());
            self.run_shell_run(params, now, trace)
        })
    }

    fn run_shell_run(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let action = required_text(params, "shell.run", "action")?;
        let action = crate::act::shell_action(action)?;
        trace.action = Some(action.to_string());

        let summary = shell_run_summary(action)?;
        let now_unix = now.unix_timestamp();
        let grant = self.require_grant_interactive(
            &caps::SHELL_RUN,
            "one action",
            wants_confirmation(params),
            summary,
            "shell.run",
            None,
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;

        self.actor.run_shell_action(action)?;
        self.charge(&grant, caps::SHELL_RUN.name, now_unix)?;
        Ok(json!({
            "method": "shell.run",
            "action": action,
        }))
    }

    /// `act.clipboard`: read or write the clipboard. One capability, both directions.
    fn act_clipboard(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("act.clipboard", now, |trace| {
            trace.capability = Some(caps::ACT_CLIPBOARD.name.to_string());
            self.run_act_clipboard(params, now, trace)
        })
    }

    fn run_act_clipboard(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let mode = match params.get("mode") {
            Some(Value::String(text)) => match text.as_str() {
                "read" => ClipboardMode::Read,
                "write" => ClipboardMode::Write,
                other => {
                    return Err(ApiError::invalid(format!(
                        "\"mode\" must be \"read\" or \"write\", not \"{}\"",
                        one_line(other, 32)
                    )));
                }
            },
            None | Some(Value::Null) => {
                return Err(ApiError::invalid(
                    "act.clipboard needs a \"mode\": \"read\" or \"write\"",
                ));
            }
            Some(_) => {
                return Err(ApiError::invalid("\"mode\" must be \"read\" or \"write\""));
            }
        };
        trace.mode = Some(mode.as_str().to_string());
        // Validate the body before consent: a refused call touches nothing.
        let text = match mode {
            ClipboardMode::Read => None,
            ClipboardMode::Write => {
                let text = required_text(params, "act.clipboard", "text")?;
                crate::act::validate_clipboard_text(text)?;
                Some(text)
            }
        };

        let now_unix = now.unix_timestamp();
        let summary = match mode {
            ClipboardMode::Read => "Read the clipboard".to_string(),
            ClipboardMode::Write => clipboard_summary(text.unwrap_or_default()),
        };
        let grant = self.require_grant_interactive(
            &caps::ACT_CLIPBOARD,
            "one call",
            wants_confirmation(params),
            summary,
            "act.clipboard",
            None,
            AllowPolicy::Once,
            trace,
            now_unix,
        )?;

        match (mode, text) {
            (ClipboardMode::Read, _) => {
                let pasted = self.actor.paste()?;
                trace.bytes = Some(pasted.len() as u64);
                self.charge(&grant, caps::ACT_CLIPBOARD.name, now_unix)?;
                Ok(json!({
                    "method": "act.clipboard",
                    "mode": "read",
                    "text": pasted,
                }))
            }
            (ClipboardMode::Write, Some(text)) => {
                self.actor.copy(text)?;
                let bytes = text.len() as u64;
                trace.bytes = Some(bytes);
                self.charge(&grant, caps::ACT_CLIPBOARD.name, now_unix)?;
                Ok(json!({
                    "method": "act.clipboard",
                    "mode": "write",
                    "bytes": bytes,
                }))
            }
            (ClipboardMode::Write, None) => Err(ApiError::internal(
                "the clipboard text disappeared between check and write",
            )),
        }
    }

    fn consent_grant(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("consent.grant", now, |trace| {
            self.run_consent_grant(params, now, trace)
        })
    }

    fn run_consent_grant(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let capability = requested_capability(params, "consent.grant")?;
        trace.capability = Some(capability.name.to_string());
        let mode = match params.get("mode") {
            None | Some(Value::Null) => GrantMode::Standing,
            Some(Value::String(text)) => GrantMode::parse(text).ok_or_else(|| {
                ApiError::invalid(format!(
                    "\"mode\" must be \"once\" or \"standing\", not \"{}\"",
                    one_line(text, 32)
                ))
            })?,
            Some(_) => {
                return Err(ApiError::invalid(
                    "\"mode\" must be \"once\" or \"standing\"",
                ));
            }
        };
        let ttl = match params.get("ttlSeconds") {
            None | Some(Value::Null) => mode.default_ttl(),
            Some(value) => value.as_u64().ok_or_else(|| {
                ApiError::invalid("\"ttlSeconds\" must be a whole number of seconds")
            })?,
        };
        if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl) {
            return Err(ApiError::new(
                ErrorCode::TtlOutOfRange,
                format!(
                    "ttlSeconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}, got {ttl}"
                ),
            ));
        }
        let now_unix = now.unix_timestamp();
        let grant = {
            let mut ledger = self.ledger();
            // Mutate a copy, write it, and only then commit: memory never runs ahead of disk.
            let mut next = ledger.clone();
            let grant = next.grant(capability.name, mode, ttl, now_unix)?;
            next.save(&self.paths.grants_file())?;
            *ledger = next;
            grant
        };
        trace.grant = GrantLabel::from_grant(&grant);
        Ok(json!({
            "capability": grant.capability,
            "mode": grant.mode.as_str(),
            "expiresAt": self.stamp(now, grant.expires_unix)?,
            "ttlSeconds": grant.ttl_seconds,
            "scope": grant.scope,
        }))
    }

    fn consent_revoke(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        self.audited("consent.revoke", now, |trace| {
            self.run_consent_revoke(params, now, trace)
        })
    }

    fn run_consent_revoke(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
        trace: &mut Trace,
    ) -> Result<Value, ApiError> {
        let capability = requested_capability(params, "consent.revoke")?;
        trace.capability = Some(capability.name.to_string());
        let now_unix = now.unix_timestamp();
        let removed = {
            let mut ledger = self.ledger();
            let mut next = ledger.clone();
            let removed = next.revoke(capability.name, now_unix);
            if removed {
                next.save(&self.paths.grants_file())?;
                *ledger = next;
            }
            removed
        };
        // After a revoke there is no grant left, whatever was there before.
        trace.grant = GrantLabel::None;
        Ok(json!({ "capability": capability.name, "revoked": removed }))
    }

    fn consent_list(&self, now: OffsetDateTime) -> Result<Value, ApiError> {
        let now_unix = now.unix_timestamp();
        let ledger = self.ledger();
        let mut grants = Vec::new();
        for grant in ledger.list(now_unix) {
            grants.push(json!({
                "capability": grant.capability,
                "mode": grant.mode.as_str(),
                "expiresAt": self.stamp(now, grant.expires_unix)?,
                "ttlSeconds": grant.ttl_seconds,
            }));
        }
        Ok(json!({ "grants": grants }))
    }

    /// The confirmations waiting for the card.
    fn consent_pending(&self, now: OffsetDateTime) -> Result<Value, ApiError> {
        let now_unix = now.unix_timestamp();
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        pending.retain(|_, request| now_unix - request.created_unix < PENDING_WAIT_SECONDS as i64);
        let list: Vec<Value> = pending
            .values()
            .map(|request| {
                json!({
                    "id": request.id,
                    "method": request.method,
                    "capability": request.capability,
                    "summary": request.summary,
                    "createdUnix": request.created_unix,
                    "expiresIn": (PENDING_WAIT_SECONDS as i64 - (now_unix - request.created_unix)).max(0),
                })
            })
            .collect();
        Ok(json!({ "pending": list }))
    }

    /// The person's answer to a confirmation. The waiting call reads it.
    fn consent_decide(
        &self,
        params: &Map<String, Value>,
        now: OffsetDateTime,
    ) -> Result<Value, ApiError> {
        let id = required_text(params, "consent.decide", "id")?.to_string();
        let allow = match params.get("allow") {
            Some(Value::Bool(value)) => *value,
            _ => {
                return Err(ApiError::invalid(
                    "consent.decide needs a boolean \"allow\"",
                ));
            }
        };
        let mut trace = Trace {
            grant: GrantLabel::None,
            ..Trace::default()
        };
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let Some(request) = pending.get_mut(&id) else {
            return Err(ApiError::invalid(
                "no such confirmation; it may have expired",
            ));
        };
        if request.decision.is_some() {
            return Err(ApiError::invalid("that confirmation was already answered"));
        }
        trace.capability = Some(request.capability.clone());
        request.decision = Some(allow);
        drop(pending);
        self.write_audit(
            "consent.decide",
            if allow {
                Outcome::Allowed
            } else {
                Outcome::Denied
            },
            &trace,
            None,
            now,
        )?;
        Ok(json!({ "id": id, "decision": if allow { "allow" } else { "deny" } }))
    }

    /// Drop the approvals the card minted for a finished answer.
    fn consent_revoke_run(&self, now: OffsetDateTime) -> Result<Value, ApiError> {
        self.audited("consent.revoke_run", now, |trace| {
            trace.grant = GrantLabel::None;
            self.run_epoch.fetch_add(1, Ordering::SeqCst);
            self.fail_waiting_pending();
            let now_unix = now.unix_timestamp();
            let mut ledger = self.ledger();
            let mut next = ledger.clone();
            let revoked = next.revoke_scope("run", now_unix);
            next.save(&self.paths.grants_file())?;
            *ledger = next;
            Ok(json!({ "revoked": revoked }))
        })
    }

    /// The panic button as one broker call: every grant goes, a waiting
    /// confirmation is denied, and in-flight approvals cannot mint (the epoch
    /// moves). Audited once; this is not about one capability, so the entry has
    /// no capability.
    fn consent_revoke_all(&self, now: OffsetDateTime) -> Result<Value, ApiError> {
        self.audited("consent.revoke_all", now, |trace| {
            trace.grant = GrantLabel::None;
            self.run_epoch.fetch_add(1, Ordering::SeqCst);
            self.fail_waiting_pending();
            let now_unix = now.unix_timestamp();
            let mut ledger = self.ledger();
            let mut next = ledger.clone();
            let revoked = next.revoke_all(now_unix);
            next.save(&self.paths.grants_file())?;
            *ledger = next;
            Ok(json!({ "revoked": revoked }))
        })
    }

    /// Deny every confirmation that has not been answered yet. The waiting
    /// call returns `consent_denied` instead of holding the thread for a minute.
    fn fail_waiting_pending(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for request in pending.values_mut() {
            if request.decision.is_none() {
                request.decision = Some(false);
            }
        }
    }

    fn audit_tail(&self, params: &Map<String, Value>) -> Result<Value, ApiError> {
        let lines = match params.get("lines") {
            None | Some(Value::Null) => DEFAULT_TAIL_LINES,
            Some(value) => {
                let number = value.as_u64().ok_or_else(|| {
                    ApiError::invalid("\"lines\" must be a whole number of lines")
                })?;
                if !(1..=MAX_TAIL_LINES as u64).contains(&number) {
                    return Err(ApiError::invalid(format!(
                        "\"lines\" must be between 1 and {MAX_TAIL_LINES}"
                    )));
                }
                number as usize
            }
        };
        Ok(Value::Array(self.audit.tail(lines)?))
    }

    /// Check that a live grant exists, recording it for the audit entry.
    ///
    /// `consequence` is what one call would do (`one capture`, `one action`), so the refusal can
    /// name the exact command that would allow it.
    fn require_grant(
        &self,
        capability: &caps::Capability,
        consequence: &str,
        app: Option<&str>,
        trace: &mut Trace,
        now_unix: i64,
    ) -> Result<Grant, ApiError> {
        let grant = self.ledger().live_for(capability.name, app, now_unix);
        let Some(grant) = grant else {
            return Err(ApiError::new(
                ErrorCode::ConsentRequired,
                format!(
                    "{} needs your consent and there is no live grant; run \
                     `omassistantctl consent grant {} --once` for {consequence}, or \
                     `omassistantctl consent grant {} --for 15m` to allow a while",
                    capability.name, capability.name, capability.name
                ),
            ));
        };
        trace.grant = GrantLabel::from_grant(&grant);
        Ok(grant)
    }

    /// A live grant, or — when the caller is the card — a confirmation the person
    /// answers. The policy decides what a yes mints: one call for text, clipboard,
    /// launches and captures; a short, window-bound key approval for navigation.
    #[allow(clippy::too_many_arguments)]
    /// Is this machine trusted, so that nothing asks?
    ///
    /// A file in the state directory, not a capability: it is a property of the
    /// machine, it is written by the person who owns it, and it is read on every
    /// call so revoking it takes effect immediately. The audit log still records
    /// every call, and says it was trusted — a paper trail, not a prompt.
    fn trusted(&self) -> bool {
        let path = self.paths.state_dir().join("trust.json");
        let Ok(raw) = std::fs::read_to_string(path) else {
            return false;
        };
        serde_json::from_str::<Value>(&raw)
            .ok()
            .and_then(|value| value.get("allowAll").and_then(Value::as_bool))
            .unwrap_or(false)
    }

    /// Keep a standing grant for every capability while the machine is trusted.
    ///
    /// Trust is expressed as ordinary grants so that everything downstream — the
    /// card, the ledger, the expiry, `consent revoke` — keeps working unchanged.
    fn ensure_trust(&self, now_unix: i64) -> Result<(), ApiError> {
        if !self.trusted() {
            // Trust off must mean off: the grants it minted are standing for a
            // day, and leaving them behind would make the switch a decoration.
            return self.revoke_trust_grants(now_unix);
        }
        let mut ledger = self.ledger();
        let mut next = ledger.clone();
        let mut changed = false;
        for capability in caps::CAPABILITIES {
            let wanted = next
                .live_for(capability.name, None, now_unix)
                .is_none_or(|grant| grant.expires_unix - now_unix < 3600);
            if !wanted {
                continue;
            }
            next.grant_scoped(
                capability.name,
                GrantMode::Standing,
                crate::grants::MAX_TTL_SECONDS,
                // The scope is what tells these apart from a grant the person
                // made by hand, so turning trust off can take back exactly what
                // it gave.
                Some(TRUST_SCOPE),
                None,
                now_unix,
            )?;
            changed = true;
        }
        if changed {
            next.save(&self.paths.grants_file())?;
            *ledger = next;
        }
        Ok(())
    }

    /// Take back every grant trust minted, and nothing else.
    fn revoke_trust_grants(&self, now_unix: i64) -> Result<(), ApiError> {
        let mut ledger = self.ledger();
        if !ledger.has_scope(TRUST_SCOPE) {
            return Ok(());
        }
        let mut next = ledger.clone();
        next.revoke_scoped(TRUST_SCOPE, now_unix);
        next.save(&self.paths.grants_file())?;
        *ledger = next;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn require_grant_interactive(
        &self,
        capability: &caps::Capability,
        consequence: &str,
        interactive: bool,
        summary: String,
        method: &str,
        app: Option<&str>,
        policy: AllowPolicy,
        trace: &mut Trace,
        now_unix: i64,
    ) -> Result<Grant, ApiError> {
        if let Some(grant) = self.ledger().live_for(capability.name, app, now_unix) {
            trace.grant = GrantLabel::from_grant(&grant);
            return Ok(grant);
        }
        if !interactive {
            return self.require_grant(capability, consequence, None, trace, now_unix);
        }
        let epoch = self.run_epoch.load(Ordering::SeqCst);

        let id = format!("c{}", crate::ids::random_hex(10));
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            pending.insert(
                id.clone(),
                PendingRequest {
                    id: id.clone(),
                    method: method.to_string(),
                    capability: capability.name.to_string(),
                    // Stored exactly as built: `one_line` used to fold runs of
                    // spaces and newlines here, so a command like `printf 'a   b'`
                    // was displayed as `printf 'a b'` — the person approved a string
                    // that was not the one that ran. The capability caps are what
                    // bound the length.
                    summary,
                    created_unix: now_unix,
                    decision: None,
                },
            );
        }

        let deadline = Instant::now() + Duration::from_secs(PENDING_WAIT_SECONDS);
        let decision = loop {
            let decided = self
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(&id)
                .and_then(|request| request.decision);
            if decided.is_some() {
                break decided;
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(80));
        };
        self.pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&id);

        match decision {
            Some(true) => {
                if self.run_epoch.load(Ordering::SeqCst) != epoch {
                    return Err(ApiError::new(
                        ErrorCode::ConsentDenied,
                        "the answer was stopped before that approval landed, so nothing was done",
                    ));
                }
                let minted_at = self.clock.now().unix_timestamp();
                let mut ledger = self.ledger();
                let mut next = ledger.clone();
                let grant = match &policy {
                    AllowPolicy::Once => next.grant_scoped(
                        capability.name,
                        GrantMode::Once,
                        APPROVAL_ONCE_TTL_SECONDS,
                        Some("run"),
                        None,
                        minted_at,
                    )?,
                    AllowPolicy::Run { seconds, target } => next.grant_scoped(
                        capability.name,
                        GrantMode::Standing,
                        *seconds,
                        Some("run"),
                        target.as_deref(),
                        minted_at,
                    )?,
                };
                next.save(&self.paths.grants_file())?;
                *ledger = next;
                trace.grant = GrantLabel::from_grant(&grant);
                Ok(grant)
            }
            Some(false) => Err(ApiError::new(
                ErrorCode::ConsentDenied,
                "you declined that action, so nothing was done",
            )),
            None => Err(ApiError::new(
                ErrorCode::ConsentTimeout,
                "nobody answered the confirmation, so nothing was done",
            )),
        }
    }

    /// Which app one launch verb starts on this machine.
    ///
    /// The verb is the caller's; the app is not. A per-verb desktop id from the person's
    /// `config.json` wins, then `mail` falls back to the desktop's own `mailto` handler, and the
    /// three verbs Omarchy has a default for use its launcher. Nothing here reads a value from the
    /// request, so a caller cannot choose the program.
    fn launch_app(&self, verb: &str) -> Result<LaunchApp, ApiError> {
        let overrides = crate::launch::LaunchOverrides::read(self.paths.config_file())?;
        if let Some(id) = overrides.for_verb(verb) {
            return Ok(LaunchApp::Desktop(id.to_string()));
        }
        if verb != "mail" {
            return Ok(LaunchApp::Command);
        }
        match self.actor.mail_handler()? {
            Some(id) => Ok(LaunchApp::Desktop(id)),
            None => Err(ApiError::new(
                ErrorCode::LaunchFailed,
                "no mail app is set up: this desktop has no mailto handler, and \
                 ~/.config/omassistant/config.json has no \"launch\": {\"mail\": \"<App>.desktop\"}",
            )),
        }
    }

    /// The focused window, recorded for the audit entry, or `no_window`.
    fn focused_window(&self, trace: &mut Trace) -> Result<FocusedWindow, ApiError> {
        let window = self.actor.focused_window()?;
        trace.app = Some(window.class.clone());
        Ok(window)
    }

    /// Compositor facts for a capture: the window rectangle or the focused monitor.
    fn capture_facts(
        &self,
        target: Target,
        trace: &mut Trace,
    ) -> Result<
        (
            Option<crate::capture::WindowFacts>,
            Option<crate::capture::MonitorFacts>,
        ),
        ApiError,
    > {
        match target {
            Target::Window => {
                let window = self.capturer.active_window()?;
                trace.app = Some(window.class.clone());
                trace.geometry = Some(window.rect.geometry());
                // The monitor is context for the caller; a failure to read it is not fatal.
                let monitor = window.monitor.and_then(|index| {
                    self.capturer
                        .monitors()
                        .ok()
                        .and_then(|monitors| monitors.into_iter().nth(index))
                });
                Ok((Some(window), monitor))
            }
            Target::Screen => Ok((None, Some(self.capturer.focused_monitor()?))),
        }
    }

    /// Spend a `once` grant now that the action actually happened; `standing` stays.
    fn charge(&self, grant: &Grant, capability: &str, now_unix: i64) -> Result<(), ApiError> {
        match grant.mode {
            GrantMode::Standing => Ok(()),
            GrantMode::Once => self.spend_once(capability, now_unix),
        }
    }

    /// Spend a `once` grant after a successful call, writing the file atomically.
    fn spend_once(&self, capability: &str, now_unix: i64) -> Result<(), ApiError> {
        let mut ledger = self.ledger();
        let mut next = ledger.clone();
        if !next.consume(capability, now_unix) {
            // Another call spent the one-shot grant between our consent check and
            // this spend. Exactly one call may come out of it.
            return Err(ApiError::new(
                ErrorCode::ConsentRequired,
                "the one-shot consent was already used; ask for another",
            ));
        }
        next.save(&self.paths.grants_file())?;
        *ledger = next;
        Ok(())
    }

    /// Run a capability or consent method and write its audit entry.
    ///
    /// `Ok` is `allowed`, `consent_required` is `denied`, anything else is `error`; the code goes
    /// into the entry, the message does not.
    fn audited<T>(
        &self,
        method: &str,
        now: OffsetDateTime,
        operation: impl FnOnce(&mut Trace) -> Result<T, ApiError>,
    ) -> Result<T, ApiError> {
        let mut trace = Trace::default();
        let outcome = operation(&mut trace);
        let code = outcome.as_ref().err().map(ApiError::code);
        let result_label = match code {
            None => Outcome::Allowed,
            Some(ErrorCode::ConsentRequired | ErrorCode::ConsentDenied) => Outcome::Denied,
            Some(_) => Outcome::Error,
        };
        self.write_audit(method, result_label, &trace, code, now)?;
        outcome
    }

    /// Write one audit entry. Split out so a consent decision can record its own
    /// outcome (allow vs deny) without going through the capability wrapper.
    fn write_audit(
        &self,
        method: &str,
        outcome: Outcome,
        trace: &Trace,
        code: Option<ErrorCode>,
        now: OffsetDateTime,
    ) -> Result<(), ApiError> {
        let entry = AuditEntry {
            ts: clock::rfc3339(now)?,
            pid: self.pid,
            method: method.to_string(),
            outcome,
            capability: trace.capability.clone(),
            grant: trace.grant,
            target: trace.target.clone(),
            app: trace.app.clone(),
            geometry: trace.geometry.clone(),
            path: trace.path.clone(),
            bytes: trace.bytes,
            chars: trace.chars,
            key_count: trace.key_count,
            mode: trace.mode.clone(),
            action: trace.action.clone(),
            error: code.map(|code| code.as_str().to_string()),
        };
        self.audit.append(&entry)
    }

    /// RFC 3339 for a Unix timestamp, in the clock's offset.
    fn stamp(&self, now: OffsetDateTime, unix_seconds: i64) -> Result<String, ApiError> {
        clock::rfc3339(clock::from_unix(unix_seconds)?.to_offset(now.offset()))
    }

    /// The ledger, recovering from a poisoned lock (the file is the source of truth).
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Which direction an `act.clipboard` call asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipboardMode {
    /// Hand the clipboard back to the caller.
    Read,
    /// Put the caller's text on the clipboard.
    Write,
}

impl ClipboardMode {
    /// The wire spelling, and what the audit entry records.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
        }
    }
}

/// Does this call want the card to ask the person (`interactive: true`)?
fn wants_confirmation(params: &Map<String, Value>) -> bool {
    params
        .get("interactive")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// What one launch verb starts on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchApp {
    /// Omarchy's own launcher for `browser`, `terminal` and `editor`.
    Command,
    /// A desktop entry the person chose (or the desktop's `mailto` handler).
    Desktop(String),
}

/// The window, or the screen, is no longer the one the person approved.
fn focus_moved(message: &str) -> ApiError {
    ApiError::new(ErrorCode::FocusMoved, message)
}

fn required_fraction(params: &Map<String, Value>, field: &str) -> Result<f64, ApiError> {
    let value = params.get(field).and_then(Value::as_f64).ok_or_else(|| {
        ApiError::invalid(format!(
            "act.pointer needs a numeric \"{field}\" from 0 to 1"
        ))
    })?;
    if !(0.0..=1.0).contains(&value) || !value.is_finite() {
        return Err(ApiError::invalid(format!(
            "\"{field}\" must be a number from 0 to 1"
        )));
    }
    Ok(value)
}

fn optional_i32(params: &Map<String, Value>, field: &str) -> Result<i32, ApiError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::Number(number)) => number
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| ApiError::invalid(format!("\"{field}\" must be a 32-bit integer"))),
        Some(_) => Err(ApiError::invalid(format!("\"{field}\" must be an integer"))),
    }
}

fn percent(fraction: f64) -> i64 {
    (fraction * 100.0).round() as i64
}

fn pointer_verb(action: &str) -> &'static str {
    match action {
        "click" => "Click",
        "double-click" => "Double-click",
        "right-click" => "Right-click",
        "middle-click" => "Middle-click",
        "scroll" => "Scroll",
        "move" => "Move the pointer",
        _ => "Point",
    }
}

fn scroll_words(dx: i32, dy: i32) -> String {
    match (dx, dy) {
        (x, 0) => format!("{x}"),
        (0, y) => format!("{y}"),
        (x, y) => format!("{x}, {y}"),
    }
}

/// A required string parameter, refused with `invalid_params` when it is missing or not a string.
/// An optional string parameter: absent and `null` both mean "not given".
fn optional_text<'a>(
    params: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, ApiError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.trim().is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.as_str())),
        Some(_) => Err(ApiError::invalid(format!("\"{field}\" must be a string"))),
    }
}

fn required_text<'a>(
    params: &'a Map<String, Value>,
    method: &str,
    field: &str,
) -> Result<&'a str, ApiError> {
    match params.get(field) {
        Some(Value::String(text)) => Ok(text.as_str()),
        None | Some(Value::Null) => Err(ApiError::invalid(format!("{method} needs a \"{field}\""))),
        Some(_) => Err(ApiError::invalid(format!("\"{field}\" must be a string"))),
    }
}

/// The capability a consent method was pointed at.
fn requested_capability(
    params: &Map<String, Value>,
    method: &str,
) -> Result<&'static caps::Capability, ApiError> {
    let name = match params.get("capability") {
        Some(Value::String(name)) => name,
        None => {
            return Err(ApiError::invalid(format!(
                "{method} needs a \"capability\""
            )));
        }
        Some(_) => return Err(ApiError::invalid("\"capability\" must be a string")),
    };
    caps::find(name).ok_or_else(|| {
        ApiError::new(
            ErrorCode::CapabilityUnknown,
            format!(
                "unknown capability \"{}\"; this daemon knows {}",
                one_line(name, 64),
                caps::CAPABILITIES
                    .iter()
                    .map(|capability| capability.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use crate::exec::CommandOutput;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    const T0: i64 = 1_700_000_000;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A runner that refuses to run anything: only the consent paths are exercised here.
    #[derive(Debug)]
    struct NoTools;

    impl CommandRunner for NoTools {
        fn run(
            &self,
            program: &str,
            _args: &[&str],
            _timeout: Duration,
        ) -> Result<CommandOutput, ApiError> {
            Err(ApiError::new(
                ErrorCode::ToolMissing,
                format!("{program} must not run in this test"),
            ))
        }

        fn run_with_input(
            &self,
            program: &str,
            _args: &[&str],
            _input: &[u8],
            _timeout: Duration,
        ) -> Result<CommandOutput, ApiError> {
            Err(ApiError::new(
                ErrorCode::ToolMissing,
                format!("{program} must not run in this test"),
            ))
        }

        fn spawn_detached(&self, program: &str, _args: &[&str]) -> Result<(), ApiError> {
            Err(ApiError::new(
                ErrorCode::ToolMissing,
                format!("{program} must not run in this test"),
            ))
        }
    }

    fn temp_paths(tag: &str) -> Paths {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "omassistantd-broker-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        Paths::new(dir.join("run.sock"), dir.join("state"))
    }

    fn broker(paths: Paths) -> Broker {
        Broker::new(BrokerConfig {
            paths,
            clock: Arc::new(FixedClock::from_unix(T0).unwrap()),
            runner: Arc::new(NoTools),
            tools: ToolSet::none(),
        })
        .unwrap()
    }

    fn call(broker: &Broker, method: &str, params: Value) -> Result<Value, ApiError> {
        let line = json!({ "id": "t", "method": method, "params": params }).to_string();
        let response = Response::from_line(&broker.handle_line(line.as_bytes())).unwrap();
        match (response.result, response.error) {
            (Some(result), _) => Ok(result),
            (None, Some(error)) => Err(ApiError::new(
                match error.code.as_str() {
                    "consent_required" => ErrorCode::ConsentRequired,
                    "capability_unknown" => ErrorCode::CapabilityUnknown,
                    "ttl_out_of_range" => ErrorCode::TtlOutOfRange,
                    "invalid_params" => ErrorCode::InvalidParams,
                    "unknown_method" => ErrorCode::UnknownMethod,
                    "bad_json" => ErrorCode::BadJson,
                    "bad_request" => ErrorCode::BadRequest,
                    "nothing_to_type" => ErrorCode::NothingToType,
                    "too_large" => ErrorCode::TooLarge,
                    "no_window" => ErrorCode::NoWindow,
                    "wtype_failed" => ErrorCode::WtypeFailed,
                    "clipboard_failed" => ErrorCode::ClipboardFailed,
                    "tool_missing" => ErrorCode::ToolMissing,
                    "capture_failed" => ErrorCode::CaptureFailed,
                    "internal" => ErrorCode::Internal,
                    other => panic!("unexpected code {other}"),
                },
                error.message,
            )),
            (None, None) => panic!("a response must carry a result or an error"),
        }
    }

    #[test]
    fn trust_off_takes_back_what_trust_gave() {
        let paths = temp_paths("trust");
        let broker = broker(paths.clone());
        std::fs::write(paths.state_dir().join("trust.json"), r#"{"allowAll":true}"#).unwrap();

        // Trusted: every capability is granted, and it says so.
        let caps = call(&broker, "capabilities", Value::Null).unwrap();
        assert_eq!(caps["trusted"], Value::Bool(true));
        let granted = caps["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|cap| !cap["grant"].is_null())
            .count();
        assert_eq!(granted, caps::CAPABILITIES.len());

        // Not trusted: nothing is left behind, not even for a minute.
        std::fs::remove_file(paths.state_dir().join("trust.json")).unwrap();
        let caps = call(&broker, "capabilities", Value::Null).unwrap();
        assert_eq!(caps["trusted"], Value::Bool(false));
        let granted = caps["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|cap| !cap["grant"].is_null())
            .count();
        assert_eq!(granted, 0, "trust off left grants behind");
    }

    #[test]
    fn a_confirmation_names_the_operation_not_just_a_label() {
        // The review called this out three times: the card has to show what runs.
        let run = shell_run_summary("volume-up").unwrap();
        assert!(run.contains("omarchy-audio-output-volume"), "{run}");
        assert!(run.contains("raise"), "{run}");
        let lock = shell_run_summary("lock").unwrap();
        assert!(lock.contains("omarchy-system-lock"), "{lock}");

        let fetch = fetch_summary("https://example.com/a/b?c=1#d");
        assert!(fetch.contains("/a/b?c=1#d"), "{fetch}");
        assert_eq!(fetch, "Read: https://example.com/a/b?c=1#d");
    }

    #[test]
    fn no_summary_hides_the_end_of_its_operation() {
        // Every capability at its own limit: the summary has to contain all of it.
        let typing = "x".repeat(crate::act::MAX_TYPE_CHARS);
        let window = crate::act::FocusedWindow {
            class: "foot".to_string(),
            address: "0x1".to_string(),
            title: "omarchy: ~".to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        assert!(type_summary(&window, &typing).contains(&typing));
        let clip = "y".repeat(crate::act::MAX_CLIPBOARD_CHARS);
        assert!(clipboard_summary(&clip).contains(&clip));
        // No decorative quotes: the payload is the payload, and a text with quotes in
        // it must not become ambiguous about where it ends.
        assert!(clipboard_summary("he said \"hi\"").ends_with("he said \"hi\""));
        let query = "q".repeat(crate::web::MAX_QUERY_CHARS);
        assert!(search_summary(&query).contains(&query));
        // And the display cap is what bounds them, so every one fits the row. (Kept
        // as values, not const assertions, so the reason lives next to the test.)
        let limits = [
            crate::act::MAX_TYPE_CHARS,
            crate::act::MAX_CLIPBOARD_CHARS,
            crate::web::MAX_QUERY_CHARS,
        ];
        for limit in limits {
            assert!(
                limit <= crate::web::MAX_SHOWN_URL_CHARS,
                "{limit} does not fit"
            );
        }
    }

    #[test]
    fn a_window_is_named_by_its_title_not_only_its_app() {
        let window = crate::act::FocusedWindow {
            class: "foot".to_string(),
            address: "0x1".to_string(),
            title: "omarchy: ~/dev".to_string(),
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        let label = window_label(&window);
        assert!(label.starts_with("foot"), "{label}");
        assert!(label.contains("omarchy: ~/dev"), "{label}");
        // A window with no title still names something.
        let bare = crate::act::FocusedWindow {
            title: String::new(),
            ..window.clone()
        };
        assert_eq!(window_label(&bare), "foot");
    }

    #[test]
    fn ping_capabilities_and_unknown_methods() {
        let paths = temp_paths("ping");
        let broker = broker(paths.clone());

        let pong = call(&broker, "ping", Value::Null).unwrap();
        assert_eq!(pong["version"], Value::String(VERSION.to_string()));
        assert_eq!(pong["pid"], Value::from(broker.pid()));
        assert!(
            pong["startedAt"]
                .as_str()
                .unwrap()
                .starts_with("2023-11-14T22:13:20")
        );

        let caps = call(&broker, "capabilities", Value::Null).unwrap();
        let list = caps["capabilities"].as_array().unwrap();
        assert_eq!(list.len(), 9);
        assert_eq!(list[0]["name"], Value::String("observe.screen".into()));
        assert_eq!(list[0]["grant"], Value::Null);
        assert_eq!(list[1]["name"], Value::String("act.type".into()));
        assert_eq!(list[2]["name"], Value::String("act.keys".into()));
        assert_eq!(list[3]["name"], Value::String("act.pointer".into()));
        assert_eq!(list[4]["name"], Value::String("act.clipboard".into()));
        assert_eq!(list[5]["name"], Value::String("act.launch".into()));
        assert_eq!(list[6]["name"], Value::String("shell.run".into()));
        assert_eq!(list[7]["name"], Value::String("shell.exec".into()));
        assert_eq!(list[8]["name"], Value::String("web.search".into()));
        assert!(list.iter().all(|entry| entry["grant"] == Value::Null));

        let err = call(&broker, "browser.open", Value::Null).unwrap_err();
        assert_eq!(err.code(), ErrorCode::UnknownMethod);
        assert!(
            err.message().contains("act.clipboard"),
            "the refusal lists what this daemon does know: {}",
            err.message()
        );
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn grants_are_minted_listed_and_revoked_with_an_audit_trail() {
        let paths = temp_paths("consent");
        let broker = broker(paths.clone());
        let granted = call(
            &broker,
            "consent.grant",
            json!({"capability": "observe.screen", "mode": "standing", "ttlSeconds": 600}),
        )
        .unwrap();
        assert_eq!(granted["mode"], Value::String("standing".into()));
        assert_eq!(
            granted["expiresAt"],
            Value::String("2023-11-14T22:23:20Z".into())
        );

        let caps = call(&broker, "capabilities", Value::Null).unwrap();
        assert_eq!(
            caps["capabilities"][0]["grant"]["mode"],
            Value::String("standing".into())
        );

        let listed = call(&broker, "consent.list", Value::Null).unwrap();
        assert_eq!(listed["grants"].as_array().unwrap().len(), 1);

        let revoked = call(
            &broker,
            "consent.revoke",
            json!({"capability": "observe.screen"}),
        )
        .unwrap();
        assert_eq!(revoked["revoked"], Value::Bool(true));
        let listed = call(&broker, "consent.list", Value::Null).unwrap();
        assert!(listed["grants"].as_array().unwrap().is_empty());

        // `consent.list` and `capabilities` are read-only views: they leave no audit entry.
        let tail = call(&broker, "audit.tail", json!({"lines": 10})).unwrap();
        let entries = tail.as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["method"], Value::String("consent.grant".into()));
        assert_eq!(entries[0]["outcome"], Value::String("allowed".into()));
        assert_eq!(entries[0]["grant"], Value::String("standing".into()));
        assert_eq!(
            entries[0]["capability"],
            Value::String("observe.screen".into())
        );
        assert_eq!(entries[1]["method"], Value::String("consent.revoke".into()));
        assert_eq!(entries[1]["grant"], Value::String("none".into()));
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn consent_errors_are_audited_with_their_code() {
        let paths = temp_paths("consent-errors");
        let broker = broker(paths.clone());
        let err = call(
            &broker,
            "consent.grant",
            json!({"capability": "observe.screen", "mode": "standing", "ttlSeconds": 5}),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::TtlOutOfRange);
        let err = call(
            &broker,
            "consent.grant",
            json!({"capability": "browser", "mode": "once"}),
        )
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::CapabilityUnknown);
        let err = call(&broker, "consent.grant", json!({"mode": "once"})).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidParams);

        let tail = call(&broker, "audit.tail", json!({"lines": 10})).unwrap();
        let entries = tail.as_array().unwrap();
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert_eq!(entry["outcome"], Value::String("error".into()));
            assert_eq!(entry["grant"], Value::String("none".into()));
        }
        assert_eq!(
            entries[0]["error"],
            Value::String("ttl_out_of_range".into())
        );
        assert_eq!(
            entries[1]["error"],
            Value::String("capability_unknown".into())
        );
        assert_eq!(entries[2]["error"], Value::String("invalid_params".into()));
        // Only the call that resolved a real capability names it; the other two never got that far.
        assert_eq!(
            entries[0]["capability"],
            Value::String("observe.screen".into())
        );
        assert_eq!(entries[1]["capability"], Value::Null);
        assert_eq!(entries[2]["capability"], Value::Null);
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn observe_screen_without_consent_is_denied_before_anything_runs() {
        let paths = temp_paths("denied");
        let broker = broker(paths.clone());
        let err = call(&broker, "observe.screen", json!({"target": "window"})).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        assert!(
            err.message()
                .contains("omassistantctl consent grant observe.screen --once"),
            "{}",
            err.message()
        );
        let tail = call(&broker, "audit.tail", json!({"lines": 5})).unwrap();
        let entry = &tail.as_array().unwrap()[0];
        assert_eq!(entry["outcome"], Value::String("denied".into()));
        assert_eq!(entry["grant"], Value::String("none".into()));
        assert_eq!(entry["error"], Value::String("consent_required".into()));
        assert_eq!(entry["target"], Value::String("window".into()));
        assert!(entry.get("path").is_none(), "nothing was written");
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn acting_without_consent_is_denied_and_audited_with_the_right_capability() {
        let paths = temp_paths("act-denied");
        let broker = broker(paths.clone());

        let err = call(&broker, "act.type", json!({"text": "hello"})).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        assert!(
            err.message()
                .contains("omassistantctl consent grant act.type --once"),
            "{}",
            err.message()
        );
        let err = call(&broker, "act.key", json!({"key": "super+return"})).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        let err = call(&broker, "act.clipboard", json!({"mode": "read"})).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ConsentRequired);
        assert!(
            err.message()
                .contains("omassistantctl consent grant act.clipboard --once"),
            "{}",
            err.message()
        );

        let tail = call(&broker, "audit.tail", json!({"lines": 10})).unwrap();
        let entries = tail.as_array().unwrap();
        assert_eq!(entries.len(), 3);
        for entry in entries {
            assert_eq!(entry["outcome"], Value::String("denied".into()));
            assert_eq!(entry["grant"], Value::String("none".into()));
            assert_eq!(entry["error"], Value::String("consent_required".into()));
            // Nothing was asked of the desktop, so no window facts were collected.
            assert!(entry.get("app").is_none(), "{entry}");
        }
        assert_eq!(entries[0]["capability"], Value::String("act.type".into()));
        assert_eq!(entries[0]["chars"], Value::from(5));
        assert!(entries[0].get("text").is_none(), "never the text");
        assert_eq!(entries[1]["keyCount"], Value::from(2));
        assert!(entries[1].get("chord").is_none(), "never the chord");
        assert_eq!(
            entries[2]["capability"],
            Value::String("act.clipboard".into())
        );
        assert_eq!(entries[2]["mode"], Value::String("read".into()));
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn acting_parameter_mistakes_are_refused_before_consent_is_even_looked_at() {
        let paths = temp_paths("act-params");
        let broker = broker(paths.clone());
        for (method, params, code) in [
            ("act.type", json!({}), ErrorCode::InvalidParams),
            ("act.type", json!({"text": 7}), ErrorCode::InvalidParams),
            ("act.type", json!({"text": ""}), ErrorCode::NothingToType),
            (
                "act.type",
                json!({"text": " \n\t "}),
                ErrorCode::NothingToType,
            ),
            (
                "act.type",
                json!({"text": "a\u{0}b"}),
                ErrorCode::InvalidParams,
            ),
            (
                "act.type",
                json!({"text": "x".repeat(4001)}),
                ErrorCode::TooLarge,
            ),
            ("act.key", json!({}), ErrorCode::InvalidParams),
            ("act.key", json!({"key": ""}), ErrorCode::InvalidParams),
            (
                "act.key",
                json!({"key": "hyper+x"}),
                ErrorCode::InvalidParams,
            ),
            ("act.key", json!({"key": "a+b"}), ErrorCode::InvalidParams),
            ("act.clipboard", json!({}), ErrorCode::InvalidParams),
            (
                "act.clipboard",
                json!({"mode": "copy"}),
                ErrorCode::InvalidParams,
            ),
            (
                "act.clipboard",
                json!({"mode": "write"}),
                ErrorCode::InvalidParams,
            ),
            (
                "act.clipboard",
                json!({"mode": "write", "text": "x".repeat(100_001)}),
                ErrorCode::TooLarge,
            ),
        ] {
            let err = call(&broker, method, params.clone()).unwrap_err();
            assert_eq!(err.code(), code, "for {method} {params}");
        }
        // None of those got as far as consent, so nothing was audited as denied, and nothing ran.
        let tail = call(&broker, "audit.tail", json!({"lines": 50})).unwrap();
        let entries = tail.as_array().unwrap();
        assert_eq!(entries.len(), 14);
        assert!(
            entries
                .iter()
                .all(|entry| entry["outcome"] == Value::String("error".into())),
            "a parameter mistake is an error, not a denial"
        );
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn a_granted_action_that_no_tool_can_perform_is_audited_as_an_error() {
        let paths = temp_paths("act-notools");
        let broker = broker(paths.clone());
        call(
            &broker,
            "consent.grant",
            json!({"capability": "act.type", "mode": "standing", "ttlSeconds": 600}),
        )
        .unwrap();
        let err = call(&broker, "act.type", json!({"text": "hello"})).unwrap_err();
        assert_eq!(err.code(), ErrorCode::ToolMissing);
        let tail = call(&broker, "audit.tail", json!({"lines": 5})).unwrap();
        let entry = &tail.as_array().unwrap()[1];
        assert_eq!(entry["outcome"], Value::String("error".into()));
        assert_eq!(entry["grant"], Value::String("standing".into()));
        assert_eq!(entry["error"], Value::String("tool_missing".into()));
        assert_eq!(entry["chars"], Value::from(5));
        // The standing grant is untouched by a failed call.
        let caps = call(&broker, "capabilities", Value::Null).unwrap();
        assert_eq!(caps["capabilities"][1]["grant"]["mode"], json!("standing"));
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn bad_lines_are_answered_without_an_id() {
        let paths = temp_paths("badlines");
        let broker = broker(paths.clone());
        for (line, expected) in [
            (b"{".as_slice(), "bad_json"),
            (b"[]".as_slice(), "bad_request"),
            (br#"{"id":"x"}"#.as_slice(), "bad_request"),
        ] {
            let value: Value = serde_json::from_str(&broker.handle_line(line)).unwrap();
            assert_eq!(value["ok"], Value::Bool(false));
            assert_eq!(value["error"]["code"], Value::String(expected.into()));
            assert_eq!(value["id"], Value::Null);
        }
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }

    #[test]
    fn audit_tail_bounds() {
        let paths = temp_paths("tail");
        let broker = broker(paths.clone());
        for bad in [json!(0), json!(201), json!("10"), json!(1.5), json!(-1)] {
            let err = call(&broker, "audit.tail", json!({"lines": bad})).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidParams, "for {bad}");
        }
        for good in [
            Value::Null,
            json!({"lines": null}),
            json!({"lines": 1}),
            json!({"lines": 200}),
        ] {
            let tail = call(&broker, "audit.tail", good.clone()).unwrap();
            assert!(tail.is_array(), "for {good}");
        }
        fs::remove_dir_all(paths.state_dir().parent().unwrap()).unwrap();
    }
}
