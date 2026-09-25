//! H.264 software encoder.
//!
//! The encoder lives in the standalone [`tilo_callstream`] crate so
//! surface streaming and the chat call app share one codec + wire
//! format; this module re-exports the types under their historical path
//! (`crate::network::encoder::{EncodeError, H264Encoder}`) so existing callers keep compiling unchanged.
//!
//! The streaming loop in [`crate::network::streaming`] owns one encoder
//! per shared surface; the caller must not queue frames behind it — encode synchronously and ship the NALU bytes, or drop the frame.

pub use tilo_callstream::encoder::{EncodeError, H264Encoder};
