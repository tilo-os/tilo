//! What a test mounts its component with on the display-less driver: the pinned fonts, the dark
//! theme and the window size a bare mount gives the app, and the small picture a capture or a test
//! shows.

/// A small `data:` picture, for a capture or a test that shows one: a
/// 64-by-64 image in two colours, small enough to travel in its address.
pub const FIXTURE_PICTURE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAEAAAABAAQMAAACQp+OdAAAABlBMVEXyyHpLP7ovf/CwAAAAf0lEQVR42j3RsRHAMAhDUcj+QzEKI1BScJAi+nHjdyp0NvjZdx7dBxYMaFAgQbh6XMlRuGBAgwIJwtTjSo6HLRjQoECCMPW4kuODCwY0KJAgTD2u5BjUggENCiQIU48rOQa+YECDAglCgz329WPBgAYFEgR7Zu8/FgxoUCABNS8YGz171qSmDgAAAABJRU5ErkJggg==";

#[cfg(any(test, feature = "test-support"))]
mod runner {
    #[cfg(feature = "model")]
    use crate::application::ComponentRunnerInputs;

    /// The runner inputs every application test draws with: the pinned font catalogue and the dark
    /// theme a capture is usually taken in.
    #[cfg(feature = "model")]
    #[must_use]
    pub fn fixture_runner_inputs() -> ComponentRunnerInputs {
        ComponentRunnerInputs {
            theme: crate::theme::Theme::dark(),
            text_renderer: crate::font_catalog::fixture_renderer(),
        }
    }

    /// The window a bare mount gives the app: wide enough for a strip of strips, as the
    /// display-less runner's default is.
    pub const MOUNT_VIEWPORT: (u32, u32) = (800, 600);
}

#[cfg(any(test, feature = "test-support"))]
pub use runner::MOUNT_VIEWPORT;
#[cfg(any(test, feature = "test-support"))]
#[cfg(feature = "model")]
pub use runner::fixture_runner_inputs;
