//! The clock as a person reads it: local date and time.
//!
//! [`local_now`] is the wall clock in the local time zone. Read it instead of
//! `chrono::Local::now()`: a screenshot capture pins it
//! ([`freeze_for_capture`]) so a frame shows the same date and time on every
//! run, and an app that reads the system clock directly breaks that.
//!
//! [`time_of_day`] renders a time the way the person reads clocks, in the
//! [`TimeStyle`](tilo_formats::TimeStyle) they chose in Settings. Every clock
//! display goes through it rather than a fixed `%H:%M`.
//!
//! The styles and how they read are `tilo-formats`', so a host without the
//! toolkit answers the same clock.
//!
//! Clock types and epoch readings are `ui::time`'s.

pub use tilo_formats::TimeStyle;

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
///
/// A capture draws every frame of the process from the thread that calls
/// this, so it pins the process's clock and not only its own thread's. A
/// test does not: it holds its own thread with [`crate::time::testing`].
pub fn freeze_for_capture() {
    if let Some(at) = capture_wall() {
        crate::time::pin_wall(at);
    }
}

/// The wall clock a capture shows, as a reading a test can hand its own
/// thread with [`crate::time::testing::with_wall`]: what
/// [`freeze_for_capture`] pins for the process.
#[must_use]
pub fn capture_wall() -> Option<crate::time::SystemTime> {
    use chrono::TimeZone;
    let (year, month, day, hour, minute) = CAPTURE_LOCAL_TIME;
    let local = chrono::NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|date| date.and_hms_opt(hour, minute, 0))
        .and_then(|naive| chrono::Local.from_local_datetime(&naive).earliest())?;
    let ms = u64::try_from(local.timestamp_millis()).ok()?;
    crate::time::UNIX_EPOCH.checked_add(crate::time::Duration::from_millis(ms))
}

/// The person's clock: the shared `locale.time_format` setting, kept with the rest of their
/// formats ([`crate::format::region`]).
#[must_use]
pub fn configured() -> TimeStyle {
    crate::format::region::current().time
}

/// `time` as the person reads clocks: `14:05` or `2:05 PM`.
#[must_use]
pub fn time_of_day(time: &impl chrono::Timelike) -> String {
    configured().format(time, crate::format::words())
}

/// [`time_of_day`] for a time kept as an hour (`0..24`) and a minute.
#[must_use]
pub fn hour_minute(hour: u32, minute: u32) -> String {
    configured().format_hm(hour, minute, crate::format::words())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn each_style_reads_its_example() {
        let time = chrono::NaiveTime::from_hms_opt(14, 5, 0).unwrap();
        let words = crate::format::words();
        for style in TimeStyle::ALL {
            assert_eq!(style.format(&time, words), style.example(words));
        }
        assert_eq!(TimeStyle::H24.example(words), "14:05");
        assert_eq!(TimeStyle::H12.example(words), "2:05 PM");
        let midnight = chrono::NaiveTime::from_hms_opt(0, 7, 0).unwrap();
        assert_eq!(TimeStyle::H24.format(&midnight, words), "00:07");
        assert_eq!(TimeStyle::H12.format(&midnight, words), "12:07 AM");
    }

    #[test]
    fn a_capture_shows_the_same_local_date_and_time() {
        // Pinned on this thread only: a capture pins the whole process,
        // which would stop the clock under every test beside this one.
        let at = capture_wall().expect("the capture time exists in the local zone");
        crate::time::testing::with_wall(at, || {
            let now = local_now();
            assert_eq!(
                (now.year(), now.month(), now.day(), now.hour(), now.minute()),
                CAPTURE_LOCAL_TIME
            );
            assert_eq!(today(), now.date_naive());
            assert_eq!(local_now(), now, "the frozen clock does not move");
        });
    }
}
