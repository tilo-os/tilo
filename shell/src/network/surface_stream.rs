//! Per-share substream framing for encoded video frames.
//!
//! Framing + per-subscriber bounded queues + backpressure counters live
//! in the standalone [`qos_callstream`] crate, so surface streaming and
//! the chat call app share one wire format; this module re-exports the
//! types so existing callers keep compiling unchanged.
//!
//! `SurfaceStreamStart` / `SurfaceStreamEnd` stay on gossip via
//! [`crate::network::NeighbourMsg`]; only the per-frame video payload
//! rides this substream. `PeerId` / `SurfaceId` here are
//! [`crate::network::protocol`]'s `u64` aliases, identical to the re-exported crate's.

pub use qos_callstream::stream::{
    DEFAULT_QUEUE_CAP, FLAG_KEYFRAME, FRAME_HEADER_LEN, FrameError, MAX_BUFFERED_STREAM_LEN,
    MAX_ENCODED_FRAME_LEN, ReceiverEvent, SurfaceFrame, SurfaceStreamReceiver, SurfaceStreamSender,
};
