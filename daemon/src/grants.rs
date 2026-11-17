//! The consent ledger: what the person has granted, for how long, and how a `once` grant is spent.
//!
//! Grants live in `<state>/grants.json`, 0600, replaced atomically (temp file in the same
//! directory, then rename) so a crash mid-write can never leave a half-written ledger. Expiry is
//! compared against a Unix timestamp; an expired grant is treated as no grant at all.
//!
//! A `once` grant is consumed **only** when a capture actually happened — see
//! [`crate::broker::Broker`]. A `standing` grant stays until it expires or is revoked.

use crate::error::{ApiError, ErrorCode};
use crate::paths::{FILE_MODE, ensure_dir};
use crate::protocol::MAX_REQUEST_BYTES;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The shortest TTL this daemon will accept, in seconds.
pub const MIN_TTL_SECONDS: u64 = 60;
/// The longest TTL this daemon will accept, in seconds (24 h).
pub const MAX_TTL_SECONDS: u64 = 86_400;
/// Default TTL for a `standing` grant: one hour.
pub const DEFAULT_STANDING_TTL_SECONDS: u64 = 3_600;
/// Default TTL for a `once` grant: five minutes, enough for one call.
pub const DEFAULT_ONCE_TTL_SECONDS: u64 = 300;

/// How long a grant lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrantMode {
    /// One capture, then it is spent.
    Once,
    /// Every call until it expires.
    Standing,
}

impl GrantMode {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Standing => "standing",
        }
    }

    /// Parse the wire spelling.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "once" => Some(Self::Once),
            "standing" => Some(Self::Standing),
            _ => None,
        }
    }

    /// The TTL to use when the caller does not name one.
    pub const fn default_ttl(self) -> u64 {
        match self {
            Self::Once => DEFAULT_ONCE_TTL_SECONDS,
            Self::Standing => DEFAULT_STANDING_TTL_SECONDS,
        }
    }
}

/// One grant, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    /// The capability the grant is for.
    pub capability: String,
    /// Whether it is spent by one call or lasts.
    pub mode: GrantMode,
    /// When it was minted (Unix seconds).
    pub granted_unix: i64,
    /// When it stops counting (Unix seconds).
    pub expires_unix: i64,
    /// The TTL that was requested, in seconds.
    pub ttl_seconds: u64,
    /// Why the grant exists: `manual` (a person ran `consent grant`) or `run`
    /// (the card approved a tool call for this answer). Absent on old files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// For a run-scoped approval: the window it was approved in. A run grant
    /// for keys only covers that app, and only while it stays focused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

impl Grant {
    /// Still usable at `now_unix`?
    pub fn is_live(&self, now_unix: i64) -> bool {
        now_unix < self.expires_unix
    }

    /// Usable for a call in `app`?
    ///
    /// `app` is what an approval binds to: for keys that is the window's class and
    /// the compositor's address (`foot#0x1`), so a second window of the same app is
    /// a different approval. Manual grants are not bound to anything.
    pub fn is_live_for(&self, app: Option<&str>, now_unix: i64) -> bool {
        if !self.is_live(now_unix) {
            return false;
        }
        if self.scope.as_deref() != Some("run") {
            return true;
        }
        match (&self.target, app) {
            (Some(target), Some(app)) => target == app,
            // A run approval without a known target cannot authorise anything.
            _ => false,
        }
    }
}

/// The on-disk shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerDoc {
    version: u32,
    grants: BTreeMap<String, Grant>,
}

/// The current contents of the ledger file.
const LEDGER_VERSION: u32 = 1;

/// The in-memory view of `grants.json`.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    grants: BTreeMap<String, Grant>,
}

