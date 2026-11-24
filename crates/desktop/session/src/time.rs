//! The device's time as the person reads it: the minute in their style and zone, and the timer
//! Clocks floats beside it. Published again when either moves, so a view that shows the time runs
//! again then and at no other time.
//!
//! Whether the clock is synchronised is published beside it (`session:clock`): a code made from a
//! seed and the clock is only as good as the clock, and an app that shows one asks this before it
//! trusts what it makes.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::mpsc;

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

pub(crate) use tilo_device_surfaces::clock::Clock;
pub(crate) use tilo_device_surfaces::time::Time;

/// Whether this device's clock is synchronised now, as the platform answers it.
fn clock() -> Clock {
    Clock {
        synchronised: tilo_clock::is_synchronised(),
    }
}

/// The minute now, as this service's cadence counts it.
fn minute() -> i64 {
    ui::clock::local_now()
        .timestamp()
        .as_second()
        .div_euclid(60)
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
        .checked_sub(jiff::Span::new().days(i64::from(day.day().saturating_sub(1))))
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
    /// The platform's answer to whether the clock is synchronised, asked again off the loop as
    /// the minute moves: asking runs a program, and a program on the loop is a frozen desktop.
    synchronised: Synchronised,
}

/// Asking the platform whether the clock is synchronised, on a worker.
struct Synchronised {
    /// Tells the worker to ask the platform now.
    demand: mpsc::Sender<()>,
    /// The answers the worker brought since the last look.
    answered: mpsc::Receiver<Clock>,
    /// The minute the platform was last asked in, so a pass alone asks nothing.
    asked: Cell<i64>,
}

impl Synchronised {
    /// Ask the platform once here, before the loop runs, and again on `wake`'s worker whenever
    /// the minute moves.
    fn start(wake: &tilo_service::Wake) -> Self {
        let (demand, wanted) = mpsc::channel::<()>();
        let (told, answered) = mpsc::channel::<Clock>();
        let wake = Arc::clone(wake);
        if let Err(error) = std::thread::Builder::new()
            .name("tilo-clock".into())
            .spawn(move || {
                while wanted.recv().is_ok() {
                    // A burst of asks is one run of the program.
                    while wanted.try_recv().is_ok() {}
                    if told.send(clock()).is_err() {
                        return;
                    }
                    wake();
                }
            })
        {
            tracing::warn!(%error, "the clock's synchronised fact does not follow the platform");
        }
        Self {
            demand,
            answered,
            asked: Cell::new(minute()),
        }
    }

    /// Ask the platform again when `minute` is not the one last asked in: one ask a minute, and
    /// none on the passes between.
    fn due(&self, minute: i64) {
        if self.asked.get() == minute {
            return;
        }
        self.asked.set(minute);
        let _ = self.demand.send(());
    }

    /// The newest answer the worker brought since the last look.
    fn taken(&self) -> Option<Clock> {
        let mut latest = None;
        while let Ok(answer) = self.answered.try_recv() {
            latest = Some(answer);
        }
        latest
    }
}

impl Served {
    pub(crate) fn new(
        activities: std::rc::Rc<crate::shell::activities::Activities>,
        wake: &tilo_service::Wake,
    ) -> Self {
        Self {
            published: Time::default(),
            // Asked before the loop: an app reads it beside the time from its first frame.
            clock: clock(),
            activities,
            synchronised: Synchronised::start(wake),
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
        // Asking is the minute's cadence, never a pass: the program runs on the worker.
        self.synchronised.due(minute());
        let clock = self.synchronised.taken();
        let clock_moved = clock.as_ref().is_some_and(|answer| *answer != self.clock);
        if let Some(clock) = clock {
            self.clock = clock;
        }
        let now = now(&self.activities);
        if now == self.published && !clock_moved {
            return false;
        }
        self.published = now;
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

    /// The clock the session publishes is the one every session publishes, asked before the loop
    /// runs: an app reads `device.clock` beside the time and asks before it trusts what it makes.
    #[test]
    fn the_clock_is_published_beside_the_time() {
        let activities = std::rc::Rc::new(crate::shell::activities::Activities::new(
            std::sync::Arc::new(runtime_collab::LiveActivities::default()),
        ));
        let wake: tilo_service::Wake = std::sync::Arc::new(|| {});
        let mut served = super::Served::new(activities, &wake);
        let published: Vec<String> = served
            .resources()
            .into_iter()
            .map(|resource| resource.id.as_str().to_owned())
            .collect();
        assert!(published.iter().any(|resource| resource == super::CLOCK));
        assert_eq!(
            served.read(super::CLOCK),
            Some(reading(&super::clock())),
            "the platform's own answer, asked before the loop"
        );
        assert!(served.poll(), "the first look publishes the time");
        assert_eq!(
            served.read(super::CLOCK),
            Some(reading(&super::clock())),
            "the platform's own answer"
        );
    }
}
