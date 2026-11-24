//! This machine's keyboard as a view draws it: the layouts it installs, and the keys of the
//! layout being inspected with what each one types at every modifier level.
//!
//! The drawing is the device's own: its physical model's outlines and the native XKB compiler's
//! legends, made for the layout the person is inspecting (or the one the keyboard types), so a
//! view draws what it is given and never compiles a layout itself.

use serde::{Deserialize, Serialize};

/// One keyboard layout this device installs, by the id it is typed by.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct KeyboardLayout {
    /// The XKB source id: `de`, `us:dvorak`.
    pub id: String,
    /// What it calls itself: `German`, `English (US) — Dvorak`.
    pub label: String,
}

/// One corner of a key's physical outline.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct KeyboardPoint {
    pub x: f64,
    pub y: f64,
}

/// One key of the drawing: where its body is and what it types.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct KeyboardKey {
    /// The physical key's XKB name (`AD01`, `SPCE`), which inspection names it by.
    pub name: String,
    /// Its body's corners, in the drawing's units from its top left.
    pub outline: Vec<KeyboardPoint>,
    /// What it types at each modifier level, the base level first: `a`, `A`, `á`, …
    pub levels: Vec<String>,
}

/// A keyboard's keys, drawn for one layout and physical model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct KeyboardDrawing {
    /// The source id the keys are drawn for.
    pub layout: String,
    /// The physical model they are laid out as: `pc105`.
    pub model: String,
    /// The width and height of the box the keys fill, in the drawing's units.
    pub width: f64,
    pub height: f64,
    /// How many modifier levels the layout supplies; every key's `levels` has as many.
    pub level_count: i64,
    pub keys: Vec<KeyboardKey>,
}

/// The keyboard this machine has, as a view reads it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct Keyboard {
    /// The layouts installed, in the order they are listed.
    pub layouts: Vec<KeyboardLayout>,
    /// Why the installed list could not be read; `layouts` is then a short built-in list.
    pub layouts_error: Option<String>,
    /// The keys of the layout being inspected, or of the one the keyboard types.
    pub drawing: Option<KeyboardDrawing>,
    /// Why the keys could not be drawn.
    pub drawing_error: Option<String>,
}

/// The keyboard as a view of another language reads it.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Keyboard, tilo_resource::NoEffect>(
        "keyboard", "Keyboard",
    )
}
