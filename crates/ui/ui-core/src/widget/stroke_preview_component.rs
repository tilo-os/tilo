//! [`StrokePreview`] as a described component.

use crate::component::{Component, Described};
use crate::widget::BrushCharacter;

use super::StrokePreview;

/// The mark a brush, its size and a pigment will paint, drawn as a sample stroke: a canvas's brush
/// pad shows it before a person touches the canvas, and draws it in again when any of the three
/// changes.
#[derive(Component, Default)]
#[component(name = "StrokePreview", role = "Image", label = "label")]
pub struct StrokePreviewProps {
    /// What the brush paints like: `round`, `pen`, `pencil`, `marker`, `airbrush`, `charcoal`,
    /// `calligraphy`, `eraser`, `pixel` or `pixel-eraser`; any other name paints like the round
    /// brush.
    #[component(required, positional)]
    pub brush: String,
    /// How wide the brush paints, in artwork units: a width past the preview's height is drawn as
    /// tall as the preview holds.
    pub size: f64,
    /// The pigment, as `#rrggbb` or `#rrggbbaa`.
    pub colour: String,
    /// Its spoken name.
    pub label: Option<String>,
}

impl Described for StrokePreview {
    type Props = StrokePreviewProps;

    fn from_props(_key: &str, props: StrokePreviewProps) -> Self {
        let StrokePreviewProps {
            brush,
            size,
            colour,
            label,
        } = props;
        let mut preview = Self::new(BrushCharacter::parse(&brush), size, super::pigment(&colour));
        preview.set_accessible_name(label);
        preview
    }

    /// The changed brush, size, colour or name is taken by the preview it stands in, which draws the
    /// new sample stroke in place.
    fn set_props(&mut self, props: StrokePreviewProps) -> bool {
        let StrokePreviewProps {
            brush,
            size,
            colour,
            label,
        } = props;
        self.set_accessible_name(label);
        self.set_sample(BrushCharacter::parse(&brush), size, super::pigment(&colour));
        true
    }
}
