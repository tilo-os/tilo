//! [`DateField`] as a described component.

use crate::component::{Bound, Component, Described, Event};

use super::DateField;

/// A form's date, shown in the person's format and picked from a calendar that opens over it.
/// A date that may be empty can be cleared.
#[derive(Component)]
#[component(name = "DateField", role = "ComboBox", label = "label")]
pub struct DateFieldProps {
    /// The day, as the moment a component binds (its ISO text), and where a picked day goes.
    #[component(bind(ty = crate::component::Moment))]
    pub value: Bound<String>,
    /// Its spoken name: what the date is for.
    pub label: Option<String>,
    /// The date may be cleared: the calendar offers "Clear date".
    pub optional: bool,
    /// A day was picked, or the date was cleared: carries the moment, or nothing for a cleared
    /// one.
    #[component(event(value = crate::component::Moment))]
    pub change: Event,
}

impl Default for DateFieldProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            optional: false,
            change: Event::None,
        }
    }
}

/// The day `value` holds, when it is one (its ISO text).
fn day_of(value: &str) -> Option<chrono::NaiveDate> {
    use chrono::NaiveDate;
    NaiveDate::parse_from_str(value.get(..10).unwrap_or(value), "%Y-%m-%d").ok()
}

/// A picked day as the moment a bound field holds: its ISO text, empty for none.
fn moment(day: Option<chrono::NaiveDate>) -> String {
    day.map_or_else(String::new, |day| day.format("%Y-%m-%d").to_string())
}

impl Described for DateField {
    type Props = DateFieldProps;

    fn from_props(key: &str, props: DateFieldProps) -> Self {
        let DateFieldProps {
            value,
            label,
            optional,
            ..
        } = props;
        let _ = label;
        let shown = value.clone();
        Self::new(key, day_of(&value.state.get()), optional).on_change(move |day| {
            shown.edited(crate::component::Value::Text(moment(day)));
        })
    }

    /// A date set elsewhere is shown in place; whether it may be cleared builds it again.
    fn set_props(&mut self, props: DateFieldProps) -> bool {
        if props.optional != self.is_optional() {
            return false;
        }
        let day = day_of(&props.value.state.get());
        if day != self.value() {
            self.set_value(day);
            return true;
        }
        false
    }
}
