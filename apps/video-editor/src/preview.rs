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

use crate::model::{
    Clip, ClipEffect, ClipId, EffectKind, Filters, MediaId, MediaKind, Project, TextSource, Track,
    TrackKind, Transform, Transition,
};
use crate::param::Param;

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
    pub fn graded(self, filters: &Filters) -> Rgb {
        let bright = Filters::gain(filters.brightness);
        let contrast = Filters::gain(filters.contrast);
        let sat = Filters::gain(filters.saturation);
        let apply = |c: f64| {
            let c = c * bright; // brightness
            (c - 0.5) * contrast + 0.5 // contrast pivot
        };
        let (mut r, mut g, mut b) = (apply(self.r), apply(self.g), apply(self.b));
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        r = luma + (r - luma) * sat;
        g = luma + (g - luma) * sat;
        b = luma + (b - luma) * sat;
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

    /// Push the colour through a clip's tone curve — the same table the
    /// shader looks up, evaluated directly. Sits between the grade and the
    /// effect stack, exactly where the compositor puts it, so this
    /// representative colour and the composited frame agree about order.
    #[must_use]
    pub fn with_curve(self, curve: &crate::curve::Curve) -> Rgb {
        if curve.is_identity() {
            return self;
        }
        let (r, g, b) = curve.apply(self.r, self.g, self.b);
        Rgb::new(r, g, b)
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
/// The `decoded` flag is always `false` here; `decode_frame` sets it when
/// the decode feature resolves real bytes.
pub fn composite(project: &Project, playhead: f64) -> CompositedFrame {
    let Some(clip) = project.top_video_clip_at(playhead) else {
        return CompositedFrame::Empty;
    };
    let base = palette_rgb(clip.palette)
        .graded(&clip.filters)
        .with_curve(&clip.curves_at(playhead))
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
/// clip under the playhead.
///
/// This is the clip's own time map — see [`Clip::source_time`] — which
/// folds in the playback rate and direction, so a 2× clip advances through
/// its source twice as fast and a reversed one runs backwards.
pub fn source_time_for(clip: &Clip, playhead: f64) -> f64 {
    clip.source_time(playhead)
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
    last_seek_at: ui::time::Instant,
    /// Re-seeks issued for the current target without a frame arriving.
    /// Each one backs the next retry off, because a flush-seek restarts
    /// whatever the pipeline was doing.
    seek_attempts: u32,
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
            last_seek_at: ui::time::now(),
            seek_attempts: 0,
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
            self.seek_attempts = 0;
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
        // Back the retry off as attempts pile up. A retry issues a *flush*
        // seek, which throws away whatever the pipeline had in flight — so
        // retrying at a fixed short interval on a machine where the seek
        // takes longer than that interval restarts it forever and no frame
        // ever arrives. Widening the gap lets a slow seek finish.
        let backoff = std::time::Duration::from_millis(150u64 << self.seek_attempts.min(4));
        let stale_retry = !self.settled && self.last_seek_at.elapsed() > backoff;
        if target_changed || stale_retry {
            self.engine.seek_ms(target_ms);
            self.engine.play();
            self.seek_attempts = if target_changed {
                0
            } else {
                self.seek_attempts + 1
            };
            self.last_seek_ms = Some(target_ms);
            self.last_seek_at = ui::time::now();
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
                self.seek_attempts = 0;
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
    for (y, row) in buf.chunks_exact_mut(w * 4).enumerate() {
        let yf = y as f64 / h as f64;
        for out in row.as_chunks_mut::<4>().0 {
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
            *out = [
                (px.r * 255.0).round().clamp(0.0, 255.0) as u8,
                (px.g * 255.0).round().clamp(0.0, 255.0) as u8,
                (px.b * 255.0).round().clamp(0.0, 255.0) as u8,
                255,
            ];
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

// ---------------------------------------------------------------------------
// Layer plan — what the GPU compositor actually draws.
// ---------------------------------------------------------------------------

/// The transition length a clip falls back to when it has not been given
/// one of its own.
///
/// Re-exported from the model so callers that only speak to the preview —
/// tests, the export's frame maths — have one name for it. The effective
/// length for a particular clip is [`Clip::transition_len`], which honours
/// its `transition_duration` and never lets a ramp outlast the footage.
pub use crate::model::DEFAULT_TRANSITION_SECS as TRANSITION_SECS;

/// Which way a [`Transition::Wipe`] travels across the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WipeDir {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

impl WipeDir {
    /// Index the shader switches on.
    pub fn index(self) -> u32 {
        match self {
            WipeDir::LeftToRight => 0,
            WipeDir::RightToLeft => 1,
            WipeDir::TopToBottom => 2,
            WipeDir::BottomToTop => 3,
        }
    }
}

/// A geometric reveal: the incoming layer is drawn only where the wipe
/// edge has already passed, with a soft (feathered) boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wipe {
    pub dir: WipeDir,
    /// 0 = nothing revealed, 1 = fully revealed.
    pub progress: f64,
    /// Half-width of the feathered edge, as a fraction of the frame.
    pub softness: f64,
}

/// One picture in the composite stack: a source sampled at a point in
/// time, graded, effected, and blended over everything beneath it.
///
/// A transition is *not* a special case here — it is expressed as the
/// alpha (and optional [`Wipe`] mask) of the incoming layer plus, where
/// the outgoing clip is needed, an extra layer beneath it. That is what
/// makes a dissolve sample both clips: the A side is a real layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub clip: ClipId,
    pub media: MediaId,
    /// Seconds into the source media this layer samples.
    pub source_time: f64,
    /// Palette slot, used for the representative fill when no source
    /// texture resolves (hermetic `--shots`, un-hydrated bytes).
    pub palette: u8,
    pub filters: Filters,
    /// The tone curve, sampled at this instant. Baked into a lookup table
    /// by the compositor; identity on a clip nobody has graded, which is
    /// what lets that clip take no lookup at all.
    pub curve: crate::curve::Curve,
    pub effects: Vec<ClipEffect>,
    /// Uniform coverage in 0..1: fade envelope × clip opacity ×
    /// transition weight.
    pub alpha: f64,
    /// Geometric mask, when this layer is wiping in.
    pub wipe: Option<Wipe>,
    /// Colour key, sampled at this instant. Inactive on a clip that has
    /// never been keyed, which is the overwhelming majority.
    pub key: crate::model::ChromaKey,
    /// Shape mask, sampled at this instant. `MaskShape::None` unless the
    /// clip carries one.
    pub mask: crate::model::Mask,
    /// Where the picture sits in the frame, sampled at this instant so an
    /// animated move or scale is already resolved by the time the
    /// compositor sees it.
    pub transform: Transform,
    /// Whether this layer's source texture carries meaningful alpha (a
    /// title, or an image with transparency). Decoded video is opaque, and
    /// honouring its alpha channel unconditionally would make a decoder
    /// that leaves it at zero render nothing at all.
    pub source_alpha: bool,
    /// For a title, the text to draw. The compositor rasterises it rather
    /// than asking the frame source for a picture, so a title renders with
    /// no decoder at all — the same in a hermetic capture as in the live
    /// editor.
    pub title: Option<TextSource>,
}

/// The transition active on `clip` at `playhead`, as (kind, progress in
/// 0..1). `None` once the transition has run out, or for a hard cut.
pub fn active_transition(clip: &Clip, playhead: f64) -> Option<(Transition, f64)> {
    if clip.transition == Transition::None {
        return None;
    }
    let dur = clip.transition_len();
    if dur <= 1e-9 {
        return None;
    }
    let into = playhead - clip.start;
    if !(0.0..dur).contains(&into) {
        return None;
    }
    Some((clip.transition, (into / dur).clamp(0.0, 1.0)))
}

/// Build a layer for `clip` sampled at absolute time `at`.
///
/// Every animatable value is read through [`Clip::param_at`] at `at`, so a
/// keyframed grade, scale or effect intensity is already a plain number by
/// the time it reaches the GPU.
struct LayerSample {
    at: f64,
    source_time: f64,
    alpha: f64,
    wipe: Option<Wipe>,
    source_alpha: bool,
    title: Option<TextSource>,
}

fn layer_at(clip: &Clip, sample: LayerSample) -> Layer {
    Layer {
        clip: clip.id,
        media: clip.media,
        source_time: sample.source_time,
        palette: clip.palette,
        filters: clip.filters_at(sample.at),
        curve: clip.curves_at(sample.at),
        effects: clip.effects_at(sample.at),
        alpha: sample.alpha.clamp(0.0, 1.0),
        wipe: sample.wipe,
        key: clip.key_at(sample.at),
        mask: clip.mask_at(sample.at),
        transform: clip.transform_at(sample.at),
        source_alpha: sample.source_alpha,
        title: sample.title,
    }
}

/// Where the outgoing clip is sampled during a transition. When the cut
/// abuts (A ends exactly where B starts) the playhead is already past
/// A's out-point, so A holds on its last frame; when the clips overlap,
/// A keeps playing.
fn outgoing_source_time(a: &Clip, playhead: f64) -> f64 {
    // `Clip::source_time` already holds the result inside `[in, out]`,
    // which is exactly the "hold on the last frame" behaviour an abutting
    // cut needs, and it honours the outgoing clip's own speed.
    a.source_time(playhead)
}

/// Build the composite stack at `playhead`: every visible clip on every
/// visible video track, ordered **bottom → top** (V1 beneath V2), each
/// with its own grade, effect chain and blend weight.
///
/// Pure: no decoding, no GPU, no I/O — so the whole layering, envelope
/// and transition model is unit-testable without a device.
pub fn plan_layers(project: &Project, playhead: f64) -> Vec<Layer> {
    // Solo wins over mute: while any video track is soloed, only soloed
    // video tracks are seen, and a muted one among them stays hidden.
    let soloing = project
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Video && t.solo);
    let mut video: Vec<&Track> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video && !t.muted && (!soloing || t.solo))
        .collect();
    // Bottom → top: V1 (index 1) is drawn first, V2 over it. `locked` is
    // an editing lock, not a visibility one, so it never hides a layer.
    video.sort_by_key(|a| a.index);

    let mut out = Vec::new();
    for track in video {
        plan_track(project, track, playhead, &mut out);
    }
    out
}

/// The clip a transition on `c` dissolves *from*, if there is one.
///
/// A transition is a property of the incoming clip's head, so the outgoing
/// side is whatever sits immediately before it — but only if it actually
/// reaches the cut. Taking the previous clip unconditionally meant a clip
/// preceded by a **gap** composited that earlier clip's last frame at full
/// alpha underneath it: footage the timeline shows ending seconds ago,
/// reappearing under a dissolve it has nothing to do with.
///
/// Abutting is compared with a tolerance rather than exactly, because clip
/// edges are seconds in floating point and a trim that lands a frame short
/// should still dissolve.
fn outgoing_neighbour<'a>(clips: &[&'a Clip], i: usize) -> Option<&'a Clip> {
    // Half a frame at 30fps: closer than a person can place an edge, and far
    // enough that a real gap never passes.
    const ABUT_EPSILON: f64 = 1.0 / 60.0;
    let c = clips.get(i)?;
    let prev = clips.get(i.checked_sub(1)?)?;
    (prev.end() >= c.start - ABUT_EPSILON).then_some(*prev)
}

