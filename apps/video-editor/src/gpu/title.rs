//! Rasterising a title into a picture the compositor treats as any other layer.
//!
//! A text source has no bytes to decode: its picture is drawn from the document,
//! with the toolkit's text stack, into straight-alpha RGBA the size of the project
//! frame. The compositor blends it like a decoded frame, so a title takes part in
//! the layer stack, the grade, the effect chain, transitions and the transform for
//! free — and renders the same in a capture as in the live editor.

use std::collections::HashMap;

use ui::text_render::{TextRenderer, TextStyle};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, Fill};

use crate::model::{TextAlign, TextSource};

/// Rasterising costs a vello render, so a title held under the playhead is drawn
/// once and re-used. The cache is keyed on the text and every style field, so
/// editing a title updates the picture on the next frame.
#[derive(Default)]
pub struct TitleRaster {
    cache: HashMap<String, std::sync::Arc<Vec<u8>>>,
    renderer: Option<ui::offscreen::OffscreenRenderer>,
    size: (u32, u32),
    text: Option<TextRenderer>,
    /// Set once a raster attempt has failed, so a box that cannot render one does
    /// not retry every frame.
    unavailable: bool,
}

/// Beyond this the oldest go: every entry costs a frame-sized buffer.
const CACHE_LIMIT: usize = 12;

impl TitleRaster {
    /// `None` when this box cannot rasterise at all: the layer then falls back to
    /// its representative fill rather than inventing a title.
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
            self.cache.clear();
        }
        let rgba = std::sync::Arc::new(rgba);
        self.cache.insert(key, rgba.clone());
        Some(rgba)
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

        if text.backdrop > 1e-3 {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::new([0.0, 0.0, 0.0, text.backdrop.clamp(0.0, 1.0) as f32]),
                None,
                &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            );
        }

        let size = (text.size.clamp(0.005, 2.0) * f64::from(height)) as f32;
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
        // An empty family name means "whatever this machine calls sans", the only
        // honest default on a box whose fonts we do not know.
        let family = if text.font.trim().is_empty() {
            ui::text_render::FontChoice::SansSerif
        } else {
            ui::text_render::FontChoice::Named(text.font.clone())
        };

        // Lines are laid out from the block's centre, so a multi-line title stays
        // centred on the frame. Where the block sits on the frame is the clip
        // transform's job.
        //
        // Wrapped to the title-safe width, not just split on newlines: a typed
        // newline is a break the author asked for, but a long sentence used to run
        // off both edges of the frame and out of the exported file. The safe width
        // matches the 8% inset the aligned cases use.
        let safe = f64::from(width) * 0.84;
        let lines: Vec<String> = text
            .content
            .lines()
            .flat_map(|line| {
                wrap_to(
                    tr,
                    line,
                    size,
                    weight,
                    text.italic,
                    &family,
                    safe,
                    f64::from(width),
                )
            })
            .collect();
        let leading = f64::from(size) * 1.25;
        let block = leading * lines.len().max(1) as f64;
        let mut y = f64::from(height) * 0.5 - block * 0.5;
        for line in &lines {
            let (w, _h) = tr.measure_font(line.as_str(), size, None, weight, text.italic, &family);
            let x = match text.align {
                TextAlign::Left => f64::from(width) * 0.08,
                TextAlign::Center => (f64::from(width) - f64::from(w)) * 0.5,
                TextAlign::Right => f64::from(width) * 0.92 - f64::from(w),
            };
            tr.draw_style(
                &mut scene,
                line.as_str(),
                color,
                Affine::translate((x, y)),
                None,
                &TextStyle::new(size)
                    .with_weight(weight)
                    .with_italic(text.italic)
                    .with_family(family.clone()),
            );
            y += leading;
        }

        let renderer = self.renderer.as_mut()?;
        Some(renderer.render_rgba(&scene))
    }
}

