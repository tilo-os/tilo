//! Peer-side foreign-surface view — the receive-side counterpart to the
//! owner's encode loop in [`crate::network::streaming`]. Owns one
//! [`tilo_callstream::surface_decoder::VideoDecoder`] for the stream's codec
//! and paints the most recently decoded frame as a vello image draw.
//!
//! [`PeerView::push_encoded`] decodes a [`crate::network::NeighbourMsg::SurfaceFrame`]
//! synchronously and stashes the latest frame for the next `draw`; older
//! frames are dropped — no queue, mirroring the encode side's
//! "encode or drop" contract.
//!
//! Pointer/keyboard events go into an internal queue via `record_*`;
//! the shell's per-frame tick drains [`PeerView::take_pending_inputs`]
//! and ships each as a [`crate::network::NeighbourMsg::RemoteInput`].

use std::collections::HashMap;
use std::sync::Arc;

use taffy::{NodeId, TaffyTree};
use tilo_callstream::surface_decoder::{DecodedFrame, DecodedPixels, VideoDecoder};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use ui::Theme;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

use crate::collab::colorimetry::Colorimetry;
use crate::diagnostics::PeerSurfaceMetrics;
use crate::network::protocol::{PeerId, SurfaceId};
use crate::network::streaming::InputEvent;
use crate::shell::color_pipeline::{OutputColorState, SurfaceColorOp, plan_surface_op};
use crate::shell::hdr_tonemap::{HdrTonemapConfig, tonemap_p010_to_rgba8};
use ui::TextRole;

/// Ceiling on the receive-side decode queue depth; once undrawn frames
/// exceed this, further pushes drop the oldest. Two is the minimum
/// useful ceiling — one paint-in-flight, one staged for the next frame.
pub(crate) const PENDING_CEILING: u32 = 2;

/// How long a push waits for its picture: not at all, not even for the
/// decoder to start. The view is pushed on the compositor loop, which must
/// never stall on a peer's decode (a wait here held input, every other
/// window and presentation for up to 150 ms per slow frame, and up to 5 s
/// while a stream's decoder started). The picture comes out of a later push or
/// [`PeerView::collect_late_picture`], once the decoder's wake (see
/// [`PeerView::set_picture_wake`]) or the poll below brings the loop back.
const PICTURE_WAIT: std::time::Duration = std::time::Duration::ZERO;
/// How often the loop looks for a picture that outlasted its push, when no
/// wake tells it.
const LATE_PICTURE_POLL: std::time::Duration = std::time::Duration::from_millis(4);
/// How long after a push it keeps looking: a decoder that holds a picture
/// back until the next unit must not keep an idle desktop awake.
const LATE_PICTURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(1);

/// How long a view waiting for a keyframe waits before asking again, in
/// case its request or the keyframe answering it was lost too.
const KEYFRAME_REQUEST_RETRY: std::time::Duration = std::time::Duration::from_millis(300);

/// `decoder`, set up for the compositor loop: it never waits for a picture.
fn loop_decoder(mut decoder: VideoDecoder) -> VideoDecoder {
    decoder.set_picture_wait(PICTURE_WAIT);
    decoder
}

/// Convert a mastering-luminance value (`1/10000 cd/m²`, per the HDR10
/// stream metadata encoding) to nits. Stream values top out around
/// `40_000_000` (4000 cd/m²), far inside `f32`'s 24-bit exact-integer
/// range, so the precision loss this cast could otherwise cause cannot
/// happen for real content, and the tone-mapper only needs an
/// approximate peak anyway. Same derivation as
/// `hdr_tonemap::mastering_luminance_to_nits`, kept local rather than
/// shared since that one is private to its own module.
#[allow(
    clippy::cast_precision_loss,
    reason = "mastering luminance stays well under 2^24 for real streams; exact precision isn't needed for a tone-map peak"
)]
fn mastering_luminance_to_nits(raw: u32) -> f32 {
    raw as f32 / 10_000.0
}

/// Outcome of a `push_encoded_with_latency` call, for structured logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Decoded and became the new latest frame.
    Decoded,
    /// Decoded, but the pending-queue ceiling was hit and the oldest
    /// frame was dropped; `frames_dropped` incremented.
    DroppedOldest,
    /// The decoder took the payload, but its picture outlasted the
    /// decoder's wait (a loaded machine). Nothing recorded yet: the picture
    /// arrives with a later push or [`PeerView::collect_late_picture`].
    Pending,
    /// The decoder rejected the payload. Nothing recorded, and the units
    /// after it wait for a keyframe like [`Self::AwaitingKeyframe`].
    DecodeFailed,
    /// A delta unit whose reference chain is broken: a unit before it never
    /// arrived, or the stream has not reached its first keyframe. It is not
    /// decoded, since a decoder fed it shows corrupt pictures or rejects
    /// every delta until the next keyframe anyway.
    AwaitingKeyframe,
    /// A delta older than one already decoded, overtaken on the way or
    /// repeated. Not decoded; the chain it belonged to has moved on.
    Stale,
}

/// Frames of streams that have no view yet, by `(source peer, surface)`.
///
/// A stream's start and its first frames travel separately, and the first
/// frame is the keyframe a decoder must begin at. A frame that overtakes the
/// start is held here and decoded as soon as the view mounts, instead of the
/// viewer waiting for the next keyframe. Bounded: the first frames of a few
/// streams, and nothing larger than a frame on the wire. Each frame keeps its
/// sequence number, so frames past the bound read as a gap to the view.
#[derive(Default)]
pub struct EarlyFrames {
    held: HashMap<(u64, u64), Vec<HeldFrame>>,
}

/// A held frame's sequence number and encoded bytes.
pub type HeldFrame = (u64, Vec<u8>);

impl EarlyFrames {
    const STREAMS: usize = 8;
    const FRAMES: usize = 4;

    /// Keep `frame` of a stream with no view, in arrival order.
    pub fn hold(&mut self, key: (u64, u64), seq: u64, frame: Vec<u8>) {
        if !self.held.contains_key(&key) && self.held.len() >= Self::STREAMS {
            return;
        }
        let frames = self.held.entry(key).or_default();
        if frames.len() < Self::FRAMES {
            frames.push((seq, frame));
        }
    }

    /// The frames held for a stream whose view just mounted, with their
    /// sequence numbers.
    pub fn take(&mut self, key: (u64, u64)) -> Vec<HeldFrame> {
        self.held.remove(&key).unwrap_or_default()
    }

    /// Drop a stream that ended.
    pub fn forget(&mut self, key: (u64, u64)) {
        self.held.remove(&key);
    }
}

