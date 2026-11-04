//! [`TimeField`] as a described component.

use crate::component::{Bound, Component, Described, Event};

use super::TimeField;

/// The time of day a day is due at: the hour and the minute it is due at, with "Remove time"
/// beside them, or a quiet "Add time" while it has none. It stands in the `time` slot of a
/// [`DateField`](crate::widget::DateField), so the day and its time read as one fact.
#[derive(Component)]
#[component(name = "TimeField", role = "Group")]
pub struct TimeFieldProps {
    /// The moment, as a bound field holds it (`09:00`, or the ISO text of an instant), and
    /// where a picked time of day goes.
    #[component(bind(ty = crate::component::Moment))]
    pub value: Bound<String>,
    /// A time was picked or removed: carries the time of day, or nothing for a removed one.
    #[component(event(value = crate::component::Moment))]
    pub change: Event,
}

impl Default for TimeFieldProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            change: Event::None,
        }
    }
}

/// A picked time as the moment a bound field holds: `09:00`, empty for none.
fn moment(time: Option<chrono::NaiveTime>) -> String {
    // format: machine (the time of day a bound moment field holds, which its DateField reads
    // back; people read the day and its time through ui::format in the chip)
    time.map_or_else(String::new, |time| time.format("%H:%M").to_string())
}

impl Described for TimeField {
    type Props = TimeFieldProps;

    fn from_props(key: &str, props: TimeFieldProps) -> Self {
        let TimeFieldProps { value, .. } = props;
        let shown = value.clone();
        Self::from_state(key, &value.state)
            .on_change(move |time| shown.edited(crate::component::Value::Text(moment(time))))
    }
}
