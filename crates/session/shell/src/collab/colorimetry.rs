//! Colorimetry types shared by capture, encode, wire, decode and display.
//!
//! They ride inside the neighbour protocol's stream announcements, so they
//! are defined in [`neighbour_wire::colorimetry`] and re-exported here.

pub use neighbour_wire::colorimetry::*;
