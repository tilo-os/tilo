//! Does the replay look like what the user saw?
//!
//! Before a report leaves the device, the device replays its own trace
//! offscreen and compares the replayed frames with the captured ones,
//! element by element, using the Inspect bounds in the trace. They match:
//! the problem is one of logic or UX, [`SessionLayer::Logic`]. They differ,
//! or a region has no semantic node: the problem is below the toolkit
//! (renderer, fonts, driver, scaling, compositor),
//! [`SessionLayer::BelowToolkit`], and the diverging regions are offered as
//! small cropped frames to attach.
//!
//! The comparison is perceptual: a pixel differs only when its colour
//! distance (`YIQ`, weighted as the eye weighs it) passes a threshold **and**
//! no pixel within one step of it on the other side is close. The second
//! condition is what ignores anti-aliasing noise: an edge rasterised a
//! sub-pixel off moves its blended pixels by one, it does not change them.

use serde::{Deserialize, Serialize};

use crate::frames::{
    ElementRegion, FrameError, FrameProvider, FrameStrip, PixelRect, SemanticLayout, StoredFrame,
    SurfaceFrame, encode_png, pixel_index,
};
use crate::mask::{MASK_RGBA, fill, masked_rects};
use crate::{Offset, Route, SessionLayer, SessionTrace};

/// How lenient the comparison is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tolerance {
    /// Colour distance, 0 (identical) to 1 (black against white), below
    /// which two pixels are the same.
    pub color: f64,
    /// A differing pixel is anti-aliasing noise when a pixel this many
    /// steps away on the other side is the same.
    pub neighbourhood: u32,
    /// A region diverges only with more differing pixels than this…
    pub min_pixels: u64,
    /// …and than this fraction of its area.
    pub min_fraction: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            color: 0.1,
            neighbourhood: 1,
            min_pixels: 4,
            min_fraction: 0.000_5,
        }
    }
}

/// Largest edge of an evidence crop.
pub const EVIDENCE_EDGE: u32 = 256;
/// Context kept around a diverging region in its evidence crop.
const EVIDENCE_MARGIN: u32 = 4;

/// Why a region is offered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionDivergence {
    /// Replayed pixels differ from captured ones.
    Differs { pixels: u64 },
    /// No semantic node describes these pixels, so replay cannot vouch for
    /// them.
    Unexplained,
    /// The replay rendered no such surface.
    Missing,
    /// The replay rendered the surface at another size.
    Resized {
        captured: (u32, u32),
        replayed: (u32, u32),
    },
}

/// A small cropped frame showing a region, as a PNG.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    /// The crop, in the frame's pixels, before downscaling.
    pub crop: PixelRect,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// One region of one step that replay does not vouch for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DivergentRegion {
    /// The semantic node, when one describes the region.
    pub element: Option<String>,
    pub bounds: PixelRect,
    pub divergence: RegionDivergence,
    /// Masked like the stored frame.
    pub captured: Evidence,
    /// Masked the same way; absent when the replay has no such surface.
    pub replayed: Option<Evidence>,
}

/// The regions of one surface at one step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepDivergence {
    pub at: Offset,
    pub route: Route,
    pub regions: Vec<DivergentRegion>,
}

/// The verdict of a divergence check.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DivergenceReport {
    /// `Logic` when every region of every step matched; the value of
    /// [`crate::SessionSummary::layer`].
    pub layer: SessionLayer,
    /// Only steps with something to offer.
    pub steps: Vec<StepDivergence>,
    /// Steps compared.
    pub compared: u32,
}

