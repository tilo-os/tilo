//! Canvas's artwork engine: what an artwork is, every edit that changes it,
//! and the pixel and geometry work that answers a caller with no screen, with
//! no window of its own.
//!
//! The artwork is layers of marks, each a vector object with a stable id. A
//! plain layer draws its marks as vector art; an oil or watercolour layer
//! paints them through [`tilo_wet_media`], re-simulated from the marks on
//! every machine. Every gesture, agent and script is one of the actions here;
//! their words are the engine's own catalogue. A window, a connector or a
//! test drives them.

// The record tests write a whole artwork as one `json!` literal, deeper than the default limit.
#![recursion_limit = "256"]

pub mod actions;
pub mod adjust;
pub mod answer;
pub mod author;
pub mod background;
pub mod brushes;
pub mod chart;
pub mod content;
pub mod content_geometry;
pub mod detail;
pub mod document;
pub mod fonts;
pub mod host;
mod locales;
pub mod media;
pub mod model;
pub mod palette;
pub mod picture;
pub mod region;
pub mod render;
pub mod resource;
pub mod selection;
pub mod settle;
pub mod strokes;
pub mod subject;
pub mod svg;
pub mod view_ops;
pub mod wand;
