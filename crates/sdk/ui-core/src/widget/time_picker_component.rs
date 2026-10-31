//! [`TimePicker`] as a described component.

use chrono::Timelike;

use crate::component::{Bound, Component, Described, Event};
use crate::widget::TimeValue;

use super::TimePicker;

/// A time of day, as three spinners for hours, minutes and seconds: the arrows step a spinner
/// and so does the wheel over its cell. It is the time in the person's own zone today.
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

/// The time of day `at` is, when it names one: its instant, or the time of the day it names.
fn time_of(at: &str) -> Option<TimeValue> {
    if let Ok(instant) = chrono::DateTime::parse_from_rfc3339(at) {
        let time = instant.time();
        return Some(TimeValue {
            hours: time.hour().try_into().ok()?,
            minutes: time.minute().try_into().ok()?,
            seconds: Some(time.second().try_into().ok()?),
        });
    }
    let clock = at.get(11..19)?;
    let part = |range: std::ops::Range<usize>| -> Option<u8> { clock.get(range)?.parse().ok() };
    Some(TimeValue {
        hours: part(0..2)?,
        minutes: part(3..5)?,
        seconds: part(6..8),
    })
}

/// Today at `time`, as the instant a bound moment holds.
fn instant_today(time: TimeValue) -> String {
    let day = crate::clock::today();
    let at = day
        .and_hms_opt(
            u32::from(time.hours),
            u32::from(time.minutes),
            u32::from(time.seconds.unwrap_or(0)),
        )
        .unwrap_or_default()
        .and_utc();
    let at = at.with_nanosecond(0).unwrap_or(at);
    at.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

impl Described for TimePicker {
    type Props = TimePickerProps;

    fn from_props(key: &str, props: TimePickerProps) -> Self {
        let TimePickerProps { value, label, .. } = props;
        let shown = value.clone();
        let time = time_of(&value.state.get()).unwrap_or_else(|| TimeValue::new(9, 0));
        let mut picker = Self::new(time.hours, time.minutes)
            .on_change(move |hours, minutes, seconds| {
                let time = TimeValue {
                    hours: hours.min(23),
                    minutes: minutes.min(59),
                    seconds: seconds.map(|seconds| seconds.min(59)),
                };
                shown.edited(crate::component::Value::Text(instant_today(time)));
            })
            .with_id(key);
        if let Some(seconds) = time.seconds {
            picker = picker.with_seconds(seconds);
        }
        if let Some(label) = label {
            picker = picker.with_accessible_name(&label);
        }
        picker
    }

    /// The time changes in place.
    fn set_props(&mut self, props: TimePickerProps) -> bool {
        if let Some(time) = time_of(&props.value.state.get())
            && time != self.value()
        {
            self.set_value(time);
        }
        true
    }
}
