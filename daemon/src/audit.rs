//! The audit log: one JSON line per capability or consent call, appended to
//! `<state>/audit/YYYY-MM-DD.jsonl` (0600).
//!
//! This file is the product. Every capability call leaves a trace — allowed, denied or errored —
//! and the entry never contains a window title, image bytes, a prompt, the text that was typed, the
//! clipboard content or a key chord. What it does contain: when, which daemon, which method, what
//! came out of it, which grant authorised it, which app (class only), the geometry we asked for,
//! where the file is, how big it is, how many characters or keys were involved, and the error code.

use crate::clock::{self, SharedClock};
use crate::error::{ApiError, ErrorCode};
use crate::paths::{FILE_MODE, ensure_dir, set_mode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The most lines one `audit.tail` call may return.
pub const MAX_TAIL_LINES: usize = 200;
/// The most entries one `audit.tail` call returns when the caller names no count.
pub const DEFAULT_TAIL_LINES: usize = 50;
/// How much of a single audit file `audit.tail` will read (a day of busier use still fits).
const MAX_TAIL_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// What came out of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// The call did what it said.
    Allowed,
    /// Consent was not there; nothing was captured.
    Denied,
    /// Something failed.
    Error,
}

impl Outcome {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }
}

/// Which grant authorised (or failed to authorise) the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantLabel {
    /// None was there, or the one that was there had expired.
    #[default]
    None,
    /// A one-shot grant.
    Once,
    /// A grant that lasts until it expires.
    Standing,
    /// A run-scoped approval the card minted for this answer, in one window.
    Run,
}

impl GrantLabel {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Once => "once",
            Self::Standing => "standing",
            Self::Run => "run",
        }
    }

    /// The label for a grant mode, or `Run` for a run-scoped approval.
    pub fn from_grant(grant: &crate::grants::Grant) -> Self {
        if grant.scope.as_deref() == Some("run") {
            return Self::Run;
        }
        match grant.mode {
            crate::grants::GrantMode::Once => Self::Once,
            crate::grants::GrantMode::Standing => Self::Standing,
        }
    }
}

/// One audit line.
///
/// Field names are camelCase on the wire; the short names (`ts`, `pid`) are deliberate. `title` is
/// **not** a field and must never become one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// RFC 3339, in the daemon's local offset.
    pub ts: String,
    /// The daemon's pid.
    pub pid: u32,
    /// The method that was called.
    pub method: String,
    /// Allowed, denied or error.
    pub outcome: Outcome,
    /// The capability, for capability and consent calls.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    /// Which grant authorised the call.
    pub grant: GrantLabel,
    /// The capture target (`window` or `screen`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The application class. Never the window title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// The geometry handed to the capture tool, `x,y wxh`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geometry: Option<String>,
    /// Where the capture was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The size of the capture or of the clipboard content, in bytes. Never the content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// How many characters `act.type` was asked to type. Never the characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<u64>,
    /// How many keys `act.key` was asked to press (modifiers plus one). Never the chord.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_count: Option<u64>,
    /// The clipboard direction, `read` or `write`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// The fixed launch action, e.g. `browser`. Never a caller-supplied command.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// The error code, when the call did not go through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Appends entries to the day's file and reads them back.
#[derive(Debug)]
pub struct AuditLog {
    dir: PathBuf,
    clock: SharedClock,
    lock: Mutex<()>,
}

impl AuditLog {
    /// A log that writes into `dir`.
    pub fn new(dir: PathBuf, clock: SharedClock) -> Self {
        Self {
            dir,
            clock,
            lock: Mutex::new(()),
        }
    }

    /// The directory the day files live in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Append one entry, creating the day file 0600 on first use.
    ///
    /// A failure here is a failure of the call itself: the broker refuses to report success for
    /// something it could not record.
    pub fn append(&self, entry: &AuditEntry) -> Result<(), ApiError> {
        let line = serde_json::to_string(entry).map_err(|err| {
            ApiError::internal(format!("could not encode the audit entry: {err}"))
        })?;
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        ensure_dir(&self.dir)?;
        let path = self
            .dir
            .join(format!("{}.jsonl", clock::date_key(self.clock.now())));
        let file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(FILE_MODE)
            .open(&path);
        let mut file = match file {
            Ok(file) => file,
            Err(err) => {
                return Err(ApiError::internal(format!(
                    "could not open {} for appending: {err}",
                    path.display()
                )));
            }
        };
        // 0600 applies at creation; a file left 0644 by an older build gets fixed here.
        set_mode(&path, FILE_MODE)?;
        if let Err(err) = file
            .write_all(line.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.flush())
        {
            return Err(ApiError::internal(format!(
                "could not append to {}: {err}",
                path.display()
            )));
        }
        Ok(())
    }

    /// The last `lines` entries, newest last, across the day files (newest file first).
    pub fn tail(&self, lines: usize) -> Result<Vec<Value>, ApiError> {
        let lines = lines.clamp(1, MAX_TAIL_LINES);
        let mut files = self.day_files()?;
        files.sort();
        let mut collected: Vec<Value> = Vec::new();
        for path in files.iter().rev() {
            if collected.len() >= lines {
                break;
            }
            let mut body = Vec::new();
            let file = fs::File::open(path)?;
            file.take(MAX_TAIL_FILE_BYTES).read_to_end(&mut body)?;
            let text = String::from_utf8_lossy(&body);
            let mut chunk: Vec<Value> = text
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
                .collect();
            let room = lines - collected.len();
            if chunk.len() > room {
                chunk.drain(..chunk.len() - room);
            }
            chunk.append(&mut collected);
            collected = chunk;
        }
        Ok(collected)
    }

