//! The client side of the socket: one request line out, one response line back.
//!
//! Shared by `omassistantctl` and by the integration tests. Transport problems surface as an
//! [`ApiError`]; a refusal from the daemon surfaces as a [`RemoteError`] carrying the daemon's code
//! and message verbatim.

use crate::error::{ApiError, ErrorCode};
use crate::protocol::{self, Frame, MAX_REQUEST_BYTES, Response};
use serde_json::{Map, Value, json};
use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// How long a client waits for an answer before giving up. Longer than the
/// broker's 55 s confirmation window plus a capture, so a slow approval is not
/// reported as a transport failure after the action already happened.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(75);

/// The largest response the shipped client will read: the broker's base64 cap
/// (4 MiB) plus room for the JSON around it.
const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

/// A refusal from the daemon: its code and its plain-words message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteError {
    /// The daemon's stable error code.
    pub code: String,
    /// The daemon's message in plain words.
    pub message: String,
}

impl RemoteError {
    /// Build one (tests and tooling).
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// One parsed answer from the daemon.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The echoed request id.
    pub id: Option<Value>,
    /// The result, when the daemon agreed.
    pub result: Option<Value>,
    /// The refusal, when it did not.
    pub error: Option<RemoteError>,
}

impl Reply {
    /// The result, or the daemon's refusal as an error.
    pub fn into_result(self) -> Result<Value, RemoteError> {
        match (self.result, self.error) {
            (Some(result), _) => Ok(result),
            (None, Some(error)) => Err(error),
            (None, None) => Err(RemoteError::new(
                "internal",
                "the daemon answered without a result or an error",
            )),
        }
    }
}

/// A connection to `omassistantd`.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    /// Connect to the daemon's socket.
    pub fn connect(path: &Path) -> Result<Self, ApiError> {
        let stream = UnixStream::connect(path).map_err(|err| {
            ApiError::new(
                ErrorCode::Unavailable,
                format!(
                    "cannot reach omassistantd at {} ({err}); start it with `omassistantd`",
                    path.display()
                ),
            )
        })?;
        stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
        stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
        })
    }

    /// Call a method and return the raw reply.
    pub fn send(&mut self, method: &str, params: Option<&Value>) -> Result<Reply, ApiError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut request = Map::new();
        request.insert("id".to_string(), json!(id));
        request.insert("method".to_string(), json!(method));
        if let Some(params) = params {
            request.insert("params".to_string(), params.clone());
        }
        let line = serde_json::to_string(&Value::Object(request))
            .map_err(|err| ApiError::internal(format!("could not encode the request: {err}")))?;
        self.send_raw(&line)
    }

    /// Send one already-encoded request line and read one answer.
    pub fn send_raw(&mut self, line: &str) -> Result<Reply, ApiError> {
        if line.len() > MAX_REQUEST_BYTES {
            return Err(ApiError::new(
                ErrorCode::RequestTooLarge,
                format!(
                    "a request may not be longer than {} KiB",
                    MAX_REQUEST_BYTES / 1024
                ),
            ));
        }
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        // A response can legitimately be as large as a vision payload (up to
        // 4 MiB of base64) plus its envelope. The request cap is not the right
        // bound for this direction.
        match protocol::read_frame(&mut self.reader, MAX_RESPONSE_BYTES)? {
            Frame::Line(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let response = Response::from_line(text.trim()).map_err(|err| {
                    ApiError::internal(format!("the daemon sent a response we cannot read: {err}"))
                })?;
                Ok(Reply {
                    id: response.id,
                    result: response.result,
                    error: response.error.map(|error| RemoteError {
                        code: error.code,
                        message: error.message,
                    }),
                })
            }
            Frame::Oversized => Err(ApiError::internal(
                "the daemon sent a response longer than this client accepts",
            )),
            Frame::Eof => Err(ApiError::new(
                ErrorCode::Unavailable,
                "the daemon closed the connection without answering",
            )),
        }
    }

    /// Call a method and return its result or the daemon's refusal.
    pub fn call(&mut self, method: &str, params: Option<&Value>) -> Result<Value, RemoteError> {
        let reply = self
            .send(method, params)
            .map_err(|err| RemoteError::new(err.code().as_str(), err.message()))?;
        reply.into_result()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn replies_resolve_to_a_result_or_a_refusal() {
        let reply = Reply {
            id: Some(json!(1)),
            result: Some(json!({"ok": true})),
            error: None,
        };
        assert_eq!(reply.into_result().unwrap(), json!({"ok": true}));

        let reply = Reply {
            id: Some(json!(1)),
            result: None,
            error: Some(RemoteError::new("consent_required", "no consent")),
        };
        let error = reply.into_result().unwrap_err();
        assert_eq!(error.code, "consent_required");
        assert_eq!(error.to_string(), "no consent");

        let reply = Reply {
            id: None,
            result: None,
            error: None,
        };
        assert_eq!(reply.into_result().unwrap_err().code, "internal");
    }

    #[test]
    fn connecting_to_nothing_is_a_clear_error() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("omassistantd-client-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("nothing.sock");
        let err = Client::connect(&socket).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Unavailable);
        assert!(err.message().contains("start it with"), "{}", err.message());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
