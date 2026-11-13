//! [`ColorSwatchPicker`] as a described component.

use crate::component::{Component, Described, Event, Value};
use crate::widget::ColorSwatch;

use super::ColorSwatchPicker;

/// A grid of swatches, one chosen (a palette of accents): the chosen cell carries the ring.
#[derive(Component, Default)]
#[component(name = "ColorSwatchPicker", role = "Group", label = "label")]
pub struct ColorSwatchPickerProps {
    /// The swatches, as hex colours, in order.
    #[component(required)]
    #[component(data)]
    pub swatches: Vec<String>,
    /// How many stand in a row.
    pub columns: i64,
    /// Which is chosen, counted from 0: none until a person chooses one.
    pub selected: Option<i64>,
    /// Its spoken name.
    pub label: Option<String>,
    /// A swatch was chosen: carries its index, counted from 0.
    #[component(event(value = i64))]
    pub change: Event,
}

impl Described for ColorSwatchPicker {
    type Props = ColorSwatchPickerProps;

    fn from_props(key: &str, props: ColorSwatchPickerProps) -> Self {
        let ColorSwatchPickerProps {
            swatches,
            columns,
            selected,
            label,
            change,
        } = props;
        let swatches: Vec<ColorSwatch> = swatches
            .iter()
            .filter_map(|hex| crate::color::parse_hex(hex.trim()).map(ColorSwatch::new))
            .collect();
        let count = usize::try_from(columns.max(1)).unwrap_or(1);
        let _ = key;
        let mut picker = Self::new(swatches, count);
        if let Some(index) = selected.and_then(|index| usize::try_from(index).ok()) {
            picker.set_selected(Some(index));
        }
        let _ = label;
        if !change.is_none() {
            picker = picker.on_change(move |index| {
                change.fire(Some(Value::Int(i64::try_from(index).unwrap_or(i64::MAX))));
            });
        }
        picker
    }

    fn set_props(&mut self, props: ColorSwatchPickerProps) -> bool {
        if props.swatches.len() != self.count() {
            return false;
        }
        self.set_selected(
            props
                .selected
                .and_then(|index| usize::try_from(index).ok())
                .filter(|index| *index < self.count()),
        );
        true
    }
}
