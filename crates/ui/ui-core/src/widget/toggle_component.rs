//! [`Toggle`] as a described component.

use crate::component::{Bound, Component, Described, Value};

use super::Toggle;

/// A switch: the same two states as a checkbox, for a setting that takes effect at once rather
/// than when a form is sent. Its knob slides to the side it turns to.
#[derive(Component)]
#[component(name = "Toggle", role = "Switch", label = "label")]
pub struct ToggleProps {
    /// Whether it is on, and where a turn of it goes.
    #[component(bind)]
    pub value: Bound<bool>,
    /// The setting's name, beside the switch at the row's leading edge: the whole row turns it,
    /// and assistive technology reads it with the state.
    #[component(positional)]
    pub label: Option<String>,
    /// Whether it can be turned now.
    pub enabled: bool,
}

impl Default for ToggleProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            enabled: true,
        }
    }
}

impl Described for Toggle {
    type Props = ToggleProps;

    fn from_props(key: &str, props: ToggleProps) -> Self {
        let ToggleProps {
            value,
            label,
            enabled,
        } = props;
        let edits = value.clone();
        let mut toggle = Self::from_state(&value.state)
            .on_view(key, move |on| edits.edited(Value::Bool(on)))
            .with_enabled(enabled);
        if let Some(label) = label {
            toggle = toggle.with_label(label);
        }
        toggle
    }

    /// Its state, its words and whether it can be turned change in place; words given to a bare
    /// switch, or taken from a labelled one, change its layout, so it is built again.
    fn set_props(&mut self, props: ToggleProps) -> bool {
        if props.label.is_some() != self.has_label() {
            return false;
        }
        if let Some(label) = props.label {
            self.set_label(label);
        }
        self.set_enabled(props.enabled);
        self.set_checked(props.value.state.get());
        true
    }
}
