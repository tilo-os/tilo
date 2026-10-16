//! Authored-picture and simulation geometry, independent of window layout.
//!
//! These limits control exported image dimensions, media sampling work and
//! changes to a selection's coverage. They never size a widget, an inset, a
//! font or a pointer target. UI geometry belongs to the toolkit's Theme.

/// Maximum painted-media simulation density, in cells per artwork unit.
pub(crate) const FINEST: f64 = 8.0;
/// Maximum subject-model input image's longest side, in pixels.
pub(crate) const SIDE: f64 = 2048.0;
/// Maximum subject-edge sampling density, in image pixels per artwork unit.
pub(crate) const MOST_PIXELS_PER_UNIT: f64 = 8.0;
/// Default subject-edge fitting radius, in the sampled picture's pixels.
pub const REFINE_PIXELS: f64 = 6.0;
/// Maximum caller-supplied refinement radius, in authored artwork units.
pub(crate) const MOST_REFINE: f64 = 200.0;
/// Safety limit for the longest exported raster image side, in pixels.
pub const MAX_PIXELS: f64 = 8192.0;
/// Selection growth amount, divided by viewing zoom before modifying coverage.
pub(crate) const GROW: f64 = 4.0;
/// Selection edge-fitting radius, divided by zoom before sampling the picture.
pub(crate) const EDGE: f64 = 8.0;
/// Selection feather radius, divided by zoom before modifying coverage.
pub(crate) const FEATHER: f64 = 8.0;
/// Procedural ground sample's longest authored side. The UI thumbnail has its
/// own Theme size; this separately bounds the simulation cells used to make
/// its paper/paint content and preserves the ground's texture frequency.
pub(crate) const SWATCH_UNITS: f64 = 240.0;
