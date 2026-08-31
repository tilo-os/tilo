//! `SharedFlatpakSession` composes surface capture, encode throttling, an
//! H.264 (or HEVC Main10 for HDR) encoder, and the `NeighbourMsg` wire
//! envelope into one live app share. Transport, cursor overlay, sandbox
//! brokering, and input routing live elsewhere — this module only produces
//! encoded frames and stream-start/stream-end envelopes.
//!
//! `EncoderChoice::Vaapi` / `EncoderChoice::Nvenc` are declared in config but
//! not linked in v1; requesting one returns `ShareError::EncoderUnavailable`
//! from the constructor rather than silently falling back.

use std::os::fd::AsRawFd;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use crate::collab::colorimetry::Colorimetry;
use crate::collab::encoder::{
    Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, ProbeOutcome, build, probe,
};
use crate::collab::hdr_metadata::Hdr10Metadata;
use crate::config::{EncoderChoice, FlatpakShareConfig};
use crate::diagnostics::{CapturePath, PeerSurfaceMetrics};
use crate::network::encoder::EncodeError;
use crate::network::protocol::{NeighbourMsg, PeerId, SurfaceId};
use crate::network::streaming::{InputEvent, ShareRegistry, StreamingShare, StreamingThrottle};
use crate::network::surface_stream::SurfaceStreamSender;
use smithay::backend::allocator::Buffer as _;
use smithay::backend::allocator::dmabuf::Dmabuf;

/// Software-encoder FPS ceiling — openh264 can't sustain more on a typical
/// laptop; hardware backends lift this.
const SOFTWARE_FPS_CEILING: u32 = 30;

/// Scans `nalus` for an H.264 IDR NAL unit (`nal_unit_type == 5`) by walking
/// Annex-B start codes. Returns `false` on no start-code / empty input —
/// treating "unknown" as non-keyframe is the safe direction for a
/// late-joining viewer.
///
/// Every lookup goes through `.get()` rather than direct indexing: a start
/// code landing as the very last bytes of `nalus` (with no NAL header byte
/// following it) must read as "not a keyframe", not panic the compositor.
fn is_h264_keyframe(nalus: &[u8]) -> bool {
    let mut i = 0usize;
    while i.saturating_add(4) <= nalus.len() {
        let Some(window) = nalus.get(i..) else {
            break;
        };
        if window.starts_with(&[0, 0, 0, 1]) {
            if window.get(4).is_some_and(|&h| h & 0x1f == 5) {
                return true;
            }
            i = i.saturating_add(4);
            continue;
        }
        if window.starts_with(&[0, 0, 1]) {
            if window.get(3).is_some_and(|&h| h & 0x1f == 5) {
                return true;
            }
            i = i.saturating_add(3);
            continue;
        }
        i = i.saturating_add(1);
    }
    false
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
    /// Row-major RGBA bytes from SHM readback.
    Rgba(Vec<u8>),
    /// DMABUF handle handed to the encoder; Arc-backed inside smithay, so
    /// staging it keeps every plane fd alive without a dup.
    Dmabuf(Dmabuf),
}

/// One encoded frame, ready to wrap in `NeighbourMsg::SurfaceFrame`.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedFrame {
    /// Surface the encoder is capturing.
    pub surface: SurfaceId,
    /// H.264 Annex-B NALU payload.
    pub nalus: Vec<u8>,
    /// Owner-side capture timestamp (monotonic ns). Receivers subtract their
    /// translated local clock to compute motion-to-photon latency.
    pub capture_ts_ns: u64,
}

impl EncodedFrame {
    /// Consumes `self` — NALUs move into the message rather than being cloned.
    pub fn into_neighbour_msg(self) -> NeighbourMsg {
        NeighbourMsg::SurfaceFrame {
            surface: self.surface,
            encoded_h264: self.nalus,
            ts: self.capture_ts_ns,
        }
    }
}

