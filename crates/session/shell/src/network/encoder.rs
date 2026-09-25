//! Video encoder for surface streaming.
//!
//! The encoder lives in the standalone [`tilo_callstream`] crate so
//! surface streaming and the chat call app share one codec stack + wire
//! format; this module re-exports it under the network namespace.
//!
//! The streaming loop in [`crate::network::streaming`] owns one encoder
//! per shared surface; the caller must not queue frames behind it — encode
//! synchronously and ship the bytes, or drop the frame.

pub use tilo_callstream::encoder::{EncodeError, VideoEncoder};
