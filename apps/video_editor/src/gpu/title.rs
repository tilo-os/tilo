//! Rasterising a title into a picture the compositor can treat as any
//! other layer.
//!
//! A [`MediaKind::Text`](crate::model::MediaKind::Text) source has no bytes
//! to decode — its picture is drawn from the document. It is drawn with the
//! *toolkit's* text stack ([`ui::text_render::TextRenderer`], i.e.
//! `cosmic-text`) rather than a second font pipeline, so a title is shaped,
//! hinted and fallback-substituted exactly the way every other string in
//! the OS is.
//!
//! The result is a straight-alpha RGBA picture the size of the project
//! frame. The compositor uploads it and blends it like a decoded frame,
//! which is what makes a title participate in the layer stack, the grade,
//! the effect chain, transitions and the geometric transform for free.
//!
//! Because nothing here decodes, a title renders identically in a hermetic
//! `--shots` capture and in the live editor.

use std::collections::HashMap;

use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

use crate::model::{TextAlign, TextSource};

/// Draws titles and remembers the last few.
///
/// One of these lives on the compositor. Rasterising costs a vello render,
/// so a title held under the playhead is drawn once and then re-used — but
/// the cache is keyed on the text and every style field, so editing a
/// title updates the picture on the next frame.
#[derive(Default)]
pub struct TitleRaster {
    cache: HashMap<String, std::sync::Arc<Vec<u8>>>,
    renderer: Option<ui::offscreen::OffscreenRenderer>,
    size: (u32, u32),
    text: Option<TextRenderer>,
    /// Set once a raster attempt has failed, so a box that cannot render
    /// one does not retry every frame of every title.
    unavailable: bool,
}

/// How many rasters to keep. A lower third, an opening title and a couple
/// of captions is the realistic working set; beyond that the oldest go.
const CACHE_LIMIT: usize = 12;

impl TitleRaster {
    pub fn new() -> Self {
        Self::default()
    }

    /// The picture for `text` at `width × height`, as tightly-packed
    /// straight-alpha RGBA.
    ///
    /// `None` when this box cannot rasterise at all — the layer then falls
    /// back to its representative fill, which is honest: it says "there is
    /// a clip here and I cannot draw it" rather than inventing a title.
    pub fn raster(
        &mut self,
        text: &TextSource,
        width: u32,
        height: u32,
    ) -> Option<std::sync::Arc<Vec<u8>>> {
        if self.unavailable || width == 0 || height == 0 {
            return None;
        }
        let key = format!("{}|{width}x{height}", text.raster_key());
        if let Some(hit) = self.cache.get(&key) {
            return Some(hit.clone());
        }
        let rgba = self.draw(text, width, height)?;
        if self.cache.len() >= CACHE_LIMIT {
            // Cheap eviction: the working set is small and every entry costs
            // a frame-sized buffer, so clearing beats tracking recency.
            self.cache.clear();
        }
        let rgba = std::sync::Arc::new(rgba);
        self.cache.insert(key, rgba.clone());
        Some(rgba)
    }

    /// Whether the last raster attempt found no way to draw. Callers show
    /// this rather than pretending the title rendered.
    pub fn is_unavailable(&self) -> bool {
        self.unavailable
    }

    fn draw(&mut self, text: &TextSource, width: u32, height: u32) -> Option<Vec<u8>> {
        if self.renderer.is_none() || self.size != (width, height) {
            match ui::offscreen::OffscreenRenderer::try_new(width, height) {
                Ok(r) => {
                    self.renderer = Some(r);
                    self.size = (width, height);
                    self.cache.clear();
                }
                Err(e) => {
                    tracing::warn!(error = %e, "no renderer for titles");
                    self.unavailable = true;
                    return None;
                }
            }
        }
        let tr = self.text.get_or_insert_with(TextRenderer::new);
        let mut scene = Scene::new();

        // A backing plate, when the title asks for one. Drawn first so the
        // glyphs sit on it.
        if text.backdrop > 1e-3 {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::new([0.0, 0.0, 0.0, text.backdrop.clamp(0.0, 1.0) as f32]),
                None,
                &Rect::new(0.0, 0.0, width as f64, height as f64),
            );
        }

        // Sizes are fractions of the frame height, so the same title reads
        // the same against a 1080p project and a 4K one.
        let size = (text.size.clamp(0.005, 2.0) * height as f64) as f32;
        let color = Color::new([
            text.color[0].clamp(0.0, 1.0) as f32,
            text.color[1].clamp(0.0, 1.0) as f32,
            text.color[2].clamp(0.0, 1.0) as f32,
            text.color[3].clamp(0.0, 1.0) as f32,
        ]);
        let weight = if text.bold {
            ui::text_render::TextWeight::Bold
        } else {
            ui::text_render::TextWeight::Regular
        };
        // An empty family name means "whatever this machine calls sans" —
        // which is the only honest default on a box whose installed fonts
        // we do not know.
        let family = if text.font.trim().is_empty() {
            ui::text_render::FontChoice::SansSerif
        } else {
            ui::text_render::FontChoice::Named(text.font.clone())
        };

        // Lay the lines out from the block's centre, so a multi-line title
        // stays centred on the frame however many lines it has. Where the
        // block sits *on* the frame is the clip transform's job, not the
        // raster's.
        let lines: Vec<&str> = text.content.lines().collect();
        let leading = size as f64 * 1.25;
        let block = leading * lines.len().max(1) as f64;
        let mut y = height as f64 * 0.5 - block * 0.5;
        for line in &lines {
            let (w, _h) = tr.measure_font(line, size, None, weight, text.italic, &family);
            let x = match text.align {
                TextAlign::Left => width as f64 * 0.08,
                TextAlign::Center => (width as f64 - w as f64) * 0.5,
                TextAlign::Right => width as f64 * 0.92 - w as f64,
            };
            tr.draw_text_font(
                &mut scene,
                line,
                size,
                color,
                Affine::translate((x, y)),
                None,
                weight,
                text.italic,
                &family,
            );
            y += leading;
        }

