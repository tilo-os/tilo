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
//! * [`Frame`] / [`FrameAcceptance`] — decoded pictures in the memory the
//!   decoder actually produced them in. A hardware decoder's output can stay a
//!   dmabuf on the GPU ([`FramePayload::Dmabuf`]) or arrive as planar NV12/P010
//!   ([`FramePayload::Planar`]); only a consumer that says it can paint nothing
//!   else gets the picture forced into system-memory RGBA.
//! * [`HwReport`] — which decoder element the pipeline actually plugged and
//!   what caps the sink negotiated. An observation of what ran, so "hardware
//!   decode is on" is a checkable fact rather than a configuration claim.
//! * [`encode`] — the same discipline for the other direction. Encoding has no
//!   autoplugger, so a pipeline that names `openh264enc` encodes on the CPU
//!   forever however capable the GPU is. [`encode::select_video`] walks a
//!   per-codec preference list (VA-API, NVENC, QSV, then `x264enc`, then the
//!   baseline) against the real registry, carries each encoder's bitrate
//!   property *and unit* so a target rate means the same thing everywhere, and
//!   reports the choice as a [`Selection`] that reads as hardware only when one
//!   was actually observed.
//! * [`SessionPool`] / [`EngineSession`] / [`FrameCache`] — one decode
//!   pipeline per source, shared between the preview and every thumbnail, with
//!   a bounded LRU of decoded frames so stepping and short seeks do not
//!   re-decode.
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
pub mod cache;
pub mod dmabuf;
pub mod encode;
pub mod engine;
pub mod event;
pub mod frame;
pub mod hw;
pub mod notification_sound;
pub mod ring;
pub mod session;
pub mod source;
pub mod transport;

/// The real GStreamer decode pipeline. Only compiled with `--features decode`;
/// consumers drive it through [`MediaEngine`] rather than directly.
#[cfg(feature = "decode")]
pub mod gst_backend;

/// GStreamer itself, re-exported so a consumer can use what
/// [`MediaEngine::audio_sink`] hands back — setting a target device, attaching
/// a level meter, tapping a tee into a mixer — without adding its own
/// gstreamer dependency and risking a second, incompatible copy in the graph.
#[cfg(feature = "decode")]
pub use gstreamer as gst;

pub use audio::{
    AudioDevice, AudioEnumerator, AudioGraph, AudioStream, GraphControl, GraphDelta,
    RealEnumerator, RealGraphControl, enumerate as enumerate_audio,
};
pub use cache::{
    FrameCache, FrameKey, SourceKey, bucket_ms_for_fps, bucket_of, read_ahead_targets,
};
pub use dmabuf::{ColorRange, Colorimetry, DmabufImage, DmabufPlane, YuvMatrix};
pub use encode::{
    AudioCodec, BitrateUnits, EncodeReport, EncoderKind, EncoderSpec, HwEncPolicy, Selection,
    VideoCodec,
};
pub use engine::{EngineConfig, MediaEngine, SeekMode};
pub use event::{MediaEvent, VideoFrame};
pub use frame::{Frame, FrameAcceptance, FramePayload, PlanarFormat, PlanarImage, PlaneData};
pub use hw::{DecoderKind, HwPolicy, HwReport};
pub use ring::{PrefetchProgress, Prefetcher, RingBuffer};
pub use session::{DEFAULT_FRAME_TIMEOUT, EngineSession, SessionPool};
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
