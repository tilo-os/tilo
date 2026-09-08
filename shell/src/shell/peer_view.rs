//! Peer-side foreign-surface view — the receive-side counterpart to the
//! owner's encode loop in [`crate::network::streaming`]. Owns one
//! [`crate::network::H264Decoder`] and paints the most recently decoded
//! RGBA frame as a vello image draw.
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
/// exceed this, further pushes drop the oldest. Two is the minimum
/// useful ceiling — one paint-in-flight, one staged for the next frame.
pub(crate) const PENDING_CEILING: u32 = 2;

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
    /// The decoder rejected the payload. Nothing recorded.
    DecodeFailed,
}

/// Peer-side widget rendering one shared surface: the decoder, the latest
/// decoded frame as a peniko image, and the pending-input queue.
pub struct PeerView {
    /// Cached `AppWidget::app_title` result, materialised at construction.
    title: String,
    /// Surface id the owner uses to address this share.
    surface_id: SurfaceId,
    /// Local peer id (the *viewer*) — the value the owner sees as the
    /// `peer` field on `RemoteInput`.
    local_peer: PeerId,
    /// Active decoder — H.264 for SDR, HEVC Main10 for HDR. SDR default
    /// so legacy peers without a codec label still work.
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
}

impl PeerView {
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
            title: format!("Peer surface #{surface_id}"),
            surface_id,
            local_peer,
            decoder: VideoDecoder::H264(crate::network::decoder::H264Decoder::new()),
            latest: None,
            hdr_tonemap: None,
            cached_image: None,
            pending_inputs: Vec::new(),
            input_sink: None,
            pointer_inside: false,
            keyboard_focused: false,
            rect: Rect::ZERO,
            dirty: true,
            metrics,
            frames_pending: 0,
        }
    }

    /// Picks the decoder from `SurfaceStreamStart.codec` (`"h264"` SDR,
    /// `"hevc-main10"` HDR). Unknown/unsupported labels fall back to SDR
    /// rather than the share silently vanishing.
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
        // Tone-map only runs HDR-into-SDR-output.
        let stream_colorimetry = if matches!(decoder, VideoDecoder::H264(_)) {
            Colorimetry::sdr_bt709()
        } else {
            Colorimetry::hdr10()
        };
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
            title: format!("Peer surface #{surface_id}"),
            surface_id,
            local_peer,
            decoder,
            latest: None,
            hdr_tonemap,
            cached_image: None,
            pending_inputs: Vec::new(),
            input_sink: None,
            pointer_inside: false,
            keyboard_focused: false,
            rect: Rect::ZERO,
            dirty: true,
            metrics,
            frames_pending: 0,
        }
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
    pub fn push_encoded(&mut self, nalus: &[u8]) -> bool {
        matches!(
            self.push_encoded_with_latency(nalus, None),
            PushOutcome::Decoded | PushOutcome::DroppedOldest,
        )
    }

    /// `latency_ns` is the caller-computed motion-to-photon delta; `None`
    /// still records a zero-latency sample. Drop-oldest: at
    /// [`PENDING_CEILING`] the previous latest is dropped but the new
    /// frame still becomes `latest` — the drop is on the paint side.
    #[tracing::instrument(level = "trace", skip(self, nalus), fields(nalu_bytes = nalus.len()))]
    pub fn push_encoded_with_latency(
        &mut self,
        nalus: &[u8],
        latency_ns: Option<u64>,
    ) -> PushOutcome {
        let Some(frame) = self.decoder.decode(nalus) else {
            return PushOutcome::DecodeFailed;
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

    /// Compat shim; prefer [`Self::record_key`] for new call sites.
    pub fn record_key_no_mods(&mut self, keysym: u32, pressed: bool) {
        self.record_key(keysym, pressed, 0);
    }

    pub fn take_pending_inputs(&mut self) -> Vec<InputEvent> {
        std::mem::take(&mut self.pending_inputs)
    }

    pub fn set_input_sink(&mut self, sink: Arc<dyn Fn(InputEvent) + Send + Sync>) {
        self.input_sink = Some(sink);
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
        let node = match taffy.new_leaf(taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        }) {
            Ok(node) => node,
            Err(err) => {
                // `TaffyTree::new_leaf` only ever returns `Ok` for a
                // fresh, unattached leaf; this arm exists so a future
                // upstream change degrades the peer surface instead of
                // aborting the whole session.
                tracing::error!("peer_view: leaf allocation failed: {err}");
                NodeId::from(0u64)
            }
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        let new_rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
                scene.draw_image(image, placement);
            }
        } else {
            let label = "Awaiting peer surface…";
            let size = tokens.typography.scale.base;
            let (lw, lh) = text_renderer.measure(label, size, None);
            let cx = rect.center().x - f64::from(lw) / 2.0;
            let cy = rect.center().y - f64::from(lh) / 2.0;
            text_renderer.draw_text(
                scene,
                label,
                size,
                tokens.colors.text_dim,
                transform.pre_translate((cx, cy).into()),
                None,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        _scale_factor: f64,
        modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x, position.y);
                let inside = point.x >= self.rect.x0
                    && point.x <= self.rect.x1
                    && point.y >= self.rect.y0
                    && point.y <= self.rect.y1;
                self.set_pointer_inside(inside);
                if inside && self.rect.width() > 0.0 && self.rect.height() > 0.0 {
                    let (width, height) = self.latest.as_ref().map_or_else(
                        || (self.rect.width(), self.rect.height()),
                        |frame| (f64::from(frame.width), f64::from(frame.height)),
                    );
                    let x = ((point.x - self.rect.x0) / self.rect.width() * width)
                        .clamp(0.0, width.max(1.0) - 1.0);
                    let y = ((point.y - self.rect.y0) / self.rect.height() * height)
                        .clamp(0.0, height.max(1.0) - 1.0);
                    self.record_pointer_motion(x, y);
                    *handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } => self.set_pointer_inside(false),
            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = matches!(state, winit::event::ElementState::Pressed);
                if pressed {
                    self.set_keyboard_focused(self.pointer_inside);
                }
                if (self.pointer_inside || self.keyboard_focused)
                    && let Some(button) = crate::compositor::client_input::evdev_button(*button)
                {
                    self.record_pointer_button(button, pressed);
                    *handled = true;
                    if pressed {
                        *focus_target = Some(node_id);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if self.keyboard_focused => {
                if let Some(keycode) = crate::compositor::client_input::evdev_keycode(event) {
                    self.record_key(
                        keycode,
                        matches!(event.state, winit::event::ElementState::Pressed),
                        Self::modifier_bits(*modifiers),
                    );
                    *handled = true;
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

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.frames_pending = 0;
    }

    /// A `percent(1.0)` leaf — layout-clean after the first apply.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl AppWidget for PeerView {
    fn app_kind(&self) -> AppKind {
        // No dedicated `Peer` variant yet; `Settings` is the catch-all.
        AppKind::Settings
    }

    fn app_title(&self) -> &str {
        &self.title
    }

    fn app_icon(&self) -> IconDescriptor {
        // Stands in for "remote / peer surface" until a dedicated icon ships.
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
        let has_image = enc_ref.draw_tags.contains(&DrawTag::IMAGE);
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
        assert!(view.take_pending_inputs().is_empty());
    }

    #[test]
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

    #[test]
    fn push_encoded_returns_false_for_empty_payload() {
        let mut view = PeerView::new(0, 0);
        assert!(!view.push_encoded(&[]));
        assert!(view.latest_frame().is_none());
    }

    #[test]
    fn push_encoded_with_latency_drops_oldest_after_ceiling() {
        const W: u32 = 64;
        const H: u32 = 64;
        let rgba = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&rgba).expect("encode");

        let metrics = PeerSurfaceMetrics::default();
        let mut view = PeerView::new_with_metrics(42, 7, metrics.clone());

        assert_eq!(
            view.push_encoded_with_latency(&nalus, Some(1_000)),
            PushOutcome::Decoded,
        );
        assert_eq!(view.frames_pending(), 1);

        assert_eq!(
            view.push_encoded_with_latency(&nalus, Some(1_500)),
            PushOutcome::Decoded,
        );
        assert_eq!(view.frames_pending(), 2);

        // Ceiling is 2; the older frame drops.
        assert_eq!(
            view.push_encoded_with_latency(&nalus, Some(2_000)),
            PushOutcome::DroppedOldest,
        );
        let snap = metrics.snapshot();
        assert_eq!(snap.frames_dropped, 1);
        assert_eq!(snap.frames_received, 3, "every accepted decode counts");

        view.mark_drawn();
        assert_eq!(view.frames_pending(), 0);

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

    /// A stream-start label the receiver can't honour must never cost the
    /// viewer the share. Not hypothetical: Fedora's `libavcodec-free`
    /// (which qOS images ship) is built
    /// `--disable-decoder='h264,hevc,vc1,vvc'`, so `HevcDecoder::new`
    /// fails on a stock image.
    #[test]
    fn an_unsupported_stream_label_still_yields_a_working_peer_view() {
        const W: u32 = 64;
        const H: u32 = 64;
        let rgba = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&rgba).expect("encode");
        let caps = crate::shell::hdr_display::DisplayCapabilities::sdr_default();

        let mut unknown =
            PeerView::new_for_stream(42, 7, "av1", PeerSurfaceMetrics::default(), &caps, None);
        assert!(
            matches!(unknown.decoder, VideoDecoder::H264(_)),
            "an unknown codec label must degrade to the SDR decoder"
        );
        assert!(
            unknown.push_encoded(&nalus),
            "the fallback decoder has to actually decode the SDR stream"
        );

        let mut hevc = PeerView::new_for_stream(
            43,
            7,
            "hevc-main10",
            PeerSurfaceMetrics::default(),
            &caps,
            None,
        );
        if matches!(hevc.decoder, VideoDecoder::H264(_)) {
            assert!(
                hevc.push_encoded(&nalus),
                "the degraded HEVC view must still decode the SDR stream \
                 the sender is really emitting"
            );
        } else {
            assert!(
                VideoDecoder::for_wire_codec("hevc-main10").is_ok(),
                "the HDR path is only reachable when the backend builds"
            );
        }
    }

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
