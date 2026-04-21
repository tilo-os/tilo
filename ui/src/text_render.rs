use std::collections::HashMap;
use std::fs;
use cosmic_text::{FontSystem, SwashCache, Buffer, Metrics, Shaping, Wrap};
use cosmic_text::fontdb::Weight;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, Font, StyleRef};

/// Concept-aligned named weight tiers for shell chrome.
///
/// Tailwind's `font-medium` / `font-semibold` / `font-bold` map 1:1 to
/// [`cosmic_text::fontdb::Weight`]'s numeric constants. We expose them as an
/// enum so widget call sites read declaratively (e.g.
/// `draw_text_weighted(..., TextWeight::Bold)`) rather than passing raw u16s
/// around. See visual-gap #7 in `docs/nirios/visual-gaps.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextWeight {
    /// 400 — body / subtitle / explanatory text. Default.
    Regular,
    /// 500 — most chrome labels (buttons, secondary titles).
    Medium,
    /// 600 — active chrome titles (tab titles, workspace-name pill).
    Semibold,
    /// 700 — h1/h2, clock, numeric monospace values.
    Bold,
}

impl TextWeight {
    /// Numeric OpenType weight (400 / 500 / 600 / 700).
    pub const fn to_u16(self) -> u16 {
        match self {
            TextWeight::Regular => 400,
            TextWeight::Medium => 500,
            TextWeight::Semibold => 600,
            TextWeight::Bold => 700,
        }
    }

    /// Matching [`cosmic_text::fontdb::Weight`] constant to feed to
    /// [`cosmic_text::Attrs::weight`].
    pub const fn to_cosmic_weight(self) -> Weight {
        Weight(self.to_u16())
    }
}

impl Default for TextWeight {
    fn default() -> Self {
        TextWeight::Regular
    }
}

pub struct TextRenderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    font_cache: HashMap<cosmic_text::fontdb::ID, Font>,
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            font_cache: HashMap::new(),
        }
    }

    pub fn measure(&mut self, text: &str, size: f32, max_width: Option<f32>) -> (f32, f32) {
        self.measure_weighted(text, size, max_width, TextWeight::Regular)
    }

    /// Same as [`TextRenderer::measure`] but respects a typographic weight.
    /// Heavier weights generally advance wider, so measurements must use
    /// the same weight the draw path will render with (visual-gap #7).
    pub fn measure_weighted(
        &mut self,
        text: &str,
        size: f32,
        max_width: Option<f32>,
        weight: TextWeight,
    ) -> (f32, f32) {
        if text.is_empty() { return (0.0, 0.0); }

        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);

        buffer.set_size(&mut self.font_system, max_width, None);
        // Explicitly set wrap mode. Word wrap is standard for UI labels.
        buffer.set_wrap(&mut self.font_system, Wrap::Word);
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut width = 0.0f32;
        let mut lines = 0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        let height = lines as f32 * line_height;
        (width, height)
    }

    pub fn draw_text(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
    ) {
        self.draw_text_weighted(scene, text, size, color, transform, max_width, TextWeight::Regular)
    }

    /// Same as [`TextRenderer::draw_text`] but takes an explicit typographic
    /// [`TextWeight`]. Use this for chrome labels that need to lift off the
    /// body (e.g. top-bar clock at [`TextWeight::Bold`], tab titles at
    /// [`TextWeight::Semibold`]). See visual-gap #7.
    pub fn draw_text_weighted(
        &mut self,
        scene: &mut Scene,
        text: &str,
        size: f32,
        color: Color,
        transform: Affine,
        max_width: Option<f32>,
        weight: TextWeight,
    ) {
        if text.is_empty() { return; }

        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        let attrs = cosmic_text::Attrs::new().weight(weight.to_cosmic_weight());
        buffer.set_text(&mut self.font_system, text, &attrs, Shaping::Advanced, None);
        buffer.set_size(&mut self.font_system, max_width, None);
        buffer.set_wrap(&mut self.font_system, Wrap::Word);
        buffer.shape_until_scroll(&mut self.font_system, false);

        for run in buffer.layout_runs() {
            let mut current_face_id: Option<cosmic_text::fontdb::ID> = None;
            let mut glyphs_to_draw = Vec::new();

            for glyph in run.glyphs.iter() {
                let face_id = glyph.font_id;

                if current_face_id != Some(face_id) && !glyphs_to_draw.is_empty() {
                    self.flush_glyphs(scene, current_face_id.unwrap(), &glyphs_to_draw, size, color, transform, run.line_y);
                    glyphs_to_draw.clear();
                }
                
                current_face_id = Some(face_id);
                glyphs_to_draw.push(glyph.clone());
            }

            if let Some(face_id) = current_face_id {
                if !glyphs_to_draw.is_empty() {
                    self.flush_glyphs(scene, face_id, &glyphs_to_draw, size, color, transform, run.line_y);
                }
            }
        }
    }

    fn flush_glyphs(
        &mut self,
        scene: &mut Scene,
        face_id: cosmic_text::fontdb::ID,
        glyphs: &[cosmic_text::LayoutGlyph],
        size: f32,
        color: Color,
        transform: Affine,
        line_y: f32,
    ) {
        let vello_font = self.font_cache.entry(face_id).or_insert_with(|| {
            let font_data = self.font_system.db().face(face_id).and_then(|f| {
                match &f.source {
                    cosmic_text::fontdb::Source::Binary(data) => Some(data.as_ref().as_ref().to_vec()),
                    cosmic_text::fontdb::Source::File(path) => fs::read(path).ok(),
                    cosmic_text::fontdb::Source::SharedFile(_, data) => Some(data.as_ref().as_ref().to_vec()),
                }
            });
            
            if let Some(data) = font_data {
                Font::new(data.into(), 0)
            } else {
                Font::new(vec![].into(), 0)
            }
        });

        let vello_glyphs: Vec<vello::glyph::Glyph> = glyphs.iter().map(|g| {
            vello::glyph::Glyph {
                id: g.glyph_id as u32,
                x: g.x,
                y: line_y + g.y,
            }
        }).collect();

        scene.draw_glyphs(vello_font)
            .font_size(size)
            .brush(color)
            .transform(transform)
            .draw(
                StyleRef::Fill(vello::peniko::Fill::NonZero),
                vello_glyphs.into_iter()
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Visual-gap #7: the named weight tiers must resolve to Tailwind's
    /// numeric OpenType weights so chrome call sites can ask for
    /// `TextWeight::Bold` and trust they will get a `Weight(700)` handoff
    /// to cosmic-text. Any drift here would silently collapse every
    /// chrome label back onto the fallback font's single weight.
    #[test]
    fn named_weights_resolve_to_tailwind_numeric_values() {
        assert_eq!(TextWeight::Regular.to_u16(), 400);
        assert_eq!(TextWeight::Medium.to_u16(), 500);
        assert_eq!(TextWeight::Semibold.to_u16(), 600);
        assert_eq!(TextWeight::Bold.to_u16(), 700);
        assert_eq!(TextWeight::Bold.to_cosmic_weight(), Weight(700));
        assert_eq!(TextWeight::Regular.to_cosmic_weight(), Weight::NORMAL);
        // Default must be Regular so existing call sites that go through
        // the legacy `draw_text` wrapper keep their 400-weight behaviour.
        assert_eq!(TextWeight::default(), TextWeight::Regular);
    }
}
