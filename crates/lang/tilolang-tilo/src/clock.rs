//! The wall clock as a program reads it: the time now, and a moment, as ISO-8601 UTC text.

/// The system time as ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`: what `now` reads.
#[must_use]
pub fn system_time() -> String {
    let secs = tilo_clock::wall()
        .duration_since(tilo_clock::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    iso_utc(secs)
}

/// Seconds since the Unix epoch as ISO-8601 UTC text.
#[must_use]
pub fn iso_utc(secs: u64) -> String {
    let (h, m, s) = (secs % 86_400 / 3600, secs % 3600 / 60, secs % 60);
    let (year, month, day) =
        tilolang::calendar::civil_from_days(i64::try_from(secs / 86_400).unwrap_or(i64::MAX));
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}
