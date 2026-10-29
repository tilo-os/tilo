//! What a wizard control does when it is used.
//!
//! A control names the effect it runs, with its arguments. Using it queues
//! that one [`Press`]; the [`crate::setup::destination::installer_app::InstallerApp`]
//! takes the queue and runs each press through its own door — the effect's
//! availability now, the typed confirmation phrase, then the handler —
//! exactly as the console and the palette reach the same effects. A control
//! holds no wizard state and never writes the document.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use ui::tilo_resource::Effect as _;

use super::effects::DestinationEffect;

/// One use of a control: the effect it runs.
#[derive(Clone, PartialEq, Eq)]
pub struct Press {
    pub effect: DestinationEffect,
}

impl Press {
    /// A press of `effect`.
    pub fn new(effect: DestinationEffect) -> Self {
        Self { effect }
    }
}

/// The presses the wizard's controls queued and the wizard has not run yet.
#[derive(Clone, Default)]
pub struct Presses(Rc<RefCell<VecDeque<Press>>>);

impl Presses {
    /// A control that queues `press` here each time it is used.
    pub fn control(&self, press: Press) -> Control {
        Control {
            queue: self.clone(),
            press,
        }
    }

    /// The oldest press not run yet.
    pub fn take(&self) -> Option<Press> {
        self.0.borrow_mut().pop_front()
    }
}

/// What one control does when used: queue its press.
#[derive(Clone)]
pub struct Control {
    queue: Presses,
    press: Press,
}

impl Control {
    /// Queue this control's press.
    pub fn fire(&self) {
        self.queue.0.borrow_mut().push_back(self.press.clone());
    }

    /// The effect this control runs.
    pub fn effect(&self) -> &DestinationEffect {
        &self.press.effect
    }

    /// The control's inventory entry: bound to its published effect.
    pub fn metadata(&self, control_id: &str) -> ui::widget::ControlBindingMetadata {
        let effect = &self.press.effect;
        ui::widget::ControlBindingMetadata {
            control_id: control_id.to_owned(),
            kind: ui::widget::ControlBindingKind::Action(effect.address_on(effect.published_at())),
        }
    }

    /// A control that queues into a queue of its own, for a widget's own
    /// tests: the returned queue holds whatever the control fired.
    #[cfg(test)]
    pub fn detached(effect: DestinationEffect) -> (Self, Presses) {
        let presses = Presses::default();
        (presses.control(Press::new(effect)), presses)
    }
}
