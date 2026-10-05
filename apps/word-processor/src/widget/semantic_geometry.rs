//! Read-only geometry from the text retained by the current Writer draw.
//!
//! This is not invocation authority or a presentation receipt. A future capture
//! adapter must separately bind its anchor and document frontier to that frame.

use super::{Affine, CARET_WIDTH, Rect, WordProcessorWidget, char_to_byte, num};
use tilo_actions::{InteractionAnchor, MAX_ANCHOR_ITEMS};

const MAX_TEXT_RECTS: usize = 4096;

impl WordProcessorWidget {
    /// Body text only: running-region layouts depend on the locally active
    /// region, so observing them cannot reliably resolve another participant.
    /// Returned rectangles are content coordinates, clipped through the actual
    /// pan/zoom into the scene's canvas. No layout, input or state is changed.
    pub(super) fn semantic_text_rects(
        &self,
        anchor: &InteractionAnchor,
        transform: Affine,
        canvas_clip: Rect,
    ) -> Option<Vec<Rect>> {
        let InteractionAnchor::Text { region, ranges } = anchor else {
            return None;
        };
        let [sx, ky, kx, sy, tx, ty] = transform.as_coeffs();
        if region != "body"
            || ranges.is_empty()
            || ranges.len() > MAX_ANCHOR_ITEMS
            || ![sx, ky, kx, sy, tx, ty].into_iter().all(f64::is_finite)
            || sx <= 0.0
            || sy <= 0.0
            || kx != 0.0
            || ky != 0.0
            || !finite_rect(canvas_clip)
        {
            return None;
        }
        // The rectangle contract supports axis-aligned positive pan/zoom.
        // Refuse other transforms instead of guessing from a bounding box.
        let content_clip = transform.inverse().transform_rect_bbox(canvas_clip);
        if !finite_rect(content_clip) {
            return None;
        }
        let mut rects = Vec::new();
        for range in ranges {
            let start = usize::try_from(range.start).ok()?;
            let end = usize::try_from(range.end).ok()?;
            let block = self.blocks.iter().find(|block| block.id == range.block)?;
            if matches!(block.kind.as_str(), "image" | "fidelity")
                || start > end
                || end > block.text.chars().count()
            {
                return None;
            }
            let (_, layout) = self.layout_cache.get(&range.block)?;
            let start = char_to_byte(&block.text, start);
            let end = char_to_byte(&block.text, end);
            let mut retained = false;
            for laid in self.laid.iter().filter(|laid| laid.id == range.block) {
                retained = true;
                if laid.text != block.text {
                    return None;
                }
                let (start, end) = (laid.shown(start), laid.shown(end));
                // A glyph may overhang its wrapping width, so horizontal
                // culling must wait for the real shaped geometry.
                if laid.y_top + laid.height <= content_clip.y0 || laid.y_top >= content_clip.y1 {
                    continue;
                }
                let clip = laid
                    .clip
                    .map_or(content_clip, |clip| clip.intersect(content_clip));
                if !finite_rect(clip) {
                    continue;
                }
                let spans = if start == end {
                    let (x, y, height) = layout.caret_geometry_bounded(start)?;
                    vec![(x, y, x + num::f32_of_f64(CARET_WIDTH), y + height)]
                } else {
                    layout.selection_rects_bounded(
                        start..end,
                        num::f32_of_f64(clip.y0 - laid.layout_y)
                            ..num::f32_of_f64(clip.y1 - laid.layout_y),
                        MAX_TEXT_RECTS - rects.len(),
                    )?
                };
                for (x0, y0, x1, y1) in spans {
                    let rect = Rect::new(
                        laid.text_x + f64::from(x0),
                        laid.layout_y + f64::from(y0),
                        laid.text_x + f64::from(x1),
                        laid.layout_y + f64::from(y1),
                    );
                    if !finite_rect(rect) {
                        return None;
                    }
                    let visible = rect.intersect(clip);
                    if finite_rect(visible) {
                        if rects.len() == MAX_TEXT_RECTS {
                            return None;
                        }
                        rects.push(visible);
                    }
                }
            }
            if !retained {
                return None;
            }
        }
        Some(rects)
    }
}

fn finite_rect(rect: Rect) -> bool {
    [rect.x0, rect.y0, rect.x1, rect.y1]
        .into_iter()
        .all(f64::is_finite)
        && rect.width() > 0.0
        && rect.height() > 0.0
}