        let renderer = self.renderer.as_mut().expect("built above");
        Some(renderer.render_rgba(&scene))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The alpha convention the compositor's shader depends on.
    ///
    /// Its layer pass multiplies coverage by the texel's alpha and blends
    /// the texel's *colour* over what is beneath. That is only correct for
    /// **straight** alpha; premultiplied bytes would darken every
    /// antialiased glyph edge toward black a second time. This pins the
    /// convention rather than assuming it — the assumption was wrong the
    /// first time it was made here.
    #[test]
    fn the_renderer_hands_back_straight_alpha() {
        let Ok(mut r) = ui::offscreen::OffscreenRenderer::try_new(8, 8) else {
            panic!("the title raster tests need a wgpu adapter (llvmpipe is enough)");
        };
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::new([1.0, 1.0, 1.0, 0.5]),
            None,
            &Rect::new(0.0, 0.0, 8.0, 8.0),
        );
        let rgba = r.render_rgba(&scene);
        let (rr, a) = (rgba[0], rgba[3]);
        assert!((a as i32 - 128).abs() <= 2, "half alpha, got {a}");
        assert_eq!(
            rr, 255,
            "straight alpha: a half-transparent white keeps its full colour",
        );
    }

    #[test]
    fn a_title_rasters_visible_glyphs_on_a_transparent_field() {
        let mut raster = TitleRaster::new();
        let text = TextSource {
            content: "HELLO".into(),
            size: 0.4,
            ..TextSource::default()
        };
        let Some(rgba) = raster.raster(&text, 128, 64) else {
            panic!("the title raster tests need a wgpu adapter (llvmpipe is enough)");
        };
        assert_eq!(rgba.len(), 128 * 64 * 4);
        let opaque = rgba.chunks_exact(4).filter(|p| p[3] > 200).count();
        let clear = rgba.chunks_exact(4).filter(|p| p[3] == 0).count();
        assert!(opaque > 100, "the glyphs cover real pixels, got {opaque}");
        assert!(
            clear > 1000,
            "and most of the field stays transparent, got {clear}"
        );
        // White text: every solid texel is bright.
        let dark = rgba
            .chunks_exact(4)
            .filter(|p| p[3] > 200 && p[0] < 180)
            .count();
        assert_eq!(dark, 0, "white glyphs raster white, not grey");
    }

    #[test]
    fn an_empty_title_rasters_nothing_rather_than_a_block() {
        let mut raster = TitleRaster::new();
        let text = TextSource::new("");
        let Some(rgba) = raster.raster(&text, 32, 32) else {
            panic!("the title raster tests need a wgpu adapter (llvmpipe is enough)");
        };
        assert!(
            rgba.chunks_exact(4).all(|p| p[3] == 0),
            "a title with no text draws nothing at all",
        );
    }

    #[test]
    fn alignment_moves_the_glyphs_across_the_frame() {
        let mut raster = TitleRaster::new();
        let centre_of_mass = |rgba: &[u8], w: u32| {
            let (mut sum, mut n) = (0f64, 0f64);
            for (i, p) in rgba.chunks_exact(4).enumerate() {
                if p[3] > 128 {
                    sum += (i as u32 % w) as f64;
                    n += 1.0;
                }
            }
            if n == 0.0 { f64::NAN } else { sum / n }
        };
        let mut at = |align| {
            let t = TextSource {
                content: "AB".into(),
                size: 0.3,
                align,
                ..TextSource::default()
            };
            let rgba = raster.raster(&t, 160, 48).expect("needs a wgpu adapter");
            centre_of_mass(&rgba, 160)
        };
        let left = at(TextAlign::Left);
        let centre = at(TextAlign::Center);
        let right = at(TextAlign::Right);
        assert!(
            left < centre,
            "left-aligned sits left of centred: {left} vs {centre}"
        );
        assert!(
            centre < right,
            "and centred left of right-aligned: {centre} vs {right}"
        );
    }

    #[test]
    fn the_backdrop_fills_the_field_behind_the_text() {
        let mut raster = TitleRaster::new();
        let t = TextSource {
            content: "x".into(),
            backdrop: 0.8,
            ..TextSource::default()
        };
        let rgba = raster.raster(&t, 32, 32).expect("needs a wgpu adapter");
        // A corner is well away from any glyph, so it is plate or nothing.
        let corner = &rgba[0..4];
        assert!(
            corner[3] > 150,
            "the plate covers the whole field, corner alpha was {}",
            corner[3],
        );
    }

    #[test]
    fn rasters_are_cached_per_style_and_size() {
        let mut raster = TitleRaster::new();
        let a = TextSource::new("same");
        assert!(raster.raster(&a, 64, 32).is_some());
        assert_eq!(raster.cache.len(), 1);
        assert!(raster.raster(&a, 64, 32).is_some());
        assert_eq!(raster.cache.len(), 1, "an identical request is a cache hit");
        let b = TextSource {
            content: "different".into(),
            ..a.clone()
        };
        assert!(raster.raster(&b, 64, 32).is_some());
        assert_eq!(raster.cache.len(), 2, "changed text rasters again");
        assert!(raster.raster(&a, 96, 32).is_some());
        assert_eq!(
            raster.cache.len(),
            1,
            "a new frame size rebuilds the renderer"
        );
    }
}
