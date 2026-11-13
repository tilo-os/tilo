//! Wizard controls, and the presses they queue.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use ui::tilo_resource::Effect as _;

use super::effects::DestinationEffect;

#[derive(Clone, PartialEq, Eq)]
pub struct Press {
    pub effect: DestinationEffect,
}

impl Press {
    pub fn new(effect: DestinationEffect) -> Self {
        Self { effect }
    }
}

#[derive(Clone, Default)]
pub struct Presses(Rc<RefCell<VecDeque<Press>>>);

impl Presses {
    pub fn control(&self, press: Press) -> Control {
        Control {
            queue: self.clone(),
            press,
        }
    }

    pub fn take(&self) -> Option<Press> {
        self.0.borrow_mut().pop_front()
    }
}

pub struct Bound {
    inner: ui::widget::Element,
    bindings: Vec<ui::widget::ControlBindingMetadata>,
    keys: Option<Control>,
}

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
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::take_control_invocation(&mut *self.inner)
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<ui::tilo_actions::ActionAddress>,
    ) {
        ui::widget::controls::refresh_control_action_availability(&mut *self.inner, allowed);
    }

    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        self.bindings.clone()
    }
}

#[derive(Clone)]
pub struct Control {
    queue: Presses,
    press: Press,
}

impl Control {
    pub fn fire(&self) {
        self.queue.0.borrow_mut().push_back(self.press.clone());
    }

    pub fn effect(&self) -> &DestinationEffect {
        &self.press.effect
    }

    pub fn metadata(&self, control_id: &str) -> ui::widget::ControlBindingMetadata {
        let effect = &self.press.effect;
        ui::widget::ControlBindingMetadata {
            control_id: control_id.to_owned(),
            kind: ui::widget::ControlBindingKind::Action(effect.address_on(effect.published_at())),
        }
    }

    #[cfg(test)]
    pub fn detached(effect: DestinationEffect) -> (Self, Presses) {
        let presses = Presses::default();
        (presses.control(Press::new(effect)), presses)
    }
}
