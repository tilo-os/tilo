//! [`Checkbox`] as a described component.

use crate::component::{Bound, Component, Described, Event, Value};

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
    #[component(positional)]
    pub label: Option<String>,
    /// Some of what it stands for is chosen: a mixed mark. The value stays `false`, so the next
    /// choose marks all of it.
    pub indeterminate: bool,
    /// Whether it can be chosen now.
    pub enabled: bool,
    /// It was pressed: what a mark is, in the app's words. A task and every step under it are
    /// done in one thing to undo; with no `check`, a press changes the bound value.
    #[component(event)]
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

    /// A mark is the box's own action: the widget holds the binding, so the press is a route
    /// an agent's invocation of it lands on, and both run from the door.
    fn press_events() -> &'static [&'static str] {
        &["check"]
    }

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
            // The app's own action for a mark, run through the door like any other.
            Event::Action(binding) => boxed = boxed.on_choose(binding),
            Event::View(intent) => boxed = boxed.on_change(move |_| intent.fire(None)),
        }
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
        self.set_feel(props.motion);
        self.set_checked(marked);
        true
    }
}