fn plan_track(project: &Project, track: &Track, playhead: f64, out: &mut Vec<Layer>) {
    let clips = project.clips_on(track.id);
    // A predecessor drawn *by* a transition must not also be drawn as an
    // ordinary layer, or a dip-to-black would be painted over at full
    // strength by its own A side.
    let mut claimed: Vec<ClipId> = Vec::new();
    for (i, c) in clips.iter().enumerate() {
        if c.covers(playhead)
            && active_transition(c, playhead).is_some()
            && let Some(prev) = outgoing_neighbour(&clips, i)
        {
            claimed.push(prev.id);
        }
    }

    // Whether a clip's picture carries its own alpha: a title always does,
    // and so can an image. Decoded video does not.
    let has_alpha = |c: &Clip| {
        project
            .media(c.media)
            .is_some_and(|m| matches!(m.kind, MediaKind::Text | MediaKind::Image))
    };
    // A title's text, for the compositor to raster. Only a Text source has
    // one, so every other clip resolves its picture the usual way.
    let title_of = |c: &Clip| {
        project
            .media(c.media)
            .filter(|m| m.kind == MediaKind::Text)
            .and_then(|m| m.text.clone())
    };

    for (i, c) in clips.iter().enumerate() {
        if !c.covers(playhead) || claimed.contains(&c.id) {
            continue;
        }
        // Opacity goes through the animation door, so a keyframed fade is
        // just a different number here — no second path.
        let base = (c.envelope(playhead) * c.param_at(Param::Opacity, playhead)).clamp(0.0, 1.0);
        let t_in = source_time_for(c, playhead);
        let alpha_src = has_alpha(c);
        let title = title_of(c);
        let mk = |alpha: f64, wipe: Option<Wipe>| {
            layer_at(
                c,
                LayerSample {
                    at: playhead,
                    source_time: t_in,
                    alpha,
                    wipe,
                    source_alpha: alpha_src,
                    title: title.clone(),
                },
            )
        };
        let prev = outgoing_neighbour(&clips, i);
        let Some((kind, t)) = active_transition(c, playhead) else {
            out.push(mk(base, None));
            continue;
        };
        // The A side, sampled where the outgoing clip is at this instant.
        let a_layer = |alpha: f64| {
            prev.map(|a| {
                layer_at(
                    a,
                    LayerSample {
                        at: playhead,
                        source_time: outgoing_source_time(a, playhead),
                        alpha,
                        wipe: None,
                        source_alpha: has_alpha(a),
                        title: title_of(a),
                    },
                )
            })
        };
        match kind {
            // mix(black, B, t) — nothing beneath, so B simply rises out
            // of the letterbox black.
            Transition::FadeIn => out.push(mk(base * t, None)),
            // mix(A, B, t) — both clips are sampled, B fades up over A.
            Transition::CrossDissolve => {
                out.extend(a_layer(1.0));
                out.push(mk(base * t, None));
            }
            // A → black → B: A dims away over the first half, B rises
            // out of black over the second.
            Transition::DipToBlack => {
                if t < 0.5 {
                    out.extend(a_layer(1.0 - 2.0 * t));
                } else {
                    out.push(mk(base * (2.0 * t - 1.0), None));
                }
            }
            // Real geometry: B is revealed behind a travelling soft edge,
            // with A intact everywhere the edge has not reached.
            Transition::Wipe => {
                out.extend(a_layer(1.0));
                out.push(mk(
                    base,
                    Some(Wipe {
                        dir: WipeDir::LeftToRight,
                        progress: t,
                        softness: 0.06,
                    }),
                ));
            }
            Transition::None => out.push(mk(base, None)),
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "decode")]
    #[allow(dead_code, reason = "only the media-stack check is used here")]
    mod generated_clip {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/generated_clip.rs"
        ));
    }

    use super::*;
    use crate::doc::sample_project;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn preview_composite_picks_clips_and_applies_grade_and_effects() {
        // Case `a_gap_before_a_dissolve_leaves_the_earlier_clip_out_of_it`:
        // A clip separated by a gap is not dragged into the next clip's
        // dissolve.
        //
        // The A side used to be `clips[i - 1]` unconditionally. With a gap
        // before the incoming clip, that composited the earlier clip's frozen
        // last frame at full alpha underneath it — footage the timeline shows
        // as having ended seconds ago, reappearing under an unrelated
        // transition.
        {
            use crate::model::{MediaItem, MediaKind, Project, Track, TrackKind, Transition};

            let mut p = Project::new("gap");
            let media = uuid::Uuid::new_v4();
            p.media.push(MediaItem {
                id: media,
                name: "m.mp4".into(),
                kind: MediaKind::Video,
                duration: 60.0,
                ..Default::default()
            });
            let track = uuid::Uuid::new_v4();
            p.tracks.push(Track {
                id: track,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                ..Default::default()
            });

            let early = p.add_clip(track, media, 0.0).unwrap();
            p.clip_mut(early).unwrap().out_point = 4.0;

            // A deliberate gap: the next clip starts well after the first ends.
            let late = p.add_clip(track, media, 20.0).unwrap();
            {
                let c = p.clip_mut(late).unwrap();
                c.out_point = 10.0;
                c.transition = Transition::CrossDissolve;
                c.transition_duration = 1.0;
            }

            // Just inside the incoming clip's transition window.
            let layers = plan_layers(&p, 20.3);
            assert!(
                layers.iter().any(|l| l.clip == late),
                "the incoming clip should be on screen: {layers:?}",
            );
            assert!(
                layers.iter().all(|l| l.clip != early),
                "a clip that ended at 4s must not be composited under a dissolve at 20s: {layers:?}",
            );

            // And the abutting case still dissolves, so the guard did not simply
            // switch transitions off.
            p.clip_mut(late).unwrap().start = 4.0;
            let layers = plan_layers(&p, 4.3);
            assert!(
                layers.iter().any(|l| l.clip == early),
                "an abutting predecessor is still the A side of the dissolve: {layers:?}",
            );
        }
        // Case `empty_when_no_clip_covers_playhead`.
        {
            let p = sample_project();
            // Far past the end of every clip.
            assert_eq!(composite(&p, 10_000.0), CompositedFrame::Empty);
        }
        // Case `picks_the_top_video_clip`.
        {
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
        // Case `fade_in_darkens_the_head`.
        {
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
        // Case `grade_changes_the_base_colour`.
        {
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
        // Case `grayscale_effect_collapses_channels_and_empty_is_a_noop`.
        {
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
                id: uuid::Uuid::new_v4(),
                kind: EffectKind::Invert,
                amount: 1.0,
            }]);
            assert!((inv.r - (1.0 - c.r)).abs() < 1e-6);
        }
        // Case `effects_stack_in_order_and_change_the_composite`.
        {
            use crate::model::{ClipEffect, EffectKind};
            let mut p = sample_project();
            // The drone open (clips[0]) carries seeded effects; strip them and prove
            // adding a grayscale effect desaturates its composite base.
            let cid = p.clips[0].id;
            p.clip_mut(cid).unwrap().clear_effects();
            let CompositedFrame::Clip { base: before, .. } = composite(&p, 6.0) else {
                panic!("expected clip");
            };
            p.clip_mut(cid).unwrap().effects = vec![ClipEffect {
                id: uuid::Uuid::new_v4(),
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
        // Case `identity_grade_is_a_near_noop`.
        {
            let c = palette_rgb(3);
            let g = c.graded(&Filters::default());
            assert!(
                (c.r - g.r).abs() < 1e-9 && (c.g - g.g).abs() < 1e-9 && (c.b - g.b).abs() < 1e-9
            );
        }
        // Case `transition_flash_decays_to_none`.
        {
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
        // Case `source_time_maps_playhead_into_the_clip_window`.
        {
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
                ..Default::default()
            });
            let tid = Uuid::new_v4();
            p.tracks.push(Track {
                id: tid,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                muted: false,
                locked: false,
                solo: false,
                gain_db: 0.0,
                pan: 0.0,
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
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        });
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        p.clip_mut(cid).unwrap().out_point = 20.0;
        (p, mid)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn preview_decoder_extracts_frames_through_the_engine_seam() {
        // Case `decoder_extracts_a_frame_through_the_engine_seam`.
        {
            // Hermetic proof of the extraction seam on the synthesized backend: a
            // real file on disk (so the hydration gate passes), a real seek + poll,
            // and a real RGBA frame keyed to the right clip. The gst backend runs
            // the identical `update` path under `--features decode`.
            let dir = std::env::temp_dir();
            let f = dir.join(format!("tilo-ve-dec-{}.mp4", std::process::id()));
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
        // Case `decoder_yields_nothing_for_an_unhydrated_reference`.
        {
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
                ..Default::default()
            });
            let tid = Uuid::new_v4();
            p.tracks.push(Track {
                id: tid,
                name: "V1".into(),
                kind: TrackKind::Video,
                index: 1,
                muted: false,
                locked: false,
                solo: false,
                gain_db: 0.0,
                pan: 0.0,
            });
            p.add_clip(tid, mid, 0.0);
            let mut dec = PreviewDecoder::new_synthesized();
            assert!(
                !dec.update(&p, 1.0),
                "no frame for an un-hydrated reference"
            );
            assert!(dec.frame().is_none());
        }
        // Case `render_rgba_produces_a_sized_opaque_buffer_reflecting_the_grade`.
        {
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
        // Case `real_decode_extracts_a_true_frame`:
        // Real GStreamer round-trip: encode a genuine H.264 MP4 in-process, then
        // decode it back through the real pipeline and assert a true frame is
        // extracted at the playhead. Compiled only with both `decode` (the real
        // MediaEngine backend) and `gst` (so this crate links GStreamer to build
        // the sample clip programmatically — no external `gst-launch`, no PATH or
        // registry fragility).
        #[cfg(all(feature = "decode", feature = "gst"))]
        'case: {
            use gstreamer as gst;
            use gstreamer::prelude::*;
            if !generated_clip::media_stack() {
                break 'case;
            }

            gst::init().expect("gstreamer init");
            let dir = std::env::temp_dir();
            let f = dir.join(format!("tilo-ve-realdec-{}.mp4", std::process::id()));
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
            let mut decoder = PreviewDecoder::new();
            assert!(
                decoder.is_real_decode(),
                "the decode feature must use the real pipeline"
            );
            // Preroll is async: pump until a frame lands or a short deadline.
            let deadline = tilo_test_env::hang_deadline();
            let mut got = false;
            while ui::time::now() < deadline {
                if decoder.update(&project, 1.0) {
                    got = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            assert!(got, "the real pipeline decoded a frame of the clip");
            let frame = decoder.frame().expect("decoded frame");

            assert_eq!(frame.width, 192);
            assert_eq!(frame.height, 108);
            assert_eq!(frame.rgba.len(), 192 * 108 * 4);
            std::fs::remove_file(&f).ok();
        }
    }

    // --- layer plan -------------------------------------------------------

    /// Two abutting clips on one video track, plus an optional second
    /// track above. Returns (project, track ids).
    fn stacked_project() -> (Project, crate::model::TrackId, crate::model::TrackId) {
        use crate::model::{MediaKind, Project, Track, TrackKind};
        use uuid::Uuid;
        let mut p = Project::new("t");
        let push_media = |p: &mut Project, palette: u8| {
            let id = Uuid::new_v4();
            p.media.push(crate::model::MediaItem {
                id,
                name: format!("m{palette}"),
                kind: MediaKind::Video,
                duration: 30.0,
                width: 16,
                height: 9,
                fps: 30.0,
                palette,
                path: crate::media::workspace_path("m.mp4"),
                hydration: crate::media::Hydration::Projected,
                ..Default::default()
            });
            id
        };
        let m1 = push_media(&mut p, 1);
        let m2 = push_media(&mut p, 2);
        let m3 = push_media(&mut p, 3);
        let v1 = Uuid::new_v4();
        let v2 = Uuid::new_v4();
        p.tracks.push(Track {
            id: v1,
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        });
        p.tracks.push(Track {
            id: v2,
            name: "V2".into(),
            kind: TrackKind::Video,
            index: 2,
            muted: false,
            locked: false,
            solo: false,
            gain_db: 0.0,
            pan: 0.0,
        });
        // V1: two abutting clips, cut at 5s.
        let a = p.add_clip(v1, m1, 0.0).unwrap();
        p.clip_mut(a).unwrap().out_point = 5.0;
        let b = p.add_clip(v1, m2, 5.0).unwrap();
        p.clip_mut(b).unwrap().out_point = 5.0;
        // V2: an insert over the second half.
        let c = p.add_clip(v2, m3, 6.0).unwrap();
        p.clip_mut(c).unwrap().out_point = 4.0;
        (p, v1, v2)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the plan copies the clip's grade through unchanged"
    )]
    fn layer_plan_stacks_tracks_and_samples_source_time() {
        // Case `the_plan_stacks_every_video_track_bottom_first`.
        {
            let (p, v1, v2) = stacked_project();
            // At t=7 both tracks are covered: V1's second clip and V2's insert.
            let layers = plan_layers(&p, 7.0);
            assert_eq!(
                layers.len(),
                2,
                "both tracks contribute a layer: {layers:?}"
            );
            let lower = p.clip(layers[0].clip).unwrap();
            let upper = p.clip(layers[1].clip).unwrap();
            assert_eq!(lower.track, v1, "V1 is composited first (underneath)");
            assert_eq!(upper.track, v2, "V2 is composited last (on top)");
        }
        // Case `a_muted_video_track_drops_out_of_the_plan`.
        {
            let (mut p, _v1, v2) = stacked_project();
            let idx = p.tracks.iter().position(|t| t.id == v2).unwrap();
            p.tracks[idx].muted = true;
            let layers = plan_layers(&p, 7.0);
            assert_eq!(layers.len(), 1, "the muted track contributes nothing");
            assert_eq!(p.clip(layers[0].clip).unwrap().track, p.tracks[0].id);
            // Locking is an edit lock, not a visibility one.
            p.tracks[idx].muted = false;
            p.tracks[idx].locked = true;
            assert_eq!(
                plan_layers(&p, 7.0).len(),
                2,
                "a locked track still renders"
            );
        }
        // Case `soloing_a_track_hides_the_others`.
        {
            let (mut p, v1, v2) = stacked_project();
            let i1 = p.tracks.iter().position(|t| t.id == v1).unwrap();
            let i2 = p.tracks.iter().position(|t| t.id == v2).unwrap();
            assert_eq!(
                plan_layers(&p, 7.0).len(),
                2,
                "both tracks render by default"
            );

            p.tracks[i2].solo = true;
            let soloed = plan_layers(&p, 7.0);
            assert_eq!(soloed.len(), 1, "soloing V2 hides V1: {soloed:?}");
            assert_eq!(p.clip(soloed[0].clip).unwrap().track, v2);

            // Soloing both is the same as soloing neither.
            p.tracks[i1].solo = true;
            assert_eq!(
                plan_layers(&p, 7.0).len(),
                2,
                "every track soloed shows every track"
            );

            // Mute still wins for a track that is itself soloed.
            p.tracks[i1].muted = true;
            let muted_solo = plan_layers(&p, 7.0);
            assert_eq!(
                muted_solo.len(),
                1,
                "a muted track stays hidden even when soloed"
            );
            assert_eq!(p.clip(muted_solo[0].clip).unwrap().track, v2);
        }
        // Case `a_layer_samples_its_clips_own_source_time`.
        {
            let (mut p, v1, _) = stacked_project();
            let cid = p.clips_on(v1)[1].id;
            p.clip_mut(cid).unwrap().in_point = 2.0;
            p.clip_mut(cid).unwrap().out_point = 7.0;
            // Playhead 1s into a clip cut at 2s → 3s of source.
            let layers = plan_layers(&p, 6.0);
            let l = layers.iter().find(|l| l.clip == cid).unwrap();
            assert!(
                (l.source_time - 3.0).abs() < 1e-9,
                "source time follows the in-point: {l:?}"
            );
        }
        // Case `the_plan_carries_the_grade_effects_and_envelope`.
        {
            let (mut p, v1, _) = stacked_project();
            let cid = p.clips_on(v1)[0].id;
            {
                let c = p.clip_mut(cid).unwrap();
                c.filters = Filters {
                    brightness: 0.7,
                    contrast: 0.4,
                    saturation: 0.9,
                };
                c.effects = vec![ClipEffect {
                    id: uuid::Uuid::new_v4(),
                    kind: EffectKind::Sepia,
                    amount: 0.5,
                }];
                c.fade_in = 2.0;
                c.opacity = 0.5;
            }
            let layers = plan_layers(&p, 1.0);
            assert_eq!(layers.len(), 1);
            let l = &layers[0];
            assert_eq!(l.filters.brightness, 0.7);
            assert_eq!(l.effects.len(), 1);
            assert_eq!(l.effects[0].kind, EffectKind::Sepia);
            // 1s into a 2s fade at half opacity.
            assert!(
                (l.alpha - 0.25).abs() < 1e-9,
                "envelope × opacity: {}",
                l.alpha
            );
        }
        // Case `an_empty_timeline_plans_nothing`.
        {
            let p = Project::new("t");
            assert!(plan_layers(&p, 0.0).is_empty());
            let (full, _, _) = stacked_project();
            assert!(
                plan_layers(&full, 10_000.0).is_empty(),
                "past every clip the plan is empty"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn transitions_plan_distinct_bounded_layer_stacks() {
        // Case `a_dissolve_puts_the_outgoing_clip_underneath_the_incoming_one`.
        {
            // The heart of the fix: a transition is two layers, so both clips
            // are sampled. The old flash sampled neither.
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            let outgoing = p.clips_on(v1)[0].id;
            p.clip_mut(incoming).unwrap().transition = Transition::CrossDissolve;
            let layers = plan_layers(&p, 5.0 + TRANSITION_SECS * 0.5);
            assert_eq!(
                layers.len(),
                2,
                "the outgoing clip is a real layer: {layers:?}"
            );
            assert_eq!(layers[0].clip, outgoing);
            assert_eq!(layers[1].clip, incoming);
            assert!(
                (layers[0].alpha - 1.0).abs() < 1e-9,
                "the outgoing clip is fully present"
            );
            assert!(
                (layers[1].alpha - 0.5).abs() < 0.02,
                "the incoming clip is half-mixed at the midpoint: {}",
                layers[1].alpha,
            );
            // The outgoing clip has run out, so it holds on its last frame.
            let a = p.clip(outgoing).unwrap();
            assert!(
                (layers[0].source_time - a.out_point).abs() < 1e-9,
                "an abutting cut freezes the outgoing clip on its tail",
            );
        }
        // Case `a_fade_in_has_no_outgoing_layer_so_it_mixes_with_black`.
        {
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            p.clip_mut(incoming).unwrap().transition = Transition::FadeIn;
            let layers = plan_layers(&p, 5.0 + TRANSITION_SECS * 0.5);
            assert_eq!(
                layers.len(),
                1,
                "a fade rises out of black, not out of the previous clip"
            );
            assert_eq!(layers[0].clip, incoming);
            assert!((layers[0].alpha - 0.5).abs() < 0.02);
        }
        // Case `a_dip_hands_the_frame_from_one_clip_to_the_other_through_black`.
        {
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            let outgoing = p.clips_on(v1)[0].id;
            p.clip_mut(incoming).unwrap().transition = Transition::DipToBlack;

            let early = plan_layers(&p, 5.0 + TRANSITION_SECS * 0.25);
            assert_eq!(early.len(), 1);
            assert_eq!(
                early[0].clip, outgoing,
                "the first half belongs to the outgoing clip"
            );
            assert!(
                (early[0].alpha - 0.5).abs() < 0.02,
                "dimming toward black: {}",
                early[0].alpha
            );

            let late = plan_layers(&p, 5.0 + TRANSITION_SECS * 0.75);
            assert_eq!(late.len(), 1);
            assert_eq!(
                late[0].clip, incoming,
                "the second half belongs to the incoming clip"
            );
            assert!(
                (late[0].alpha - 0.5).abs() < 0.02,
                "rising out of black: {}",
                late[0].alpha
            );

            let bottom = plan_layers(&p, 5.0 + TRANSITION_SECS * 0.5);
            let lit: f64 = bottom.iter().map(|l| l.alpha).sum();
            assert!(lit < 0.02, "the midpoint of a dip is black: {bottom:?}");
        }
        // Case `a_wipe_carries_geometry_not_a_flash`.
        {
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            p.clip_mut(incoming).unwrap().transition = Transition::Wipe;
            let layers = plan_layers(&p, 5.0 + TRANSITION_SECS * 0.5);
            assert_eq!(
                layers.len(),
                2,
                "a wipe reveals the incoming clip over the outgoing one"
            );
            assert!(layers[0].wipe.is_none(), "the outgoing clip is unmasked");
            let w = layers[1]
                .wipe
                .expect("the incoming clip carries a wipe mask");
            assert!((w.progress - 0.5).abs() < 0.02, "halfway across: {w:?}");
            assert!(w.softness > 0.0, "the edge is feathered: {w:?}");
            // The incoming clip is fully opaque *within* the revealed region —
            // a wipe is geometry, not a cross-fade.
            assert!((layers[1].alpha - 1.0).abs() < 1e-9);
        }
        // Case `each_transition_plans_a_distinct_stack`.
        {
            let (base, v1, _) = stacked_project();
            let at = 5.0 + TRANSITION_SECS * 0.5;
            let mut seen: Vec<(Transition, Vec<Layer>)> = Vec::new();
            for kind in [
                Transition::CrossDissolve,
                Transition::FadeIn,
                Transition::DipToBlack,
                Transition::Wipe,
            ] {
                let mut p = base.clone();
                let incoming = p.clips_on(v1)[1].id;
                p.clip_mut(incoming).unwrap().transition = kind;
                seen.push((kind, plan_layers(&p, at)));
            }
            for i in 0..seen.len() {
                for j in (i + 1)..seen.len() {
                    assert_ne!(
                        seen[i].1, seen[j].1,
                        "{:?} and {:?} must not plan the same stack",
                        seen[i].0, seen[j].0,
                    );
                }
            }
        }
        // Case `a_transition_expires_and_leaves_a_single_layer`.
        {
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            p.clip_mut(incoming).unwrap().transition = Transition::CrossDissolve;
            let after = plan_layers(&p, 5.0 + TRANSITION_SECS + 0.05);
            assert_eq!(
                after.len(),
                1,
                "past the transition only the incoming clip remains"
            );
            assert!((after[0].alpha - 1.0).abs() < 1e-9);
        }
        // Case `a_transition_never_outruns_a_very_short_clip`.
        {
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            {
                let c = p.clip_mut(incoming).unwrap();
                c.transition = Transition::CrossDissolve;
                c.out_point = c.in_point + 0.2; // shorter than the default ramp
            }
            let clip = p.clip(incoming).unwrap().clone();
            assert!(
                active_transition(&clip, 5.1).is_some(),
                "still transitioning early on"
            );
            assert!(
                active_transition(&clip, 5.19).is_some_and(|(_, t)| t > 0.9),
                "the transition is compressed into the clip's own length",
            );
            assert!(
                active_transition(&clip, 5.25).is_none(),
                "and is over by the clip's end"
            );
        }
        // Case `a_clips_own_transition_length_drives_the_plan`.
        {
            // The ramp used to be a constant for every clip in every project.
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            p.clip_mut(incoming).unwrap().transition = Transition::CrossDissolve;

            // Unset: the default ramp, half-mixed at half of it.
            let at_default_mid = 5.0 + TRANSITION_SECS * 0.5;
            let default_mid = plan_layers(&p, at_default_mid);
            assert!(
                (default_mid[1].alpha - 0.5).abs() < 0.02,
                "{:?}",
                default_mid[1].alpha
            );

            // A longer ramp: the same instant is now only a quarter of the way in.
            p.clip_mut(incoming).unwrap().transition_duration = TRANSITION_SECS * 2.0;
            let slower = plan_layers(&p, at_default_mid);
            assert!(
                (slower[1].alpha - 0.25).abs() < 0.02,
                "a longer transition is less far along at the same instant: {}",
                slower[1].alpha,
            );
            // ...and it is still running where the default one had finished.
            assert!(
                plan_layers(&p, 5.0 + TRANSITION_SECS * 1.5).len() == 2,
                "the outgoing clip is still underneath past the default length",
            );

            // A shorter ramp is already over there.
            p.clip_mut(incoming).unwrap().transition_duration = TRANSITION_SECS * 0.25;
            let quick = plan_layers(&p, at_default_mid);
            assert_eq!(
                quick.len(),
                1,
                "a short transition has finished by the default midpoint"
            );
        }
    }
}