/// Compare `strip`'s frames with what `replay` renders at the same offsets.
///
/// # Errors
///
/// When the replay cannot reach a step or a stored frame cannot be read.
pub fn check(
    trace: &SessionTrace,
    strip: &FrameStrip,
    replay: &mut dyn FrameProvider,
    tolerance: &Tolerance,
) -> Result<DivergenceReport, FrameError> {
    let layout = SemanticLayout::from_trace(trace);
    let mut report = DivergenceReport {
        layer: SessionLayer::Logic,
        steps: Vec::new(),
        compared: 0,
    };
    for at in strip.offsets() {
        let replayed = replay.frames_at(at)?;
        for stored in strip.frames.iter().filter(|frame| frame.at == at) {
            report.compared = report.compared.saturating_add(1);
            let regions = layout.for_comparison(&stored.route, at);
            let found = replayed
                .iter()
                .find(|frame| frame.same_surface(&stored.route));
            let step = compare_frame(stored, found, &regions, tolerance)?;
            if !step.regions.is_empty() {
                report.steps.push(step);
            }
        }
    }
    report.layer = if report.steps.is_empty() {
        SessionLayer::Logic
    } else {
        SessionLayer::BelowToolkit
    };
    Ok(report)
}

/// Compare one stored frame with its replayed counterpart, region by
/// region.
///
/// # Errors
///
/// When the stored frame cannot be decoded or evidence cannot be encoded.
pub fn compare_frame(
    stored: &StoredFrame,
    replayed: Option<&SurfaceFrame>,
    regions: &[ElementRegion],
    tolerance: &Tolerance,
) -> Result<StepDivergence, FrameError> {
    let captured = stored.decode()?;
    let bounds = captured.bounds();
    let mut step = StepDivergence {
        at: stored.at,
        route: stored.route.clone(),
        regions: Vec::new(),
    };
    let whole = |divergence, replayed: Option<Evidence>| -> Result<DivergentRegion, FrameError> {
        Ok(DivergentRegion {
            element: None,
            bounds,
            divergence,
            captured: evidence(&captured, bounds)?,
            replayed,
        })
    };
    let Some(replayed) = replayed else {
        step.regions.push(whole(RegionDivergence::Missing, None)?);
        return Ok(step);
    };
    if replayed.width != captured.width || replayed.height != captured.height {
        let shown = evidence(replayed, replayed.bounds())?;
        step.regions.push(whole(
            RegionDivergence::Resized {
                captured: (captured.width, captured.height),
                replayed: (replayed.width, replayed.height),
            },
            Some(shown),
        )?);
        return Ok(step);
    }
    // Replayed pixels carry the same data the user's did: mask them the
    // same way before comparing or offering them. The stored frame's own
    // mask list is authoritative, the layout adds nothing it lacks.
    let mut replayed = replayed.clone();
    for rect in stored
        .masked
        .iter()
        .chain(&masked_rects(regions, stored.scale, bounds))
    {
        fill(&mut replayed, rect, MASK_RGBA);
    }
    // What the recording device painted on its own clock and from its own
    // state is left out on both sides: no replay reproduces it, so it
    // neither diverges nor matches.
    let mut compared = captured.clone();
    for rect in transient_rects(regions, stored.scale, bounds) {
        fill(&mut compared, &rect, MASK_RGBA);
        fill(&mut replayed, &rect, MASK_RGBA);
    }
    let differs = difference(&compared, &replayed, tolerance);
    for rect in &stored.unexplained {
        step.regions.push(DivergentRegion {
            element: None,
            bounds: *rect,
            divergence: RegionDivergence::Unexplained,
            captured: evidence(&captured, *rect)?,
            replayed: Some(evidence(&replayed, *rect)?),
        });
    }
    // Smallest element first: a difference is attributed to the innermost
    // node that shows it, and a parent diverges only for differences its
    // diverging children do not account for.
    let mut elements: Vec<(&ElementRegion, PixelRect)> = regions
        .iter()
        .filter(|region| region.described && !region.masked && !region.transient)
        .map(|region| {
            (
                region,
                PixelRect::covering(&region.bounds, stored.scale, bounds),
            )
        })
        .filter(|(_, rect)| !rect.is_empty())
        .collect();
    elements.sort_by_key(|(_, rect)| rect.area());
    let mut claimed = vec![false; differs.len()];
    for (region, rect) in elements {
        let pixels = count_unclaimed(&differs, &claimed, captured.width, &rect);
        if pixels <= threshold(&rect, tolerance) {
            continue;
        }
        claim(&differs, &mut claimed, captured.width, &rect);
        step.regions.push(DivergentRegion {
            element: Some(region.id.clone()),
            bounds: rect,
            divergence: RegionDivergence::Differs { pixels },
            captured: evidence(&captured, rect)?,
            replayed: Some(evidence(&replayed, rect)?),
        });
    }
    Ok(step)
}

