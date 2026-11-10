//! Time, as the daemon sees it: RFC 3339 timestamps, local dates for the audit files, and a clock
//! that can be frozen in tests.

use crate::error::ApiError;
use std::fmt;
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;
use time::{OffsetDateTime, UtcOffset};

/// Everything that needs "now" takes this, so tests can freeze and advance it.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current instant, already shifted to the offset this clock reports.
    fn now(&self) -> OffsetDateTime;
}

/// The wall clock, shifted into the machine's local offset at startup.
///
/// The offset is resolved **once**, before the daemon spawns any thread: `time`'s local-offset
/// lookup deliberately refuses to guess inside a multithreaded process, and we would rather pin a
/// correct local offset at start-up than silently log in UTC forever.
pub struct SystemClock {
    offset: UtcOffset,
}

impl SystemClock {
    /// The local offset if it can be determined, UTC otherwise.
    pub fn new() -> Self {
        let (offset, _) = local_offset();
        Self { offset }
    }

    /// Always UTC (useful in containers and tests).
    pub fn utc() -> Self {
        Self {
            offset: UtcOffset::UTC,
        }
    }

    /// The offset this clock reports.
    pub fn offset(&self) -> UtcOffset {
        self.offset
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc().to_offset(self.offset)
    }
}

impl fmt::Debug for SystemClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SystemClock")
            .field("offset", &self.offset)
            .finish()
    }
}

/// A clock stuck at one instant. Tests advance it by replacing the instant.
#[derive(Debug, Clone)]
pub struct FixedClock {
    at: OffsetDateTime,
}

impl FixedClock {
    /// Freeze at an instant.
    pub fn new(at: OffsetDateTime) -> Self {
        Self { at }
    }

    /// Freeze at a Unix timestamp, in UTC.
    pub fn from_unix(seconds: i64) -> Result<Self, ApiError> {
        Ok(Self {
            at: from_unix(seconds)?,
        })
    }

    /// Freeze at a Unix timestamp, in the given offset.
    pub fn from_unix_in(seconds: i64, offset: UtcOffset) -> Result<Self, ApiError> {
        Ok(Self {
            at: from_unix(seconds)?.to_offset(offset),
        })
    }

    /// Move the clock (tests only).
    pub fn set_unix(&mut self, seconds: i64) -> Result<(), ApiError> {
        self.at = from_unix(seconds)?;
        Ok(())
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.at
    }
}

/// The machine's local offset, and whether it could be determined.
///
/// `false` means "we fell back to UTC"; the daemon says so on start-up, because the audit file names
/// would then follow UTC days.
pub fn local_offset() -> (UtcOffset, bool) {
    match UtcOffset::current_local_offset() {
        Ok(offset) => (offset, true),
        Err(_) => (UtcOffset::UTC, false),
    }
}

/// A Unix timestamp as an instant (UTC).
pub fn from_unix(seconds: i64) -> Result<OffsetDateTime, ApiError> {
    OffsetDateTime::from_unix_timestamp(seconds).map_err(|err| {
        ApiError::internal(format!(
            "{seconds} is not a representable timestamp ({err})"
        ))
    })
}

/// RFC 3339 with a numeric offset, to the second: `2026-11-07T21:07:52+01:00`.
pub fn rfc3339(at: OffsetDateTime) -> Result<String, ApiError> {
    let whole_seconds = at.replace_nanosecond(0).unwrap_or(at);
    whole_seconds
        .format(&Rfc3339)
        .map_err(|err| ApiError::internal(format!("could not format a timestamp ({err})")))
}

/// The audit file's date key: `YYYY-MM-DD` in the clock's own offset.
pub fn date_key(at: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        at.year(),
        u8::from(at.month()),
        at.day()
    )
}

/// The shot file's timestamp: `YYYYMMDD-HHMMSS` in the clock's own offset.
pub fn shot_stamp(at: OffsetDateTime) -> String {
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// A clock shared between the broker, the audit log and the capture engine.
pub type SharedClock = Arc<dyn Clock>;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // 2023-11-14T22:13:20Z
    const T0: i64 = 1_700_000_000;

    #[test]
    fn rfc3339_is_whole_seconds_and_offset_aware() {
        let utc = from_unix(T0).unwrap();
        assert_eq!(rfc3339(utc).unwrap(), "2023-11-14T22:13:20Z");
        let oslo = UtcOffset::from_hms(1, 0, 0).unwrap();
        assert_eq!(
            rfc3339(utc.to_offset(oslo)).unwrap(),
            "2023-11-14T23:13:20+01:00"
        );
    }

    #[test]
    fn date_key_and_shot_stamp_follow_the_offset() {
        let utc = from_unix(T0).unwrap();
        assert_eq!(date_key(utc), "2023-11-14");
        assert_eq!(shot_stamp(utc), "20231114-221320");
        // +02:00 pushes 22:13:20Z past midnight.
        let east = UtcOffset::from_hms(2, 0, 0).unwrap();
        let shifted = utc.to_offset(east);
        assert_eq!(date_key(shifted), "2023-11-15");
        assert_eq!(shot_stamp(shifted), "20231115-001320");
    }

    #[test]
    fn fixed_clock_reports_what_it_was_given() {
        let mut clock = FixedClock::from_unix(T0).unwrap();
        assert_eq!(clock.now().unix_timestamp(), T0);
        clock.set_unix(T0 + 60).unwrap();
        assert_eq!(clock.now().unix_timestamp(), T0 + 60);
        assert!(FixedClock::from_unix(i64::MIN).is_err());
    }

    #[test]
    fn system_clock_is_consistent() {
        let clock = SystemClock::new();
        let first = clock.now().unix_timestamp();
        let second = clock.now().unix_timestamp();
        assert!((second - first).abs() <= 1);
        assert_eq!(SystemClock::utc().offset(), UtcOffset::UTC);
    }
}
