//! [`TimePicker`] as a described component.

use crate::component::{Bound, Component, Described, Event};
use crate::widget::TimeValue;

use super::TimePicker;

/// A time of day, as spinners for hours and minutes, and for seconds when the time holds some:
/// the arrows step a spinner and so does the wheel over its cell. It is the time in the person's
/// own zone today.
#[derive(Component)]
#[component(name = "TimePicker", role = "Group", label = "label")]
pub struct TimePickerProps {
    /// The moment, as the instant a component binds (its ISO text): today at the time chosen.
    /// A change sets the time kept in it.
    #[component(bind(ty = crate::component::Moment))]
    pub value: Bound<String>,
    /// Its spoken name.
    pub label: Option<String>,
    /// The moment changed: carries the instant, at the time chosen today.
    #[component(event(value = crate::component::Moment))]
    pub change: Event,
}

impl Default for TimePickerProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            change: Event::None,
        }
    }
}

/// Today at `time` in the person's zone, as the instant a bound moment holds.
fn instant_today(time: TimeValue) -> String {
    let at = jiff::civil::Time::new(
        i8::try_from(time.hours).unwrap_or(0),
        i8::try_from(time.minutes).unwrap_or(0),
        i8::try_from(time.seconds.unwrap_or(0)).unwrap_or(0),
        0,
    )
    .unwrap_or(jiff::civil::Time::midnight());
    crate::widget::date_field::instant_text(crate::clock::today(), at)
}

impl Described for TimePicker {
    type Props = TimePickerProps;

    fn from_props(key: &str, props: TimePickerProps) -> Self {
        let TimePickerProps { value, label, .. } = props;
        let shown = value.clone();
        let mut picker = Self::from_text_state(&value.state)
            .on_change(move |hours, minutes, seconds| {
                let time = TimeValue {
                    hours: hours.min(23),
                    minutes: minutes.min(59),
                    seconds: seconds.map(|seconds| seconds.min(59)),
                };
                shown.edited(crate::component::Value::Text(instant_today(time)));
            })
            .with_id(key);
        if let Some(seconds) = picker.value().seconds {
            picker = picker.with_seconds(seconds);
        }
        if let Some(label) = label {
            picker = picker.with_accessible_name(&label);
        }
        picker
    }

    /// The time changes in place.
    fn set_props(&mut self, props: TimePickerProps) -> bool {
        self.accessible_name = props.label;
        true
    }
}
