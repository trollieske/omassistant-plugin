//! What `act.launch` starts: the verb is fixed, the app is the person's.
//!
//! The model picks a verb — `browser`, `terminal`, `editor`, `mail` — and nothing
//! else. It never names a program, a path or an argument, so a compromised or
//! confused model cannot start something the person did not configure.
//!
//! The app behind each verb comes from this machine:
//!
//! * `browser`, `terminal`, `editor` use Omarchy's own launchers, which already
//!   follow `omarchy default browser|terminal|editor`. That is the person's choice
//!   made in Omarchy, not ours to copy.
//! * `mail` has no Omarchy default, so it uses the desktop's `mailto` handler —
//!   whatever the person's desktop opens for mail — and can be pointed at a
//!   specific app in `~/.config/omassistant/config.json`:
//!
//!   ```json
//!   { "launch": { "mail": "io.github.billsecond.mailcraft.desktop" } }
//!   ```
//!
//!   The value is a **desktop entry id**, not a command. That keeps the property
//!   the rest of the daemon depends on: no string from a request ever reaches an
//!   exec. A desktop id is looked up in the XDG data dirs and handed to
//!   `gtk-launch`, which is the same mechanism the launcher uses.

use crate::error::{ApiError, ErrorCode};
use crate::exec::one_line;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

/// One desktop entry that is installed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    /// The desktop id (`folio.desktop`), which is what `gtk-launch` wants.
    pub id: String,
    /// The name a person sees in the launcher (`Files`).
    pub name: String,
}

/// The longest desktop entry id this daemon will accept.
pub const MAX_DESKTOP_ID_CHARS: usize = 160;

/// Omarchy's special workspace for minimized windows (`omarchy-hyprland-window-minimize`).
pub const MINIMIZED_WORKSPACE: &str = "special:minimized";

/// Desktop ids are file names, not paths: letters, digits and the separators
/// real ids use, ending in `.desktop`.
const DESKTOP_ID_SUFFIX: &str = ".desktop";

/// Per-verb app choices, read from the person's `config.json`.
///
/// Every value is a desktop entry id; an absent verb falls back to the defaults
/// described in the module comment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOverrides {
    /// What `browser` opens, when the person overrode Omarchy's launcher.
    pub browser: Option<String>,
    /// What `terminal` opens, when overridden.
    pub terminal: Option<String>,
    /// What `editor` opens, when overridden.
    pub editor: Option<String>,
    /// What `mail` opens. The only verb with no Omarchy default.
    pub mail: Option<String>,
}

impl LaunchOverrides {
    /// The override for one verb, if the person set one.
    #[must_use]
    pub fn for_verb(&self, verb: &str) -> Option<&str> {
        match verb {
            "browser" => self.browser.as_deref(),
            "terminal" => self.terminal.as_deref(),
            "editor" => self.editor.as_deref(),
            "mail" => self.mail.as_deref(),
            _ => None,
        }
    }

    /// Read the `launch` object of `config.json`, if it is there.
    ///
    /// A missing file is not an error: nothing has been configured yet. A file
    /// that exists but holds a bad value is an error naming the key, because a
    /// typo that silently falls back to another app is worse than a refusal.
    pub fn read(path: &Path) -> Result<Self, ApiError> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(ApiError::new(
                    ErrorCode::Unavailable,
                    format!("could not read {}: {err}", path.display()),
                ));
            }
        };
        let value: Value = serde_json::from_str(&raw).map_err(|err| {
            ApiError::invalid(format!(
                "{} is not valid JSON ({err}); fix it or remove it",
                path.display()
            ))
        })?;
        let Some(launch) = value.get("launch") else {
            return Ok(Self::default());
        };
        if launch.is_null() {
            return Ok(Self::default());
        }
        let object = launch.as_object().ok_or_else(|| {
            ApiError::invalid(
                "\"launch\" must be an object of app ids, e.g. {\"mail\": \"Mail.desktop\"}",
            )
        })?;
        let pick = |verb: &str| -> Result<Option<String>, ApiError> {
            match object.get(verb) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => {
                    if text.trim().is_empty() {
                        return Ok(None);
                    }
                    validate_desktop_id(text)
                        .map(Some)
                        .map_err(|err| err.context(format!("launch.{verb}")))
                }
                Some(_) => Err(ApiError::invalid(format!(
                    "\"launch.{verb}\" must be a desktop id like \"Mail.desktop\", not a number or an object"
                ))),
            }
        };
        Ok(Self {
            browser: pick("browser")?,
            terminal: pick("terminal")?,
            editor: pick("editor")?,
            mail: pick("mail")?,
        })
    }
}

