//! The width classes a view's container is laid out in (layout.md, "Responding to space").
//!
//! One token, read by the toolkit's layout (`ui::widget::WidthClass`) and by a
//! tilolang app's `space`: a window is compact, medium, expanded or wide by the same widths in
//! both, so what an app reads and what the shell lays out agree on when a window is compact.

/// The width, in `rem`, below which a container is compact.
pub const COMPACT_BELOW: f32 = 30.0;
/// The width, in `rem`, below which a container is medium.
pub const MEDIUM_BELOW: f32 = 52.0;
/// The width, in `rem`, below which a container is expanded.
pub const EXPANDED_BELOW: f32 = 75.0;
/// How far past a boundary a size has to go before its class changes, in `rem`: a window
/// resized right at a boundary keeps one class instead of flickering between two.
pub const HYSTERESIS: f32 = 0.5;
