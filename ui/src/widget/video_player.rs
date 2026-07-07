//! VideoPlayer — a reusable video surface with a full playback-controls overlay.
//!
//! The widget paints the latest decoded picture scaled-to-fit and overlays a
//! controls bar: play/pause, a scrubbable seek bar with a buffered range,
//! current / total time, a volume control and a fullscreen toggle. Controls
//! auto-hide a few seconds after the last cursor movement; keyboard shortcuts
//! (Space play/pause, ←/→ seek, F fullscreen, M mute) work while the surface is
//! focused. Colours come from the [`Theme`], never hard-coded.
//!
//! ## Two frame sources, one surface
//!
//! * **Engine mode** (the shared path) — the widget owns a
//!   [`media::MediaEngine`]. Each [`Widget::sync`] pumps the engine, pulls the
//!   freshest frame, and reflects its playhead / duration / buffering. This is
//!   what the local video player, the editor preview and the video app all use,
//!   so streaming + collaborative playback land once. Attach one with
//!   [`VideoPlayer::with_engine`] / [`VideoPlayer::set_engine`].
//! * **Push mode** (legacy) — call sites feed raw H.264 NAL units via
//!   [`VideoPlayer::feed_frame`], decoded in-process by openh264. Preserved so
//!   existing consumers keep working while they migrate onto the engine.
//!
//! Both paths land the picture in the same `latest_frame`, so `draw` is shared.

use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::Widget;

/// How long the controls bar stays visible after the last cursor movement.
const CONTROLS_AUTO_HIDE: Duration = Duration::from_secs(3);

/// Height of the controls overlay band at the bottom of the rect.
const CONTROLS_BAR_HEIGHT: f64 = 44.0;

/// Side length of the square play/pause icon button at the left of the bar.
const PLAY_BUTTON_SIZE: f64 = 30.0;

/// Vertical thickness of the scrub track.
const SCRUB_TRACK_HEIGHT: f64 = 5.0;

/// Radius of the scrub thumb.
const SCRUB_THUMB_RADIUS: f64 = 7.0;

/// Width reserved for the volume control (speaker + level track).
const VOLUME_WIDTH: f64 = 78.0;

/// Seek step for the ←/→ keyboard shortcuts.
const KEY_SEEK_STEP: Duration = Duration::from_secs(5);

/// Fallback stream length (ms) used only in push mode, where the widget has no
/// engine to report a real duration.
const PUSH_MODE_DEFAULT_DURATION_MS: u64 = 60_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

/// Video surface with an overlaid playback-controls bar.
pub struct VideoPlayer {
    /// Shared streaming engine (engine mode). `None` in push mode.
    engine: Option<media::MediaEngine>,
    /// Legacy in-process H.264 decoder for [`Self::feed_frame`]. Lazily
    /// constructed on the first non-empty push so engine-mode consumers never
    /// touch the openh264 runtime.
    decoder: Option<openh264::decoder::Decoder>,
    /// Most recently produced picture, RGBA. Shared render cache for both the
    /// engine and push paths.
    latest_frame: Option<PenikoImage>,
    /// PTS of the frame currently cached in `latest_frame`, so engine-mode
    /// `sync` only rebuilds the peniko image when the picture actually changes.
    last_frame_pts: Option<u64>,
    /// Reflected playback state (engine-derived in engine mode; set by
    /// `play`/`pause` in push mode).
    state: PlaybackState,
    /// Playhead + stream metadata mirrored from the engine (or defaults in push
    /// mode).
    position_ms: u64,
    duration_ms: u64,
    /// Buffering / prefetch progress 0..=100 (the buffered-range indicator).
    buffering: u8,
    /// Output volume 0..=1 and mute flag.
    volume: f32,
    muted: bool,
    /// Fullscreen intent, and whether the user has ever toggled it (so a fresh
    /// player expresses no fullscreen opinion until asked — see
    /// [`Widget::fullscreen_intent`]).
    fullscreen: bool,
    fullscreen_touched: bool,
    /// Optional seek callback (push-mode consumers that own their own clock).
    on_seek: Option<Box<dyn FnMut(Duration)>>,
    /// Bounds + hit regions, recomputed each draw.
    rect: Rect,
    play_button_rect: Rect,
    scrub_track_rect: Rect,
    volume_track_rect: Rect,
    fullscreen_button_rect: Rect,
    /// Controls fade-out deadline. `None` means hidden.
    controls_visible_until: Option<Instant>,
    /// Whether the user is mid-drag on the scrub track.
    scrubbing: bool,
    last_cursor: Point,
    focused: bool,
    /// Repaint dirty flag (incremental redraw).
    dirty: bool,
    /// Stable e2e/test identifier, when set by the host.
    test_id: Option<&'static str>,
}

