//! The reusable Tilo video-streaming stack.
//!
//! Transport-agnostic pieces of the compositor's collaboration
//! surface-streaming pipeline, reused by a second consumer — the chat
//! call app's screen/camera path — instead of carrying a second codec +
//! wire format. Encoding and decoding go through the shared media stack
//! (`media`, `GStreamer`), which chooses hardware first.
//!
//! [`encoder`] (RGBA→access unit), [`decoder`] (access unit→RGBA), [`stream`]
//! (per-subscriber framing + drop-oldest backpressure + sequence-gap
//! detection, transport-decoupled), [`throttle`] (damage-aware frame-rate
//! cap), [`session`] (the throttle+encode→frame / reframe→decode seam
//! both the compositor and chat drive).

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod decoder;
pub mod encoder;
pub mod session;
pub mod stream;
pub mod throttle;

pub use decoder::{
    DecodedFrame, MAX_DECODED_HEIGHT, MAX_DECODED_PIXELS, MAX_DECODED_RGBA_BYTES,
    MAX_DECODED_WIDTH, VideoDecoder,
};
pub use encoder::{EncodeError, VideoEncoder};
pub use media::StreamCodec;
pub use media::realtime::{CodecCapabilities, local_capabilities, negotiate};
pub use session::{VideoStreamReceiver, VideoStreamSender};
pub use stream::{
    FrameError, MAX_BUFFERED_STREAM_LEN, MAX_ENCODED_FRAME_LEN, PeerId, ReceiverEvent,
    SurfaceFrame, SurfaceId, SurfaceStreamReceiver, SurfaceStreamSender,
};
pub use throttle::StreamingThrottle;
