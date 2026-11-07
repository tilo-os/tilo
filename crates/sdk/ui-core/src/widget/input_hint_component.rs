//! [`InputHint`] as a described component.

use crate::component::{Component, Described};
use crate::input::{InputBinding, KeyboardBinding};

use super::InputHint;

/// The keys of a shortcut as drawn keycaps, in the person's own keyboard legends ("Strg" on a
/// German keyboard, the Command key on a Mac), beside the command it runs. Never a typed string.
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

    /// Another chord shows where the hint stands, its keycaps drawn for it.
    fn set_props(&mut self, props: InputHintProps) -> bool {
        self.set_binding(InputBinding::Keyboard(binding(props.keys)));
        true
    }
}

/// The chord of the keys, in the person's own legends.
fn binding(keys: Vec<String>) -> KeyboardBinding {
    KeyboardBinding::from_keys(keys.iter().map(String::as_str))
}
