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

/// A toolkit control whose uses fire wizard [`Control`]s: a disk's row, a
/// choice's segments. The toolkit widget draws and handles it; the
/// inventory names the published action each use runs, so a control is
/// listed as the effect it is, never as an anonymous callback.
pub struct Bound {
    inner: ui::widget::Element,
    bindings: Vec<ui::widget::ControlBindingMetadata>,
    /// The control Enter and Space use while the widget has focus.
    keys: Option<Control>,
}

/// Whether `event` presses Enter or Space: what uses a focused control.
pub fn is_activation_key(event: &winit::event::WindowEvent) -> bool {
    use winit::event::{ElementState, KeyEvent, WindowEvent};
    use winit::keyboard::{Key, NamedKey};
    matches!(
        event,
        WindowEvent::KeyboardInput {
            event: KeyEvent {
                logical_key: Key::Named(NamedKey::Enter | NamedKey::Space),
                state: ElementState::Pressed,
                ..
            },
            ..
        }
    )
}

impl Bound {
    /// `inner`, whose uses fire `controls`, each listed under its control id.
    pub fn new<'a>(
        inner: ui::widget::Element,
        controls: impl IntoIterator<Item = (String, &'a Control)>,
    ) -> Self {
        Self {
            inner,
            bindings: controls
                .into_iter()
                .map(|(id, control)| control.metadata(&id))
                .collect(),
            keys: None,
        }
    }

    /// Enter and Space on the focused widget use `control` and are consumed,
    /// so the wizard's own Enter (advance the step) never acts past a
    /// focused control.
    #[must_use]
    pub fn activated_by_keys(mut self, control: &Control) -> Self {
        self.keys = Some(control.clone());
        self
    }
}

impl ui::widget::Decorator for Bound {
    type Inner = dyn ui::widget::Widget;

    fn inner(&self) -> &(dyn ui::widget::Widget + 'static) {
        &*self.inner
    }

    fn inner_mut(&mut self) -> &mut (dyn ui::widget::Widget + 'static) {
        &mut *self.inner
    }

    ui::provides!(dyn ui::widget::controls::ActionControls; inner = inner);

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if let Some(control) = &self.keys
            && self.inner.is_focused()
            && self.inner.is_enabled()
            && is_activation_key(event)
        {
            control.fire();
            *ctx.handled = true;
            return;
        }
        self.inner.handle_event(event, ctx);
    }
}

impl ui::widget::controls::ActionControls for Bound {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.bindings.clone()
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
