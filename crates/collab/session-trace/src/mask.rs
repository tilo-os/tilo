//! Exact frame redaction.
//!
//! The trace knows where every sensitive node was on screen, so frame
//! redaction is exact rather than guessed: each rectangle a `Personal` or
//! `Secret` node covered is filled, rounded outward to whole pixels, in the
//! stored frame itself. Pixels no semantic node describes (a web view, a
//! foreign or legacy client, a surface with no keyframe at all) are not
//! guessed at either: they are flagged per region as unexplained, for the
//! reporter to look at in review.

use crate::frames::{ElementRegion, PixelRect, SurfaceFrame, pixel_index};

/// What masked pixels are filled with: opaque, and far from any theme's
/// text or surface colour, so a mask reads as a mask in review.
pub const MASK_RGBA: [u8; 4] = [0x3a, 0x3a, 0x3a, 0xff];

/// A region list longer than this is merged into its bounding box; the
/// flagged area can only grow.
const MAX_UNEXPLAINED: usize = 64;

/// What masking a frame found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaskReport {
    pub masked: Vec<PixelRect>,
    pub unexplained: Vec<PixelRect>,
}

/// The pixel rectangles of every node in `regions` that must be masked.
pub fn masked_rects(regions: &[ElementRegion], scale: f64, bounds: PixelRect) -> Vec<PixelRect> {
    let mut rects: Vec<PixelRect> = Vec::new();
    for region in regions.iter().filter(|region| region.masked) {
        let rect = PixelRect::covering(&region.bounds, scale, bounds);
        if !rect.is_empty() && !rects.iter().any(|kept| kept.contains(&rect)) {
            rects.retain(|kept| !rect.contains(kept));
            rects.push(rect);
        }
    }
    rects
}

/// The pixel rectangles no described node covers, plus every node whose
/// own content is not described. A surface with no layout is unexplained
/// as a whole.
pub fn unexplained_rects(
    regions: &[ElementRegion],
    scale: f64,
    bounds: PixelRect,
) -> Vec<PixelRect> {
    let mut uncovered = vec![bounds];
    for region in regions.iter().filter(|region| region.described) {
        let cut = PixelRect::covering(&region.bounds, scale, bounds);
        if cut.is_empty() {
            continue;
        }
        uncovered = uncovered
            .iter()
            .flat_map(|rect| rect.subtract(&cut))
            .collect();
        if uncovered.is_empty() {
            break;
        }
        uncovered = bounded(uncovered);
    }
    for region in regions.iter().filter(|region| !region.described) {
        let rect = PixelRect::covering(&region.bounds, scale, bounds);
        if !rect.is_empty() && !uncovered.iter().any(|kept| kept.contains(&rect)) {
            uncovered.push(rect);
        }
    }
    bounded(uncovered)
}

fn bounded(rects: Vec<PixelRect>) -> Vec<PixelRect> {
    if rects.len() <= MAX_UNEXPLAINED {
        return rects;
    }
    vec![
        rects
            .iter()
            .fold(PixelRect::default(), |all, rect| all.union(rect)),
    ]
}

/// Fill every masked node's rectangle in `frame` and report what was
/// masked and what no node explains.
pub fn mask_frame(frame: &mut SurfaceFrame, regions: &[ElementRegion]) -> MaskReport {
    let bounds = frame.bounds();
    let masked = masked_rects(regions, frame.scale, bounds);
    for rect in &masked {
        fill(frame, rect, MASK_RGBA);
    }
    MaskReport {
        unexplained: unexplained_rects(regions, frame.scale, bounds),
        masked,
    }
}

