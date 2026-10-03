//! Dropbox timestamps: `2015-05-12T15:50:38Z` (UTC, whole seconds).

use std::time::SystemTime;

use chrono::DateTime;

/// Parse a Dropbox timestamp, an RFC 3339 UTC time. `None` for anything else.
pub fn parse(text: &str) -> Option<SystemTime> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(SystemTime::from)
}

/// Format as `YYYY-MM-DDTHH:MM:SSZ`.
#[cfg(any(test, feature = "fake-server"))]
pub fn format(time: SystemTime) -> String {
    DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn round_trips_dropbox_timestamps() {
        let time = parse("2015-05-12T15:50:38Z").unwrap();
        assert_eq!(
            time.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_431_445_838
        );
        assert_eq!(format(time), "2015-05-12T15:50:38Z");
        assert_eq!(format(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        assert_eq!(
            format(parse("2024-02-29T23:59:59Z").unwrap()),
            "2024-02-29T23:59:59Z"
        );
        assert!(parse("2015-05-12 15:50:38").is_none());
        assert!(parse("2015-13-12T15:50:38Z").is_none());
    }
}
