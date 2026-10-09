//! The ISO 8601 timestamps Graph uses (`2017-09-14T12:34:53.912Z`).

use std::time::SystemTime;

use chrono::{DateTime, SecondsFormat, Utc};

/// Parse an RFC 3339 timestamp: `Z` or a `±hh:mm` offset, optional
/// fractional seconds. `None` for anything else.
pub fn parse(text: &str) -> Option<SystemTime> {
    cloud_source::http::parse_rfc3339(text)
}

/// Format as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn format(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_timestamps_parse_and_round_trip() {
        let plain = parse("2016-03-21T20:01:37Z").unwrap();
        assert_eq!(
            plain
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1_458_590_497
        );
        let fractional = parse("2017-09-14T12:34:53.912Z").unwrap();
        assert_eq!(format(fractional), "2017-09-14T12:34:53.912Z");
        assert_eq!(
            parse("2016-03-21T22:01:37+02:00").unwrap(),
            plain,
            "offsets are applied"
        );
        assert_eq!(format(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert!(parse("2016-13-21T20:01:37Z").is_none());
        assert!(parse("yesterday").is_none());
    }
}
