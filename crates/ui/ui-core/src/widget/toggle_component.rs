//! [`Toggle`] as a described component.

use crate::component::{Bound, Component, Described, Event, Value};

use super::Toggle;

/// A switch: the same two states as a checkbox, for a setting that takes effect at once rather
/// than when a form is sent. Its knob slides to the side it turns to.
#[derive(Component)]
#[component(name = "Toggle", role = "Switch", label = "label")]
pub struct ToggleProps {
    /// Whether it is on, and where a turn of it goes.
    #[component(bind)]
    pub value: Bound<bool>,
    /// Whether it is on, for a view that reads the state rather than binding a place of its own:
    /// the switch shows it and turns only through its event, so a change from elsewhere shows
    /// when the view is drawn again.
    pub shown: Option<bool>,
    /// The setting's name, beside the switch at the row's leading edge: the whole row turns it,
    /// and assistive technology reads it with the state.
    #[component(positional)]
    pub label: Option<String>,
    /// Whether it can be turned now.
    pub enabled: bool,
    /// It was turned: whether it is on now, in the app's words.
    #[component(event(value = bool))]
    pub check: Event,
}

impl Default for ToggleProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            shown: None,
            label: None,
            enabled: true,
            check: Event::None,
        }
    }
}

impl Described for Toggle {
    type Props = ToggleProps;

    fn from_props(key: &str, props: ToggleProps) -> Self {
        let ToggleProps {
            value,
            shown,
            label,
            enabled,
            check,
        } = props;
        let edits = value.clone();
        let mut toggle = match shown {
            Some(on) => Self::given(key, on),
            None => Self::from_state(&value.state).with_id(key),
        }
        .with_enabled(enabled);
        toggle = match check {
            // The view's own action sets what it changes; the switch itself edits nothing.
            Event::Action(binding) if shown.is_some() => toggle.on_choose_action(binding),
            Event::Action(_) | Event::None | Event::View(_) => {
                let fired = check.clone();
                toggle.on_choose(key, move |on| {
                    edits.edited(Value::Bool(on));
                    fired.fire(Some(Value::Bool(on)));
                })
            }
        };
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
        self.set_checked(props.shown.unwrap_or_else(|| props.value.state.get()));
        true
    }
}
