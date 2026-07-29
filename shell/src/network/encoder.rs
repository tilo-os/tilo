//! H.264 software encoder.
//!
//! The encoder now lives in the standalone [`qos_callstream`] crate so the
//! compositor's surface streaming and the chat call app share one copy of
//! the codec + wire format (see that crate's `encoder` module). This
//! module re-exports the types under their historical path
//! (`crate::network::encoder::{EncodeError, H264Encoder}`) so the
//! flatpak-share session and the streaming loop keep compiling unchanged.
//!
//! The streaming loop in [`crate::network::streaming`] owns one encoder
//! per shared surface; the caller must not queue frames behind it —
//! encode synchronously and ship the NALU bytes, or drop the frame.

pub use qos_callstream::encoder::{EncodeError, H264Encoder};
