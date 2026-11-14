//! The device's time as the person reads it: the minute in their style and zone. Published again
//! when the minute turns, so a view that shows the time runs again then and at no other time.

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
}

impl Time {
    fn now() -> Self {
        Self {
            clock: ui::clock::time_of_day(&ui::clock::local_now()),
        }
    }
}

/// The time as a view of another language reads it (`device.time`).
#[must_use]
pub(crate) fn surface() -> tilo_actions::surface::Surface {
    tilo_resource::surface::reading_surface::<Time, NoEffect>("time", "Time")
}

/// The time as the session's services publish it.
#[derive(Default)]
pub(crate) struct Served {
    published: Time,
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
        let now = Time::now();
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
