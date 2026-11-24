//! The device's time as the person reads it: the minute in their style and zone, and the timer
//! Clocks floats beside it. Published again when either moves, so a view that shows the time runs
//! again then and at no other time.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

pub(crate) const RESOURCE: &str = "session:time";

/// The time as the session publishes it: the kind a reader with no words of its own knows it by.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Time")]
pub(crate) struct TimeRoot;

pub(crate) use tilo_device_surfaces::time::Time;

/// The time now, as the session publishes it.
fn now(activities: &crate::shell::activities::Activities) -> Time {
    let mut time = Time {
        clock: ui::clock::time_of_day(ui::clock::local_now()),
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

/// The time as the session's services publish it.
pub(crate) struct Served {
    published: Time,
    activities: std::rc::Rc<crate::shell::activities::Activities>,
}

impl Served {
    pub(crate) fn new(activities: std::rc::Rc<crate::shell::activities::Activities>) -> Self {
        Self {
            published: Time::default(),
            activities,
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<NoEffect>(
            tilo_resource::LiveRow::typed::<TimeRoot>(RESOURCE, crate::i18n::t("session-time"))
                .into_resource(),
        )]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.published))
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
        if now == self.published {
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
