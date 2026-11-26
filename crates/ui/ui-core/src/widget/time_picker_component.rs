//! [`TimePicker`] as a described component.

use crate::widget::TimeValue;
use tilo_ui_base::{
    component::Bound, component::Component, component::Described, component::Event,
};

use super::TimePicker;

/// A time of day, as spinners for hours and minutes, and for seconds when the time holds some:
/// the arrows step a spinner and so does the wheel over its cell. It is the time in the person's
/// own zone today.
#[derive(Component)]
#[component(name = "TimePicker", role = "Group", label = "label")]
pub struct TimePickerProps {
    /// The moment, as the instant a component binds (its ISO text): today at the time chosen.
    /// A change sets the time kept in it.
    #[component(bind(ty = tilo_ui_base::component::Moment))]
    pub value: Bound<String>,
    /// Its spoken name.
    pub label: Option<String>,
    /// The minutes the wheel and the chevrons step the minute cell by: a quarter hour by default.
    pub step: i64,
    /// The time changed: carries the instant, at the time chosen today.
    #[component(event(value = tilo_ui_base::component::Moment))]
    pub changed: Event,
}

impl Default for TimePickerProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            step: 15,
            changed: Event::None,
        }
    }
}

/// A step in minutes, clamped to the minute dial.
fn step_of(step: i64) -> u8 {
    u8::try_from(step.clamp(1, 59)).unwrap_or(15)
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
    crate::widget::date_field::instant_text(tilo_ui_base::clock::today(), at)
}

impl Described for TimePicker {
    type Props = TimePickerProps;

    fn from_props(key: &str, props: TimePickerProps) -> Self {
        let TimePickerProps {
            value,
            label,
            step,
            changed,
        } = props;
        let shown = value.clone();
        let fires = changed;
        let mut picker = Self::from_text_state(&value.state)
            .on_change(move |hours, minutes, seconds| {
                let time = TimeValue {
                    hours: hours.min(23),
                    minutes: minutes.min(59),
                    seconds: seconds.map(|seconds| seconds.min(59)),
                };
                let instant = instant_today(time);
                shown.edited(tilo_ui_base::component::Value::Text(instant.clone()));
                fires.fire(Some(tilo_ui_base::component::Value::Text(instant)));
            })
            .with_step(step_of(step))
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
        self.set_step(step_of(props.step));
        true
    }
}
