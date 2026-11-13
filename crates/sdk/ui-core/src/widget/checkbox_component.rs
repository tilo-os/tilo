//! [`Checkbox`] as a described component.

use crate::component::{Bound, Component, Described, Event, Value};

use super::Checkbox;

/// A box that is marked or not, bound to what it edits (whether to be told of a reply, which rows a
/// bulk action takes).
#[derive(Component)]
#[component(name = "Checkbox", role = "Checkbox", label = "label")]
pub struct CheckboxProps {
    /// Whether it is marked, and where a mark made in it goes.
    #[component(bind)]
    pub value: Bound<bool>,
    /// What it is for, beside the box: the whole row chooses it.
    #[component(positional)]
    pub label: Option<String>,
    /// Some of what it stands for is chosen: a mixed mark.
    pub indeterminate: bool,
    /// Whether it can be chosen now.
    pub enabled: bool,
    /// It was pressed: what a mark is, in the app's words.
    #[component(event(press))]
    pub check: Event,
    /// The pace its mark plays at: the theme's unless it says otherwise.
    pub motion: crate::motion::Feel,
}

impl Default for CheckboxProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            indeterminate: false,
            enabled: true,
            check: Event::None,
            motion: crate::motion::Feel::Standard,
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
            check,
            motion,
        } = props;
        let mut boxed = Self::from_state(&value.state)
            .with_id(key)
            .with_enabled(enabled)
            .with_feel(motion);
        match check {
            Event::None => {
                let edits = value.clone();
                boxed = boxed.on_change(move |marked| edits.edited(Value::Bool(marked)));
            }
            Event::Action(binding) => boxed = boxed.on_choose(binding),
            Event::View(intent) => boxed = boxed.on_change(move |_| intent.fire(None)),
        }
        if let Some(label) = label {
            boxed = boxed.with_label(label);
        }
        boxed.indeterminate(indeterminate)
    }

    fn set_props(&mut self, props: CheckboxProps) -> bool {
        if props.label.is_some() != self.has_label()
            || props.indeterminate != self.is_indeterminate()
        {
            return false;
        }
        let marked = props.value.state.get();
        self.set_enabled(props.enabled);
        self.set_feel(props.motion);
        self.set_checked(marked);
        true
    }
}