/// A peer's announcement that one of its surfaces is streaming: which
/// surface, which app's tab shows it, how it is encoded, and where input on
/// the local view goes back to.
#[derive(Clone)]
pub struct PeerStreamStart {
    pub source_peer: u64,
    pub surface: u64,
    pub local_peer: u64,
    pub app_id: String,
    pub codec: String,
    /// The size the sender announced; decoded pictures are cropped to it.
    pub display_size: Option<(u32, u32)>,
    pub hdr_static_metadata: Option<crate::collab::hdr_metadata::Hdr10Metadata>,
    pub input_sink: Arc<dyn Fn(InputEvent) + Send + Sync>,
    /// Asks the sender for a keyframe, naming the unit the view could not
    /// decode; see [`PeerView::set_keyframe_sink`].
    pub keyframe_sink: Arc<dyn Fn(u64) + Send + Sync>,
    /// The workspace tab the sender shows the window in; `0` when it names
    /// none and the view goes to the first free tab of the app.
    pub tab: u64,
}

/// Peer-side widget rendering one shared surface: the decoder, the latest
/// decoded frame as a peniko image, and the pending-input queue.
pub struct PeerView {
    /// Cached `AppWidget::app_title` result, resolved on first use so
    /// building a view never waits on the catalogue.
    title: std::sync::OnceLock<String>,
    /// Surface id the owner uses to address this share.
    surface_id: SurfaceId,
    /// Local peer id (the *viewer*) — the value the owner sees as the
    /// `peer` field on `RemoteInput`.
    local_peer: PeerId,
    /// Active decoder for the stream's codec. SDR H.264 by default so
    /// legacy peers without a codec label still work.
    decoder: VideoDecoder,
    /// Most recent decoded frame, ready to paint.
    latest: Option<DecodedFrame>,
    /// HDR configuration snapshot from the stream-start message. `None`
    /// on SDR streams; drives the tone-map decision on
    /// [`Self::ensure_image`].
    hdr_tonemap: Option<HdrTonemapConfig>,
    /// Cached `peniko::ImageData` derived from `latest`; rebuilt only
    /// when `latest` changes.
    cached_image: Option<vello::peniko::ImageData>,
    /// `latest` at full precision when it carries an HDR signal; composited
    /// in place of `cached_image` where the output shows HDR.
    cached_hdr: Option<Arc<crate::shell::hdr_surface::HdrPlane>>,
    /// Pending input events to ship back to the owner.
    pending_inputs: Vec<InputEvent>,
    /// Production sends directly through this non-blocking bridge; tests
    /// without one keep using `take_pending_inputs`.
    input_sink: Option<Arc<dyn Fn(InputEvent) + Send + Sync>>,
    pointer_inside: bool,
    /// Click-derived keyboard focus. Pointer hover is deliberately separate:
    /// moving out of a text field must not destroy its caret or IME session.
    keyboard_focused: bool,
    rect: Rect,
    /// True when the view needs to repaint; without this, the default
    /// leaf [`Widget::needs_redraw`] returns `true` every frame,
    /// defeating the App's redraw-only-when-dirty gate.
    dirty: bool,
    /// Shared observability counters — the OSD overlay and the e2e test
    /// can read them without touching the widget.
    metrics: PeerSurfaceMetrics,
    /// Decoded frames the shell hasn't yet pulled through the paint
    /// loop. Once this exceeds [`PENDING_CEILING`], the next push drops
    /// the oldest.
    frames_pending: u32,
    /// Until when the loop wakes to collect a picture still decoding after
    /// its push returned; see [`LATE_PICTURE_WINDOW`].
    late_poll_until: Option<ui::time::Instant>,
    /// Wakes the loop when a picture comes out; see [`Self::set_picture_wake`].
    picture_wake: Option<media::realtime::PictureWake>,
    /// The newest push whose picture is still decoding: its motion-to-photon
    /// latency then, and when, so a late picture's latency counts the wait.
    pending_push: Option<(Option<u64>, ui::time::Instant)>,
    /// Which units continue the decoder's reference chain.
    chain: neighbour_wire::unit_chain::UnitChain,
    /// Asks the sender for a keyframe once the chain broke.
    keyframe_sink: Option<Arc<dyn Fn(u64) + Send + Sync>>,
    /// When the view last asked for a keyframe.
    keyframe_requested: Option<ui::time::Instant>,
}

impl PeerView {
    fn content_rect(&self) -> Rect {
        let Some(frame) = self.latest.as_ref() else {
            return self.rect;
        };
        aspect_fit_rect(self.rect, frame.width, frame.height)
    }
    /// A fresh metrics handle is created — see [`Self::new_with_metrics`]
    /// to share one instead.
    pub fn new(surface_id: SurfaceId, local_peer: PeerId) -> Self {
        Self::new_with_metrics(surface_id, local_peer, PeerSurfaceMetrics::default())
    }

    /// Same as [`Self::new`] but with a caller-supplied metrics handle,
    /// so an OSD overlay or e2e test can observe drop/latency counters.
    pub fn new_with_metrics(
        surface_id: SurfaceId,
        local_peer: PeerId,
        metrics: PeerSurfaceMetrics,
    ) -> Self {
        Self {
            title: std::sync::OnceLock::new(),
            surface_id,
            local_peer,
            decoder: {
                let mut d = loop_decoder(VideoDecoder::h264());
                d.set_stream_colorimetry(Some(Colorimetry::sdr_bt709()));
                d
            },
            latest: None,
            hdr_tonemap: None,
            cached_image: None,
            cached_hdr: None,
            pending_inputs: Vec::new(),
            input_sink: None,
            pointer_inside: false,
            keyboard_focused: false,
            rect: Rect::ZERO,
            dirty: true,
            metrics,
            frames_pending: 0,
            late_poll_until: None,
            picture_wake: None,
            pending_push: None,
            chain: neighbour_wire::unit_chain::UnitChain::default(),
            keyframe_sink: None,
            keyframe_requested: None,
        }
    }