impl VideoPlayer {
    /// Construct a paused player with no engine, no decoder and no frame.
    pub fn new() -> Self {
        Self {
            engine: None,
            decoder: None,
            latest_frame: None,
            last_frame_pts: None,
            state: PlaybackState::Paused,
            position_ms: 0,
            duration_ms: PUSH_MODE_DEFAULT_DURATION_MS,
            buffering: 100,
            volume: 0.8,
            muted: false,
            fullscreen: false,
            fullscreen_touched: false,
            on_seek: None,
            rect: Rect::ZERO,
            play_button_rect: Rect::ZERO,
            scrub_track_rect: Rect::ZERO,
            volume_track_rect: Rect::ZERO,
            fullscreen_button_rect: Rect::ZERO,
            controls_visible_until: None,
            scrubbing: false,
            last_cursor: Point::ZERO,
            focused: false,
            dirty: true,
            test_id: None,
        }
    }

    /// Attach a shared [`media::MediaEngine`] (builder form). The widget then
    /// pulls frames + playhead from it every `sync`.
    pub fn with_engine(mut self, engine: media::MediaEngine) -> Self {
        self.set_engine(engine);
        self
    }

    /// Attach (or replace) the engine after construction.
    pub fn set_engine(&mut self, engine: media::MediaEngine) {
        self.volume = engine.volume();
        self.engine = Some(engine);
        self.dirty = true;
    }

    /// Mutable access to the engine, so the host can `open` a source or drive
    /// collaborative transport (`apply_transport` / `transport_state`).
    pub fn engine_mut(&mut self) -> Option<&mut media::MediaEngine> {
        self.engine.as_mut()
    }

    /// Shared read access to the engine.
    pub fn engine(&self) -> Option<&media::MediaEngine> {
        self.engine.as_ref()
    }

    /// Open a [`media::Source`] on the attached engine (no-op without one).
    pub fn open(&mut self, source: media::Source) {
        if let Some(engine) = &mut self.engine {
            engine.open(source);
            self.last_frame_pts = None;
            self.dirty = true;
        }
    }

    /// Give this player a stable identifier for the e2e test harness.
    pub fn with_test_id(mut self, id: &'static str) -> Self {
        self.test_id = Some(id);
        self
    }

