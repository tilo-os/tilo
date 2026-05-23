//! VideoPlayer — H.264 frame consumer with overlay playback controls.
//!
//! The widget owns a lazily-constructed [`openh264::decoder::Decoder`]. Call
//! sites push NALU bytes via [`VideoPlayer::feed_frame`]; the decoder emits a
//! [`peniko::ImageData`] (RGBA) which is then painted scaled-to-fit on every draw.
//!
//! Controls (play/pause button + scrub track) overlay the bottom of the rect.
//! They auto-hide three seconds after the last `CursorMoved` event so the
//! video can fill the rect during normal playback. Hovering re-arms the
//! visibility timer.
//!
//! State semantics:
//! * `PlaybackState::Paused` is the construction default — the widget owns no
//!   playback clock of its own, it just reflects whether the consumer is
//!   actively feeding frames.
//! * `latest_frame` is the most recently decoded picture; cleared only when
//!   the consumer drops the widget. A `Stopped` state leaves the last frame
//!   on screen.
//!
//! The decoder is constructed lazily on the first non-empty `feed_frame`
//! call. Owning the openh264 decoder up-front would force the (relatively
//! expensive) lib loader to fire at widget-construction time, which makes
//! examples and tests slower than they need to be.

use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::Widget;

/// How long the controls bar stays visible after the last cursor movement.
/// Matches the YouTube / VLC convention of a three-second fade-out.
const CONTROLS_AUTO_HIDE: Duration = Duration::from_secs(3);

/// Height of the controls overlay band at the bottom of the rect.
const CONTROLS_BAR_HEIGHT: f64 = 40.0;

/// Side length of the square play/pause icon button at the left of the
/// controls bar.
const PLAY_BUTTON_SIZE: f64 = 32.0;

/// Vertical thickness of the scrub track. Thin enough to read as a "track"
/// rather than a slider.
const SCRUB_TRACK_HEIGHT: f64 = 4.0;

/// Radius of the scrub thumb. Half a thumb hangs above the track and half
/// below for a centred handle.
const SCRUB_THUMB_RADIUS: f64 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

/// H.264 video surface with overlaid playback controls.
pub struct VideoPlayer {
    /// Lazily-initialised on the first non-empty `feed_frame` call. Held
    /// in `Option` so widget construction never touches the openh264
    /// runtime — tests / examples that never feed frames pay nothing.
    decoder: Option<openh264::decoder::Decoder>,
    /// Most recently decoded picture, RGBA. `peniko::ImageData` is `Arc`-backed
    /// so handing the same image to the scene every frame is cheap.
    latest_frame: Option<PenikoImage>,
    /// Current playback state. The widget never advances this on its own;
    /// it tracks whatever the consumer set last via `play`/`pause`.
    state: PlaybackState,
    /// Optional seek callback invoked when the user clicks on the scrub
    /// track. Receives the seek target as a `Duration` from the start.
    on_seek: Option<Box<dyn FnMut(Duration)>>,
    /// Bounds of the widget in screen-space, populated by `apply_layout`.
    rect: Rect,
    /// Hit-region for the play/pause icon button, recomputed every draw.
    play_button_rect: Rect,
    /// Hit-region for the scrub track, recomputed every draw.
    scrub_track_rect: Rect,
    /// Time at which the controls overlay will fade out. `None` means
    /// "already hidden" (the initial state — overlays don't blink on at
    /// construction).
    controls_visible_until: Option<Instant>,
    /// Last cursor position seen, in logical coordinates. Captured so the
    /// click handler can reuse it without waiting for another move event.
    last_cursor: Point,
}

impl VideoPlayer {
    /// Construct a paused player with no decoder and no frame. Cheap —
    /// the openh264 lib loader isn't touched until the first frame
    /// arrives.
    pub fn new() -> Self {
        Self {
            decoder: None,
            latest_frame: None,
            state: PlaybackState::Paused,
            on_seek: None,
            rect: Rect::ZERO,
            play_button_rect: Rect::ZERO,
            scrub_track_rect: Rect::ZERO,
            controls_visible_until: None,
            last_cursor: Point::ZERO,
        }
    }

