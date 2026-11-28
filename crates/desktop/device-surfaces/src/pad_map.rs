//! A drawing tablet's pad map as the device publishes it: what each of the pad's buttons, rings and
//! strips does now, while the person holds the button that shows it.

use serde::{Deserialize, Serialize};
use tilo_resource::NoEffect;

/// The map of one pad: its name, its controls and what each does, arranged around a drawing of
/// the pad when its model's arrangement is known.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct PadMap {
    /// Whether the map stands now: while the person holds the pad's map button or one of its mode
    /// buttons. False draws nothing.
    pub visible: bool,
    /// The pad's name, as the device reports it.
    pub name: String,
    /// What the map says of itself: that it stands while the button is held.
    pub hint: String,
    /// Whether the controls sit around a drawing of the pad, each at its own edge: the model's
    /// arrangement is known. Otherwise they are listed.
    pub drawn: bool,
    /// Every control, buttons first, then rings, then strips: what the map lists when the model's
    /// arrangement is not known.
    pub controls: Vec<PadMapControl>,
    /// The controls along the pad's top edge, left to right, when [`Self::drawn`].
    pub top: Vec<PadMapControl>,
    /// Those along its left edge, top to bottom.
    pub left: Vec<PadMapControl>,
    /// Those along its right edge, top to bottom.
    pub right: Vec<PadMapControl>,
    /// Those along its bottom edge, left to right.
    pub bottom: Vec<PadMapControl>,
    /// The whole map, one control and what it does per line, as a screen reader says it.
    pub spoken: String,
}

/// One control of a pad and what it does now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, tilo_resource::JsonSchema)]
pub struct PadMapControl {
    /// What the control is called: `Button 1`, `Ring 1`, `Strip 1`.
    pub name: String,
    /// What its keycap reads: a button's number, a ring's or strip's name.
    pub cap: String,
    /// What it does now: the focused app's own word for it, or Tilo's mapping.
    pub does: String,
    /// Whether it does something worth reading first: not nothing, and not left to the app.
    pub active: bool,
}

/// The pad map as a view of another language reads it (`device.pad_map`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<PadMap, NoEffect>("pad_map", "PadMap")
}
