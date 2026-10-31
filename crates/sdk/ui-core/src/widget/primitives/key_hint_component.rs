//! [`KeyHints`] and its hints as described components.

use crate::component::{Component, Described};

use super::{KeyHint, KeyHints};

/// Shortcuts and what they do, side by side: each hint the drawn keycaps of a chord and its
/// label ("Enter Save", "Esc Cancel"), in the person's own keyboard legends. Never a sentence
/// with a separator glyph.
#[derive(Component, Default)]
#[component(name = "KeyHints", role = "Group")]
pub struct KeyHintsProps {
    /// The hints, in order.
    #[component(slot(default))]
    pub hints: Vec<KeyHintProps>,
}

/// One hint of a strip: the keys of a chord and what pressing them does.
#[derive(Component, Default)]
#[component(name = "KeyHint", role = "StaticText", label = "label")]
pub struct KeyHintProps {
    /// The keys of the chord, modifiers first: `["Ctrl", "S"]`.
    #[component(required, positional)]
    pub keys: Vec<String>,
    /// What pressing them does, in a word or two: "Save".
    #[component(required, positional)]
    pub label: String,
}

crate::component::part_slot!(KeyHintProps);

impl Described for KeyHints {
    type Props = KeyHintsProps;

    fn from_props(_key: &str, props: KeyHintsProps) -> Self {
        Self::new(
            props
                .hints
                .into_iter()
                .map(|hint| KeyHint::new(hint.keys.iter().map(String::as_str), hint.label))
                .collect(),
        )
    }
}
