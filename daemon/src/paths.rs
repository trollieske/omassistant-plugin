//! Where the daemon's files live, and how their permissions are kept.
//!
//! * socket  `$XDG_RUNTIME_DIR/omassistantd.sock` (0600, parent created 0700 when missing)
//! * state   `$XDG_STATE_HOME/omassistant` or `~/.local/state/omassistant` (0700)
//! * audit   `<state>/audit/YYYY-MM-DD.jsonl` (0600, append-only in practice)
//! * shots   `<state>/shots/YYYYMMDD-HHMMSS-<6 hex>.png` (0600)
//! * grants  `<state>/grants.json` (0600, replaced atomically)
//!
//! Both binaries accept `--socket` and `--state-dir` overrides so tests never touch the real ones.

use crate::error::{ApiError, ErrorCode};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

/// Directories this daemon creates are private to the person.
pub const DIR_MODE: u32 = 0o700;
/// Files this daemon creates are private to the person.
pub const FILE_MODE: u32 = 0o600;
/// The longest file name component this daemon will turn into a path.
pub const MAX_FILE_NAME_BYTES: usize = 200;

/// The resolved socket and state locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    socket: PathBuf,
    state_dir: PathBuf,
}

impl Paths {
    /// Build from explicit locations.
    pub fn new(socket: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            socket: socket.into(),
            state_dir: state_dir.into(),
        }
    }

    /// Resolve the defaults, honouring the overrides the CLI passes in.
    pub fn resolve(
        socket_override: Option<PathBuf>,
        state_override: Option<PathBuf>,
    ) -> Result<Self, ApiError> {
        let socket = match socket_override {
            Some(path) => path,
            None => default_socket_path()?,
        };
        let state_dir = match state_override {
            Some(path) => path,
            None => default_state_dir()?,
        };
        Ok(Self { socket, state_dir })
    }

    /// The Unix socket the daemon listens on.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The state directory (`omassistant`).
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// `<state>/audit`.
    pub fn audit_dir(&self) -> PathBuf {
        self.state_dir.join("audit")
    }

    /// `<state>/shots`.
    pub fn shots_dir(&self) -> PathBuf {
        self.state_dir.join("shots")
    }

    /// `<state>/grants.json`.
    pub fn grants_file(&self) -> PathBuf {
        self.state_dir.join("grants.json")
    }

    /// Create the state directory tree, each level 0700.
    pub fn ensure_state(&self) -> Result<(), ApiError> {
        ensure_dir(&self.state_dir)?;
        ensure_dir(&self.audit_dir())?;
        ensure_dir(&self.shots_dir())?;
        Ok(())
    }

    /// The audit file for a `YYYY-MM-DD` key.
    pub fn audit_file(&self, date_key: &str) -> Result<PathBuf, ApiError> {
        if !is_date_key(date_key) {
            return Err(ApiError::internal(format!(
                "{date_key:?} is not a YYYY-MM-DD date key"
            )));
        }
        Ok(self.audit_dir().join(format!("{date_key}.jsonl")))
    }

    /// A path inside `shots/` for a generated file name.
    ///
    /// The name never comes from a caller, but it is validated anyway: a shot may only ever be
    /// written inside `shots/`, with no separators and no `..`.
    pub fn shot_path(&self, name: &str) -> Result<PathBuf, ApiError> {
        validate_file_name(name)?;
        if !is_shot_name(name) {
            return Err(ApiError::internal(format!(
                "{name:?} is not a YYYYMMDD-HHMMSS-<hex>.png shot name"
            )));
        }
        Ok(self.shots_dir().join(name))
    }

    /// A temporary path inside `shots/` for a generated file name.
    pub fn scratch_path(&self, name: &str) -> Result<PathBuf, ApiError> {
        validate_file_name(name)?;
        Ok(self.shots_dir().join(name))
    }
}

/// `$XDG_RUNTIME_DIR/omassistantd.sock`.
pub fn default_socket_path() -> Result<PathBuf, ApiError> {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => Ok(PathBuf::from(dir).join("omassistantd.sock")),
        _ => Err(ApiError::new(
            ErrorCode::Unavailable,
            "XDG_RUNTIME_DIR is not set, so there is no socket path to use; pass --socket <path> \
             (or run this inside a graphical session)",
        )),
    }
}

/// `$XDG_STATE_HOME/omassistant` or `~/.local/state/omassistant`.
pub fn default_state_dir() -> Result<PathBuf, ApiError> {
    if let Some(dir) = std::env::var_os("XDG_STATE_HOME") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir).join("omassistant"));
        }
    }
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("omassistant")),
        _ => Err(ApiError::new(
            ErrorCode::Unavailable,
            "neither XDG_STATE_HOME nor HOME is set, so there is nowhere to keep grants and \
             audit entries; pass --state-dir <path>",
        )),
    }
}

/// Create a directory if it is missing, and make it 0700 **only when we created it**.
pub fn ensure_dir(path: &Path) -> Result<(), ApiError> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(ApiError::new(
            ErrorCode::Unavailable,
            format!("{} exists and is not a directory", path.display()),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|err| {
                ApiError::new(
                    ErrorCode::Unavailable,
                    format!("could not create {}: {err}", path.display()),
                )
            })?;
            set_mode(path, DIR_MODE)
        }
        Err(err) => Err(ApiError::new(
            ErrorCode::Unavailable,
            format!("could not inspect {}: {err}", path.display()),
        )),
    }
}

/// chmod a path.
pub fn set_mode(path: &Path, mode: u32) -> Result<(), ApiError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|err| {
        ApiError::internal(format!(
            "could not set mode {mode:o} on {}: {err}",
            path.display()
        ))
    })
}

