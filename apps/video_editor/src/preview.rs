//! The composited-frame producer for the preview canvas.
//!
//! Given the project and a playhead time, this computes a [`CompositedFrame`]
//! — a small, render-agnostic description of what the preview should show:
//! the source clip, its colour grade (brightness/contrast/saturation), the
//! fade/opacity envelope, and the transition flash. The widget layer
//! ([`crate::app`]) turns that into Vello fills.
//!
//! ## Real vs. gated decode
//!
//! Two ways the canvas can be filled:
//!
//!   * **Representative frame (default / `--shots`)** — no decoder. The
//!     frame is synthesised from clip *metadata* (palette + grade), so the
//!     preview is deterministic and needs no media pipeline. This is what
//!     the headless CI box and screenshots use.
//!   * **Real decode (`--features decode`)** — [`PreviewDecoder`] resolves the
//!     clip's bytes through the [`crate::media`] FUSE seam and, when they're
//!     hydrated, seeks a shared [`media::MediaEngine`] (a real `GStreamer`
//!     `playbin` pipeline under the feature) to the clip's *source time* and
//!     pulls the true decoded RGBA frame. If the bytes aren't hydrated yet (a
//!     projected reference on a fresh collaborator) it falls back to the
//!     representative frame — the editor never blocks on hydration.
//!
//! The colour math here is the single source of truth for *both* the
//! representative gradient and (eventually) a real shader, so the grade you
//! see in the offline preview matches the graded export.

use crate::model::{Clip, ClipEffect, EffectKind, Filters, MediaId, Project, Transition};

/// A normalised RGB triple in 0..1. Render-agnostic so the math is testable
/// without a GPU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Rgb {
    pub fn new(r: f64, g: f64, b: f64) -> Self {
        Self {
            r: r.clamp(0.0, 1.0),
            g: g.clamp(0.0, 1.0),
            b: b.clamp(0.0, 1.0),
        }
    }
    pub fn luma(&self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
    /// Apply a per-clip [`Filters`] grade. Brightness scales, contrast
    /// pivots around mid-grey, saturation lerps toward/away from luma.
    #[must_use]
    #[allow(
        clippy::many_single_char_names,
        reason = "r/g/b are the RGB channels and f the Filters grade being applied, the \
                  standard vocabulary for colour math"
    )]
    pub fn graded(self, f: &Filters) -> Rgb {
        let bright = Filters::gain(f.brightness);
        let contrast = Filters::gain(f.contrast);
        let sat = Filters::gain(f.saturation);
        let apply = |c: f64| {
            let c = c * bright; // brightness
            (c - 0.5) * contrast + 0.5 // contrast pivot
        };
        let (mut r, mut g, mut b) = (apply(self.r), apply(self.g), apply(self.b));
        let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        r = l + (r - l) * sat;
        g = l + (g - l) * sat;
        b = l + (b - l) * sat;
        Rgb::new(r, g, b)
    }
    /// Scale toward black by `gain` (the fade/opacity envelope).
    #[must_use]
    pub fn faded(self, gain: f64) -> Rgb {
        let g = gain.clamp(0.0, 1.0);
        Rgb::new(self.r * g, self.g * g, self.b * g)
    }

    /// Apply an ordered stack of [`ClipEffect`]s, in list order, after the grade.
    #[must_use]
    pub fn with_effects(self, effects: &[ClipEffect]) -> Rgb {
        effects.iter().fold(self, apply_effect)
    }
}

