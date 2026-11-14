//! The device's time as the person reads it: the minute in their style and zone, and the timer
//! Clocks floats beside it. Published again when either moves, so a view that shows the time runs
//! again then and at no other time.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, DoorError, ResourceSpec};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

pub(crate) const RESOURCE: &str = "session:time";

/// The time now, as the bar shows it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub(crate) struct Time {
    /// The minute, in the person's style (`4:28 PM`, `16:28`).
    pub clock: String,
    /// The timer or stopwatch Clocks floats, by its name: its label, or `Timer` or `Stopwatch`
    /// when the app named none; empty when none floats.
    pub timer: String,
    /// What the timer has left, or what the stopwatch has run.
    pub timer_left: String,
    pub timer_paused: bool,
}

impl Time {
    fn now(activities: &crate::chrome::Activities) -> Self {
        let mut time = Self {
            clock: ui::clock::time_of_day(&ui::clock::local_now()),
            ..Self::default()
        };
        let Some(published) = activities.published(crate::chrome::CLOCKS_APP) else {
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
}

/// The time as a view of another language reads it (`device.time`).
#[must_use]
pub(crate) fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Time, NoEffect>("time", "Time")
}

/// The time as the session's services publish it.
pub(crate) struct Served {
    published: Time,
    activities: std::rc::Rc<crate::chrome::Activities>,
}

impl Served {
    pub(crate) fn new(activities: std::rc::Rc<crate::chrome::Activities>) -> Self {
        Self {
            published: Time::default(),
            activities,
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<NoEffect>(ResourceSpec::new(
            RESOURCE,
            tilo_action_policy::roles::SESSION_KIND,
            crate::i18n::t("session-time"),
        ))]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| reading(&self.published))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        Err(DoorError::Unknown(ActionAddress::new(resource, action)))
    }

    fn poll(&mut self) -> bool {
        let now = Time::now(&self.activities);
        if now == self.published {
            return false;
        }
        self.published = now;
        true
    }

    fn wakes_in(&self) -> Option<std::time::Duration> {
        let second = ui::clock::local_now().timestamp().rem_euclid(60);
        Some(std::time::Duration::from_secs(
            60_u64.saturating_sub(u64::try_from(second).unwrap_or(0)),
        ))
    }
}
