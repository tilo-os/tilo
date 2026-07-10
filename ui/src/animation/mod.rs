//! Shared motion system. One curve set (`Curve`), one wall-clock driver
//! (`Animated`), one interruptible open/close driver (`Transition`), and
//! one stateful velocity-carrying `Spring` for interactive drag.
//!
//! Fire-and-forget animations (fades, spawns, snaps) use `Animated` /
//! `Transition` — they are wall-clock parametric, so a dropped frame skips
//! rather than slows. Interactive gestures that need velocity continuity
//! across inputs (the workspace wheel) use `Spring`, fed the real frame dt.

pub mod animated;
pub mod spring;

pub use animated::{Animated, Curve, Lerp, Transition};
pub use spring::Spring;
