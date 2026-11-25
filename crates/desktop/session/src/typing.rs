//! Whether the person is typing, as the device publishes it (`device.input`): a widget, a hint or
//! anything else that would cover what they type steps aside until they pause, and comes back on
//! its own once they do. Published again when typing starts and when the pause ends, and at no
//! other time.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_resource::NoEffect;
use tilo_service::{Service, Started, offer, reading};

pub(crate) use tilo_device_surfaces::input::Input;

pub(crate) const RESOURCE: &str = "session:input";

/// The keyboard as the session publishes it: the kind a reader with no words of its own knows it
/// by.
#[derive(tilo_resource::Row)]
#[row(kind = "session", label = "Keyboard")]
pub(crate) struct InputRoot;

/// Whether the person is typing, as the session's services publish it.
pub(crate) struct Served {
    published: Input,
    /// When text was last typed, as each pass reads it from the compositor.
    typed_at: Rc<Cell<Option<Instant>>>,
}

impl Served {
    pub(crate) fn new(typed_at: Rc<Cell<Option<Instant>>>) -> Self {
        Self {
            published: Input::default(),
            typed_at,
        }
    }
}

impl Service for Served {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![offer::<NoEffect>(
            tilo_resource::LiveRow::typed::<InputRoot>(
                RESOURCE,
                crate::i18n::t("session-keyboard"),
            )
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
        let now = Input::at(self.typed_at.get(), Instant::now());
        if now == self.published {
            return false;
        }
        self.published = now;
        true
    }

    fn wakes_in(&self) -> Option<std::time::Duration> {
        Input::pause_in(self.typed_at.get(), Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Text typed is published as typing on the next poll, and the pause clears it once its time
    /// comes, without another key.
    #[test]
    fn typing_is_published_and_ends_with_the_pause() {
        let typed_at = Rc::new(Cell::new(None));
        let mut served = Served::new(Rc::clone(&typed_at));
        assert!(!served.poll(), "nothing typed: nothing new");
        assert_eq!(served.read(RESOURCE).unwrap()["typing"], false);
        typed_at.set(Some(Instant::now()));
        assert!(served.poll(), "typing is news");
        assert_eq!(served.read(RESOURCE).unwrap()["typing"], true);
        assert!(
            served
                .wakes_in()
                .is_some_and(|left| left <= Duration::from_secs(3)),
            "the service wakes for the pause"
        );
        typed_at.set(Instant::now().checked_sub(Duration::from_secs(4)));
        assert!(served.poll(), "the pause is news");
        assert_eq!(served.read(RESOURCE).unwrap()["typing"], false);
        assert_eq!(served.wakes_in(), None, "nothing left to wake for");
    }
}
