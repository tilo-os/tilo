//! What the preview shows at the playhead: the source clip, its grade, the fade
//! and opacity envelope and the transition flash, computed without a device.
//!
//! The colour math here is the single source of truth for the representative
//! frame and for the export, so a capture, the canvas and an exported frame agree.
//! Under `--features decode` the compositor fills the layers with decoded
//! pictures; a reference whose bytes are not hydrated falls back to the
//! representative frame, so the editor never blocks on hydration.

use crate::model::{
    Clip, ClipEffect, ClipId, EffectKind, Filters, MediaId, MediaKind, Project, TextSource, Track,
    TrackKind, Transform, Transition,
};
use crate::param::Param;

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
    pub fn with_curve(self, curve: &crate::curve::Curve) -> Rgb {
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
pub use crate::model::DEFAULT_TRANSITION_SECS as TRANSITION_SECS;

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
    pub curve: crate::curve::Curve,
    pub effects: Vec<ClipEffect>,
    /// Uniform coverage in 0..1: fade envelope × clip opacity ×
    /// transition weight.
    pub alpha: f64,
    pub wipe: Option<Wipe>,
    pub key: crate::model::ChromaKey,
    pub mask: crate::model::Mask,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::sample_project;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn preview_composite_picks_clips_and_applies_grade_and_effects() {
        // A clip after a gap must not be dragged into the next clip's
        // dissolve.
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

            let late = p.add_clip(track, media, 20.0).unwrap();
            {
                let c = p.clip_mut(late).unwrap();
                c.out_point = 10.0;
                c.transition = Transition::CrossDissolve;
                c.transition_duration = 1.0;
            }

            let layers = plan_layers(&p, 20.3);
            assert!(
                layers.iter().any(|l| l.clip == late),
                "the incoming clip should be on screen: {layers:?}",
            );
            assert!(
                layers.iter().all(|l| l.clip != early),
                "a clip that ended at 4s must not be composited under a dissolve at 20s: {layers:?}",
            );

            p.clip_mut(late).unwrap().start = 4.0;
            let layers = plan_layers(&p, 4.3);
            assert!(
                layers.iter().any(|l| l.clip == early),
                "an abutting predecessor is still the A side of the dissolve: {layers:?}",
            );
        }
        {
            let p = sample_project();
            assert_eq!(composite(&p, 10_000.0), CompositedFrame::Empty);
        }
        {
            let p = sample_project();
            match composite(&p, 23.0) {
                CompositedFrame::Clip { media, .. } => {
                    let top = p.top_video_clip_at(23.0).unwrap();
                    assert_eq!(media, top.media);
                }
                CompositedFrame::Empty => panic!("expected a clip at the playhead"),
            }
        }
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
        {
            use crate::model::{ClipEffect, EffectKind};
            let c = palette_rgb(0); // a saturated bluish clip
            assert_eq!(
                c.with_effects(&[]),
                c,
                "no effects leaves the colour unchanged"
            );
            let g = c.with_effects(&[ClipEffect::new(EffectKind::Grayscale)]);
            assert!(
                (g.r - g.g).abs() < 0.12 && (g.g - g.b).abs() < 0.12,
                "grayscale converges the channels: {g:?}",
            );
            let inv = c.with_effects(&[ClipEffect {
                id: uuid::Uuid::new_v4(),
                kind: EffectKind::Invert,
                amount: 1.0,
            }]);
            assert!((inv.r - (1.0 - c.r)).abs() < 1e-6);
        }
        {
            use crate::model::{ClipEffect, EffectKind};
            let mut p = sample_project();
            let cid = p.clips[0].id;
            p.clip_mut(cid).unwrap().clear_effects();
            let CompositedFrame::Clip { base: before, .. } = composite(&p, 6.0) else {
                panic!("expected clip");
            };
            p.clip_mut(cid).unwrap().effects = vec![ClipEffect {
                id: uuid::Uuid::new_v4(),
                kind: EffectKind::Grayscale,
                amount: 1.0,
            }]
            .into();
            let CompositedFrame::Clip { base: after, .. } = composite(&p, 6.0) else {
                panic!("expected clip");
            };
            let spread = |c: Rgb| (c.r - c.g).abs() + (c.g - c.b).abs();
            assert!(
                spread(after) < spread(before),
                "grayscale reduces channel spread in the composite"
            );
        }
        {
            let c = palette_rgb(3);
            let g = c.graded(&Filters::default());
            assert!(
                (c.r - g.r).abs() < 1e-9 && (c.g - g.g).abs() < 1e-9 && (c.b - g.b).abs() < 1e-9
            );
        }
        {
            let p = sample_project();
            let CompositedFrame::Clip { flash: early, .. } = composite(&p, 21.05) else {
                panic!()
            };
            assert!(early.is_some(), "flash present right at the cut");
            let CompositedFrame::Clip { flash: late, .. } = composite(&p, 24.0) else {
                panic!()
            };
            assert!(late.is_none(), "flash gone well past the cut");
        }
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
            let cid = p.add_clip(tid, mid, 5.0).unwrap();
            p.clip_mut(cid).unwrap().in_point = 3.0;
            p.clip_mut(cid).unwrap().out_point = 13.0;
            let clip = p.clip(cid).unwrap();
            assert!((source_time_for(clip, 5.0) - 3.0).abs() < 1e-9);
            assert!((source_time_for(clip, 9.0) - 7.0).abs() < 1e-9);
        }
    }

    #[test]
    fn render_rgba_paints_the_graded_clip_opaque() {
        let p = sample_project();
        let buf = render_rgba(&p, 5.0, 32, 18);
        assert_eq!(buf.len(), 32 * 18 * 4);
        assert!(
            buf.chunks(4).all(|px| px[3] == 255),
            "every pixel is opaque"
        );
        let any_colour = buf.chunks(4).any(|px| px[0] > 8 || px[1] > 8 || px[2] > 8);
        assert!(any_colour, "the graded clip renders visible colour");
    }

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
        let a = p.add_clip(v1, m1, 0.0).unwrap();
        p.clip_mut(a).unwrap().out_point = 5.0;
        let b = p.add_clip(v1, m2, 5.0).unwrap();
        p.clip_mut(b).unwrap().out_point = 5.0;
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
        {
            let (p, v1, v2) = stacked_project();
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
        {
            let (mut p, _v1, v2) = stacked_project();
            let idx = p.tracks.iter().position(|t| t.id == v2).unwrap();
            p.tracks[idx].muted = true;
            let layers = plan_layers(&p, 7.0);
            assert_eq!(layers.len(), 1, "the muted track contributes nothing");
            assert_eq!(p.clip(layers[0].clip).unwrap().track, p.tracks[0].id);
            p.tracks[idx].muted = false;
            p.tracks[idx].locked = true;
            assert_eq!(
                plan_layers(&p, 7.0).len(),
                2,
                "a locked track still renders"
            );
        }
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

            p.tracks[i1].solo = true;
            assert_eq!(
                plan_layers(&p, 7.0).len(),
                2,
                "every track soloed shows every track"
            );

            p.tracks[i1].muted = true;
            let muted_solo = plan_layers(&p, 7.0);
            assert_eq!(
                muted_solo.len(),
                1,
                "a muted track stays hidden even when soloed"
            );
            assert_eq!(p.clip(muted_solo[0].clip).unwrap().track, v2);
        }
        {
            let (mut p, v1, _) = stacked_project();
            let cid = p.clips_on(v1)[1].id;
            p.clip_mut(cid).unwrap().in_point = 2.0;
            p.clip_mut(cid).unwrap().out_point = 7.0;
            let layers = plan_layers(&p, 6.0);
            let l = layers.iter().find(|l| l.clip == cid).unwrap();
            assert!(
                (l.source_time - 3.0).abs() < 1e-9,
                "source time follows the in-point: {l:?}"
            );
        }
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
                }]
                .into();
                c.fade_in = 2.0;
                c.opacity = 0.5;
            }
            let layers = plan_layers(&p, 1.0);
            assert_eq!(layers.len(), 1);
            let l = &layers[0];
            assert_eq!(l.filters.brightness, 0.7);
            assert_eq!(l.effects.len(), 1);
            assert_eq!(l.effects[0].kind, EffectKind::Sepia);
            assert!(
                (l.alpha - 0.25).abs() < 1e-9,
                "envelope × opacity: {}",
                l.alpha
            );
        }
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
        {
            // A transition is two layers, so both clips are sampled.
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
            assert!((layers[1].alpha - 1.0).abs() < 1e-9);
        }
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
        {
            let (mut p, v1, _) = stacked_project();
            let incoming = p.clips_on(v1)[1].id;
            p.clip_mut(incoming).unwrap().transition = Transition::CrossDissolve;

            let at_default_mid = 5.0 + TRANSITION_SECS * 0.5;
            let default_mid = plan_layers(&p, at_default_mid);
            assert!(
                (default_mid[1].alpha - 0.5).abs() < 0.02,
                "{:?}",
                default_mid[1].alpha
            );

            p.clip_mut(incoming).unwrap().transition_duration = TRANSITION_SECS * 2.0;
            let slower = plan_layers(&p, at_default_mid);
            assert!(
                (slower[1].alpha - 0.25).abs() < 0.02,
                "a longer transition is less far along at the same instant: {}",
                slower[1].alpha,
            );
            assert!(
                plan_layers(&p, 5.0 + TRANSITION_SECS * 1.5).len() == 2,
                "the outgoing clip is still underneath past the default length",
            );

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
