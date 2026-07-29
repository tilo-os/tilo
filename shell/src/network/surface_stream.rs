//! Per-share substream framing for encoded video frames.
//!
//! The framing + per-subscriber bounded queues + backpressure counters
//! now live in the standalone [`qos_callstream`] crate (its `stream`
//! module) so the compositor's surface streaming and the chat call app
//! share one copy of the wire format. This module re-exports the types
//! under their historical path (`crate::network::surface_stream::*`) so
//! the flatpak-share session and the actor's substream adapter keep
//! compiling unchanged.
//!
//! `SurfaceStreamStart` / `SurfaceStreamEnd` announcements stay on
//! gossipsub via [`crate::network::NeighbourMsg`]; only the per-frame
//! video payload rides this substream. The `PeerId` / `SurfaceId` handles
//! used to key streams remain [`crate::network::protocol`]'s aliases —
//! both are `u64`, identical to the crate's, so the re-exported
//! `SurfaceStreamSender` methods accept them transparently.

pub use qos_callstream::stream::{
    DEFAULT_QUEUE_CAP, FLAG_KEYFRAME, FRAME_HEADER_LEN, FrameError, ReceiverEvent, SurfaceFrame,
    SurfaceStreamReceiver, SurfaceStreamSender,
};