/// Paint `rect` of `frame` with `rgba`. `rect` is clipped to the frame.
pub fn fill(frame: &mut SurfaceFrame, rect: &PixelRect, rgba: [u8; 4]) {
    let rect = rect.intersect(&frame.bounds());
    if rect.is_empty() {
        return;
    }
    for y in rect.y..rect.bottom() {
        let (Some(start), Some(end)) = (
            pixel_index(frame.width, rect.x, y),
            pixel_index(frame.width, rect.right(), y),
        ) else {
            continue;
        };
        if let Some(row) = frame.rgba.get_mut(start..end) {
            for pixel in row.chunks_exact_mut(4) {
                pixel.copy_from_slice(&rgba);
            }
        }
    }
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
    use crate::frames::{FrameLimits, FrameStripBuilder, PixelPurpose, SurfaceRect};
    use crate::{Offset, Route};

    fn frame(width: u32, height: u32, scale: f64) -> SurfaceFrame {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                rgba.extend_from_slice(&[
                    u8::try_from(x % 256).unwrap(),
                    u8::try_from(y % 256).unwrap(),
                    200,
                    255,
                ]);
            }
        }
        SurfaceFrame::new(
            Offset(0),
            Route::new("so.tilo.mail", "room-1", ""),
            width,
            height,
            scale,
            rgba,
        )
        .unwrap()
    }

    fn node(id: &str, bounds: SurfaceRect, masked: bool) -> ElementRegion {
        ElementRegion {
            id: id.into(),
            bounds,
            masked,
            described: true,
            transient: false,
        }
    }

    fn is_mask(frame: &SurfaceFrame, x: u32, y: u32) -> bool {
        frame.pixel(x, y) == Some(MASK_RGBA)
    }

    #[test]
    fn a_personal_node_is_masked_exactly_and_nothing_else_is() {
        let original = frame(64, 48, 1.0);
        let mut masked = original.clone();
        let regions = [
            node("root", edges(0.0, 0.0, 64.0, 48.0), false),
            node("to", edges(10.0, 8.0, 30.0, 16.0), true),
        ];
        let report = mask_frame(&mut masked, &regions);
        assert_eq!(report.masked, vec![PixelRect::new(10, 8, 20, 8)]);
        assert!(report.unexplained.is_empty(), "the root describes it all");
        for y in 0..48 {
            for x in 0..64 {
                let inside = (10..30).contains(&x) && (8..16).contains(&y);
                if inside {
                    assert!(is_mask(&masked, x, y), "({x},{y}) must be masked");
                } else {
                    assert_eq!(
                        masked.pixel(x, y),
                        original.pixel(x, y),
                        "({x},{y}) untouched"
                    );
                }
            }
        }
    }

    #[test]
    fn fractional_and_scaled_bounds_round_outward() {
        let mut masked = frame(40, 40, 2.0);
        let regions = [node("secret", edges(2.25, 3.5, 5.1, 6.0), true)];
        let report = mask_frame(&mut masked, &regions);
        // 2.25*2 = 4.5 -> 4, 5.1*2 = 10.2 -> 11; 3.5*2 = 7, 6*2 = 12.
        assert_eq!(report.masked, vec![PixelRect::new(4, 7, 7, 5)]);
        assert!(is_mask(&masked, 4, 7) && is_mask(&masked, 10, 11));
        assert!(!is_mask(&masked, 3, 7) && !is_mask(&masked, 11, 11));
        assert!(!is_mask(&masked, 4, 6) && !is_mask(&masked, 4, 12));
    }

    #[test]
    fn a_node_reaching_past_the_surface_is_clipped() {
        let mut masked = frame(16, 16, 1.0);
        let regions = [node("n", edges(-4.0, 10.0, 40.0, 99.0), true)];
        let report = mask_frame(&mut masked, &regions);
        assert_eq!(report.masked, vec![PixelRect::new(0, 10, 16, 6)]);
    }

    #[test]
    fn content_no_node_describes_is_flagged_not_guessed() {
        let mut captured = frame(100, 50, 1.0);
        let regions = [
            node("sidebar", edges(0.0, 0.0, 30.0, 50.0), false),
            ElementRegion {
                id: "web".into(),
                bounds: edges(40.0, 10.0, 90.0, 40.0),
                masked: false,
                described: false,
                transient: false,
            },
        ];
        let before = captured.clone();
        let report = mask_frame(&mut captured, &regions);
        assert!(report.masked.is_empty());
        assert_eq!(
            captured, before,
            "unexplained pixels are flagged, never painted"
        );
        let area: u64 = report.unexplained.iter().map(PixelRect::area).sum();
        assert_eq!(
            area,
            70 * 50,
            "everything right of the sidebar is unexplained"
        );
        assert!(
            report
                .unexplained
                .iter()
                .all(|rect| rect.x >= 30 && rect.right() <= 100)
        );

        let mut foreign = frame(20, 20, 1.0);
        let report = mask_frame(&mut foreign, &[]);
        assert_eq!(report.unexplained, vec![PixelRect::new(0, 0, 20, 20)]);
    }

    #[test]
    fn stored_frames_hold_the_mask_and_sealing_covers_a_late_keyframe() {
        use crate::fixtures::route;
        use crate::{Event, InspectKeyframe, Origin, TimedEvent};
        let mut strip = FrameStripBuilder::new(PixelPurpose::Keep, FrameLimits::default());
        let mut shot = frame(32, 32, 1.0);
        shot.route = route("so.tilo.mail");
        shot.at = Offset(1_000);
        let early = [node("a", edges(0.0, 0.0, 8.0, 8.0), true)];
        assert_eq!(
            strip.push(shot.clone(), &early).unwrap(),
            crate::frames::FramePush::Kept
        );
        assert_eq!(
            strip.push(shot, &early).unwrap(),
            crate::frames::FramePush::Unchanged
        );
        // The keyframe that shows node `b` reached the trace after the frame.
        let late = vec![
            node("a", edges(0.0, 0.0, 8.0, 8.0), true),
            node("b", edges(16.0, 16.0, 24.0, 24.0), true),
        ];
        let trace = crate::SessionTrace {
            origin: Origin::Explicit,
            started_unix_ms: 0,
            duration: Offset(5_000),
            environment: crate::Environment::default(),
            snapshots: Vec::new(),
            timeline: vec![TimedEvent {
                at: Offset(1_200),
                event: Event::Inspect(InspectKeyframe {
                    route: route("so.tilo.mail"),
                    reply: crate::frames::answer(&late),
                }),
            }],
            tags: Vec::new(),
            layer: crate::SessionLayer::Logic,
            redactions: crate::Redactions::default(),
            truncated: false,
        };
        let sealed = strip.finish(&trace).unwrap();
        let bytes = sealed.encode().unwrap();
        let read = crate::frames::FrameStrip::decode(&bytes).unwrap();
        assert_eq!(read, sealed);
        let stored = &read.frames[0];
        assert_eq!(
            stored.masked,
            vec![PixelRect::new(0, 0, 8, 8), PixelRect::new(16, 16, 8, 8)]
        );
        let pixels = stored.decode().unwrap();
        assert!(is_mask(&pixels, 0, 0) && is_mask(&pixels, 7, 7));
        assert!(is_mask(&pixels, 16, 16) && is_mask(&pixels, 23, 23));
        assert!(!is_mask(&pixels, 12, 12));
        let unexplained: u64 = stored.unexplained.iter().map(PixelRect::area).sum();
        assert_eq!(
            unexplained,
            32 * 32 - 2 * 64,
            "only the two nodes' own rectangles are described"
        );
    }
}
