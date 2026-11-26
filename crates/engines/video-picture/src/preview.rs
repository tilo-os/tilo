//! What the preview shows at the playhead: the source clip, its grade, the fade
//! and opacity envelope and the transition flash, computed without a device.
//!
//! The colour math here is the single source of truth for the representative
//! frame and for the export, so a capture, the canvas and an exported frame agree.
//! Under `--features decode` the compositor fills the layers with decoded
//! pictures; a reference whose bytes are not hydrated falls back to the
//! representative frame, so the editor never blocks on hydration.

use video_edit::model::{
    Clip, ClipEffect, ClipId, EffectKind, Filters, MediaId, MediaKind, Project, TextSource, Track,
    TrackKind, Transform, Transition,
};
use video_edit::param::Param;

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
    /// Brightness scales, contrast pivots around mid-grey, saturation lerps toward
    /// or away from luma.
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
    #[must_use]
    pub fn faded(self, gain: f64) -> Rgb {
        let g = gain.clamp(0.0, 1.0);
        Rgb::new(self.r * g, self.g * g, self.b * g)
    }

    /// In list order, after the grade.
    #[must_use]
    pub fn with_effects(self, effects: &[ClipEffect]) -> Rgb {
        effects.iter().fold(self, apply_effect)
    }

    /// The same table the shader looks up, between the grade and the effect
    /// stack, where the compositor puts it.
    #[must_use]
    pub fn with_curve(self, curve: &video_edit::curve::Curve) -> Rgb {
        if curve.is_identity() {
            return self;
        }
        let (r, g, b) = curve.apply(self.r, self.g, self.b);
        Rgb::new(r, g, b)
    }
}

fn lerp(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    Rgb::new(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

/// The one place an effect's look is defined, for both preview and export.
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
        // A flat colour has no spatial extent, so a vignette reads as a gentle
        // overall darkening here; the spatial falloff is the shader's.
        EffectKind::Vignette => lerp(c, Rgb::new(0.0, 0.0, 0.0), a * 0.4),
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flash {
    pub white: bool,
    pub alpha: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CompositedFrame {
    Empty,
    Clip {
        media: MediaId,
        base: Rgb,
        envelope: f64,
        flash: Option<Flash>,
        decoded: bool,
    },
}

/// Pure: no decoding, no I/O; `decoded` is always `false` here.
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

/// A dissolve or fade flashes white, a dip black; both decay over 0.6 s.
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

/// Mirrors the palette indices `clip_color` uses, so the frame agrees with the
/// clip body.
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

/// The clip's own time map ([`Clip::source_time`]), so playback rate and
/// direction are already folded in.
pub fn source_time_for(clip: &Clip, playhead: f64) -> f64 {
    clip.source_time(playhead)
}

/// The same grade, fade and transition maths the canvas shows, in a tightly
/// packed `width*height*4` RGBA buffer, so an export encodes what was previewed.
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
                    let mut c = lerp(base, black, 0.18);
                    if yf < 0.5 {
                        c = lerp(c, lerp(base, Rgb::new(1.0, 1.0, 1.0), 0.18), 0.55);
                    }
                    if (0.62..0.72).contains(&yf) {
                        c = lerp(c, lerp(base, black, 0.15), 0.6);
                    }
                    c
                }
            };
            if env < 0.999 {
                px = lerp(px, black, 1.0 - env);
            }
            if let Some(f) = flash {
                let target = if f.white {
                    Rgb::new(1.0, 1.0, 1.0)
                } else {
                    black
                };
                px = lerp(px, target, f.alpha);
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

/// Re-exported so callers that only speak to the preview have one name for it.
/// The effective length for a clip is [`Clip::transition_len`], which honours
/// `transition_duration` and never lets a ramp outlast the footage.
pub use video_edit::model::DEFAULT_TRANSITION_SECS as TRANSITION_SECS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WipeDir {
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

impl WipeDir {
    pub fn index(self) -> u32 {
        match self {
            WipeDir::LeftToRight => 0,
            WipeDir::RightToLeft => 1,
            WipeDir::TopToBottom => 2,
            WipeDir::BottomToTop => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wipe {
    pub dir: WipeDir,
    pub progress: f64,
    /// Half-width of the feathered edge, as a fraction of the frame.
    pub softness: f64,
}

/// A source sampled at a point in time, graded, effected and blended over what
/// is beneath it.
///
/// A transition is no special case: it is this layer's alpha (and [`Wipe`] mask)
/// plus, where the outgoing clip is needed, the A side as a real layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub clip: ClipId,
    pub media: MediaId,
    pub source_time: f64,
    pub palette: u8,
    pub filters: Filters,
    pub curve: video_edit::curve::Curve,
    pub effects: Vec<ClipEffect>,
    /// Uniform coverage in 0..1: fade envelope × clip opacity ×
    /// transition weight.
    pub alpha: f64,
    pub wipe: Option<Wipe>,
    pub key: video_edit::model::ChromaKey,
    pub mask: video_edit::model::Mask,
    pub transform: Transform,
    /// A title or an image can carry alpha; decoded video is opaque, and honouring
    /// its alpha unconditionally would make a decoder that leaves it at zero
    /// render nothing at all.
    pub source_alpha: bool,
    /// A title is rasterised by the compositor rather than decoded, so it renders
    /// the same in a capture as in the live editor.
    pub title: Option<TextSource>,
}

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

/// Every animatable value is read through [`Clip::param_at`] at `at`, so a
/// keyframed grade, scale or intensity is a plain number before the GPU.
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

/// `Clip::source_time` holds the result inside `[in, out]`, which is the "hold on
/// the last frame" an abutting cut needs, and honours the outgoing clip's speed.
fn outgoing_source_time(a: &Clip, playhead: f64) -> f64 {
    a.source_time(playhead)
}

/// Every visible clip on every visible video track, bottom to top, each with its
/// grade, effect chain and blend weight. Pure, so the whole layering, envelope
/// and transition model is testable without a device.
pub fn plan_layers(project: &Project, playhead: f64) -> Vec<Layer> {
    let soloing = project
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Video && t.solo);
    let mut video: Vec<&Track> = project
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video && !t.muted && (!soloing || t.solo))
        .collect();
    // Bottom to top: V1 is drawn first. `locked` is an edit lock, not a
    // visibility one.
    video.sort_by_key(|a| a.index);

    let mut out = Vec::new();
    for track in video {
        plan_track(project, track, playhead, &mut out);
    }
    out
}

/// The clip a transition on `c` dissolves from: whatever sits immediately before
/// it, and only if it actually reaches the cut. A clip after a **gap** must not
/// composite its last frame underneath an unrelated dissolve.
///
/// Abutting is compared with a tolerance, because clip edges are seconds in
/// floating point and a trim that lands a frame short should still dissolve.
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
    let mut claimed: Vec<ClipId> = Vec::new();
    for (i, c) in clips.iter().enumerate() {
        if c.covers(playhead)
            && active_transition(c, playhead).is_some()
            && let Some(prev) = outgoing_neighbour(&clips, i)
        {
            claimed.push(prev.id);
        }
    }

    // A title or an image carries its own alpha; decoded video does not.
    let has_alpha = |c: &Clip| {
        project
            .media(c.media)
            .is_some_and(|m| matches!(m.kind, MediaKind::Text | MediaKind::Image))
    };
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
            // lerp(black, B, t) — nothing beneath, so B simply rises out
            // of the letterbox black.
            Transition::FadeIn => out.push(mk(base * t, None)),
            // lerp(A, B, t) — both clips are sampled, B fades up over A.
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
