//! [`ColorPicker`] as a described component.

use crate::component::{Bound, Component, Described, Event, Value};
use crate::widget::ColorSwatch;

use super::ColorPicker;

/// A colour, picked from a saturation and value field, a hue strip, the red, green and blue sliders, or
/// a hex field; a palette of quick picks sits under them when given.
#[derive(Component)]
#[component(name = "ColorPicker", role = "Group", label = "label")]
pub struct ColorPickerProps {
    /// The colour, as its hex text ("#3a7bd5"), and where a picked colour goes.
    #[component(bind)]
    pub value: Bound<String>,
    /// A palette of quick picks under the sliders.
    #[component(data)]
    pub swatches: Vec<String>,
    /// How many swatches stand in a row.
    pub columns: Option<i64>,
    /// Its spoken name.
    pub label: Option<String>,
    /// A colour was picked: carries its hex text.
    #[component(event(value = String))]
    pub change: Event,
}

impl Default for ColorPickerProps {
    fn default() -> Self {
        Self {
            value: Bound::default(),
            swatches: Vec::new(),
            columns: None,
            label: None,
            change: Event::None,
        }
    }
}

fn color_of(hex: &str) -> Option<vello::peniko::Color> {
    crate::color::parse_hex(hex.trim())
}

impl Described for ColorPicker {
    type Props = ColorPickerProps;

    fn from_props(_key: &str, props: ColorPickerProps) -> Self {
        let ColorPickerProps {
            value,
            swatches,
            columns: _,
            label,
            ..
        } = props;
        let shown = value.clone();
        let fallback = swatches
            .iter()
            .find_map(|hex| color_of(hex))
            .unwrap_or(vello::peniko::Color::TRANSPARENT);
        let mut picker = Self::from_text_state(&value.state, fallback)
            .on_change(move |color| {
                shown.edited(Value::Text(crate::color::to_hex(color)));
            })
            .with_accessible_name(label.as_deref().map_or_else(
                || crate::i18n::widget_format("widget-color-name", &[]),
                str::to_owned,
            ));
        if !swatches.is_empty() {
            picker = picker.with_swatches(
                swatches
                    .iter()
                    .filter_map(|hex| color_of(hex).map(ColorSwatch::new)),
            );
        }
        picker
    }

    fn set_props(&mut self, props: ColorPickerProps) -> bool {
        // A colour set elsewhere shows in place through the binding, and so does its spoken
        // name; a palette of quick picks is what it is built around, so one that carries any
        // is built again.
        if !props.swatches.is_empty() {
            return false;
        }
        self.accessible_name = props.label;
        true
    }
}
