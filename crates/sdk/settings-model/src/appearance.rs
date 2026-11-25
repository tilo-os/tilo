//! The `appearance` section's plain leaves and bounds, which the toolkit draws by and the
//! settings store's typed keys write within. The schedule is [`crate::day_schedule`]'s.

pub use crate::day_schedule::{MODE_KEY, SECTION};

/// The section of this device's own appearance, beside the person's.
pub const DEVICE_SECTION: &str = "appearance_device";

/// The bounds of the person's text scale.
pub const TEXT_SCALE_MIN: f32 = 0.5;
pub const TEXT_SCALE_MAX: f32 = 4.0;

/// The bounds of how much of a theme's translucency the person keeps.
pub const TRANSLUCENCY_MIN: f32 = 0.0;
pub const TRANSLUCENCY_MAX: f32 = 1.0;