    /// Register a seek callback (push-mode consumers). Receives the seek target
    /// as a `Duration` from the start.
    pub fn on_seek<F: FnMut(Duration) + 'static>(mut self, f: F) -> Self {
        self.on_seek = Some(Box::new(f));
        self
    }

    /// Push H.264 NALU bytes to the in-process decoder (push mode). Returns
    /// `true` when a frame was decoded and `latest_frame` updated.
    pub fn feed_frame(&mut self, nalus: &[u8]) -> bool {
        if nalus.is_empty() {
            return false;
        }
        if self.decoder.is_none() {
            match openh264::decoder::Decoder::new() {
                Ok(d) => self.decoder = Some(d),
                Err(_) => return false,
            }
        }
        let decoder = self.decoder.as_mut().expect("decoder just initialised");
        let yuv = match decoder.decode(nalus) {
            Ok(Some(yuv)) => yuv,
            Ok(None) | Err(_) => return false,
        };
        use openh264::formats::YUVSource;
        let (width, height) = yuv.dimensions();
        let mut rgba = vec![0u8; width * height * 4];
        yuv.write_rgba8(&mut rgba);
        self.latest_frame = Some(rgba_image(width as u32, height as u32, Arc::new(rgba)));
        self.dirty = true;
        true
    }

    /// Current playback state.
    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// Borrow the most recently produced frame, if any.
    pub fn latest_frame(&self) -> Option<&PenikoImage> {
        self.latest_frame.as_ref()
    }

    /// Whether playback is currently running (engine-aware).
    pub fn is_playing(&self) -> bool {
        match &self.engine {
            Some(e) => e.is_playing(),
            None => self.state == PlaybackState::Playing,
        }
    }

    /// Start (or resume) playback.
    pub fn play(&mut self) {
        if let Some(e) = &mut self.engine {
            e.play();
        }
        self.state = PlaybackState::Playing;
        self.dirty = true;
    }

    /// Pause playback.
    pub fn pause(&mut self) {
        if let Some(e) = &mut self.engine {
            e.pause();
        }
        self.state = PlaybackState::Paused;
        self.dirty = true;
    }

    /// Toggle play/pause.
    pub fn toggle_play(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// The current playhead in milliseconds.
    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    /// The total duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// Seek to an absolute playhead position in milliseconds (clamped).
    pub fn seek_to_ms(&mut self, ms: u64) {
        let target = ms.min(self.duration_ms);
        self.position_ms = target;
        if let Some(e) = &mut self.engine {
            e.seek_ms(target);
        }
        if let Some(cb) = self.on_seek.as_mut() {
            cb(Duration::from_millis(target));
        }
        self.dirty = true;
    }

    /// Set output volume 0..=1 (also unmutes).
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.muted = false;
        if let Some(e) = &mut self.engine {
            e.set_volume(self.volume);
        }
        self.dirty = true;
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    /// Toggle mute (engine volume drops to 0 while muted).
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if let Some(e) = &mut self.engine {
            e.set_volume(if self.muted { 0.0 } else { self.volume });
        }
        self.dirty = true;
    }

    /// Whether fullscreen is currently requested.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// Toggle fullscreen intent.
    pub fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        self.fullscreen_touched = true;
        self.dirty = true;
    }

    /// The effective output volume for the meter (0 while muted).
    fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    /// Force the controls overlay visible for a long while — used by the
    /// headless `--shots` path so a single captured frame shows the full
    /// controls bar (which otherwise auto-hides when no cursor is moving).
    pub fn reveal_controls(&mut self) {
        self.controls_visible_until = Some(Instant::now() + Duration::from_secs(3600));
        self.dirty = true;
    }

    fn controls_visible_now(&self) -> bool {
        match self.controls_visible_until {
            Some(deadline) => Instant::now() < deadline,
            None => false,
        }
    }

    fn show_controls(&mut self) {
        self.controls_visible_until = Some(Instant::now() + CONTROLS_AUTO_HIDE);
    }

    fn controls_bar_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            (self.rect.y1 - CONTROLS_BAR_HEIGHT).max(self.rect.y0),
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn playhead_fraction(&self) -> f64 {
        if self.duration_ms == 0 {
            0.0
        } else {
            (self.position_ms as f64 / self.duration_ms as f64).clamp(0.0, 1.0)
        }
    }

    /// Pull the freshest engine state into the widget's render caches. Split
    /// out of `sync` so the borrow of `self.engine` is fully released before
    /// the fields it mirrors into are written.
    fn pump_engine(&mut self) {
        let snapshot = self.engine.as_mut().map(|engine| {
            engine.poll();
            let frame = engine
                .latest_frame()
                .map(|f| (f.width, f.height, f.rgba.clone(), f.pts_ms));
            (
                engine.position_ms(),
                engine.duration_ms(),
                engine.buffering_percent(),
                engine.is_playing(),
                frame,
            )
        });
        let Some((pos, dur, buffering, playing, frame)) = snapshot else {
            return;
        };
        if self.position_ms != pos {
            self.position_ms = pos;
            self.dirty = true;
        }
        if let Some(d) = dur {
            if d > 0 && self.duration_ms != d {
                self.duration_ms = d;
                self.dirty = true;
            }
        }
        if self.buffering != buffering {
            self.buffering = buffering;
            self.dirty = true;
        }
        let new_state = if playing { PlaybackState::Playing } else { PlaybackState::Paused };
        if self.state != new_state {
            self.state = new_state;
            self.dirty = true;
        }
        if let Some((w, h, rgba, pts)) = frame {
            if self.last_frame_pts != Some(pts) || self.latest_frame.is_none() {
                self.last_frame_pts = Some(pts);
                self.latest_frame = Some(rgba_image(w, h, rgba));
                self.dirty = true;
            }
        }
    }

    /// Paint the frame + (conditionally) the controls overlay.
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // 1) Letterbox background.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // 2) Latest frame, contain-fit — or a placeholder when nothing is loaded.
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
                scene.fill(Fill::NonZero, transform, &image_brush, Some(brush_transform), &dest);
            }
        } else {
            self.paint_empty_placeholder(scene, transform, theme);
        }

        // 3) Controls overlay — only within the auto-hide window.
        if !self.controls_visible_now() {
            self.play_button_rect = Rect::ZERO;
            self.scrub_track_rect = Rect::ZERO;
            self.volume_track_rect = Rect::ZERO;
            self.fullscreen_button_rect = Rect::ZERO;
            return;
        }
        self.paint_controls(scene, transform, text_renderer, theme);
    }

    fn paint_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let bar = self.controls_bar_rect();
        if bar.width() <= 0.0 || bar.height() <= 0.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        let accent = colors.accent[ACCENT_PRIMARY_IDX];

        // Scrim so controls read over bright frames.
        scene.fill(Fill::NonZero, transform, Color::from_rgba8(0, 0, 0, 170), None, &bar);

        let pad = theme.tokens.spacing.s2 as f64;
        let mid_y = bar.y0 + bar.height() * 0.5;

        // --- Play / pause button (left) ---
        let icon = PLAY_BUTTON_SIZE.min(bar.height() - 8.0).max(0.0);
        let play_btn = Rect::new(bar.x0 + pad, mid_y - icon * 0.5, bar.x0 + pad + icon, mid_y + icon * 0.5);
        self.play_button_rect = play_btn;
        self.draw_play_glyph(scene, transform, play_btn, colors.text_primary);

        // --- Fullscreen button (far right) ---
        let fs_size = icon;
        let fs_btn = Rect::new(bar.x1 - pad - fs_size, mid_y - fs_size * 0.5, bar.x1 - pad, mid_y + fs_size * 0.5);
        self.fullscreen_button_rect = fs_btn;
        draw_fullscreen_glyph(scene, transform, fs_btn, colors.text_primary, self.fullscreen);

        // --- Volume control (left of fullscreen) ---
        let vol_x1 = fs_btn.x0 - pad;
        let vol_x0 = (vol_x1 - VOLUME_WIDTH).max(play_btn.x1);
        let speaker = Rect::new(vol_x0, mid_y - icon * 0.4, vol_x0 + icon * 0.8, mid_y + icon * 0.4);
        draw_speaker_glyph(scene, transform, speaker, colors.text_primary, self.muted);
        let vtrack_x0 = speaker.x1 + pad * 0.5;
        let vtrack = Rect::new(
            vtrack_x0,
            mid_y - SCRUB_TRACK_HEIGHT * 0.5,
            vol_x1,
            mid_y + SCRUB_TRACK_HEIGHT * 0.5,
        );
        if vtrack.width() > 0.0 {
            self.volume_track_rect = vtrack;
            let shape = RoundedRect::from_rect(vtrack, SCRUB_TRACK_HEIGHT * 0.5);
            scene.fill(Fill::NonZero, transform, colors.border_standard, None, &shape);
            let filled = Rect::new(
                vtrack.x0,
                vtrack.y0,
                vtrack.x0 + vtrack.width() * self.effective_volume() as f64,
                vtrack.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(filled, SCRUB_TRACK_HEIGHT * 0.5),
            );
        } else {
            self.volume_track_rect = Rect::ZERO;
        }

        // --- Time label + scrub track (centre) ---
        let time_text = format!(
            "{} / {}",
            format_ms(self.position_ms),
            format_ms(self.duration_ms)
        );
        let time_size = 12.0f32;
        let (tw, _th) = text_renderer.measure(&time_text, time_size, None);
        let time_x1 = vol_x0 - pad;
        let time_x0 = (time_x1 - tw as f64).max(play_btn.x1 + pad);
        text_renderer.draw_text(
            scene,
            &time_text,
            time_size,
            colors.text_dim,
            transform * Affine::translate((time_x0, mid_y - time_size as f64 * 0.5)),
            None,
        );

        let track_x0 = play_btn.x1 + pad;
        let track_x1 = time_x0 - pad;
        if track_x1 > track_x0 {
            let track = Rect::new(
                track_x0,
                mid_y - SCRUB_TRACK_HEIGHT * 0.5,
                track_x1,
                mid_y + SCRUB_TRACK_HEIGHT * 0.5,
            );
            self.scrub_track_rect = track;
            // Base track.
            scene.fill(
                Fill::NonZero,
                transform,
                colors.border_standard,
                None,
                &RoundedRect::from_rect(track, SCRUB_TRACK_HEIGHT * 0.5),
            );
            // Buffered range (lighter, up to buffering %).
            let buf_frac = (self.buffering as f64 / 100.0).clamp(0.0, 1.0);
            if buf_frac > 0.0 {
                let buffered = Rect::new(track.x0, track.y0, track.x0 + track.width() * buf_frac, track.y1);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.text_dim,
                    None,
                    &RoundedRect::from_rect(buffered, SCRUB_TRACK_HEIGHT * 0.5),
                );
            }
            // Played range (accent).
            let frac = self.playhead_fraction();
            let played = Rect::new(track.x0, track.y0, track.x0 + track.width() * frac, track.y1);
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(played, SCRUB_TRACK_HEIGHT * 0.5),
            );
            // Thumb.
            let thumb_cx = track.x0 + track.width() * frac;
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &Circle::new((thumb_cx, mid_y), SCRUB_THUMB_RADIUS),
            );
        } else {
            self.scrub_track_rect = Rect::ZERO;
        }

        // Outline so the bar reads as a distinct surface.
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &bar);
    }

    /// Paint the empty-surface placeholder: a dim ringed play glyph centred in
    /// the letterbox, so a player with no media reads as intentionally empty
    /// ("nothing loaded — Open a video") rather than a blank/broken black rect.
    fn paint_empty_placeholder(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let dim = theme.tokens.colors.text_dim;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let r = (self.rect.width().min(self.rect.height()) * 0.08).clamp(16.0, 44.0);
        if r <= 0.0 {
            return;
        }
        // Faint ring.
        scene.stroke(&Stroke::new(2.0), transform, dim, None, &Circle::new((cx, cy), r));
        // Play triangle inside it, nudged right so it reads centred.
        let s = r * 0.62;
        let mut p = BezPath::new();
        p.move_to((cx - s * 0.35, cy - s * 0.62));
        p.line_to((cx + s * 0.7, cy));
        p.line_to((cx - s * 0.35, cy + s * 0.62));
        p.close_path();
        scene.fill(Fill::NonZero, transform, dim, None, &p);
    }

    fn draw_play_glyph(&self, scene: &mut Scene, transform: Affine, r: Rect, color: Color) {
        let cx = r.center().x;
        let cy = r.center().y;
        let s = r.height().min(r.width()) * 0.5;
        if self.is_playing() {
            // Two vertical bars.
            let bw = s * 0.34;
            let gap = s * 0.30;
            let bar1 = Rect::new(cx - gap - bw, cy - s * 0.6, cx - gap, cy + s * 0.6);
            let bar2 = Rect::new(cx + gap, cy - s * 0.6, cx + gap + bw, cy + s * 0.6);
            scene.fill(Fill::NonZero, transform, color, None, &bar1);
            scene.fill(Fill::NonZero, transform, color, None, &bar2);
        } else {
            // Right-pointing triangle.
            let mut p = BezPath::new();
            p.move_to((cx - s * 0.5, cy - s * 0.7));
            p.line_to((cx + s * 0.7, cy));
            p.line_to((cx - s * 0.5, cy + s * 0.7));
            p.close_path();
            scene.fill(Fill::NonZero, transform, color, None, &p);
        }
    }
}

