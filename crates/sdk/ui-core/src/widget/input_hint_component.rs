//! [`InputHint`] as a described component.

use crate::component::{Component, Described};
use crate::input::KeyboardBinding;

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
        Self::keyboard(KeyboardBinding::from_keys(
            props.keys.iter().map(String::as_str),
        ))
    }
}
