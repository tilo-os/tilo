//! [`Dropdown`] as a described component.

use crate::component::{Bound, Component, Described, Value};
use crate::icons::Icon;
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
}

impl Default for DropdownProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            options: Vec::new(),
            placeholder: None,
            icon: None,
            caption: None,
            size: ControlSize::default(),
            fill: false,
            enabled: true,
        }
    }
}

impl Described for Dropdown {
    type Props = DropdownProps;

    fn from_props(key: &str, props: DropdownProps) -> Self {
        let DropdownProps {
            value,
            options,
            placeholder,
            icon,
            caption,
            size,
            fill,
            enabled,
        } = props;
        let edits = value.clone();
        let mut dropdown = Self::from_state(options, &value.state)
            .on_change(move |chosen| edits.edited(Value::Text(chosen.to_owned())))
            .with_id(key)
            .with_enabled(enabled)
            .control_size(size);
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
        if props.options != self.options() {
            self.set_options(props.options);
        }
        let chosen = props.value.state.get();
        if chosen != self.selected() {
            self.set_selected(chosen);
        }
        true
    }
}
