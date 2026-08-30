//! The reusable qOS video-streaming stack.
//!
//! Transport-agnostic pieces of the compositor's collaboration
//! surface-streaming pipeline, reused by a second consumer — the chat
//! call app's screen/camera path — instead of carrying a second H.264
//! codec + wire format.
//!
//! [`encoder`] (RGBA→Annex-B), [`decoder`] (Annex-B→RGBA), [`stream`]
//! (per-subscriber framing + drop-oldest backpressure + sequence-gap
//! detection, transport-decoupled), [`throttle`] (damage-aware frame-rate
//! cap), [`session`] (the throttle+encode→frame / reframe→decode seam
//! both the compositor and chat drive).

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

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
