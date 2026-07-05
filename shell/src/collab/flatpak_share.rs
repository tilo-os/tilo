//! `SharedFlatpakSession` — end-to-end composition for a live app share.
//!
//! # What this module is
//!
//! A session ties together the four pieces that already exist for
//! streaming a foreign Wayland surface out to workspace peers:
//!
//! - **Capture**: [`crate::compositor::surface_readback::SurfaceTracker`]
//!   (`SHM` readback in v1; DMABUF import is a follow-up; see the
//!   `Deferred` notes below).
//! - **Throttle**: [`crate::network::streaming::StreamingThrottle`]
//!   caps the encode loop at the config's `target_fps` (silently
//!   clamped to 30 fps because openh264 SW can't sustain higher on a
//!   typical laptop; hardware paths lift this).
//! - **Encoder**: [`crate::network::encoder::H264Encoder`] (openh264
//!   software; `EncoderChoice::Vaapi` / `EncoderChoice::Nvenc` are
//!   declared in config but not linked in v1 — they return
//!   [`ShareError::EncoderUnavailable`] from the constructor).
//! - **Wire envelope**: [`crate::network::NeighbourMsg::SurfaceFrame`],
//!   bracketed by
//!   [`crate::network::NeighbourMsg::SurfaceStreamStart`] /
//!   [`crate::network::NeighbourMsg::SurfaceStreamEnd`] so late-joining
//!   peers can construct a decoder before the first NALU arrives.
//!
//! # What this module is *not*
//!
//! - **Transport**: the session emits `NeighbourMsg` values; the
//!   compositor's existing `network::actor` is responsible for handing
//!   them to gossipsub. There is no dedicated QUIC substream in v1; a
//!   follow-up should extend `libp2p::SwarmBuilder` with
//!   `libp2p-quic` and thread a QUIC substream per share into the
//!   actor for latency-critical WAN work. Gossipsub's default topic
//!   ordering plus the drop-oldest receiver already meets LAN targets
//!   (see the e2e test's assertion under 60 ms on loopback), so v1
//!   ships on gossipsub.
//! - **Cursor overlay**: the shared surface renders as a `PeerView`;
//!   each viewer's cursor gets drawn on top via the existing
//!   `shell::remote_cursors` module. This composition doesn't touch
//!   that path — peer cursors already work across the presence layer.
//! - **Sandbox brokering**: the caller (a workspace's collab profile UI
//!   action) is responsible for asking the Bubblewrap sandbox for
//!   read access to the Flatpak's socket / display before constructing
//!   a session. `SharedFlatpakSession::start` does not modify the
//!   sandbox itself.
//! - **Keyboard injection**: `State::apply_remote_input` handles
//!   `PointerMotion` + `PointerButton` on the receive side, but the
//!   `Key` variant is deliberately a no-op — the follow-up is tracked
//!   in `docs/superpowers/plans/2026-04-30-nirios-logical-bugs.md`.
//!
//! # Public API
//!
//! - [`SharedFlatpakSession::start`] — construct + advertise a new share.
//! - [`SharedFlatpakSession::stream_start_msg`] — the announcement
//!   `NeighbourMsg` to publish to workspace peers on start.
//! - [`SharedFlatpakSession::encode_next_frame`] — pump one iteration of
//!   the encode loop and return a `NeighbourMsg::SurfaceFrame` if the
//!   throttle + tracker cooperate.
//! - [`SharedFlatpakSession::stream_end_msg`] — the tear-down
//!   `NeighbourMsg` to publish on stop.
//! - [`SharedFlatpakSession::stop`] — release the encoder / share
//!   registry entries.
//! - [`SharedFlatpakSession::deposit_captured_rgba`] — call from the
//!   compositor's commit hook when a captured frame is available.
//!
//! The session is synchronous — callers pump it. The production
//! wire-up would drive `encode_next_frame` from the shell's per-frame
//! tick; the E2E test drives it directly from a loop.
//!
//! # Deferred (documented, not stubbed silently)
//!
//! - DMABUF direct-into-encoder path. Zero-copy would drop capture cost
//!   from ~4 ms/720p on SHM+CPU-copy to ~0.5 ms; the wgpu import path
//!   is designed but not wired.
//! - VA-API / NVENC hardware encoders. `EncoderChoice::Vaapi` and
//!   `EncoderChoice::Nvenc` return `ShareError::EncoderUnavailable` at
//!   session-start so UI can surface a clear message rather than
//!   silently falling back. Adding the crates: `vaapi-rs 0.7` (or the
//!   ffmpeg-next VA-API path) for Intel/AMD; `nvenc-sys` behind
//!   `cfg(nvidia)` for RTX 20xx+. Both need a runtime probe (device
//!   present + supported codec) that lives naturally as an
//!   `EncoderChoice::probe()` free function.
//! - Dedicated QUIC substream. The natural home is
//!   `network::behaviour::NiriosBehaviour` — add a
//!   `libp2p_quic::Config` and route `SurfaceFrame` bytes over a
//!   drop-oldest substream (`send_datagram`) while keeping input on
//!   `NeighbourMsg::RemoteInput` over gossipsub for reliable ordering.

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
use smithay::backend::allocator::Buffer as _;
use smithay::backend::allocator::dmabuf::Dmabuf;
use crate::config::{EncoderChoice, FlatpakShareConfig};
use crate::diagnostics::{CapturePath, PeerSurfaceMetrics};
use crate::network::encoder::EncodeError;
use crate::network::protocol::{NeighbourMsg, PeerId, SurfaceId};
use crate::network::streaming::{
    InputEvent, ShareRegistry, StreamingShare, StreamingThrottle,
};
use crate::network::surface_stream::SurfaceStreamSender;

