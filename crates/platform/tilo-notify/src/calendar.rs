//! Local wall-clock alarms, shared by apps and the host that delivers them.
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone};

/// An alarm in this device's local time. Monday is bit zero; no weekdays
/// means a single occurrence. A repeating alarm needs no network connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalendarAlarm {
    pub hour: u8,
    pub minute: u8,
    pub weekdays: u8,
}

impl CalendarAlarm {
    pub fn valid(self) -> bool {
        self.hour < 24 && self.minute < 60 && self.weekdays < 128
    }

    pub fn next_in<Tz: TimeZone>(
        self,
        last_rung: Option<NaiveDate>,
        after: &DateTime<Tz>,
    ) -> Option<DateTime<Tz>> {
        if !self.valid() {
            return None;
        }
        let time = NaiveTime::from_hms_opt(self.hour.into(), self.minute.into(), 0)?;
        for day in 0..=8_i64 {
            let date = after
                .date_naive()
                .checked_add_signed(chrono::Duration::days(day))?;
            let weekday = 1u8.checked_shl(date.weekday().num_days_from_monday())?;
            if last_rung == Some(date) || (self.weekdays != 0 && self.weekdays & weekday == 0) {
                continue;
            }
            let mut naive = date.and_time(time);
            for _ in 0..180 {
                if let Some(candidate) = after.timezone().from_local_datetime(&naive).earliest() {
                    if candidate > *after {
                        return Some(candidate);
                    }
                    break;
                }
                naive = naive.checked_add_signed(chrono::Duration::minutes(1))?;
            }
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlarmSchedule {
    pub alarm: CalendarAlarm,
    pub enabled_at_ms: i64,
    pub last_rung: Option<NaiveDate>,
}
