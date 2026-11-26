//! [`CurveEditor`] as a described component: the points of a tone curve, shaped by hand.

use crate::widget::CurveEditor;
use tilo_ui_base::{component::Component, component::Described, component::Event};

/// A tone curve a person shapes: a box over the whole range each way, the straight line a plain
/// picture takes, and the points that bend it. A press puts a point where it landed or grabs
/// the one it landed on, a drag bends the curve through it, and letting go well outside the box
/// takes an interior point off again. The line is drawn as the grade applies it, so what is
/// shaped is what the picture gets.
#[derive(Component, Default)]
#[component(name = "CurveEditor", role = "Group", label = "label")]
pub struct CurveEditorProps {
    /// The points, a flat list of x and y each in `0..=1`, in ascending x. Both ends are kept
    /// at x 0 and x 1; an empty list is the straight line.
    pub points: Vec<f64>,
    /// How tall the box stands, in logical pixels: 160 unless said.
    pub height: f64,
    /// What the curve is, for assistive technology: "The red curve".
    pub label: Option<String>,
    /// The points moved: carries them all, a flat list of x and y, each `0..=1`.
    #[component(event(value = Vec<f64>))]
    pub change: Event,
}

impl Described for CurveEditor {
    type Props = CurveEditorProps;

    fn from_props(_key: &str, props: CurveEditorProps) -> Self {
        let mut editor = Self::new(props.points, props.change);
        if let Some(label) = props.label {
            editor = editor.with_label(label);
        }
        if props.height > 0.0 {
            editor = editor.with_height(props.height);
        }
        editor
    }

    fn set_props(&mut self, props: CurveEditorProps) -> bool {
        self.set_change(props.change);
        self.set_label(props.label);
        if props.height > 0.0 {
            self.set_height(props.height);
        }
        self.set_points(props.points);
        true
    }
}