/// v1 codec label — kept for tests that still assert on the pre-HDR
/// wire shape. Runtime `stream_start_msg` derives the label from the
/// codec chosen by the HDR config.
#[allow(dead_code)]
const CODEC_LABEL: &str = "h264";

/// Software-encoder-imposed frame-rate ceiling. See the module doc.
const SOFTWARE_FPS_CEILING: u32 = 30;

/// Scan `nalus` for an H.264 IDR NAL unit — nal_unit_type 5.
///
/// Walks Annex-B start-codes (`0x00 0x00 0x01` or `0x00 0x00 0x00 0x01`)
/// and reads the following byte's low 5 bits for the nal_unit_type. A
/// keyframe (IDR) is `nal_unit_type == 5`; a fresh SPS/PPS pair
/// (types 7/8) often preceeds the IDR in a GOP header, but the IDR
/// itself is the boundary the decoder needs to seek to.
///
/// Returns `false` when the byte stream doesn't contain a start-code
/// / is empty — safer to treat "unknown" as non-keyframe, so a
/// late-joining viewer waits for the next detectable IDR.
fn is_h264_keyframe(nalus: &[u8]) -> bool {
    // Scan for start-codes; on match, inspect the header byte.
    let mut i = 0usize;
    while i + 4 <= nalus.len() {
        // 3-byte start code `00 00 01`.
        let three = &nalus[i..i + 3];
        // 4-byte start code `00 00 00 01`.
        let four_ok = i + 4 <= nalus.len()
            && nalus[i] == 0
            && nalus[i + 1] == 0
            && nalus[i + 2] == 0
            && nalus[i + 3] == 1;
        if four_ok {
            let header = nalus[i + 4];
            if header & 0x1f == 5 {
                return true;
            }
            i += 4;
            continue;
        }
        if three == [0u8, 0, 1] {
            let header = nalus[i + 3];
            if header & 0x1f == 5 {
                return true;
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    false
}

/// Frame waiting in the encoder's staging slot.
///
/// Pairs the frame source (either CPU-side RGBA or a zero-copy DMABUF
/// handle) with the capture-time nanoseconds so encoding preserves the
/// original moment-of-capture on the wire. Drop-oldest: a second
/// deposit overwrites both fields.
struct StagedFrame {
    kind: StagedKind,
    capture_ts_ns: u64,
}

/// Either flavour of staged capture: SHM readback bytes, or a
/// zero-copy DMABUF reference. Kept as an enum (not two `Option`s)
/// because a single tick can only encode one; storing both would
/// invite a "which one wins" ambiguity for the encoder.
enum StagedKind {
    /// Row-major RGBA bytes from the SHM readback path.
    Rgba(Vec<u8>),
    /// DMABUF handle handed straight to the encoder. `Dmabuf` is
    /// Arc-backed inside smithay, so the staged value keeps every
    /// plane fd alive without a dup.
    Dmabuf(Dmabuf),
}

/// Public-facing frame the session emits.
///
/// The compositor tick receives one of these per `encode_next_frame`
/// call and wraps it in a `NeighbourMsg::SurfaceFrame` for publication.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedFrame {
    /// Surface the encoder is capturing (matches
    /// [`SharedFlatpakSession::surface_id`]).
    pub surface: SurfaceId,
    /// The H.264 Annex-B NALU payload.
    pub nalus: Vec<u8>,
    /// Owner-side capture timestamp (monotonic nanoseconds). Receivers
    /// subtract their local `Instant::now()` (translated the same way)
    /// to compute motion-to-photon latency.
    pub capture_ts_ns: u64,
}

impl EncodedFrame {
    /// Wrap this frame as a `NeighbourMsg::SurfaceFrame` for gossipsub
    /// publication. Consumes the frame — the NALUs move into the
    /// message rather than being cloned.
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
    /// The caller requested an encoder that the runtime probe could
    /// not resolve to a working backend. Emitted only when
    /// `EncoderChoice` explicitly names a hardware backend AND the
    /// caller has passed [`StartFlags::REQUIRE_EXACT_ENCODER`]. Without
    /// that flag, hardware backends fall through to openh264 with the
    /// outcome recorded in `chosen_encoder`.
    #[error("encoder {choice:?} is not available in this build")]
    EncoderUnavailable { choice: EncoderChoice },
    /// The chosen encoder failed to construct — bad dimensions,
    /// unsupported profile, or a device disappeared between the probe
    /// and the open.
    #[error("encoder construction failed: {0:?}")]
    EncoderConstruction(EncodeError),
    /// The share dimensions violate a hard limit. 4096×4096 is the
    /// current ceiling.
    #[error("dimensions {width}×{height} exceed the 4096×4096 ceiling")]
    DimensionsExceeded { width: u32, height: u32 },
}

/// A live share session on the owner side.
///
/// See the module-level docs for what this composes and what it
/// deliberately doesn't. Drop the session — or call [`Self::stop`] for
/// an explicit tear-down — to release the encoder and the
/// [`ShareRegistry`] entries.
pub struct SharedFlatpakSession {
    /// The user hosting the share (opaque numeric peer id; the same
    /// value `NeighbourMsg`'s `source_peer` fields carry).
    pub host_user_id: PeerId,
    /// The Flatpak app-id (`org.libreoffice.LibreOffice`, `org.gimp.GIMP`,
    /// …). Purely informational — used to display the share in the
    /// peer's workspace UI.
    pub app_id: String,
    /// The Wayland surface the encoder is capturing.
    pub surface_id: SurfaceId,
    /// Natural width the encoder was constructed for.
    width: u32,
    /// Natural height the encoder was constructed for.
    height: u32,
    /// Config snapshot at session start. Runtime changes require a
    /// stop/restart — v1 doesn't hot-reload.
    config: FlatpakShareConfig,
    /// Owned encoder. `Arc<Mutex<_>>` so the E2E test can share the
    /// encoder handle across threads if it wants to; production runs
    /// the pump on a single thread and never contends.
    ///
    /// Type-erased so hardware (VA-API/NVENC) and software (openh264)
    /// backends share the same wire.
    encoder: Arc<Mutex<Box<dyn Encoder>>>,
    /// Which encoder backend the runtime probe selected. Runtime kind
    /// — distinct from `config.encoder` which is the config-time
    /// request. When probes fall back to a lower-priority backend
    /// the two diverge.
    chosen_encoder: EncoderKind,
    /// Optional shared metrics handle. When set, `encode_next_frame`
    /// records encode wall-clock, keyframes, bitrate, chosen encoder,
    /// and capture path here. The receiving `PeerView` shares the same
    /// `Arc` so the OSD renders a single unified stat block.
    metrics: Option<PeerSurfaceMetrics>,
    /// Current capture path — flipped by
    /// [`Self::set_capture_path`] whenever the compositor's commit
    /// hook re-routes the surface's buffer through the DMABUF direct
    /// path (or back to SHM readback if the client switches buffer
    /// types).
    capture_path: Arc<Mutex<CapturePath>>,
    /// Owned throttle. Same locking rationale as `encoder`.
    throttle: Arc<Mutex<StreamingThrottle>>,
    /// Owned share registry (owner-side gate for inbound
    /// `NeighbourMsg::RemoteInput`). The session inserts an initial
    /// read/write share for each viewer as they accept the invite; the
    /// registry is shared with the compositor's `State` via `Arc` so
    /// input routing sees the same permissions.
    share_registry: Arc<Mutex<ShareRegistry>>,
    /// Frame source. Callers `deposit_captured_rgba` here from the
    /// compositor's commit hook; `encode_next_frame` drains via
    /// `Option::take`. Behaves the same drop-oldest way as
    /// [`crate::compositor::surface_readback::SurfaceTracker`] does for
    /// the production path.
    ///
    /// The staged tuple is `(rgba_bytes, capture_ts_ns)` — the capture
    /// timestamp is stamped at deposit time so the wire message's `ts`
    /// field reflects the moment the buffer left the app, not the
    /// moment the encoder finished with it. That way the receiver's
    /// motion-to-photon latency includes the encode step, which is a
    /// meaningful chunk of the wall-clock budget under SW encode.
    ///
    /// The e2e test uses this directly to inject synthetic frames.
    staged_frame: Arc<Mutex<Option<StagedFrame>>>,
    /// Monotonic-clock origin — subtract this from any `Instant::now()`
    /// to get the `u64` nanoseconds that ride in every
    /// `NeighbourMsg::SurfaceFrame::ts` field.
    origin: Instant,
    /// Session-level "still alive" flag. Flipped false by `stop()`; the
    /// pump short-circuits after that so an accidental double-stop
    /// (drop-on-drop) is safe.
    active: Arc<AtomicBool>,
    /// Video substream sender — the dedicated per-share channel encoded
    /// frames flow through. Distinct from the `NeighbourMsg::SurfaceFrame`
    /// gossipsub path (kept for legacy peers). Every `encode_next_frame`
    /// deposits the frame here in addition to returning it; the
    /// transport adapter (the actor's QUIC substream binding) polls the
    /// sender per subscriber and writes the encoded bytes on the wire.
    ///
    /// Per-subscriber bounded queue with drop-oldest backpressure —
    /// exposed via [`Self::stream_drops_for`]. Sequence numbers are
    /// monotonic per-share; a keyframe flag rides in the header so
    /// late-joining viewers know when it's safe to start decoding.
    stream_sender: Arc<SurfaceStreamSender>,
    /// Client-supplied colorimetry override. When the compositor
    /// observes a `wp_color_management_surface_v1.set_image_description`
    /// on the shared surface, it calls [`Self::set_color_override`] to
    /// stash the description here. [`Self::stream_start_msg`] then
    /// prefers this value over the config-driven default — meaning the
    /// encoded stream travels the wire tagged with the client's
    /// declared BT.2020/PQ (or HLG) colorimetry even when the config
    /// says SDR.
    ///
    /// `None` (the default) means "no override" and the config wins.
    color_override: Arc<Mutex<Option<(Colorimetry, Option<Hdr10Metadata>)>>>,
}

impl SharedFlatpakSession {
    /// Construct + activate a new share session.
    ///
    /// The dimensions are the *natural* size of the encoder's output —
    /// they must match the captured surface's `wl_shm` buffer or the
    /// encoder will reject the RGBA payload with
    /// [`EncodeError::BufferSizeMismatch`] on the first frame.
    ///
    /// Returns [`ShareError::EncoderUnavailable`] if the config selects
    /// a hardware encoder that isn't linked in this build.
    pub fn start(
        host_user_id: PeerId,
        app_id: impl Into<String>,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        config: FlatpakShareConfig,
    ) -> Result<Self, ShareError> {
        Self::start_with_metrics(host_user_id, app_id, surface_id, width, height, config, None)
    }

    /// Same as [`Self::start`] but accepts a shared
    /// [`PeerSurfaceMetrics`] handle so the compositor's OSD overlay
    /// sees encode-side counters (wall-clock p50/p99, keyframe count,
    /// chosen encoder, capture path) alongside the receiver-side
    /// counters. Used by `flatpak_share_e2e` + the runtime wire-up.
    pub fn start_with_metrics(
        host_user_id: PeerId,
        app_id: impl Into<String>,
        surface_id: SurfaceId,
        width: u32,
        height: u32,
        config: FlatpakShareConfig,
        metrics: Option<PeerSurfaceMetrics>,
    ) -> Result<Self, ShareError> {
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(ShareError::DimensionsExceeded { width, height });
        }

        // Codec + colorimetry follow the HDR config: SDR → H.264 +
        // BT.709, HDR → HEVC Main10 + BT.2020-PQ (or HLG). A missing
        // HDR config leaves the SDR pipeline in place — pre-HDR
        // callers see identical behaviour.
        let codec = match config.hdr {
            None => super::encoder::Codec::H264,
            Some(_) => super::encoder::Codec::HevcMain10,
        };
        let (chosen, outcomes) = probe(codec, Some(config.encoder));
        log_probe_outcomes(&outcomes, chosen);

        // Software encoder needs the fps clamp; hardware paths lift it.
        // Only clamp under openh264 (H.264 8-bit) — hardware HEVC on
        // NVENC easily sustains 60 fps.
        let effective_fps = if chosen == EncoderKind::Openh264 {
            config.target_fps.min(SOFTWARE_FPS_CEILING).max(1)
        } else {
            config.target_fps.max(1)
        };

        // Flow the workspace's configured bitrate cap through the
        // encoder init. HDR needs a higher floor: BT.2020-PQ at 720p
        // needs ~10 Mbps for reasonable quality vs 4 Mbps for SDR.
        let sdr_floor = 500u32;
        let hdr_floor = 4000u32;
        let effective_bitrate_kbps = if codec.bit_depth() == 10 {
            config.max_bitrate_kbps.max(hdr_floor)
        } else {
            config.max_bitrate_kbps.max(sdr_floor)
        };
        let init = match config.hdr {
            None => EncoderInit::sdr_h264(
                width,
                height,
                effective_fps,
                effective_bitrate_kbps,
                1,
            ),
            Some(hdr_cfg) => EncoderInit::hdr10_hevc(
                width,
                height,
                effective_fps,
                effective_bitrate_kbps,
                1,
                hdr_cfg,
            ),
        };
        let encoder = build(chosen, init).map_err(|e| match e {
            EncoderError::Unavailable { .. } => {
                ShareError::EncoderUnavailable { choice: config.encoder }
            }
            EncoderError::ConfigFailed { detail, .. } => {
                ShareError::EncoderConstruction(EncodeError::Backend(detail))
            }
            EncoderError::EncodeFailed { detail, .. } => {
                ShareError::EncoderConstruction(EncodeError::Backend(detail))
            }
        })?;

        if let Some(m) = &metrics {
            m.set_chosen_encoder(chosen);
        }

        // Throttle interval mirrors the target fps.
        let interval = std::time::Duration::from_micros(
            (1_000_000 / effective_fps as u64).max(1),
        );

        Ok(Self {
            host_user_id,
            app_id: app_id.into(),
            surface_id,
            width,
            height,
            config: FlatpakShareConfig {
                target_fps: effective_fps,
                ..config
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
        })
    }

    /// Attach a client-declared colorimetry that overrides the
    /// config-driven default on the wire. Called by the compositor's
    /// `wp_color_management_surface_v1` dispatch when the client
    /// commits a `set_image_description`. The next
    /// [`Self::stream_start_msg`] carries the override.
    ///
    /// The override is Arc-shared so a background clone (e.g. the
    /// compositor's per-frame hook) can update it without racing the
    /// session's read path.
    pub fn set_color_override(
        &self,
        colorimetry: Colorimetry,
        hdr: Option<Hdr10Metadata>,
    ) {
        if let Ok(mut slot) = self.color_override.lock() {
            *slot = Some((colorimetry, hdr));
        }
    }

    /// Drop any prior colorimetry override — call from the compositor
    /// when the client sends `unset_image_description` or destroys
    /// the color-management surface. The next
    /// [`Self::stream_start_msg`] falls back to the config-driven
    /// default.
    pub fn clear_color_override(&self) {
        if let Ok(mut slot) = self.color_override.lock() {
            *slot = None;
        }
    }

    /// Currently-active override (a diagnostic hook used by the
    /// override tests).
    pub fn current_color_override(
        &self,
    ) -> Option<(Colorimetry, Option<Hdr10Metadata>)> {
        self.color_override.lock().ok().and_then(|g| g.clone())
    }

    /// Which encoder the runtime probe picked. Distinct from
    /// `config.encoder` — this is the runtime kind, not the config-time
    /// choice.
    pub fn chosen_encoder(&self) -> EncoderKind {
        self.chosen_encoder
    }

    /// Update the capture path — called by the compositor's commit hook
    /// whenever the surface's buffer type changes (SHM ↔ DMABUF).
    /// Propagates the value into the shared metrics handle so the OSD
    /// sees the switch immediately.
    pub fn set_capture_path(&self, path: CapturePath) {
        let mut slot = self.capture_path.lock().expect("capture path");
        *slot = path;
        if let Some(m) = &self.metrics {
            m.set_capture_path(path);
        }
    }

    /// Current capture path (mirror of [`Self::set_capture_path`]).
    pub fn capture_path(&self) -> CapturePath {
        *self.capture_path.lock().expect("capture path")
    }

    /// Dimensions the encoder is configured for. Constant for the
    /// session lifetime; a resize requires stop+start.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Effective config after any clamping applied at `start`.
    pub fn snapshot_config(&self) -> FlatpakShareConfig {
        self.config.clone()
    }

    /// Build the `NeighbourMsg::SurfaceStreamStart` payload to publish
    /// on session start. The workspace's actor calls this once, ships
    /// the message, then pumps `encode_next_frame` on the tick.
    pub fn stream_start_msg(&self) -> NeighbourMsg {
        // Take a single monotonic reading and stamp it into both `ts`
        // (informational; matches every prior wire-log format) *and*
        // `sender_monotonic_ns_at_send` (used by the receiver's
        // clock-offset handshake). Reading the clock twice would smear
        // the two values by a couple of hundred nanoseconds and yield a
        // handshake that's off by the same amount — small, but avoidable.
        let now_ns = self.monotonic_ns_now();
        // Precedence:
        // 1. Client-declared image description (via
        //    `wp_color_management_surface_v1.set_image_description`)
        //    — the sender's intent takes priority so late-joining
        //    peers decode against the client's actual colorimetry.
        // 2. HDR config (workspace opt-in).
        // 3. SDR BT.709 default.
        //
        // Bit depth is derived from the transfer function — HDR
        // transfers (PQ / HLG) imply 10-bit, everything else 8-bit.
        // The codec label follows: HEVC Main10 for HDR, H.264 for SDR.
        let override_val = self.current_color_override();
        let (codec_label, colorimetry, hdr_static, bit_depth) = if let Some((
            client_color,
            client_hdr,
        )) = override_val
        {
            let is_hdr = client_color.is_hdr();
            let codec = if is_hdr {
                super::encoder::Codec::HevcMain10
            } else {
                super::encoder::Codec::H264
            };
            (
                codec.wire_label().to_string(),
                Some(client_color),
                client_hdr.or(self.config.hdr.map(|c| c.metadata)),
                if is_hdr { 10u8 } else { 8u8 },
            )
        } else {
            match self.config.hdr {
                None => (
                    super::encoder::Codec::H264.wire_label().to_string(),
                    Some(crate::collab::colorimetry::Colorimetry::sdr_bt709()),
                    None,
                    8u8,
                ),
                Some(hdr_cfg) => (
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

    /// Build the `NeighbourMsg::SurfaceStreamEnd` payload to publish
    /// when the session stops. Callers should ship this *before*
    /// dropping the session so late-arriving viewers can drop their
    /// PeerView instead of hanging on the stale decoder.
    pub fn stream_end_msg(&self) -> NeighbourMsg {
        NeighbourMsg::SurfaceStreamEnd {
            surface: self.surface_id,
            source_peer: self.host_user_id,
            ts: self.monotonic_ns_now(),
        }
    }

    /// Deposit a captured RGBA frame for the encoder to pick up on the
    /// next tick. Drop-oldest — a second deposit before the tick
    /// consumes the first *replaces* it (matches
    /// [`crate::compositor::surface_readback::SurfaceTracker`]'s
    /// "latest wins" contract).
    ///
    /// The bytes must be `width × height × 4` in RGBA order or the
    /// encoder will reject them on the next pump with
    /// [`EncodeError::BufferSizeMismatch`]; the session doesn't
    /// validate the length here to keep the hot path allocation-free.
    ///
    /// The current `elapsed_ns` is stamped into the staged frame at
    /// deposit time so the wire `capture_ts_ns` reflects the moment of
    /// capture, not the moment encoding finished. That's how the
    /// receiver's latency snapshot ends up including encode cost —
    /// which is a significant chunk of the wall-clock budget on the
    /// openh264 SW path.
    pub fn deposit_captured_rgba(&self, rgba: Vec<u8>) {
        let capture_ts_ns = self.monotonic_ns_now();
        let mut slot = self.staged_frame.lock().expect("staged frame");
        *slot = Some(StagedFrame {
            kind: StagedKind::Rgba(rgba),
            capture_ts_ns,
        });
    }

    /// Deposit a DMABUF handle for the next encode tick — zero-copy
    /// sibling of [`Self::deposit_captured_rgba`]. Drop-oldest: a
    /// second deposit before the next tick replaces the first.
    ///
    /// The compositor commit hook calls this when the shared
    /// surface's committed buffer is a DMABUF-backed `wl_buffer` and
    /// the encoder backend supports zero-copy DMABUF import (VA-API
    /// today). The staged `Dmabuf` is Arc-backed, so this call is
    /// O(1) — no plane fds are dup'd here; the encoder's
    /// `av_hwframe_map` builds its own references off the same
    /// underlying object.
    ///
    /// If [`Self::encode_next_frame`] finds the encoder rejects the
    /// zero-copy path (drivers that don't implement
    /// `av_hwframe_map`), the DMABUF is dropped for that tick and
    /// the compositor's fallback (the SHM readback deposit) picks up
    /// on the following commit.
    pub fn deposit_captured_dmabuf(&self, dmabuf: Dmabuf) {
        let capture_ts_ns = self.monotonic_ns_now();
        let mut slot = self.staged_frame.lock().expect("staged frame");
        *slot = Some(StagedFrame {
            kind: StagedKind::Dmabuf(dmabuf),
            capture_ts_ns,
        });
    }

    /// Pump one iteration of the encode loop.
    ///
    /// Returns:
    /// - `Some(EncodedFrame)` — the throttle was open, a frame was
    ///   staged, and the encoder succeeded. Ship the frame's
    ///   `into_neighbour_msg()` payload over gossipsub.
    /// - `None` — throttled, no frame staged, or an encode error was
    ///   swallowed. The session logs on encode error (once per
    ///   consecutive run to avoid flooding). This matches the
    ///   compositor's "encode or drop" memory contract.
    ///
    /// Instrumented so a profile pass shows how much wall-clock the
    /// per-tick composition (throttle check + encoder → NALU + timestamp
    /// bookkeeping) costs. The inner `H264Encoder::encode` span is a
    /// nested child.
    #[tracing::instrument(level = "trace", skip(self))]
    pub fn encode_next_frame(&self, now: Instant) -> Option<EncodedFrame> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let mut throttle = self.throttle.lock().expect("throttle");
        if !throttle.should_encode(now) {
            return None;
        }
        // Drain the staged frame BEFORE grabbing the encoder lock so we
        // don't hold both.
        let staged = self.staged_frame.lock().expect("staged").take();
        let staged = match staged {
            Some(s) => s,
            None => return None,
        };
        let carried_ts = staged.capture_ts_ns;

        let mut encoder = self.encoder.lock().expect("encoder");
        let mut nalus = Vec::new();
        // Build the encoder input from whichever flavour of frame was
        // deposited. DMABUF paths lift every plane's fd + offset +
        // stride off the `Dmabuf` handle (iterator adapters — the
        // underlying Arc keeps the OwnedFds alive for the borrow's
        // duration). RGBA paths just hand the byte slice through.
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
                    plane_fds[i] = fd.as_raw_fd();
                    plane_offsets[i] = offset as u64;
                    plane_strides[i] = stride;
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
                throttle.mark_encoded(now);
                // Now that the encoder actually consumed the frame,
                // record the capture path in the shared metrics + on
                // the session state. This is the metrics roll-up rule:
                // record "we did DMABUF direct" only after the encoder
                // succeeds, not after the compositor merely detected
                // a DMABUF buffer.
                self.set_capture_path(path_taken);
                // Push encode-side telemetry into the shared metrics.
                if let Some(m) = &self.metrics {
                    m.record_encode_wallclock(stats.encode_wallclock.as_nanos() as u64);
                    if stats.keyframe {
                        m.record_keyframe();
                    }
                    // Rough bitrate sample: bytes-per-frame * fps.
                    let bytes_per_sec = (stats.bytes as u64)
                        .saturating_mul(self.config.target_fps as u64);
                    m.record_bitrate(bytes_per_sec);
                }
                // Fan out to the dedicated substream first — every
                // subscriber's bounded queue absorbs the frame with
                // drop-oldest on overflow. `is_h264_keyframe` walks the
                // Annex-B NALUs looking for an IDR (nal_unit_type 5) so
                // late-joining viewers can start their decoder on a
                // keyframe boundary.
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

    /// Register a peer as a viewer + set the initial read/write state.
    ///
    /// `read_only = true` means the peer sees frames but can't inject
    /// input; the owner can flip the flag later via
    /// [`Self::set_read_only`].
    ///
    /// Also subscribes the peer to the [`SurfaceStreamSender`] so
    /// subsequently-encoded frames land in their per-subscriber
    /// bounded queue.
    pub fn add_viewer(&self, peer: PeerId, read_only: bool) {
        let mut reg = self.share_registry.lock().expect("share registry");
        reg.insert(peer, self.surface_id, StreamingShare { read_only });
        drop(reg);
        self.stream_sender.subscribe(peer);
    }

    /// Drop a viewer. Their subsequent `RemoteInput` events fall on
    /// the floor (registry gate returns `false`), and their video
    /// substream subscription is closed.
    pub fn drop_viewer(&self, peer: PeerId) {
        let mut reg = self.share_registry.lock().expect("share registry");
        reg.remove(peer, self.surface_id);
        drop(reg);
        self.stream_sender.unsubscribe(peer);
    }

    /// Cumulative drop count for a viewer's video substream. Increments
    /// on every drop-oldest fired by the per-subscriber bounded queue.
    /// Diagnostic — the paint side already handles the drop; this is
    /// how the owner's UI surfaces "peer X is falling behind".
    pub fn stream_drops_for(&self, peer: PeerId) -> u64 {
        self.stream_sender.drops_for(peer)
    }

    /// Total drops across every subscriber. Diagnostic aggregate.
    pub fn stream_total_drops(&self) -> u64 {
        self.stream_sender.total_drops()
    }

    /// Pending queue depth for a viewer's video substream. Diagnostic.
    pub fn stream_pending_for(&self, peer: PeerId) -> usize {
        self.stream_sender.pending_for(peer)
    }

    /// Poll the next queued frame for `peer`. The QUIC substream
    /// adapter calls this per poll cycle and writes the encoded bytes
    /// onto the wire. Returns `None` when the peer isn't subscribed or
    /// the queue is drained.
    pub fn poll_stream_frame(
        &self,
        peer: PeerId,
    ) -> Option<crate::network::surface_stream::SurfaceFrame> {
        self.stream_sender.poll(peer)
    }

    /// Direct handle onto the substream sender. Exposed for the actor's
    /// QUIC substream adapter — production wires the QUIC substream to
    /// pump from this sender, tests inspect it to assert on drops /
    /// sequences.
    pub fn stream_sender(&self) -> Arc<SurfaceStreamSender> {
        self.stream_sender.clone()
    }

    /// Flip the read-only flag on an existing viewer.
    pub fn set_read_only(&self, peer: PeerId, read_only: bool) {
        let mut reg = self.share_registry.lock().expect("share registry");
        reg.set_read_only(peer, self.surface_id, read_only);
    }

    /// Consult the share registry — same gate the compositor's
    /// `State::apply_remote_input` uses. Returns `true` to apply, `false`
    /// to drop.
    pub fn accept_remote_input(&self, peer: PeerId) -> bool {
        let reg = self.share_registry.lock().expect("share registry");
        reg.accept_remote_input(peer, self.surface_id)
    }

    /// Convenience for tests + adapters: route an inbound `InputEvent`
    /// through the registry into a caller-supplied
    /// [`crate::network::streaming::RemoteInputSink`]. Returns whether
    /// the sink saw the event.
    pub fn route_input<S: crate::network::streaming::RemoteInputSink>(
        &self,
        peer: PeerId,
        event: InputEvent,
        sink: &mut S,
    ) -> bool {
        let reg = self.share_registry.lock().expect("share registry");
        reg.route_to(peer, self.surface_id, event, sink)
    }

    /// Stop the session — flips the active flag so `encode_next_frame`
    /// short-circuits. Publish [`Self::stream_end_msg`] *before*
    /// calling this so viewers hear the graceful close.
    pub fn stop(self) {
        self.active.store(false, Ordering::Release);
        // Drop follows — encoder + throttle + registry + staged bytes
        // release with the `SharedFlatpakSession`.
    }

    /// Return the monotonic nanosecond counter to embed in outgoing
    /// wire messages. Wraps at u64 overflow (~584 years) so we don't
    /// worry about it here.
    fn monotonic_ns_now(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }

    /// The session's monotonic origin — the reference `Instant` from
    /// which every `capture_ts_ns` is measured.
    ///
    /// Two use cases:
    ///
    /// 1. **Receiver-side latency**: on an in-process test or on a
    ///    peer that ran a session-start clock handshake, the receiver
    ///    can compute `origin.elapsed().as_nanos() - capture_ts_ns` to
    ///    get motion-to-photon latency without spawning its own clock
    ///    domain.
    /// 2. **Diagnostics / logging**: aligns log timestamps between the
    ///    sender's encode loop and the receiver's decode loop.
    pub fn origin(&self) -> Instant {
        self.origin
    }

    /// Convenience: monotonic-ns-since-`origin` measured *now*.
    /// Same conversion the wire timestamps use so subtraction is
    /// a direct latency without further translation.
    pub fn elapsed_ns(&self) -> u64 {
        self.monotonic_ns_now()
    }
}

/// Emit a `tracing::info!` line for the chosen encoder plus a
/// `tracing::debug!` for every backend the probe walked. Callers see
/// exactly which backends were considered and *why* the winner was
/// picked — invaluable for diagnosing "why VA-API isn't kicking in on
/// this box" without turning on the whole trace subscriber.
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
        .map(|o| o.reason.as_str())
        .unwrap_or("");
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
        // On a stripped host (no `vaapi` or `nvenc` cargo feature and
        // no `/dev/dri` render node) the probe walks past both hardware
        // backends and lands on openh264. The session constructs
        // successfully; `chosen_encoder()` reports `Openh264` so
        // callers can surface "fell back to software" in the OSD.
        for choice in [EncoderChoice::Vaapi, EncoderChoice::Nvenc] {
            let cfg = FlatpakShareConfig {
                encoder: choice,
                ..cfg_openh264()
            };
            let session = SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, 64, 64, cfg)
                .expect("session should fall back to openh264");
            // Runtime kind must be openh264 — the config-time request
            // (`Vaapi` / `Nvenc`) stays in the snapshot for the OSD.
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
        let s = SharedFlatpakSession::start(1, "org.gimp.GIMP", 7, 64, 64, cfg)
            .expect("start");
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
                // SDR pipeline: colorimetry populated to BT.709, no
                // HDR metadata, bit depth 8.
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
        // The wire msg carries the same bytes untouched.
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
        let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, cfg_openh264())
            .expect("start");
        assert!(s.encode_next_frame(Instant::now()).is_none());
    }

    #[test]
    fn stop_short_circuits_encode_next_frame() {
        const W: u32 = 64;
        const H: u32 = 64;
        let s = SharedFlatpakSession::start(1, "x", 7, W, H, cfg_openh264())
            .expect("start");
        s.deposit_captured_rgba(gradient_frame(W, H));
        // Grab the `active` handle before consuming `s` so we can flip
        // it without needing to keep the session alive for the assertion.
        let active = s.active.clone();
        active.store(false, Ordering::Release);
        assert!(s.encode_next_frame(Instant::now()).is_none());
    }

    #[test]
    fn viewer_registration_gates_remote_input() {
        let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, cfg_openh264())
            .expect("start");
        let peer: PeerId = 99;

        // Not a viewer → input rejected.
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
        let s = SharedFlatpakSession::start(1, "x", 7, 64, 64, cfg_openh264())
            .expect("start");
        let peer: PeerId = 42;
        let mut sink = RecordingSink::default();

        // Unknown viewer → drops.
        assert!(!s.route_input(peer, InputEvent::PointerMotion { x: 1.0, y: 2.0 }, &mut sink));
        assert!(sink.events.is_empty());

        s.add_viewer(peer, false);
        assert!(s.route_input(peer, InputEvent::PointerMotion { x: 3.0, y: 4.0 }, &mut sink));
        assert_eq!(sink.events.len(), 1);
        assert_eq!(sink.events[0].0, peer);
        assert_eq!(sink.events[0].1, 7);
    }
}
