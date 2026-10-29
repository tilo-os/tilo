//! Outputs: the screens the compositor shows the desktop on.
//!
//! An output has a mode in physical pixels and a scale; its place in the
//! logical layout is its mode over its scale, which is what clients see
//! (`wl_output`, `xdg_output`) and the pointer moves in.

use std::time::Duration;

use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Logical, Point, Transform};

use crate::state::State;

/// The refresh interval assumed when a mode reports none (60 Hz).
const FALLBACK_INTERVAL: Duration = Duration::from_micros(16_667);

/// The time between frames at `refresh_mhz` millihertz; 60 Hz when the
/// mode reports no rate.
pub(crate) fn refresh_interval(refresh_mhz: i32) -> Duration {
    u64::try_from(refresh_mhz)
        .ok()
        .and_then(|mhz| 1_000_000_000_000_u64.checked_div(mhz))
        .map_or(FALLBACK_INTERVAL, Duration::from_nanos)
}

/// One output as a backend describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputConfig {
    /// The connector name (`eDP-1`, `HEADLESS-1`), unique per session.
    pub name: String,
    /// The mode, in physical pixels.
    pub size: (i32, i32),
    /// The scale clients render at (`1.0`, `1.5`, `2.0`).
    pub scale: f64,
    /// The top-left corner in the logical layout.
    pub position: (i32, i32),
    /// The refresh rate in millihertz.
    pub refresh_mhz: i32,
}

impl OutputConfig {
    /// A 60 Hz output at scale 1 at the origin.
    pub fn new(name: impl Into<String>, width: i32, height: i32) -> Self {
        Self {
            name: name.into(),
            size: (width, height),
            scale: 1.0,
            position: (0, 0),
            refresh_mhz: 60_000,
        }
    }

    /// The same output at another scale.
    #[must_use]
    pub const fn with_scale(mut self, scale: f64) -> Self {
        self.scale = scale;
        self
    }

    /// The same output at another place in the logical layout.
    #[must_use]
    pub const fn at(mut self, x: i32, y: i32) -> Self {
        self.position = (x, y);
        self
    }

    pub(crate) fn location(&self) -> Point<i32, Logical> {
        Point::from(self.position)
    }

    /// The output and its `wl_output` global.
    pub(crate) fn create(&self, display: &DisplayHandle, model: &str) -> Output {
        let output = Output::new(
            self.name.clone(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "Tilo".into(),
                model: model.into(),
            },
        );
        let mode = Mode {
            size: self.size.into(),
            refresh: self.refresh_mhz,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Fractional(self.scale)),
            Some(self.location()),
        );
        output.set_preferred(mode);
        output.create_global::<State>(display);
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_intervals_follow_the_mode_and_fall_back_to_60_hz() {
        assert_eq!(refresh_interval(60_000), Duration::from_nanos(16_666_666));
        assert_eq!(refresh_interval(120_000), Duration::from_nanos(8_333_333));
        assert_eq!(refresh_interval(0), FALLBACK_INTERVAL);
        assert_eq!(refresh_interval(-1), FALLBACK_INTERVAL);
    }
}
