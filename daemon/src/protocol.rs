//! The wire protocol: one JSON object per line (LF), request at most 256 KiB.
//!
//! ```text
//! → {"id":"1","method":"observe.screen","params":{"target":"window"}}
//! ← {"id":"1","ok":true,"result":{…}}
//! ← {"id":"1","ok":false,"error":{"code":"consent_required","message":"…"}}
//! ```
//!
//! `id` and `params` are optional; `id` is echoed verbatim. A line that is not a JSON object is
//! answered with `bad_json`, a JSON value that is not a request with `bad_request`, and both
//! answers carry no `id` (there is none to echo). Anything longer than
//! [`MAX_REQUEST_BYTES`] is answered with `request_too_large` after the line has been drained.

use crate::error::ApiError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::io::{self, BufRead};

/// A request line may not be longer than this (256 KiB), excluding the newline.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;

/// A request that parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Echoed back untouched; `None` when the caller did not send one.
    pub id: Option<Value>,
    /// The method name.
    pub method: String,
    /// The parameters, empty when absent.
    pub params: Map<String, Value>,
}

/// What came out of a line of input.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseOutcome {
    /// A well-formed request.
    Request(Request),
    /// Not JSON at all.
    BadJson(String),
    /// JSON, but not a request.
    BadRequest(String),
}

/// Parse one request line (without its newline).
pub fn parse_request(line: &[u8]) -> ParseOutcome {
    let text = match std::str::from_utf8(line) {
        Ok(text) => text,
        Err(_) => return ParseOutcome::BadJson("the request is not valid UTF-8".to_string()),
    };
    let text = text.trim();
    if text.is_empty() {
        return ParseOutcome::BadJson("the request is empty; expected a JSON object".to_string());
    }
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => return ParseOutcome::BadJson(format!("the request is not valid JSON: {err}")),
    };
    let map = match value {
        Value::Object(map) => map,
        other => {
            return ParseOutcome::BadRequest(format!(
                "the request must be a JSON object, not {}",
                json_kind(&other)
            ));
        }
    };
    let id = match map.get("id") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.clone()),
    };
    let method = match map.get("method") {
        Some(Value::String(method)) if !method.is_empty() => method.clone(),
        Some(other) => {
            return ParseOutcome::BadRequest(format!(
                "\"method\" must be a non-empty string, not {}",
                json_kind(other)
            ));
        }
        None => return ParseOutcome::BadRequest("the request has no \"method\"".to_string()),
    };
    let params = match map.get("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(params)) => params.clone(),
        Some(other) => {
            return ParseOutcome::BadRequest(format!(
                "\"params\" must be an object, not {}",
                json_kind(other)
            ));
        }
    };
    ParseOutcome::Request(Request { id, method, params })
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The error body of a failed response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    /// The stable machine code, e.g. `consent_required`.
    pub code: String,
    /// The message in plain words.
    pub message: String,
}

/// One response line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The request's id, echoed.
    pub id: Option<Value>,
    /// Whether the call succeeded.
    pub ok: bool,
    /// The result, absent on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error, absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    /// A successful response.
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// A failed response.
    pub fn err(id: Option<Value>, error: &ApiError) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: error.code().as_str().to_string(),
                message: error.message().to_string(),
            }),
        }
    }

    /// Parse a response line.
    pub fn from_line(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }

    /// The line to write, without the newline.
    ///
    /// Serializing our own response types cannot fail in practice; if it somehow does, the caller
    /// still gets a well-formed error rather than nothing.
    pub fn to_line(&self) -> String {
        match serde_json::to_string(self) {
            Ok(line) => line,
            Err(_) => concat!(
                r#"{"id":null,"ok":false,"error":{"code":"internal","#,
                r#""message":"the response could not be encoded"}}"#
            )
            .to_string(),
        }
    }
}

/// One line read off the socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// A line, with the newline and any trailing carriage return removed.
    Line(Vec<u8>),
    /// A line longer than the cap; it was drained and dropped.
    Oversized,
    /// The peer closed the connection.
    Eof,
}

/// Read one newline-terminated frame, refusing to buffer more than `max_bytes`.
///
/// The oversized case still consumes input up to and including the newline, so a caller can answer
/// with an error and keep the connection usable.
pub fn read_frame<R: BufRead>(reader: &mut R, max_bytes: usize) -> io::Result<Frame> {
    let mut buffer: Vec<u8> = Vec::new();
    let mut oversized = false;
    loop {
        let (consumed, saw_newline) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if buffer.is_empty() && !oversized {
                    return Ok(Frame::Eof);
                }
                break;
            }
            match available.iter().position(|byte| *byte == b'\n') {
                Some(position) => {
                    push_capped(
                        &mut buffer,
                        &available[..position],
                        max_bytes,
                        &mut oversized,
                    );
                    (position + 1, true)
                }
                None => {
                    let length = available.len();
                    push_capped(&mut buffer, available, max_bytes, &mut oversized);
                    (length, false)
                }
            }
        };
        reader.consume(consumed);
        if saw_newline {
            break;
        }
    }
    if oversized {
        return Ok(Frame::Oversized);
    }
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
    Ok(Frame::Line(buffer))
}