    /// Register a callback invoked when the user clicks on the scrub
    /// track. The callback receives the target playback position as a
    /// `Duration` (the widget assumes a placeholder media length of one
    /// minute when computing this — the real length will be supplied via
    /// a setter once playback timing is wired in).
    pub fn on_seek<F: FnMut(Duration) + 'static>(mut self, f: F) -> Self {
        self.on_seek = Some(Box::new(f));
        self
    }

    /// Push H.264 NALU bytes to the decoder. Returns `true` when a frame
    /// was successfully extracted (and `latest_frame` updated), `false`
    /// otherwise — including the empty-input case, which we explicitly
    /// short-circuit so a no-op call never wakes openh264.
    pub fn feed_frame(&mut self, nalus: &[u8]) -> bool {
        if nalus.is_empty() {
            return false;
        }

        // Lazy init. `Decoder::new()` uses the in-tree source build (via the
        // `source` feature on the openh264 dep), so it returns
        // `Result<Decoder, _>` rather than panicking.
        if self.decoder.is_none() {
            match openh264::decoder::Decoder::new() {
                Ok(d) => self.decoder = Some(d),
                Err(_) => return false,
            }
        }

        let decoder = self.decoder.as_mut().expect("decoder just initialised");
        let yuv = match decoder.decode(nalus) {
            Ok(Some(yuv)) => yuv,
            // `Ok(None)` is the "buffering, no frame yet" path (typical
            // before SPS/PPS land); `Err` is a malformed bitstream. Both
            // are equally non-actionable from the widget's perspective so
            // they collapse into `false`.
            Ok(None) | Err(_) => return false,
        };

        // `dimensions()` lives on the `YUVSource` trait — pull it into
        // scope so the method-call resolves on `DecodedYUV`.
        use openh264::formats::YUVSource;
        let (width, height) = yuv.dimensions();
        let mut rgba = vec![0u8; width * height * 4];
        yuv.write_rgba8(&mut rgba);

        let blob = Blob::new(Arc::new(rgba));
        let img = PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: width as u32,
            height: height as u32,
        };
        self.latest_frame = Some(img);
        true
    }

    /// Current playback state. Reflects the last `play`/`pause` call;
    /// never advanced internally by the widget.
    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// Borrow the most recently decoded frame, if any.
    pub fn latest_frame(&self) -> Option<&PenikoImage> {
        self.latest_frame.as_ref()
    }

    /// Flip the widget to `Playing`. Idempotent.
    pub fn play(&mut self) {
        self.state = PlaybackState::Playing;
    }

    /// Flip the widget to `Paused`. Idempotent.
    pub fn pause(&mut self) {
        self.state = PlaybackState::Paused;
    }

    /// Whether the controls overlay should paint this frame. Driven by
    /// `controls_visible_until` — `None` (initial) and any expired
    /// deadline both hide the bar.
    fn controls_visible_now(&self) -> bool {
        match self.controls_visible_until {
            Some(deadline) => Instant::now() < deadline,
            None => false,
        }
    }

    /// Compute the controls bar rect (bottom strip) given the current
    /// widget rect.
    fn controls_bar_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            (self.rect.y1 - CONTROLS_BAR_HEIGHT).max(self.rect.y0),
            self.rect.x1,
            self.rect.y1,
        )
    }

    /// Paint the frame and (conditionally) the controls overlay. Split
    /// out from the `Widget::draw` impl so tests can hit it directly
    /// without spinning up taffy.
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // 1) Solid background — fills letterbox bars when the video's
        //    aspect doesn't match the widget rect.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // 2) Latest frame, contain-fit to preserve aspect ratio.
        if let Some(img) = self.latest_frame.clone() {
            let iw = img.width as f64;
            let ih = img.height as f64;
            let rw = self.rect.width();
            let rh = self.rect.height();
            if iw > 0.0 && ih > 0.0 && rw > 0.0 && rh > 0.0 {
                let scale = (rw / iw).min(rh / ih);
                let dw = iw * scale;
                let dh = ih * scale;
                let dx = self.rect.x0 + (rw - dw) * 0.5;
                let dy = self.rect.y0 + (rh - dh) * 0.5;
                let brush_transform =
                    Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
                let dest = Rect::new(dx, dy, dx + dw, dy + dh);
                let image_brush = vello::peniko::ImageBrush::new(img);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    &image_brush,
                    Some(brush_transform),
                    &dest,
                );
            }
        }

        // 3) Controls overlay — only when within the auto-hide window.
        if !self.controls_visible_now() {
            // Reset hit-regions to empty rects so an off-screen click
            // can't accidentally hit a stale bounds while the bar is
            // invisible.
            self.play_button_rect = Rect::ZERO;
            self.scrub_track_rect = Rect::ZERO;
            return;
        }

        let bar = self.controls_bar_rect();
        if bar.width() <= 0.0 || bar.height() <= 0.0 {
            return;
        }

        // Semi-transparent backdrop so the controls are readable over
        // bright video frames.
        let bar_bg = Color::from_rgba8(0, 0, 0, 160);
        scene.fill(Fill::NonZero, transform, bar_bg, None, &bar);

        // Inset the controls horizontally by the standard padding token
        // so the icon and track don't kiss the rect edges.
        let pad = theme.tokens.spacing.s2 as f64;
        let icon_size = PLAY_BUTTON_SIZE.min(bar.height() - 4.0).max(0.0);
        let icon_top = bar.y0 + (bar.height() - icon_size) * 0.5;
        let play_btn = Rect::new(
            bar.x0 + pad,
            icon_top,
            bar.x0 + pad + icon_size,
            icon_top + icon_size,
        );
        self.play_button_rect = play_btn;

        // Play/pause glyph — chosen from `state`. The Unicode triangle /
        // double-bar render through the standard text renderer; sizing
        // matches the icon button height minus 8 px of breathing room.
        let glyph = match self.state {
            PlaybackState::Playing => "\u{275A}\u{275A}", // double bar
            PlaybackState::Paused | PlaybackState::Stopped => "\u{25B6}", // triangle
        };
        let glyph_size = (icon_size * 0.55).max(8.0) as f32;
        let (gw, _gh) = text_renderer.measure(glyph, glyph_size, None);
        let gx = play_btn.center().x - gw as f64 * 0.5;
        let gy = play_btn.y0 + (play_btn.height() - glyph_size as f64) * 0.5;
        text_renderer.draw_text(
            scene,
            glyph,
            glyph_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((gx, gy)),
            None,
        );

        // Scrub track: thin rounded rect spanning the rest of the bar.
        let track_x0 = play_btn.x1 + pad;
        let track_x1 = bar.x1 - pad;
        if track_x1 > track_x0 {
            let track_cy = bar.y0 + bar.height() * 0.5;
            let track = Rect::new(
                track_x0,
                track_cy - SCRUB_TRACK_HEIGHT * 0.5,
                track_x1,
                track_cy + SCRUB_TRACK_HEIGHT * 0.5,
            );
            self.scrub_track_rect = track;
            let track_shape = RoundedRect::from_rect(track, SCRUB_TRACK_HEIGHT * 0.5);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.border_standard,
                None,
                &track_shape,
            );

            // Thumb pinned to the start of the track for now (no real
            // playhead clock yet). The accent fill keeps the affordance
            // visible against the dim track colour.
            let thumb_cx = track_x0;
            let thumb_cy = track_cy;
            let thumb = vello::kurbo::Circle::new((thumb_cx, thumb_cy), SCRUB_THUMB_RADIUS);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.accent[ACCENT_PRIMARY_IDX],
                None,
                &thumb,
            );
        } else {
            self.scrub_track_rect = Rect::ZERO;
        }

        // Subtle 1 px outline around the bar so the overlay reads as a
        // separate surface from the video underneath.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &bar,
        );
    }
}

