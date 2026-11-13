//! `tilo-media`: playback, decode, encode and the audio graph, with one API
//! on Linux, macOS and the web.
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
//! default features; [`availability`] answers per feature on the running target.
//!
//! Frames reach the consumer in the memory the decoder produced them in
//! ([`FramePayload`]); only a consumer that says it can paint nothing else gets
//! system-memory RGBA.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod audio;
pub mod audio_output;
pub mod audio_runtime;
pub mod audio_watch;
pub mod cache;
pub mod calibration;
pub mod calibration_fusion;
mod caps;
pub mod dmabuf;
pub mod edit;
pub mod encode;
pub mod engine;
pub mod event;
pub mod event_sound;
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
pub mod video_surface;
pub mod voice;
#[cfg(web_page)]
mod web;
#[cfg(web_page)]
mod web_edit;

/// The `GStreamer` decode pipeline; only with `--features decode`. Consumers drive
/// it through [`MediaEngine`].
#[cfg(gst_decode)]
pub mod gst_backend;

#[cfg(gst_decode)]
pub mod pcm;

#[cfg(any(gst_decode, gst_realtime))]
mod gst_frames;

/// `GStreamer`, re-exported so a consumer can configure the sink from
/// [`MediaEngine::audio_sink`] without another `GStreamer` dependency.
#[cfg(any(gst_decode, gst_realtime))]
pub use gstreamer as gst;

pub use audio::{
    AudioDevice, AudioEnumerator, AudioGraph, AudioStream, GraphControl, GraphDelta,
    RealEnumerator, RealGraphControl, enumerate as enumerate_audio,
};
pub use cache::{
    FrameCache, FrameKey, SourceKey, bucket_ms_for_fps, bucket_of, read_ahead_targets,
};
pub use caps::{Availability, Feature, availability, available};
pub use dmabuf::{ColorRange, Colorimetry, YuvMatrix};
#[cfg(unix)]
pub use dmabuf::{DmabufImage, DmabufPlane};
pub use encode::{
    AudioCodec, BitrateUnits, EncodeReport, EncoderKind, EncoderSpec, HwEncPolicy, Selection,
    VideoCodec,
};
pub use engine::{EngineConfig, MediaChapter, MediaEngine, MediaTrack, SeekMode, SeekWaitError};
pub use event::{ElementPicture, MediaEvent, VideoFrame};
pub use frame::{Frame, FrameAcceptance, FramePayload, PlanarFormat, PlanarImage, PlaneData};
pub use hw::{DecoderKind, HwPolicy, HwReport, StreamCodec};
pub use now_playing::{
    NowPlaying, NowPlayingAction, NowPlayingSession, NowPlayingState, PlaybackStatus,
};
pub use realtime::{
    CodecCapabilities, DecodeOutput, DecodedPicture, DecoderConfig, EncodedUnit, EncoderConfig,
    InputFormat, InputFrame, PictureWake, RealtimeDecoder, RealtimeEncoder, RealtimeError,
    StreamColor,
};
pub use ring::{PrefetchProgress, Prefetcher, RingBuffer};
pub use session::{DEFAULT_FRAME_TIMEOUT, EngineSession, SessionPool};
pub use source::{Source, WORKSPACE_ROOT};
pub use transport::{Correction, DRIFT_SEEK_THRESHOLD_MS, TransportState, correction};
