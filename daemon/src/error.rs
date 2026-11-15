//! Error codes and the error type that travels over the wire (and through the daemon).
//!
//! Every error the broker returns carries a stable machine code and a message in plain words. The
//! audit log records the code, never the message.

use std::fmt;

/// The machine-readable error codes this daemon can return.
///
/// These are part of the wire protocol: the CLI prints the message, callers branch on the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The line was not a JSON object.
    BadJson,
    /// The JSON was valid but not a request (`method` missing or the wrong shape).
    BadRequest,
    /// A request line longer than [`crate::protocol::MAX_REQUEST_BYTES`].
    RequestTooLarge,
    /// The method is not one this daemon implements.
    UnknownMethod,
    /// A parameter is present but not acceptable.
    InvalidParams,
    /// `observe.screen` was called without an unexpired grant.
    ConsentRequired,
    /// The person declined the confirmation.
    ConsentDenied,
    /// Nobody answered the confirmation before it expired.
    ConsentTimeout,
    /// The capability is not one this daemon knows.
    CapabilityUnknown,
    /// `ttlSeconds` outside [`crate::grants::MIN_TTL_SECONDS`]..=[`crate::grants::MAX_TTL_SECONDS`].
    TtlOutOfRange,
    /// No window is focused, so there is nothing to capture or type into.
    NoWindow,
    /// The focused window is no longer the one the person approved, so nothing was done.
    FocusMoved,
    /// No monitor reports focus, so there is nothing to capture.
    NoMonitor,
    /// The geometry reported by the compositor is not capturable (zero or absurd size).
    InvalidGeometry,
    /// `grim` (or the compositor) refused the capture, or wrote nothing.
    CaptureFailed,
    /// A required external tool is not installed.
    ToolMissing,
    /// The text to type, or the clipboard content, would exceed the cap for that capability.
    TooLarge,
    /// `act.type` was given an empty or whitespace-only text.
    NothingToType,
    /// `wtype` refused (or timed out), so nothing was typed.
    WtypeFailed,
    /// `wl-copy` or `wl-paste` refused (or timed out).
    ClipboardFailed,
    /// A fixed launch command refused or timed out.
    LaunchFailed,
    /// A fixed desktop action refused or timed out.
    ActionFailed,
    /// Another daemon already listens on this socket.
    AlreadyRunning,
    /// The daemon cannot serve at all (missing environment, unusable path, dead socket).
    Unavailable,
    /// Something went wrong on our side.
    Internal,
}

impl ErrorCode {
    /// The code as it appears on the wire and in the audit log.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadJson => "bad_json",
            Self::BadRequest => "bad_request",
            Self::RequestTooLarge => "request_too_large",
            Self::UnknownMethod => "unknown_method",
            Self::InvalidParams => "invalid_params",
            Self::ConsentRequired => "consent_required",
            Self::ConsentDenied => "consent_denied",
            Self::ConsentTimeout => "consent_timeout",
            Self::CapabilityUnknown => "capability_unknown",
            Self::TtlOutOfRange => "ttl_out_of_range",
            Self::NoWindow => "no_window",
            Self::FocusMoved => "focus_moved",
            Self::NoMonitor => "no_monitor",
            Self::InvalidGeometry => "invalid_geometry",
            Self::CaptureFailed => "capture_failed",
            Self::ToolMissing => "tool_missing",
            Self::TooLarge => "too_large",
            Self::NothingToType => "nothing_to_type",
            Self::WtypeFailed => "wtype_failed",
            Self::ClipboardFailed => "clipboard_failed",
            Self::LaunchFailed => "launch_failed",
            Self::ActionFailed => "action_failed",
            Self::AlreadyRunning => "already_running",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error with a stable code and a message in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    code: ErrorCode,
    message: String,
}

impl ApiError {
    /// Build an error from a code and a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Something went wrong on our side.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    /// A parameter is present but not acceptable.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidParams, message)
    }

    /// The machine code.
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The message in plain words.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Add context in front of the message, keeping the code.
    pub fn context(self, prefix: impl fmt::Display) -> Self {
        Self {
            code: self.code,
            message: format!("{prefix}: {}", self.message),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<std::io::Error> for ApiError {
    fn from(err: std::io::Error) -> Self {
        Self::internal(err.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_snake_case_and_unique() {
        let codes = [
            ErrorCode::BadJson,
            ErrorCode::BadRequest,
            ErrorCode::RequestTooLarge,
            ErrorCode::UnknownMethod,
            ErrorCode::InvalidParams,
            ErrorCode::ConsentRequired,
            ErrorCode::CapabilityUnknown,
            ErrorCode::TtlOutOfRange,
            ErrorCode::NoWindow,
            ErrorCode::FocusMoved,
            ErrorCode::NoMonitor,
            ErrorCode::InvalidGeometry,
            ErrorCode::CaptureFailed,
            ErrorCode::ToolMissing,
            ErrorCode::TooLarge,
            ErrorCode::NothingToType,
            ErrorCode::WtypeFailed,
            ErrorCode::ClipboardFailed,
            ErrorCode::AlreadyRunning,
            ErrorCode::Unavailable,
            ErrorCode::Internal,
        ];
        let mut seen: Vec<&str> = codes.iter().map(|c| c.as_str()).collect();
        let total = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), total, "codes must be unique");
        for code in codes {
            assert!(
                code.as_str()
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{code} is not snake_case"
            );
        }
    }

    #[test]
    fn display_and_context() {
        let err = ApiError::new(ErrorCode::NoWindow, "nothing is focused");
        assert_eq!(err.to_string(), "no_window: nothing is focused");
        assert_eq!(err.message(), "nothing is focused");
        let with_context = err.context("observe.screen");
        assert_eq!(with_context.code(), ErrorCode::NoWindow);
        assert_eq!(with_context.message(), "observe.screen: nothing is focused");
    }
}
