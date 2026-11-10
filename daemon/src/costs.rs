//! A local cost report from pi's own session files.
//!
//! Every assistant message in a pi session carries `message.usage.cost.total` (USD) and token
//! counts. This module only reads those fields; it never returns prompts, answers, images or window
//! titles. The overlay uses it for the total and per-conversation usage view; `omassistantctl costs`
//! prints the same report.
//!
//! A missing or malformed usage object is ignored, not an error: an older provider or a partial
//! line must not make the whole report unusable. Files are matched by the `pi-sessions/*.jsonl`
//! convention the plugin already uses; symlinks and giant files are skipped.

use crate::error::ApiError;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::Path;

/// A session file larger than this is skipped rather than parsed in memory.
pub const MAX_SESSION_BYTES: u64 = 128 * 1024 * 1024;

/// Cost and tokens for one conversation.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCost {
    /// The file name inside `pi-sessions/` (not an absolute path).
    pub file: String,
    /// First timestamp seen in the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Last timestamp seen in the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_at: Option<String>,
    /// Provider-reported cost, in US dollars.
    pub cost_usd: f64,
    /// Assistant messages with usage data.
    pub messages: u64,
    /// Input tokens across those messages.
    pub input_tokens: u64,
    /// Output tokens across those messages.
    pub output_tokens: u64,
    /// `provider/model` strings used in this session.
    pub models: Vec<String>,
}

/// Cost and tokens for one model across every session.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    /// `provider/model`.
    pub model: String,
    /// Provider-reported cost, in US dollars.
    pub cost_usd: f64,
    /// Assistant messages with usage data.
    pub messages: u64,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
}

/// Everything `costs` reports.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CostReport {
    /// Sum of every session, in US dollars.
    pub total_usd: f64,
    /// Newest first.
    pub sessions: Vec<SessionCost>,
    /// Most expensive first.
    pub models: Vec<ModelCost>,
}

/// Read `<state>/pi-sessions/*.jsonl` and sum the usage blocks.
///
/// A missing directory is an empty report. An unreadable directory or a file that exists but cannot
/// be opened is an error; a single malformed line is skipped.
pub fn scan(pi_sessions: &Path) -> Result<CostReport, ApiError> {
    let entries = match fs::read_dir(pi_sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CostReport::default());
        }
        Err(error) => {
            return Err(ApiError::internal(format!(
                "could not read the pi session directory {}: {error}",
                pi_sessions.display()
            )));
        }
    };

    let mut sessions: Vec<SessionCost> = Vec::new();
    let mut models: BTreeMap<String, ModelCost> = BTreeMap::new();
    let mut total = 0.0_f64;

    for entry in entries {
        let entry = entry.map_err(|error| {
            ApiError::internal(format!(
                "could not read an entry in {}: {error}",
                pi_sessions.display()
            ))
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl") {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if !metadata.file_type().is_file() || metadata.len() > MAX_SESSION_BYTES {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let file_name = file_name.to_string();
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => continue,
        };

        let mut session = SessionCost {
            file: file_name,
            ..SessionCost::default()
        };
        let mut session_models: BTreeMap<String, ()> = BTreeMap::new();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let timestamp = record
                .get("timestamp")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(timestamp) = &timestamp {
                if session.started_at.is_none() {
                    session.started_at = Some(timestamp.clone());
                }
                session.last_at = Some(timestamp.clone());
            }
            if record.get("type").and_then(Value::as_str) != Some("message") {
                continue;
            }
            let Some(message) = record.get("message") else {
                continue;
            };
            if message.get("role").and_then(Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(usage) = message.get("usage") else {
                continue;
            };
            let cost = usage
                .get("cost")
                .and_then(|cost| cost.get("total"))
                .and_then(Value::as_f64)
                .filter(|cost| cost.is_finite() && *cost >= 0.0);
            let Some(cost) = cost else { continue };
            let input = usage.get("input").and_then(Value::as_u64).unwrap_or(0);
            let output = usage.get("output").and_then(Value::as_u64).unwrap_or(0);
            let provider = message
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let model = message
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let key = match (provider.is_empty(), model.is_empty()) {
                (true, true) => "unknown".to_string(),
                (true, false) => model.to_string(),
                (false, true) => provider.to_string(),
                (false, false) => format!("{provider}/{model}"),
            };

            session.cost_usd += cost;
            session.messages = session.messages.saturating_add(1);
            session.input_tokens = session.input_tokens.saturating_add(input);
            session.output_tokens = session.output_tokens.saturating_add(output);
            session_models.insert(key.clone(), ());

            let entry = models.entry(key.clone()).or_insert_with(|| ModelCost {
                model: key,
                ..ModelCost::default()
            });
            entry.cost_usd += cost;
            entry.messages = entry.messages.saturating_add(1);
            entry.input_tokens = entry.input_tokens.saturating_add(input);
            entry.output_tokens = entry.output_tokens.saturating_add(output);
        }
        session.models = session_models.into_keys().collect();
        total += session.cost_usd;
        sessions.push(session);
    }

    // Newest first: the file name starts with the ISO timestamp, and lastAt is
    // RFC 3339, so a string sort is enough. Current sessions win ties.
    sessions.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| b.file.cmp(&a.file)));
    let mut models: Vec<ModelCost> = models.into_values().collect();
    models.sort_by(|a, b| {
        b.cost_usd
            .total_cmp(&a.cost_usd)
            .then_with(|| a.model.cmp(&b.model))
    });

    Ok(CostReport {
        total_usd: total,
        sessions,
        models,
    })
}