/// Greedy on word boundaries. A single word too long for the frame is broken by
/// character rather than allowed to run off the edge, since the part outside is
/// simply gone from the exported file.
///
/// Measured through the same `measure_font` the drawing uses, so the wrap and the
/// paint agree about what fits. Called only when a raster is rebuilt.
fn wrap_to(
    tr: &mut ui::text_render::TextRenderer,
    line: &str,
    size: f32,
    weight: ui::text_render::TextWeight,
    italic: bool,
    family: &ui::text_render::FontChoice,
    max_w: f64,
    frame_w: f64,
) -> Vec<String> {
    let fits = |tr: &mut ui::text_render::TextRenderer, s: &str| -> bool {
        f64::from(tr.measure_font(s, size, None, weight, italic, family).0) <= max_w
    };
    // Mid-letter breaking is measured against the **frame**, not the title-safe box:
    // safe-area is a placement margin, and "HELLO" chopped to "HELL" / "O" is worse
    // than a title sitting slightly wider than broadcast-safe.
    let fits_frame = |tr: &mut ui::text_render::TextRenderer, s: &str| -> bool {
        f64::from(tr.measure_font(s, size, None, weight, italic, family).0) <= frame_w
    };

    // An empty line is a deliberate blank line.
    if line.trim().is_empty() {
        return vec![String::new()];
    }

    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if fits(tr, &candidate) {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if fits_frame(tr, word) {
            current = word.to_string();
            continue;
        }
        let mut piece = String::new();
        for ch in word.chars() {
            let mut next = piece.clone();
            next.push(ch);
            if piece.is_empty() || fits_frame(tr, &next) {
                piece = next;
            } else {
                out.push(std::mem::take(&mut piece));
                piece.push(ch);
            }
        }
        current = piece;
    }
    if !current.is_empty() {
        out.push(current);
    }
    if out.is_empty() {
        vec![String::new()]
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_rasters_straight_alpha_glyphs_within_the_safe_margin() {
        // The layer pass multiplies coverage by the texel's alpha and blends the
        // texel's colour over what is beneath, which is only correct for **straight**
        // alpha: premultiplied bytes would darken every antialiased edge.
        {
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
            assert!((i32::from(a) - 128).abs() <= 2, "half alpha, got {a}");
            assert_eq!(
                rr, 255,
                "straight alpha: a half-transparent white keeps its full colour",
            );
        }
        // A single word wider than the title-safe box is left whole: safe-area is a
        // margin, not a limit.
        {
            let mut tr = ui::text_render::TextRenderer::new();
            let family = ui::text_render::FontChoice::default();
            let weight = ui::text_render::TextWeight::Bold;
            let size = 0.52 * 720.0;
            let safe = 1280.0 * 0.84;
            let frame = 1280.0;
            let w = f64::from(
                tr.measure_font("HELLO", size, None, weight, false, &family)
                    .0,
            );
            assert!(
                w > safe && w < frame,
                "this test is only meaningful for a word between the safe box and \
             the frame; measured {w:.0} against safe {safe:.0}, frame {frame:.0}",
            );

            let lines = wrap_to(&mut tr, "HELLO", size, weight, false, &family, safe, frame);
            assert_eq!(
                lines,
                vec!["HELLO".to_string()],
                "a word that fits the picture stays one word",
            );

            let huge = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
            let broken = wrap_to(&mut tr, huge, size, weight, false, &family, safe, frame);
            assert!(
                broken.len() > 1,
                "a word too wide for the frame is still broken up: {broken:?}",
            );
        }
        {
            let mut raster = TitleRaster::default();
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
            let dark = rgba
                .chunks_exact(4)
                .filter(|p| p[3] > 200 && p[0] < 180)
                .count();
            assert_eq!(dark, 0, "white glyphs raster white, not grey");
        }
        {
            let mut raster = TitleRaster::default();
            let text = TextSource::new("");
            let Some(rgba) = raster.raster(&text, 32, 32) else {
                panic!("the title raster tests need a wgpu adapter (llvmpipe is enough)");
            };
            assert!(
                rgba.chunks_exact(4).all(|p| p[3] == 0),
                "a title with no text draws nothing at all",
            );
        }
    }

    #[test]
    fn title_alignment_backdrop_and_cache_follow_the_style() {
        {
            let mut raster = TitleRaster::default();
            let centre_of_mass = |rgba: &[u8], w: u32| {
                let (mut sum, mut n) = (0f64, 0f64);
                for (i, p) in rgba.chunks_exact(4).enumerate() {
                    if p[3] > 128 {
                        sum += f64::from(i as u32 % w);
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
        {
            let mut raster = TitleRaster::default();
            let t = TextSource {
                content: "x".into(),
                backdrop: 0.8,
                ..TextSource::default()
            };
            let rgba = raster.raster(&t, 32, 32).expect("needs a wgpu adapter");
            let corner = &rgba[0..4];
            assert!(
                corner[3] > 150,
                "the plate covers the whole field, corner alpha was {}",
                corner[3],
            );
        }
        {
            let mut raster = TitleRaster::default();
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
}
