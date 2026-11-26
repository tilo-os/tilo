//! [`ListRow`] as a control an agent's actions land on: its own activation, the check box
//! and every action its trailing control or its parts hold.

use tilo_ui_base::widget::controls::ActionControls;
use tilo_ui_base::{widget::ActionEmitter, widget::ControlBindingMetadata, widget::Widget};

use super::{ActionHandler, ListRow};

impl ActionControls for ListRow {
    fn attach_controls(
        &mut self,
        domain: &tilo_ui_base::ControlDomain,
        attachment: tilo_ui_base::control_domain::DomainAttachment,
    ) {
        self.activation.attach(domain, attachment);
        if let Some(action) = &mut self.check_action {
            let _ = action.attach(domain, attachment);
        }
        for action in &mut self.actions {
            if let ActionHandler::Bound(emitter) = &mut action.handler {
                let _ = emitter.attach(domain, attachment);
            }
        }
        if let Some(swipe) = &self.swipe {
            swipe.press.attach(domain, attachment);
        }
        for child in Widget::children_mut(self) {
            tilo_ui_base::widget::controls::attach_controls(child, domain, attachment);
        }
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        if let Some(invocation) = self.activation.take_invocation() {
            return Some(invocation);
        }
        if let Some(invocation) = self.check_action.as_mut().and_then(ActionEmitter::take) {
            return Some(invocation);
        }
        for action in &mut self.actions {
            if let ActionHandler::Bound(emitter) = &mut action.handler
                && let Some(invocation) = emitter.take()
            {
                return Some(invocation);
            }
        }
        if let Some(invocation) = self.swipe.as_ref().and_then(|swipe| swipe.press.take()) {
            return Some(invocation);
        }
        Widget::children_mut(self)
            .into_iter()
            .find_map(tilo_ui_base::widget::controls::take_control_invocation)
    }

    fn control_action_bindings(&self) -> Vec<ControlBindingMetadata> {
        let mut bindings = self.activation.bindings();
        let id = self.activation.id();
        if let Some(action) = &self.check_action {
            bindings.push(action.metadata(format!("{id}/check")));
        }
        for (index, action) in self.actions.iter().enumerate() {
            match &action.handler {
                ActionHandler::Bound(emitter) => {
                    bindings.push(emitter.metadata(format!("{id}/action/{index}")));
                }
                // A callback-only action reaches no API: the inventory lists what an agent can invoke.
                ActionHandler::Callback(_) => {}
            }
        }
        if let Some(swipe) = &self.swipe {
            bindings.extend(swipe.press.metadata(format!("{id}/swipe")));
        }
        for child in Widget::children(self) {
            bindings.extend(tilo_ui_base::widget::controls::control_action_bindings(
                child,
            ));
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        let mut changed = self.activation.refresh_availability(allowed);
        if let Some(action) = &mut self.check_action {
            let before = action.is_available();
            action.refresh_availability(allowed);
            changed |= before != action.is_available();
        }
        for action in &mut self.actions {
            if let ActionHandler::Bound(emitter) = &mut action.handler {
                let before = emitter.is_available();
                emitter.refresh_availability(allowed);
                changed |= before != emitter.is_available();
            }
        }
        if let Some(swipe) = &self.swipe
            && swipe.press.refresh_availability(allowed)
        {
            changed = true;
        }
        if changed {
            self.dirty = true;
        }
        for child in Widget::children_mut(self) {
            tilo_ui_base::widget::controls::refresh_control_action_availability(child, allowed);
        }
    }
}
