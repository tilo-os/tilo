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

use ui::text_render::{TextRenderer, TextStyle};
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
                &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            );
        }

        // Sizes are fractions of the frame height, so the same title reads
        // the same against a 1080p project and a 4K one.
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
        // Wrapped to the title-safe width, not just split on newlines. A
        // typed newline is still a break — it is the one the author asked for
        // — but a long sentence used to run off both edges of the frame and
        // out of the exported file, which is not something a title can be
        // allowed to do.
        //
        // The safe width matches the inset the left- and right-aligned cases
        // already use (8% each side), so a wrapped line never sits closer to
        // the edge than a deliberately aligned one.
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

        // Built (or refused, returning early) at the top of this call.
        let renderer = self.renderer.as_mut()?;
        Some(renderer.render_rgba(&scene))
    }
}

/// Break `line` into pieces that each fit inside `max_w`.
///
/// Greedy on word boundaries, which is what a reader expects and what every
/// other text surface here does. A single word too long for the frame — a URL,
/// a compound noun, a language that does not space its words — is broken by
/// character rather than allowed to run off the edge: a title that leaves the
/// frame is worse than one with an awkward break, because the part outside is
/// simply gone from the exported file.
///
/// Measured through the same `measure_font` the drawing uses, so the wrap and
/// the paint cannot disagree about what fits. Called only when a title's
/// raster is rebuilt, never per frame.
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
    // Breaking mid-letter is measured against the **frame**, not the
    // title-safe box. Safe-area is a placement margin, not a hard bound: a
    // single word that merely runs into the margin should use the margin,
    // because "HELLO" chopped to "HELL" / "O" is worse than a title sitting
    // slightly wider than broadcast-safe. Only a word that cannot fit the
    // picture at all is broken.
    let fits_frame = |tr: &mut ui::text_render::TextRenderer, s: &str| -> bool {
        f64::from(tr.measure_font(s, size, None, weight, italic, family).0) <= frame_w
    };

    // An empty line is a deliberate blank line, and must survive as one.
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
        // The word alone: on its own line if it fits the picture at all,
        // and only broken up when even that is impossible.
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
        // Case `the_renderer_hands_back_straight_alpha`:
        // The alpha convention the compositor's shader depends on.
        //
        // Its layer pass multiplies coverage by the texel's alpha and blends
        // the texel's *colour* over what is beneath. That is only correct for
        // **straight** alpha; premultiplied bytes would darken every
        // antialiased glyph edge toward black a second time. This pins the
        // convention rather than assuming it — the assumption was wrong the
        // first time it was made here.
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
        // Case `one_big_word_uses_the_safe_margin_rather_than_being_chopped`:
        // A single word wider than the title-safe box is left whole.
        //
        // Wrapping was added so a long title stops running off the frame, but it
        // took the safe box as a hard bound, so one big word got chopped
        // mid-letter — "HELLO" at 55% of frame height came out "HELL" / "O".
        // Safe-area is a placement margin, not a limit: a word that merely runs
        // into the margin should use the margin. Only a word too wide for the
        // picture itself is broken.
        {
            let mut tr = ui::text_render::TextRenderer::new();
            let family = ui::text_render::FontChoice::default();
            let weight = ui::text_render::TextWeight::Bold;
            // 1280x720 at size 0.52, at which the bold UI face sets the word
            // wider than 0.84 of the frame but narrower than the frame.
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

            // ...and a word that genuinely cannot fit the picture still breaks,
            // rather than running off both edges.
            let huge = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
            let broken = wrap_to(&mut tr, huge, size, weight, false, &family, safe, frame);
            assert!(
                broken.len() > 1,
                "a word too wide for the frame is still broken up: {broken:?}",
            );
        }
        // Case `a_title_rasters_visible_glyphs_on_a_transparent_field`.
        {
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
        // Case `an_empty_title_rasters_nothing_rather_than_a_block`.
        {
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
    }

    #[test]
    fn title_alignment_backdrop_and_cache_follow_the_style() {
        // Case `alignment_moves_the_glyphs_across_the_frame`.
        {
            let mut raster = TitleRaster::new();
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
        // Case `the_backdrop_fills_the_field_behind_the_text`.
        {
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
        // Case `rasters_are_cached_per_style_and_size`.
        {
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
}