/// Blend `a` toward `b` by `t` (0..1).
fn lerp(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    Rgb::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

/// Apply one [`ClipEffect`] to a colour. The single source of truth for both the
/// representative preview and the (gated) export, so the look matches.
fn apply_effect(c: Rgb, e: &ClipEffect) -> Rgb {
    let a = e.amount.clamp(0.0, 1.0);
    match e.kind {
        EffectKind::Grayscale => {
            let l = c.luma();
            lerp(c, Rgb::new(l, l, l), a)
        }
        EffectKind::Sepia => {
            let sr = 0.393 * c.r + 0.769 * c.g + 0.189 * c.b;
            let sg = 0.349 * c.r + 0.686 * c.g + 0.168 * c.b;
            let sb = 0.272 * c.r + 0.534 * c.g + 0.131 * c.b;
            lerp(c, Rgb::new(sr, sg, sb), a)
        }
        EffectKind::Invert => lerp(c, Rgb::new(1.0 - c.r, 1.0 - c.g, 1.0 - c.b), a),
        EffectKind::Warm => Rgb::new(c.r + a * 0.16, c.g + a * 0.04, c.b - a * 0.16),
        EffectKind::Cool => Rgb::new(c.r - a * 0.16, c.g + a * 0.04, c.b + a * 0.16),
        // A flat representative colour has no spatial extent, so a vignette
        // reads as a gentle overall darkening here; the real spatial falloff
        // lands in the (gated) shader/export.
        EffectKind::Vignette => lerp(c, Rgb::new(0.0, 0.0, 0.0), a * 0.4),
    }
}

/// The transition flash overlaid on a clip's head, as a colour + alpha the
/// widget paints over the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flash {
    pub white: bool,
    pub alpha: f64,
}

/// What the preview canvas should render at the playhead.
#[derive(Clone, Debug, PartialEq)]
pub enum CompositedFrame {
    /// No clip under the playhead — letterbox only.
    Empty,
    /// A clip is visible. The widget paints a representative grade unless a
    /// real decoded frame is supplied (see `decoded`).
    Clip {
        media: MediaId,
        /// The clip's base palette colour, already graded + faded — what a
        /// flat representative frame fills.
        base: Rgb,
        /// Combined fade-in/out × opacity envelope in 0..1.
        envelope: f64,
        /// Optional transition flash at the head.
        flash: Option<Flash>,
        /// `true` when a real decoded picture is available (decode feature
        /// + hydrated bytes). The widget blits it instead of the gradient.
        decoded: bool,
    },
}

/// Compute the composited frame at `playhead`. Pure — no decoding, no I/O.
/// The `decoded` flag is always `false` here; [`decode_frame`] sets it when
/// the decode feature resolves real bytes.
pub fn composite(project: &Project, playhead: f64) -> CompositedFrame {
    let Some(clip) = project.top_video_clip_at(playhead) else {
        return CompositedFrame::Empty;
    };
    let base = palette_rgb(clip.palette)
        .graded(&clip.filters)
        .with_effects(&clip.effects);
    let envelope = clip.envelope(playhead) * clip.opacity;
    let flash = transition_flash(clip, playhead);
    CompositedFrame::Clip {
        media: clip.media,
        base: base.faded(envelope),
        envelope,
        flash,
        decoded: false,
    }
}

/// The head-transition flash for `clip` at `playhead`, if any. A dissolve /
/// fade flashes white, a dip flashes black; both decay over 0.6s.
fn transition_flash(clip: &Clip, playhead: f64) -> Option<Flash> {
    if clip.transition == Transition::None {
        return None;
    }
    let into = playhead - clip.start;
    if !(0.0..0.6).contains(&into) {
        return None;
    }
    let alpha = (1.0 - into / 0.6) * 0.65;
    Some(Flash {
        white: clip.transition != Transition::DipToBlack,
        alpha,
    })
}

/// A deterministic base colour per palette slot, in linear 0..1. Mirrors
/// the theme palette indices used by `clip_color` so the representative
/// frame agrees with the timeline clip body.
pub fn palette_rgb(palette: u8) -> Rgb {
    match palette % 7 {
        0 => Rgb::new(0.30, 0.55, 0.95),
        1 => Rgb::new(0.25, 0.70, 0.90),
        2 => Rgb::new(0.30, 0.78, 0.50),
        3 => Rgb::new(0.95, 0.72, 0.28),
        4 => Rgb::new(0.55, 0.45, 0.95),
        5 => Rgb::new(0.92, 0.35, 0.40),
        _ => Rgb::new(0.55, 0.58, 0.62),
    }
}