impl Default for VideoPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for VideoPlayer {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::auto(),
                },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = logical;
                if self.rect.contains(logical) {
                    // Any cursor movement over the widget extends the
                    // visibility deadline by another three seconds.
                    self.controls_visible_until =
                        Some(Instant::now() + CONTROLS_AUTO_HIDE);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Left,
                ..
            } => {
                if !self.controls_visible_now() {
                    return;
                }
                if self.play_button_rect.contains(self.last_cursor) {
                    // Toggle play/pause on icon click.
                    self.state = match self.state {
                        PlaybackState::Playing => PlaybackState::Paused,
                        PlaybackState::Paused | PlaybackState::Stopped => {
                            PlaybackState::Playing
                        }
                    };
                    *handled = true;
                } else if self.scrub_track_rect.contains(self.last_cursor)
                    && self.scrub_track_rect.width() > 0.0
                {
                    // Translate the click X into a fraction of the track,
                    // then into a `Duration` using a placeholder one-
                    // minute total length. The real length will arrive
                    // via a setter when the media-pipeline integration
                    // lands.
                    let frac = ((self.last_cursor.x - self.scrub_track_rect.x0)
                        / self.scrub_track_rect.width())
                    .clamp(0.0, 1.0);
                    let target = Duration::from_secs_f64(60.0 * frac);
                    if let Some(cb) = self.on_seek.as_mut() {
                        cb(target);
                    }
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "VideoPlayer"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;

    /// A freshly-constructed `VideoPlayer` must report `Paused`. The
    /// widget never starts playing on its own — the consumer flips it
    /// once frames are queued.
    #[test]
    fn video_player_starts_paused() {
        let vp = VideoPlayer::new();
        assert_eq!(vp.state(), PlaybackState::Paused);
    }

    /// Round-trip a tiny encoded frame through openh264's in-tree
    /// encoder and assert that feeding the resulting NALU bytes back
    /// into the decoder produces a `latest_frame`.
    ///
    /// We do this rather than hand-crafting a bitstream because the
    /// openh264 0.9 `source` feature already provides both the
    /// encoder and the decoder — synthesising a valid SPS/PPS/IDR
    /// triple manually would be fragile.
    #[test]
    fn video_player_feed_frame_updates_latest_image() {
        use openh264::encoder::Encoder;
        use openh264::formats::YUVBuffer;

        // 16×16 mid-grey frame. YUV 4:2:0 means width and height must be
        // multiples of 2 — 16 satisfies that comfortably.
        //
        // Layout of the buffer that `YUVBuffer::from_vec` accepts: a
        // single `(3 * w * h) / 2`-byte plane laid out as `[Y..., U...,
        // V...]`. We fill Y with mid-grey (128) and leave U/V at neutral
        // chroma (128).
        let w = 16usize;
        let h = 16usize;
        let y_len = w * h;
        let uv_len = (w * h) / 4;
        let total = y_len + 2 * uv_len;
        let mut plane = vec![128u8; total];
        // Chroma planes at 128 = neutral (no colour cast); already done
        // by the `vec![128u8; total]` initialiser, but the explicit
        // overwrite below documents intent for future readers.
        for c in &mut plane[y_len..] {
            *c = 128;
        }
        let yuv = YUVBuffer::from_vec(plane, w, h);

        let mut enc = Encoder::new().expect("openh264 encoder construction");
        let bitstream = enc.encode(&yuv).expect("encoding the grey frame");
        let encoded = bitstream.to_vec();
        assert!(!encoded.is_empty(), "encoder produced bytes");

        let mut vp = VideoPlayer::new();
        let decoded = vp.feed_frame(&encoded);
        assert!(decoded, "decoder consumed the round-tripped frame");
        assert!(vp.latest_frame().is_some(), "latest_frame populated");
    }

    /// Painting a `VideoPlayer` whose `latest_frame` is set must emit at
    /// least one `DrawTag::IMAGE` in the resulting vello encoding — the
    /// frame texture goes through `scene.fill(.., &PenikoImage, ..)`,
    /// which is what `DrawTag::IMAGE` encodes.
    #[test]
    fn video_player_paint_with_image_uses_image_draw_command() {
        use vello_encoding::DrawTag;

        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 64.0, 48.0);

        // Inject a 16×16 white RGBA fixture directly — mirrors the
        // pattern used by `widget::image::tests` for the same kind of
        // encoding-level assertion.
        let blob = Blob::new(Arc::new(vec![255u8; 4 * 16 * 16]));
        vp.latest_frame = Some(PenikoImage {
            data: blob,
            format: Format::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 16,
            height: 16,
        });

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let image_count = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::IMAGE)
            .count();
        assert!(
            image_count >= 1,
            "expected at least one DrawTag::IMAGE, got {}",
            image_count
        );
    }
}
