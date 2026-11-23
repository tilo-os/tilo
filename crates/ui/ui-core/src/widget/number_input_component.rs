//! [`NumberInput`] as a described component.

use crate::component::{Bound, Component, Described};
use crate::widget::ControlSize;

use super::NumberInput;

/// A number with a step either side (a page size, a margin, a count), set by typing, its
/// steppers and the arrow keys within its range.
#[derive(Component)]
#[component(name = "NumberInput", role = "SpinButton", label = "label")]
pub struct NumberInputProps {
    /// The value, and where a change of it goes.
    #[component(bind)]
    pub value: Bound<f64>,
    /// The number's spoken name, for a field with no label beside it.
    pub label: Option<String>,
    /// The smallest it may be.
    pub min: Option<f64>,
    /// The largest it may be.
    pub max: Option<f64>,
    /// How much one press of a stepper adds; 1 unless said.
    pub step: Option<f64>,
    /// Decimal places it shows: 0 unless said, for a whole number.
    pub precision: Option<i64>,
    /// A unit after the number, shown and not typed: "px", "%".
    pub suffix: Option<String>,
    /// Smaller, for a toolbar: bare steppers, no field frame.
    pub compact: bool,
    /// Whether it can be changed now.
    pub enabled: bool,
}

impl Default for NumberInputProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            label: None,
            min: None,
            max: None,
            step: None,
            precision: None,
            suffix: None,
            compact: false,
            enabled: true,
        }
    }
}

impl Described for NumberInput {
    type Props = NumberInputProps;

    fn from_props(key: &str, props: NumberInputProps) -> Self {
        let NumberInputProps {
            value,
            label,
            min,
            max,
            step,
            precision,
            suffix,
            compact,
            enabled,
        } = props;
        let edits = value.clone();
        let mut input = Self::from_state(&value.state)
            .on_edit(edits)
            .with_id(key)
            .with_enabled(enabled);
        if let Some(label) = label {
            input = input.with_accessible_name(label);
        }
        if let (Some(min), Some(max)) = (min, max) {
            input = input.with_range(min, max);
        } else if let Some(max) = max {
            input = input.with_range(f64::NEG_INFINITY, max);
        } else if let Some(min) = min {
            input = input.with_range(min, f64::INFINITY);
        }
        if let Some(step) = step {
            input = input.with_step(step);
        }
        if let Some(precision) = precision {
            input = input.with_precision(usize::try_from(precision.max(0)).unwrap_or_default());
        }
        if let Some(suffix) = suffix {
            input = input.with_suffix(&suffix);
        }
        if compact {
            input = input.compact().control_size(ControlSize::Small);
        }
        input
    }

    /// Its value changes in place; another range, step or shape is built again.
    fn set_props(&mut self, props: NumberInputProps) -> bool {
        if self.is_compact() != props.compact
            || self.range()
                != (
                    props.min.unwrap_or(f64::NEG_INFINITY),
                    props.max.unwrap_or(f64::INFINITY),
                )
        {
            return false;
        }
        self.set_enabled(props.enabled);
        self.set_value(props.value.state.get());
        true
    }
}
