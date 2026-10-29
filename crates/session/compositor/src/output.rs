//! Outputs: the screens the compositor shows the desktop on.
//!
//! An output has a mode in physical pixels and a scale; its place in the
//! logical layout is its mode over its scale, which is what clients see
//! (`wl_output`, `xdg_output`) and the pointer moves in.

use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Logical, Point, Transform};

use crate::state::State;

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
