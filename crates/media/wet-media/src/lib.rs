//! Wet media: deterministic simulations of real paint over vector marks. A
//! painting is stored as its marks (strokes and filled regions with a colour
//! and a medium) and this crate turns a layer's marks into the pixels that
//! paint would leave; the same marks give the same pixels on every machine, so
//! a painting syncs as marks and each collaborator simulates it locally.
//!
//! [`MediumEngine`] is one kind of paint ([`watercolour`], [`oil`]),
//! [`Preset`] is shareable data over an engine, and [`LayerPainter`] keeps one
//! layer's simulation in step with its marks. An engine outside this crate
//! implements the same contract behind a sandbox boundary, so it can neither
//! read the document nor reach the network.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod brush;
mod check;
pub mod colour;
pub mod composite;
pub mod engine;
pub mod field;
pub mod form;
mod grid;
pub mod ground;
mod mask;
pub mod oil;
pub mod painter;
pub mod presets;
pub mod watercolour;

pub use brush::StrokePoint;
pub use check::self_check;
pub use colour::{Paint, Pigment, Rgb, parse_paint};
pub use engine::{
    Blend, Canvas, Engines, Erase, Fill, LayerSim, Mark, MediumEngine, ParamSpec, Preset, Settings,
    Stroke,
};
pub use field::{ColourField, FieldKind};
pub use form::Form;
pub use ground::{Heightfield, Texture};
pub use painter::{LayerPainter, Painted, windowed};

#[cfg(test)]
mod tests;
