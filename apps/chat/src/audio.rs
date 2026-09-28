//! Device-free legacy codec diagnostics reuse the native media codec.
//! Actual capture/playout belongs to the shell service and its current lease.
pub use media::voice::gst::{OpusDecoder, OpusEncoder, test_tone};
pub const RATE: i32 = 48_000;
pub const CHANNELS: i32 = 1;
