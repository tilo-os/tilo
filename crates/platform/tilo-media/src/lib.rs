//! `tilo-media`: playback, decode, encode and the audio graph, with one API
//! on Linux, macOS and the web.
//!
//! Shared streaming and playback layer for media surfaces in the OS.
//!
//! ## Backends
//!
//! | | Linux | macOS | Web |
//! |---|---|---|---|
//! | [`MediaEngine`] | `GStreamer` (`decode`) | `GStreamer.framework` (`decode`) | the page's media element; frames read back as RGBA |
//! | now playing | the toolkit's MPRIS service | [`NowPlayingSession`]: `MPNowPlayingInfoCenter`, `MPRemoteCommandCenter` | Media Session |
//! | [`realtime`], [`encode`] | `GStreamer` | `GStreamer` | `Unsupported` |
//! | [`audio`] graph | `PipeWire` | `Unsupported` | `Unsupported` |
//!
//! The `decode`, `encode`, `realtime` and `pipewire` features select native
//! backends and compile to nothing on the web, so an app keeps them in its
//! default features. [`availability`] answers per feature on the running
//! target.
//!
//! ## Shape
//!
//! * [`Source`] — a codec-agnostic reference to playable media: a local file, a
//!   path on the `/var/workspace` FUSE mount (read *through* the mount so it
//!   hydrates on demand), or a remote stream URL.
//! * [`MediaEngine`] — the control surface: `open` / `play` / `pause` /
//!   `seek_ms` / `set_volume`, a monotonic playhead, `duration`, buffering %,
//!   and a [`MediaEvent`] stream drained with [`MediaEngine::poll`]. With
//!   `--features decode` it drives a `GStreamer` `playbin`→`appsink` pipeline
//!   with audio output; without it a synthesized-frame fallback is used.
//! * [`TransportState`] + [`TransportSync`] — collaborative "follow-the-host"
//!   playback: a host publishes the transport, followers project it forward and
//!   drift-correct. The CRDT wiring lives in [`CrdtTransport`] behind the trait
//!   so a local-only player carries no automerge weight.
//! * [`Frame`] / [`FrameAcceptance`] — decoded pictures in the memory chosen by
//!   the decoder. A hardware decoder's output can stay a
//!   dmabuf on the GPU ([`FramePayload::Dmabuf`]) or arrive as planar NV12/P010
//!   ([`FramePayload::Planar`]); only a consumer that says it can paint nothing
//!   else gets the picture forced into system-memory RGBA.
//! * [`HwReport`] — which decoder element the pipeline plugged and what caps
//!   the sink negotiated.
//! * [`encode`] — the same discipline for the other direction. Encoding has no
//!   autoplugger, so a pipeline that names `openh264enc` encodes on the CPU
//!   forever however capable the GPU is. [`encode::select_video`] walks a
//!   per-codec preference list (VA-API, NVENC, QSV, then `x264enc`, then the
//!   baseline) against the registry, carries each encoder's bitrate
//!   property *and unit* so a target rate means the same thing everywhere, and
//!   reports the choice as a [`Selection`] that reads as hardware only when one
//!   was observed.
//! * [`realtime`] — the low-latency path next to playback:
//!   [`RealtimeEncoder`] (one frame in, its access unit out; forced keyframes,
//!   live bitrate changes, zero-copy dmabuf import on VA-API) and
//!   [`RealtimeDecoder`] (one access unit in, its picture out), both choosing
//!   hardware first, then royalty-free software, then Cisco's system
//!   `openh264`, from the tables in [`hw::realtime`]; plus the codec
//!   negotiation peers use to agree on AV1, VP9 or H.264.
//! * [`SessionPool`] / [`EngineSession`] / [`FrameCache`] — one decode
//!   pipeline per source, shared between the preview and every thumbnail, with
//!   a bounded LRU of decoded frames so stepping and short seeks do not
//!   re-decode.
//! * [`Prefetcher`] / [`RingBuffer`] — bounded read-ahead that tracks
//!   file/workspace buffering progress.
//! * [`audio`] — the audio graph client: enumerate the PipeWire/WirePlumber
//!   node graph (devices + per-application streams), move one app's stream
//!   between output devices, adjust per-stream volume/mute, switch the default
//!   device, and diff enumerations for hotplug. Every media surface plays
//!   *through* `PipeWire`; this is where the OS routes it.
//! * [`sync_playout`] — the member side of a synchronized speaker group:
//!   frames stamped on the leader's clock play at a shared presentation time
//!   through a jitter buffer and a drift-steered resampler, mapped to the
//!   member's channel role.
//! * [`pw_stream`] — native `PipeWire` playback and capture streams: playback
//!   pulled from a [`sync_playout`] renderer at the device's presentation
//!   time, capture pinned to one chosen node that fails rather than fall back
//!   to the default source.
//! * [`calibration`] — acoustic calibration of a speaker group: a log sweep,
//!   its deconvolution into each speaker's impulse response, and the delay,
//!   level and room-EQ trims that make the members sound as one.

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
pub mod calibration;
pub mod calibration_fusion;
mod caps;
pub mod dmabuf;
pub mod encode;
pub mod engine;
pub mod event;
pub mod event_sound;
/// Registry refresh when system extensions (the codec pack) merge or unmerge.
#[cfg(any(gst_decode, gst_encode, gst_realtime))]
pub mod extensions;
pub mod frame;
pub mod handoff;
pub mod hw;
mod now_playing;
pub mod pw_stream;
pub mod realtime;
pub mod ring;
pub mod session;
pub mod source;
pub mod sync_playout;
pub mod transport;
pub mod voice;
/// The page backend: the browser's media element decodes and plays.
#[cfg(web_page)]
mod web;

