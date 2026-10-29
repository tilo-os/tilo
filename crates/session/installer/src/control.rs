//! What a wizard control does when it is used.
//!
//! A control names the published action it runs and the arguments it runs
//! it with. Using it queues that one [`Press`]; the
//! [`crate::installer_app::InstallerApp`] takes the queue and runs each
//! press through its action door — resolution against the tree it publishes
//! now, availability, the typed confirmation phrase, admission — exactly as
//! the console and the palette reach the same actions. A control holds no
//! wizard state and never writes the document.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use serde_json::{Map, Value};
use tilo_actions::ActionAddress;

/// One use of a control: the action it runs and its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct Press {
    pub address: ActionAddress,
    pub args: Map<String, Value>,
}

impl Press {
    /// A press of `action` on `resource` with `args` (a JSON object; any
    /// other value means no arguments).
    pub fn new(resource: &str, action: &str, args: Value) -> Self {
        Self {
            address: ActionAddress::new(resource, action),
            args: match args {
                Value::Object(map) => map,
                _ => Map::new(),
            },
        }
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

    /// The published action this control runs.
    pub fn address(&self) -> &ActionAddress {
        &self.press.address
    }

    /// The control's inventory entry: bound to its published action.
    pub fn metadata(&self, control_id: &str) -> ui::widget::ControlBindingMetadata {
        ui::widget::ControlBindingMetadata {
            control_id: control_id.to_owned(),
            kind: ui::widget::ControlBindingKind::Action(self.press.address.clone()),
        }
    }

    /// A control that queues into a queue of its own, for a widget's own
    /// tests: the returned queue holds whatever the control fired.
    #[cfg(test)]
    pub fn detached(resource: &str, action: &str, args: Value) -> (Self, Presses) {
        let presses = Presses::default();
        (presses.control(Press::new(resource, action, args)), presses)
    }
}
