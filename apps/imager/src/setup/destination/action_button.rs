//! [`ActionButton`]: a wizard action, composed from the toolkit's own
//! controls.
//!
//! A wizard's action is a `ui::Button` in the role the step gives it (the
//! step's main action is primary, Back and the alternatives are secondary,
//! a low-key way past is plain), or a `ui::Checkbox` for a promise the
//! person makes. The toolkit draws it, sizes it and moves its focus; what
//! this adds is what the installer needs from every control:
//!
//!   * using it fires its [`Control`]: the published action the
//!     [`crate::setup::destination::installer_app::InstallerApp`] runs
//!     through its action door;
//!   * its inventory entry names that action, not an anonymous callback;
//!   * a disabled action leaves the focus order, so keyboard navigation
//!     skips what cannot be used.

use ui::icons::Icon;
use ui::widget::{Button, ButtonRole, Checkbox, ControlSize, Element, Widget};

use crate::setup::destination::control::{Control, is_activation_key};

pub struct ActionButton {
    label: String,
    /// The published action the control runs when used.
    control: Control,
    /// Stable E2E `test_id`. Defaults to the label, but the label can change,
    /// so it can be pinned via [`ActionButton::with_test_id`] to stay
    /// addressable across rebuilds.
    test_id: Option<String>,
    /// `Some` makes the action a checkbox with this checked state.
    checked: Option<bool>,
    role: ButtonRole,
    size: ControlSize,
    icon: Option<Icon>,
    enabled: bool,
    /// The toolkit control showing all of the above.
    inner: Element,
}

impl ActionButton {
    /// A secondary button firing `control` when used.
    pub fn new(label: &str, control: Control) -> Self {
        let mut button = Self {
            label: label.to_string(),
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

    /// The step's main action.
    #[must_use]
    pub fn primary(mut self) -> Self {
        self.role = ButtonRole::Primary;
        self.compose();
        self
    }

    /// A low-key alternative: a plain button, still a full-size target.
    #[must_use]
    pub fn quiet(mut self) -> Self {
        self.role = ButtonRole::Plain;
        self.compose();
        self
    }

    /// A step's one deliberate action, at the prominent control size.
    #[must_use]
    pub fn large(mut self) -> Self {
        self.size = ControlSize::Large;
        self.compose();
        self
    }

    /// Lead the label with `icon` (Back's chevron).
    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self.compose();
        self
    }

    /// Pin a stable E2E `test_id` (defaults to the label otherwise).
    #[must_use]
    pub fn with_test_id(mut self, id: &str) -> Self {
        self.test_id = Some(id.to_string());
        self.compose();
        self
    }

    /// Show the action as a checkbox in this checked state.
    #[must_use]
    pub fn checkbox(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self.compose();
        self
    }

    /// Disabled: dimmed, inert, and out of the focus order.
    #[must_use]
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self.compose();
        self
    }

    fn id(&self) -> &str {
        self.test_id.as_deref().unwrap_or(self.label.as_str())
    }

    /// Build the toolkit control for the current settings.
    fn compose(&mut self) {
        let control = self.control.clone();
        let id = self.id().to_owned();
        self.inner = if let Some(checked) = self.checked {
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

    /// A focused action consumes its Enter and Space, so the wizard's own
    /// Enter (advance the step) never acts past it: Enter on a focused Back
    /// goes back.
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

    /// A disabled action is announced as out of the focus order, as it is.
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
        let mut button = ActionButton::new("Back", control);
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
        // Phase `enter_and_space_fire_the_focused_action`.
        {
            assert_key_fires(NamedKey::Enter);
            assert_key_fires(NamedKey::Space);
        }
        // Phase `button_semantics_are_named_focusable_and_activatable`.
        {
            let (control, presses) = Control::detached(DestinationEffect::Reboot);
            let mut button = ActionButton::new("Reboot now", control).with_test_id("reboot");
            button.set_focus(true);
            let node = button.accessibility().expect("button semantics");
            assert_eq!(node.role, ui::a11y::Role::Button);
            assert_eq!(node.name.as_deref(), Some("Reboot now"));
            assert!(button.is_focusable());
            assert!(button.is_focused());
            assert_eq!(button.test_id(), Some("reboot"));

            let mut doc = AutoCommit::new();
            assert!(button.perform_action(
                &ui::a11y::Action::Activate,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(presses.take().map(|press| press.effect) == Some(DestinationEffect::Reboot));
            assert_eq!(doc.length(ROOT), 0);
            assert!(button.is_focused(), "AT activation must not steal focus");
            // A disabled button runs nothing and leaves the focus order.
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
        // Phase `recovery_ack_is_a_cleanly_named_native_checkbox`.
        {
            let (control, presses) = Control::detached(DestinationEffect::SetRecoveryPromise {
                acknowledged: false,
            });
            let mut button = ActionButton::new("I will save my recovery key", control)
                .with_test_id("recovery_ack")
                .checkbox(true);
            let node = button.accessibility().expect("checkbox semantics");
            assert_eq!(node.role, ui::a11y::Role::Checkbox);
            assert_eq!(node.name.as_deref(), Some("I will save my recovery key"));
            assert_eq!(node.state.checked, Some(true));
            assert_eq!(button.test_id(), Some("recovery_ack"));
            let mut doc = AutoCommit::new();
            assert!(button.perform_action(
                &ui::a11y::Action::ToggleCheck,
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT
            ));
            assert!(
                presses.take().map(|press| press.effect)
                    == Some(DestinationEffect::SetRecoveryPromise {
                        acknowledged: false
                    })
            );
            assert_eq!(doc.length(ROOT), 0);
        }
        // Phase `the_inventory_names_the_published_action`.
        {
            let (control, _) = Control::detached(DestinationEffect::Reboot);
            let button = ActionButton::new("Reboot now", control).with_test_id("reboot");
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
