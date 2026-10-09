//! RFC 3339 timestamps as drivews writes them (`2024-05-01T12:30:00Z`,
//! optionally with fractional seconds or an offset).

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, SecondsFormat, Utc};

/// Parse an RFC 3339 timestamp, keeping its fractional seconds. `None` for
/// anything else.
pub fn parse(text: &str) -> Option<SystemTime> {
    cloud_source::http::parse_rfc3339(text)
}

/// Format as `YYYY-MM-DDTHH:MM:SSZ` (whole seconds, UTC).
pub fn format(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Milliseconds since the epoch, as docws wants `mtime`/`btime`.
pub fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn timestamps_round_trip() {
        let time = UNIX_EPOCH + Duration::from_secs(1_714_566_600);
        assert_eq!(format(time), "2024-05-01T12:30:00Z");
        assert_eq!(parse("2024-05-01T12:30:00Z"), Some(time));
        assert_eq!(
            parse("2024-05-01T12:30:00.123Z"),
            Some(time + Duration::from_millis(123))
        );
        assert_eq!(parse("2024-05-01T14:30:00+02:00"), Some(time));
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(UNIX_EPOCH));
        assert_eq!(parse("yesterday"), None);
        assert_eq!(parse("2024-13-01T00:00:00Z"), None);
    }
}