/// A short human line for a session file, useful in errors and tests.
pub fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::io::Write;

    fn write_session(dir: &Path, name: &str, lines: &[&str]) {
        fs::create_dir_all(dir).unwrap();
        let mut file = File::create(dir.join(name)).unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "omassistant-costs-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn totals_sessions_and_models_are_summed_without_content() {
        let dir = temp_dir("sum");
        write_session(
            &dir,
            "2026-11-10T12-57-36-000Z_a.jsonl",
            &[
                r#"{"type":"session","timestamp":"2026-11-10T12:57:36.000Z"}"#,
                r#"{"type":"message","timestamp":"2026-11-10T12:58:19.000Z","message":{"role":"assistant","provider":"alpha","model":"fast-model","usage":{"input":1614,"output":55,"cost":{"total":0.004}}}}"#,
                r#"{"type":"message","timestamp":"2026-11-10T12:58:20.000Z","message":{"role":"assistant","provider":"alpha","model":"fast-model","usage":{"input":100,"output":10,"cost":{"total":0.002}}}}"#,
            ],
        );
        write_session(
            &dir,
            "2026-11-10T13-03-06-000Z_b.jsonl",
            &[
                r#"{"type":"message","timestamp":"2026-11-10T13:03:06.000Z","message":{"role":"assistant","provider":"beta","model":"slow-model","usage":{"input":100,"output":10,"cost":{"total":0.0005}}}}"#,
                r#"not json at all"#,
                r#"{"type":"message","timestamp":"2026-11-10T13:03:07.000Z","message":{"role":"user"}}"#,
            ],
        );
        let report = scan(&dir).unwrap();
        assert!((report.total_usd - 0.0065).abs() < 1e-9);
        assert_eq!(report.sessions.len(), 2);
        // Newest first: b's last message is 13:03, a's is 12:58.
        assert_eq!(report.sessions[0].file, "2026-11-10T13-03-06-000Z_b.jsonl");
        assert_eq!(report.sessions[0].messages, 1);
        assert_eq!(report.sessions[0].models, vec!["beta/slow-model"]);
        assert_eq!(report.sessions[1].file, "2026-11-10T12-57-36-000Z_a.jsonl");
        assert!((report.sessions[1].cost_usd - 0.006).abs() < 1e-9);
        assert_eq!(report.sessions[1].messages, 2);
        assert_eq!(report.sessions[1].input_tokens, 1714);
        assert_eq!(report.sessions[1].models, vec!["alpha/fast-model"]);
        assert_eq!(report.models.len(), 2);
        assert_eq!(report.models[0].model, "alpha/fast-model");
        assert!((report.models[0].cost_usd - 0.006).abs() < 1e-9);
        assert!(!format!("{report:?}").contains("assistant message text"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_directory_is_an_empty_report_and_usage_without_cost_is_ignored() {
        let empty = scan(Path::new("/definitely/not/an/omassistant/pi-sessions")).unwrap();
        assert_eq!(empty.total_usd, 0.0);
        assert!(empty.sessions.is_empty());

        let dir = temp_dir("nocost");
        write_session(
            &dir,
            "2026-11-10T13-03-06-000Z_c.jsonl",
            &[
                r#"{"type":"message","timestamp":"2026-11-10T13:03:06.000Z","message":{"role":"assistant","usage":{"input":10,"output":2}}}"#,
            ],
        );
        let report = scan(&dir).unwrap();
        assert_eq!(report.total_usd, 0.0);
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(report.sessions[0].messages, 0);
        assert!(report.sessions[0].models.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_session_paths_are_errors_not_panics() {
        let dir = temp_dir("error");
        fs::write(dir.join("not-a-dir.jsonl"), b"{}").unwrap();
        // A file named like a directory is not an entry we recurse into; a
        // missing path was already covered. The scan must stay finite and safe.
        let report = scan(&dir).unwrap();
        assert_eq!(report.sessions.len(), 1);
        let _ = fs::remove_dir_all(&dir);
        let missing = scan(&dir);
        assert!(missing.is_ok(), "a vanished directory is an empty report");
    }

    #[test]
    fn the_error_code_for_an_unreadable_directory_is_internal() {
        let err = ApiError::internal("x");
        assert_eq!(err.code(), ErrorCode::Internal);
        assert_eq!(file_name(Path::new("/a/b/c.jsonl")), "c.jsonl");
    }
}
