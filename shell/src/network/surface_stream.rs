//! Per-share substream framing for encoded video frames.
//!
//! Framing + per-subscriber bounded queues + backpressure counters live
//! in the standalone [`qos_callstream`] crate, so the compositor's
//! surface streaming and the chat call app share one copy of the wire
//! format; this module re-exports the types so existing callers keep
//! compiling unchanged.
//!
//! `SurfaceStreamStart` / `SurfaceStreamEnd` announcements stay on
//! gossipsub via [`crate::network::NeighbourMsg`]; only the per-frame
//! video payload rides this substream. `PeerId` / `SurfaceId` here are
//! [`crate::network::protocol`]'s `u64` aliases, identical to the
//! re-exported crate's, so `SurfaceStreamSender` accepts them
//! transparently.

pub use qos_callstream::stream::{
    DEFAULT_QUEUE_CAP, FLAG_KEYFRAME, FRAME_HEADER_LEN, FrameError, ReceiverEvent, SurfaceFrame,
    SurfaceStreamReceiver, SurfaceStreamSender,
};
