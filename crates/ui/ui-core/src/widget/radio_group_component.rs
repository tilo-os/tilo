//! [`RadioGroup`] as a described component.

use tilo_ui_base::{component::Component, component::Described, component::Event, component::Value};

use super::{RadioGroup, RadioOption};

tilo_ui_base::part_type!(RadioProps);

/// A short list of choices, exactly one of which is made, each a radio beside its words: for a
/// choice a person reads before making (how a report is sent, who may open a page). A few short
/// choices that switch a setting at once are a `ToggleGroup`; many are a `Dropdown`. The group is
/// one stop of the keyboard's order, and the arrow keys move the choice along it.
#[derive(Component)]
#[component(name = "RadioGroup", role = "Group", label = "label")]
pub struct RadioGroupProps {
    /// The choices, in order.
    #[component(part(default))]
    pub options: Vec<RadioProps>,
    /// Which choice is made, counted from 0: none until the person makes one.
    pub selected: Option<i64>,
    /// Its spoken name: what the choices are of.
    pub label: Option<String>,
    /// Whether any of it can be chosen now.
    pub enabled: bool,
    /// Why none of it can be chosen, while none can: its tooltip, and what assistive technology
    /// reads.
    pub description: Option<String>,
    /// A choice was made: carries its index, counted from 0.
    #[component(event(value = i64))]
    pub change: Event,
}

impl Default for RadioGroupProps {
    fn default() -> Self {
        Self {
            options: Vec::new(),
            selected: None,
            label: None,
            enabled: true,
            description: None,
            change: Event::default(),
        }
    }
}

/// One choice of a [`RadioGroup`]: what it says, and a line under that saying what it means.
#[derive(Clone, Component)]
#[component(name = "Radio", role = "RadioButton", label = "label")]
pub struct RadioProps {
    /// What it says, beside its radio: the whole row chooses it.
    #[component(required, positional)]
    pub label: String,
    /// A line under it in smaller words: what choosing it means.
    pub detail: Option<String>,
    /// Whether it can be chosen now.
    pub enabled: bool,
    /// Why it cannot be chosen, while it cannot: its tooltip, and what assistive technology
    /// reads.
    pub description: Option<String>,
}

impl Default for RadioProps {
    fn default() -> Self {
        Self {
            label: String::new(),
            detail: None,
            enabled: true,
            description: None,
        }
    }
}

impl From<RadioProps> for RadioOption {
    fn from(props: RadioProps) -> Self {
        let mut option = RadioOption::new(props.label).with_enabled(props.enabled);
        if let Some(detail) = props.detail {
            option = option.with_detail(detail);
        }
        if let Some(why) = props.description {
            option = option.with_why(why);
        }
        option
    }
}

fn index_of(selected: Option<i64>, count: usize) -> Option<usize> {
    selected
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index < count)
}

impl Described for RadioGroup {
    type Props = RadioGroupProps;

    fn from_props(key: &str, props: RadioGroupProps) -> Self {
        let RadioGroupProps {
            options,
            selected,
            label,
            enabled,
            description,
            change,
        } = props;
        let options: Vec<RadioOption> = options.into_iter().map(RadioOption::from).collect();
        let selected = index_of(selected, options.len());
        let mut group = Self::new(options, selected)
            .with_id(key)
            .with_enabled(enabled);
        if let Some(label) = label {
            group = group.with_accessible_name(label);
        }
        if let Some(why) = description {
            group = group.with_why(why);
        }
        if !change.is_none() {
            group = group.on_change(move |index| {
                change.fire(Some(Value::Int(i64::try_from(index).unwrap_or(i64::MAX))));
            });
        }
        group
    }

    /// The choice made, their words and whether they can be made change in place: a group of
    /// another number of choices is built again.
    fn set_props(&mut self, props: RadioGroupProps) -> bool {
        if props.options.len() != self.options().len() {
            return false;
        }
        if props.label != self.accessible_name {
            return false;
        }
        self.set_selected(index_of(props.selected, props.options.len()));
        self.set_options(props.options.into_iter().map(RadioOption::from).collect());
        self.set_enabled(props.enabled);
        self.set_why(props.description);
        true
    }
}
