//! Peer-side foreign-surface view.
//!
//! `PeerView` is the receive-side counterpart to the owner's encode loop in
//! [`crate::network::streaming`]. It owns one [`crate::network::H264Decoder`]
//! and paints the most recently decoded RGBA frame as a vello image draw,
//! blitted into the widget's content rect.
//!
//! When a [`crate::network::NeighbourMsg::SurfaceFrame`] arrives over the
//! workspace's gossipsub topic, [`PeerView::push_encoded`] decodes it
//! synchronously and stashes the latest frame; the next `draw` pass blits
//! it. Older frames are dropped — no queue, mirroring the encode side's
//! "encode or drop" contract.
//!
//! Pointer and keyboard events on a `PeerView` get recorded into an
//! internal queue via [`PeerView::record_pointer_motion`] /
//! [`PeerView::record_pointer_button`] / [`PeerView::record_key`]. The
//! shell's per-frame tick drains [`PeerView::take_pending_inputs`] and
//! ships each `InputEvent` over gossipsub as a
//! [`crate::network::NeighbourMsg::RemoteInput`], routed on the owner side
//! through the [`crate::network::streaming::ShareRegistry`].

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{Modifiers, WindowEvent};

use ui::Theme;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

use crate::collab::colorimetry::Colorimetry;
use crate::diagnostics::PeerSurfaceMetrics;
use crate::network::decoder::{DecodedFrame, DecodedPixels, VideoDecoder};
use crate::network::protocol::{PeerId, SurfaceId};
use crate::network::streaming::InputEvent;
use crate::shell::color_pipeline::{OutputColorState, SurfaceColorOp, plan_surface_op};
use crate::shell::hdr_tonemap::{HdrTonemapConfig, tonemap_p010_to_rgba8};

/// Ceiling on the receive-side decode queue depth; once undrawn frames
/// exceed this, further pushes drop the oldest and bump the metrics
/// counter. Two is the minimum useful ceiling — one paint-in-flight, one
/// staged for the next frame. Raising it trades latency for burst
/// tolerance; the owner's "encode or drop" contract means the peer should
/// almost never backlog under steady state.
pub(crate) const PENDING_CEILING: u32 = 2;

/// Outcome of a `push_encoded_with_latency` call, for structured logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Decoded and became the new latest frame; latency recorded on the
    /// shared [`PeerSurfaceMetrics`].
    Decoded,
    /// Decoded, but the pending-queue ceiling was hit and the oldest
    /// frame was dropped; `frames_dropped` incremented.
    DroppedOldest,
    /// The decoder rejected the payload (corrupt NALU, or a mid-stream
    /// keyframe not yet arrived). Nothing recorded.
    DecodeFailed,
}

