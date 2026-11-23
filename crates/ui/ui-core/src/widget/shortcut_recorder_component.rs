//! [`ShortcutRecorder`] as a described component.

use crate::component::{Bound, Component, Described};

use super::ShortcutRecorder;

/// A keyboard shortcut as keycaps that are the control: pressing them records a new chord, with
/// a reset while it differs from the default.
#[derive(Component, Default)]
#[component(name = "ShortcutRecorder", role = "Button", label = "label")]
pub struct ShortcutRecorderProps {
    /// The chord, as its text ("Ctrl+Shift+K"), and where a recorded one goes.
    #[component(bind)]
    pub value: Bound<String>,
    /// The chord it started as, which its reset puts back.
    #[component(data)]
    pub default: Option<String>,
    /// Its spoken name: the command the shortcut runs.
    pub label: Option<String>,
}

impl Described for ShortcutRecorder {
    type Props = ShortcutRecorderProps;

    fn from_props(key: &str, props: ShortcutRecorderProps) -> Self {
        let ShortcutRecorderProps {
            value,
            default,
            label,
        } = props;
        let edits = value.clone();
        let default = default.unwrap_or_default();
        let _ = (key, label);
        Self::new(key, &value.state, &default).on_edit(edits)
    }

    /// The chord shown changes in place; another default is built again.
    fn set_props(&mut self, props: ShortcutRecorderProps) -> bool {
        if props
            .default
            .as_deref()
            .is_some_and(|default| default != self.default_chord())
        {
            return false;
        }
        let chord = props.value.state.get();
        if chord != self.chord() {
            self.set_chord(&chord);
        }
        true
    }
}
