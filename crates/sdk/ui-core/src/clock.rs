//! The clock as a person reads it: local date and time.
//!
//! [`local_now`] is the wall clock in the local time zone. Read it instead of
//! `chrono::Local::now()`: a screenshot capture pins it
//! ([`freeze_for_capture`]) so a frame shows the same date and time on every
//! run, and an app that reads the system clock directly breaks that.
//!
//! The toolkit's monotonic and wall clock types are re-exported here too;
//! new code names them through `ui::time`.

pub use crate::time::*;

/// The local date and time every screenshot capture shows:
/// 30 June 2026, 10:42.
pub const CAPTURE_LOCAL_TIME: (i32, u32, u32, u32, u32) = (2026, 6, 30, 10, 42);

/// The current local date and time.
#[must_use]
pub fn local_now() -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::<chrono::Local>::from(crate::time::to_std(crate::time::wall()))
}

/// Today's local date.
#[must_use]
pub fn today() -> chrono::NaiveDate {
    local_now().date_naive()
}

/// Pin the wall clock to [`CAPTURE_LOCAL_TIME`] in the local time zone for
/// the rest of the process. The launcher calls this before it builds an app
/// for `--shots`; never call it in a live session.
pub fn freeze_for_capture() {
    use chrono::TimeZone;
    let (year, month, day, hour, minute) = CAPTURE_LOCAL_TIME;
    let Some(local) = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|date| date.and_hms_opt(hour, minute, 0))
        .and_then(|naive| chrono::Local.from_local_datetime(&naive).earliest())
    else {
        return;
    };
    let Ok(ms) = u64::try_from(local.timestamp_millis()) else {
        return;
    };
    if let Some(at) = crate::time::UNIX_EPOCH.checked_add(crate::time::Duration::from_millis(ms)) {
        crate::time::freeze_wall(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn a_capture_shows_the_same_local_date_and_time() {
        freeze_for_capture();
        let now = local_now();
        assert_eq!(
            (now.year(), now.month(), now.day(), now.hour(), now.minute()),
            CAPTURE_LOCAL_TIME
        );
        assert_eq!(today(), now.date_naive());
        assert_eq!(local_now(), now, "the frozen clock does not move");
    }
}
