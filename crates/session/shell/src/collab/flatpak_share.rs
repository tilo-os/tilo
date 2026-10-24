//! `SharedFlatpakSession` composes surface capture, encode throttling, a
//! video encoder, and the `NeighbourMsg` wire envelope into one live app
//! share. Transport, cursor overlay, sandbox brokering, and input routing live
//! elsewhere — this module only produces encoded frames and
//! stream-start/stream-end envelopes.
//!
//! The codec is negotiated with the viewers ([`media::realtime::negotiate`]):
//! AV1 or VP9 when this machine encodes it in hardware and every viewer
//! decodes it in hardware, H.264 otherwise (always for a viewer that never
//! advertised capabilities); HDR uses a hardware 10-bit encoder (AV1 or HEVC
//! Main10) every viewer can decode, else the share runs in SDR.
//! [`SharedFlatpakSession::renegotiate`] switches codec when the viewer set
//! changes, and the caller then announces the new stream.

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use crate::collab::colorimetry::Colorimetry;
use crate::collab::encoder::nv12::Region;
use crate::collab::encoder::{
    Codec, Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, build,
};
use crate::collab::hdr_metadata::{Hdr10Metadata, HdrConfig};
use crate::config::{EncoderChoice, FlatpakShareConfig};
use crate::diagnostics::{CapturePath, PeerSurfaceMetrics};
use crate::network::encoder::EncodeError;
use crate::network::protocol::{NeighbourMsg, PeerId, SurfaceId};
use crate::network::streaming::{InputEvent, ShareRegistry, StreamingShare, StreamingThrottle};
use crate::network::surface_stream::SurfaceStreamSender;
use media::realtime::{CodecCapabilities, RealtimePolicy, negotiate, negotiate_hdr};
#[cfg(target_os = "linux")]
use smithay::backend::allocator::Buffer as _;
#[cfg(target_os = "linux")]
use smithay::backend::allocator::dmabuf::Dmabuf;
use tilo_lock::MutexRecover as _;

/// The least time between two keyframes forced at viewers' requests. Every
/// viewer of a share decodes the same stream, so one keyframe answers all of
/// them; this bounds what viewers asking repeatedly (or a hostile one) cost
/// the encoder and the link.
pub const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(250);

/// The time between two frames at `hz`, rounded up so a second never holds
/// one more.
fn frame_interval(hz: u32) -> Duration {
    Duration::from_micros(1_000_000_u64.div_ceil(u64::from(hz.max(1))))
}

/// Frame waiting in the encoder's staging slot: source bytes/handle paired
/// with capture time so the wire preserves moment-of-capture, not
/// encode-finish time. Drop-oldest: a second deposit overwrites both fields.
struct StagedFrame {
    kind: StagedKind,
    capture_ts_ns: u64,
}

/// Either flavour of staged capture. An enum, not two `Option`s, because a
/// single tick can only encode one and storing both would invite ambiguity
/// about which wins.
enum StagedKind {
    /// Row-major RGBA bytes, the whole frame.
    Rgba(Vec<u8>),
    /// The window as the frame store holds it when the frame is encoded.
    Window,
    /// DMABUF handle handed to the encoder; Arc-backed inside smithay, so
    /// staging it keeps every plane fd alive without a dup.
    #[cfg(target_os = "linux")]
    Dmabuf(Dmabuf),
}

