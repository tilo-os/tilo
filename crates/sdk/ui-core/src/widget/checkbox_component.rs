//! [`Checkbox`] as a described component.

use crate::component::{Bound, Component, Described, Value};

use super::Checkbox;

/// A box that is marked or not, bound to what it edits (whether to be told of a reply, which
/// rows a bulk action takes). A labelled box is one target, its label included.
#[derive(Component)]
#[component(name = "Checkbox", role = "Checkbox", label = "label")]
pub struct CheckboxProps {
    /// Whether it is marked, and where a mark made in it goes.
    #[component(bind)]
    pub value: Bound<bool>,
    /// What it is for, beside the box: the whole row chooses it.
    pub label: Option<String>,
    /// Some of what it stands for is chosen: a mixed mark. The value stays `false`, so the next
    /// choose marks all of it.
    pub indeterminate: bool,
    /// Whether it can be chosen now.
    pub enabled: bool,
}

impl Default for CheckboxProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            indeterminate: false,
            enabled: true,
        }
    }
}

impl Described for Checkbox {
    type Props = CheckboxProps;

    fn from_props(key: &str, props: CheckboxProps) -> Self {
        let CheckboxProps {
            value,
            label,
            indeterminate,
            enabled,
        } = props;
        let edits = value.clone();
        let mut boxed = Self::from_state(&value.state)
            .on_change(move |marked| edits.edited(Value::Bool(marked)))
            .with_id(key)
            .with_enabled(enabled);
        if let Some(label) = label {
            boxed = boxed.with_label(label);
        }
        boxed.indeterminate(indeterminate)
    }

    /// Whether it is marked changes in place; a box that gains or loses its label, or its mixed
    /// mark, is built again.
    fn set_props(&mut self, props: CheckboxProps) -> bool {
        if props.label.is_some() != self.has_label()
            || props.indeterminate != self.is_indeterminate()
        {
            return false;
        }
        let marked = props.value.state.get();
        self.set_enabled(props.enabled);
        self.set_checked(marked);
        true
    }
}
