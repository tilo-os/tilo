//! [`Press`] — what pressing a control that stands inside another widget does — a menu row, a
//! swipe, a nav place, a tree node, a card's face.
//!
//! A view binds those presses as it binds a button's: the event is the action the view attached
//! ([`crate::component::Event::Action`]). The widget that owns the parts holds one `Press` per
//! part, so the press is carried out through the same door as its own controls: it is reported
//! in the widget's inventory, handed over when the host drains the window, and refreshed with
//! the rest when what the app publishes changes. A press the view keeps for itself
//! ([`crate::component::Event::View`]) is reported to the view, which decides what it does.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::component::Event;
use crate::widget::{ActionEmitter, ControlBindingMetadata};

/// What pressing a part does: run the action the view bound to it, or report the event to the
/// view.
#[derive(Clone, Default)]
pub struct Press {
    /// The action, shared with the command the part runs (a menu row's command runs whenever
    /// it is chosen, and the menu is built anew each time it opens).
    action: Option<Rc<RefCell<ActionEmitter<()>>>>,
    /// The view's own event, fired when the part is pressed.
    view: Option<crate::component::Intent>,
}

impl std::fmt::Debug for Press {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (self.action.is_some(), self.view.is_some()) {
            (false, false) => f.write_str("Press::None"),
            (true, _) => f.write_str("Press::Action(..)"),
            (false, true) => f.write_str("Press::View(..)"),
        }
    }
}

impl Press {
    /// What pressing a part bound to `event` does.
    #[must_use]
    pub fn of(event: Event) -> Self {
        match event {
            Event::None => Self::default(),
            Event::Action(binding) => Self {
                action: Some(Rc::new(RefCell::new(ActionEmitter::new(binding)))),
                view: None,
            },
            Event::View(intent) => Self {
                action: None,
                view: Some(intent),
            },
        }
    }

    /// Whether pressing the part does nothing.
    #[must_use]
    pub fn is_none(&self) -> bool {
        self.action.is_none() && self.view.is_none()
    }

    /// Whether the part can be pressed now: its action is one the app offers here.
    #[must_use]
    pub fn is_available(&self) -> bool {
        match (&self.action, &self.view) {
            (Some(action), _) => action.borrow().is_available(),
            (None, Some(_)) => true,
            (None, None) => false,
        }
    }

    /// The action the part runs, when it runs one: what a menu tells its placed commands from
    /// those the system adds by.
    #[must_use]
    pub fn address(&self) -> Option<tilo_actions::ActionAddress> {
        self.action
            .as_ref()
            .map(|action| action.borrow().binding().address().clone())
    }

    /// Press it: run the action, or report the event to the view.
    pub fn run(&self) {
        if let Some(action) = &self.action {
            if let Err(error) = action.borrow_mut().emit(()) {
                tracing::warn!(%error, "a part's action refused its arguments");
            }
            return;
        }
        if let Some(view) = &self.view {
            view.fire(None);
        }
    }

    /// The press as the command a part runs when it is chosen.
    #[must_use]
    pub fn runner(&self) -> Rc<dyn Fn()> {
        let press = self.clone();
        Rc::new(move || press.run())
    }

    /// The press as a handler, `None` for a part that does nothing.
    #[must_use]
    pub fn handler(&self) -> Option<Box<dyn FnMut()>> {
        if self.is_none() {
            return None;
        }
        let press = self.clone();
        Some(Box::new(move || press.run()))
    }

    /// Hand over the press the host has not collected yet.
    pub(crate) fn take(&self) -> Option<tilo_actions::ControlInvocation> {
        self.action
            .as_ref()
            .and_then(|action| action.borrow_mut().take())
    }

    /// The part as a control of the inventory, `None` for a part that runs no action.
    #[must_use]
    pub fn metadata(&self, control_id: impl Into<String>) -> Option<ControlBindingMetadata> {
        self.action
            .as_ref()
            .map(|action| action.borrow().metadata(control_id))
    }

    pub(crate) fn install_custody(
        &self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        if let Some(action) = &self.action {
            let _ = action.borrow_mut().install_custody(domain, attachment);
        }
    }

    /// Returns whether the press became available or unavailable.
    pub(crate) fn refresh_availability(
        &self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) -> bool {
        let Some(action) = &self.action else {
            return false;
        };
        let mut action = action.borrow_mut();
        let before = action.is_available();
        action.refresh_availability(allowed);
        before != action.is_available()
    }
}
