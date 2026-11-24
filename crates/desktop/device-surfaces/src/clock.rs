//! The device's clock, as a view that has to know whether its time can be trusted reads it.

use serde::{Deserialize, Serialize};
use tilo_resource::NoEffect;

/// Whether the device's clock is synchronised to a time source.
///
/// A code made from a seed and the clock, and a comparison of an answer with the time it was
/// sent at, are only as good as the clock. `None` is a device whose platform cannot say, which
/// is no reason for a warning: one nobody can act on is noise.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Clock {
    /// Whether it is synchronised; `None` where this platform cannot say.
    pub synchronised: Option<bool>,
}

/// The clock as a view of another language reads it (`device.clock`).
#[must_use]
pub(crate) fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Clock, NoEffect>("clock", "Clock")
}
