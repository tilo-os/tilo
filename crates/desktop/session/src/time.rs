//! The device's time as the person reads it: the minute in their style and zone, and the timer
//! Clocks floats beside it. Published again when either moves, so a view that shows the time runs
//! again then and at no other time.
//!
//! Whether the clock is synchronised is published beside it (`session:clock`): a code made from a
//! seed and the clock is only as good as the clock, and an app that shows one asks this before it
//! trusts what it makes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

pub(crate) const RESOURCE: &str = "session:time";

/// The fact that the time can be trusted, served beside the time itself.
pub(crate) const CLOCK: &str = "session:clock";

/// The time as the session publishes it: the kind a reader with no words of its own knows it by.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Time")]
pub(crate) struct TimeRoot;

/// The clock as the session publishes it: the kind a reader with no words of its own knows it by.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Clock")]
pub(crate) struct ClockRoot;

pub(crate) use tilo_device_surfaces::time::Time;
pub(crate) use tilo_device_surfaces::clock::Clock;

/// Whether this device's clock is synchronised now, as the platform answers it.
fn clock() -> Clock {
    Clock {
        synchronised: tilo_clock::is_synchronised(),
    }
}

/// The time now, as the session publishes it: the minute, and today as a calendar reads it — the
/// day, its weekday, the month's name and the month's own shape, in the person's formats and
/// language, so a view that draws a month needs no clock and no words of its own.
fn now(activities: &crate::shell::activities::Activities) -> Time {
    let local = ui::clock::local_now();
    let day = local.datetime().date();
    let formats = ui::format::region::current();
    let words = ui::format::words();
    let week = tilo_formats::week(formats);
    let first = day
        .checked_sub(jiff::Span::new().days(i64::from(day.day()) - 1))
        .unwrap_or(day);
    let mut time = Time {
        clock: ui::clock::time_of_day(local),
        year: i64::from(day.year()),
        month: i64::from(day.month()),
        day: i64::from(day.day()),
        weekday: i64::from(day.weekday().to_monday_one_offset()),
        week_start: i64::from(formats.first_day.weekday().to_monday_one_offset()),
        month_name: tilo_formats::month_name(day.month(), words),
        days_in_month: i64::from(day.days_in_month()),
        first_weekday: i64::from(first.weekday().to_monday_one_offset()),
        weekdays: week
            .iter()
            .map(|weekday| tilo_formats::weekday_short(*weekday, words))
            .collect(),
        weekday_names: week
            .iter()
            .map(|weekday| tilo_formats::weekday_name(*weekday, words))
            .collect(),
        ..Time::default()
    };
    let Some(published) = activities.published(crate::shell::activities::CLOCKS_APP) else {
        return time;
    };
    let Some(summary) = published.summary.filter(|_| published.overlay) else {
        return time;
    };
    let countdown = summary.countdown.unwrap_or_default();
    let stopwatch = summary.kind == ui::live_activity::Kind::Stopwatch;
    time.timer = if !summary.title.is_empty() {
        summary.title
    } else if stopwatch {
        "Stopwatch".to_owned()
    } else {
        "Timer".to_owned()
    };
    time.timer_left = ui::format::timecode(u64::try_from(countdown).unwrap_or_default());
    // A paused timer is one with time still to run before it is due.
    time.timer_paused = summary.tone == ui::live_activity::Tone::Neutral && countdown > 0;
    time
}

/// The time as the session's services publish it, and the clock fact beside it.
pub(crate) struct Served {
    published: Time,
    clock: Clock,
    activities: std::rc::Rc<crate::shell::activities::Activities>,
}

impl Served {
    pub(crate) fn new(activities: std::rc::Rc<crate::shell::activities::Activities>) -> Self {
        Self {
            published: Time::default(),
            clock: Clock::default(),
            activities,
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            offer::<NoEffect>(
                tilo_resource::LiveRow::typed::<TimeRoot>(RESOURCE, crate::i18n::t("session-time"))
                    .into_resource(),
            ),
            offer::<NoEffect>(
                tilo_resource::LiveRow::typed::<ClockRoot>(CLOCK, crate::i18n::t("session-clock"))
                    .into_resource(),
            ),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        match resource {
            RESOURCE => Some(reading(&self.published)),
            CLOCK => Some(reading(&self.clock)),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(ActionAddress::new(resource, action)))
    }

    fn poll(&mut self) -> bool {
        let now = now(&self.activities);
        // As often as the minute moves, which is this service's own cadence: asking the platform
        // runs a program, and the answer outlives a second either way.
        let clock = clock();
        if now == self.published && clock == self.clock {
            return false;
        }
        self.published = now;
        self.clock = clock;
        true
    }

    fn wakes_in(&self) -> Option<std::time::Duration> {
        let second = ui::clock::local_now()
            .timestamp()
            .as_second()
            .rem_euclid(60);
        Some(std::time::Duration::from_secs(
            60_u64.saturating_sub(u64::try_from(second).unwrap_or(0)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The clock the session publishes is the one every session publishes, empty until its first
    /// poll: an app reads `device.clock` beside the time and asks before it trusts what it makes.
    #[test]
    fn the_clock_is_published_beside_the_time() {
        let activities = std::rc::Rc::new(crate::shell::activities::Activities::new(
            std::sync::Arc::new(runtime_collab::LiveActivities::default()),
        ));
        let mut served = super::Served::new(activities);
        let published: Vec<String> = served
            .resources()
            .into_iter()
            .map(|resource| resource.kind)
            .collect();
        assert!(published.iter().any(|resource| resource == super::CLOCK));
        assert_eq!(
            served.read(super::CLOCK),
            Some(reading(&Clock::default())),
            "nothing is said until the platform is asked"
        );
        assert!(served.poll(), "the first look publishes both");
        assert_eq!(
            served.read(super::CLOCK),
            Some(reading(&super::clock())),
            "the platform's own answer"
        );
    }
}
