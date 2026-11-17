//! Local wall-clock alarms, shared by apps and the host that delivers them.
use jiff::Zoned;
use jiff::civil::{Date, Time};
use jiff::tz::AmbiguousOffset;

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

    pub fn next_in(self, last_rung: Option<Date>, after: &Zoned) -> Option<Zoned> {
        if !self.valid() {
            return None;
        }
        let time = Time::new(
            i8::try_from(self.hour).ok()?,
            i8::try_from(self.minute).ok()?,
            0,
            0,
        )
        .ok()?;
        for day in 0..=8_i32 {
            let date = after.date().checked_add(jiff::Span::new().days(day)).ok()?;
            let weekday =
                1u8.checked_shl(date.weekday().to_monday_zero_offset().unsigned_abs().into())?;
            if last_rung == Some(date) || (self.weekdays != 0 && self.weekdays & weekday == 0) {
                continue;
            }
            let mut naive = date.to_datetime(time);
            for _ in 0..180 {
                let local = after.time_zone().to_ambiguous_zoned(naive);
                if !matches!(local.offset(), AmbiguousOffset::Gap { .. }) {
                    let candidate = local.earlier().ok()?;
                    if candidate > *after {
                        return Some(candidate);
                    }
                    break;
                }
                naive = naive.checked_add(jiff::SignedDuration::from_mins(1)).ok()?;
            }
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlarmSchedule {
    pub alarm: CalendarAlarm,
    pub enabled_at_ms: i64,
    pub last_rung: Option<Date>,
}