/// Build a peniko RGBA image from tightly-packed bytes.
fn rgba_image(width: u32, height: u32, rgba: Arc<Vec<u8>>) -> PenikoImage {
    PenikoImage {
        data: Blob::new(rgba),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }
}

/// Vector speaker glyph; draws a mute cross when `muted`.
fn draw_speaker_glyph(scene: &mut Scene, transform: Affine, r: Rect, color: Color, muted: bool) {
    let s = r.height();
    let x0 = r.x0;
    let cy = r.center().y;
    // Body: a small rectangle + a triangular cone.
    let mut body = BezPath::new();
    body.move_to((x0, cy - s * 0.18));
    body.line_to((x0 + s * 0.22, cy - s * 0.18));
    body.line_to((x0 + s * 0.5, cy - s * 0.42));
    body.line_to((x0 + s * 0.5, cy + s * 0.42));
    body.line_to((x0 + s * 0.22, cy + s * 0.18));
    body.line_to((x0, cy + s * 0.18));
    body.close_path();
    scene.fill(Fill::NonZero, transform, color, None, &body);
    if muted {
        let x1 = x0 + s * 0.62;
        let x2 = x0 + s * 0.9;
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((x1, cy - s * 0.2), (x2, cy + s * 0.2)));
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((x1, cy + s * 0.2), (x2, cy - s * 0.2)));
    } else {
        // Two sound-wave arcs approximated by short strokes.
        let wx = x0 + s * 0.6;
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((wx, cy - s * 0.22), (wx + s * 0.12, cy)));
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((wx + s * 0.12, cy), (wx, cy + s * 0.22)));
    }
}