    /// Every `*.jsonl` file in the audit directory.
    fn day_files(&self) -> Result<Vec<PathBuf>, ApiError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(ApiError::new(
                    ErrorCode::Unavailable,
                    format!("could not read {}: {err}", self.dir.display()),
                ));
            }
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| {
                ApiError::internal(format!("could not read {}: {err}", self.dir.display()))
            })?;
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".jsonl") {
                files.push(entry.path());
            }
        }
        Ok(files)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const T0: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "omassistantd-audit-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(method: &str, outcome: Outcome) -> AuditEntry {
        AuditEntry {
            ts: "2023-11-14T22:13:20Z".to_string(),
            pid: 42,
            method: method.to_string(),
            outcome,
            capability: Some("observe.screen".to_string()),
            grant: GrantLabel::Once,
            target: Some("window".to_string()),
            app: Some("foot".to_string()),
            geometry: Some("0,0 800x600".to_string()),
            path: Some("/tmp/x.png".to_string()),
            bytes: Some(1234),
            chars: None,
            key_count: None,
            mode: None,
            action: None,
            error: None,
        }
    }

    #[test]
    fn appends_one_line_per_entry_with_private_permissions() {
        let dir = temp_dir("append");
        let log = AuditLog::new(dir.clone(), Arc::new(FixedClock::from_unix(T0).unwrap()));
        log.append(&entry("observe.screen", Outcome::Allowed))
            .unwrap();
        log.append(&entry("observe.screen", Outcome::Denied))
            .unwrap();
        let file = dir.join("2023-11-14.jsonl");
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE);
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(text.lines().count(), 2);
        for line in text.lines() {
            let value: Value = serde_json::from_str(line).unwrap();
            assert!(value.is_object());
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tail_is_newest_last_across_days() {
        let dir = temp_dir("tail");
        let clock = Arc::new(FixedClock::from_unix(T0).unwrap());
        let log = AuditLog::new(dir.clone(), clock);
        log.append(&entry("first", Outcome::Allowed)).unwrap();
        log.append(&entry("second", Outcome::Denied)).unwrap();
        log.append(&entry("third", Outcome::Error)).unwrap();

        let tail = log.tail(2).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0]["method"], Value::String("second".into()));
        assert_eq!(tail[1]["method"], Value::String("third".into()));

        let all = log.tail(50).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0]["method"], Value::String("first".into()));

        // A second day file is read too, and still ordered oldest first.
        fs::write(
            dir.join("2023-11-15.jsonl"),
            "{\"ts\":\"2023-11-15T00:00:00Z\",\"pid\":42,\"method\":\"next-day\",\"outcome\":\"allowed\",\"grant\":\"none\"}\n",
        )
        .unwrap();
        let all = log.tail(10).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[3]["method"], Value::String("next-day".into()));

        // Out-of-range requests are clamped, malformed lines are skipped.
        fs::write(dir.join("2023-11-16.jsonl"), "not json at all\n").unwrap();
        assert_eq!(log.tail(0).unwrap().len(), 1);
        assert_eq!(log.tail(10_000).unwrap().len(), 4);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_audit_directory_tails_to_nothing() {
        let dir = temp_dir("empty");
        let log = AuditLog::new(
            dir.join("audit"),
            Arc::new(FixedClock::from_unix(T0).unwrap()),
        );
        assert!(log.tail(10).unwrap().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn entries_have_exactly_the_documented_keys() {
        let mut entry = entry("observe.screen", Outcome::Allowed);
        entry.error = Some("capture_failed".to_string());
        let value = serde_json::to_value(&entry).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "app",
                "bytes",
                "capability",
                "error",
                "geometry",
                "grant",
                "method",
                "outcome",
                "path",
                "pid",
                "target",
                "ts",
            ]
        );
        assert!(
            !object.contains_key("title"),
            "the title must never be logged"
        );
        assert!(
            !object.contains_key("base64"),
            "image data must never be logged"
        );
        assert_eq!(value["grant"], Value::String("once".into()));
        assert_eq!(value["outcome"], Value::String("allowed".into()));
    }

    #[test]
    fn an_acting_entry_counts_but_never_quotes() {
        // The shape of an `act.type` entry: a count, never the text.
        let mut typed = entry("act.type", Outcome::Allowed);
        typed.capability = Some("act.keys".to_string());
        typed.target = None;
        typed.geometry = None;
        typed.path = None;
        typed.bytes = None;
        typed.chars = Some(11);
        let text = serde_json::to_string(&typed).unwrap();
        assert!(text.contains("\"chars\":11"), "{text}");
        let value: Value = serde_json::from_str(&text).unwrap();
        for forbidden in ["text", "chord", "clipboard", "title"] {
            assert!(
                !value.as_object().unwrap().contains_key(forbidden),
                "{forbidden} must never be a field of an audit entry"
            );
        }

        // The shape of an `act.key` entry: how many keys, never which ones.
        let mut pressed = entry("act.key", Outcome::Allowed);
        pressed.capability = Some("act.keys".to_string());
        pressed.key_count = Some(2);
        let value = serde_json::to_value(&pressed).unwrap();
        assert_eq!(value["keyCount"], Value::from(2));
        let text = serde_json::to_string(&value).unwrap();
        assert!(!text.contains("super"), "{text}");
        assert!(!text.contains("return"), "{text}");

        // The shape of an `act.clipboard` entry: direction and size, never the content.
        let mut copied = entry("act.clipboard", Outcome::Allowed);
        copied.capability = Some("act.clipboard".to_string());
        copied.mode = Some("write".to_string());
        copied.bytes = Some(7);
        let value = serde_json::to_value(&copied).unwrap();
        assert_eq!(value["mode"], Value::String("write".into()));
        assert_eq!(value["bytes"], Value::from(7));
        assert!(value.get("text").is_none());
    }
}
