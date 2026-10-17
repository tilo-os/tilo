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
//! detection, transport-decoupled) and [`throttle`] (damage-aware frame-rate
//! cap).
//!
//! Behind features, the compositor's halves of the stack: `surface_decoder`
//! (`surface-decoder`: every wire codec, RGBA or P010, with the sender's
//! colorimetry — what a peer's shared surface decodes through) and
//! `mp4_recording` (`recording`: the MP4/MOV muxer behind screen recordings).

pub mod decoder;
pub mod encoder;
#[cfg(feature = "recording")]
pub mod mp4_recording;
pub mod stream;
#[cfg(feature = "surface-decoder")]
pub mod surface_decoder;
pub mod throttle;

// Streaming consumers share decoder admission without depending on the peer protocol.
#[cfg(feature = "unit-chain")]
pub use neighbour_wire::unit_chain;

pub use decoder::{
    DecodedFrame, MAX_DECODED_HEIGHT, MAX_DECODED_PIXELS, MAX_DECODED_RGBA_BYTES,
    MAX_DECODED_WIDTH, VideoDecoder,
};
pub use encoder::{EncodeError, VideoEncoder};
pub use media::StreamCodec;
pub use media::realtime::{CodecCapabilities, is_keyframe, local_capabilities, negotiate};
pub use stream::{
    FrameError, MAX_BUFFERED_STREAM_LEN, MAX_ENCODED_FRAME_LEN, PeerId, ReceiverEvent,
    SurfaceFrame, SurfaceId, SurfaceStreamReceiver, SurfaceStreamSender,
};
pub use throttle::StreamingThrottle;