/// Vector fullscreen glyph: four corner brackets (enter) or inward brackets
/// (exit). Kept vector so it never depends on a glyph being in the font.
fn draw_fullscreen_glyph(scene: &mut Scene, transform: Affine, r: Rect, color: Color, active: bool) {
    let stroke = Stroke::new(1.8);
    let m = r.height() * 0.28;
    let corners = [
        (r.x0, r.y0, 1.0, 1.0),
        (r.x1, r.y0, -1.0, 1.0),
        (r.x0, r.y1, 1.0, -1.0),
        (r.x1, r.y1, -1.0, -1.0),
    ];
    for (x, y, sx, sy) in corners {
        // When active (currently fullscreen) draw inward-pointing brackets.
        let (ax, ay) = if active { (x + sx * m, y + sy * m) } else { (x, y) };
        let dir = if active { -1.0 } else { 1.0 };
        scene.stroke(
            &stroke,
            transform,
            color,
            None,
            &Line::new((ax, ay), (ax + sx * m * dir, ay)),
        );
        scene.stroke(
            &stroke,
            transform,
            color,
            None,
            &Line::new((ax, ay), (ax, ay + sy * m * dir)),
        );
    }
}

/// Format milliseconds as `M:SS` (or `H:MM:SS` past an hour).
fn format_ms(ms: u64) -> String {
    let total = ms / 1000;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl Default for VideoPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for VideoPlayer {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        self.pump_engine();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::auto(), height: Dimension::auto() },
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
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = logical;
                if self.scrubbing {
                    self.seek_from_cursor();
                    self.show_controls();
                    self.dirty = true;
                    *handled = true;
                } else if self.rect.contains(logical) {
                    self.show_controls();
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if !self.rect.contains(self.last_cursor) {
                    return;
                }
                // Clicking the surface focuses it for keyboard shortcuts.
                *focus_target = Some(node_id);
                if !self.controls_visible_now() {
                    // First click just reveals the controls.
                    self.show_controls();
                    self.dirty = true;
                    *handled = true;
                    return;
                }
                if self.play_button_rect.contains(self.last_cursor) {
                    self.toggle_play();
                    *handled = true;
                } else if self.fullscreen_button_rect.contains(self.last_cursor) {
                    self.toggle_fullscreen();
                    *handled = true;
                } else if self.volume_track_rect.contains(self.last_cursor)
                    && self.volume_track_rect.width() > 0.0
                {
                    let frac = ((self.last_cursor.x - self.volume_track_rect.x0)
                        / self.volume_track_rect.width())
                    .clamp(0.0, 1.0);
                    self.set_volume(frac as f32);
                    *handled = true;
                } else if self.scrub_track_rect.contains(self.last_cursor)
                    && self.scrub_track_rect.width() > 0.0
                {
                    self.scrubbing = true;
                    self.seek_from_cursor();
                    *handled = true;
                }
                self.show_controls();
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.scrubbing {
                    self.scrubbing = false;
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: key_event, .. }
                if self.focused && key_event.state == ElementState::Pressed =>
            {
                match &key_event.logical_key {
                    Key::Named(NamedKey::Space) => {
                        self.toggle_play();
                        self.show_controls();
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        let step = KEY_SEEK_STEP.as_millis() as u64;
                        self.seek_to_ms(self.position_ms.saturating_sub(step));
                        self.show_controls();
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let step = KEY_SEEK_STEP.as_millis() as u64;
                        self.seek_to_ms(self.position_ms.saturating_add(step));
                        self.show_controls();
                        *handled = true;
                    }
                    Key::Character(c) if c.eq_ignore_ascii_case("f") => {
                        self.toggle_fullscreen();
                        *handled = true;
                    }
                    Key::Character(c) if c.eq_ignore_ascii_case("m") => {
                        self.toggle_mute();
                        self.show_controls();
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "VideoPlayer"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.controls_visible_now()
            && (self.play_button_rect.contains(point)
                || self.fullscreen_button_rect.contains(point)
                || self.scrub_track_rect.contains(point)
                || self.volume_track_rect.contains(point))
        {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn fullscreen_intent(&self) -> Option<bool> {
        // No opinion until the user first toggles fullscreen, so a freshly
        // launched player doesn't force the compositor's window state.
        if self.fullscreen_touched {
            Some(self.fullscreen)
        } else {
            None
        }
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        // The compositor granted or (e.g. via a global keybind) exited
        // fullscreen — reflect it so our button + intent stay truthful.
        if self.fullscreen != active {
            self.fullscreen = active;
            self.fullscreen_touched = true;
            self.dirty = true;
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep pumping while playing (to pull frames + advance the scrubber) and
        // while the controls are fading so the auto-hide actually repaints.
        self.is_playing() || self.controls_visible_now()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Accessibility: a media player surface. `Role::Group` named "Video
    /// player"; `busy` while playing.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name("Video player")
                .with_state(AccessState {
                    busy: self.is_playing(),
                    ..Default::default()
                }),
        )
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl VideoPlayer {
    /// Seek to the playhead the cursor's X maps to on the scrub track.
    fn seek_from_cursor(&mut self) {
        if self.scrub_track_rect.width() <= 0.0 {
            return;
        }
        let frac = ((self.last_cursor.x - self.scrub_track_rect.x0)
            / self.scrub_track_rect.width())
        .clamp(0.0, 1.0);
        let target = (frac * self.duration_ms as f64) as u64;
        self.seek_to_ms(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;

    fn press(key: Key) -> WindowEvent {
        // Same construction idiom as the block_editor tests: zero a KeyEvent
        // (its `platform_specific` field has no public constructor) then set the
        // fields we assert on.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn dispatch(vp: &mut VideoPlayer, ev: &WindowEvent) -> bool {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus_target = None;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        vp.handle_event(ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus_target, node, &taffy);
        handled
    }

    #[test]
    fn video_player_starts_paused() {
        let vp = VideoPlayer::new();
        assert_eq!(vp.state(), PlaybackState::Paused);
        assert!(!vp.is_playing());
    }

    #[test]
    fn engine_mode_sync_pulls_a_frame_and_metadata() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        vp.sync(&mut doc, &root);
        assert!(vp.latest_frame().is_some(), "engine frame pulled into the surface");
        assert!(vp.duration_ms() > 0, "duration mirrored from the engine");
    }

    #[test]
    fn space_toggles_play_on_focused_engine_player() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_focus(true);
        assert!(!vp.is_playing());
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::Space))));
        assert!(vp.is_playing(), "Space started playback via the engine");
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::Space))));
        assert!(!vp.is_playing(), "Space paused it again");
    }

    #[test]
    fn arrow_keys_seek_the_engine_playhead() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_focus(true);
        vp.seek_to_ms(20_000);
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::ArrowRight))));
        assert_eq!(vp.position_ms(), 25_000, "→ seeks forward 5s");
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::ArrowLeft))));
        assert_eq!(vp.position_ms(), 20_000, "← seeks back 5s");
        // The seek propagated to the engine, not just the widget cache.
        assert_eq!(vp.engine().unwrap().position_ms(), 20_000);
    }

    #[test]
    fn scrub_click_seeks_to_the_track_fraction() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.duration_ms = 100_000;
        // Reveal + lay out the controls by painting a frame.
        vp.show_controls();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(vp.scrub_track_rect.width() > 0.0, "scrub track laid out");
        // Click at the horizontal midpoint of the track → ~50% of duration.
        let mid_x = vp.scrub_track_rect.x0 + vp.scrub_track_rect.width() * 0.5;
        let mid_y = vp.scrub_track_rect.center().y;
        dispatch(&mut vp, &cursor_moved(mid_x, mid_y));
        let handled = dispatch(&mut vp, &mouse_press());
        assert!(handled, "scrub press handled");
        let pos = vp.position_ms();
        assert!((45_000..=55_000).contains(&pos), "seeked to ~50%: {pos}ms");
    }

    #[test]
    fn f_key_toggles_fullscreen_intent() {
        let mut vp = VideoPlayer::new();
        vp.set_focus(true);
        assert_eq!(Widget::fullscreen_intent(&vp), None, "no opinion until asked");
        assert!(dispatch(&mut vp, &press(Key::Character("f".into()))));
        assert_eq!(Widget::fullscreen_intent(&vp), Some(true));
        assert!(dispatch(&mut vp, &press(Key::Character("F".into()))));
        assert_eq!(Widget::fullscreen_intent(&vp), Some(false));
    }

    #[test]
    fn accessibility_reports_group_role_and_busy_while_playing() {
        use crate::a11y::Role;
        let mut vp = VideoPlayer::new();
        let node = Widget::accessibility(&vp).expect("player is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Video player"));
        assert!(!node.state.busy);
        vp.play();
        assert!(Widget::accessibility(&vp).unwrap().state.busy);
    }

    /// Push mode still works: a round-tripped openh264 frame lands in
    /// `latest_frame` without any engine attached.
    #[test]
    fn push_mode_feed_frame_updates_latest_image() {
        use openh264::encoder::Encoder;
        use openh264::formats::YUVBuffer;

        let w = 16usize;
        let h = 16usize;
        let total = w * h + 2 * ((w * h) / 4);
        let plane = vec![128u8; total];
        let yuv = YUVBuffer::from_vec(plane, w, h);
        let mut enc = Encoder::new().expect("openh264 encoder");
        let encoded = enc.encode(&yuv).expect("encode grey frame").to_vec();
        assert!(!encoded.is_empty());

        let mut vp = VideoPlayer::new();
        assert!(vp.feed_frame(&encoded), "decoder consumed the frame");
        assert!(vp.latest_frame().is_some());
    }

    #[test]
    fn empty_surface_paints_a_placeholder_and_no_image() {
        use vello_encoding::DrawTag;
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 320.0, 240.0);
        // No frame loaded: the empty state must draw a placeholder, never an image.
        assert!(vp.latest_frame().is_none());
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
        assert_eq!(image_count, 0, "no frame → no image draw in the empty state");
        // The letterbox fill + the ring + the play triangle are all painted.
        assert!(
            scene.encoding().draw_tags.len() >= 2,
            "empty state still paints a visible placeholder"
        );
    }

    #[test]
    fn paint_with_image_uses_image_draw_command() {
        use vello_encoding::DrawTag;
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 64.0, 48.0);
        vp.latest_frame = Some(rgba_image(16, 16, Arc::new(vec![255u8; 4 * 16 * 16])));
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
        assert!(image_count >= 1, "expected an IMAGE draw, got {image_count}");
    }
}