/// Errors from session construction or lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    /// Requested encoder not resolvable to a working backend.
    #[error("encoder {choice:?} is not available in this build")]
    EncoderUnavailable { choice: EncoderChoice },
    /// Chosen encoder failed to construct — bad dimensions, unsupported
    /// profile, or a device disappeared between probe and open.
    #[error("encoder construction failed: {0:?}")]
    EncoderConstruction(EncodeError),
    /// Share dimensions exceed the 4096×4096 ceiling.
    #[error("dimensions {width}×{height} exceed the 4096×4096 ceiling")]
    DimensionsExceeded { width: u32, height: u32 },
}

/// Client-declared colorimetry override plus optional HDR static metadata,
/// as attached by [`SharedFlatpakSession::set_color_override`].
type ColorOverride = Option<(Colorimetry, Option<Hdr10Metadata>)>;

/// A live share session on the owner side. Drop it — or call [`Self::stop`]
/// for an explicit tear-down — to release the encoder and registry entries.
pub struct SharedFlatpakSession {
    /// User hosting the share; matches `NeighbourMsg`'s `source_peer`.
    pub host_user_id: PeerId,
    /// Flatpak app-id; display-only, not used for logic.
    pub app_id: String,
    pub surface_id: SurfaceId,
    width: u32,
    height: u32,
    /// Config snapshot at session start — no hot-reload; changes require
    /// stop/restart.
    config: FlatpakShareConfig,
    /// Owned encoder. `Arc<Mutex<_>>` so tests can share the handle across
    /// threads; production never contends (single-threaded pump).
    /// Type-erased so hardware and software backends share one interface.
    encoder: Arc<Mutex<Box<dyn Encoder>>>,
    /// Runtime-selected encoder backend; distinct from `config.encoder`
    /// (the config-time request) — they diverge when the probe falls back.
    chosen_encoder: EncoderKind,
    /// Optional shared metrics handle; `encode_next_frame` records encode
    /// stats here, and the receiving `PeerView` shares the same `Arc` for a
    /// unified OSD stat block.
    metrics: Option<PeerSurfaceMetrics>,
    /// Current capture path; flipped by [`Self::set_capture_path`] when the
    /// compositor's commit hook re-routes the surface's buffer between SHM
    /// and DMABUF.
    capture_path: Arc<Mutex<CapturePath>>,
    /// Owned throttle. Same locking rationale as `encoder`.
    throttle: Arc<Mutex<StreamingThrottle>>,
    /// Owner-side gate for inbound `NeighbourMsg::RemoteInput`; shared with
    /// the compositor's `State` via `Arc` so input routing sees the same
    /// permissions.
    share_registry: Arc<Mutex<ShareRegistry>>,
    /// Frame source: deposit fns write here from the compositor's commit
    /// hook, `encode_next_frame` drains via `Option::take` (drop-oldest).
    /// The capture timestamp is stamped at deposit time, not encode time,
    /// so the receiver's latency measurement includes encode cost.
    staged_frame: Arc<Mutex<Option<StagedFrame>>>,
    /// Monotonic-clock origin; `Instant::now() - origin` gives the `u64` ns
    /// used in every wire `ts` field.
    origin: Instant,
    /// Still-alive flag; `stop()` flips it false so the pump short-circuits
    /// and a double-stop is safe.
    active: Arc<AtomicBool>,
    /// Per-share video substream sender — every `encode_next_frame` deposits
    /// here in addition to returning the frame; the transport adapter polls
    /// per subscriber and writes bytes on the wire. Per-subscriber bounded
    /// queue with drop-oldest backpressure; a keyframe flag in the header
    /// lets late-joining viewers know when it's safe to start decoding.
    stream_sender: Arc<SurfaceStreamSender>,
    /// Client-declared colorimetry override, set via
    /// [`Self::set_color_override`] when the compositor sees
    /// `wp_color_management_surface_v1.set_image_description`.
    /// [`Self::stream_start_msg`] prefers this over the config default.
    /// `None` means no override.
    color_override: Arc<Mutex<ColorOverride>>,
    /// True when HDR was configured but no HEVC hardware was available at
    /// start, so the session fell back to SDR H.264 and `stream_start_msg`
    /// tags SDR colorimetry to match the actual bytes. A client override can
    /// still tag the wire HDR while the encoded bytes stay 8-bit — receivers
    /// should trust `bit_depth`, not just colorimetry.
    hdr_requested_but_unavailable: bool,
}

