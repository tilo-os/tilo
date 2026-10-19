//! What is left of the toolkit's early animation primitives besides the
//! motion library (`crate::motion`, where every motion lives): `IdleFade`
//! for chrome that shows on activity and fades once quiet, the looping
//! indicators' cadence, and `Lerp` for blending values that are not motion.

pub mod idle_fade;
pub mod indicator;
pub mod lerp;

pub use idle_fade::{CHROME_FADE, IdleFade};
pub use indicator::{
    INDICATOR_STEP, indicator_elapsed, indicator_phase, indicator_step, next_indicator_step,
};
pub use lerp::Lerp;