impl Ledger {
    /// An empty ledger (nothing is granted).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Read the ledger file; a missing file is an empty ledger.
    ///
    /// A corrupt file is an error, not an empty ledger: silently forgetting grants would be a lie
    /// about what the person consented to, and the daemon refuses to start instead.
    pub fn load(path: &Path) -> Result<Self, ApiError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::empty()),
            Err(err) => {
                return Err(ApiError::new(
                    ErrorCode::Unavailable,
                    format!("could not read {}: {err}", path.display()),
                ));
            }
        };
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::empty());
        }
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(ApiError::new(
                ErrorCode::TooLarge,
                format!("{} is larger than a sane grants file", path.display()),
            ));
        }
        let doc: LedgerDoc = serde_json::from_slice(&bytes).map_err(|err| {
            ApiError::new(
                ErrorCode::Unavailable,
                format!(
                    "{} is not a grants file ({err}); move it aside to start with no grants",
                    path.display()
                ),
            )
        })?;
        if doc.version != LEDGER_VERSION {
            return Err(ApiError::new(
                ErrorCode::Unavailable,
                format!(
                    "{} was written by a newer omassistantd (version {}); move it aside to start \
                     with no grants",
                    path.display(),
                    doc.version
                ),
            ));
        }
        // The file is not trusted just because it parses: a hand-written (or
        // future-agent-written) grant must obey the same TTL bounds the API
        // enforces. Anything out of bounds is dropped, not honoured forever.
        let mut grants = doc.grants;
        grants.retain(|name, grant| {
            if name.is_empty() || grant.capability != *name {
                return false;
            }
            if grant.expires_unix <= grant.granted_unix {
                return false;
            }
            matches!(grant.scope.as_deref(), None | Some("manual") | Some("run"))
                && grant
                    .target
                    .as_deref()
                    .is_none_or(|target| !target.is_empty() && target.chars().count() <= 128)
                && u64::try_from(grant.expires_unix - grant.granted_unix)
                    .is_ok_and(|span| (MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&span))
        });
        Ok(Self { grants })
    }

    /// Replace the ledger file atomically: temp file in the same directory, then rename.
    pub fn save(&self, path: &Path) -> Result<(), ApiError> {
        let dir = path.parent().ok_or_else(|| {
            ApiError::internal(format!("{} has no parent directory", path.display()))
        })?;
        ensure_dir(dir)?;
        let doc = LedgerDoc {
            version: LEDGER_VERSION,
            grants: self.grants.clone(),
        };
        let mut body = serde_json::to_vec_pretty(&doc).map_err(|err| {
            ApiError::internal(format!("could not encode the grants file: {err}"))
        })?;
        body.push(b'\n');
        let temp = dir.join(format!(
            ".grants.json.tmp-{}-{}",
            std::process::id(),
            crate::ids::random_hex(6)
        ));
        let written = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(FILE_MODE)
                .open(&temp)?;
            file.write_all(&body)?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(err) = written {
            let _ = fs::remove_file(&temp);
            return Err(ApiError::internal(format!(
                "could not write {}: {err}",
                temp.display()
            )));
        }
        if let Err(err) = fs::rename(&temp, path) {
            let _ = fs::remove_file(&temp);
            return Err(ApiError::internal(format!(
                "could not replace {}: {err}",
                path.display()
            )));
        }
        Ok(())
    }

    /// The live grant for a capability, if any.
    pub fn live(&self, capability: &str, now_unix: i64) -> Option<Grant> {
        self.grants
            .get(capability)
            .filter(|grant| grant.is_live(now_unix))
            .cloned()
    }

    /// Grant (or extend) a capability, after checking the TTL bounds.
    pub fn grant(
        &mut self,
        capability: &str,
        mode: GrantMode,
        ttl_seconds: u64,
        now_unix: i64,
    ) -> Result<Grant, ApiError> {
        self.grant_scoped(
            capability,
            mode,
            ttl_seconds,
            Some("manual"),
            None,
            now_unix,
        )
    }

    /// Mint a grant with an explicit scope (`manual`, `run`, or none).
    pub fn grant_scoped(
        &mut self,
        capability: &str,
        mode: GrantMode,
        ttl_seconds: u64,
        scope: Option<&str>,
        target: Option<&str>,
        now_unix: i64,
    ) -> Result<Grant, ApiError> {
        if !(MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&ttl_seconds) {
            return Err(ApiError::new(
                ErrorCode::TtlOutOfRange,
                format!(
                    "ttlSeconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}, got \
                     {ttl_seconds}"
                ),
            ));
        }
        let ttl = i64::try_from(ttl_seconds).map_err(|_| {
            ApiError::new(
                ErrorCode::TtlOutOfRange,
                format!("ttlSeconds must be between {MIN_TTL_SECONDS} and {MAX_TTL_SECONDS}"),
            )
        })?;
        // ttl is bounded above by MAX_TTL_SECONDS, and saturating_add keeps a hostile clock sane.
        let expires_unix = now_unix.saturating_add(ttl);
        let granted_unix = match self.grants.get(capability) {
            // Extending the same kind of grant keeps its start and only moves the end forward.
            Some(existing) if existing.is_live(now_unix) && existing.mode == mode => {
                existing.granted_unix
            }
            _ => now_unix,
        };
        let grant = Grant {
            capability: capability.to_string(),
            mode,
            granted_unix,
            expires_unix,
            ttl_seconds,
            scope: scope.map(str::to_string),
            target: target.map(str::to_string),
        };
        self.grants.insert(capability.to_string(), grant.clone());
        self.prune(now_unix);
        Ok(grant)
    }

    /// Spend a `once` grant. Returns `true` when one was there to spend.
    pub fn consume(&mut self, capability: &str, now_unix: i64) -> bool {
        match self.grants.get(capability) {
            Some(grant) if grant.is_live(now_unix) && grant.mode == GrantMode::Once => {
                self.grants.remove(capability);
                self.prune(now_unix);
                true
            }
            _ => false,
        }
    }

    /// Remove every grant with this scope (the card's run-scoped approvals).
    /// Returns how many were live.
    pub fn revoke_scope(&mut self, scope: &str, now_unix: i64) -> usize {
        let removed: Vec<String> = self
            .grants
            .iter()
            .filter(|(_, grant)| grant.scope.as_deref() == Some(scope))
            .map(|(name, _)| name.clone())
            .collect();
        let live = removed
            .iter()
            .filter(|name| {
                self.grants
                    .get(*name)
                    .is_some_and(|grant| grant.is_live(now_unix))
            })
            .count();
        for name in removed {
            self.grants.remove(&name);
        }
        self.prune(now_unix);
        live
    }

    /// Drop every grant, the panic button. Returns how many were live at
    /// `now_unix`; expired entries are cleared too but not counted.
    pub fn revoke_all(&mut self, now_unix: i64) -> usize {
        let live = self
            .grants
            .values()
            .filter(|grant| grant.is_live(now_unix))
            .count();
        self.grants.clear();
        live
    }

    /// Revoke whatever is there. Returns `true` when a live grant was removed.
    pub fn revoke(&mut self, capability: &str, now_unix: i64) -> bool {
        let was_live = self
            .grants
            .get(capability)
            .is_some_and(|grant| grant.is_live(now_unix));
        self.grants.remove(capability);
        self.prune(now_unix);
        was_live
    }

    /// Does any live grant carry this scope?
    pub fn has_scope(&self, scope: &str) -> bool {
        self.grants
            .values()
            .any(|grant| grant.scope.as_deref() == Some(scope))
    }

    /// Remove every grant with this scope. Used to take back what trust granted,
    /// without touching the grants the person made by hand.
    pub fn revoke_scoped(&mut self, scope: &str, now_unix: i64) {
        self.grants
            .retain(|_, grant| grant.scope.as_deref() != Some(scope));
        self.prune(now_unix);
    }

    /// A live grant for `capability` usable in `app` (see [`Grant::is_live_for`]).
    pub fn live_for(&self, capability: &str, app: Option<&str>, now_unix: i64) -> Option<Grant> {
        self.grants
            .get(capability)
            .filter(|grant| grant.is_live_for(app, now_unix))
            .cloned()
    }

    /// Every live grant, sorted by capability name.
    pub fn list(&self, now_unix: i64) -> Vec<Grant> {
        self.grants
            .values()
            .filter(|grant| grant.is_live(now_unix))
            .cloned()
            .collect()
    }

    /// Drop expired entries so the file does not grow forever.
    fn prune(&mut self, now_unix: i64) {
        self.grants.retain(|_, grant| grant.is_live(now_unix));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    const T0: i64 = 1_700_000_000;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_file(tag: &str) -> std::path::PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "omassistantd-grants-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("grants.json")
    }

    #[test]
    fn ttl_bounds_are_enforced() {
        let mut ledger = Ledger::empty();
        for bad in [0u64, 1, 59, MAX_TTL_SECONDS + 1, u64::MAX] {
            let err = ledger
                .grant("observe.screen", GrantMode::Standing, bad, T0)
                .unwrap_err();
            assert_eq!(err.code(), ErrorCode::TtlOutOfRange, "ttl {bad}");
        }
        for good in [MIN_TTL_SECONDS, 300, MAX_TTL_SECONDS] {
            let grant = ledger
                .grant("observe.screen", GrantMode::Standing, good, T0)
                .unwrap();
            assert_eq!(grant.ttl_seconds, good);
            assert_eq!(grant.expires_unix, T0 + good as i64);
        }
    }

    #[test]
    fn a_hand_written_grant_outside_the_ttl_bounds_is_dropped_on_load() {
        let path = temp_file("load-bounds");
        let now = T0;
        let body = format!(
            r#"{{"version":{LEDGER_VERSION},"grants":{{
              "observe.screen":{{"capability":"observe.screen","mode":"standing",
                "grantedUnix":{now},"expiresUnix":{},"ttlSeconds":315360000,"granted":"2023-11-14T22:13:20Z",
                "expires":"2093-11-14T22:13:20Z"}},
              "act.keys":{{"capability":"act.keys","mode":"standing",
                "grantedUnix":{now},"expiresUnix":{},"ttlSeconds":900,"granted":"2023-11-14T22:13:20Z",
                "expires":"2023-11-14T22:28:20Z"}}
            }}}}"#,
            now + 315_360_000,
            now + 900
        );
        fs::write(&path, body).unwrap();
        let ledger = Ledger::load(&path).unwrap();
        let names: Vec<_> = ledger.list(now).into_iter().map(|g| g.capability).collect();
        assert_eq!(
            names,
            vec!["act.keys".to_string()],
            "the 21-year grant is dropped"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap_or(&path));
    }

    #[test]
    fn a_run_grant_is_bound_to_its_window() {
        let mut ledger = Ledger::empty();
        let grant = ledger
            .grant_scoped(
                "act.keys",
                GrantMode::Standing,
                60,
                Some("run"),
                Some("foot"),
                T0,
            )
            .unwrap();
        assert_eq!(grant.target.as_deref(), Some("foot"));
        assert!(ledger.live_for("act.keys", Some("foot"), T0).is_some());
        assert!(
            ledger.live_for("act.keys", Some("kitty"), T0).is_none(),
            "a key approval does not follow focus into another app"
        );
        assert!(
            ledger.live_for("act.keys", None, T0).is_none(),
            "a run approval is never used when the window is unknown"
        );
        // A manual grant is not window-bound.
        ledger
            .grant("act.keys", GrantMode::Standing, 60, T0)
            .unwrap();
        assert!(ledger.live_for("act.keys", Some("kitty"), T0).is_some());
    }

    #[test]
    fn revoking_run_scope_keeps_a_manual_grant() {
        let mut ledger = Ledger::empty();
        ledger
            .grant("act.type", GrantMode::Standing, 600, T0)
            .unwrap();
        ledger
            .grant_scoped(
                "act.keys",
                GrantMode::Standing,
                60,
                Some("run"),
                Some("foot"),
                T0,
            )
            .unwrap();
        assert_eq!(ledger.revoke_scope("run", T0), 1);
        assert!(ledger.live("act.type", T0).is_some());
        assert!(ledger.live("act.keys", T0).is_none());
    }

    #[test]
    fn a_grant_loaded_with_an_unknown_scope_is_dropped() {
        let path = temp_file("load-scope");
        let body = format!(
            r#"{{"version":{LEDGER_VERSION},"grants":{{
              "act.keys":{{"capability":"act.keys","mode":"standing",
                "grantedUnix":{T0},"expiresUnix":{},"ttlSeconds":600,"scope":"forever",
                "granted":"2023-11-14T22:13:20Z","expires":"2023-11-14T22:23:20Z"}}
            }}}}"#,
            T0 + 600
        );
        fs::write(&path, body).unwrap();
        let ledger = Ledger::load(&path).unwrap();
        assert!(
            ledger.list(T0).is_empty(),
            "an unknown scope is not honoured"
        );
        let _ = fs::remove_dir_all(path.parent().unwrap_or(&path));
    }

    #[test]
    fn expiry_is_exclusive() {
        let mut ledger = Ledger::empty();
        ledger
            .grant("observe.screen", GrantMode::Standing, 60, T0)
            .unwrap();
        assert!(ledger.live("observe.screen", T0).is_some());
        assert!(ledger.live("observe.screen", T0 + 59).is_some());
        assert!(ledger.live("observe.screen", T0 + 60).is_none());
        assert!(ledger.live("observe.screen", T0 + 3_600).is_none());
        assert!(ledger.list(T0 + 60).is_empty());
    }

    #[test]
    fn once_is_spent_and_standing_is_not() {
        let mut ledger = Ledger::empty();
        ledger
            .grant("observe.screen", GrantMode::Once, 300, T0)
            .unwrap();
        assert_eq!(
            ledger.live("observe.screen", T0).unwrap().mode,
            GrantMode::Once
        );
        assert!(ledger.consume("observe.screen", T0));
        assert!(ledger.live("observe.screen", T0).is_none());
        assert!(
            !ledger.consume("observe.screen", T0),
            "nothing left to spend"
        );

        ledger
            .grant("observe.screen", GrantMode::Standing, 3_600, T0)
            .unwrap();
        assert!(
            !ledger.consume("observe.screen", T0),
            "standing is not spendable"
        );
        assert!(ledger.live("observe.screen", T0).is_some());
        assert_eq!(ledger.list(T0).len(), 1);
    }

    #[test]
    fn extending_keeps_the_start_and_moves_the_end() {
        let mut ledger = Ledger::empty();
        let first = ledger
            .grant("observe.screen", GrantMode::Standing, 600, T0)
            .unwrap();
        let second = ledger
            .grant("observe.screen", GrantMode::Standing, 900, T0 + 30)
            .unwrap();
        assert_eq!(second.granted_unix, first.granted_unix);
        assert_eq!(second.expires_unix, T0 + 30 + 900);
        // A different mode replaces the grant outright.
        let third = ledger
            .grant("observe.screen", GrantMode::Once, 300, T0 + 60)
            .unwrap();
        assert_eq!(third.mode, GrantMode::Once);
        assert_eq!(third.granted_unix, T0 + 60);
        assert_eq!(third.expires_unix, T0 + 360);
    }

    #[test]
    fn revoke_reports_whether_a_live_grant_went_away() {
        let mut ledger = Ledger::empty();
        assert!(!ledger.revoke("observe.screen", T0));
        ledger
            .grant("observe.screen", GrantMode::Standing, 60, T0)
            .unwrap();
        assert!(ledger.revoke("observe.screen", T0 + 1));
        assert!(ledger.live("observe.screen", T0 + 1).is_none());
        ledger
            .grant("observe.screen", GrantMode::Standing, 60, T0)
            .unwrap();
        assert!(
            !ledger.revoke("observe.screen", T0 + 600),
            "an expired grant is nothing to revoke"
        );
    }

    #[test]
    fn round_trips_through_the_file_atomically() {
        let path = temp_file("roundtrip");
        let mut ledger = Ledger::empty();
        ledger
            .grant("observe.screen", GrantMode::Once, 300, T0)
            .unwrap();
        ledger.save(&path).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, FILE_MODE, "grants.json must be 0600");
        let loaded = Ledger::load(&path).unwrap();
        let grant = loaded.live("observe.screen", T0).unwrap();
        assert_eq!(grant.mode, GrantMode::Once);
        assert_eq!(grant.expires_unix, T0 + 300);
        // No temp file is left behind.
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_missing_file_is_an_empty_ledger_and_a_corrupt_one_is_an_error() {
        let path = temp_file("corrupt");
        assert_eq!(Ledger::load(&path).unwrap().list(T0), Vec::new());
        fs::write(&path, b"   \n").unwrap();
        assert!(Ledger::load(&path).unwrap().list(T0).is_empty());
        fs::write(&path, b"{not json").unwrap();
        let err = Ledger::load(&path).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(err.message().contains("move it aside"), "{}", err.message());
        fs::write(&path, br#"{"version":99,"grants":{}}"#).unwrap();
        let err = Ledger::load(&path).unwrap_err();
        assert!(
            err.message().contains("newer omassistantd"),
            "{}",
            err.message()
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn a_run_grant_is_bound_to_one_window_not_one_app() {
        let mut ledger = Ledger::default();
        let now = 1000;
        ledger
            .grant_scoped(
                "act.keys",
                GrantMode::Standing,
                60,
                Some("run"),
                Some("foot#0x1"),
                now,
            )
            .unwrap();

        // The window that was approved.
        assert!(
            ledger
                .live_for("act.keys", Some("foot#0x1"), now + 1)
                .is_some()
        );
        // Another window of the same app is a different place to type.
        assert!(
            ledger
                .live_for("act.keys", Some("foot#0x2"), now + 1)
                .is_none()
        );
        // And a class-only lookup (an older grant, or a compositor that reports no
        // address) must not be enough to match an address-bound one.
        assert!(ledger.live_for("act.keys", Some("foot"), now + 1).is_none());
    }
}