    /// Picks the decoder from `SurfaceStreamStart.codec` (`"h264"`,
    /// `"vp9"`, `"av1"` SDR; `"hevc-main10"` HDR). Unknown labels fall back
    /// to SDR H.264 rather than the share silently vanishing.
    pub fn new_for_stream(
        surface_id: SurfaceId,
        local_peer: PeerId,
        codec: &str,
        metrics: PeerSurfaceMetrics,
        display_caps: &crate::shell::hdr_display::DisplayCapabilities,
        hdr_static_metadata: Option<crate::collab::hdr_metadata::Hdr10Metadata>,
    ) -> Self {
        // HDR streams announce their static metadata; they decode to 10-bit.
        let bit_depth = if hdr_static_metadata.is_some() { 10 } else { 0 };
        let decoder = VideoDecoder::for_stream(codec, bit_depth).unwrap_or_else(|e| {
            tracing::warn!(
                surface = surface_id,
                codec = codec,
                error = %e,
                "peer view falling back to H264 SDR decoder"
            );
            VideoDecoder::h264()
        });
        // Tone-map only runs HDR-into-SDR-output.
        let stream_colorimetry = if decoder.is_ten_bit() {
            Colorimetry::hdr10()
        } else {
            Colorimetry::sdr_bt709()
        };
        // What the sender announces for these streams; the bitstream may
        // not say (see `VideoDecoder::set_stream_colorimetry`).
        let mut decoder = loop_decoder(decoder);
        decoder.set_stream_colorimetry(Some(stream_colorimetry));
        let output_state = OutputColorState::from_display(display_caps);
        // Same derivation as `HdrTonemapConfig::from_metadata`.
        let source_peak = hdr_static_metadata
            .map(|m| mastering_luminance_to_nits(m.mastering.max_display_mastering_luminance))
            .filter(|nits| nits.is_finite() && *nits > 0.0);
        let hdr_tonemap = match plan_surface_op(stream_colorimetry, &output_state, source_peak) {
            SurfaceColorOp::TonemapHdrToSdr { config } => Some(config),
            // These upload without a CPU tone-map.
            SurfaceColorOp::Passthrough
            | SurfaceColorOp::MapSdrToHdr
            | SurfaceColorOp::TranscodeHdrTransfer => None,
        };
        Self {
            title: std::sync::OnceLock::new(),
            surface_id,
            local_peer,
            decoder,
            latest: None,
            hdr_tonemap,
            cached_image: None,
            cached_hdr: None,
            pending_inputs: Vec::new(),
            input_sink: None,
            pointer_inside: false,
            keyboard_focused: false,
            rect: Rect::ZERO,
            dirty: true,
            metrics,
            frames_pending: 0,
            late_poll_until: None,
            picture_wake: None,
            pending_push: None,
            chain: neighbour_wire::unit_chain::UnitChain::default(),
            keyframe_sink: None,
            keyframe_requested: None,
        }
    }

    /// Follow a new `SurfaceStreamStart` for the same share: the sender
    /// switched codec (a viewer joined or left) or announced a display size.
    /// The decoder is replaced only when the codec changed; the old frame
    /// stays on screen until the new stream's first keyframe decodes.
    pub fn restart_stream(&mut self, codec: &str, display_size: Option<(u32, u32)>) {
        let bit_depth = if self.decoder.is_ten_bit() { 10 } else { 0 };
        if let Ok(decoder) = VideoDecoder::for_stream(codec, bit_depth)
            && decoder.codec() != self.decoder.codec()
        {
            tracing::info!(
                surface = self.surface_id,
                codec,
                "peer stream switched codec; rebuilding the decoder"
            );
            let colorimetry = if decoder.is_ten_bit() {
                Colorimetry::hdr10()
            } else {
                Colorimetry::sdr_bt709()
            };
            self.decoder = loop_decoder(decoder);
            self.decoder.set_stream_colorimetry(Some(colorimetry));
            self.decoder.set_picture_wake(self.picture_wake.clone());
            self.pending_push = None;
            self.late_poll_until = None;
            // The new stream starts at its own keyframe.
            self.chain.lose();
            self.keyframe_requested = None;
        }
        self.decoder.set_display_size(display_size);
    }

    pub fn codec(&self) -> media::StreamCodec {
        self.decoder.codec()
    }

    /// Whether this peer view is running the HDR path. Diagnostics-only.
    pub fn is_hdr(&self) -> bool {
        if self.hdr_tonemap.is_some() {
            return true;
        }
        self.decoder
            .last_colorimetry()
            .is_some_and(super::super::collab::colorimetry::Colorimetry::is_hdr)
    }

