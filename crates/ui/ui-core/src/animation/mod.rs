//! Animation that is not motion (`crate::motion`): `IdleFade` for chrome that shows on activity and
//! fades once quiet, the looping indicators' cadence, and `Lerp` for blending values.

pub mod idle_fade;
pub mod indicator;
pub mod lerp;

pub use idle_fade::{CHROME_FADE, IdleFade};
pub use indicator::{
    INDICATOR_STEP, indicator_elapsed, indicator_phase, indicator_step, next_indicator_step,
};
pub use lerp::Lerp;
