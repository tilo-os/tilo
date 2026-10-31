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

/// A day given as its ISO text.
fn day_of(text: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok()
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
        let selected = day_of(&value.state.get()).unwrap_or_else(crate::clock::today);
        let mut picker = Self::new(selected)
            .on_change(move |day| {
                shown.edited(crate::component::Value::Text(
                    day.format("%Y-%m-%d").to_string(),
                ));
            })
            .marked(marked.iter().filter_map(|day| day_of(day)));
        if let Some(label) = label {
            picker = picker.with_accessible_name(&label);
        }
        picker
    }

    /// The day chosen changes in place; its marked days are built again.
    fn set_props(&mut self, props: DatePickerProps) -> bool {
        if !props.marked.is_empty() || self.has_marked() {
            return false;
        }
        if let Some(day) = day_of(&props.value.state.get())
            && day != self.selected()
        {
            self.set_selected(day);
        }
        true
    }
}
