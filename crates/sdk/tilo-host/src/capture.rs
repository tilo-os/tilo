//! Screen capture, as the host provides it.
//!
//! An app asks [`crate::host::HostServices::screen_capture`] for a stream and
//! drains the returned channel once per frame; the UI thread never blocks
//! while the host shows its source picker. The host mediates the permission
//! prompt and never invents a stream: without a grant the answer is
//! [`ScreenCaptureOutcome::Cancelled`] or [`ScreenCaptureOutcome::Unavailable`].

use std::sync::mpsc::Receiver;

/// What the app would like the user to choose from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenCaptureRequest {
    /// Offer whole monitors.
    pub monitors: bool,
    /// Offer individual windows.
    pub windows: bool,
    /// Draw the pointer into the captured frames.
    pub embed_cursor: bool,
}

impl Default for ScreenCaptureRequest {
    fn default() -> Self {
        Self {
            monitors: true,
            windows: true,
            embed_cursor: true,
        }
    }
}

/// Where the granted frames can be read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CaptureStream {
    /// A `PipeWire` node to open (for example with `pipewiresrc`).
    PipeWire { node_id: u32 },
}

/// A stream the user granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenCaptureGrant {
    pub stream: CaptureStream,
    /// The source's size in pixels, when the host reports it.
    pub size: Option<(u32, u32)>,
}

/// The result of a screen-capture request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScreenCaptureOutcome {
    Granted(ScreenCaptureGrant),
    /// The user dismissed the picker.
    Cancelled,
    /// The host cannot capture the screen, or the request failed. Carries a
    /// human-readable reason.
    Unavailable(String),
}

/// A pending screen-capture request: the receiver the UI thread drains each
/// frame.
pub type ScreenCaptureChannel = Receiver<ScreenCaptureOutcome>;
