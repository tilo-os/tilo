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
//!   * **Real decode (`--features decode`)** — [`decode_frame`] resolves the
//!     clip's bytes through the [`crate::media`] FUSE seam and, when they're
//!     hydrated, feeds the nearest segment to a `ui::VideoPlayer` (openh264)
//!     to produce a true RGBA picture. If the bytes aren't hydrated yet (a
//!     projected reference on a fresh collaborator) it falls back to the
//!     representative frame — the editor never blocks on hydration.
//!
//! The colour math here is the single source of truth for *both* the
//! representative gradient and (eventually) a real shader, so the grade you
//! see in the offline preview matches the graded export.

use crate::model::{Clip, Filters, MediaId, Project, Transition};

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
        Self { r: r.clamp(0.0, 1.0), g: g.clamp(0.0, 1.0), b: b.clamp(0.0, 1.0) }
    }
    pub fn luma(&self) -> f64 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }
    /// Apply a per-clip [`Filters`] grade. Brightness scales, contrast
    /// pivots around mid-grey, saturation lerps toward/away from luma.
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
    pub fn faded(self, gain: f64) -> Rgb {
        let g = gain.clamp(0.0, 1.0);
        Rgb::new(self.r * g, self.g * g, self.b * g)
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
    let base = palette_rgb(clip.palette).graded(&clip.filters);
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
    Some(Flash { white: clip.transition != Transition::DipToBlack, alpha })
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

/// Real-decode entry point. Behind the `decode` feature it resolves the
/// clip's bytes through the FUSE seam and decodes the nearest frame; without
/// the feature it is a no-op that returns the representative frame unchanged.
///
/// Always falls back gracefully: an un-hydrated (projected) reference or a
/// decode failure yields the representative frame, so the preview is never
/// blank or blocking.
#[cfg(feature = "decode")]
pub fn decode_frame(
    project: &Project,
    playhead: f64,
    player: &mut ui::widget::VideoPlayer,
) -> CompositedFrame {
    let frame = composite(project, playhead);
    let CompositedFrame::Clip { media, base, envelope, flash, .. } = frame else {
        return frame;
    };
    let Some(item) = project.media(media) else { return frame };
    let source = item.source();
    if !source.is_decodable() {
        // Projected reference: bytes not hydrated yet — representative frame.
        return CompositedFrame::Clip { media, base, envelope, flash, decoded: false };
    }
    // Hydrated: read the source and feed it to the openh264-backed player.
    // (A production build seeks to the keyframe at the playhead; this slice
    // feeds the leading bytes, which is enough to prove the seam end-to-end.)
    let decoded = match std::fs::read(&source.abs_path) {
        Ok(bytes) => player.feed_frame(&bytes),
        Err(_) => false,
    };
    CompositedFrame::Clip { media, base, envelope, flash, decoded }
}

/// Without the decode feature, the preview is always the representative
/// frame — kept as a same-named fn so the call site is feature-agnostic.
#[cfg(not(feature = "decode"))]
pub fn decode_frame(project: &Project, playhead: f64) -> CompositedFrame {
    composite(project, playhead)
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
            _ => panic!("expected a clip at the playhead"),
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
        assert!(head < mid, "head ({head}) should be dimmer than mid ({mid})");
        assert!(head < 0.2, "very head of a fade-in is near-black");
    }

    #[test]
    fn grade_changes_the_base_colour() {
        let neutral = palette_rgb(0);
        let brighter = neutral.graded(&Filters { brightness: 0.9, contrast: 0.5, saturation: 0.5 });
        assert!(brighter.luma() > neutral.luma(), "brightness raises luma");
        let desat = neutral.graded(&Filters { brightness: 0.5, contrast: 0.5, saturation: 0.0 });
        // Full desaturation collapses channels toward luma → near-equal.
        assert!((desat.r - desat.g).abs() < 1e-6 && (desat.g - desat.b).abs() < 1e-6);
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
}