/// The pixel rectangles of every node in `regions` that paints transient
/// feedback (see [`ElementRegion::transient`]).
fn transient_rects(regions: &[ElementRegion], scale: f64, bounds: PixelRect) -> Vec<PixelRect> {
    regions
        .iter()
        .filter(|region| region.transient)
        .map(|region| PixelRect::covering(&region.bounds, scale, bounds))
        .filter(|rect| !rect.is_empty())
        .collect()
}

fn threshold(rect: &PixelRect, tolerance: &Tolerance) -> u64 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "a non-negative fraction of a pixel count, ceiled"
    )]
    let fraction = (rect.area() as f64 * tolerance.min_fraction.max(0.0)).ceil() as u64;
    tolerance.min_pixels.max(fraction)
}

fn cells(width: u32, rect: &PixelRect) -> impl Iterator<Item = usize> + '_ {
    (rect.y..rect.bottom()).flat_map(move |y| {
        (rect.x..rect.right()).filter_map(move |x| pixel_index(width, x, y).map(|index| index / 4))
    })
}

fn count_unclaimed(differs: &[bool], claimed: &[bool], width: u32, rect: &PixelRect) -> u64 {
    cells(width, rect)
        .filter(|&cell| {
            differs.get(cell).copied().unwrap_or(false)
                && !claimed.get(cell).copied().unwrap_or(true)
        })
        .fold(0_u64, |count, _| count.saturating_add(1))
}

fn claim(differs: &[bool], claimed: &mut [bool], width: u32, rect: &PixelRect) {
    for cell in cells(width, rect) {
        if differs.get(cell).copied().unwrap_or(false)
            && let Some(slot) = claimed.get_mut(cell)
        {
            *slot = true;
        }
    }
}

/// Per pixel, whether `a` and `b` differ beyond anti-aliasing noise.
/// Both frames have the same size.
pub fn difference(a: &SurfaceFrame, b: &SurfaceFrame, tolerance: &Tolerance) -> Vec<bool> {
    let limit = tolerance.color.clamp(0.0, 1.0).powi(2) * MAX_YIQ_DELTA;
    let reach = tolerance.neighbourhood;
    let mut out = Vec::with_capacity(
        usize::try_from(u64::from(a.width).saturating_mul(u64::from(a.height))).unwrap_or(0),
    );
    for y in 0..a.height {
        for x in 0..a.width {
            let (Some(pa), Some(pb)) = (a.pixel(x, y), b.pixel(x, y)) else {
                out.push(true);
                continue;
            };
            if yiq_delta(pa, pb) <= limit {
                out.push(false);
                continue;
            }
            let noise = blends_into(pa, b, x, y, reach) && blends_into(pb, a, x, y, reach);
            out.push(!noise);
        }
    }
    out
}

/// Slack, per channel, on the blend range of a neighbourhood.
const BLEND_SLACK: u8 = 8;