/// Every desktop entry this machine has, newest XDG precedence first.
///
/// This is the same list the launcher shows, so "open the file manager" means
/// the app the person actually has, not one name we guessed. Entries marked
/// `NoDisplay=true` are for helpers and are left out.
#[must_use]
pub fn installed_apps() -> Vec<InstalledApp> {
    installed_apps_in(&data_dirs())
}

/// [`installed_apps`] against an explicit list of data directories.
#[must_use]
pub fn installed_apps_in(dirs: &[PathBuf]) -> Vec<InstalledApp> {
    let mut apps: Vec<InstalledApp> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir.join("applications")) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            // The first copy of an id wins, exactly like the launcher.
            if seen.iter().any(|known| known == id) {
                continue;
            }
            if validate_desktop_id(id).is_err() {
                continue;
            }
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            if raw.lines().any(|line| line.trim() == "NoDisplay=true") {
                continue;
            }
            // Only the [Desktop Entry] group describes the app itself; an action
            // group's Name is something else.
            let mut name = String::new();
            let mut in_entry = false;
            let mut has_exec = false;
            for line in raw.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_entry = line == "[Desktop Entry]";
                    continue;
                }
                if !in_entry {
                    continue;
                }
                if let Some(value) = line.strip_prefix("Name=") {
                    if name.is_empty() {
                        name = value.trim().to_string();
                    }
                } else if line.starts_with("Exec=") {
                    has_exec = true;
                }
            }
            if !has_exec {
                continue;
            }
            seen.push(id.to_string());
            apps.push(InstalledApp {
                id: id.to_string(),
                name: if name.is_empty() {
                    app_name(id)
                } else {
                    one_line(&name, 60)
                },
            });
        }
    }
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps
}

/// Is this desktop id installed here?
#[must_use]
pub fn is_installed(id: &str) -> bool {
    data_dirs()
        .iter()
        .any(|dir| dir.join("applications").join(id).is_file())
}

/// The XDG data dirs, in the order the desktop reads them.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME") {
        Some(home) if !home.is_empty() => dirs.push(PathBuf::from(home)),
        _ => {
            if let Some(home) = std::env::var_os("HOME") {
                if !home.is_empty() {
                    dirs.push(PathBuf::from(home).join(".local").join("share"));
                }
            }
        }
    }
    let list = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in list.split(':') {
        if !dir.is_empty() {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs
}

/// The app's own name from a desktop id: `io.github.billsecond.mailcraft.desktop`
/// and `MailCraft.desktop` become the string that matches the window class.
#[must_use]
pub fn app_name(id: &str) -> String {
    let stem = id.strip_suffix(DESKTOP_ID_SUFFIX).unwrap_or(id);
    // Reverse-DNS ids end in the app's own name; keep that last component.
    let last = stem.rsplit('.').next().unwrap_or(stem);
    one_line(last, 40)
}

/// A desktop id made readable for a confirmation: `io.github.billsecond.mailcraft.desktop`
/// becomes `mailcraft`. The confirmation should name the app, not a file name.
#[must_use]
pub fn display_name(id: &str) -> String {
    let label: String = app_name(id)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == ' ')
        .collect();
    if label.is_empty() {
        one_line(id, 40)
    } else {
        label
    }
}

