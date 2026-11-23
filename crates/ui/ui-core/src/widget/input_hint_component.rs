//! [`InputHint`] as a described component.

use crate::component::{Component, Described};
use crate::input::{InputBinding, KeyboardBinding};

use super::InputHint;

/// The keys of a shortcut as drawn keycaps, in the person's own keyboard legends.
#[derive(Component, Default)]
#[component(name = "InputHint", role = "StaticText")]
pub struct InputHintProps {
    /// The keys of the chord, modifiers first: `["Ctrl", "Shift", "P"]`.
    #[component(required, positional)]
    pub keys: Vec<String>,
}

impl Described for InputHint {
    type Props = InputHintProps;

    fn from_props(_key: &str, props: InputHintProps) -> Self {
        Self::keyboard(binding(props.keys))
    }

    fn set_props(&mut self, props: InputHintProps) -> bool {
        self.set_binding(InputBinding::Keyboard(binding(props.keys)));
        true
    }
}

fn binding(keys: Vec<String>) -> KeyboardBinding {
    KeyboardBinding::from_keys(keys.iter().map(String::as_str))
}
