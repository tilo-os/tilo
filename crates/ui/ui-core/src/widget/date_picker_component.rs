//! [`DatePicker`] as a described component.

use tilo_ui_base::{component::Bound, component::Component, component::Described, component::Event};

use super::DatePicker;

/// A month's calendar: the day chosen is ringed, today marked, and the arrows go between months.
#[derive(Component)]
#[component(name = "DatePicker", role = "Group", label = "label")]
pub struct DatePickerProps {
    /// The day chosen, as the moment a component binds (its ISO text), and where choosing one goes.
    #[component(bind(ty = tilo_ui_base::component::Moment))]
    pub value: Bound<String>,
    /// Days that hold something (an event, a note): a dot under the number.
    #[component(data)]
    pub marked: Vec<String>,
    /// Its spoken name.
    pub label: Option<String>,
    /// A day was chosen: carries the moment.
    #[component(event(value = tilo_ui_base::component::Moment))]
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

    fn from_props(key: &str, props: DatePickerProps) -> Self {
        let DatePickerProps {
            value,
            marked,
            label,
            ..
        } = props;
        let shown = value.clone();
        let mut picker = Self::from_text_state(&value.state)
            .on_choose(key, move |day| {
                shown.edited(tilo_ui_base::component::Value::Text(day.to_string()));
            })
            .marked(
                marked
                    .iter()
                    .filter_map(|day| tilo_ui_base::component::moment_day(day)),
            );
        if let Some(label) = label {
            picker = picker.with_accessible_name(&label);
        }
        picker
    }

    fn set_props(&mut self, props: DatePickerProps) -> bool {
        // A day set elsewhere shows in place through the binding; its spoken name changes
        // where it stands. Days that hold something are the calendar's own marks, which only
        // building it again lays out.
        if !props.marked.is_empty() || self.has_marked() {
            return false;
        }
        self.accessible_name = props.label;
        true
    }
}
