//! Shared motion system. One curve set (`Curve`), one wall-clock driver
//! (`Animated`), one interruptible open/close driver (`Transition`), and
//! one stateful velocity-carrying `Spring` for interactive drag, and one
//! `IdleFade` for chrome that shows on activity and fades once it is quiet.
//!
//! Fire-and-forget animations (fades, spawns, snaps) use `Animated` /
//! `Transition` — they are wall-clock parametric, so a dropped frame skips
//! rather than slows. Interactive gestures that need velocity continuity
//! across inputs (the workspace wheel) use `Spring`, fed the real frame dt.

pub mod animated;
pub mod idle_fade;
pub mod indicator;
pub mod spring;

pub use animated::{Animated, Curve, Lerp, Transition, ease_out};
pub use idle_fade::{CHROME_FADE, IdleFade};
pub use indicator::{INDICATOR_STEP, indicator_elapsed, indicator_step, next_indicator_step};
pub use spring::Spring;
