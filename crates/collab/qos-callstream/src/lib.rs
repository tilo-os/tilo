//! The reusable qOS video-streaming stack.
//!
//! This crate holds the transport-agnostic pieces of the compositor's
//! collaboration surface-streaming pipeline so a second consumer — the
//! chat call app streaming a portal-selected screen/window to call
//! peers — reuses the *same* encode/transport/decode code instead of
//! carrying its own second H.264 codec + wire format.
//!
//! - [`encoder`] — [`encoder::H264Encoder`], the software openh264
//!   RGBA→Annex-B encoder. The compositor's `network::encoder` and the
//!   flatpak-share session encode through this exact type; the chat call
//!   app's camera + screen-share path now does too.
//! - [`decoder`] — [`decoder::H264Decoder`], the SDR openh264
//!   Annex-B→RGBA decoder, yielding a plain [`decoder::DecodedFrame`]
//!   (`width`, `height`, `rgba`) the receiver blits into a call tile.
//! - [`stream`] — the per-subscriber frame-transport substrate:
//!   [`stream::SurfaceFrame`] framing (sequence + keyframe flag),
//!   [`stream::SurfaceStreamSender`] with drop-oldest backpressure, and
//!   [`stream::SurfaceStreamReceiver`] with sequence-gap detection. The
//!   wire binding is decoupled — any bidirectional byte stream (the
//!   compositor's gossipsub/QUIC path, or chat's iroh-QUIC call
//!   transport) carries the framed bytes.
//! - [`throttle`] — [`throttle::StreamingThrottle`], the damage-aware
//!   frame-rate cap the encode loop threads through.
//! - [`session`] — [`session::VideoStreamSender`] /
//!   [`session::VideoStreamReceiver`], the throttle+encode→frame and
//!   reframe→decode compositions. This is the clean seam a caller drives:
//!   the compositor composes the same pieces in `SharedFlatpakSession`;
//!   chat drives these directly for its call video.

pub mod decoder;
pub mod encoder;
pub mod session;
pub mod stream;
pub mod throttle;

pub use decoder::{DecodedFrame, H264Decoder};
pub use encoder::{EncodeError, H264Encoder};
pub use session::{VideoStreamReceiver, VideoStreamSender};
pub use stream::{
    FrameError, PeerId, ReceiverEvent, SurfaceFrame, SurfaceId, SurfaceStreamReceiver,
    SurfaceStreamSender,
};
pub use throttle::StreamingThrottle;