/// Check one desktop entry id and return it unchanged.
///
/// Refused: a path separator, `..`, a leading `-` (which would read as an option
/// to `gtk-launch`), whitespace or control characters, a missing `.desktop`
/// suffix, or anything longer than [`MAX_DESKTOP_ID_CHARS`].
pub fn validate_desktop_id(raw: &str) -> Result<String, ApiError> {
    let id = raw.trim();
    if id.is_empty() {
        return Err(ApiError::invalid(
            "an app id is required, e.g. \"Mail.desktop\"",
        ));
    }
    if id.chars().count() > MAX_DESKTOP_ID_CHARS {
        return Err(ApiError::invalid(format!(
            "an app id may not be longer than {MAX_DESKTOP_ID_CHARS} characters"
        )));
    }
    if !id.ends_with(DESKTOP_ID_SUFFIX) || id.chars().count() == DESKTOP_ID_SUFFIX.chars().count() {
        return Err(ApiError::invalid(format!(
            "\"{}\" is not a desktop id; it must end in {DESKTOP_ID_SUFFIX}",
            one_line(id, 64)
        )));
    }
    if id.starts_with('-') || id.contains("..") {
        return Err(ApiError::invalid(format!(
            "\"{}\" is not a desktop id; ids are plain file names like \"Mail.desktop\"",
            one_line(id, 64)
        )));
    }
    let ok = id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+' | '-'));
    if !ok {
        return Err(ApiError::invalid(format!(
            "\"{}\" is not a desktop id; use letters, digits, '.', '_', '+' and '-' only",
            one_line(id, 64)
        )));
    }
    Ok(id.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn config_with(body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "omassistant-launch-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn a_real_desktop_id_is_accepted() {
        for id in [
            "io.github.billsecond.mailcraft.desktop",
            "HEY.desktop",
            "thunderbird.desktop",
            "org.gnome.Evolution.desktop",
        ] {
            assert_eq!(validate_desktop_id(id).unwrap(), id);
        }
        // Surrounding whitespace is the one thing worth forgiving.
        assert_eq!(
            validate_desktop_id("  HEY.desktop\n").unwrap(),
            "HEY.desktop"
        );
    }

    #[test]
    fn a_desktop_id_reads_as_an_app_name() {
        assert_eq!(
            display_name("io.github.billsecond.mailcraft.desktop"),
            "mailcraft"
        );
        assert_eq!(display_name("HEY.desktop"), "HEY");
        assert_eq!(display_name("thunderbird.desktop"), "thunderbird");
        // The raw stem is what gets matched against the window class.
        assert_eq!(
            app_name("io.github.billsecond.mailcraft.desktop"),
            "mailcraft"
        );
        assert_eq!(app_name("HEY.desktop"), "HEY");
    }

    #[test]
    fn the_app_list_is_the_launchers_list() {
        let dir = std::env::temp_dir().join(format!(
            "omassistant-apps-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let apps = dir.join("applications");
        fs::create_dir_all(&apps).unwrap();
        fs::write(
            apps.join("folio.desktop"),
            "[Desktop Entry]\nName=Files\nExec=folio %U\nCategories=System;FileManager;\n",
        )
        .unwrap();
        // NoDisplay entries are helpers, not apps a person starts.
        fs::write(
            apps.join("helper.desktop"),
            "[Desktop Entry]\nName=Helper\nExec=helper\nNoDisplay=true\n",
        )
        .unwrap();
        // An id that is not a plain file name is skipped even if the file exists.
        fs::write(
            apps.join("bad name.desktop"),
            "[Desktop Entry]\nName=Bad\nExec=bad\n",
        )
        .unwrap();

        let found = installed_apps_in(std::slice::from_ref(&dir));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].id, "folio.desktop");
        assert_eq!(found[0].name, "Files");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_command_is_not_a_desktop_id() {
        // Everything a hostile or confused value could try to smuggle in.
        for bad in [
            "betterbird",
            "flatpak run io.github.billsecond.mailcraft",
            "/usr/bin/betterbird",
            "../../bin/sh",
            "-k",
            "--help",
            "Mail.desktop -x",
            "Mail.desktop\n; rm -rf ~",
            "foo/bar.desktop",
            "Mail.desktop/../evil",
            ".desktop",
            "",
            "   ",
        ] {
            assert!(
                validate_desktop_id(bad).is_err(),
                "{bad:?} must not pass as a desktop id"
            );
        }
        assert!(validate_desktop_id(&format!("{}.desktop", "a".repeat(200))).is_err());
    }

    #[test]
    fn overrides_come_from_the_launch_object_only() {
        let path =
            config_with(r#"{"desktopWidget": false, "launch": {"mail": "MailCraft.desktop"}}"#);
        let overrides = LaunchOverrides::read(&path).unwrap();
        assert_eq!(overrides.for_verb("mail"), Some("MailCraft.desktop"));
        assert_eq!(overrides.for_verb("browser"), None);
        assert_eq!(overrides.for_verb("nonsense"), None);
    }

    #[test]
    fn a_missing_config_means_no_overrides() {
        let path = std::env::temp_dir().join("omassistant-launch-does-not-exist.json");
        let _ = fs::remove_file(&path);
        assert_eq!(
            LaunchOverrides::read(&path).unwrap(),
            LaunchOverrides::default()
        );
    }

    #[test]
    fn a_bad_value_names_the_key_instead_of_falling_back() {
        let path = config_with(r#"{"launch": {"mail": "flatpak run MailCraft"}}"#);
        let err = LaunchOverrides::read(&path).unwrap_err();
        assert!(err.message().contains("launch.mail"), "{}", err.message());

        let path = config_with(r#"{"launch": "MailCraft.desktop"}"#);
        let err = LaunchOverrides::read(&path).unwrap_err();
        assert!(
            err.message().contains("\"launch\" must be an object"),
            "{}",
            err.message()
        );

        let path = config_with(r#"{"launch": {"mail": 42}}"#);
        let err = LaunchOverrides::read(&path).unwrap_err();
        assert!(err.message().contains("launch.mail"), "{}", err.message());

        let path = config_with("{not json");
        assert!(LaunchOverrides::read(&path).is_err());
    }

    #[test]
    fn an_empty_or_absent_verb_is_no_override() {
        let path = config_with(r#"{"launch": {"mail": null, "browser": "  "}}"#);
        let overrides = LaunchOverrides::read(&path).unwrap();
        assert_eq!(overrides, LaunchOverrides::default());
    }
}