/// The GStreamer decode pipeline. Only compiled with `--features decode`;
/// consumers drive it through [`MediaEngine`].
#[cfg(gst_decode)]
pub mod gst_backend;

/// Sample-to-[`Frame`] conversion shared by playback and the real-time
/// decoder.
#[cfg(any(gst_decode, gst_realtime))]
mod gst_frames;

/// `GStreamer` itself, re-exported so consumers can configure the sink returned
/// by [`MediaEngine::audio_sink`] without adding another `GStreamer` dependency.
#[cfg(any(gst_decode, gst_realtime))]
pub use gstreamer as gst;

pub use audio::{
    AudioDevice, AudioEnumerator, AudioGraph, AudioStream, GraphControl, GraphDelta,
    RealEnumerator, RealGraphControl, enumerate as enumerate_audio,
};
pub use cache::{
    FrameCache, FrameKey, SourceKey, bucket_ms_for_fps, bucket_of, read_ahead_targets,
};
pub use caps::{Availability, CAPABILITIES, Feature, availability};
pub use dmabuf::{ColorRange, Colorimetry, YuvMatrix};
#[cfg(unix)]
pub use dmabuf::{DmabufImage, DmabufPlane};
pub use encode::{
    AudioCodec, BitrateUnits, EncodeReport, EncoderKind, EncoderSpec, HwEncPolicy, Selection,
    VideoCodec,
};
pub use engine::{EngineConfig, MediaChapter, MediaEngine, MediaTrack, SeekMode, SeekWaitError};
pub use event::{MediaEvent, VideoFrame};
pub use frame::{Frame, FrameAcceptance, FramePayload, PlanarFormat, PlanarImage, PlaneData};
pub use hw::{DecoderKind, HwPolicy, HwReport, StreamCodec};
pub use now_playing::{
    NowPlaying, NowPlayingAction, NowPlayingSession, NowPlayingState, PlaybackStatus,
};
pub use realtime::{
    CodecCapabilities, DecodeOutput, DecodedPicture, DecoderConfig, EncodedUnit, EncoderConfig,
    InputFormat, InputFrame, RealtimeDecoder, RealtimeEncoder, RealtimeError, StreamColor,
};
pub use ring::{PrefetchProgress, Prefetcher, RingBuffer};
pub use session::{DEFAULT_FRAME_TIMEOUT, EngineSession, SessionPool};
pub use source::{Source, WORKSPACE_ROOT};
pub use transport::{
    Correction, CrdtTransport, DRIFT_SEEK_THRESHOLD_MS, LocalTransport, TransportState,
    TransportSync, correction,
};