/// Whether `pixel` is a blend of what `frame` shows within `reach` of
/// `(x, y)`: every channel lies within that neighbourhood's range. An edge
/// rasterised a sub-pixel off only re-blends the colours on either side of
/// it; a changed pixel shows a colour that is not there.
fn blends_into(pixel: [u8; 4], frame: &SurfaceFrame, x: u32, y: u32, reach: u32) -> bool {
    let x0 = x.saturating_sub(reach);
    let y0 = y.saturating_sub(reach);
    let x1 = x.saturating_add(reach).min(frame.width.saturating_sub(1));
    let y1 = y.saturating_add(reach).min(frame.height.saturating_sub(1));
    let mut low = [u8::MAX; 4];
    let mut high = [u8::MIN; 4];
    for qy in y0..=y1 {
        for qx in x0..=x1 {
            if let Some(q) = frame.pixel(qx, qy) {
                for ((low, high), value) in low.iter_mut().zip(high.iter_mut()).zip(q) {
                    *low = (*low).min(value);
                    *high = (*high).max(value);
                }
            }
        }
    }
    pixel
        .iter()
        .zip(low.iter().zip(&high))
        .all(|(value, (low, high))| {
            *value >= low.saturating_sub(BLEND_SLACK) && *value <= high.saturating_add(BLEND_SLACK)
        })
}

/// Largest [`yiq_delta`], black against white.
const MAX_YIQ_DELTA: f64 = 35_215.0;

/// Squared perceptual distance of two pixels in `YIQ`, each first composed
/// over white (premultiplied alpha), weighted as in Kotsarenko and Ramos'
/// "Measuring perceived color difference using YIQ NTSC transmission color
/// space in mobile applications".
fn yiq_delta(a: [u8; 4], b: [u8; 4]) -> f64 {
    let over_white = |pixel: [u8; 4]| {
        let alpha = f64::from(pixel[3]) / 255.0;
        let channel = |value: u8| f64::from(value) + 255.0 * (1.0 - alpha);
        (channel(pixel[0]), channel(pixel[1]), channel(pixel[2]))
    };
    let yiq = |(red, green, blue): (f64, f64, f64)| {
        (
            red * 0.298_895_31 + green * 0.586_622_47 + blue * 0.114_482_23,
            red * 0.595_977_99 - green * 0.274_176_80 - blue * 0.321_801_19,
            red * 0.211_470_17 - green * 0.522_617_11 + blue * 0.311_146_94,
        )
    };
    let (luma_a, in_phase_a, quadrature_a) = yiq(over_white(a));
    let (luma_b, in_phase_b, quadrature_b) = yiq(over_white(b));
    let luma = luma_a - luma_b;
    let in_phase = in_phase_a - in_phase_b;
    let quadrature = quadrature_a - quadrature_b;
    0.5053 * luma * luma + 0.299 * in_phase * in_phase + 0.1957 * quadrature * quadrature
}

/// A crop of `frame` around `rect`, downscaled to at most
/// [`EVIDENCE_EDGE`], as a PNG.
///
/// # Errors
///
/// When the crop cannot be encoded.
pub fn evidence(frame: &SurfaceFrame, rect: PixelRect) -> Result<Evidence, FrameError> {
    let crop = rect.inflate(EVIDENCE_MARGIN, frame.bounds());
    if crop.is_empty() {
        return Ok(Evidence {
            crop,
            width: 0,
            height: 0,
            png: Vec::new(),
        });
    }
    let longest = crop.width.max(crop.height);
    let (width, height) = if longest <= EVIDENCE_EDGE {
        (crop.width, crop.height)
    } else {
        let scale = |edge: u32| ratio(edge, EVIDENCE_EDGE, longest).clamp(1, EVIDENCE_EDGE);
        (scale(crop.width), scale(crop.height))
    };
    let mut rgba = Vec::with_capacity(crate::frames::rgba_len(width, height).unwrap_or(0));
    for y in 0..height {
        for x in 0..width {
            // Nearest source pixel: evidence shows what was there, blended
            // pixels would invent colours the frame never had.
            let sx = crop.x.saturating_add(ratio(x, crop.width, width));
            let sy = crop.y.saturating_add(ratio(y, crop.height, height));
            rgba.extend_from_slice(&frame.pixel(sx, sy).unwrap_or([0, 0, 0, 0]));
        }
    }
    Ok(Evidence {
        crop,
        width,
        height,
        png: encode_png(&rgba, width, height)?,
    })
}