fn push_capped(buffer: &mut Vec<u8>, chunk: &[u8], max_bytes: usize, oversized: &mut bool) {
    if *oversized {
        return;
    }
    if buffer.len().saturating_add(chunk.len()) > max_bytes {
        *oversized = true;
        buffer.clear();
        buffer.shrink_to_fit();
        return;
    }
    buffer.extend_from_slice(chunk);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn frames(input: &[u8], cap: usize) -> Vec<Frame> {
        let mut reader = Cursor::new(input.to_vec());
        let mut out = Vec::new();
        loop {
            let frame = read_frame(&mut reader, cap).unwrap();
            let done = frame == Frame::Eof;
            out.push(frame);
            if done {
                return out;
            }
        }
    }

    #[test]
    fn reads_every_line() {
        let input = b"{\"method\":\"ping\"}\n{\"method\":\"capabilities\"}\n";
        let got = frames(input, 1024);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], Frame::Line(b"{\"method\":\"ping\"}".to_vec()));
        assert_eq!(
            got[1],
            Frame::Line(b"{\"method\":\"capabilities\"}".to_vec())
        );
        assert_eq!(got[2], Frame::Eof);
    }

    #[test]
    fn a_partial_last_line_is_still_a_line() {
        let got = frames(b"{\"method\":\"ping\"}", 1024);
        assert_eq!(
            got,
            vec![Frame::Line(b"{\"method\":\"ping\"}".to_vec()), Frame::Eof]
        );
    }

    #[test]
    fn a_trailing_carriage_return_is_dropped() {
        let got = frames(b"{\"method\":\"ping\"}\r\n", 1024);
        assert_eq!(got[0], Frame::Line(b"{\"method\":\"ping\"}".to_vec()));
    }

    #[test]
    fn the_cap_is_inclusive_and_oversized_lines_are_drained() {
        let line = vec![b'a'; 16];
        let second = b"{\"a\":1}".to_vec();
        let mut input = Vec::new();
        input.extend_from_slice(&line);
        input.push(b'\n');
        input.extend_from_slice(&second);
        input.push(b'\n');
        // Exactly at the cap: accepted.
        let got = frames(&input, 16);
        assert_eq!(got[0], Frame::Line(line.clone()));
        assert_eq!(got[1], Frame::Line(second.clone()));
        // One byte less: the first line is oversized, the second still arrives.
        let got = frames(&input, 15);
        assert_eq!(got[0], Frame::Oversized);
        assert_eq!(got[1], Frame::Line(second));
        assert_eq!(got[2], Frame::Eof);
    }

    #[test]
    fn an_oversized_line_without_a_newline_ends_at_eof() {
        let input = vec![b'a'; 100];
        assert_eq!(frames(&input, 16), vec![Frame::Oversized, Frame::Eof]);
    }

    #[test]
    fn parses_a_request_with_id_and_params() {
        let outcome = parse_request(
            br#"{"id":"abc","method":"observe.screen","params":{"target":"screen"}}"#,
        );
        let request = match outcome {
            ParseOutcome::Request(request) => request,
            other => panic!("expected a request, got {other:?}"),
        };
        assert_eq!(request.id, Some(Value::String("abc".to_string())));
        assert_eq!(request.method, "observe.screen");
        assert_eq!(
            request.params.get("target"),
            Some(&Value::String("screen".to_string()))
        );
    }

    #[test]
    fn parses_a_request_without_id_or_params() {
        let request = match parse_request(b"{\"method\":\"ping\"}") {
            ParseOutcome::Request(request) => request,
            other => panic!("expected a request, got {other:?}"),
        };
        assert_eq!(request.id, None);
        assert!(request.params.is_empty());
    }

    #[test]
    fn ids_of_any_json_type_are_echoed() {
        for raw in [
            r#"{"id":7,"method":"ping"}"#,
            r#"{"id":{"nested":[1]},"method":"ping"}"#,
            r#"{"id":null,"method":"ping"}"#,
        ] {
            let request = match parse_request(raw.as_bytes()) {
                ParseOutcome::Request(request) => request,
                other => panic!("expected a request, got {other:?}"),
            };
            if raw.contains("null") {
                assert_eq!(request.id, None, "an explicit null id is no id");
            } else {
                assert!(request.id.is_some());
            }
        }
    }

    #[test]
    fn malformed_input_is_reported_not_guessed() {
        match parse_request(b"{") {
            ParseOutcome::BadJson(message) => {
                assert!(message.contains("not valid JSON"), "{message}")
            }
            other => panic!("expected bad_json, got {other:?}"),
        }
        match parse_request(b"") {
            ParseOutcome::BadJson(_) => {}
            other => panic!("expected bad_json, got {other:?}"),
        }
        match parse_request(&[0xff, 0xfe]) {
            ParseOutcome::BadJson(message) => assert!(message.contains("UTF-8")),
            other => panic!("expected bad_json, got {other:?}"),
        }
        for raw in [
            b"[1,2]".as_slice(),
            br#""just a string""#,
            br#"{"id":"1"}"#,
            br#"{"method":123}"#,
            br#"{"method":""}"#,
            br#"{"method":"ping","params":[]}"#,
            b"null",
        ] {
            match parse_request(raw) {
                ParseOutcome::BadRequest(_) => {}
                other => panic!("expected bad_request for {raw:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn responses_are_one_line_and_shaped_as_documented() {
        let ok = Response::ok(Some(Value::String("1".into())), serde_json::json!({"x":1}));
        let line = ok.to_line();
        assert!(!line.contains('\n'));
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], Value::String("1".into()));
        assert_eq!(value["ok"], Value::Bool(true));
        assert_eq!(value["result"]["x"], Value::from(1));
        assert!(value.get("error").is_none());

        let err = ApiError::new(crate::error::ErrorCode::UnknownMethod, "no such method");
        let line = Response::err(None, &err).to_line();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["ok"], Value::Bool(false));
        assert_eq!(
            value["error"]["code"],
            Value::String("unknown_method".into())
        );
        assert_eq!(
            value["error"]["message"],
            Value::String("no such method".into())
        );
        assert!(value.get("result").is_none());
        assert_eq!(value["id"], Value::Null);
    }
}