    pub fn metrics(&self) -> &PeerSurfaceMetrics {
        &self.metrics
    }

    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }

    pub fn local_peer(&self) -> PeerId {
        self.local_peer
    }

    /// Drops failed decodes silently (the next frame typically
    /// recovers). Returns `true` if a new frame was decoded.
    pub fn push_encoded(&mut self, seq: u64, nalus: &[u8]) -> bool {
        matches!(
            self.push_encoded_with_latency(seq, nalus, None),
            PushOutcome::Decoded | PushOutcome::DroppedOldest,
        )
    }

    /// `latency_ns` is the caller-computed motion-to-photon delta; `None`
    /// still records a zero-latency sample. Drop-oldest: at
    /// `PENDING_CEILING` the previous latest is dropped but the new
    /// frame still becomes `latest` — the drop is on the paint side.
    ///
    /// `seq` is the unit's `SurfaceFrame::seq`. Units can go missing between
    /// the owner's encoder and here (a saturated queue, a failed read), and
    /// a delta after a missing unit references a picture this decoder never
    /// saw: it is held back until the owner's next keyframe restarts the
    /// chain.
    #[tracing::instrument(level = "trace", skip(self, nalus), fields(nalu_bytes = nalus.len()))]
    pub fn push_encoded_with_latency(
        &mut self,
        seq: u64,
        nalus: &[u8],
        latency_ns: Option<u64>,
    ) -> PushOutcome {
        let keyframe = media::realtime::is_keyframe(self.codec(), nalus);
        match self.chain.admit(seq, keyframe) {
            neighbour_wire::unit_chain::Admission::Decode => {}
            neighbour_wire::unit_chain::Admission::Stale => return PushOutcome::Stale,
            neighbour_wire::unit_chain::Admission::AwaitingKeyframe { broke } => {
                if broke {
                    tracing::debug!(
                        surface = self.surface_id,
                        seq,
                        "peer stream lost a unit; asking for a keyframe"
                    );
                }
                self.request_keyframe(seq, broke);
                return PushOutcome::AwaitingKeyframe;
            }
        }
        if keyframe {
            self.keyframe_requested = None;
        }
        let decoded = self.decoder.decode(nalus);
        let pending = self.decoder.picture_pending();
        let now = ui::time::Instant::now();
        self.late_poll_until = pending
            .then(|| now.checked_add(LATE_PICTURE_WINDOW))
            .flatten();
        self.pending_push = pending.then_some((latency_ns, now));
        let Some(frame) = decoded else {
            if pending {
                return PushOutcome::Pending;
            }
            // Whatever the decoder kept of a rejected unit is no base for
            // the deltas after it.
            let broke = self.chain.intact();
            self.chain.lose();
            self.request_keyframe(seq, broke);
            return PushOutcome::DecodeFailed;
        };
        self.accept(frame, latency_ns)
    }

    /// Whether a pushed payload's picture is still being decoded.
    pub fn picture_pending(&self) -> bool {
        self.decoder.picture_pending()
    }

    /// Show a picture that finished after its push returned, without
    /// waiting: a stream that went quiet (the window stopped changing) would
    /// otherwise keep its last picture stranded in the decoder. `None` when
    /// none has finished.
    pub fn collect_late_picture(&mut self) -> Option<PushOutcome> {
        let frame = self.decoder.take_late_frame();
        // Approximate: the picture is the newest pending push's, or older.
        let latency_ns = self.pending_push.and_then(|(latency_ns, pushed)| {
            let waited = u64::try_from(pushed.elapsed().as_nanos()).unwrap_or(u64::MAX);
            latency_ns.map(|latency_ns| latency_ns.saturating_add(waited))
        });
        if !self.decoder.picture_pending() {
            self.late_poll_until = None;
            self.pending_push = None;
        }
        Some(self.accept(frame?, latency_ns))
    }

    /// Run `wake` on a media thread each time a pushed picture finishes
    /// decoding, so the compositor loop wakes to collect it with
    /// [`Self::collect_late_picture`] the moment it is ready, instead of
    /// polling every `LATE_PICTURE_POLL` while one is outstanding.
    pub fn set_picture_wake(&mut self, wake: Option<media::realtime::PictureWake>) {
        self.decoder.set_picture_wake(wake.clone());
        self.picture_wake = wake;
    }

    fn accept(&mut self, frame: DecodedFrame, latency_ns: Option<u64>) -> PushOutcome {
        let dropped = self.frames_pending >= PENDING_CEILING && self.latest.is_some();
        if dropped {
            self.metrics.record_dropped();
        } else {
            self.frames_pending = self.frames_pending.saturating_add(1);
        }

        self.latest = Some(frame);
        self.cached_image = None;
        self.cached_hdr = None;
        self.dirty = true;

        self.metrics.record_received(latency_ns.unwrap_or(0));

        if dropped {
            PushOutcome::DroppedOldest
        } else {
            PushOutcome::Decoded
        }
    }

    #[cfg(test)]
    pub(crate) fn frames_pending(&self) -> u32 {
        self.frames_pending
    }

    pub fn latest_frame(&self) -> Option<&DecodedFrame> {
        self.latest.as_ref()
    }

    /// Coordinates are surface-local logical pixels.
    pub fn record_pointer_motion(&mut self, x: f64, y: f64) {
        self.emit_input(InputEvent::PointerMotion { x, y });
    }

    pub fn record_pointer_button(&mut self, button: u32, pressed: bool) {
        self.emit_input(InputEvent::PointerButton { button, pressed });
    }

    /// `modifiers` is the xkbcommon-style bitfield (Shift=1, Ctrl=4,
    /// Alt=8, Logo=64); callers that don't track modifiers can pass `0`
    /// since the owner's keycode stream recovers state itself.
    pub fn record_key(&mut self, keysym: u32, pressed: bool, modifiers: u32) {
        self.emit_input(InputEvent::Key {
            keysym,
            pressed,
            modifiers,
        });
    }

    pub fn take_pending_inputs(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.pending_inputs)
    }

    pub fn set_input_sink(&mut self, sink: Arc<dyn Fn(InputEvent) + Send + Sync>) {
        self.input_sink = Some(sink);
    }

    /// Where the view asks the sender for a keyframe once a unit went
    /// missing or its decoder rejected one, naming that unit's sequence
    /// number, instead of staying frozen until the sender's periodic one.
    pub fn set_keyframe_sink(&mut self, sink: Arc<dyn Fn(u64) + Send + Sync>) {
        self.keyframe_sink = Some(sink);
    }

    /// Ask for a keyframe when the chain just broke, or again when the last
    /// request has gone unanswered for [`KEYFRAME_REQUEST_RETRY`].
    fn request_keyframe(&mut self, seq: u64, broke: bool) {
        let due = broke
            || self
                .keyframe_requested
                .is_none_or(|asked| asked.elapsed() >= KEYFRAME_REQUEST_RETRY);
        if !due {
            return;
        }
        if let Some(sink) = &self.keyframe_sink {
            self.keyframe_requested = Some(ui::time::Instant::now());
            sink(seq);
        }
    }

    fn emit_input(&mut self, event: InputEvent) {
        if let Some(sink) = &self.input_sink {
            sink(event);
        } else {
            self.pending_inputs.push(event);
        }
    }

    fn set_pointer_inside(&mut self, inside: bool) {
        if self.pointer_inside == inside {
            return;
        }
        self.pointer_inside = inside;
        self.emit_input(if inside {
            InputEvent::PointerEnter {
                surface: self.surface_id,
            }
        } else {
            InputEvent::PointerLeave {
                surface: self.surface_id,
            }
        });
    }

    fn set_keyboard_focused(&mut self, focused: bool) {
        if self.keyboard_focused == focused {
            return;
        }
        self.keyboard_focused = focused;
        self.emit_input(InputEvent::KeyboardFocus {
            surface: self.surface_id,
            focused,
        });
    }

    fn modifier_bits(modifiers: Modifiers) -> u32 {
        let state = modifiers.state();
        u32::from(state.shift_key())
            | (u32::from(state.control_key()) << 2)
            | (u32::from(state.alt_key()) << 3)
            | (u32::from(state.super_key()) << 6)
    }

    /// SDR RGBA frames upload straight through; HDR P010 frames are CPU
    /// tone-mapped into RGBA.
    fn ensure_image(&mut self) -> Option<&vello::peniko::ImageData> {
        if self.cached_image.is_none()
            && let Some(frame) = &self.latest
        {
            let rgba = match &frame.pixels {
                DecodedPixels::Rgba8(bytes) => bytes.clone(),
                DecodedPixels::P010 {
                    y,
                    uv,
                    y_stride,
                    uv_stride,
                } => {
                    let cfg = self.hdr_tonemap.as_ref().copied().unwrap_or_default();
                    self.cached_hdr = crate::shell::hdr_surface::SurfaceSignal::for_colorimetry(
                        frame.colorimetry,
                        cfg,
                    )
                    .and_then(|signal| {
                        crate::shell::hdr_surface::HdrPlane::p010(
                            y,
                            uv,
                            (*y_stride, *uv_stride),
                            (frame.width, frame.height),
                            signal,
                        )
                    })
                    .map(Arc::new);
                    tonemap_p010_to_rgba8(
                        y,
                        uv,
                        *y_stride,
                        *uv_stride,
                        frame.width,
                        frame.height,
                        frame.colorimetry,
                        cfg,
                    )
                }
            };
            let blob = vello::peniko::Blob::new(std::sync::Arc::new(rgba));
            self.cached_image = Some(ui::raster::rgba8(blob, frame.width, frame.height));
        }
        self.cached_image.as_ref()
    }
}

