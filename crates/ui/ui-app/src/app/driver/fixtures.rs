//! What a test mounts its component with on the display-less driver: the pinned fonts, the dark
//! theme and the window size a bare mount gives the app, and the small picture a capture or a test
//! shows.

pub use crate::pictures::FIXTURE_PICTURE;

#[cfg(any(test, feature = "test-support"))]
mod runner {
    /// The window a bare mount gives the app: wide enough for a strip of strips, as the
    /// display-less runner's default is.
    pub const MOUNT_VIEWPORT: (u32, u32) = (800, 600);
}

#[cfg(any(test, feature = "test-support"))]
#[cfg(feature = "model")]
pub use crate::application::fixture_runner_inputs;
#[cfg(any(test, feature = "test-support"))]
pub use runner::MOUNT_VIEWPORT;
