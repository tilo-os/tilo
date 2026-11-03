//! [`DatePicker`] as a described component.

use crate::component::{Bound, Component, Described, Event};

use super::DatePicker;

/// A month's calendar: the day chosen is ringed, today marked, and the arrows go between
/// months. Days that hold something carry a dot.
#[derive(Component)]
#[component(name = "DatePicker", role = "Group", label = "label")]
pub struct DatePickerProps {
    /// The day chosen, as the moment a component binds (its ISO text), and where choosing one
    /// goes.
    #[component(bind(ty = crate::component::Moment))]
    pub value: Bound<String>,
    /// Days that hold something (an event, a note): a dot under the number.
    pub marked: Vec<String>,
    /// Its spoken name.
    pub label: Option<String>,
    /// A day was chosen: carries the moment.
    #[component(event(value = crate::component::Moment))]
    pub change: Event,
}

impl Default for DatePickerProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            marked: Vec::new(),
            label: None,
            change: Event::None,
        }
    }
}

impl Described for DatePicker {
    type Props = DatePickerProps;

    fn from_props(_key: &str, props: DatePickerProps) -> Self {
        let DatePickerProps {
            value,
            marked,
            label,
            ..
        } = props;
        let shown = value.clone();
        let mut picker = Self::from_text_state(&value.state)
            .on_change(move |day| {
                shown.edited(crate::component::Value::Text(
                    day.format("%Y-%m-%d").to_string(),
                ));
            })
            .marked(
                marked
                    .iter()
                    .filter_map(|day| crate::component::moment_day(day)),
            );
        if let Some(label) = label {
            picker = picker.with_accessible_name(&label);
        }
        picker
    }

    /// The day chosen changes in place; its marked days are built again.
    fn set_props(&mut self, props: DatePickerProps) -> bool {
        // A day set elsewhere shows in place through the binding.
        props.marked.is_empty() && !self.has_marked()
    }
}
