//! Adapters from Tube's action bus to shared toolkit controls.

use ui::LocalState;
use ui::widget::Toggle;

use crate::action::{self, Action, ActionBus};

/// Build the toolkit's standard toggle while routing user intent through
/// Tube's existing [`ActionBus`].
///
/// The local binding supplies the model's current value without inventing a
/// second Automerge path. Pointer, keyboard, focus, accessibility, minimum
/// target sizing, and painting all remain owned by [`Toggle`].
pub fn action_toggle(on: bool, bus: &ActionBus, action: Action) -> Toggle {
    let state = LocalState::new(on);
    let bus = bus.clone();
    Toggle::from_state(&state).on_change(move |_| action::push(&bus, action.clone()))
}

#[cfg(test)]
mod tests {
    use automerge::AutoCommit;
    use ui::widget::Widget;

    use super::*;
    use crate::action::SettingFlag;

    #[test]
    fn accessibility_activation_uses_tube_action_bus() {
        let bus = action::new_bus();
        let mut toggle =
            action_toggle(true, &bus, Action::ToggleSetting(SettingFlag::ShowComments))
                .with_id("switch_comments")
                .with_accessible_name("Show comments");
        let mut state = AutoCommit::new();

        assert!(toggle.perform_action(
            &ui::a11y::Action::ToggleCheck,
            &mut state,
            &automerge::ROOT,
        ));
        assert_eq!(
            action::drain(&bus),
            vec![Action::ToggleSetting(SettingFlag::ShowComments)]
        );
        assert!(!toggle.is_checked());
    }

    #[test]
    fn adapter_keeps_shared_toggle_semantics_and_identity() {
        let bus = action::new_bus();
        let toggle = action_toggle(false, &bus, Action::ToggleSetting(SettingFlag::Autoplay))
            .with_id("switch_autoplay")
            .with_accessible_name("Autoplay");

        assert_eq!(toggle.test_id(), Some("switch_autoplay"));
        let node = toggle.accessibility().expect("toggle accessibility");
        assert_eq!(node.role, ui::a11y::Role::Switch);
        assert_eq!(node.name.as_deref(), Some("Autoplay"));
        assert_eq!(node.state.checked, Some(false));
        assert!(node.state.focusable);
    }
}