/// Peer-side widget rendering one shared surface: the decoder, the latest
/// decoded frame as a peniko image, and the pending-input queue.
/// Constructed by the chrome when a foreign tab is bound to a peer-shared
/// surface; lives for the tab's lifetime.
pub struct PeerView {
    /// Cached title string returned by `AppWidget::app_title` — materialised
    /// once at construction rather than per-frame.
    title: String,
    /// Surface id the owner uses to address this share. Echoed back in
    /// `RemoteInput` so the owner can route input to the right surface.
    surface_id: SurfaceId,
    /// Local peer id (the *viewer*) — the value the owner sees as the
    /// `peer` field on `RemoteInput`.
    local_peer: PeerId,
    /// Active decoder — H.264 for SDR, HEVC Main10 for HDR. Chosen at
    /// construction from `SurfaceStreamStart.codec`; SDR default so
    /// legacy peers that call [`Self::new`] without a codec label still work.
    decoder: VideoDecoder,
    /// Most recent decoded frame, ready to paint. `None` until the first
    /// successful decode.
    latest: Option<DecodedFrame>,
    /// HDR configuration snapshot from the stream-start message. `None`
    /// on SDR streams; drives the tone-map decision on
    /// [`Self::ensure_image`].
    hdr_tonemap: Option<HdrTonemapConfig>,
    /// Cached `peniko::ImageData` derived from `latest`. Rebuilt only when
    /// `latest` changes so the steady-state draw path doesn't reallocate.
    cached_image: Option<vello::peniko::ImageData>,
    /// Pending input events to ship back to the owner. Drained per frame
    /// by the shell's tick hook.
    pending_inputs: Vec<InputEvent>,
    /// Layout rect computed by taffy.
    rect: Rect,
    /// True when the view needs to repaint — set on every new decoded
    /// frame, cleared by `mark_drawn`. Without this, the default leaf
    /// [`Widget::needs_redraw`] would return `true` every frame, defeating
    /// the App's redraw-only-when-dirty gate. Starts `true` so the first
    /// frame paints the placeholder.
    dirty: bool,
    /// Shared observability counters (cheap `Arc` clone) — the OSD overlay
    /// and the e2e test can read them without touching the widget. Callers
    /// that want a shared handle up front use `PeerView::new_with_metrics`.
    metrics: PeerSurfaceMetrics,
    /// Decoded frames the shell hasn't yet pulled through the paint loop.
    /// Bumped on every accepted decode, cleared on `mark_drawn`. Once this
    /// exceeds [`PENDING_CEILING`], the next push drops the oldest,
    /// keeping motion-to-photon bounded even when the peer is CPU-starved.
    frames_pending: u32,
}

impl PeerView {
    /// Construct a new peer view for `surface_id` owned by the remote peer;
    /// the viewer's own peer id is `local_peer`. A fresh metrics handle is
    /// created — see [`Self::new_with_metrics`] to share one instead.
    pub fn new(surface_id: SurfaceId, local_peer: PeerId) -> Self {
        Self::new_with_metrics(surface_id, local_peer, PeerSurfaceMetrics::default())
    }

    /// Same as [`Self::new`] but with a caller-supplied metrics handle.
    /// The OSD overlay + the flatpak-share e2e test both use this to
    /// observe drop/latency counters without touching the widget.
    pub fn new_with_metrics(
        surface_id: SurfaceId,
        local_peer: PeerId,
        metrics: PeerSurfaceMetrics,
    ) -> Self {
        Self {
            title: format!("Peer surface #{surface_id}"),
            surface_id,
            local_peer,
            decoder: VideoDecoder::H264(crate::network::decoder::H264Decoder::new()),
            latest: None,
            hdr_tonemap: None,
            cached_image: None,
            pending_inputs: Vec::new(),
            rect: Rect::ZERO,
            dirty: true,
            metrics,
            frames_pending: 0,
        }
    }

    /// Codec-aware constructor: picks the decoder from
    /// `SurfaceStreamStart.codec` (`"h264"` SDR, `"hevc-main10"` HDR) and
    /// captures the tone-map config from the stream's HDR static metadata.
    /// Unknown/unsupported labels fall back to SDR so the caller can show a
    /// placeholder + OSD diagnostic rather than the share silently vanishing.
    pub fn new_for_stream(
        surface_id: SurfaceId,
        local_peer: PeerId,
        codec: &str,
        metrics: PeerSurfaceMetrics,
        display_caps: &crate::shell::hdr_display::DisplayCapabilities,
        hdr_static_metadata: Option<crate::collab::hdr_metadata::Hdr10Metadata>,
    ) -> Self {
        let decoder = VideoDecoder::for_wire_codec(codec).unwrap_or_else(|e| {
            tracing::warn!(
                surface = surface_id,
                codec = codec,
                error = %e,
                "peer view falling back to H264 SDR decoder"
            );
            VideoDecoder::H264(crate::network::decoder::H264Decoder::new())
        });
        // Route the SDR/HDR decision through the canonical output colour
        // pipeline so receive matches local composition — the tone-mapper
        // only runs on HDR-into-non-HDR-output.
        let stream_colorimetry = if matches!(decoder, VideoDecoder::H264(_)) {
            Colorimetry::sdr_bt709()
        } else {
            Colorimetry::hdr10()
        };
        let output_state = OutputColorState::from_display(display_caps);
        // Mastering peak, when signalled — same derivation as
        // `HdrTonemapConfig::from_metadata`, fed here so the pipeline sizes
        // tone-map compression against the real source peak.
        let source_peak = hdr_static_metadata
            .map(|m| m.mastering.max_display_mastering_luminance as f32 / 10_000.0)
            .filter(|nits| nits.is_finite() && *nits > 0.0);
        let hdr_tonemap = match plan_surface_op(stream_colorimetry, &output_state, source_peak) {
            SurfaceColorOp::TonemapHdrToSdr { config } => Some(config),
            // Passthrough, MapSdrToHdr (the `hdr_output_encode.wgsl` path),
            // and the HLG↔PQ transcode all upload without a CPU tone-map.
            SurfaceColorOp::Passthrough
            | SurfaceColorOp::MapSdrToHdr
            | SurfaceColorOp::TranscodeHdrTransfer => None,
        };
        Self {
            title: format!("Peer surface #{surface_id}"),
            surface_id,
            local_peer,
            decoder,
            latest: None,
            hdr_tonemap,
            cached_image: None,
            pending_inputs: Vec::new(),
            rect: Rect::ZERO,
            dirty: true,
            metrics,
            frames_pending: 0,
        }
    }