/// `value * numerator / denominator` without overflow; 0 for a zero
/// denominator.
fn ratio(value: u32, numerator: u32, denominator: u32) -> u32 {
    u64::from(value)
        .saturating_mul(u64::from(numerator))
        .checked_div(u64::from(denominator))
        .and_then(|scaled| u32::try_from(scaled).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A surface rectangle from its edges.
    fn edges(x0: f64, y0: f64, x1: f64, y1: f64) -> SurfaceRect {
        SurfaceRect {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        }
    }
    use crate::fixtures::route;
    use crate::frames::{FrameLimits, FrameStripBuilder, PixelPurpose, SurfaceRect};
    use crate::{Event, InspectKeyframe, Origin, TimedEvent};

    const W: u32 = 96;
    const H: u32 = 64;

    fn blank() -> SurfaceFrame {
        SurfaceFrame::new(
            Offset(0),
            route("so.tilo.notes"),
            W,
            H,
            1.0,
            [250, 250, 250, 255].repeat((W * H) as usize),
        )
        .unwrap()
    }

    /// A dark bar from `x0` to `x1` (sub-pixel), rows 20..30, with the
    /// partly covered edge columns blended the way a rasteriser would.
    fn with_bar(mut frame: SurfaceFrame, x0: f64, x1: f64, ink: u8) -> SurfaceFrame {
        for y in 20..30 {
            for x in 0..W {
                let left = f64::from(x);
                let cover = (x1.min(left + 1.0) - x0.max(left)).clamp(0.0, 1.0);
                if cover > 0.0 {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "a blend of two bytes stays a byte"
                    )]
                    let value = (f64::from(ink) * cover + 250.0 * (1.0 - cover)).round() as u8;
                    fill(
                        &mut frame,
                        &PixelRect::new(x, y, 1, 1),
                        [value, value, value, 255],
                    );
                }
            }
        }
        frame
    }

    fn regions() -> Vec<ElementRegion> {
        let node = |id: &str, bounds| ElementRegion {
            id: id.into(),
            bounds,
            masked: false,
            described: true,
            transient: false,
        };
        vec![
            node("root", edges(0.0, 0.0, f64::from(W), f64::from(H))),
            node("title", edges(8.0, 16.0, 80.0, 34.0)),
            node("status", edges(8.0, 44.0, 80.0, 56.0)),
        ]
    }

    fn trace_with(regions: &[ElementRegion]) -> SessionTrace {
        SessionTrace {
            origin: Origin::Explicit,
            started_unix_ms: 0,
            duration: Offset(10_000),
            environment: crate::Environment::default(),
            snapshots: Vec::new(),
            timeline: vec![TimedEvent {
                at: Offset(0),
                event: Event::Inspect(InspectKeyframe {
                    route: route("so.tilo.notes"),
                    reply: crate::frames::answer(regions),
                }),
            }],
            tags: Vec::new(),
            layer: SessionLayer::Logic,
            redactions: crate::Redactions::default(),
            truncated: false,
        }
    }

    fn strip_of(trace: &SessionTrace, frames: Vec<SurfaceFrame>) -> FrameStrip {
        let layout = SemanticLayout::from_trace(trace);
        let mut builder = FrameStripBuilder::new(PixelPurpose::Check, FrameLimits::default());
        for frame in frames {
            let regions = layout
                .at(&frame.route, frame.at)
                .unwrap_or_default()
                .to_vec();
            builder.push(frame, &regions).unwrap();
        }
        builder.finish(trace).unwrap()
    }

    struct Replay(Vec<SurfaceFrame>, Vec<Offset>);

    impl FrameProvider for Replay {
        fn frames_at(&mut self, at: Offset) -> Result<Vec<SurfaceFrame>, FrameError> {
            // Tests assert on the order asked for.
            self.1.push(at);
            Ok(self
                .0
                .iter()
                .filter(|frame| frame.at == at)
                .cloned()
                .collect())
        }
    }

    fn at(frame: SurfaceFrame, offset: u64) -> SurfaceFrame {
        SurfaceFrame {
            at: Offset(offset),
            ..frame
        }
    }

    #[test]
    fn identical_frames_are_a_logic_problem() {
        let trace = trace_with(&regions());
        let shot = with_bar(blank(), 10.0, 60.0, 20);
        let strip = strip_of(&trace, vec![at(shot.clone(), 100), at(blank(), 200)]);
        let mut replay = Replay(vec![at(shot, 100), at(blank(), 200)], Vec::new());
        let report = check(&trace, &strip, &mut replay, &Tolerance::default()).unwrap();
        assert_eq!(report.layer, SessionLayer::Logic);
        assert!(report.steps.is_empty());
        assert_eq!(report.compared, 2);
        assert_eq!(replay.1, vec![Offset(100), Offset(200)]);
    }

    #[test]
    fn anti_aliasing_noise_is_not_a_divergence() {
        let trace = trace_with(&regions());
        let captured = with_bar(blank(), 10.3, 60.3, 20);
        // Rasterised a fraction of a pixel off, with a slightly different
        // blend: every edge pixel changes, none of the content does.
        let replayed = with_bar(blank(), 10.8, 60.8, 24);
        assert_ne!(captured, replayed);
        let strip = strip_of(&trace, vec![at(captured, 100)]);
        let mut replay = Replay(vec![at(replayed, 100)], Vec::new());
        let report = check(&trace, &strip, &mut replay, &Tolerance::default()).unwrap();
        assert_eq!(report.layer, SessionLayer::Logic, "{:?}", report.steps);
    }

    #[test]
    fn a_genuine_difference_names_the_element_and_crops_evidence() {
        let trace = trace_with(&regions());
        let captured = with_bar(blank(), 10.0, 60.0, 20);
        // The replay drew a much shorter title: the renderer dropped glyphs.
        let replayed = with_bar(blank(), 10.0, 30.0, 20);
        let strip = strip_of(&trace, vec![at(captured, 100)]);
        let mut replay = Replay(vec![at(replayed, 100)], Vec::new());
        let report = check(&trace, &strip, &mut replay, &Tolerance::default()).unwrap();
        assert_eq!(report.layer, SessionLayer::BelowToolkit);
        assert_eq!(report.steps.len(), 1);
        let step = &report.steps[0];
        assert_eq!(step.at, Offset(100));
        assert_eq!(step.regions.len(), 1, "{:?}", step.regions);
        let region = &step.regions[0];
        assert_eq!(region.element.as_deref(), Some("title"));
        // 30 columns by 10 rows went missing; only the two corner pixels
        // where both sides blend into each other read as edge noise.
        assert_eq!(region.divergence, RegionDivergence::Differs { pixels: 298 });
        assert_eq!(region.bounds, PixelRect::new(8, 16, 72, 18));
        assert_eq!(region.captured.crop, PixelRect::new(4, 12, 80, 26));
        let crop = crate::frames::decode_png(
            &region.captured.png,
            region.captured.width,
            region.captured.height,
        )
        .unwrap();
        assert_eq!(crop.len(), 80 * 26 * 4);
        assert!(region.replayed.is_some());
    }

    #[test]
    fn unexplained_and_missing_surfaces_are_below_the_toolkit() {
        let only_title = vec![ElementRegion {
            id: "title".into(),
            bounds: edges(0.0, 0.0, f64::from(W), 40.0),
            masked: false,
            described: true,
            transient: false,
        }];
        let trace = trace_with(&only_title);
        let strip = strip_of(&trace, vec![at(blank(), 100)]);
        let mut replay = Replay(vec![at(blank(), 100)], Vec::new());
        let report = check(&trace, &strip, &mut replay, &Tolerance::default()).unwrap();
        assert_eq!(report.layer, SessionLayer::BelowToolkit);
        let offered = &report.steps[0].regions;
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].divergence, RegionDivergence::Unexplained);
        assert_eq!(offered[0].bounds, PixelRect::new(0, 40, W, H - 40));

        let trace = trace_with(&regions());
        let strip = strip_of(&trace, vec![at(blank(), 100)]);
        let mut empty = Replay(Vec::new(), Vec::new());
        let report = check(&trace, &strip, &mut empty, &Tolerance::default()).unwrap();
        assert_eq!(
            report.steps[0].regions[0].divergence,
            RegionDivergence::Missing
        );
    }

    #[test]
    fn masked_nodes_are_masked_on_both_sides_and_never_compared() {
        let mut layout = regions();
        layout.push(ElementRegion {
            id: "recipient".into(),
            bounds: edges(8.0, 44.0, 80.0, 56.0),
            masked: true,
            described: true,
            transient: false,
        });
        let trace = trace_with(&layout);
        let captured = with_bar(blank(), 10.0, 60.0, 20);
        let mut replayed = captured.clone();
        // The replay shows another name under the mask.
        fill(
            &mut replayed,
            &PixelRect::new(10, 46, 40, 8),
            [0, 0, 0, 255],
        );
        let strip = strip_of(&trace, vec![at(captured, 100)]);
        let mut replay = Replay(vec![at(replayed, 100)], Vec::new());
        let report = check(&trace, &strip, &mut replay, &Tolerance::default()).unwrap();
        assert_eq!(report.layer, SessionLayer::Logic, "{:?}", report.steps);
    }

    #[test]
    fn transient_feedback_is_left_out_at_every_step_it_ever_appeared_in() {
        let pill = ElementRegion {
            id: "root~transient:0".into(),
            bounds: edges(8.0, 44.0, 80.0, 56.0),
            masked: false,
            described: true,
            transient: true,
        };
        let mut shown = regions();
        shown.push(pill.clone());
        let layout = SemanticLayout::from_keyframes(vec![
            (route("so.tilo.notes"), Offset(50), shown),
            (route("so.tilo.notes"), Offset(90), regions()),
        ]);
        // At 100 the recording showed no pill, yet the replay may.
        let compared = layout.for_comparison(&route("so.tilo.notes"), Offset(100));
        assert!(compared.contains(&pill), "{compared:?}");
        assert!(
            layout
                .for_comparison(&route("so.tilo.other"), Offset(100))
                .is_empty(),
            "another surface declared nothing"
        );
    }

    #[test]
    fn transient_feedback_is_left_out_on_both_sides() {
        let mut layout = regions();
        layout.push(ElementRegion {
            id: "save-status".into(),
            bounds: edges(8.0, 44.0, 80.0, 56.0),
            masked: false,
            described: true,
            transient: true,
        });
        let trace = trace_with(&layout);
        // The recording caught the pill fading; the replay shows it solid.
        let captured = with_bar(blank(), 10.0, 60.0, 20);
        let mut replayed = captured.clone();
        fill(
            &mut replayed,
            &PixelRect::new(10, 46, 40, 8),
            [0, 0, 0, 255],
        );
        let strip = strip_of(&trace, vec![at(captured.clone(), 100)]);
        let mut replay = Replay(vec![at(replayed.clone(), 100)], Vec::new());
        let report = check(&trace, &strip, &mut replay, &Tolerance::default()).unwrap();
        assert_eq!(report.layer, SessionLayer::Logic, "{:?}", report.steps);

        // Outside it the same difference still diverges.
        let trace = trace_with(&regions());
        let strip = strip_of(&trace, vec![at(captured, 100)]);
        let mut replay = Replay(vec![at(replayed, 100)], Vec::new());
        let report = check(&trace, &strip, &mut replay, &Tolerance::default()).unwrap();
        assert_eq!(report.layer, SessionLayer::BelowToolkit);
    }
}
