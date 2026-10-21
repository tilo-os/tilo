//! What the pointer and keys do on the canvas, apart from the widget: each
//! finished gesture is one of the artwork's actions, made here from the
//! marks and the pointer's path, so it can be tested without a window.

pub mod area;
pub mod brush;
pub mod edit;
pub mod outline;
pub mod points;
pub mod read;
pub mod snap;
pub mod text;

/// What the tools put down, read from their settings when a gesture
/// starts: the one thing gestures need to know of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Ink {
    /// `#rrggbb`.
    pub colour: String,
    /// The brush's width, and the outline width of lines and open paths,
    /// in artwork units.
    pub width: f64,
    /// The layer new marks go on; the top layer when absent.
    pub layer: Option<String>,
}
