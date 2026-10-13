//! Additive differential controls for d3f933ec's baseline and outline APIs.
//! Original API bodies below are copied from the immutable target. Only their
//! cache-dependent helpers are substituted with the SAME production shaper's
//! fresh, uncached width probe and glyph flattening. Neither oracle calls the
//! candidate CacheQuery, cache key, public baseline or public outline method.
use super::*;

impl TextRenderer {
    fn pinned_original_collapse_width(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
        warm_shape: bool,
    ) -> Option<f32> {
        let width = max_width?;
        if style.wrap_mode == TextWrapMode::Balance
            || !matches!(style.hyphenation, Hyphenation::None)
            || (warm_shape && matches!(style.align, ParagraphAlign::Center | ParagraphAlign::End))
        {
            return Some(width);
        }
        let prepared = self.shape_buffer(text, None, style);
        let natural = Self::buffer_metrics(&prepared, style.line_height_px());
        if natural.0 <= width {
            None
        } else {
            Some(width)
        }
    }
    fn pinned_original_fresh_runs(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> Vec<GlyphRun> {
        let prepared = self.shape_buffer(text, max_width, style);
        self.flatten_glyph_runs(&prepared.buffer, style)
    }
    fn pinned_original_first_baseline_style(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> f32 {
        let scaled_style = self.scaled_style(style);
        let style = &scaled_style;
        if text.is_empty() {
            return style.size * 0.8;
        }
        let max_width = self.pinned_original_collapse_width(text, max_width, style, true);
        let runs = self.pinned_original_fresh_runs(text, max_width, style);
        runs.first()
            .and_then(|run| run.glyphs.first())
            .map_or(style.size * 0.8, |glyph| glyph.y)
    }

    fn pinned_original_glyph_outlines(
        &mut self,
        text: &str,
        max_width: Option<f32>,
        style: &TextStyle,
    ) -> Vec<GlyphOutline> {
        if text.is_empty() {
            return Vec::new();
        }
        let scaled_style = self.scaled_style(style);
        let style = &scaled_style;
        let max_width = self.pinned_original_collapse_width(text, max_width, style, false);
        let prepared = self.shape_buffer(text, max_width, style);
        let variations = style.resolved_variations();
        let line_starts: Vec<usize> = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at.saturating_add(1)))
            .collect();
        let mut placed = Vec::new();
        for run in prepared.buffer.layout_runs() {
            let line_start = line_starts.get(run.line_i).copied().unwrap_or(0);
            for glyph in run.glyphs {
                if run.text.get(glyph.start..glyph.end) == Some("\u{ad}") {
                    continue;
                }
                let (x, y) = glyph_paint_origin(
                    glyph,
                    hanging_glyph_x(&run, glyph, style.hanging_punctuation),
                    run.line_y,
                );
                placed.push((
                    glyph.font_id,
                    glyph.font_weight,
                    glyph.glyph_id,
                    glyph.font_size,
                    Point::new(f64::from(x), f64::from(y)),
                    f64::from(glyph.w),
                    line_start.saturating_add(glyph.start)..line_start.saturating_add(glyph.end),
                ));
            }
        }
        placed
            .into_iter()
            .filter_map(|(face, weight, glyph, size, origin, advance, cluster)| {
                let font = self.vello_font(face, weight)?;
                let coords = self.normalized_coords(face, &variations, weight);
                Some(GlyphOutline {
                    path: outline_path(&font, glyph, size, &coords)?,
                    origin,
                    advance,
                    cluster,
                })
            })
            .collect()
    }
}

