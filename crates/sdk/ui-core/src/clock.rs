//! The clock as a person reads it: local date and time.
//!
//! [`local_now`] is the wall clock in the local time zone. Read it instead of
//! `chrono::Local::now()`: a screenshot capture pins it
//! ([`freeze_for_capture`]) so a frame shows the same date and time on every
//! run, and an app that reads the system clock directly breaks that.
//!
//! [`time_of_day`] renders a time the way the person reads clocks, in the
//! [`TimeStyle`] they chose in Settings. Every clock display goes through it
//! rather than a fixed `%H:%M`.
//!
//! Clock types and epoch readings are `ui::time`'s.

use std::sync::{Mutex, PoisonError};

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

/// How times of day render: the shared `locale.time_format` setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TimeStyle {
    /// `14:05`
    #[default]
    H24,
    /// `2:05 PM`
    H12,
}

impl TimeStyle {
    pub const ALL: [TimeStyle; 2] = [TimeStyle::H24, TimeStyle::H12];

    /// The style the person chose, re-read once the settings change.
    #[must_use]
    pub fn configured() -> Self {
        static CONFIGURED: Mutex<Option<(Option<settings_store::SettingsStamp>, TimeStyle)>> =
            Mutex::new(None);
        let stamp = settings_store::stamp();
        let mut configured = CONFIGURED.lock().unwrap_or_else(PoisonError::into_inner);
        match *configured {
            Some((seen, style)) if seen == stamp => style,
            _ => {
                let style = settings_store::load_section("locale")
                    .remove("time_format")
                    .and_then(|value| serde_json::from_value(value).ok())
                    .unwrap_or_default();
                *configured = Some((stamp, style));
                style
            }
        }
    }

    /// `time` in this style.
    #[must_use]
    pub fn format(self, time: &impl chrono::Timelike) -> String {
        self.format_hm(time.hour(), time.minute())
    }

    /// `hour:minute` (hour `0..24`) in this style.
    #[must_use]
    pub fn format_hm(self, hour: u32, minute: u32) -> String {
        match self {
            TimeStyle::H24 => format!("{hour:02}:{minute:02}"),
            TimeStyle::H12 => {
                let half = if hour % 24 < 12 { "AM" } else { "PM" };
                let hour = match hour % 12 {
                    0 => 12,
                    hour => hour,
                };
                format!("{hour}:{minute:02} {half}")
            }
        }
    }

    #[must_use]
    pub fn example(self) -> &'static str {
        match self {
            TimeStyle::H24 => "14:05",
            TimeStyle::H12 => "2:05 PM",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            TimeStyle::H24 => "24-hour (14:05)",
            TimeStyle::H12 => "12-hour (2:05 PM)",
        }
    }
}

/// `time` as the person reads clocks: `14:05` or `2:05 PM`.
#[must_use]
pub fn time_of_day(time: &impl chrono::Timelike) -> String {
    TimeStyle::configured().format(time)
}

/// [`time_of_day`] for a time kept as an hour (`0..24`) and a minute.
#[must_use]
pub fn hour_minute(hour: u32, minute: u32) -> String {
    TimeStyle::configured().format_hm(hour, minute)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};

    #[test]
    fn each_style_reads_its_example() {
        let time = chrono::NaiveTime::from_hms_opt(14, 5, 0).unwrap();
        for style in TimeStyle::ALL {
            assert_eq!(style.format(&time), style.example());
        }
        let midnight = chrono::NaiveTime::from_hms_opt(0, 7, 0).unwrap();
        assert_eq!(TimeStyle::H24.format(&midnight), "00:07");
        assert_eq!(TimeStyle::H12.format(&midnight), "12:07 AM");
    }

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