    /// Whether this peer view is running the HDR path — either an
    /// HDR tone-map is active or the decoded frames carry a HDR
    /// colorimetry. Diagnostics-only.
    pub fn is_hdr(&self) -> bool {
        if self.hdr_tonemap.is_some() {
            return true;
        }
        self.decoder
            .last_colorimetry()
            .map(|c| c.is_hdr())
            .unwrap_or(false)
    }

    /// Borrow the metrics handle. Clone it to hand to an OSD overlay
    /// or a diagnostics readout.
    pub fn metrics(&self) -> &PeerSurfaceMetrics {
        &self.metrics
    }

    /// Surface id this view renders.
    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }

    /// Local peer id (the viewer's).
    pub fn local_peer(&self) -> PeerId {
        self.local_peer
    }

    /// Push a NALU byte payload through the decoder. Drops failed decodes
    /// silently (the next frame typically recovers); on success, stashes
    /// the frame and invalidates the cached `peniko::ImageData`. Returns
    /// `true` if a new frame was decoded.
    pub fn push_encoded(&mut self, nalus: &[u8]) -> bool {
        matches!(
            self.push_encoded_with_latency(nalus, None),
            PushOutcome::Decoded | PushOutcome::DroppedOldest,
        )
    }

    /// Push a NALU payload and record backpressure/latency in the shared
    /// metrics. `latency_ns` is the caller-computed motion-to-photon
    /// delta; pass `None` when latency isn't tracked (a zero-latency
    /// sample is recorded so the counter still ticks). Drop-oldest: if the
    /// pending-queue depth is already at [`PENDING_CEILING`], the previous
    /// latest is dropped (`frames_dropped` increments) but the new frame
    /// still becomes `latest` — the drop is on the paint side, not the decoder.
    #[tracing::instrument(level = "trace", skip(self, nalus), fields(nalu_bytes = nalus.len()))]
    pub fn push_encoded_with_latency(
        &mut self,
        nalus: &[u8],
        latency_ns: Option<u64>,
    ) -> PushOutcome {
        let frame = match self.decoder.decode(nalus) {
            Some(f) => f,
            None => return PushOutcome::DecodeFailed,
        };

        let dropped = self.frames_pending >= PENDING_CEILING && self.latest.is_some();
        if dropped {
            self.metrics.record_dropped();
        } else {
            self.frames_pending = self.frames_pending.saturating_add(1);
        }

        self.latest = Some(frame);
        self.cached_image = None;
        self.dirty = true;

        self.metrics.record_received(latency_ns.unwrap_or(0));

        if dropped {
            PushOutcome::DroppedOldest
        } else {
            PushOutcome::Decoded
        }
    }

    /// Number of pending (undrawn) decoded frames. Exposed for
    /// diagnostics / tests; production callers don't need to read this
    /// because `mark_drawn` clears it as a side-effect.
    #[cfg(test)]
    pub(crate) fn frames_pending(&self) -> u32 {
        self.frames_pending
    }

    /// Borrow the most recent decoded frame, if any.
    pub fn latest_frame(&self) -> Option<&DecodedFrame> {
        self.latest.as_ref()
    }

    /// Record a pointer motion event the shell should ship to the owner
    /// on the next tick. Coordinates are surface-local logical pixels.
    pub fn record_pointer_motion(&mut self, x: f64, y: f64) {
        self.pending_inputs.push(InputEvent::PointerMotion { x, y });
    }

    /// Record a pointer button transition.
    pub fn record_pointer_button(&mut self, button: u32, pressed: bool) {
        self.pending_inputs
            .push(InputEvent::PointerButton { button, pressed });
    }

    /// Record a key press/release. `modifiers` is the xkbcommon-style
    /// bitfield (Shift=1, Ctrl=4, Alt=8, Logo=64); callers that don't
    /// track modifiers can pass `0` since the owner's keycode stream
    /// recovers state itself.
    pub fn record_key(&mut self, keysym: u32, pressed: bool, modifiers: u32) {
        self.pending_inputs.push(InputEvent::Key {
            keysym,
            pressed,
            modifiers,
        });
    }

    /// Compat shim: records a key without modifier metadata. Existing call
    /// sites keep working without a rewrite; new call sites should prefer
    /// [`Self::record_key`] directly.
    pub fn record_key_no_mods(&mut self, keysym: u32, pressed: bool) {
        self.record_key(keysym, pressed, 0);
    }

    /// Drain the pending-input queue. The shell's per-frame tick calls
    /// this and ships each event over gossipsub.
    pub fn take_pending_inputs(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.pending_inputs)
    }

    /// Rebuild the cached `peniko::ImageData` from `latest` if needed
    /// (cheap no-op once populated). SDR RGBA frames upload straight
    /// through; HDR P010 frames are CPU tone-mapped
    /// ([`tonemap_p010_to_rgba8`]) into RGBA regardless of display HDR support.
    fn ensure_image(&mut self) -> Option<&vello::peniko::ImageData> {
        if self.cached_image.is_none() {
            if let Some(frame) = &self.latest {
                let rgba = match &frame.pixels {
                    DecodedPixels::Rgba8(bytes) => bytes.clone(),
                    DecodedPixels::P010 {
                        y,
                        uv,
                        y_stride,
                        uv_stride,
                    } => {
                        let cfg = self.hdr_tonemap.as_ref().copied().unwrap_or_default();
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
                self.cached_image = Some(vello::peniko::ImageData {
                    data: blob,
                    format: vello::peniko::ImageFormat::Rgba8,
                    alpha_type: vello::peniko::ImageAlphaType::Alpha,
                    width: frame.width,
                    height: frame.height,
                });
            }
        }
        self.cached_image.as_ref()
    }
}

impl Widget for PeerView {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .expect("peer-view leaf");
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).expect("layout");
        let new_rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
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
        // Fall through to the raised surface so a pre-first-frame view
        // still looks intentional rather than a hole in the chrome.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );

        let rect = self.rect;

        if let Some(image) = self.ensure_image() {
            // Scale + translate so the natural-size image fills `rect`.
            let img_w = image.width as f64;
            let img_h = image.height as f64;
            if img_w > 0.0 && img_h > 0.0 {
                let sx = rect.width() / img_w;
                let sy = rect.height() / img_h;
                let placement =
                    Affine::translate((rect.x0, rect.y0)) * Affine::scale_non_uniform(sx, sy);
                scene.draw_image(image, transform * placement);
            }
        } else {
            let label = "Awaiting peer surface…";
            let size = tokens.typography.scale.base;
            let (lw, lh) = text_renderer.measure(label, size, None);
            let cx = rect.center().x - lw as f64 / 2.0;
            let cy = rect.center().y - lh as f64 / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                size,
                tokens.colors.text_dim,
                transform * Affine::translate((cx, cy)),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Real winit→`InputEvent` translation lives at the shell level,
        // which calls `record_*` directly.
    }

    fn name(&self) -> &str {
        "PeerView"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// PeerView's pixels change only on a new decoded frame or a layout
    /// rect change; hover/focus do nothing visually. Overridden so the
    /// App's CursorMoved redraw gate can skip frames over an idle peer
    /// surface.
    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.frames_pending = 0;
    }

    /// PeerView's taffy node is a `percent(1.0)` leaf — its intrinsic
    /// size never changes, only its parent slot. Layout-clean after
    /// the first apply.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl AppWidget for PeerView {
    fn app_kind(&self) -> AppKind {
        // No dedicated `Peer` variant yet; `Settings` is the catch-all
        // glyph the shell uses for foreign tabs in `shell::to_ui_kind`.
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        // Globe glyph stands in for "remote / peer surface" until a
        // dedicated icon ships.
        IconDescriptor::Glyph("\u{f0ac}")
    }

    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::encoder::H264Encoder;

    /// Synthesise a deterministic RGBA gradient so the round-trip needs
    /// no binary fixture.
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

    /// Encode a frame via the encoder, push the NALU bytes through
    /// `PeerView::push_encoded`, then draw and confirm a `DrawTag::IMAGE`
    /// lands in the scene's encoding.
    #[test]
    fn decoded_frame_paints_image_into_scene() {
        use vello_encoding::DrawTag;
        const W: u32 = 64;
        const H: u32 = 64;
        let rgba = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&rgba).expect("encode");

        let mut view = PeerView::new(42, 7);
        let progressed = view.push_encoded(&nalus);
        assert!(progressed, "decoder produced no frame from encoder output");
        let latest = view.latest_frame().expect("frame stashed");
        assert_eq!(latest.width, W);
        assert_eq!(latest.height, H);
        assert!(
            latest.pixels.is_rgba(),
            "H.264 decoder should yield RGBA pixels"
        );

        // 100×100 rect at origin so the image has somewhere to blit.
        view.rect = Rect::new(0.0, 0.0, 100.0, 100.0);

        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        let theme = Theme::default();
        let presence = PresenceCtx::default();
        view.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let enc_ref = scene.encoding();
        let has_image = enc_ref.draw_tags.iter().any(|t| *t == DrawTag::IMAGE);
        assert!(
            has_image,
            "PeerView::draw emitted no image into the scene's draw stream"
        );
    }

    #[test]
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
        let has_image = enc_ref.draw_tags.iter().any(|t| *t == DrawTag::IMAGE);
        assert!(
            !has_image,
            "fresh PeerView with no decoded frame leaked an image draw"
        );
    }

    #[test]
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
        // Next drain is empty.
        assert!(view.take_pending_inputs().is_empty());
    }

    #[test]
    fn push_encoded_returns_false_for_empty_payload() {
        let mut view = PeerView::new(0, 0);
        assert!(!view.push_encoded(&[]));
        assert!(view.latest_frame().is_none());
    }

    /// Drop-oldest under backpressure — the third push (with two
    /// undrawn frames buffered) marks the older frame as dropped.
    #[test]
    fn push_encoded_with_latency_drops_oldest_after_ceiling() {
        const W: u32 = 64;
        const H: u32 = 64;
        let rgba = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&rgba).expect("encode");

        let metrics = PeerSurfaceMetrics::default();
        let mut view = PeerView::new_with_metrics(42, 7, metrics.clone());

        // First push — decoded, nothing dropped.
        assert_eq!(
            view.push_encoded_with_latency(&nalus, Some(1_000)),
            PushOutcome::Decoded,
        );
        assert_eq!(view.frames_pending(), 1);

        // Second push — still within the ceiling.
        assert_eq!(
            view.push_encoded_with_latency(&nalus, Some(1_500)),
            PushOutcome::Decoded,
        );
        assert_eq!(view.frames_pending(), 2);

        // Third push — ceiling is 2; the older frame drops. Metrics
        // increments `frames_dropped`.
        assert_eq!(
            view.push_encoded_with_latency(&nalus, Some(2_000)),
            PushOutcome::DroppedOldest,
        );
        let snap = metrics.snapshot();
        assert_eq!(snap.frames_dropped, 1);
        assert_eq!(snap.frames_received, 3, "every accepted decode counts");

        // `mark_drawn` clears the pending counter — the paint pass
        // caught up with the buffered decodes.
        view.mark_drawn();
        assert_eq!(view.frames_pending(), 0);

        // Next push after mark_drawn — no drop.
        assert_eq!(
            view.push_encoded_with_latency(&nalus, Some(2_500)),
            PushOutcome::Decoded,
        );
        let snap = metrics.snapshot();
        assert_eq!(
            snap.frames_dropped, 1,
            "no additional drops after mark_drawn"
        );
    }

    /// A stream-start label the receiver cannot honour must never cost the
    /// viewer the share. Not hypothetical: Fedora's `libavcodec-free`
    /// (which qOS images ship) is built
    /// `--disable-decoder='h264,hevc,vc1,vvc'`, so `HevcDecoder::new`
    /// fails on a stock image even with `hdr-decode` compiled in. Both
    /// labels are driven through the real constructor, and the resulting
    /// view is made to decode a real frame — "constructed something" isn't
    /// the same as "the share works".
    #[test]
    fn an_unsupported_stream_label_still_yields_a_working_peer_view() {
        const W: u32 = 64;
        const H: u32 = 64;
        let rgba = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&rgba).expect("encode");
        let caps = crate::shell::hdr_display::DisplayCapabilities::sdr_default();

        // "av1" is unknown in every build, so this fallback arm is
        // exercised regardless of what libavcodec was compiled with.
        let mut unknown = PeerView::new_for_stream(
            42,
            7,
            "av1",
            PeerSurfaceMetrics::default(),
            &caps,
            None,
        );
        assert!(
            matches!(unknown.decoder, VideoDecoder::H264(_)),
            "an unknown codec label must degrade to the SDR decoder"
        );
        assert!(
            unknown.push_encoded(&nalus),
            "the fallback decoder has to actually decode the SDR stream"
        );

        // "hevc-main10" is known, resolving to whichever of the two legal
        // outcomes this host allows; either way the view is usable.
        let mut hevc = PeerView::new_for_stream(
            43,
            7,
            "hevc-main10",
            PeerSurfaceMetrics::default(),
            &caps,
            None,
        );
        if matches!(hevc.decoder, VideoDecoder::H264(_)) {
            // Degraded path — what a stock qOS image does.
            assert!(
                hevc.push_encoded(&nalus),
                "the degraded HEVC view must still decode the SDR stream \
                 the sender is really emitting"
            );
        } else {
            // Real HEVC decoder — the backend genuinely builds.
            assert!(
                VideoDecoder::for_wire_codec("hevc-main10").is_ok(),
                "the HDR path is only reachable when the backend builds"
            );
        }
    }

    /// The metrics handle is [`Clone`]-shared — an OSD overlay can
    /// hold a copy and see counters advance without touching the widget.
    #[test]
    fn metrics_handle_is_shared_via_clone() {
        const W: u32 = 64;
        const H: u32 = 64;
        let rgba = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&rgba).expect("encode");

        let metrics = PeerSurfaceMetrics::default();
        let external = metrics.clone();
        let mut view = PeerView::new_with_metrics(1, 1, metrics);

        assert_eq!(external.snapshot().frames_received, 0);
        view.push_encoded_with_latency(&nalus, Some(500));
        assert_eq!(external.snapshot().frames_received, 1);
        assert_eq!(external.snapshot().latency_ns_mean, 500);
    }
}