/// Reject anything that is not a single, ordinary path component.
pub fn validate_file_name(name: &str) -> Result<(), ApiError> {
    if name.is_empty() {
        return Err(ApiError::internal(
            "an empty file name is not a path".to_string(),
        ));
    }
    if name.len() > MAX_FILE_NAME_BYTES {
        return Err(ApiError::internal(format!(
            "a file name may not be longer than {MAX_FILE_NAME_BYTES} bytes"
        )));
    }
    if name.starts_with('.') {
        return Err(ApiError::internal(format!(
            "{name:?} would be a hidden file"
        )));
    }
    if name.bytes().any(|b| b.is_ascii_control()) {
        return Err(ApiError::internal(format!(
            "{name:?} contains a control character"
        )));
    }
    let as_path = Path::new(name);
    if as_path.components().count() != 1 {
        return Err(ApiError::internal(format!(
            "{name:?} is not a single path component"
        )));
    }
    match as_path.components().next() {
        Some(Component::Normal(part)) if part == OsStr::new(name) => Ok(()),
        _ => Err(ApiError::internal(format!(
            "{name:?} is not an ordinary file name"
        ))),
    }
}

/// `YYYYMMDD-HHMMSS-<6 hex>.png`.
pub fn is_shot_name(name: &str) -> bool {
    let stem = match name.strip_suffix(".png") {
        Some(stem) => stem,
        None => return false,
    };
    let bytes = stem.as_bytes();
    if bytes.len() != 8 + 1 + 6 + 1 + 6 {
        return false;
    }
    let digits = |from: usize, to: usize| bytes[from..to].iter().all(u8::is_ascii_digit);
    let separators = bytes[8] == b'-' && bytes[15] == b'-';
    let hex = bytes[16..].iter().all(u8::is_ascii_hexdigit)
        && bytes[16..].iter().all(|b| !b.is_ascii_uppercase());
    digits(0, 8) && separators && digits(9, 15) && hex
}

/// `YYYY-MM-DD`.
pub fn is_date_key(key: &str) -> bool {
    let bytes = key.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "omassistantd-paths-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_names_must_be_single_components() {
        for bad in [
            "",
            "../escape.png",
            "a/b.png",
            "./x.png",
            ".hidden",
            "..",
            "/etc/passwd",
            "with\0nul",
            "line\nbreak",
        ] {
            assert!(
                validate_file_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
        for good in [
            "20261107-210752-9f3a2c.png",
            "vision-abc.jpg",
            "grants.json",
        ] {
            assert!(validate_file_name(good).is_ok(), "{good:?} should pass");
        }
    }

    #[test]
    fn shot_names_are_checked() {
        assert!(is_shot_name("20261107-210752-9f3a2c.png"));
        assert!(!is_shot_name("20261107-210752-9F3A2C.png"));
        assert!(!is_shot_name("20261107-210752-9f3a2c.jpg"));
        assert!(!is_shot_name("2026110-210752-9f3a2c.png"));
        assert!(!is_shot_name("20261107_210752-9f3a2c.png"));
        assert!(!is_shot_name("../20261107-210752-9f3a2c.png"));
    }

    #[test]
    fn date_keys_are_checked() {
        assert!(is_date_key("2026-11-07"));
        assert!(!is_date_key("2026-1-07"));
        assert!(!is_date_key("20261107"));
        assert!(!is_date_key("2026-11-07.jsonl"));
        assert!(!is_date_key("../2026-11-07"));
    }

    #[test]
    fn shot_paths_stay_inside_the_shots_dir() {
        let dir = temp_dir("shots");
        let paths = Paths::new(dir.join("run.sock"), dir.join("state"));
        let good = paths.shot_path("20261107-210752-9f3a2c.png").unwrap();
        assert_eq!(good, paths.shots_dir().join("20261107-210752-9f3a2c.png"));
        assert!(paths.shot_path("../../etc/passwd").is_err());
        assert!(paths.shot_path("/etc/passwd").is_err());
        assert!(paths.shot_path("notes.txt").is_err());
        let audit = paths.audit_file("2026-11-07").unwrap();
        assert_eq!(audit, paths.audit_dir().join("2026-11-07.jsonl"));
        assert!(paths.audit_file("../../2026-11-07").is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn state_tree_is_private() {
        let dir = temp_dir("state");
        let paths = Paths::new(dir.join("run.sock"), dir.join("state"));
        paths.ensure_state().unwrap();
        for path in [
            paths.state_dir().to_path_buf(),
            paths.audit_dir(),
            paths.shots_dir(),
        ] {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, DIR_MODE, "{} should be 0700", path.display());
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn existing_directories_keep_their_mode() {
        let dir = temp_dir("existing");
        let shared = dir.join("shared");
        fs::create_dir(&shared).unwrap();
        set_mode(&shared, 0o755).unwrap();
        ensure_dir(&shared).unwrap();
        let mode = fs::metadata(&shared).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o755,
            "we must not chmod a directory we did not create"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn defaults_need_the_environment() {
        // The real environment of this machine has both set; only assert the shapes.
        if let Ok(socket) = default_socket_path() {
            assert_eq!(socket.file_name().unwrap(), "omassistantd.sock");
        }
        if let Ok(state) = default_state_dir() {
            assert_eq!(state.file_name().unwrap(), "omassistant");
        }
        assert!(
            Paths::resolve(
                Some(PathBuf::from("/tmp/x.sock")),
                Some(PathBuf::from("/tmp/s"))
            )
            .is_ok()
        );
    }
}
