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
    /// The setting's name, read beside its state by assistive technology.
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
            .on_change(move |on| edits.edited(Value::Bool(on)))
            .with_id(key)
            .with_enabled(enabled);
        if let Some(label) = label {
            toggle = toggle.with_accessible_name(&label);
        }
        toggle
    }

    /// Its state and whether it can be turned change in place.
    fn set_props(&mut self, props: ToggleProps) -> bool {
        self.set_enabled(props.enabled);
        self.set_checked(props.value.state.get());
        true
    }
}
