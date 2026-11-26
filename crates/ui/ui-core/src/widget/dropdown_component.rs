//! [`Dropdown`] as a described component.

use tilo_ui_base::{component::Bound, component::Component, component::Described, component::Event, component::Value};
use tilo_ui_base::icons::Icon;
use crate::widget::ControlSize;

use super::Dropdown;

/// One of a short list, chosen from a field that opens it: a project, a sort order, a unit.
#[derive(Component)]
#[component(name = "Dropdown", role = "ComboBox", label = "placeholder")]
pub struct DropdownProps {
    /// What is chosen, and where a choice goes.
    #[component(bind)]
    pub value: Bound<String>,
    /// What the list offers, in order.
    pub options: Vec<String>,
    /// What each option stands for, in order, when it is not the text shown: the list offers the
    /// words, and a choice reaches the bound value as its id. Empty makes each option its own
    /// value.
    #[component(data)]
    pub values: Vec<String>,
    /// What the field says while nothing is chosen, and what names it to assistive technology.
    pub placeholder: Option<String>,
    /// A glyph before the field: the kind of thing chosen.
    pub icon: Option<Icon>,
    /// A caption above the field.
    pub caption: Option<String>,
    /// How large the field is: `regular` unless said.
    pub size: ControlSize,
    /// Take the row's width rather than fitting its content.
    pub fill: bool,
    /// Whether it can be chosen from now.
    pub enabled: bool,
    /// A choice was made: carries the chosen option's text.
    #[component(event(value = String))]
    pub change: Event,
}

impl Default for DropdownProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            options: Vec::new(),
            values: Vec::new(),
            placeholder: None,
            icon: None,
            caption: None,
            size: ControlSize::default(),
            fill: false,
            enabled: true,
            change: Event::None,
        }
    }
}

impl Described for Dropdown {
    type Props = DropdownProps;

    fn from_props(key: &str, props: DropdownProps) -> Self {
        let DropdownProps {
            value,
            options,
            values,
            placeholder,
            icon,
            caption,
            size,
            fill,
            enabled,
            change,
        } = props;
        let edits = value.clone();
        let mut dropdown = Self::from_state(options.clone(), &value.state)
            .with_id(key)
            .on_change(move |chosen| {
                // A view that names an action for the choice owns it: the action records the
                // change with its words, rather than a bare edit of the bound field.
                if change.is_none() {
                    edits.edited(Value::Text(chosen.to_owned()));
                } else {
                    change.fire(Some(Value::Text(chosen.to_owned())));
                }
            })
            .with_enabled(enabled)
            .control_size(size);
        if !values.is_empty() {
            dropdown.set_value_options(values.into_iter().zip(options).collect());
        }
        if let Some(placeholder) = &placeholder {
            dropdown = dropdown
                .with_placeholder(placeholder)
                .with_accessible_name(placeholder);
        }
        if let Some(icon) = icon {
            dropdown = dropdown.leading_icon(icon);
        }
        if let Some(caption) = caption {
            dropdown = dropdown.caption(caption);
        }
        if fill {
            dropdown = dropdown.fill();
        }
        dropdown
    }

    fn set_props(&mut self, props: DropdownProps) -> bool {
        if props.size != ControlSize::default() || props.fill {
            return false;
        }
        self.set_enabled(props.enabled);
        if props.values.is_empty() {
            if props.options != self.options() {
                self.set_options(props.options);
            }
        } else if props.values != self.values() || props.options != self.options() {
            self.set_value_options(props.values.into_iter().zip(props.options).collect());
        }
        let chosen = props.value.state.get();
        if chosen != self.selected() {
            self.set_selected(chosen);
        }
        true
    }
}