/// The source time (seconds into the media) the preview must decode for the
/// clip under the playhead: the clip's in-point plus how far the playhead has
/// travelled into the clip on the timeline.
pub fn source_time_for(clip: &Clip, playhead: f64) -> f64 {
    (clip.in_point + (playhead - clip.start)).max(0.0)
}

/// A decoded RGBA picture ready to blit onto the preview canvas.
pub type DecodedFrame = ::media::VideoFrame;

/// Stateful preview decoder: owns a shared [`media::MediaEngine`] and, for the
/// clip under the playhead, seeks to the clip's source time and pulls the real
/// decoded frame. The engine's synthesized fallback needs no gstreamer, so this
/// type compiles on every build; with `--features decode` (which turns on
/// `media/decode`) the engine is a real `GStreamer` `playbin` pipeline and the
/// frame is the clip's true picture.
///
/// The extraction seam ([`Self::update`]) — top-clip selection, FUSE resolution,
/// the hydration gate, engine open/seek/poll, and frame caching — is exercised
/// hermetically by the synthesized backend in tests, and drives the real
/// pipeline unchanged under the decode feature.
pub struct PreviewDecoder {
    engine: ::media::MediaEngine,
    open_path: Option<std::path::PathBuf>,
    last_seek_ms: Option<u64>,
    last_seek_at: std::time::Instant,
    /// True once a frame for the current seek target has landed and the engine
    /// has been paused to hold it (so the preview is a static frame at the
    /// playhead, not free-running playback).
    settled: bool,
    frame: Option<DecodedFrame>,
    frame_media: Option<MediaId>,
}

impl PreviewDecoder {
    /// Build a decoder. Under `--features decode` this stands up a real
    /// `GStreamer` pipeline; otherwise the engine's synthesized fallback (which
    /// this preview never blits — the widget shows the representative frame when
    /// decode is off).
    pub fn new() -> Self {
        Self::from_engine(::media::MediaEngine::new())
    }

    /// Force the synthesized backend regardless of build features — the seam
    /// test drives this so extraction is exercised without a real decode.
    pub fn new_synthesized() -> Self {
        Self::from_engine(::media::MediaEngine::new_synthesized())
    }

    fn from_engine(engine: ::media::MediaEngine) -> Self {
        Self {
            engine,
            open_path: None,
            last_seek_ms: None,
            last_seek_at: std::time::Instant::now(),
            settled: false,
            frame: None,
            frame_media: None,
        }
    }

    /// Whether the engine is decoding through a real `GStreamer` pipeline.
    pub fn is_real_decode(&self) -> bool {
        self.engine.is_real_decode()
    }

    /// The last decoded frame, if one is cached for the current clip.
    pub fn frame(&self) -> Option<&DecodedFrame> {
        self.frame.as_ref()
    }

    /// Which media the cached frame belongs to.
    pub fn frame_media(&self) -> Option<MediaId> {
        self.frame_media
    }

