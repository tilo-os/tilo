//! The bridged guild's voice: who is in its voice channels, and the bot
//! in one of them carrying a Tilo call (discord-bridge.md §2, "Voice").
//!
//! [`call`] decides which channel the bot is in and reports occupancy;
//! [`connection`] is the voice gateway and UDP path of one join, with
//! [`rtp`] for transport encryption, [`dave`] for Discord's end-to-end
//! encryption and [`mix`] for the Tilo speakers mixed into one stream.

pub mod call;
pub mod connection;
pub mod dave;
pub mod mix;
pub mod permissions;
pub mod rtp;

/// Frames from the host waiting for the voice connection: a second of
/// audio from a few speakers. Beyond it frames are dropped, never queued
/// into latency.
pub const AUDIO_QUEUE: usize = 128;

/// A `Call` from the host, for the gateway task.
#[derive(Debug)]
pub struct CallCommand {
    pub call: Option<String>,
    pub status: Option<String>,
}