impl SharedFlatpakSession {
    /// Construct + activate a new share session. `width`/`height` must match
    /// the captured surface's `wl_shm` buffer or the encoder rejects the
    /// first frame with [`EncodeError::BufferSizeMismatch`].
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
        config: FlatpakShareConfig,
    ) -> Result<Self, ShareError> {
        Self::start_with_metrics(
            host_user_id,
            app_id,
            surface_id,
            width,
            height,
            &config,
            None,
        )
    }

    /// Same as [`Self::start`] but attaches a shared [`PeerSurfaceMetrics`]
    /// handle so the OSD sees encode-side counters alongside receiver-side
    /// ones.
    ///
    /// # Errors
    ///
    /// Same failure modes as [`Self::start`].
    pub fn start_with_metrics(
        host_user_id: PeerId,
        app_id: impl Into<String>,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        config: &FlatpakShareConfig,
        metrics: Option<PeerSurfaceMetrics>,
    ) -> Result<Self, ShareError> {
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(ShareError::DimensionsExceeded { width, height });
        }

        // Codec follows the HDR config: SDR → H.264, HDR → HEVC Main10. No
        // HDR config keeps pre-HDR callers' behaviour unchanged.
        let codec = match config.hdr {
            None => super::encoder::Codec::H264,
            Some(_) => super::encoder::Codec::HevcMain10,
        };
        let (chosen, outcomes) = probe(codec, Some(config.encoder));
        log_probe_outcomes(&outcomes, chosen);

        // Only clamp fps under openh264 — hardware HEVC (e.g. NVENC) easily
        // sustains 60 fps.
        let effective_fps = if chosen == EncoderKind::Openh264 {
            config.target_fps.clamp(1, SOFTWARE_FPS_CEILING)
        } else {
            config.target_fps.max(1)
        };

        // HDR needs a higher bitrate floor: BT.2020-PQ at 720p needs ~10
        // Mbps for reasonable quality vs ~4 Mbps for SDR.
        let sdr_floor = 500u32;
        let hdr_floor = 4000u32;
        let effective_bitrate_kbps = if codec.bit_depth() == 10 {
            config.max_bitrate_kbps.max(hdr_floor)
        } else {
            config.max_bitrate_kbps.max(sdr_floor)
        };
        // HDR requested but the probe fell through to openh264: stay usable
        // rather than fail to start — build an SDR encoder so the wire label
        // matches the actual bytes, instead of a silently HDR-labelled
        // 8-bit stream.
        let hdr_requested_but_unavailable = config.hdr.is_some() && chosen == EncoderKind::Openh264;
        let init = match (config.hdr, hdr_requested_but_unavailable) {
            (None, _) | (Some(_), true) => {
                EncoderInit::sdr_h264(width, height, effective_fps, effective_bitrate_kbps, 1)
            }
            (Some(hdr_cfg), false) => EncoderInit::hdr10_hevc(
                width,
                height,
                effective_fps,
                effective_bitrate_kbps,
                1,
                hdr_cfg,
            ),
        };
        let encoder = build(chosen, init).map_err(|e| match e {
            EncoderError::Unavailable { .. } => ShareError::EncoderUnavailable {
                choice: config.encoder,
            },
            EncoderError::ConfigFailed { detail, .. }
            | EncoderError::EncodeFailed { detail, .. } => {
                ShareError::EncoderConstruction(EncodeError::Backend(detail))
            }
        })?;

        if let Some(m) = &metrics {
            m.set_chosen_encoder(chosen);
        }

        // `effective_fps` is clamped/maxed to at least 1 above, so this
        // division never actually hits zero; `checked_div` plus a 1-second
        // fallback keeps that provable rather than merely assumed.
        let interval = std::time::Duration::from_micros(
            1_000_000u64
                .checked_div(u64::from(effective_fps))
                .unwrap_or(1_000_000)
                .max(1),
        );

        Ok(Self {
            host_user_id,
            app_id: app_id.into(),
            surface_id,
            width,
            height,
            config: FlatpakShareConfig {
                max_bitrate_kbps: config.max_bitrate_kbps,
                target_fps: effective_fps,
                encoder: config.encoder,
                hdr: config.hdr,
            },
            encoder: Arc::new(Mutex::new(encoder)),
            chosen_encoder: chosen,
            metrics,
            capture_path: Arc::new(Mutex::new(CapturePath::ShmReadback)),
            throttle: Arc::new(Mutex::new(StreamingThrottle::with_interval(interval))),
            share_registry: Arc::new(Mutex::new(ShareRegistry::new())),
            staged_frame: Arc::new(Mutex::new(None)),
            origin: Instant::now(),
            active: Arc::new(AtomicBool::new(true)),
            stream_sender: Arc::new(SurfaceStreamSender::new(surface_id, host_user_id)),
            color_override: Arc::new(Mutex::new(None)),
            hdr_requested_but_unavailable,
        })
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

    /// Currently-active colorimetry override.
    pub fn current_color_override(&self) -> ColorOverride {
        self.color_override.lock().ok().and_then(|g| *g)
    }

    /// Runtime-selected encoder; distinct from `config.encoder` (the
    /// config-time request).
    pub fn chosen_encoder(&self) -> EncoderKind {
        self.chosen_encoder
    }

    /// Bitstream codec the encoder is emitting; distinct from
    /// `chosen_encoder` since one backend can drive either H.264 or HEVC
    /// Main10 depending on the HDR config.
    pub fn chosen_codec(&self) -> super::encoder::Codec {
        // Lock held only for a cheap Copy read; no contention with encode_next_frame.
        self.encoder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .codec()
    }

    /// Update the capture path (SHM ↔ DMABUF); propagates into the shared
    /// metrics handle so the OSD sees the switch immediately.
    pub fn set_capture_path(&self, path: CapturePath) {
        // Dropped before the metrics call below, which doesn't need it.
        let mut slot = self
            .capture_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = path;
        drop(slot);
        if let Some(m) = &self.metrics {
            m.set_capture_path(path);
        }
    }

    pub fn capture_path(&self) -> CapturePath {
        *self
            .capture_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Dimensions the encoder is configured for; constant for the session
    /// lifetime (resize requires stop+start).
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Effective config after any clamping applied at `start`.
    pub fn snapshot_config(&self) -> FlatpakShareConfig {
        self.config.clone()
    }

    /// Build the `NeighbourMsg::SurfaceStreamStart` payload to publish on
    /// session start.
    pub fn stream_start_msg(&self) -> NeighbourMsg {
        // Single monotonic reading stamped into both `ts` and
        // `sender_monotonic_ns_at_send` — reading twice would smear the
        // clock-offset handshake by a couple hundred ns.
        let now_ns = self.monotonic_ns_now();
        // Precedence: client-declared colorimetry > HDR config > SDR BT.709
        // default. Bit depth follows the transfer function (PQ/HLG ⇒ 10-bit).
        let override_val = self.current_color_override();
        let (codec_label, colorimetry, hdr_static, bit_depth) =
            if let Some((client_color, client_hdr)) = override_val {
                let is_hdr = client_color.is_hdr();
                let codec = if is_hdr {
                    super::encoder::Codec::HevcMain10
                } else {
                    super::encoder::Codec::H264
                };
                (
                    codec.wire_label().to_string(),
                    Some(client_color),
                    client_hdr.or_else(|| self.config.hdr.map(|c| c.metadata)),
                    if is_hdr { 10u8 } else { 8u8 },
                )
            } else {
                // Wire must match actual bytes: SDR runtime encoder ⇒ SDR
                // wire label even if HDR was requested.
                match (self.config.hdr, self.hdr_requested_but_unavailable) {
                    (None, _) | (Some(_), true) => (
                        super::encoder::Codec::H264.wire_label().to_string(),
                        Some(crate::collab::colorimetry::Colorimetry::sdr_bt709()),
                        None,
                        8u8,
                    ),
                    (Some(hdr_cfg), false) => (
                        super::encoder::Codec::HevcMain10.wire_label().to_string(),
                        Some(hdr_cfg.colorimetry()),
                        Some(hdr_cfg.metadata),
                        10u8,
                    ),
                }
            };
        NeighbourMsg::SurfaceStreamStart {
            surface: self.surface_id,
            source_peer: self.host_user_id,
            width: self.width,
            height: self.height,
            codec: codec_label,
            ts: now_ns,
            sender_monotonic_ns_at_send: now_ns,
            colorimetry,
            hdr_static_metadata: hdr_static,
            bit_depth,
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
    /// [`EncodeError::BufferSizeMismatch`] on the next pump.
    pub fn deposit_captured_rgba(&self, rgba: Vec<u8>) {
        let capture_ts_ns = self.monotonic_ns_now();
        let mut slot = self
            .staged_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(StagedFrame {
            kind: StagedKind::Rgba(rgba),
            capture_ts_ns,
        });
    }

    /// Zero-copy sibling of [`Self::deposit_captured_rgba`] — drop-oldest,
    /// same contract. `Dmabuf` is Arc-backed so this is O(1), no fd dup. If
    /// [`Self::encode_next_frame`] finds the encoder rejects zero-copy
    /// import, the frame is dropped for that tick and the compositor's SHM
    /// fallback picks up on the next commit.
    pub fn deposit_captured_dmabuf(&self, dmabuf: Dmabuf) {
        let capture_ts_ns = self.monotonic_ns_now();
        let mut slot = self
            .staged_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(StagedFrame {
            kind: StagedKind::Dmabuf(dmabuf),
            capture_ts_ns,
        });
    }

    /// Pump one iteration of the encode loop. `None` covers throttled,
    /// nothing staged, or a swallowed encode error (logged once per
    /// consecutive run) — matches the compositor's "encode or drop" contract.
    #[tracing::instrument(level = "trace", skip(self))]
    pub fn encode_next_frame(&self, now: Instant) -> Option<EncodedFrame> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        // Held only for the throttle check — released immediately rather
        // than across the (potentially slow) encode below; re-acquired
        // briefly again on success to record `mark_encoded`.
        {
            let throttle = self
                .throttle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !throttle.should_encode(now) {
                return None;
            }
        }
        // Drain staged frame before taking the encoder lock — avoid holding both.
        let staged = self
            .staged_frame
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let staged = staged?;
        let carried_ts = staged.capture_ts_ns;

        let mut encoder = self
            .encoder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut nalus = Vec::new();
        // DMABUF paths lift plane fd/offset/stride off the `Dmabuf` handle
        // (Arc keeps the fds alive for the borrow); RGBA paths just hand the
        // byte slice through.
        let (encode_result, path_taken) = match &staged.kind {
            StagedKind::Rgba(rgba) => (
                encoder.encode(FrameInput::Rgba(rgba), &mut nalus),
                CapturePath::ShmReadback,
            ),
            StagedKind::Dmabuf(dmabuf) => {
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
                    // `.take(4)` bounds `i` to the four plane slots, but
                    // index via `.get_mut()` anyway rather than trust that
                    // invariant to hold forever.
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
                let input = FrameInput::DmabufFd {
                    plane_fds,
                    plane_offsets,
                    plane_strides,
                    num_planes,
                    drm_fourcc: fmt.code as u32,
                    drm_modifier: u64::from(fmt.modifier),
                    width: dmabuf.width(),
                    height: dmabuf.height(),
                };
                (encoder.encode(input, &mut nalus), CapturePath::DmabufDirect)
            }
        };
        match encode_result {
            Ok(stats) => {
                self.throttle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .mark_encoded(now);
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
                    let bytes_per_sec =
                        (stats.bytes as u64).saturating_mul(u64::from(self.config.target_fps));
                    m.record_bitrate(bytes_per_sec);
                }
                let keyframe = is_h264_keyframe(&nalus);
                self.stream_sender.publish(keyframe, nalus.clone());
                Some(EncodedFrame {
                    surface: self.surface_id,
                    nalus,
                    capture_ts_ns: carried_ts,
                })
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

    /// Register a peer as a viewer with the given read-only state (flip
    /// later via [`Self::set_read_only`]) and subscribe them to the
    /// [`SurfaceStreamSender`].
    pub fn add_viewer(&self, peer: PeerId, read_only: bool) {
        let mut reg = self
            .share_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.insert(peer, self.surface_id, StreamingShare { read_only });
        drop(reg);
        self.stream_sender.subscribe(peer);
    }

    /// Drop a viewer — subsequent `RemoteInput` events fall on the floor and
    /// their substream subscription closes.
    pub fn drop_viewer(&self, peer: PeerId) {
        let mut reg = self
            .share_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.remove(peer, self.surface_id);
        drop(reg);
        self.stream_sender.unsubscribe(peer);
    }

    /// Cumulative drop-oldest count for a viewer's substream — diagnostic,
    /// surfaces "peer is falling behind" in the owner's UI.
    pub fn stream_drops_for(&self, peer: PeerId) -> u64 {
        self.stream_sender.drops_for(peer)
    }

    /// Total drops across every subscriber. Diagnostic aggregate.
    pub fn stream_total_drops(&self) -> u64 {
        self.stream_sender.total_drops()
    }

    /// Pending queue depth for a viewer's substream. Diagnostic.
    pub fn stream_pending_for(&self, peer: PeerId) -> usize {
        self.stream_sender.pending_for(peer)
    }

    /// Poll the next queued frame for `peer`; `None` when unsubscribed or drained.
    pub fn poll_stream_frame(
        &self,
        peer: PeerId,
    ) -> Option<crate::network::surface_stream::SurfaceFrame> {
        self.stream_sender.poll(peer)
    }

    /// Direct handle onto the substream sender, for the actor's QUIC
    /// adapter (or test inspection).
    pub fn stream_sender(&self) -> Arc<SurfaceStreamSender> {
        self.stream_sender.clone()
    }

    /// Flip the read-only flag on an existing viewer.
    pub fn set_read_only(&self, peer: PeerId, read_only: bool) {
        let mut reg = self
            .share_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.set_read_only(peer, self.surface_id, read_only);
    }

    /// Consult the share registry — same gate `State::apply_remote_input` uses.
    pub fn accept_remote_input(&self, peer: PeerId) -> bool {
        let reg = self
            .share_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.accept_remote_input(peer, self.surface_id)
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
        let reg = self
            .share_registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reg.route_to(peer, self.surface_id, event, sink)
    }

    /// Stop the session (flips `active`, so `encode_next_frame`
    /// short-circuits). Publish [`Self::stream_end_msg`] *before* calling this.
    pub fn stop(self) {
        self.active.store(false, Ordering::Release);
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

    /// Monotonic ns since `origin`, measured now — same conversion the wire
    /// timestamps use.
    pub fn elapsed_ns(&self) -> u64 {
        self.monotonic_ns_now()
    }
}

/// Logs the chosen encoder plus every backend the probe walked and why —
/// diagnoses "why VA-API isn't kicking in" without the full trace subscriber.
fn log_probe_outcomes(outcomes: &[ProbeOutcome], chosen: EncoderKind) {
    for o in outcomes {
        tracing::debug!(
            kind = o.kind.as_str(),
            available = o.available,
            reason = %o.reason,
            "encoder probe outcome"
        );
    }
    let reason = outcomes
        .iter()
        .find(|o| o.kind == chosen)
        .map_or("", |o| o.reason.as_str());
    tracing::info!(
        chosen = chosen.as_str(),
        reason,
        "flatpak share encoder selected"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::streaming::test_support::RecordingSink;

    fn cfg_openh264() -> FlatpakShareConfig {
        FlatpakShareConfig {
            max_bitrate_kbps: 8000,
            target_fps: 30,
            encoder: EncoderChoice::Openh264,
            hdr: None,
        }
    }

    fn gradient_frame(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let r = ((x * 255) / width.max(1)) as u8;
                let g = ((y * 255) / height.max(1)) as u8;
                out.push(r);
                out.push(g);
                out.push(128);
                out.push(255);
            }
        }
        out
    }

    #[test]
    fn requested_vaapi_or_nvenc_falls_back_to_openh264_when_hw_absent() {
        // On a stripped host (no hw feature, no /dev/dri) the probe falls
        // through to openh264 for both choices.
        for choice in [EncoderChoice::Vaapi, EncoderChoice::Nvenc] {
            let cfg = FlatpakShareConfig {
                encoder: choice,
                ..cfg_openh264()
            };
            let session = SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, 64, 64, cfg)
                .expect("session should fall back to openh264");
            #[cfg(not(feature = "vaapi"))]
            #[cfg(not(feature = "nvenc"))]
            assert_eq!(
                session.chosen_encoder(),
                EncoderKind::Openh264,
                "no hardware features compiled: must land on openh264"
            );
        }
    }

    #[test]
    fn start_rejects_zero_and_oversized_dimensions() {
        match SharedFlatpakSession::start(1, "x", 1, 0, 64, cfg_openh264()) {
            Err(ShareError::DimensionsExceeded { .. }) => {}
            Err(other) => panic!("expected DimensionsExceeded for 0 width, got {other:?}"),
            Ok(_) => panic!("zero-width session should not construct"),
        }
        match SharedFlatpakSession::start(1, "x", 1, 5000, 64, cfg_openh264()) {
            Err(ShareError::DimensionsExceeded { .. }) => {}
            Err(other) => {
                panic!("expected DimensionsExceeded for 5000 width, got {other:?}")
            }
            Ok(_) => panic!("oversized-width session should not construct"),
        }
    }

    #[test]
    fn target_fps_clamps_to_software_ceiling() {
        let cfg = FlatpakShareConfig {
            target_fps: 60,
            ..cfg_openh264()
        };
        let s = SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, 64, 64, cfg).expect("start");
        assert_eq!(
            s.snapshot_config().target_fps,
            SOFTWARE_FPS_CEILING,
            "SW encoder must clamp target_fps to the ceiling"
        );
    }

    #[test]
    fn stream_start_and_end_messages_carry_the_session_ids() {
        let s = SharedFlatpakSession::start(0xBEEF, "org.gimp.GIMP", 42, 64, 64, cfg_openh264())
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
            } => {
                assert_eq!(surface, 42);
                assert_eq!(source_peer, 0xBEEF);
                assert_eq!(width, 64);
                assert_eq!(height, 64);
                assert_eq!(codec, "h264");
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

    #[test]
    fn encode_next_frame_produces_annex_b_bytes() {
        const W: u32 = 64;
        const H: u32 = 64;
        let s = SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, W, H, cfg_openh264())
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
                encoded_h264,
                ts,
            } => {
                assert_eq!(surface, 7);
                assert_eq!(encoded_h264, frame.nalus);
                assert_eq!(ts, frame.capture_ts_ns);
            }
            other => panic!("expected SurfaceFrame, got {other:?}"),
        }
    }

    #[test]
    fn encode_next_frame_none_when_no_staged_frame() {
        let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, cfg_openh264()).expect("start");
        assert!(s.encode_next_frame(Instant::now()).is_none());
    }

    #[test]
    fn stop_short_circuits_encode_next_frame() {
        const W: u32 = 64;
        const H: u32 = 64;
        let s = SharedFlatpakSession::start(1, "x", 7, W, H, cfg_openh264()).expect("start");
        s.deposit_captured_rgba(gradient_frame(W, H));
        // Flip `active` directly (bypassing `stop`) to isolate the short-circuit.
        let active = s.active.clone();
        active.store(false, Ordering::Release);
        assert!(s.encode_next_frame(Instant::now()).is_none());
    }

    #[test]
    fn viewer_registration_gates_remote_input() {
        let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, cfg_openh264()).expect("start");
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

    #[test]
    fn route_input_delivers_only_when_accepted() {
        let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, cfg_openh264()).expect("start");
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