    /// Point the decoder at the clip under `playhead` and pump one step,
    /// returning `true` when a decoded frame for that clip is available.
    ///
    /// Gracefully yields `false` (and the widget falls back to the
    /// representative frame) when there is no clip, the reference is not
    /// hydrated yet, or the pipeline has not prerolled a frame — the editor
    /// never blocks on decode.
    pub fn update(&mut self, project: &Project, playhead: f64) -> bool {
        let Some(clip) = project.top_video_clip_at(playhead).cloned() else {
            self.frame = None;
            self.frame_media = None;
            return false;
        };
        let Some(item) = project.media(clip.media) else {
            self.frame = None;
            self.frame_media = None;
            return false;
        };
        let source = item.source();
        if !source.is_decodable() {
            // Projected reference: bytes not hydrated yet — no real frame.
            self.frame = None;
            self.frame_media = None;
            return false;
        }
        let abs = source.abs_path;
        if self.open_path.as_deref() != Some(abs.as_path()) {
            self.engine.open(crate::media::engine_source(&abs));
            self.open_path = Some(abs);
            self.last_seek_ms = None;
            self.settled = false;
            self.frame = None;
        }
        let target_ms = (source_time_for(&clip, playhead) * 1000.0).max(0.0) as u64;
        // Drive the frame at the playhead by seeking, then briefly *playing* so
        // the decoder pushes the frame at the seek target to the sink, then
        // pausing to hold it static (the editor's own clock owns the playhead;
        // the pipeline must not free-run). Seek on a genuine target change, with
        // a rate-limited retry because a freshly-opened pipeline reaches its
        // ready state asynchronously and an early seek can be dropped.
        let target_changed = self.last_seek_ms != Some(target_ms);
        let stale_retry =
            !self.settled && self.last_seek_at.elapsed() > std::time::Duration::from_millis(150);
        if target_changed || stale_retry {
            self.engine.seek_ms(target_ms);
            self.engine.play();
            self.last_seek_ms = Some(target_ms);
            self.last_seek_at = std::time::Instant::now();
            self.settled = false;
        }
        self.engine.poll();
        if let Some(f) = self.engine.latest_frame() {
            self.frame = Some(f.clone());
            self.frame_media = Some(clip.media);
            if !self.settled {
                // Hold the frame: stop the pipeline advancing past the playhead.
                self.engine.pause();
                self.settled = true;
            }
        }
        self.frame_media == Some(clip.media) && self.frame.is_some()
    }
}

impl Default for PreviewDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Rasterize the composited representative frame at `playhead` into a tightly
/// packed `width*height*4` RGBA buffer — the **same** grade / fade / transition
/// math the preview canvas shows, so a `gst` export encodes exactly what the
/// timeline previews. Pure, so the export pipeline can push these straight into
/// an `appsrc`.
pub fn render_rgba(project: &Project, playhead: f64, width: u32, height: u32) -> Vec<u8> {
    let w = width.max(1) as usize;
    let h = height.max(1) as usize;
    let mut buf = vec![0u8; w * h * 4];

    let (bands, env, flash) = match composite(project, playhead) {
        CompositedFrame::Empty => (None, 1.0, None),
        CompositedFrame::Clip {
            base,
            envelope,
            flash,
            ..
        } => (Some(base), envelope, flash),
    };

    let black = Rgb::new(0.0, 0.0, 0.0);
    for y in 0..h {
        let yf = y as f64 / h as f64;
        for x in 0..w {
            let mut px = match bands {
                None => black,
                Some(base) => {
                    // Mirror draw_preview: darkened base, a lighter upper band,
                    // and a darker mid band.
                    let mut c = mix(base, black, 0.18);
                    if yf < 0.5 {
                        c = mix(c, mix(base, Rgb::new(1.0, 1.0, 1.0), 0.18), 0.55);
                    }
                    if (0.62..0.72).contains(&yf) {
                        c = mix(c, mix(base, black, 0.15), 0.6);
                    }
                    c
                }
            };
            // Fade/opacity envelope dims toward black.
            if env < 0.999 {
                px = mix(px, black, 1.0 - env);
            }
            // Transition flash.
            if let Some(f) = flash {
                let target = if f.white {
                    Rgb::new(1.0, 1.0, 1.0)
                } else {
                    black
                };
                px = mix(px, target, f.alpha);
            }
            let i = (y * w + x) * 4;
            if let Some(px_bytes) = buf.get_mut(i..).and_then(<[u8]>::first_chunk_mut::<4>) {
                *px_bytes = [
                    (px.r * 255.0).round().clamp(0.0, 255.0) as u8,
                    (px.g * 255.0).round().clamp(0.0, 255.0) as u8,
                    (px.b * 255.0).round().clamp(0.0, 255.0) as u8,
                    255,
                ];
            }
        }
    }
    buf
}