fn compatibility_styles() -> Vec<TextStyle> {
    let plain = TextStyle::new(18.0);
    vec![
        plain.clone(),
        plain.clone().with_align(ParagraphAlign::Center),
        plain.clone().with_align(ParagraphAlign::End),
        plain.clone().with_wrap_mode(TextWrapMode::Balance),
        plain.clone().with_auto_hyphenation("en-US"),
        plain
            .with_hanging_punctuation(HangingPunctuation::BOTH)
            .with_font_variation(OpenTypeTag::WIDTH, 90.0)
            .with_font_feature(OpenTypeTag::KERNING, 0),
    ]
}
#[test]
fn main_first_baseline_matches_pinned_original_after_shared_cache_warmth() {
    for scale in [1.0, 1.5, 2.0] {
        let mut current = crate::font_catalog::fixture_renderer();
        let mut original = crate::font_catalog::fixture_renderer();
        current.set_text_scale(scale);
        original.set_text_scale(scale);
        for text in [
            "",
            " ",
            "\u{ad}",
            "office e\u{301} Ελληνικά العربية\nline two",
        ] {
            for style in compatibility_styles() {
                for width in [None, Some(40.0), Some(1000.0)] {
                    let prepared = PreparedText::new(text, style.clone());
                    current.measure_prepared(&prepared, width);
                    current.draw_prepared(
                        &mut Scene::new(),
                        &prepared,
                        Color::WHITE,
                        Affine::IDENTITY,
                        width,
                    );
                    let expected =
                        original.pinned_original_first_baseline_style(text, width, &style);
                    for _ in 0..2 {
                        assert_eq!(
                            current.first_baseline_style(text, width, &style).to_bits(),
                            expected.to_bits(),
                            "original baseline: {text:?} scale {scale} width {width:?} style {style:?}"
                        );
                    }
                    if text.is_empty() {
                        assert_eq!(expected.to_bits(), (style.size * scale * 0.8).to_bits());
                    }
                }
            }
        }
    }
}
fn assert_outline_geometry(actual: &[GlyphOutline], expected: &[GlyphOutline]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "original outlined glyph filtering/count"
    );
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(
            actual.origin, expected.origin,
            "original baseline placement"
        );
        assert_eq!(
            actual.advance.to_bits(),
            expected.advance.to_bits(),
            "original pen advance"
        );
        assert_eq!(
            actual.cluster, expected.cluster,
            "original UTF-8 cluster range"
        );
        assert_eq!(
            actual.path.elements(),
            expected.path.elements(),
            "original parsed font outline"
        );
    }
}
#[test]
fn main_glyph_outlines_match_pinned_original_after_other_public_cache_operations() {
    for scale in [1.0, 1.5, 2.0] {
        let mut current = crate::font_catalog::fixture_renderer();
        let mut original = crate::font_catalog::fixture_renderer();
        current.set_text_scale(scale);
        original.set_text_scale(scale);
        for text in [
            "",
            "office",
            "(office) e\u{301} Ελληνικά\nالعربية 👩‍💻",
            "hy\u{ad}phenation",
        ] {
            for style in compatibility_styles() {
                for width in [None, Some(40.0), Some(1000.0)] {
                    let prepared = PreparedText::new(text, style.clone());
                    current.measure_prepared(&prepared, width);
                    current.draw_style(
                        &mut Scene::new(),
                        text,
                        Color::WHITE,
                        Affine::IDENTITY,
                        width,
                        &style,
                    );
                    current.first_baseline_style(text, width, &style);
                    let expected = original.pinned_original_glyph_outlines(text, width, &style);
                    if text == "office" {
                        assert!(
                            !expected.is_empty(),
                            "real fixture must expose outline geometry"
                        );
                    }
                    for _ in 0..2 {
                        assert_outline_geometry(
                            &current.glyph_outlines(text, width, &style),
                            &expected,
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn main_outline_and_baseline_change_with_current_scale_width_and_font_generation() {
    let text = "Current typography wraps onto another line";
    let style = TextStyle::new(18.0).with_align(ParagraphAlign::End);
    let mut current = crate::font_catalog::fixture_renderer();
    let before = current.glyph_outlines(text, Some(90.0), &style);
    for (scale, width) in [(1.5, 50.0), (2.0, 160.0), (1.0, 90.0)] {
        current.set_text_scale(scale);
        current.bump_generation();
        let mut original = crate::font_catalog::fixture_renderer();
        original.set_text_scale(scale);
        let outlines = current.glyph_outlines(text, Some(width), &style);
        assert_outline_geometry(
            &outlines,
            &original.pinned_original_glyph_outlines(text, Some(width), &style),
        );
        assert_eq!(
            current
                .first_baseline_style(text, Some(width), &style)
                .to_bits(),
            original
                .pinned_original_first_baseline_style(text, Some(width), &style)
                .to_bits()
        );
        if scale != 1.0 {
            assert_ne!(
                outlines[0].path.elements(),
                before[0].path.elements(),
                "control must expose changed real outline, rather than empty/equal geometry"
            );
        }
    }
}
