//! A Miracast (Wi-Fi Display) source.
//!
//! Wi-Fi Display mirrors a screen to a TV or adapter over Wi-Fi Direct. A
//! session has three parts, each a module here:
//!
//! - **Finding and joining the sink** ([`ie`], and [`p2p`] with the
//!   `networkmanager` feature): the sink advertises itself in Wi-Fi Direct
//!   frames with WFD information elements; the source forms a P2P group with
//!   it, announcing its own elements.
//! - **Negotiation** ([`source`], built on [`rtsp`], [`params`] and
//!   [`modes`]): over RTSP on TCP port 7236 the source learns what the sink
//!   decodes and picks a video mode and audio codec (messages M1 to M7),
//!   keeps the session alive (M16) and ends it.
//! - **Media** ([`ts`], and [`media`] with the `gstreamer` feature): H.264
//!   video and AAC or LPCM audio in an MPEG transport stream, sent as RTP over
//!   UDP.
//!
//! [`sink`] is a strict minimal sink for testing a source end to end, and
//! [`headless`] (with `gstreamer`) a headless screen to capture in such tests.
//!
//! This is an independent implementation of the Wi-Fi Display
//! specification. No code is taken from other Miracast implementations.

#[cfg(feature = "gstreamer")]
pub mod headless;
pub mod ie;
#[cfg(feature = "gstreamer")]
pub mod media;
pub mod modes;
#[cfg(feature = "networkmanager")]
pub mod p2p;
pub mod params;
pub mod rtsp;
pub mod sink;
pub mod source;
pub mod ts;

pub use source::{
    AudioPreference, Media, MediaError, MediaFactory, MediaPlan, SessionEnd, Source, SourceError,
    SourceEvent, SourceOptions, StopHandle,
};