/// Per-channel linear interpolation between two colours.
fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    Rgb::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::sample_project;

    #[test]
    fn empty_when_no_clip_covers_playhead() {
        let p = sample_project();
        // Far past the end of every clip.
        assert_eq!(composite(&p, 10_000.0), CompositedFrame::Empty);
    }

    #[test]
    fn picks_the_top_video_clip() {
        let p = sample_project();
        // At t=21..27 the sunset insert on V2 overlays the V1 spine.
        match composite(&p, 23.0) {
            CompositedFrame::Clip { media, .. } => {
                let top = p.top_video_clip_at(23.0).unwrap();
                assert_eq!(media, top.media);
            }
            CompositedFrame::Empty => panic!("expected a clip at the playhead"),
        }
    }

    #[test]
    fn fade_in_darkens_the_head() {
        let p = sample_project();
        // The drone clip on V1 has a 1.5s fade-in starting at t=0.
        let CompositedFrame::Clip { envelope: head, .. } = composite(&p, 0.05) else {
            panic!("expected clip");
        };
        let CompositedFrame::Clip { envelope: mid, .. } = composite(&p, 5.0) else {
            panic!("expected clip");
        };
        assert!(
            head < mid,
            "head ({head}) should be dimmer than mid ({mid})"
        );
        assert!(head < 0.2, "very head of a fade-in is near-black");
    }

    #[test]
    fn grade_changes_the_base_colour() {
        let neutral = palette_rgb(0);
        let brighter = neutral.graded(&Filters {
            brightness: 0.9,
            contrast: 0.5,
            saturation: 0.5,
        });
        assert!(brighter.luma() > neutral.luma(), "brightness raises luma");
        let desat = neutral.graded(&Filters {
            brightness: 0.5,
            contrast: 0.5,
            saturation: 0.0,
        });
        // Full desaturation collapses channels toward luma → near-equal.
        assert!((desat.r - desat.g).abs() < 1e-6 && (desat.g - desat.b).abs() < 1e-6);
    }

    #[test]
    fn grayscale_effect_collapses_channels_and_empty_is_a_noop() {
        use crate::model::{ClipEffect, EffectKind};
        let c = palette_rgb(0); // a saturated bluish clip
        assert_eq!(
            c.with_effects(&[]),
            c,
            "no effects leaves the colour unchanged"
        );
        let g = c.with_effects(&[ClipEffect::new(EffectKind::Grayscale)]);
        // Grayscale pulls the channels strongly toward luma (near-equal).
        assert!(
            (g.r - g.g).abs() < 0.12 && (g.g - g.b).abs() < 0.12,
            "grayscale converges the channels: {g:?}",
        );
        // Invert flips the colour.
        let inv = c.with_effects(&[ClipEffect {
            kind: EffectKind::Invert,
            amount: 1.0,
        }]);
        assert!((inv.r - (1.0 - c.r)).abs() < 1e-6);
    }

    #[test]
    fn effects_stack_in_order_and_change_the_composite() {
        use crate::model::{ClipEffect, EffectKind};
        let mut p = sample_project();
        // The drone open (clips[0]) carries seeded effects; strip them and prove
        // adding a grayscale effect desaturates its composite base.
        let cid = p.clips[0].id;
        p.clip_mut(cid).unwrap().effects.clear();
        let CompositedFrame::Clip { base: before, .. } = composite(&p, 6.0) else {
            panic!("expected clip");
        };
        p.clip_mut(cid).unwrap().effects = vec![ClipEffect {
            kind: EffectKind::Grayscale,
            amount: 1.0,
        }];
        let CompositedFrame::Clip { base: after, .. } = composite(&p, 6.0) else {
            panic!("expected clip");
        };
        let spread = |c: Rgb| (c.r - c.g).abs() + (c.g - c.b).abs();
        assert!(
            spread(after) < spread(before),
            "grayscale reduces channel spread in the composite"
        );
    }

    #[test]
    fn identity_grade_is_a_near_noop() {
        let c = palette_rgb(3);
        let g = c.graded(&Filters::default());
        assert!((c.r - g.r).abs() < 1e-9 && (c.g - g.g).abs() < 1e-9 && (c.b - g.b).abs() < 1e-9);
    }

    #[test]
    fn transition_flash_decays_to_none() {
        let p = sample_project();
        // V2 sunset dissolves in at t=21.
        let CompositedFrame::Clip { flash: early, .. } = composite(&p, 21.05) else {
            panic!()
        };
        assert!(early.is_some(), "flash present right at the cut");
        let CompositedFrame::Clip { flash: late, .. } = composite(&p, 24.0) else {
            panic!()
        };
        assert!(late.is_none(), "flash gone well past the cut");
    }

    #[test]
    fn source_time_maps_playhead_into_the_clip_window() {
        use crate::model::{MediaKind, Project, Track, TrackKind};
        use uuid::Uuid;
        let mut p = Project::new("t");
        let mid = Uuid::new_v4();
        p.media.push(crate::model::MediaItem {
            id: mid,
            name: "s".into(),
            kind: MediaKind::Video,
            duration: 20.0,
            width: 16,
            height: 9,
            fps: 30.0,
            palette: 0,
            path: crate::media::workspace_path("s.mp4"),
            hydration: crate::media::Hydration::Projected,
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
        });
        // Clip placed at timeline 5s, sourcing in_point=3s.
        let cid = p.add_clip(tid, mid, 5.0).unwrap();
        p.clip_mut(cid).unwrap().in_point = 3.0;
        p.clip_mut(cid).unwrap().out_point = 13.0;
        let clip = p.clip(cid).unwrap();
        // Playhead 5s (clip head) → source 3s; playhead 9s → source 7s.
        assert!((source_time_for(clip, 5.0) - 3.0).abs() < 1e-9);
        assert!((source_time_for(clip, 9.0) - 7.0).abs() < 1e-9);
    }

    // Build a one-clip project whose media points at a real file on disk, so
    // the decoder's hydration gate reports it decodable.
    fn project_referencing(
        path: &std::path::Path,
    ) -> (crate::model::Project, crate::model::MediaId) {
        use crate::model::{MediaKind, Project, Track, TrackKind};
        use uuid::Uuid;
        let mut p = Project::new("t");
        let mid = Uuid::new_v4();
        p.media.push(crate::model::MediaItem {
            id: mid,
            name: "clip".into(),
            kind: MediaKind::Video,
            duration: 30.0,
            width: 16,
            height: 9,
            fps: 30.0,
            palette: 0,
            path: path.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
        });
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 20.0;
        (p, mid)
    }

    #[test]
    fn decoder_extracts_a_frame_through_the_engine_seam() {
        // Hermetic proof of the extraction seam on the synthesized backend: a
        // real file on disk (so the hydration gate passes), a real seek + poll,
        // and a real RGBA frame keyed to the right clip. The gst backend runs
        // the identical `update` path under `--features decode`.
        let dir = std::env::temp_dir();
        let f = dir.join(format!("qos-ve-dec-{}.mp4", std::process::id()));
        std::fs::write(&f, b"pretend-bytes").unwrap();
        let (project, mid) = project_referencing(&f);

        let mut dec = PreviewDecoder::new_synthesized();
        let got = dec.update(&project, 2.0);
        assert!(got, "a frame is produced for the clip under the playhead");
        assert_eq!(
            dec.frame_media(),
            Some(mid),
            "frame is keyed to the clip's media"
        );
        let frame = dec.frame().expect("cached frame");
        assert_eq!(
            frame.rgba.len(),
            frame.width as usize * frame.height as usize * 4
        );
        assert!(frame.width > 0 && frame.height > 0, "a real, sized picture");

        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn decoder_yields_nothing_for_an_unhydrated_reference() {
        // A projected reference (bytes not local) must not fabricate a frame —
        // the widget then shows the representative frame, never blocks.
        use crate::model::{MediaKind, Project, Track, TrackKind};
        use uuid::Uuid;
        let mut p = Project::new("t");
        let mid = Uuid::new_v4();
        p.media.push(crate::model::MediaItem {
            id: mid,
            name: "ghost".into(),
            kind: MediaKind::Video,
            duration: 10.0,
            width: 16,
            height: 9,
            fps: 30.0,
            palette: 0,
            // Absolute path that does not exist → resolve reports Projected.
            path: "/definitely/not/here-preview.mp4".into(),
            hydration: crate::media::Hydration::Projected,
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
        });
        p.add_clip(tid, mid, 0.0);
        let mut dec = PreviewDecoder::new_synthesized();
        assert!(
            !dec.update(&p, 1.0),
            "no frame for an un-hydrated reference"
        );
        assert!(dec.frame().is_none());
    }

    #[test]
    fn render_rgba_produces_a_sized_opaque_buffer_reflecting_the_grade() {
        let p = sample_project();
        // Inside the graded drone open (brighter than a neutral clip elsewhere).
        let buf = render_rgba(&p, 5.0, 32, 18);
        assert_eq!(buf.len(), 32 * 18 * 4);
        assert!(
            buf.chunks(4).all(|px| px[3] == 255),
            "every pixel is opaque"
        );
        // Not a uniform black frame — the composited clip contributes colour.
        let any_colour = buf.chunks(4).any(|px| px[0] > 8 || px[1] > 8 || px[2] > 8);
        assert!(any_colour, "the graded clip renders visible colour");
    }

    // Real GStreamer round-trip: encode a genuine H.264 MP4 in-process, then
    // decode it back through the real pipeline and assert a true frame is
    // extracted at the playhead. Compiled only with both `decode` (the real
    // MediaEngine backend) and `gst` (so this crate links GStreamer to build
    // the sample clip programmatically — no external `gst-launch`, no PATH or
    // registry fragility).
    #[cfg(all(feature = "decode", feature = "gst"))]
    #[test]
    fn real_decode_extracts_a_true_frame() {
        use gstreamer as gst;
        use gstreamer::prelude::*;

        gst::init().expect("gstreamer init");
        let dir = std::env::temp_dir();
        let f = dir.join(format!("qos-ve-realdec-{}.mp4", std::process::id()));
        let desc = format!(
            "videotestsrc pattern=smpte num-buffers=60 ! \
             video/x-raw,width=192,height=108,framerate=30/1 ! videoconvert ! \
             openh264enc ! h264parse ! mp4mux ! filesink location={}",
            f.display()
        );
        let pipeline = gst::parse::launch(&desc)
            .expect("build sample-clip pipeline")
            .downcast::<gst::Pipeline>()
            .expect("pipeline");
        pipeline
            .set_state(gst::State::Playing)
            .expect("play sample pipeline");
        let bus = pipeline.bus().expect("bus");
        for msg in bus.iter_timed(gst::ClockTime::from_seconds(30)) {
            use gst::MessageView;
            match msg.view() {
                MessageView::Eos(_) => break,
                MessageView::Error(e) => panic!("sample encode error: {}", e.error()),
                _ => {}
            }
        }
        let _ = pipeline.set_state(gst::State::Null);
        assert!(
            f.exists() && std::fs::metadata(&f).unwrap().len() > 512,
            "a real sample clip was encoded"
        );

        let (project, _mid) = project_referencing(&f);
        let mut dec = PreviewDecoder::new();
        assert!(
            dec.is_real_decode(),
            "the decode feature must use the real pipeline"
        );
        // Preroll is async: pump until a frame lands or a short deadline.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut got = false;
        while std::time::Instant::now() < deadline {
            if dec.update(&project, 1.0) {
                got = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(got, "the real pipeline decoded a frame of the clip");
        let frame = dec.frame().expect("decoded frame");
        assert_eq!(frame.width, 192);
        assert_eq!(frame.height, 108);
        assert_eq!(frame.rgba.len(), 192 * 108 * 4);
        std::fs::remove_file(&f).ok();
    }
}
