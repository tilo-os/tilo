//! Document and picture content: the one way to draw what a person made or
//! captured (a page, a slide, a sheet's grid, a map, a terminal's cells, a
//! video frame) rather than interface.
//!
//! A [`DocumentCanvas`] is a component that hosts a [`CanvasContent`]. The
//! content paints through a [`CanvasPainter`], which offers shapes,
//! pictures, document text, clips and layers, and takes its colours as
//! [`Ink`]: a colour the document holds ([`DocColor`]) or a role the theme
//! resolves ([`CanvasRole`]). It has no controls, rows, buttons or theme
//! surfaces; the chrome around a canvas is built from components.

mod canvas;
mod ink;
mod painter;
mod picture;

pub use canvas::{CanvasContent, CanvasDamage, DocumentCanvas};
pub use ink::{CanvasRole, DocColor, Gradient, GradientShape, Ink, Paint};
pub use painter::{Blend, CanvasPainter, Layer};
pub use picture::{Picture, PictureQuality};

/// Geometry a canvas lays its content out in.
pub mod geom {
    pub use tilo_ui_paint::kurbo::{
        Affine, BezPath, Cap, Circle, Ellipse, Join, Line, Point, Rect, RoundedRect,
        RoundedRectRadii, Shape, Size, Stroke, Vec2,
    };
}
