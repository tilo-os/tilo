use std::collections::HashMap;
use std::fs;
use cosmic_text::{FontSystem, SwashCache, Buffer, Metrics, Shaping, Wrap};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, Font, StyleRef};

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
        if text.is_empty() { return (0.0, 0.0); }
        
        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        buffer.set_text(&mut self.font_system, text, &cosmic_text::Attrs::new(), Shaping::Advanced, None);
        
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
        if text.is_empty() { return; }
        
        let line_height = size * 1.25;
        let mut buffer = Buffer::new(&mut self.font_system, Metrics::new(size, line_height));
        buffer.set_text(&mut self.font_system, text, &cosmic_text::Attrs::new(), Shaping::Advanced, None);
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