impl Widget for PeerView {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let new_rect = ui::widget::layout_rect(taffy, node, origin);
        // Rect changed → the placement matrix in `draw` changes too. Exact
        // equality is safe: taffy produces integer-valued logical pixels.
        if new_rect != self.rect {
            self.dirty = true;
        }
        self.rect = new_rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let image = self.ensure_image().cloned();
        // A window streamed from another device is shown at its own shape,
        // centred on the letterbox every theme gives media that does not
        // fill its frame; it is never resized to fit this tile. Before its
        // first picture the view stands on the raised surface, so it still
        // reads as intentional rather than a hole in the chrome.
        let ground = if image.is_some() {
            tokens.colors.media.letterbox
        } else {
            tokens.colors.surfaces.raised
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            ground,
            None,
            &self.rect,
        );

        let rect = self.content_rect();

        let hdr_plane = self.cached_hdr.clone();
        if let Some(image) = &image {
            // Scale + translate so the natural-size image fills `rect`.
            let img_w = f64::from(image.width);
            let img_h = f64::from(image.height);
            if img_w > 0.0 && img_h > 0.0 {
                let sx = rect.width() / img_w;
                let sy = rect.height() / img_h;
                // `transform.pre_translate(t).pre_scale_non_uniform(sx, sy)`
                // is kurbo's non-operator equivalent of
                // `transform * Affine::translate(t) * Affine::scale_non_uniform(sx, sy)`
                // (composition is associative, so chaining the two `pre_*`
                // combinators onto `transform` gives the same matrix).
                let placement = transform
                    .pre_translate((rect.x0, rect.y0).into())
                    .pre_scale_non_uniform(sx, sy);
                if let Some(plane) = &hdr_plane {
                    crate::shell::hdr_surface::record(
                        plane,
                        image,
                        placement,
                        transform.transform_rect_bbox(rect),
                    );
                }
                scene.draw_image(image, placement);
            }
        } else {
            let label = crate::shell::i18n::t("peer-view-awaiting");
            let style = tokens.typography.style(TextRole::Body);
            let (lw, lh) = text_renderer.measure_style(&label, None, &style);
            let cx = rect.center().x - f64::from(lw) / 2.0;
            let cy = rect.center().y - f64::from(lh) / 2.0;
            text_renderer.draw_style(
                scene,
                &label,
                tokens.colors.text_dim,
                transform.pre_translate((cx, cy).into()),
                None,
                &style,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x, position.y);
                let content = self.content_rect();
                let inside = content.contains(point);
                self.set_pointer_inside(inside);
                if inside && content.width() > 0.0 && content.height() > 0.0 {
                    let (width, height) = self.latest.as_ref().map_or_else(
                        || (self.rect.width(), self.rect.height()),
                        |frame| (f64::from(frame.width), f64::from(frame.height)),
                    );
                    let x = ((point.x - content.x0) / content.width() * width)
                        .clamp(0.0, width.max(1.0) - 1.0);
                    let y = ((point.y - content.y0) / content.height() * height)
                        .clamp(0.0, height.max(1.0) - 1.0);
                    self.record_pointer_motion(x, y);
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } => self.set_pointer_inside(false),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = matches!(state, winit::event::ElementState::Pressed);
                if pressed {
                    self.set_keyboard_focused(self.pointer_inside);
                }
                if (self.pointer_inside || self.keyboard_focused)
                    && let Some(button) = crate::shell::input_policy::evdev_button(*button)
                {
                    self.record_pointer_button(button, pressed);
                    *ctx.handled = true;
                    if pressed {
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.pointer_inside => {
                let ((dx, dy), _) = crate::shell::input_policy::axis_values(*delta);
                if dx != 0.0 || dy != 0.0 {
                    self.emit_input(InputEvent::PointerAxis { dx, dy });
                }
                *ctx.handled = true;
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text))
                if self.keyboard_focused && !text.is_empty() =>
            {
                self.emit_input(InputEvent::Text { text: text.clone() });
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event, .. } if self.keyboard_focused => {
                if let Some(keycode) = crate::shell::input_policy::evdev_keycode(event) {
                    self.record_key(
                        keycode,
                        matches!(event.state, winit::event::ElementState::Pressed),
                        Self::modifier_bits(*ctx.modifiers),
                    );
                    *ctx.handled = true;
                }
            }
            WindowEvent::Focused(false) => self.set_keyboard_focused(false),
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "PeerView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    /// Lets the App's `CursorMoved` redraw gate skip frames over an idle
    /// peer surface.
    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    /// While a pushed picture is still decoding, wake the loop shortly so it
    /// is collected even if no further frame of the stream arrives. A view
    /// with a picture wake needs no poll: the decoder wakes the loop when a
    /// picture is out, an element fails or a new one has started. Only the
    /// moment a starting element is given up for showing nothing is
    /// announced by no one, so the view wakes once for it.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let pending = self.decoder.picture_pending();
        let startup = self.decoder.startup_deadline().filter(|_| pending);
        if self.picture_wake.is_some() {
            return startup;
        }
        // Without a wake, poll for a while after each push, and for as long
        // as the decoder is still starting.
        let now = ui::time::Instant::now();
        self.late_poll_until
            .max(startup)
            .filter(|until| now < *until && pending)
            .and_then(|_| now.checked_add(LATE_PICTURE_POLL))
            .or(startup)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.frames_pending = 0;
    }

    /// A `percent(1.0)` leaf — layout-clean after the first apply.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

fn aspect_fit_rect(viewport: Rect, width: u32, height: u32) -> Rect {
    if width == 0 || height == 0 || viewport.width() <= 0.0 || viewport.height() <= 0.0 {
        return viewport;
    }
    let scale = (viewport.width() / f64::from(width)).min(viewport.height() / f64::from(height));
    let fitted_width = f64::from(width) * scale;
    let fitted_height = f64::from(height) * scale;
    Rect::from_center_size(viewport.center(), (fitted_width, fitted_height))
}

impl AppWidget for PeerView {
    fn app_kind(&self) -> AppKind {
        // No dedicated `Peer` variant yet; `Settings` is the catch-all.
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        self.title.get_or_init(|| {
            crate::shell::i18n::t_args(
                "peer-view-surface-title",
                &[("id", self.surface_id.to_string().into())],
            )
        })
    }

    fn app_icon(&self) -> IconDescriptor {
        // Stands in for "remote / peer surface" until a dedicated icon ships.
        IconDescriptor::Glyph("\u{f0ac}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::encoder::VideoEncoder;
    use media::StreamCodec;

    /// A VP9 stream from the royalty-free software encoder: no GPU and no
    /// Cisco binary needed.
    fn vp9_unit(width: u32, height: u32) -> Vec<u8> {
        let mut enc = VideoEncoder::new(StreamCodec::Vp9, width, height, 30).expect("VP9 encoder");
        let mut units = enc.encode(&gradient_frame(width, height)).expect("encode");
        units.extend(enc.finish().expect("finish"));
        units.swap_remove(0).data
    }

    fn vp9_view(metrics: PeerSurfaceMetrics) -> PeerView {
        let caps = crate::shell::hdr_display::DisplayCapabilities::sdr_default();
        PeerView::new_for_stream(42, 7, "vp9", metrics, &caps, None)
    }

    /// Push `unit` and settle its picture as the compositor loop does: a
    /// push never waits, so a picture still decoding is collected once it
    /// is out. A rejected unit stays [`PushOutcome::DecodeFailed`], and one
    /// whose picture never comes stays [`PushOutcome::Pending`].
    fn push_settled(view: &mut PeerView, unit: &[u8], latency_ns: Option<u64>) -> PushOutcome {
        let outcome = view.push_encoded_with_latency(0, unit, latency_ns);
        if outcome != PushOutcome::Pending {
            return outcome;
        }
        let deadline = tilo_test_env::hang_deadline();
        while std::time::Instant::now() < deadline {
            if let Some(outcome) = view.collect_late_picture() {
                return outcome;
            }
            if !view.picture_pending() {
                return PushOutcome::DecodeFailed;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        PushOutcome::Pending
    }

    /// `coord * 255 / span.max(1)`, computed via saturating/checked ops
    /// rather than raw operators: saturating multiply can't wrap for a
    /// huge `coord`, and `span.max(1)` makes the divisor provably
    /// nonzero so `checked_div` never needs its fallback.
    fn u8_from_gradient(coord: u32, span: u32) -> u8 {
        let scaled = coord
            .saturating_mul(255)
            .checked_div(span.max(1))
            .unwrap_or(0);
        // Safe: `scaled <= 255` whenever `coord < span`, which every
        // caller's `0..span` loop below guarantees.
        u8::try_from(scaled).unwrap_or(u8::MAX)
    }

    /// A deterministic RGBA gradient, so the round-trip needs no binary fixture.
    fn gradient_frame(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            (width as usize)
                .saturating_mul(height as usize)
                .saturating_mul(4),
        );
        for y in 0..height {
            for x in 0..width {
                out.push(u8_from_gradient(x, width));
                out.push(u8_from_gradient(y, height));
                out.push(128);
                out.push(255);
            }
        }
        out
    }

    #[test]
    fn peer_view_paints_fitted_frames_or_a_placeholder() {
        fn aspect_fit_preserves_pixels_and_letterboxes() {
            let viewport = Rect::new(0.0, 0.0, 1000.0, 1000.0);
            let fitted = aspect_fit_rect(viewport, 1920, 1080);
            assert!(fitted.x0.abs() < 0.001);
            assert!((fitted.x1 - 1000.0).abs() < 0.001);
            assert!((fitted.height() - 562.5).abs() < 0.001);
            assert!((fitted.y0 - 218.75).abs() < 0.001);
        }

        fn decoded_frame_paints_image_into_scene() {
            use vello_encoding::DrawTag;
            const W: u32 = 64;
            const H: u32 = 64;
            let nalus = vp9_unit(W, H);

            let mut view = vp9_view(PeerSurfaceMetrics::default());
            assert_eq!(
                push_settled(&mut view, &nalus, None),
                PushOutcome::Decoded,
                "decoder produced no frame from encoder output"
            );
            let latest = view.latest_frame().expect("frame stashed");
            assert_eq!(latest.width, W);
            assert_eq!(latest.height, H);
            assert!(latest.pixels.is_rgba(), "an SDR stream yields RGBA pixels");

            view.rect = Rect::new(0.0, 0.0, 100.0, 100.0);

            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            let theme = Theme::default();
            let presence = PresenceCtx::default();
            view.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

            let enc_ref = scene.encoding();
            let has_image = enc_ref.draw_tags.contains(&DrawTag::IMAGE);
            assert!(
                has_image,
                "PeerView::draw emitted no image into the scene's draw stream"
            );
        }

        fn empty_view_paints_placeholder_text_not_image() {
            use vello_encoding::DrawTag;
            let mut view = PeerView::new(1, 1);
            view.rect = Rect::new(0.0, 0.0, 100.0, 100.0);
            let mut scene = Scene::new();
            let mut tr = TextRenderer::new();
            let theme = Theme::default();
            let presence = PresenceCtx::default();
            view.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let enc_ref = scene.encoding();
            let has_image = enc_ref.draw_tags.contains(&DrawTag::IMAGE);
            assert!(
                !has_image,
                "fresh PeerView with no decoded frame leaked an image draw"
            );
        }

        aspect_fit_preserves_pixels_and_letterboxes();
        decoded_frame_paints_image_into_scene();
        empty_view_paints_placeholder_text_not_image();
    }

    #[test]
    fn frames_ahead_of_their_stream_start_wait_bounded_in_order() {
        let mut early = EarlyFrames::default();
        for frame in 0..6_u8 {
            early.hold((1, 2), u64::from(frame), vec![frame]);
        }
        assert_eq!(
            early.take((1, 2)),
            vec![(0, vec![0]), (1, vec![1]), (2, vec![2]), (3, vec![3])],
            "the first frames, keyframe first, and only a few"
        );
        assert!(early.take((1, 2)).is_empty());
        for stream in 0..20 {
            early.hold((stream, 0), 0, vec![1]);
        }
        assert_eq!(
            (0..20)
                .filter(|stream| !early.take((*stream, 0)).is_empty())
                .count(),
            EarlyFrames::STREAMS
        );
        early.hold((3, 3), 0, vec![1]);
        early.forget((3, 3));
        assert!(early.take((3, 3)).is_empty());
    }

    #[test]
    fn peer_view_records_inputs_and_keeps_click_focus() {
        fn pending_inputs_recorded_and_drained() {
            let mut view = PeerView::new(1, 9);
            view.record_pointer_motion(10.0, 20.0);
            view.record_pointer_button(272, true);
            view.record_key(0xFF0D, true, 0);
            let drained = view.take_pending_inputs();
            assert_eq!(drained.len(), 3);
            assert!(matches!(drained[0], InputEvent::PointerMotion { .. }));
            assert!(matches!(drained[1], InputEvent::PointerButton { .. }));
            assert!(matches!(drained[2], InputEvent::Key { .. }));
            assert!(view.take_pending_inputs().is_empty());
        }

        fn pointer_leave_preserves_click_focus_until_explicit_blur() {
            let mut view = PeerView::new(42, 9);
            view.set_pointer_inside(true);
            view.set_keyboard_focused(true);
            view.set_pointer_inside(false);
            assert!(view.keyboard_focused, "hover leave must preserve the caret");

            view.set_keyboard_focused(false);
            let events = view.take_pending_inputs();
            assert!(matches!(
                events.as_slice(),
                [
                    InputEvent::PointerEnter { surface: 42 },
                    InputEvent::KeyboardFocus {
                        surface: 42,
                        focused: true
                    },
                    InputEvent::PointerLeave { surface: 42 },
                    InputEvent::KeyboardFocus {
                        surface: 42,
                        focused: false
                    }
                ]
            ));
        }

        fn wheel_and_composed_text_go_to_the_window_under_focus() {
            let mut view = PeerView::new(42, 9);
            let send =
                |view: &mut PeerView, event: WindowEvent| ui::testing::dispatch(view, &event);
            let wheel = || ui::synthetic_input::mouse_wheel(0.0, 1.0);
            let commit = || WindowEvent::Ime(winit::event::Ime::Commit("é".into()));

            // Neither reaches a window the pointer is not over or that has
            // not been clicked into.
            assert!(!send(&mut view, wheel()));
            assert!(!send(&mut view, commit()));
            assert!(view.take_pending_inputs().is_empty());

            view.set_pointer_inside(true);
            view.set_keyboard_focused(true);
            let _ = view.take_pending_inputs();
            assert!(send(&mut view, wheel()));
            assert!(send(&mut view, commit()));
            let events = view.take_pending_inputs();
            assert!(
                matches!(
                    events.as_slice(),
                    [InputEvent::PointerAxis { dx, dy }, InputEvent::Text { text }]
                        if *dx == 0.0 && *dy < 0.0 && text == "é"
                ),
                "{events:?}"
            );
        }

        pending_inputs_recorded_and_drained();
        pointer_leave_preserves_click_focus_until_explicit_blur();
        wheel_and_composed_text_go_to_the_window_under_focus();
    }

    /// Once every pushed picture is out, the view neither collects a
    /// phantom picture nor keeps the loop awake looking for one.
    #[test]
    fn a_settled_view_neither_collects_nor_wakes() {
        const W: u32 = 64;
        const H: u32 = 64;
        let mut view = vp9_view(PeerSurfaceMetrics::default());
        assert_eq!(view.next_wake_deadline(), None, "nothing pushed yet");
        assert_eq!(
            push_settled(&mut view, &vp9_unit(W, H), None),
            PushOutcome::Decoded,
        );
        assert!(!view.picture_pending());
        assert_eq!(view.collect_late_picture(), None);
        assert_eq!(view.next_wake_deadline(), None);
    }

    /// A stream's first push costs the loop no decoder start-up: the
    /// decoder starts on a media thread, the push returns, and the decoder
    /// wakes the loop to collect the picture. Until the first picture the
    /// view wakes once more at most, when a decoder that never showed
    /// anything would be given up; once it shows, the view keeps the loop
    /// asleep.
    #[test]
    fn the_first_push_never_waits_for_the_decoder_to_start() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const W: u32 = 64;
        const H: u32 = 64;
        let nalus = vp9_unit(W, H);
        let mut view = vp9_view(PeerSurfaceMetrics::default());
        let wakes = Arc::new(AtomicUsize::new(0));
        view.set_picture_wake(Some({
            let wakes = Arc::clone(&wakes);
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            })
        }));
        let started = std::time::Instant::now();
        let outcome = view.push_encoded_with_latency(0, &nalus, None);
        eprintln!(
            "first push of a stream: {:?}, {outcome:?}",
            started.elapsed()
        );
        match outcome {
            PushOutcome::Pending => {
                assert!(view.picture_pending(), "the keyframe is still decoding");
                assert!(
                    view.next_wake_deadline().is_some(),
                    "one wake for the start-up deadline"
                );
                let deadline = tilo_test_env::hang_deadline();
                while view.picture_pending() && std::time::Instant::now() < deadline {
                    if wakes.load(Ordering::SeqCst) > 0
                        && let Some(outcome) = view.collect_late_picture()
                    {
                        assert_eq!(outcome, PushOutcome::Decoded);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                assert!(
                    wakes.load(Ordering::SeqCst) > 0,
                    "the decoder woke the loop"
                );
                assert!(!view.picture_pending(), "the keyframe's picture came out");
            }
            // A loaded runner can descend into the decoder's own threads
            // long enough for them to finish before the push returns.
            PushOutcome::Decoded => {}
            outcome => panic!("the stream's keyframe decodes: {outcome:?}"),
        }
        assert!(view.latest_frame().is_some(), "the keyframe is shown");
        assert_eq!(
            view.next_wake_deadline(),
            None,
            "a view that shows its stream keeps the loop asleep"
        );
    }

    /// Push `unit` until a push returns before its picture is out, and
    /// return how many pushes before it came back with their picture. On a
    /// quiet machine the first push does: a decode takes milliseconds, the
    /// push microseconds. On a loaded one the decoder's thread can finish a
    /// tiny picture before the pushing thread runs again, so a few are
    /// tried. A push that waited for its picture never returns before it, so
    /// this fails then.
    fn push_until_pending(view: &mut PeerView, unit: &[u8], latency_ns: Option<u64>) -> usize {
        const ATTEMPTS: usize = 50;
        for decoded in 0..ATTEMPTS {
            match view.push_encoded_with_latency(0, unit, latency_ns) {
                PushOutcome::Pending => return decoded,
                outcome => {
                    assert_eq!(outcome, PushOutcome::Decoded, "push {decoded}");
                    view.mark_drawn();
                }
            }
        }
        panic!("none of {ATTEMPTS} pushes returned before its picture: pushes wait");
    }

    /// A push on the compositor loop hands the unit over and returns without
    /// waiting for its picture; the decoder's wake brings the loop back the
    /// moment the picture is out, and the picture's latency counts the time
    /// it took. Without a wake the view polls; with one it never keeps the
    /// loop awake itself.
    #[test]
    fn a_push_never_waits_and_the_finished_picture_wakes_the_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const W: u32 = 64;
        const H: u32 = 64;
        let nalus = vp9_unit(W, H);
        let metrics = PeerSurfaceMetrics::default();
        let mut view = vp9_view(metrics.clone());
        let wakes = Arc::new(AtomicUsize::new(0));
        view.set_picture_wake(Some({
            let wakes = Arc::clone(&wakes);
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::SeqCst);
            })
        }));
        // The first unit starts the decoder on a media thread: its start-up
        // and its picture each wake the loop once.
        assert_eq!(push_settled(&mut view, &nalus, None), PushOutcome::Decoded);
        view.mark_drawn();
        let deadline = tilo_test_env::hang_deadline();
        while wakes.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            wakes.swap(0, Ordering::SeqCst),
            2,
            "the decoder's start-up and its first picture woke the loop"
        );
        let latency_ns = 1_000_000;
        let decoded = push_until_pending(&mut view, &nalus, Some(latency_ns));
        assert!(view.picture_pending(), "a pending push is still decoding");
        assert_eq!(
            view.next_wake_deadline(),
            None,
            "the decoder's wake brings the loop back, not a poll"
        );
        // One picture per push: those that came back with theirs, then the
        // pending one. Each wakes the loop once it is ready to collect.
        let pictures = decoded.saturating_add(1);
        let deadline = tilo_test_env::hang_deadline();
        while wakes.load(Ordering::SeqCst) < pictures && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            pictures,
            "each picture woke the loop once"
        );
        assert_eq!(view.collect_late_picture(), Some(PushOutcome::Decoded));
        assert!(!view.picture_pending());
        assert!(
            metrics.snapshot().latency_ns_max > latency_ns,
            "a late picture's latency includes its decode"
        );
        assert_eq!(view.next_wake_deadline(), None);

        view.set_picture_wake(None);
        view.mark_drawn();
        push_until_pending(&mut view, &nalus, None);
        assert!(view.next_wake_deadline().is_some(), "polled while pending");
    }

    #[test]
    fn peer_view_stream_queue_bounds_latency_and_rebuilds_decoders() {
        fn push_encoded_returns_false_for_empty_payload() {
            let mut view = PeerView::new(0, 0);
            assert!(!view.push_encoded(0, &[]));
            assert!(view.latest_frame().is_none());
        }

        fn push_encoded_with_latency_drops_oldest_after_ceiling() {
            const W: u32 = 64;
            const H: u32 = 64;
            let nalus = vp9_unit(W, H);

            let metrics = PeerSurfaceMetrics::default();
            let mut view = vp9_view(metrics.clone());

            assert_eq!(
                push_settled(&mut view, &nalus, Some(1_000)),
                PushOutcome::Decoded,
            );
            assert_eq!(view.frames_pending(), 1);

            assert_eq!(
                push_settled(&mut view, &nalus, Some(1_500)),
                PushOutcome::Decoded,
            );
            assert_eq!(view.frames_pending(), 2);

            // Ceiling is 2; the older frame drops.
            assert_eq!(
                push_settled(&mut view, &nalus, Some(2_000)),
                PushOutcome::DroppedOldest,
            );
            let snap = metrics.snapshot();
            assert_eq!(snap.frames_dropped, 1);
            assert_eq!(snap.frames_received, 3, "every accepted decode counts");

            view.mark_drawn();
            assert_eq!(view.frames_pending(), 0);

            assert_eq!(
                push_settled(&mut view, &nalus, Some(2_500)),
                PushOutcome::Decoded,
            );
            let snap = metrics.snapshot();
            assert_eq!(
                snap.frames_dropped, 1,
                "no additional drops after mark_drawn"
            );
        }

        /// A stream-start label the receiver can't honour must never cost the
        /// viewer the share: it degrades to the SDR H.264 decoder.
        fn an_unknown_stream_label_degrades_to_h264() {
            let caps = crate::shell::hdr_display::DisplayCapabilities::sdr_default();
            let unknown =
                PeerView::new_for_stream(42, 7, "h266", PeerSurfaceMetrics::default(), &caps, None);
            assert_eq!(unknown.codec(), StreamCodec::H264);
            let hevc = PeerView::new_for_stream(
                43,
                7,
                "hevc-main10",
                PeerSurfaceMetrics::default(),
                &caps,
                None,
            );
            assert_eq!(hevc.codec(), StreamCodec::HevcMain10);
            assert!(
                hevc.is_hdr(),
                "a 10-bit stream runs the HDR path on an SDR display"
            );
        }

        /// A sender that switches codec announces a new stream; the view
        /// follows it and decodes the new codec.
        fn a_codec_switch_rebuilds_the_decoder() {
            const W: u32 = 64;
            const H: u32 = 64;
            let caps = crate::shell::hdr_display::DisplayCapabilities::sdr_default();
            let mut view =
                PeerView::new_for_stream(42, 7, "h264", PeerSurfaceMetrics::default(), &caps, None);
            view.restart_stream("vp9", Some((W, H)));
            assert_eq!(view.codec(), StreamCodec::Vp9);
            assert_eq!(
                push_settled(&mut view, &vp9_unit(W, H), None),
                PushOutcome::Decoded,
                "the new codec decodes"
            );
            view.restart_stream("vp9", Some((W, H)));
            assert!(
                view.latest_frame().is_some(),
                "an unchanged codec keeps the picture"
            );
        }

        fn metrics_handle_is_shared_via_clone() {
            const W: u32 = 64;
            const H: u32 = 64;
            let nalus = vp9_unit(W, H);

            let metrics = PeerSurfaceMetrics::default();
            let external = metrics.clone();
            let mut view = vp9_view(metrics);

            assert_eq!(external.snapshot().frames_received, 0);
            assert_eq!(
                push_settled(&mut view, &nalus, Some(500)),
                PushOutcome::Decoded
            );
            assert_eq!(external.snapshot().frames_received, 1);
            // The first picture comes out after its push returns (the
            // decoder starts on a media thread), so its latency is the
            // push's plus the decode it waited for, as both handles see.
            let seen = external.snapshot().latency_ns_mean;
            assert_eq!(seen, view.metrics().snapshot().latency_ns_mean);
            assert!(seen >= 500, "the push's latency is counted: {seen}");
        }

        push_encoded_returns_false_for_empty_payload();
        push_encoded_with_latency_drops_oldest_after_ceiling();
        an_unknown_stream_label_degrades_to_h264();
        a_codec_switch_rebuilds_the_decoder();
        metrics_handle_is_shared_via_clone();
    }
}
