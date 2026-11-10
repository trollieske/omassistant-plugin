//! Argument scanning and small formatting helpers shared by both binaries.
//!
//! Deliberately hand-rolled and tiny: two binaries, a handful of flags, and no dependency on a CLI
//! framework. Everything here returns `Result<_, String>` with a message a person can act on — the
//! binaries turn those into exit code 2.

use std::collections::BTreeMap;

/// Which flags a binary knows.
#[derive(Debug, Clone, Copy)]
pub struct ScanSpec {
    /// Flags that take no value, e.g. `--json`.
    pub boolean: &'static [&'static str],
    /// Flags that take the next argument or `=value`, e.g. `--socket`.
    pub valued: &'static [&'static str],
}

/// What was on the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scanned {
    /// Boolean flags that were present, without their dashes.
    pub flags: Vec<String>,
    /// Value flags that were present, without their dashes.
    pub values: BTreeMap<String, String>,
    /// Everything that was not a flag, in order.
    pub positional: Vec<String>,
}

impl Scanned {
    /// Whether a boolean flag was given.
    pub fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|seen| seen == flag)
    }

    /// A value flag's value.
    pub fn value(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).map(String::as_str)
    }
}

/// Scan `argv` (without the program name) against `spec`.
pub fn scan(argv: &[String], spec: &ScanSpec) -> Result<Scanned, String> {
    let mut scanned = Scanned::default();
    let mut index = 0;
    while index < argv.len() {
        let argument = argv[index].as_str();
        index += 1;
        if argument == "--" {
            scanned.positional.extend(argv[index..].iter().cloned());
            break;
        }
        let Some(name) = argument.strip_prefix("--") else {
            if argument.len() > 1 && argument.starts_with('-') && argument != "-" {
                if argument.trim_start_matches('-') == "h" {
                    scanned.flags.push("help".to_string());
                    continue;
                }
                return Err(format!("unknown flag \"{argument}\""));
            }
            scanned.positional.push(argument.to_string());
            continue;
        };
        let (name, inline_value) = match name.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (name, None),
        };
        if spec.boolean.contains(&name) {
            if let Some(value) = inline_value {
                return Err(format!("--{name} does not take a value (got \"{value}\")"));
            }
            scanned.flags.push(name.to_string());
            continue;
        }
        if spec.valued.contains(&name) {
            let value = match inline_value {
                Some(value) => value,
                None => match argv.get(index) {
                    Some(value) => {
                        index += 1;
                        value.clone()
                    }
                    None => return Err(format!("--{name} needs a value")),
                },
            };
            if scanned.values.insert(name.to_string(), value).is_some() {
                return Err(format!("--{name} was given twice"));
            }
            continue;
        }
        return Err(format!("unknown flag \"--{name}\""));
    }
    Ok(scanned)
}

/// Parse `90s`, `15m`, `1h`, `1h30m`, `2d` into seconds.
pub fn parse_duration(text: &str) -> Result<u64, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("a duration looks like 15m, 1h or 90s".to_string());
    }
    let mut total: u64 = 0;
    let mut digits = String::new();
    let mut segments = 0usize;
    for character in text.chars() {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        let value: u64 = digits
            .parse()
            .map_err(|_| format!("\"{text}\" is not a duration I can read (try 15m or 1h)"))?;
        digits.clear();
        let multiplier: u64 = match character {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            other => {
                return Err(format!(
                    "\"{other}\" is not a duration unit; use s, m, h or d (like 15m or 1h)"
                ));
            }
        };
        total = total.saturating_add(value.saturating_mul(multiplier));
        segments += 1;
    }
    if !digits.is_empty() {
        return Err(format!("\"{text}\" has no unit; write 15m or 1h"));
    }
    if segments == 0 {
        return Err(format!("\"{text}\" is not a duration; write 15m or 1h"));
    }
    Ok(total)
}

/// `1234` → `1.2 KiB`, for human output only.
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1} KiB", bytes as f64 / 1024.0);
    }
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn spec() -> ScanSpec {
        ScanSpec {
            boolean: &["json", "once", "help"],
            valued: &["socket", "state-dir", "lines"],
        }
    }

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn flags_values_and_positionals() {
        let scanned = scan(
            &argv(&[
                "--socket",
                "/run/x.sock",
                "observe.screen",
                "--json",
                "--lines=5",
                "extra",
            ]),
            &spec(),
        )
        .unwrap();
        assert_eq!(scanned.value("socket"), Some("/run/x.sock"));
        assert_eq!(scanned.value("lines"), Some("5"));
        assert!(scanned.has("json"));
        assert_eq!(scanned.positional, argv(&["observe.screen", "extra"]));
    }

    #[test]
    fn short_help_and_double_dash() {
        let scanned = scan(&argv(&["-h"]), &spec()).unwrap();
        assert!(scanned.has("help"));
        let scanned = scan(&argv(&["--", "--json"]), &spec()).unwrap();
        assert_eq!(scanned.positional, argv(&["--json"]));
        assert!(!scanned.has("json"));
    }

    #[test]
    fn mistakes_are_usage_errors() {
        assert!(scan(&argv(&["--nope"]), &spec()).is_err());
        assert!(scan(&argv(&["-x"]), &spec()).is_err());
        assert!(scan(&argv(&["--socket"]), &spec()).is_err());
        assert!(scan(&argv(&["--json=yes"]), &spec()).is_err());
        assert!(scan(&argv(&["--lines", "1", "--lines", "2"]), &spec()).is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("15m").unwrap(), 900);
        assert_eq!(parse_duration("1h").unwrap(), 3_600);
        assert_eq!(parse_duration("1h30m").unwrap(), 5_400);
        assert_eq!(parse_duration("2d").unwrap(), 172_800);
        assert_eq!(parse_duration(" 60s ").unwrap(), 60);
        for bad in ["", "abc", "15", "m", "15x", "-5m", "1e3s"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} should not parse");
        }
        assert!(
            parse_duration("999999999999999999999999s").is_err(),
            "an absurd number is refused"
        );
    }

    #[test]
    fn byte_sizes() {
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}
