//! qOS shared media layer.
//!
//! One streaming + playback layer for every media surface in the OS — the local
//! video player, the video editor's preview, and the video app's watch page all
//! decode through the same engine and paint through the same `ui` widget, so a
//! codec fix or a collaborative-playback improvement lands once and everywhere.
//!
//! ## Shape
//!
//! * [`Source`] — a codec-agnostic reference to playable media: a local file, a
//!   path on the `/var/workspace` FUSE mount (read *through* the mount so it
//!   hydrates on demand), or a remote stream URL.
//! * [`MediaEngine`] — the control surface: `open` / `play` / `pause` /
//!   `seek_ms` / `set_volume`, a monotonic playhead, `duration`, buffering %,
//!   and a [`MediaEvent`] stream drained with [`MediaEngine::poll`]. With
//!   `--features decode` it drives a real `GStreamer` `playbin`→`appsink` pipeline
//!   with real audio out; without it a synthesized-frame fallback keeps
//!   hermetic builds green.
//! * [`TransportState`] + [`TransportSync`] — collaborative "follow-the-host"
//!   playback: a host publishes the transport, followers project it forward and
//!   drift-correct. The CRDT wiring lives in [`CrdtTransport`] behind the trait
//!   so a local-only player carries no automerge weight.
//! * [`Prefetcher`] / [`RingBuffer`] — bounded read-ahead that turns a
//!   file/workspace read into genuine buffering progress.
//! * [`audio`] — the audio graph client: enumerate the PipeWire/WirePlumber
//!   node graph (devices + per-application streams), move one app's stream
//!   between output devices, adjust per-stream volume/mute, switch the default
//!   device, and diff enumerations for hotplug. Every media surface plays
//!   *through* `PipeWire`; this is where the OS routes it.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod audio;
pub mod audio_runtime;
pub mod engine;
pub mod event;
pub mod notification_sound;
pub mod ring;
pub mod source;
pub mod transport;

pub use audio::{
    AudioDevice, AudioEnumerator, AudioGraph, AudioStream, GraphControl, GraphDelta,
    RealEnumerator, RealGraphControl, enumerate as enumerate_audio,
};
pub use engine::MediaEngine;
pub use event::{MediaEvent, VideoFrame};
pub use ring::{PrefetchProgress, Prefetcher, RingBuffer};
pub use source::{Source, WORKSPACE_ROOT};
pub use transport::{
    Correction, CrdtTransport, DRIFT_SEEK_THRESHOLD_MS, LocalTransport, TransportState,
    TransportSync, correction,
};

/// Wall-clock time in unix milliseconds, for stamping a [`TransportState`]'s
/// epoch. Followers on other machines project the playhead forward from this,
/// so it must be a wall clock (roughly synced across peers), not a monotonic
/// one. Saturates to 0 before the unix epoch (unreachable in practice).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
