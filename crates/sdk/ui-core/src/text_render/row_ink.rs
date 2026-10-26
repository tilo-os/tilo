//! How far the ink of a laid-out paragraph's rows reaches past their bands,
//! so rich text drawn into a clip considers every row whose ink may show.

use cosmic_text::fontdb::Weight;
use vello::kurbo::Rect;
use vello::peniko::FontData as Font;

use super::{FontVariation, TextRenderer, glyph_paint_origin};

/// The bounds a face declares for all its glyphs at `size` (its `head`
/// box), y down around a glyph's origin; its ascent and descent where it
/// declares none.
fn face_declared_ink(font: &Font, size: f32, coords: &[vello::NormalizedCoord]) -> Option<Rect> {
    use skrifa::instance::{LocationRef, NormalizedCoord, Size};
    use skrifa::{FontRef, MetadataProvider};
    let face = FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let location: Vec<_> = coords
        .iter()
        .map(|v| NormalizedCoord::from_bits(*v))
        .collect();
    let metrics = face.metrics(Size::new(size), LocationRef::new(&location));
    let bounds = metrics.bounds.map_or_else(
        || {
            Rect::new(
                0.0,
                -f64::from(metrics.ascent),
                0.0,
                -f64::from(metrics.descent),
            )
        },
        |bounds| {
            Rect::new(
                f64::from(bounds.x_min),
                -f64::from(bounds.y_max),
                f64::from(bounds.x_max),
                -f64::from(bounds.y_min),
            )
        },
    );
    [bounds.y0, bounds.y1]
        .iter()
        .all(|v| v.is_finite())
        .then_some(bounds)
}

/// The vertical extent of `bounds` (a glyph's ink around its origin, y
/// down) for `glyph` of `run`, on the baseline it is drawn on with its
/// baseline shift. A pixel's coverage is the area it shares with the
/// outline, so nothing paints past these bounds.
#[allow(
    clippy::cast_possible_truncation,
    reason = "glyph bounds at a text size are far inside f32"
)]
fn rich_glyph_ink(
    layout: &crate::richtext::RichTextLayout,
    run: &cosmic_text::LayoutRun<'_>,
    glyph: &cosmic_text::LayoutGlyph,
    bounds: Rect,
) -> (f32, f32) {
    let shift = if layout.baselines.is_empty() {
        0.0
    } else {
        layout
            .baseline_at(layout.glyph_global_byte(run.line_i, glyph.start))
            .offset(glyph.font_size)
    };
    let (_, y) = glyph_paint_origin(glyph, glyph.x, run.line_y + shift);
    (y + bounds.y0 as f32, y + bounds.y1 as f32)
}

/// The bounds each face a walk over glyphs meets declares
/// ([`face_declared_ink`]), looked up once for each face, weight and size.
#[derive(Default)]
struct DeclaredInk {
    seen: Vec<((cosmic_text::fontdb::ID, Weight, u32), Option<Rect>)>,
    last: Option<usize>,
}

impl DeclaredInk {
    fn of(
        &mut self,
        renderer: &mut TextRenderer,
        layout: &crate::richtext::RichTextLayout,
        glyph: &cosmic_text::LayoutGlyph,
    ) -> Option<Rect> {
        let key = (glyph.font_id, glyph.font_weight, glyph.font_size.to_bits());
        if let Some(last) = self.last
            && let Some((seen, bounds)) = self.seen.get(last)
            && *seen == key
        {
            return *bounds;
        }
        let index = if let Some(index) = self.seen.iter().position(|(seen, _)| *seen == key) {
            index
        } else {
            let bounds = renderer.face_declared_ink(
                glyph.font_id,
                glyph.font_weight,
                glyph.font_size,
                &layout.font_variations,
            );
            self.seen.push((key, bounds));
            self.seen.len().saturating_sub(1)
        };
        self.last = Some(index);
        self.seen.get(index).and_then(|(_, bounds)| *bounds)
    }
}

impl TextRenderer {
    /// How far the ink of `layout`'s rows reaches past their bands
    /// ([`crate::richtext::InkReach`]): from the bounds each face declares
    /// for all its glyphs, at the size and on the baseline each glyph is
    /// drawn with. The layout keeps each logical line's reach
    /// ([`crate::richtext::InkReachCache`]), so after an edit only the lines
    /// it replaced are walked, looking a face up only where it changes.
    pub fn rich_ink_reach(
        &mut self,
        layout: &crate::richtext::RichTextLayout,
    ) -> crate::richtext::InkReach {
        use crate::richtext::InkReach;
        if let Some(reach) = layout.ink_reach.all.get() {
            return *reach;
        }
        let mut lines = layout
            .ink_reach
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lines.resize(layout.buffer.lines.len(), None);
        let mut faces = DeclaredInk::default();
        // A line's rows come one after another: its reach is kept once the
        // next line starts.
        let mut pending: Option<(usize, InkReach)> = None;
        for run in layout.buffer.layout_runs() {
            if lines.get(run.line_i).is_none_or(Option::is_some) {
                continue;
            }
            if let Some((line, reach)) = pending.filter(|(line, _)| *line != run.line_i)
                && let Some(slot) = lines.get_mut(line)
            {
                *slot = Some(reach);
            }
            let mut reach = pending
                .filter(|(line, _)| *line == run.line_i)
                .map_or_else(InkReach::default, |(_, reach)| reach);
            let band_bottom = run.line_top + run.line_height;
            for glyph in run.glyphs {
                let Some(bounds) = faces.of(self, layout, glyph) else {
                    continue;
                };
                let (top, bottom) = rich_glyph_ink(layout, &run, glyph, bounds);
                reach.above = reach.above.max(run.line_top - top);
                reach.below = reach.below.max(bottom - band_bottom);
            }
            pending = Some((run.line_i, reach));
        }
        if let Some((line, reach)) = pending
            && let Some(slot) = lines.get_mut(line)
        {
            *slot = Some(reach);
        }
        let reach = lines
            .iter()
            .flatten()
            .fold(InkReach::default(), |all, line| InkReach {
                above: all.above.max(line.above),
                below: all.below.max(line.below),
            });
        let _ = layout.ink_reach.all.set(reach);
        reach
    }

    /// How far the ink of logical line `line`'s rows reaches past their
    /// bands, worked out as [`Self::rich_ink_reach`] does; the most of any
    /// line's for a line `layout` does not have.
    pub fn rich_line_ink_reach(
        &mut self,
        layout: &crate::richtext::RichTextLayout,
        line: usize,
    ) -> crate::richtext::InkReach {
        let all = self.rich_ink_reach(layout);
        layout
            .ink_reach
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(line)
            .copied()
            .flatten()
            .unwrap_or(all)
    }

    /// The bounds `face` declares for all its glyphs at `size`, y down
    /// ([`face_declared_ink`]).
    fn face_declared_ink(
        &mut self,
        face: cosmic_text::fontdb::ID,
        weight: Weight,
        size: f32,
        variations: &[FontVariation],
    ) -> Option<Rect> {
        let font = self.vello_font(face, weight)?;
        let coords = self.normalized_coords(face, variations, weight);
        face_declared_ink(&font, size, &coords)
    }
}
