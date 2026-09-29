#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

//! The desktop's appearance preferences: light or dark, the accent colour,
//! and the increased-contrast and reduced-motion accessibility options.
//! [`System`] reads them from this target's own source:
//!
//! | Target | Source |
//! |---|---|
//! | Linux | the XDG Settings portal's `org.freedesktop.appearance`, followed live |
//! | macOS | the chosen accent colour and the accessibility display options, re-read on [`System::refresh`] |
//! | elsewhere | nothing: every preference is unset |
//!
//! On macOS and Windows light or dark belongs to the window system (winit's
//! `Window::theme`), which the caller merges in.

#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    path = "unsupported.rs"
)]
mod imp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Light,
    Dark,
}

/// One reading of the desktop's preferences. `None` and `false` mean the
/// desktop states no preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Appearance {
    pub scheme: Option<Scheme>,
    /// sRGB.
    pub accent: Option<[u8; 3]>,
    pub high_contrast: bool,
    pub reduce_motion: bool,
}

/// This target's appearance source. [`Default`] is one that has read
/// nothing.
#[derive(Default)]
pub struct System {
    reading: imp::Reading,
}

impl System {
    /// Starts reading. A source that reports changes itself (the portal)
    /// calls `changed` from its own thread after each one.
    pub fn follow(changed: impl Fn() + Send + 'static) -> Self {
        Self {
            reading: imp::follow(changed),
        }
    }

    /// Re-reads what the platform reports only when asked.
    pub fn refresh(&mut self) {
        #[cfg(target_os = "macos")]
        {
            self.reading = imp::read();
        }
    }

    pub fn current(&self) -> Appearance {
        imp::current(&self.reading)
    }
}
