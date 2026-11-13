//! The drawing layer: the only place in the UI stack that names the renderer.
//!
//! Components and the theme (`tilo-ui`) and the compositor draw through this
//! crate. Everything else builds interface from components.

pub mod color;
pub mod color_compat;
pub mod raster;
pub mod scene;

pub use vello;
pub use vello::{kurbo, peniko};
