//! [`Slider`] as a described component.

use super::Slider;
use crate::component::{Bound, Component, Described, Event, Value};

/// A value along a track (a level, a volume, a share), set by dragging the knob or pressing the
/// track and stepped by the arrow keys, finely with Shift.
#[derive(Component)]
#[component(name = "Slider", role = "Slider", label = "label")]
pub struct SliderProps {
    /// The value, and where a change of it goes.
    #[component(bind)]
    pub value: Bound<f64>,
    /// The value's spoken name, for a slider with no label beside it: "Volume".
    pub label: Option<String>,
    /// The lowest value, 0 unless said.
    pub min: Option<f64>,
    /// The highest value, 1 unless said.
    pub max: Option<f64>,
    /// Values snap to multiples of this from the minimum; the arrow keys move one step.
    pub step: Option<f64>,
    /// Show the value in words beside the track: "72%".
    pub shows_value: bool,
    /// Whether it can be moved now.
    pub enabled: bool,
    /// The person moved it: the value it is at now is the value.
    #[component(event(value = f64))]
    pub change: Event,
}

impl Default for SliderProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            min: None,
            max: None,
            step: None,
            shows_value: false,
            enabled: true,
            change: Event::None,
        }
    }
}

impl Described for Slider {
    type Props = SliderProps;

    fn from_props(key: &str, props: SliderProps) -> Self {
        let SliderProps {
            value,
            label,
            min,
            max,
            step,
            shows_value,
            enabled,
            change,
        } = props;
        let edits = value.clone();
        let mut slider = Self::from_state(&value.state)
            .on_change(move |number| {
                edits.edited(Value::Float(number));
                change.fire(Some(Value::Float(number)));
            })
            .with_id(key)
            .with_enabled(enabled);
        if let Some(label) = label {
            slider = slider.with_accessible_name(&label);
        }
        if let (Some(min), Some(max)) = (min, max) {
            slider = slider.range(min, max);
        } else if let Some(max) = max {
            slider = slider.range(0.0, max);
        } else if let Some(min) = min {
            slider = slider.range(min, 1.0);
        }
        if let Some(step) = step {
            slider = slider.step(step);
        }
        if shows_value {
            slider = slider.shows_value();
        }
        slider
    }

    /// Its value and whether it can be moved change in place; another range or step is built
    /// again.
    fn set_props(&mut self, props: SliderProps) -> bool {
        let range = (props.min.unwrap_or(0.0), props.max.unwrap_or(1.0));
        if range != self.bounds() || props.step != self.step_size() {
            return false;
        }
        self.set_enabled(props.enabled);
        self.set_value(props.value.state.get());
        true
    }
}