/// Build the encoder's `FrameInput::DmabufFd` from `dmabuf`'s planes.
/// `.take(4)` bounds the plane count, but writes go through `.get_mut()`
/// anyway rather than trust that invariant to hold forever. Split out of
/// `SharedFlatpakSession::encode_next_frame` to keep it short.
#[cfg(target_os = "linux")]
fn dmabuf_frame_input(dmabuf: &Dmabuf) -> FrameInput<'_> {
    let mut plane_fds = [-1i32; 4];
    let mut plane_offsets = [0u64; 4];
    let mut plane_strides = [0u32; 4];
    let mut num_planes: u8 = 0;
    for (i, (fd, (offset, stride))) in dmabuf
        .handles()
        .zip(dmabuf.offsets().zip(dmabuf.strides()))
        .take(4)
        .enumerate()
    {
        if let Some(slot) = plane_fds.get_mut(i) {
            *slot = fd.as_raw_fd();
        }
        if let Some(slot) = plane_offsets.get_mut(i) {
            *slot = u64::from(offset);
        }
        if let Some(slot) = plane_strides.get_mut(i) {
            *slot = stride;
        }
        num_planes = num_planes.saturating_add(1);
    }
    let fmt = dmabuf.format();
    FrameInput::DmabufFd {
        plane_fds,
        plane_offsets,
        plane_strides,
        num_planes,
        drm_fourcc: fmt.code as u32,
        drm_modifier: u64::from(fmt.modifier),
        width: dmabuf.width(),
        height: dmabuf.height(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncodedFrame {
    pub surface: SurfaceId,
    pub source_peer: PeerId,
    pub nalus: Vec<u8>,
    /// Owner-side capture timestamp (monotonic ns). Receivers subtract their
    /// translated local clock to compute motion-to-photon latency.
    pub capture_ts_ns: u64,
    /// Place in the share's encoded sequence; see `NeighbourMsg::SurfaceFrame::seq`.
    pub seq: u64,
}

impl EncodedFrame {
    pub fn into_neighbour_msg(self) -> NeighbourMsg {
        NeighbourMsg::SurfaceFrame {
            surface: self.surface,
            source_peer: self.source_peer,
            encoded_h264: self.nalus,
            ts: self.capture_ts_ns,
            seq: self.seq,
        }
    }
}

/// `SurfaceStreamStart::codec` of a page shared as a display list.
pub const PAINT_CODEC: &str = ui::webview::paint::stream::wire::CODEC;

/// What [`SharedFlatpakSession::assemble`] builds a session from.
struct Started {
    host_user_id: PeerId,
    app_id: String,
    surface_id: SurfaceId,
    width: u32,
    height: u32,
    local_caps: CodecCapabilities,
    metrics: Option<PeerSurfaceMetrics>,
    paint: bool,
}

/// The encoder of a paint share: its frames come from the app already
/// encoded, so it is never handed pixels and refuses any it is given.
struct PaintPassthrough {
    width: u32,
    height: u32,
}

impl Encoder for PaintPassthrough {
    fn kind(&self) -> EncoderKind {
        EncoderKind::RoyaltyFreeSoftware
    }
    fn codec(&self) -> Codec {
        Codec::H264
    }
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn accepts_dmabuf(&self, _fourcc: u32, _modifier: u64) -> bool {
        false
    }
    fn force_keyframe(&mut self) {}
    fn encode(
        &mut self,
        _input: FrameInput<'_>,
        _out: &mut Vec<u8>,
    ) -> Result<crate::collab::encoder::FrameStats, EncoderError> {
        Err(EncoderError::EncodeFailed {
            kind: self.kind(),
            detail: "paint_passthrough".into(),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("encoder {choice:?} is not available in this build")]
    EncoderUnavailable { choice: EncoderChoice },
    /// Chosen encoder failed to construct — bad dimensions, unsupported
    /// profile, or a device disappeared between probe and open.
    #[error("encoder construction failed: {0:?}")]
    EncoderConstruction(EncodeError),
    #[error("dimensions {width}×{height} exceed the 4096×4096 ceiling")]
    DimensionsExceeded { width: u32, height: u32 },
}

type ColorOverride = Option<(Colorimetry, Option<Hdr10Metadata>)>;

/// The codec a session is running and what chose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodecState {
    kind: EncoderKind,
    codec: Codec,
    /// HDR config in effect: `None` for an SDR stream, including when HDR was
    /// configured but no hardware 10-bit encoder every viewer can decode
    /// exists (the wire must match the bytes).
    hdr: Option<HdrConfig>,
    /// The size frames are encoded at: the window's, or smaller where a
    /// receiver's hardware decoder stops short of it.
    encoded: (u32, u32),
}

/// The size to encode a `source`-sized window at in `codec` for `receivers`:
/// at most what every receiver that decodes `codec` in hardware takes, so
/// none falls back to software decoding. With mixed receivers this is the
/// lowest common limit: one encode serves all, where an encode per limit
/// would multiply the host's encoder load. Aspect is kept; sides are even;
/// a window is never scaled up.
pub fn encode_size(
    codec: Codec,
    source: (u32, u32),
    receivers: &[CodecCapabilities],
) -> (u32, u32) {
    let even = |value: u32| (value & !1).max(2);
    // `numerator * scale / denominator` in integers, saturating.
    let scaled = |numerator: u32, scale: u32, denominator: u32| {
        u64::from(numerator)
            .saturating_mul(u64::from(scale))
            .checked_div(u64::from(denominator.max(1)))
            .map_or(u32::MAX, |value| u32::try_from(value).unwrap_or(u32::MAX))
    };
    let fitted = receivers
        .iter()
        .filter(|receiver| receiver.hardware_decode.contains(&codec))
        .filter_map(|receiver| receiver.hardware_decode_max.get(&codec).copied())
        .map(|(max_w, max_h)| {
            if source.0 <= max_w && source.1 <= max_h {
                return source;
            }
            // Fit inside the limit at the window's aspect.
            let by_width = scaled(source.1, max_w, source.0);
            if by_width <= max_h {
                (max_w, by_width)
            } else {
                (scaled(source.0, max_h, source.1), max_h)
            }
        })
        .min_by_key(|(w, h)| u64::from(*w).saturating_mul(u64::from(*h)))
        .unwrap_or(source);
    (even(fitted.0), even(fitted.1))
}

/// This machine's capabilities as the configured preference allows using
/// them: a software-only preference cannot encode in hardware.
fn usable_local(policy: RealtimePolicy) -> CodecCapabilities {
    let mut caps = media::realtime::local_capabilities();
    if policy == RealtimePolicy::SoftwareOnly {
        caps.hardware_encode.clear();
    }
    caps
}

/// The codec (and HDR config, if HDR is on) for these viewers.
fn choose(
    config: &FlatpakShareConfig,
    local: &CodecCapabilities,
    receivers: &[CodecCapabilities],
) -> Option<(Codec, Option<HdrConfig>)> {
    if let Some(hdr) = config.hdr
        && let Some(n) = negotiate_hdr(local, receivers)
    {
        return Some((n.codec, Some(hdr)));
    }
    negotiate(local, receivers).map(|n| (n.codec, None))
}

/// A live share session on the owner side. Drop it to release the encoder and registry entries.
pub struct SharedFlatpakSession {
    pub host_user_id: PeerId,
    pub app_id: String,
    pub surface_id: SurfaceId,
    width: u32,
    height: u32,
    /// The app streams the window's page as a display list; see
    /// [`Self::start_paint`].
    paint: bool,
    config: FlatpakShareConfig,
    encoder: Arc<Mutex<Box<dyn Encoder>>>,
    /// What the running encoder is, and why; replaced by [`Self::renegotiate`].
    codec_state: Arc<Mutex<CodecState>>,
    /// This machine's capabilities, read once at start.
    local_caps: CodecCapabilities,
    metrics: Option<PeerSurfaceMetrics>,
    capture_path: Arc<Mutex<CapturePath>>,
    throttle: Arc<Mutex<StreamingThrottle>>,
    /// Frames a second a changing window is streamed at.
    rate_hz: AtomicU32,
    /// Owner-side gate for inbound `NeighbourMsg::RemoteInput`; shared with
    /// the compositor's `State` via `Arc` so input routing sees the same
    /// permissions.
    share_registry: Arc<Mutex<ShareRegistry>>,
    /// Frame source: deposit fns write here from the compositor's commit
    /// hook, `encode_next_frame` drains via `Option::take` (drop-oldest).
    /// The capture timestamp is stamped at deposit time, not encode time,
    /// so the receiver's latency measurement includes encode cost.
    staged_frame: Arc<Mutex<Option<StagedFrame>>>,
    /// Rung when a frame is staged or the session ends, for the thread that
    /// encodes in the background ([`Self::encode_in_background`]).
    staged_ready: Arc<Condvar>,
    /// Frames that thread encoded, waiting for [`Self::pump`] to take them.
    encoded: Mutex<Vec<EncodedFrame>>,
    /// Whether a thread of its own encodes this window.
    in_background: AtomicBool,
    /// The next frame encoded is a keyframe. A flag, not a call on the
    /// encoder, so asking never waits for an encode in progress.
    keyframe_wanted: AtomicBool,
    /// The newest of the window's frames the frame store was seen holding.
    window_noted: AtomicU64,
    /// The window's frame last handed to the encoder, when the frame before
    /// the next one came from the frame store.
    window_taken: Mutex<Option<u64>>,
    /// Which dmabuf layouts the encoder imports, as it answered.
    dmabuf_layouts: Mutex<std::collections::HashMap<(u32, u64), bool>>,
    origin: Instant,
    active: Arc<AtomicBool>,
    /// Per-share video substream sender — every `encode_next_frame` deposits
    /// here in addition to returning the frame; the transport adapter polls
    /// per subscriber and writes bytes on the wire. Per-subscriber bounded
    /// queue with drop-oldest backpressure; a keyframe flag in the header
    /// lets late-joining viewers know when it's safe to start decoding.
    stream_sender: Arc<SurfaceStreamSender>,
    /// Sequence of a paint share's frames, which bypass the encoder.
    paint_seq: AtomicU64,
    /// `seq` of the newest keyframe encoded, plus one; `0` before the first.
    keyframe_after: AtomicU64,
    /// When a viewer's request last forced a keyframe.
    keyframe_requested: Mutex<Option<Instant>>,
    /// The newest unit while it is a keyframe. A reader starting while the
    /// window has not changed since starts at it, so subscribing costs no
    /// encode however often it happens.
    standing_keyframe: Mutex<Option<EncodedFrame>>,
    color_override: Arc<Mutex<ColorOverride>>,
    /// The workspace tab holding the window, announced with the stream so a
    /// viewer shows it in exactly that tab; `0` names none.
    tab: u64,
}

impl SharedFlatpakSession {
    /// Construct + activate a new share session. `width`/`height` must match
    /// the captured surface's `wl_shm` buffer or the encoder rejects the
    /// first frame with `EncodeError::BufferSizeMismatch`.
    ///
    /// # Errors
    ///
    /// Returns [`ShareError::DimensionsExceeded`] if `width`/`height` are
    /// zero or exceed the 4096×4096 ceiling, [`ShareError::EncoderUnavailable`]
    /// if the configured encoder isn't available in this build, or
    /// [`ShareError::EncoderConstruction`] if building the chosen encoder
    /// backend fails.
    pub fn start(
        host_user_id: PeerId,
        app_id: impl Into<String>,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        config: &FlatpakShareConfig,
    ) -> Result<Self, ShareError> {
        Self::start_with_metrics(
            host_user_id,
            app_id,
            surface_id,
            width,
            height,
            config,
            &[],
            None,
        )
    }

    /// Same as [`Self::start`], negotiating the codec with `receivers` (the
    /// viewers' advertised capabilities; see
    /// `crate::network::media_caps::capabilities`) and attaching a shared
    /// [`PeerSurfaceMetrics`] handle so the OSD sees encode-side counters.
    ///
    /// # Errors
    ///
    /// Same failure modes as [`Self::start`], plus
    /// [`ShareError::EncoderUnavailable`] when this machine can produce no
    /// codec every receiver decodes.
    pub fn start_with_metrics(
        host_user_id: PeerId,
        app_id: impl Into<String>,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        config: &FlatpakShareConfig,
        receivers: &[CodecCapabilities],
        metrics: Option<PeerSurfaceMetrics>,
    ) -> Result<Self, ShareError> {
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(ShareError::DimensionsExceeded { width, height });
        }
        let local_caps = usable_local(config.encoder.policy());
        let (encoder, state) = Self::open_encoder(config, &local_caps, receivers, width, height)?;
        if let Some(m) = &metrics {
            m.set_chosen_encoder(state.kind);
        }
        Ok(Self::assemble(
            Started {
                host_user_id,
                app_id: app_id.into(),
                surface_id,
                width,
                height,
                local_caps,
                metrics,
                paint: false,
            },
            config,
            encoder,
            state,
        ))
    }

    /// A share of a window whose app streams its page as a display list
    /// ([`PAINT_CODEC`]): the app hands over frames ([`Self::paint_message`]),
    /// so nothing is captured or encoded and viewers draw the page themselves.
    ///
    /// # Errors
    ///
    /// [`ShareError::DimensionsExceeded`] for a window outside 1..=4096.
    pub fn start_paint(
        host_user_id: PeerId,
        app_id: impl Into<String>,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        config: &FlatpakShareConfig,
    ) -> Result<Self, ShareError> {
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(ShareError::DimensionsExceeded { width, height });
        }
        let encoder = PaintPassthrough { width, height };
        let state = CodecState {
            kind: encoder.kind(),
            codec: encoder.codec(),
            hdr: None,
            encoded: (width, height),
        };
        Ok(Self::assemble(
            Started {
                host_user_id,
                app_id: app_id.into(),
                surface_id,
                width,
                height,
                local_caps: CodecCapabilities::default(),
                metrics: None,
                paint: true,
            },
            config,
            Box::new(encoder),
            state,
        ))
    }

    fn assemble(
        started: Started,
        config: &FlatpakShareConfig,
        encoder: Box<dyn Encoder>,
        state: CodecState,
    ) -> Self {
        let Started {
            host_user_id,
            app_id,
            surface_id,
            width,
            height,
            local_caps,
            metrics,
            paint,
        } = started;
        let effective_fps = config.target_fps.max(1);
        let stream_sender = Arc::new(SurfaceStreamSender::new(surface_id, host_user_id));
        stream_sender.set_format(
            state.codec,
            display_size(state.codec, state.encoded.0, state.encoded.1),
        );

        Self {
            host_user_id,
            app_id,
            surface_id,
            width,
            height,
            paint,
            config: FlatpakShareConfig {
                max_bitrate_kbps: config.max_bitrate_kbps,
                target_fps: effective_fps,
                encoder: config.encoder,
                hdr: config.hdr,
            },
            encoder: Arc::new(Mutex::new(encoder)),
            codec_state: Arc::new(Mutex::new(state)),
            local_caps,
            metrics,
            capture_path: Arc::new(Mutex::new(CapturePath::ShmReadback)),
            throttle: Arc::new(Mutex::new(StreamingThrottle::with_interval(
                frame_interval(effective_fps),
            ))),
            rate_hz: AtomicU32::new(effective_fps),
            share_registry: Arc::new(Mutex::new(ShareRegistry::new())),
            staged_frame: Arc::new(Mutex::new(None)),
            staged_ready: Arc::new(Condvar::new()),
            encoded: Mutex::new(Vec::new()),
            in_background: AtomicBool::new(false),
            keyframe_wanted: AtomicBool::new(false),
            window_noted: AtomicU64::new(0),
            window_taken: Mutex::new(None),
            dmabuf_layouts: Mutex::new(std::collections::HashMap::new()),
            origin: Instant::now(),
            active: Arc::new(AtomicBool::new(true)),
            stream_sender,
            paint_seq: AtomicU64::new(0),
            keyframe_after: AtomicU64::new(0),
            keyframe_requested: Mutex::new(None),
            standing_keyframe: Mutex::new(None),
            color_override: Arc::new(Mutex::new(None)),
            tab: 0,
        }
    }

    /// Negotiate a codec for `receivers` and open its encoder. An HDR
    /// request without a usable 10-bit encoder degrades to SDR rather than
    /// failing the share.
    fn open_encoder(
        config: &FlatpakShareConfig,
        local: &CodecCapabilities,
        receivers: &[CodecCapabilities],
        width: u32,
        height: u32,
    ) -> Result<(Box<dyn Encoder>, CodecState), ShareError> {
        let (codec, hdr) =
            choose(config, local, receivers).ok_or(ShareError::EncoderUnavailable {
                choice: config.encoder,
            })?;
        // HDR needs a higher bitrate floor: BT.2020-PQ at 720p needs ~10
        // Mbps for reasonable quality vs ~4 Mbps for SDR.
        let bitrate_kbps = if hdr.is_some() {
            config.max_bitrate_kbps.max(4000)
        } else {
            config.max_bitrate_kbps.max(500)
        };
        let fps = config.target_fps.max(1);
        let output = encode_size(codec, (width, height), receivers);
        let mut init = match hdr {
            None => EncoderInit::sdr(codec, width, height, fps, bitrate_kbps, 1),
            Some(hdr_cfg) => {
                EncoderInit::hdr10(codec, width, height, fps, bitrate_kbps, 1, hdr_cfg)
            }
        };
        if output != (width & !1, height & !1) {
            init.output = Some(output);
        }
        let encoder = build(init, config.encoder).map_err(|e| match e {
            EncoderError::Unavailable { .. } => ShareError::EncoderUnavailable {
                choice: config.encoder,
            },
            EncoderError::ConfigFailed { detail, .. }
            | EncoderError::EncodeFailed { detail, .. } => {
                ShareError::EncoderConstruction(EncodeError::Backend(detail))
            }
        })?;
        let state = CodecState {
            kind: encoder.kind(),
            codec,
            hdr,
            encoded: output,
        };
        tracing::info!(
            codec = codec.wire_label(),
            encoder = state.kind.as_str(),
            hdr = hdr.is_some(),
            receivers = receivers.len(),
            "flatpak share codec negotiated"
        );
        Ok((encoder, state))
    }

    /// Re-run codec negotiation for the current `receivers`. Returns `true`
    /// when the codec changed: the encoder was replaced (its first frame is a
    /// keyframe) and the caller must publish [`Self::stream_start_msg`] so
    /// receivers rebuild their decoders. A negotiation that cannot open an
    /// encoder keeps the running one.
    pub fn renegotiate(&self, receivers: &[CodecCapabilities]) -> bool {
        if self.paint {
            return false;
        }
        let Some((codec, hdr)) = choose(&self.config, &self.local_caps, receivers) else {
            return false;
        };
        let current = *self.codec_state.lock_recover();
        if current.codec == codec
            && current.hdr.is_some() == hdr.is_some()
            && current.encoded == encode_size(codec, (self.width, self.height), receivers)
        {
            return false;
        }
        let opened = Self::open_encoder(
            &self.config,
            &self.local_caps,
            receivers,
            self.width,
            self.height,
        );
        let (encoder, state) = match opened {
            Ok(pair) => pair,
            Err(error) => {
                tracing::warn!(surface = self.surface_id, %error, "codec switch failed; keeping the running encoder");
                return false;
            }
        };
        *self.encoder.lock_recover() = encoder;
        self.dmabuf_layouts.lock_recover().clear();
        *self.codec_state.lock_recover() = state;
        *self.standing_keyframe.lock_recover() = None;
        self.stream_sender.set_format(
            state.codec,
            display_size(state.codec, state.encoded.0, state.encoded.1),
        );
        if let Some(m) = &self.metrics {
            m.set_chosen_encoder(state.kind);
        }
        true
    }

    /// Attach a client-declared colorimetry override; the next
    /// [`Self::stream_start_msg`] carries it. Arc-shared so a background
    /// clone can update it without racing the session's read path.
    pub fn set_color_override(&self, colorimetry: Colorimetry, hdr: Option<Hdr10Metadata>) {
        if let Ok(mut slot) = self.color_override.lock() {
            *slot = Some((colorimetry, hdr));
        }
    }

    /// Drops any colorimetry override; the next [`Self::stream_start_msg`]
    /// falls back to the config default.
    pub fn clear_color_override(&self) {
        if let Ok(mut slot) = self.color_override.lock() {
            *slot = None;
        }
    }

    pub fn current_color_override(&self) -> ColorOverride {
        self.color_override.lock().ok().and_then(|g| *g)
    }

    pub fn chosen_encoder(&self) -> EncoderKind {
        self.codec_state.lock_recover().kind
    }

    /// Bitstream codec the encoder is emitting.
    pub fn chosen_codec(&self) -> Codec {
        self.codec_state.lock_recover().codec
    }

    /// Update the capture path (SHM ↔ DMABUF); propagates into the shared
    /// metrics handle so the OSD sees the switch immediately.
    pub fn set_capture_path(&self, path: CapturePath) {
        let mut slot = self.capture_path.lock_recover();
        *slot = path;
        drop(slot);
        if let Some(m) = &self.metrics {
            m.set_capture_path(path);
        }
    }

    pub fn capture_path(&self) -> CapturePath {
        *self.capture_path.lock_recover()
    }

    /// The window's size, the size frames are captured at.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The size frames are encoded and announced at: the window's, or less
    /// for a receiver's hardware decoder.
    pub fn encoded_dimensions(&self) -> (u32, u32) {
        self.codec_state.lock_recover().encoded
    }

    /// Name the workspace tab the window is in, for the stream start.
    #[must_use]
    pub fn with_tab(mut self, tab: u64) -> Self {
        self.tab = tab;
        self
    }

    /// The workspace tab the stream names; `0` when none.
    pub fn tab(&self) -> u64 {
        self.tab
    }

    /// Stream a changing window at `hz`: the refresh of the display it is on.
    pub fn set_rate(&self, hz: u32) {
        let rate = hz.max(1);
        if self.rate_hz.swap(rate, Ordering::AcqRel) != rate {
            self.throttle
                .lock_recover()
                .set_interval(frame_interval(rate));
        }
    }

    /// Frames a second a changing window is streamed at.
    pub fn rate(&self) -> u32 {
        self.rate_hz.load(Ordering::Acquire)
    }

    pub fn snapshot_config(&self) -> FlatpakShareConfig {
        self.config.clone()
    }

    pub fn stream_start_msg(&self) -> NeighbourMsg {
        // Single monotonic reading stamped into both `ts` and
        // `sender_monotonic_ns_at_send` — reading twice would smear the
        // clock-offset handshake by a couple hundred ns.
        let now_ns = self.monotonic_ns_now();
        let state = *self.codec_state.lock_recover();
        // The label and bit depth always describe the bytes. Colour
        // precedence: client-declared colorimetry > HDR config > SDR BT.709.
        let (default_color, default_hdr, bit_depth) = match state.hdr {
            Some(hdr_cfg) => (hdr_cfg.colorimetry(), Some(hdr_cfg.metadata), 10u8),
            None => (Colorimetry::sdr_bt709(), None, 8u8),
        };
        let (colorimetry, hdr_static) = match self.current_color_override() {
            Some((client_color, client_hdr)) => (Some(client_color), client_hdr.or(default_hdr)),
            None => (Some(default_color), default_hdr),
        };
        let codec_label = if self.paint {
            PAINT_CODEC.to_string()
        } else {
            state.codec.wire_label().to_string()
        };
        NeighbourMsg::SurfaceStreamStart {
            surface: self.surface_id,
            source_peer: self.host_user_id,
            app_id: self.app_id.clone(),
            width: state.encoded.0,
            height: state.encoded.1,
            codec: codec_label,
            ts: now_ns,
            sender_monotonic_ns_at_send: now_ns,
            colorimetry,
            hdr_static_metadata: hdr_static,
            bit_depth,
            tab: self.tab,
        }
    }

    /// Whether the app streams this window's page ([`Self::start_paint`]).
    pub fn is_paint(&self) -> bool {
        self.paint
    }

    /// One frame of a paint share's stream, as the app encoded it.
    pub fn paint_message(&self, payload: Vec<u8>) -> NeighbourMsg {
        NeighbourMsg::SurfaceFrame {
            surface: self.surface_id,
            source_peer: self.host_user_id,
            encoded_h264: payload,
            ts: self.monotonic_ns_now(),
            seq: self.paint_seq.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Build the `NeighbourMsg::SurfaceStreamEnd` payload. Ship *before*
    /// dropping the session so late viewers drop their `PeerView` instead
    /// of hanging on a stale decoder.
    pub fn stream_end_msg(&self) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamEnd {
            surface: self.surface_id,
            source_peer: self.host_user_id,
            ts: self.monotonic_ns_now(),
        }
    }

    /// Deposit a captured RGBA frame for the next tick. Drop-oldest — an
    /// undrained deposit is replaced, not queued. Bytes must be
    /// `width × height × 4` RGBA, unvalidated here to keep the hot path
    /// allocation-free; a mismatch surfaces as
    /// `EncodeError::BufferSizeMismatch` on the next pump.
    pub fn deposit_captured_rgba(&self, rgba: Vec<u8>) {
        self.stage(StagedKind::Rgba(rgba));
    }

    /// The frame store holds frame `seq` of the window
    /// ([`crate::compositor::foreign::ForeignFrameStore::frame_seq`]).
    /// Returns whether it is one not seen before.
    pub fn note_window_frame(&self, seq: u64) -> bool {
        !self.paint && seq != 0 && self.window_noted.swap(seq, Ordering::AcqRel) != seq
    }

    /// Encode the window as the frame store holds it when it is encoded,
    /// converting only what changed since the frame before.
    pub fn stage_window(&self) {
        self.stage(StagedKind::Window);
    }

    /// Encode the window as the frame store holds it, even unchanged since
    /// the frame before: a reader starts at it.
    pub fn stage_window_again(&self) {
        *self.window_taken.lock_recover() = None;
        self.stage(StagedKind::Window);
    }

    /// Frame `seq` of the window was sent as the client's dmabuf, so the
    /// store's copy of it is not encoded again.
    pub fn note_window_frame_sent(&self, seq: u64) {
        self.window_noted.store(seq, Ordering::Release);
    }

    /// Whether the encoder imports `dmabuf` without a copy.
    #[cfg(target_os = "linux")]
    pub fn takes_dmabuf(&self, dmabuf: &Dmabuf) -> bool {
        let format = dmabuf.format();
        let layout = (format.code as u32, u64::from(format.modifier));
        if let Some(takes) = self.dmabuf_layouts.lock_recover().get(&layout) {
            return *takes;
        }
        let takes = self
            .encoder
            .lock_recover()
            .accepts_dmabuf(layout.0, layout.1);
        self.dmabuf_layouts.lock_recover().insert(layout, takes);
        takes
    }

    /// The window as the frame store holds it, with what changed since the
    /// frame last taken from it; `None` when it holds none at the stream's
    /// size or nothing changed since.
    fn window_frame(&self) -> Option<(Vec<u8>, Option<Vec<Region>>)> {
        let store = crate::compositor::foreign::store();
        let snapshot = store.snapshot(self.surface_id)?;
        if (snapshot.width, snapshot.height) != (self.width, self.height) {
            return None;
        }
        let previous = self.window_taken.lock_recover().replace(snapshot.seq);
        if previous == Some(snapshot.seq) {
            return None;
        }
        let changed = previous
            .and_then(|previous| store.snapshot_changes(self.surface_id, previous, snapshot.seq));
        Some((snapshot.rgba, changed))
    }

    /// Replace any undrained deposit with `kind`, stamped now. A paint
    /// share never takes captured frames.
    fn stage(&self, kind: StagedKind) {
        if self.paint {
            return;
        }
        let capture_ts_ns = self.monotonic_ns_now();
        let mut slot = self.staged_frame.lock_recover();
        *slot = Some(StagedFrame {
            kind,
            capture_ts_ns,
        });
        drop(slot);
        self.staged_ready.notify_one();
    }

    /// Encode this window on a thread of its own from now on, so an encode
    /// never holds up the compositor. Each frame it encodes waits for
    /// [`Self::pump`], and `encoded` is called to say one does. The thread
    /// ends with the session.
    ///
    /// # Errors
    ///
    /// The thread could not be spawned; the session then keeps encoding
    /// where [`Self::pump`] is called.
    pub fn encode_in_background(
        self: &Arc<Self>,
        encoded: Arc<dyn Fn() + Send + Sync>,
    ) -> std::io::Result<()> {
        if self.paint || self.in_background.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let session = Arc::downgrade(self);
        let slot = Arc::clone(&self.staged_frame);
        let ready = Arc::clone(&self.staged_ready);
        let active = Arc::clone(&self.active);
        let spawned = std::thread::Builder::new()
            .name(format!("share-encode-{}", self.surface_id))
            .spawn(move || {
                let mut due_in: Option<Duration> = None;
                loop {
                    {
                        let staged = slot.lock_recover();
                        let live = |staged: &mut Option<StagedFrame>| {
                            staged.is_none() && active.load(Ordering::Acquire)
                        };
                        match due_in {
                            Some(wait) => drop(ready.wait_timeout_while(staged, wait, live)),
                            None => drop(ready.wait_while(staged, live)),
                        }
                    }
                    if !active.load(Ordering::Acquire) {
                        return;
                    }
                    let Some(session) = session.upgrade() else {
                        return;
                    };
                    let now = Instant::now();
                    if session.staged_due().is_some_and(|due| due <= now)
                        && let Some(frame) = session.encode_next_frame(now)
                    {
                        session.encoded.lock_recover().push(frame);
                        encoded();
                    }
                    due_in = session
                        .staged_due()
                        .map(|due| due.saturating_duration_since(Instant::now()));
                }
            });
        if let Err(error) = spawned {
            self.in_background.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    /// The frames encoded since last asked: those the background thread
    /// encoded, or, without one, the staged frame encoded now if it is due.
    pub fn pump(&self, now: Instant) -> Vec<EncodedFrame> {
        if self.in_background.load(Ordering::Acquire) {
            return std::mem::take(&mut *self.encoded.lock_recover());
        }
        self.encode_next_frame(now).into_iter().collect()
    }

    /// Whether a thread of its own encodes this window.
    pub fn encodes_in_background(&self) -> bool {
        self.in_background.load(Ordering::Acquire)
    }

    /// Zero-copy sibling of [`Self::deposit_captured_rgba`] — drop-oldest,
    /// same contract. `Dmabuf` is Arc-backed so this is O(1), no fd dup. If
    /// [`Self::encode_next_frame`] finds the encoder rejects zero-copy
    /// import, the frame is dropped for that tick and the compositor's SHM
    /// fallback picks up on the next commit.
    #[cfg(target_os = "linux")]
    pub fn deposit_captured_dmabuf(&self, dmabuf: Dmabuf) {
        self.stage(StagedKind::Dmabuf(dmabuf));
    }

    /// Pump one iteration of the encode loop. `None` covers throttled,
    /// nothing staged, or a swallowed encode error (logged once per
    /// consecutive run) — matches the compositor's "encode or drop" contract.
    #[tracing::instrument(level = "trace", skip(self))]
    /// When a captured frame the throttle held back is due to be encoded;
    /// `None` when nothing waits. A window that goes still right after a
    /// change commits nothing more, so its last frame is sent at this time
    /// rather than on a commit that never comes.
    pub fn staged_due(&self) -> Option<Instant> {
        if !self.active.load(Ordering::Acquire) || self.staged_frame.lock_recover().is_none() {
            return None;
        }
        Some(
            self.throttle
                .lock_recover()
                .next_due()
                .unwrap_or(self.origin),
        )
    }

    pub fn encode_next_frame(&self, now: Instant) -> Option<EncodedFrame> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        // Held only for the throttle check — released immediately rather
        // than across the (potentially slow) encode below; re-acquired
        // briefly again on success to record `mark_encoded`.
        {
            let throttle = self.throttle.lock_recover();
            if !throttle.should_encode(now) {
                return None;
            }
        }
        // Drain staged frame before taking the encoder lock — avoid holding both.
        let staged = self.staged_frame.lock_recover().take();
        let staged = staged?;
        let carried_ts = staged.capture_ts_ns;
        let window = if matches!(staged.kind, StagedKind::Window) {
            Some(self.window_frame()?)
        } else {
            *self.window_taken.lock_recover() = None;
            None
        };

        let mut encoder = self.encoder.lock_recover();
        if self.keyframe_wanted.swap(false, Ordering::AcqRel) {
            encoder.force_keyframe();
        }
        let mut nalus = Vec::new();
        // DMABUF paths lift plane fd/offset/stride off the `Dmabuf` handle
        // (Arc keeps the fds alive for the borrow); RGBA paths just hand the
        // byte slice through.
        let (encode_result, path_taken) = match &staged.kind {
            StagedKind::Rgba(rgba) => (
                encoder.encode(FrameInput::Rgba(rgba), &mut nalus),
                CapturePath::ShmReadback,
            ),
            StagedKind::Window => {
                let input = match &window {
                    Some((rgba, Some(changed))) => FrameInput::RgbaChanged { rgba, changed },
                    Some((rgba, None)) => FrameInput::Rgba(rgba),
                    None => return None,
                };
                (encoder.encode(input, &mut nalus), CapturePath::ShmReadback)
            }
            #[cfg(target_os = "linux")]
            StagedKind::Dmabuf(dmabuf) => (
                encoder.encode(dmabuf_frame_input(dmabuf), &mut nalus),
                CapturePath::DmabufDirect,
            ),
        };
        drop(encoder);
        match encode_result {
            Ok(stats) => {
                self.throttle.lock_recover().mark_encoded(now);
                // Record capture path only after the encoder actually
                // succeeds, not merely when a DMABUF buffer was detected.
                self.set_capture_path(path_taken);
                if let Some(m) = &self.metrics {
                    m.record_encode_wallclock(
                        u64::try_from(stats.encode_wallclock.as_nanos()).unwrap_or(u64::MAX),
                    );
                    if stats.keyframe {
                        m.record_keyframe();
                    }
                    // Rough bitrate sample: bytes-per-frame * fps.
                    let bytes_per_sec = (stats.bytes as u64).saturating_mul(u64::from(self.rate()));
                    m.record_bitrate(bytes_per_sec);
                }
                let seq = match self.stream_sender.publish(stats.keyframe, nalus.clone()) {
                    Ok(seq) => seq,
                    Err(error) => {
                        tracing::warn!(
                            surface = self.surface_id,
                            error = ?error,
                            "dropping encoded frame rejected by stream limits"
                        );
                        // The next unit would reference this one without a
                        // gap in the sequence to tell viewers it is missing.
                        self.encoder.lock_recover().force_keyframe();
                        *self.standing_keyframe.lock_recover() = None;
                        return None;
                    }
                };
                if stats.keyframe {
                    self.keyframe_after
                        .fetch_max(seq.saturating_add(1), Ordering::AcqRel);
                }
                let frame = EncodedFrame {
                    surface: self.surface_id,
                    source_peer: self.host_user_id,
                    nalus,
                    capture_ts_ns: carried_ts,
                    seq,
                };
                *self.standing_keyframe.lock_recover() = stats.keyframe.then(|| frame.clone());
                Some(frame)
            }
            Err(err) => {
                tracing::debug!(
                    surface = self.surface_id,
                    ?path_taken,
                    error = ?err,
                    "encoder rejected frame, dropping"
                );
                None
            }
        }
    }

    /// A viewer could not use unit `seq` (one before it went missing, or its
    /// decoder rejected one): make the next unit a keyframe. Refused when a
    /// keyframe after `seq` is already on its way, since that answers it, or
    /// within [`KEYFRAME_REQUEST_INTERVAL`] of the last forced one. Returns
    /// whether a keyframe was forced, so the caller encodes a frame even
    /// when the window is not changing.
    pub fn request_keyframe(&self, seq: u64, now: Instant) -> bool {
        if self.paint || seq < self.keyframe_after.load(Ordering::Acquire) {
            return false;
        }
        let mut requested = self.keyframe_requested.lock_recover();
        if requested.is_some_and(|at| now.saturating_duration_since(at) < KEYFRAME_REQUEST_INTERVAL)
        {
            return false;
        }
        *requested = Some(now);
        drop(requested);
        self.keyframe_wanted.store(true, Ordering::Release);
        true
    }

    /// A new reader can start only at a keyframe. Returns the standing one
    /// for the caller to send that reader; otherwise the next unit is a
    /// keyframe, which the caller encodes from the window as it stands.
    pub fn start_reader(&self) -> Option<EncodedFrame> {
        if self.paint {
            return None;
        }
        let standing = self.standing_keyframe.lock_recover().clone();
        if standing.is_none() {
            self.keyframe_wanted.store(true, Ordering::Release);
        }
        standing
    }

    /// The keyframe a viewer added while the window stands still starts at;
    /// the caller sends it to that viewer.
    pub fn standing_keyframe(&self) -> Option<EncodedFrame> {
        self.standing_keyframe.lock_recover().clone()
    }

    /// Register a peer as a viewer with the given read-only state (flip
    /// later via [`Self::set_read_only`]) and subscribe them to the
    /// [`SurfaceStreamSender`]. It starts at [`Self::start_reader`]'s
    /// keyframe.
    pub fn add_viewer(&self, peer: PeerId, read_only: bool) {
        let mut reg = self.share_registry.lock_recover();
        reg.insert(peer, self.surface_id, StreamingShare { read_only });
        drop(reg);
        self.stream_sender.subscribe(peer);
        let _ = self.start_reader();
    }

    /// Drop a viewer — subsequent `RemoteInput` events fall on the floor and
    /// their substream subscription closes.
    pub fn drop_viewer(&self, peer: PeerId) {
        let mut reg = self.share_registry.lock_recover();
        reg.remove(peer, self.surface_id);
        drop(reg);
        self.stream_sender.unsubscribe(peer);
    }

    /// Make the live viewer set exactly match the workspace's replicated
    /// recipient set. Existing read-only grants are preserved; newly added
    /// viewers start with control enabled and still pass through the
    /// compositor firewall before any input reaches the app.
    pub fn sync_viewers(&self, desired: &std::collections::HashSet<PeerId>) {
        self.sync_viewers_read_only(desired, &std::collections::HashSet::new());
    }

    /// [`Self::sync_viewers`], with the viewers in `read_only` only watching:
    /// a room member who views someone else's window gets no control of it
    /// unless its owner grants it. A viewer moving into `read_only` loses
    /// its control in place; read-only grants are otherwise preserved, as
    /// [`Self::sync_viewers`] keeps them.
    pub fn sync_viewers_read_only(
        &self,
        desired: &std::collections::HashSet<PeerId>,
        read_only: &std::collections::HashSet<PeerId>,
    ) {
        let existing = self
            .share_registry
            .lock_recover()
            .peers_for(self.surface_id);
        for peer in existing {
            if !desired.contains(&peer) {
                self.drop_viewer(peer);
            }
        }
        for peer in desired {
            let watching = read_only.contains(peer);
            match self.viewer_share(*peer) {
                None => self.add_viewer(*peer, watching),
                Some(share) if watching && !share.read_only => {
                    self.set_read_only(*peer, true);
                }
                Some(_) => {}
            }
        }
    }

    /// Cumulative drop-oldest count for a viewer's substream — diagnostic,
    /// surfaces "peer is falling behind" in the owner's UI.
    pub fn stream_drops_for(&self, peer: PeerId) -> u64 {
        self.stream_sender.drops_for(peer)
    }

    pub fn stream_total_drops(&self) -> u64 {
        self.stream_sender.total_drops()
    }

    pub fn stream_pending_for(&self, peer: PeerId) -> usize {
        self.stream_sender.pending_for(peer)
    }

    pub fn poll_stream_frame(
        &self,
        peer: PeerId,
    ) -> Option<crate::network::surface_stream::SurfaceFrame> {
        self.stream_sender.poll(peer)
    }

    pub fn stream_sender(&self) -> Arc<SurfaceStreamSender> {
        self.stream_sender.clone()
    }

    pub fn set_read_only(&self, peer: PeerId, read_only: bool) {
        let mut reg = self.share_registry.lock_recover();
        reg.set_read_only(peer, self.surface_id, read_only);
    }

    pub fn accept_remote_input(&self, peer: PeerId) -> bool {
        let reg = self.share_registry.lock_recover();
        reg.accept_remote_input(peer, self.surface_id)
    }

    /// Snapshot this viewer's current control grant so the compositor's
    /// full firewall can mirror the session-owned permission immediately
    /// before reviewing an inbound event.
    pub fn viewer_share(&self, peer: PeerId) -> Option<StreamingShare> {
        let reg = self.share_registry.lock_recover();
        reg.get(peer, self.surface_id).cloned()
    }

    pub fn viewer_ids(&self) -> Vec<PeerId> {
        self.share_registry
            .lock_recover()
            .peers_for(self.surface_id)
    }

    /// Route an inbound `InputEvent` through the share-registry gate only —
    /// see [`crate::network::streaming::ShareRegistry::route_to`]. A sink
    /// that is the compositor's seat must not be driven from here;
    /// [`crate::compositor::state::State::review_remote_input`] runs the
    /// region/z-order/scrubber/rate checks.
    pub fn route_input<S: crate::network::streaming::RemoteInputSink>(
        &self,
        peer: PeerId,
        event: InputEvent,
        sink: &mut S,
    ) -> bool {
        let reg = self.share_registry.lock_recover();
        reg.route_to(peer, self.surface_id, event, sink)
    }

    /// Monotonic ns counter for outgoing wire messages; wraps at u64
    /// overflow (~584 years), not a practical concern.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the u128 -> u64 wrap is intentional (see doc above); a saturating cast would silently change that behaviour at the multi-century mark"
    )]
    fn monotonic_ns_now(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }

    /// Reference `Instant` every `capture_ts_ns` is measured from — lets a
    /// receiver compute `origin.elapsed().as_nanos() - capture_ts_ns` for
    /// motion-to-photon latency without its own clock domain.
    pub fn origin(&self) -> Instant {
        self.origin
    }

    pub fn elapsed_ns(&self) -> u64 {
        self.monotonic_ns_now()
    }
}

/// The display size stamped on substream frames; H.264 frames keep the
/// original format without one.
fn display_size(codec: Codec, width: u32, height: u32) -> Option<(u16, u16)> {
    (codec != Codec::H264).then(|| {
        (
            u16::try_from(width).unwrap_or(u16::MAX),
            u16::try_from(height).unwrap_or(u16::MAX),
        )
    })
}

/// Ending the session ends the thread encoding it.
impl Drop for SharedFlatpakSession {
    fn drop(&mut self) {
        let slot = self.staged_frame.lock_recover();
        self.active.store(false, Ordering::Release);
        drop(slot);
        self.staged_ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paint_share_announces_its_codec_and_never_captures_or_encodes() {
        let s = SharedFlatpakSession::start_paint(
            0xBEEF,
            "so.tilo.browser",
            42,
            800,
            600,
            &cfg_software(),
        )
        .expect("start");
        assert!(s.is_paint());
        match s.stream_start_msg() {
            NeighbourMsg::SurfaceStreamStart {
                codec,
                width,
                height,
                app_id,
                ..
            } => {
                assert_eq!(codec, PAINT_CODEC);
                assert_eq!((width, height), (800, 600));
                assert_eq!(app_id, "so.tilo.browser");
            }
            other => panic!("expected SurfaceStreamStart, got {other:?}"),
        }
        s.deposit_captured_rgba(vec![0; 800 * 600 * 4]);
        assert!(
            s.staged_due().is_none(),
            "the window's pixels are never captured"
        );
        assert!(s.encode_next_frame(Instant::now()).is_none());
        assert!(!s.renegotiate(&[CodecCapabilities::default()]));
        match s.paint_message(vec![0, 1, 2]) {
            NeighbourMsg::SurfaceFrame {
                surface,
                source_peer,
                encoded_h264,
                ..
            } => {
                assert_eq!(
                    (surface, source_peer, encoded_h264),
                    (42, 0xBEEF, vec![0, 1, 2])
                );
            }
            other => panic!("expected SurfaceFrame, got {other:?}"),
        }
        s.add_viewer(9, false);
        assert!(
            s.accept_remote_input(9),
            "viewers drive a painted window as any other"
        );
        assert!(SharedFlatpakSession::start_paint(1, "x", 1, 0, 600, &cfg_software()).is_err());
    }
    use crate::network::streaming::test_support::RecordingSink;

    fn cfg_software() -> FlatpakShareConfig {
        FlatpakShareConfig {
            max_bitrate_kbps: 8000,
            target_fps: 30,
            encoder: EncoderChoice::Software,
            hdr: None,
        }
    }

    fn gradient_frame(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(width.saturating_mul(height).saturating_mul(4) as usize);
        for y in 0..height {
            for x in 0..width {
                let r = u8::try_from(x.saturating_mul(255).checked_div(width.max(1)).unwrap_or(0))
                    .unwrap_or(255);
                let g = u8::try_from(
                    y.saturating_mul(255)
                        .checked_div(height.max(1))
                        .unwrap_or(0),
                )
                .unwrap_or(255);
                out.push(r);
                out.push(g);
                out.push(128);
                out.push(255);
            }
        }
        out
    }

    /// A Pi-class viewer's 1080p hardware decoder bounds the encode; the
    /// window keeps its aspect and is never scaled up; with several viewers
    /// the smallest limit serves all; a viewer decoding in software or
    /// without a stated limit bounds nothing.
    #[test]
    fn the_encode_fits_every_hardware_decoder_watching() {
        use media::StreamCodec::{Av1, H264};
        let board = |limit| CodecCapabilities {
            hardware_decode: [H264].into(),
            hardware_decode_max: [(H264, limit)].into(),
            ..CodecCapabilities::default()
        };
        let desk = CodecCapabilities {
            hardware_decode: [H264, Av1].into(),
            ..CodecCapabilities::default()
        };
        let pi = board((1920, 1088));
        let small = board((1280, 720));

        assert_eq!(
            encode_size(H264, (2560, 1440), std::slice::from_ref(&pi)),
            (1920, 1080)
        );
        assert_eq!(
            encode_size(H264, (1440, 2560), std::slice::from_ref(&pi)),
            (612, 1088)
        );
        assert_eq!(
            encode_size(H264, (1280, 800), std::slice::from_ref(&pi)),
            (1280, 800)
        );
        assert_eq!(
            encode_size(H264, (2560, 1440), &[pi.clone(), small, desk.clone()]),
            (1280, 720),
            "one encode at the lowest common limit"
        );
        assert_eq!(encode_size(H264, (3840, 2160), &[desk]), (3840, 2160));
        assert_eq!(
            encode_size(Av1, (3840, 2160), &[pi]),
            (3840, 2160),
            "a codec the board decodes in software is not its concern"
        );
        assert_eq!(
            encode_size(H264, (1001, 701), &[]),
            (1000, 700),
            "even sides"
        );
    }

    #[test]
    #[allow(
        clippy::items_after_statements,
        reason = "a phase with an early return keeps its original body as a nested fn"
    )]
    fn share_start_negotiates_a_codec_every_viewer_decodes() {
        // Phase `a_hardware_preference_still_starts_a_share`.
        {
            // Preferring a vendor is a preference: without that hardware the
            // media stack falls through to software.
            for choice in [EncoderChoice::Vaapi, EncoderChoice::Nvenc] {
                let cfg = FlatpakShareConfig {
                    encoder: choice,
                    ..cfg_software()
                };
                SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, 64, 64, &cfg)
                    .expect("a preference never makes a share fail to start");
            }
        }
        // Phase `a_software_share_never_claims_hardware`.
        {
            let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, &cfg_software()).expect("start");
            assert!(
                !s.chosen_encoder().is_hardware(),
                "{:?}",
                s.chosen_encoder()
            );
        }
        // Phase `a_legacy_viewer_pins_h264_and_its_departure_allows_a_switch`.
        fn a_legacy_viewer_pins_h264_and_its_departure_allows_a_switch() {
            let cfg = cfg_software();
            let local = usable_local(cfg.encoder.policy());
            if !local.can_encode(Codec::H264) {
                eprintln!("no H.264 encoder here (no hardware, no Cisco openh264); not run");
                return;
            }
            let legacy = [CodecCapabilities::legacy_peer()];
            let s =
                SharedFlatpakSession::start_with_metrics(1, "x", 7, 64, 64, &cfg, &legacy, None)
                    .expect("start");
            assert_eq!(s.chosen_codec(), Codec::H264);
            assert!(!s.renegotiate(&legacy), "same viewers, same codec");
        }
        a_legacy_viewer_pins_h264_and_its_departure_allows_a_switch();
        // Phase `a_share_nobody_can_decode_fails_to_start`.
        fn a_share_nobody_can_decode_fails_to_start() {
            let cfg = cfg_software();
            let local = usable_local(cfg.encoder.policy());
            let legacy = [CodecCapabilities::legacy_peer()];
            if local.can_encode(Codec::H264) {
                eprintln!("an H.264 encoder exists here; the failure case cannot occur");
                return;
            }
            assert!(matches!(
                SharedFlatpakSession::start_with_metrics(1, "x", 7, 64, 64, &cfg, &legacy, None),
                Err(ShareError::EncoderUnavailable { .. })
            ));
        }
        a_share_nobody_can_decode_fails_to_start();
        // Phase `start_rejects_zero_and_oversized_dimensions`.
        {
            match SharedFlatpakSession::start(1, "x", 1, 0, 64, &cfg_software()) {
                Err(ShareError::DimensionsExceeded { .. }) => {}
                Err(other) => panic!("expected DimensionsExceeded for 0 width, got {other:?}"),
                Ok(_) => panic!("zero-width session should not construct"),
            }
            match SharedFlatpakSession::start(1, "x", 1, 5000, 64, &cfg_software()) {
                Err(ShareError::DimensionsExceeded { .. }) => {}
                Err(other) => {
                    panic!("expected DimensionsExceeded for 5000 width, got {other:?}")
                }
                Ok(_) => panic!("oversized-width session should not construct"),
            }
        }
        // Phase `a_software_encoder_streams_at_the_display_rate`.
        {
            let cfg = FlatpakShareConfig {
                target_fps: 90,
                ..cfg_software()
            };
            let s =
                SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, 64, 64, &cfg).expect("start");
            assert_eq!(s.rate(), 90);
            let frame = vec![0x80_u8; 64 * 64 * 4];
            let start = Instant::now();
            let encoded_at = |at: Instant| {
                s.deposit_captured_rgba(frame.clone());
                s.encode_next_frame(at).is_some()
            };
            assert!(encoded_at(start));
            assert!(encoded_at(start + Duration::from_micros(11_200)));
            assert!(!encoded_at(start + Duration::from_micros(13_000)));
            s.set_rate(144);
            assert_eq!(s.rate(), 144);
            assert!(encoded_at(start + Duration::from_micros(29_200)));
        }
    }

    #[test]
    fn a_window_encoded_on_its_own_thread_hands_each_frame_to_the_pump() {
        let session = Arc::new(
            SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, 64, 64, &cfg_software())
                .expect("start"),
        );
        let (encoded, wakes) = std::sync::mpsc::channel();
        let encoded = Mutex::new(encoded);
        session
            .encode_in_background(Arc::new(move || {
                let _ = encoded.lock_recover().send(());
            }))
            .expect("encode thread");
        assert!(session.encodes_in_background());

        session.deposit_captured_rgba(vec![0x20_u8; 64 * 64 * 4]);
        wakes
            .recv_timeout(Duration::from_secs(60))
            .expect("the frame is encoded");
        assert_eq!(session.pump(Instant::now()).len(), 1);
        assert!(session.pump(Instant::now()).is_empty());

        // Deposited before the next frame is due: held back, then encoded
        // when due without another commit.
        session.deposit_captured_rgba(vec![0xE0_u8; 64 * 64 * 4]);
        wakes
            .recv_timeout(Duration::from_secs(60))
            .expect("the held-back frame is encoded");
        assert_eq!(session.pump(Instant::now()).len(), 1);
    }

    #[test]
    fn share_stream_encodes_staged_frames_and_announces_its_session() {
        // Phase `stream_start_and_end_messages_carry_the_session_ids`.
        {
            let s =
                SharedFlatpakSession::start(0xBEEF, "org.gimp.GIMP", 42, 64, 64, &cfg_software())
                    .expect("start");
            match s.stream_start_msg() {
                NeighbourMsg::SurfaceStreamStart {
                    surface,
                    source_peer,
                    width,
                    height,
                    codec,
                    ts,
                    sender_monotonic_ns_at_send,
                    colorimetry,
                    hdr_static_metadata,
                    bit_depth,
                    app_id,
                    tab: _,
                } => {
                    assert_eq!(surface, 42);
                    assert_eq!(source_peer, 0xBEEF);
                    assert_eq!(width, 64);
                    assert_eq!(height, 64);
                    assert_eq!(codec, s.chosen_codec().wire_label());
                    assert_eq!(app_id, "org.gimp.GIMP");
                    assert!(sender_monotonic_ns_at_send > 0);
                    assert_eq!(sender_monotonic_ns_at_send, ts);
                    assert_eq!(
                        colorimetry,
                        Some(crate::collab::colorimetry::Colorimetry::sdr_bt709())
                    );
                    assert!(hdr_static_metadata.is_none());
                    assert_eq!(bit_depth, 8);
                }
                other => panic!("expected SurfaceStreamStart, got {other:?}"),
            }
            match s.stream_end_msg() {
                NeighbourMsg::SurfaceStreamEnd {
                    surface,
                    source_peer,
                    ..
                } => {
                    assert_eq!(surface, 42);
                    assert_eq!(source_peer, 0xBEEF);
                }
                other => panic!("expected SurfaceStreamEnd, got {other:?}"),
            }
        }
        // Phase `encode_next_frame_produces_annex_b_bytes`.
        {
            const W: u32 = 64;
            const H: u32 = 64;
            let s = SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, W, H, &cfg_software())
                .expect("start");
            s.deposit_captured_rgba(gradient_frame(W, H));
            let frame = s
                .encode_next_frame(Instant::now())
                .expect("encoder should yield a frame");
            assert_eq!(frame.surface, 7);
            assert!(!frame.nalus.is_empty(), "empty NALU payload");
            let msg = frame.clone().into_neighbour_msg();
            match msg {
                NeighbourMsg::SurfaceFrame {
                    surface,
                    source_peer,
                    encoded_h264,
                    ts,
                    seq,
                } => {
                    assert_eq!(surface, 7);
                    assert_eq!(seq, frame.seq);
                    assert_eq!(source_peer, 1);
                    assert_eq!(encoded_h264, frame.nalus);
                    assert_eq!(ts, frame.capture_ts_ns);
                }
                other => panic!("expected SurfaceFrame, got {other:?}"),
            }
        }
        // Phase `encode_next_frame_none_when_no_staged_frame`.
        {
            let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, &cfg_software()).expect("start");
            assert!(s.encode_next_frame(Instant::now()).is_none());
        }
        // Phase `stop_short_circuits_encode_next_frame`.
        {
            const W: u32 = 64;
            const H: u32 = 64;
            let s = SharedFlatpakSession::start(1, "x", 7, W, H, &cfg_software()).expect("start");
            s.deposit_captured_rgba(gradient_frame(W, H));
            // Flip `active` directly (bypassing `stop`) to isolate the short-circuit.
            let active = s.active.clone();
            active.store(false, Ordering::Release);
            assert!(s.encode_next_frame(Instant::now()).is_none());
        }
    }

    #[test]
    fn a_reader_starts_at_the_standing_keyframe_until_the_window_changes() {
        const W: u32 = 64;
        const H: u32 = 64;
        let s = SharedFlatpakSession::start(1, "x", 7, W, H, &cfg_software()).expect("start");
        let changed = || {
            let mut frame = gradient_frame(W, H);
            frame.iter_mut().take(4096).for_each(|byte| *byte = !*byte);
            frame
        };
        let mut now = Instant::now();
        s.deposit_captured_rgba(gradient_frame(W, H));
        let first = s.encode_next_frame(now).expect("first frame");
        assert_eq!(s.standing_keyframe(), Some(first.clone()));
        s.add_viewer(9, true);
        assert_eq!(s.start_reader(), Some(first.clone()));
        assert_eq!(s.start_reader(), Some(first.clone()));

        now += Duration::from_millis(100);
        s.deposit_captured_rgba(changed());
        let delta = s.encode_next_frame(now).expect("the changed window");
        assert!(delta.seq > first.seq);
        assert_eq!(s.standing_keyframe(), None);
        assert_eq!(s.start_reader(), None);

        now += Duration::from_millis(100);
        s.deposit_captured_rgba(changed());
        let keyframe = s.encode_next_frame(now).expect("a keyframe of the window");
        assert_eq!(s.standing_keyframe(), Some(keyframe.clone()));
        assert_eq!(s.start_reader(), Some(keyframe));
    }

    #[test]
    fn viewer_membership_gates_remote_input() {
        // Phase `viewer_registration_gates_remote_input`.
        {
            let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, &cfg_software()).expect("start");
            let peer: PeerId = 99;

            assert!(!s.accept_remote_input(peer));

            s.add_viewer(peer, false);
            assert!(s.accept_remote_input(peer));

            s.set_read_only(peer, true);
            assert!(!s.accept_remote_input(peer));

            s.set_read_only(peer, false);
            assert!(s.accept_remote_input(peer));

            s.drop_viewer(peer);
            assert!(!s.accept_remote_input(peer));
        }
        // Phase `viewer_reconciliation_adds_and_retires_exact_recipients`.
        {
            let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, &cfg_software()).expect("start");
            s.add_viewer(10, false);
            s.set_read_only(10, true);

            s.sync_viewers(&std::collections::HashSet::from([10, 20]));
            assert!(
                !s.accept_remote_input(10),
                "an existing read-only grant must stay read-only"
            );
            assert!(s.accept_remote_input(20), "new recipients receive control");

            s.sync_viewers(&std::collections::HashSet::from([20]));
            assert!(
                s.viewer_share(10).is_none(),
                "removed recipients are retired"
            );
            assert!(s.accept_remote_input(20));
        }
        // Phase `route_input_delivers_only_when_accepted`.
        {
            let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, &cfg_software()).expect("start");
            let peer: PeerId = 42;
            let mut sink = RecordingSink::default();

            assert!(!s.route_input(
                peer,
                InputEvent::PointerMotion { x: 1.0, y: 2.0 },
                &mut sink
            ));
            assert!(sink.events.is_empty());

            s.add_viewer(peer, false);
            assert!(s.route_input(
                peer,
                InputEvent::PointerMotion { x: 3.0, y: 4.0 },
                &mut sink
            ));
            assert_eq!(sink.events.len(), 1);
            assert_eq!(sink.events[0].0, peer);
            assert_eq!(sink.events[0].1, 7);
        }
    }
}
