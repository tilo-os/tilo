//! A hosted surface's device-local door, shared with whoever lists and
//! invokes its actions while its host owns the surface.
//!
//! A host mounts a private surface app (setup, Quick Settings, a sheet)
//! and keeps it to itself: only the host may lay it out, draw it or run it.
//! The catalogue a person searching, a script or an agent on this device
//! drives it through is elsewhere, and may not borrow the host. The host
//! hands that catalogue a [`SharedDoor`]: on every pass it [`serves`] what
//! was asked through the app's own door
//! ([`AppWidget::invoke_device_local_action`]) and [`publishes`] what the
//! app offers now ([`AppWidget::device_local_resources`]). The catalogue
//! reads [`SharedDoor::actions`] and queues with [`SharedDoor::invoke`];
//! the answer arrives once the host served it. Nothing here reaches a room
//! or another device.
//!
//! [`serves`]: SharedDoor::serve
//! [`publishes`]: SharedDoor::publish

use std::cell::RefCell;
use std::rc::Rc;

use crate::widget::{AppWidget, ResourceOperationStart};

/// What an invocation answers, once its host served it.
pub type Answer = Rc<RefCell<Option<Result<ResourceOperationStart, tilo_actions::DoorError>>>>;

/// A hosted surface's door, cloned between its host and its catalogue.
#[derive(Clone, Default)]
pub struct SharedDoor(Rc<RefCell<State>>);

#[derive(Default)]
struct State {
    actions: Vec<tilo_actions::ActionDescriptor>,
    revision: u64,
    queued: Vec<Queued>,
}

struct Queued {
    address: tilo_actions::ActionAddress,
    arguments: serde_json::Map<String, serde_json::Value>,
    answer: Answer,
}

impl SharedDoor {
    /// What the surface offered when its host last published it.
    #[must_use]
    pub fn actions(&self) -> Vec<tilo_actions::ActionDescriptor> {
        self.0.borrow().actions.clone()
    }

    /// Changes whenever [`Self::actions`] does.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.0.borrow().revision
    }

    /// Queue `address` for the host. The answer is `None` until the host
    /// served it.
    pub fn invoke(
        &self,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Answer {
        let answer = Answer::default();
        self.0.borrow_mut().queued.push(Queued {
            address,
            arguments,
            answer: Rc::clone(&answer),
        });
        answer
    }

    /// Whether anything waits for the host.
    #[must_use]
    pub fn has_queued(&self) -> bool {
        !self.0.borrow().queued.is_empty()
    }

    /// Host: run what was queued through `app`'s door, oldest first.
    /// Returns whether anything ran.
    pub fn serve(&self, app: &mut dyn AppWidget) -> bool {
        let queued = std::mem::take(&mut self.0.borrow_mut().queued);
        let ran = !queued.is_empty();
        for invocation in queued {
            let started =
                app.invoke_device_local_action(&invocation.address, &invocation.arguments);
            *invocation.answer.borrow_mut() = Some(started);
        }
        ran
    }

    /// Host: refuse what was queued, for a surface that is gone.
    pub fn refuse_queued(&self) {
        let queued = std::mem::take(&mut self.0.borrow_mut().queued);
        for invocation in queued {
            *invocation.answer.borrow_mut() =
                Some(Err(tilo_actions::DoorError::Unknown(invocation.address)));
        }
    }

    /// Host: publish what `app` offers now; nothing for a surface that is
    /// gone or offers nothing to this device.
    pub fn publish(&self, app: Option<&dyn AppWidget>) {
        let actions = app
            .and_then(AppWidget::device_local_resources)
            .map(|resources| tilo_actions::door::survey(&resources))
            .unwrap_or_default();
        let mut state = self.0.borrow_mut();
        if state.actions != actions {
            state.actions = actions;
            state.revision = state.revision.wrapping_add(1);
        }
    }
}
