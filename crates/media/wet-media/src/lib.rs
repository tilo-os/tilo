//! # Wet media
//!
//! Deterministic simulations of real paint over vector marks. A painting is
//! stored as its marks (brush strokes and filled regions with a colour and a
//! medium); this crate turns a layer's marks into the pixels that paint would
//! leave. The same marks give the same pixels on every machine, so a
//! painting syncs as marks and each collaborator simulates it locally.
//!
//! * [`MediumEngine`] is one kind of paint ([`watercolour`], [`oil`]).
//! * [`Preset`] is shareable data over an engine: parameters, a suggested
//!   ground and a palette of named pigments ([`presets::builtin`]).
//! * [`LayerPainter`] keeps one layer's simulation in step with its marks,
//!   re-simulating only what changed.
//! * [`Heightfield`] is the support (paper grain, linen weave) both the
//!   simulation and the display read.
//!
//! ## Engines beyond the built-in ones
//!
//! [`MediumEngine`] and [`LayerSim`] are the whole contract: an engine
//! receives marks in cell coordinates with a seed, and returns straight-alpha
//! sRGB pixels and a [`Blend`]. A third-party engine implements the same
//! contract behind a sandbox boundary (a WebAssembly or WGSL module receiving
//! the serialised [`Mark`]s), so it can neither read the document nor reach
//! the network, and a preset naming it is plain data like any other.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod brush;
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
