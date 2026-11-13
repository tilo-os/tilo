//! One step's deliberate action: its label, icon, test id, and whether it is checked or
//! disabled.

use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::widget::{Button, ButtonRole, Checkbox, ControlSize, Element, Widget};

use crate::setup::destination::control::{Control, is_activation_key};

pub struct ActionButton {
    key: String,
    args: Vec<(String, ArgValue)>,
    label: String,
    control: Control,
    test_id: Option<String>,
    checked: Option<bool>,
    role: ButtonRole,
    size: ControlSize,
    icon: Option<Icon>,
    enabled: bool,
    inner: Element,
}

impl ActionButton {
    pub fn new(key: &str, control: Control) -> Self {
        let mut button = Self {
            key: key.to_string(),
            args: Vec::new(),
            label: String::new(),
            control,
            test_id: None,
            checked: None,
            role: ButtonRole::Secondary,
            size: ControlSize::Regular,
            icon: None,
            enabled: true,
            inner: Box::new(ui::widget::Spacer::new()),
        };
        button.compose();
        button
    }

    #[must_use]
    pub fn with_args(mut self, args: &[(&str, ArgValue)]) -> Self {
        self.args = args
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        self.compose();
        self
    }

    #[must_use]
    pub fn primary(mut self) -> Self {
        self.role = ButtonRole::Primary;
        self.compose();
        self
    }

    #[must_use]
    pub fn quiet(mut self) -> Self {
        self.role = ButtonRole::Plain;
        self.compose();
        self
    }

    #[must_use]
    pub fn large(mut self) -> Self {
        self.size = ControlSize::Large;
        self.compose();
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self.compose();
        self
    }

    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = Some(id.to_string());
        self.compose();
        self
    }

    #[must_use]
    pub fn checkbox(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self.compose();
        self
    }

    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self.compose();
        self
    }

    fn id(&self) -> &str {
        self.test_id.as_deref().unwrap_or(self.key.as_str())
    }

    fn compose(&mut self) {
        let control = self.control.clone();
        self.label = if self.args.is_empty() {
            crate::setup::strings::t(&self.key)
        } else {
            let args: Vec<(&str, ArgValue)> = self
                .args
                .iter()
                .map(|(name, value)| (name.as_str(), value.clone()))
                .collect();
            crate::setup::strings::t_args(&self.key, &args)
        };
        let id = self.id().to_owned();
        self.inner = if let Some(checked) = self.checked {
            let id = self.id().to_owned();
            Box::new(
                Checkbox::from_state(&ui::LocalState::new(checked))
                    .with_id(&id)
                    .with_label(self.label.clone())
                    .with_enabled(self.enabled)
                    .on_view(&id, move |_| control.fire()),
            )
        } else {
            let mut button = Button::new(&id, &self.label)
                .role(self.role)
                .control_size(self.size)
                .with_enabled(self.enabled)
                .on_effect(move || control.fire());
            if let Some(icon) = self.icon {
                button = button.with_icon(icon);
            }
            Box::new(button)
        };
    }
}

impl ui::widget::Decorator for ActionButton {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        &*self.inner
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut *self.inner
    }

    ui::provides!(dyn ui::widget::controls::ActionControls; inner = inner);

    fn is_focusable(&self) -> bool {
        self.enabled && self.inner.is_focusable()
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        if self.enabled && self.inner.is_focused() && is_activation_key(event) {
            self.control.fire();
            *ctx.handled = true;
            return;
        }
        self.inner.handle_event(event, ctx);
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.inner.accessibility().map(|mut node| {
            node.state.focusable = self.enabled && self.inner.is_focusable();
            node
        })
    }

    fn name(&self) -> &'static str {
        "ActionButton"
    }
}

impl ui::widget::controls::ActionControls for ActionButton {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        ui::widget::controls::take_control_invocation(&mut *self.inner)
    }

    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        vec![self.control.metadata(self.id())]
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use automerge::{AutoCommit, ROOT, ReadDoc};
    use taffy::TaffyTree;
    use ui::synthetic_input::named_key;
    use ui::theme::Theme;
    use winit::keyboard::NamedKey;

    use super::*;
    use crate::setup::destination::effects::DestinationEffect;

    fn assert_key_fires(key: NamedKey) {
        let (control, presses) = Control::detached(DestinationEffect::Back);
        let mut button = ActionButton::new("installer-preview-back", control);
        button.set_focus(true);
        let mut doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = button.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        ui::testing::Dispatch::new()
            .doc(&mut doc)
            .layout(&taffy, node)
            .send(&mut button, &named_key(key));
        let fired = presses.take().expect("the button fires its action");
        assert!(fired.effect == DestinationEffect::Back);
        assert!(presses.take().is_none(), "one activation is one press");
        assert_eq!(doc.length(ROOT), 0, "a button never writes the document");
        assert!(button.is_focused(), "activation must not move focus");
    }

    #[test]
    fn action_buttons_are_named_activatable_native_controls() {
        {
            assert_key_fires(NamedKey::Enter);
            assert_key_fires(NamedKey::Space);
        }
        {
            let (control, presses) = Control::detached(DestinationEffect::Reboot);
            let mut button =
                ActionButton::new("installer-action-reboot", control).with_test_id("reboot");
            button.set_focus(true);
            let node = button.accessibility().expect("button semantics");
            assert_eq!(node.role, ui::a11y::Role::Button);
            assert_eq!(node.name.as_deref(), Some("Reboot now"));
            assert!(button.is_focusable());
            assert!(button.is_focused());
            assert_eq!(button.test_id(), Some("reboot"));

            let mut doc = AutoCommit::new();
            crate::setup::destination::press(&mut button, &ui::a11y::Action::Activate, &mut doc);
            assert!(presses.take().map(|press| press.effect) == Some(DestinationEffect::Reboot));
            assert_eq!(doc.length(ROOT), 0);
            assert!(button.is_focused(), "AT activation must not steal focus");
            let mut disabled = button.disabled(true);
            assert!(!disabled.is_focusable());
            assert!(!disabled.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().is_none());
        }
        {
            let (control, presses) = Control::detached(DestinationEffect::SetRecoveryPromise {
                acknowledged: false,
            });
            let mut button = ActionButton::new("installer-action-recovery-promise", control)
                .with_test_id("recovery_ack")
                .checkbox(true);
            let node = button.accessibility().expect("checkbox semantics");
            assert_eq!(node.role, ui::a11y::Role::Checkbox);
            assert_eq!(node.name.as_deref(), Some("I will save my recovery key"));
            assert_eq!(node.state.checked, Some(true));
            assert_eq!(button.test_id(), Some("recovery_ack"));
            let mut doc = AutoCommit::new();
            crate::setup::destination::press(&mut button, &ui::a11y::Action::ToggleCheck, &mut doc);
            assert!(
                presses.take().map(|press| press.effect)
                    == Some(DestinationEffect::SetRecoveryPromise {
                        acknowledged: false
                    })
            );
            assert_eq!(doc.length(ROOT), 0);
        }
        {
            let (control, _) = Control::detached(DestinationEffect::Reboot);
            let button =
                ActionButton::new("installer-action-reboot", control).with_test_id("reboot");
            let bindings = ui::widget::controls::control_action_bindings(&button);
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].control_id, "reboot");
            assert!(matches!(
                bindings[0].kind,
                ui::widget::ControlBindingKind::Action(_)
            ));
        }
    }
}
