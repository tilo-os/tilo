//! Wall-clock milliseconds and the RFC 3339 timestamps Discord writes.

pub fn now_ms() -> u64 {
    tilo_clock::unix_now_ms()
}

/// `ms` since the Unix epoch as `2026-10-12T08:30:00.123000+00:00`.
pub fn rfc3339(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}000+00:00",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil, for days since 1970-01-01.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "bounded calendar arithmetic on a u64 day count"
)]
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_like_discord() {
        assert_eq!(super::rfc3339(0), "1970-01-01T00:00:00.000000+00:00");
        assert_eq!(
            super::rfc3339(1_791_801_000_123),
            "2026-10-12T10:30:00.123000+00:00"
        );
    }
}
