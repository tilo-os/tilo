#![cfg(test)]

use super::*;
use crate::font_catalog::{FontCatalog, configured_font_system_with_catalog};
use crate::richtext::{RichSpan, RichTextLayout, TextAlign};
use cosmic_text::fontdb::{ID, Source};
use skrifa::{FontRef, MetadataProvider, Tag};

fn catalog() -> FontCatalog {
    FontCatalog::controlled_fixture(crate::font_catalog::checkout_root(), "en-US")
}

fn renderer() -> TextRenderer {
    TextRenderer::with_font_catalog(&catalog()).expect("verified controlled catalog")
}

fn named_style(family: &str) -> TextStyle {
    TextStyle::new(24.0)
        .with_family(FontChoice::Named(family.to_owned()))
        .with_optical_sizing(OpticalSizing::None)
}

fn draw(renderer: &mut TextRenderer, text: &str, style: &TextStyle) -> Scene {
    let mut scene = Scene::new();
    renderer.draw_style(
        &mut scene,
        text,
        Color::WHITE,
        Affine::IDENTITY,
        None,
        style,
    );
    assert!(!scene.encoding().resources.glyph_runs.is_empty());
    scene
}

fn one_run(scene: &Scene) -> &vello_encoding::GlyphRun {
    let runs = &scene.encoding().resources.glyph_runs;
    assert_eq!(runs.len(), 1);
    assert!(!runs[0].glyphs.is_empty());
    &runs[0]
}

fn face_id(renderer: &TextRenderer, family: &str) -> ID {
    renderer
        .font_system
        .db()
        .query(&cosmic_text::fontdb::Query {
            families: &[Family::Name(family)],
            ..cosmic_text::fontdb::Query::default()
        })
        .expect("catalog family")
}

fn assert_source_owner(renderer: &TextRenderer, id: ID, font: &Font) {
    let (owner, _) = font.data.clone().into_raw_parts();
    let face = renderer.font_system.db().face(id).expect("shaped face");
    match &face.source {
        Source::Binary(data) | Source::SharedFile(_, data) => {
            assert!(
                Arc::ptr_eq(&owner, data),
                "paint must retain the shaping owner"
            );
        }
        Source::File(_) => panic!("shaping must have resolved the file source"),
    }
}

#[test]
fn font_sources_and_collection_indices_are_shared_with_emitted_runs() {
    // phase: collection_indices_and_shared_owners_reach_the_real_scene
    {
        let mut renderer = renderer();
        let mut fonts = Vec::new();
        for (family, index) in [
            ("Noto Sans CJK JP", 0),
            ("Noto Sans CJK KR", 1),
            ("Noto Sans CJK SC", 2),
            ("Noto Sans CJK TC", 3),
            ("Noto Sans CJK HK", 4),
        ] {
            let id = face_id(&renderer, family);
            assert_eq!(renderer.font_system.db().face(id).unwrap().index, index);
            let style = named_style(family);
            let scene = draw(&mut renderer, "漢", &style);
            let font = one_run(&scene).font.clone();
            assert_eq!(font.index, index, "{family}");
            assert_source_owner(&renderer, id, &font);
            let repeat = draw(&mut renderer, "漢", &style);
            assert_eq!(one_run(&repeat).font.data.id(), font.data.id());
            fonts.push(font);
        }
        let (owner, _) = fonts[0].data.clone().into_raw_parts();
        for font in &fonts[1..] {
            let (other, _) = font.data.clone().into_raw_parts();
            assert!(
                Arc::ptr_eq(&owner, &other),
                "TTC faces share one byte owner"
            );
        }
    }
    // phase: binary_font_source_is_shared_with_emitted_runs
    {
        let mut renderer = renderer();
        // Keep the explicit fixture database, replacing only this known TTC's
        // representation to cover Binary as well as the loader's File source.
        let ids: Vec<_> = renderer
            .font_system
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.starts_with("Noto Sans CJK "))
            })
            .map(|face| face.id)
            .collect();
        assert_eq!(ids.len(), 5);
        for id in ids {
            renderer.font_system.db_mut().remove_face(id);
        }
        let bytes =
            std::fs::read(crate::font_catalog::checkout_root().join("NotoSansCJK-VF.ttc")).unwrap();
        let owner: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
        assert_eq!(
            renderer
                .font_system
                .db_mut()
                .load_font_source(Source::Binary(owner.clone()))
                .len(),
            5
        );
        renderer.bump_generation();
        let style = named_style("Noto Sans CJK SC");
        let scene = draw(&mut renderer, "漢", &style);
        let font = &one_run(&scene).font;
        assert_eq!(font.index, 2);
        let (emitted_owner, _) = font.data.clone().into_raw_parts();
        assert!(Arc::ptr_eq(&owner, &emitted_owner));
    }
    // phase: different_shaped_weights_reuse_bytes_and_system_replacement_refreshes_handles
    {
        let mut renderer = renderer();
        let style = named_style("Noto Sans");
        let regular = draw(&mut renderer, "A", &style);
        let bold = draw(
            &mut renderer,
            "A",
            &style.clone().with_weight(TextWeight::Bold),
        );
        let (regular_owner, _) = one_run(&regular).font.data.clone().into_raw_parts();
        let (bold_owner, _) = one_run(&bold).font.data.clone().into_raw_parts();
        assert!(Arc::ptr_eq(&regular_owner, &bold_owner));
        renderer.font_system = configured_font_system_with_catalog(&catalog()).unwrap();
        renderer.bump_generation();
        let replacement = draw(&mut renderer, "A", &style);
        let new_font = &one_run(&replacement).font;
        assert_source_owner(&renderer, face_id(&renderer, "Noto Sans"), new_font);
        let (new_owner, _) = new_font.data.clone().into_raw_parts();
        assert!(!Arc::ptr_eq(&regular_owner, &new_owner));
        assert_ne!(one_run(&regular).font.data.id(), new_font.data.id());
        // The old scene remains readable while its own handle keeps bytes alive.
        assert_eq!(one_run(&regular).font.data.len(), new_font.data.len());
    }
}

fn emitted_coords(scene: &Scene, run: &vello_encoding::GlyphRun) -> Vec<i16> {
    scene.encoding().resources.normalized_coords[run.normalized_coords.clone()].to_vec()
}

fn expected_coords(font: &Font, weight: u16, optical: Option<f32>) -> Vec<i16> {
    let font = FontRef::from_index(font.data.as_ref(), font.index).unwrap();
    let mut settings = vec![(Tag::new(b"wght"), f32::from(weight))];
    if let Some(size) = optical {
        settings.push((Tag::new(b"opsz"), size));
    }
    font.axes()
        .location(settings)
        .coords()
        .iter()
        .map(|coord| coord.to_bits())
        .collect()
}

fn assert_plain_weight(renderer: &mut TextRenderer, style: &TextStyle, expected: u16) {
    let buffer = renderer.shape_buffer("Hamburgefon", None, style);
    let shaped: Vec<_> = buffer
        .buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .map(|glyph| (glyph.font_id, glyph.font_weight))
        .collect();
    assert!(!shaped.is_empty());
    assert!(shaped.iter().all(|(_, weight)| *weight == Weight(expected)));
    for _ in 0..2 {
        // Includes the cached draw path.
        let scene = draw(renderer, "Hamburgefon", style);
        let run = one_run(&scene);
        assert_eq!(
            run.font.index,
            renderer.font_system.db().face(shaped[0].0).unwrap().index
        );
        assert_source_owner(renderer, shaped[0].0, &run.font);
        assert_eq!(
            emitted_coords(&scene, run),
            expected_coords(&run.font, expected, None)
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn runs_paint_with_their_shaped_weight_and_axis_order() {
    // phase: plain_paint_uses_named_and_rounded_explicit_shaped_weights
    {
        let mut renderer = renderer();
        // Adwaita has a 100..900 weight range. Noto Sans also covers its narrower
        // 400..700 range, so requests below/above a face's range are exercised.
        // CJK SC also exercises weight coordinates on collection index 2.
        for family in ["Adwaita Sans", "Noto Sans", "Noto Sans CJK SC"] {
            let base = named_style(family);
            for (weight, expected) in [
                (TextWeight::Regular, 400),
                (TextWeight::Medium, 500),
                (TextWeight::Semibold, 600),
                (TextWeight::Bold, 700),
            ] {
                assert_plain_weight(&mut renderer, &base.clone().with_weight(weight), expected);
            }
            for (value, expected) in [
                (675.4, 675),
                (675.5, 676),
                (400.49, 400),
                (-20.0, 1),
                (1200.0, 1000),
            ] {
                let style = base
                    .clone()
                    .with_weight(TextWeight::Bold)
                    .with_font_variation(OpenTypeTag::WEIGHT, value);
                assert_plain_weight(&mut renderer, &style, expected);
            }
        }
    }
    // phase: weight_fix_preserves_optical_axis_order_in_actual_runs
    {
        let mut renderer = renderer();
        let style = named_style("Adwaita Sans")
            .with_weight(TextWeight::Bold)
            .with_optical_sizing(OpticalSizing::Value(32.0));
        let scene = draw(&mut renderer, "A", &style);
        let run = one_run(&scene);
        assert_eq!(
            emitted_coords(&scene, run),
            expected_coords(&run.font, 700, Some(32.0))
        );
        let font = FontRef::from_index(run.font.data.as_ref(), run.font.index).unwrap();
        assert_eq!(
            font.axes()
                .iter()
                .map(|axis| axis.tag())
                .collect::<Vec<_>>(),
            [Tag::new(b"opsz"), Tag::new(b"wght")]
        );
        assert_eq!(emitted_coords(&scene, run)[0], 16384);
    }
    // phase: rich_same_face_runs_split_at_shaped_weight_and_retain_span_bold
    {
        let mut renderer = renderer();
        let style = named_style("Noto Sans").with_font_variation(OpenTypeTag::WEIGHT, 675.4);
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[
                RichSpan::new("A"),
                RichSpan::new("B").bold(true),
                RichSpan::new("C"),
            ],
            &style,
            None,
            TextAlign::Left,
            vec![],
            vec![],
        );
        let glyphs: Vec<_> = layout
            .buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter().cloned())
            .collect();
        assert_eq!(glyphs.len(), 3);
        assert_eq!(
            glyphs.iter().map(|g| g.font_weight.0).collect::<Vec<_>>(),
            [675, 700, 675]
        );
        assert!(glyphs.iter().all(|g| g.font_id == glyphs[0].font_id));
        let mut scene = Scene::new();
        renderer.draw_rich(&mut scene, &layout, Affine::IDENTITY, Color::WHITE);
        let resources = &scene.encoding().resources;
        assert_eq!(resources.glyph_runs.len(), 3);
        for (run, glyph) in resources.glyph_runs.iter().zip(&glyphs) {
            assert_eq!(run.glyphs.len(), 1);
            assert_eq!(
                resources.glyphs[run.glyphs.start].id,
                u32::from(glyph.glyph_id)
            );
            assert_eq!(
                emitted_coords(&scene, run),
                expected_coords(&run.font, glyph.font_weight.0, None)
            );
            assert_source_owner(&renderer, glyph.font_id, &run.font);
        }
    }
    // phase: rich_equal_weight_adjacent_spans_stay_batched
    {
        let mut renderer = renderer();
        let style = named_style("Noto Sans").with_weight(TextWeight::Bold);
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[
                RichSpan::new("A"),
                RichSpan::new("B").bold(true),
                RichSpan::new("C"),
            ],
            &style,
            None,
            TextAlign::Left,
            vec![],
            vec![],
        );
        let mut scene = Scene::new();
        renderer.draw_rich(&mut scene, &layout, Affine::IDENTITY, Color::WHITE);
        let run = one_run(&scene);
        assert_eq!(run.glyphs.len(), 3);
        assert_eq!(
            emitted_coords(&scene, run),
            expected_coords(&run.font, 700, None)
        );
    }
    // phase: rich_visible_discretionary_hyphen_uses_its_bold_marker_weight
    {
        let mut renderer = renderer();
        let style = named_style("Noto Sans").with_font_variation(OpenTypeTag::WEIGHT, 675.4);
        let bold = named_style("Noto Sans").with_weight(TextWeight::Bold);
        let width = renderer.measure_style("ab-", None, &bold).0 + 0.5;
        let spans = [RichSpan::new("ab\u{ad}cd").bold(true)];
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &spans,
            &style,
            Some(width),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let marker = layout
            .buffer
            .layout_runs()
            .find_map(|run| {
                run.glyphs
                    .last()
                    .filter(|g| run.text.get(g.start..g.end) == Some("\u{ad}"))
                    .map(|g| (g.clone(), run.line_y))
            })
            .expect("the controlled narrow layout exercises the discretionary break");
        assert_eq!(marker.0.font_weight, Weight(700));
        // Resolve the expected visible glyph from the selected face's charmap,
        // independent of the renderer's visible_discretionary_hyphen helper.
        let font = renderer
            .font_system
            .get_font(marker.0.font_id, marker.0.font_weight)
            .unwrap();
        let charmap = font.as_swash().charmap();
        let unicode_hyphen = charmap.map('\u{2010}');
        let expected_id = if unicode_hyphen == 0 {
            charmap.map('-')
        } else {
            unicode_hyphen
        };
        assert_ne!(expected_id, 0);
        let mut scene = Scene::new();
        renderer.draw_rich(&mut scene, &layout, Affine::IDENTITY, Color::WHITE);
        let resources = &scene.encoding().resources;
        let hyphen_runs: Vec<_> = resources
            .glyph_runs
            .iter()
            .filter(|run| {
                let glyphs = &resources.glyphs[run.glyphs.clone()];
                glyphs.len() == 1
                    && glyphs[0].id == u32::from(expected_id)
                    && (glyphs[0].x - marker.0.x).abs() < 0.001
                    && (glyphs[0].y - (marker.1 + marker.0.y)).abs() < 0.001
            })
            .collect();
        assert_eq!(hyphen_runs.len(), 1);
        let run = hyphen_runs[0];
        assert_eq!(
            emitted_coords(&scene, run),
            expected_coords(&run.font, 700, None)
        );
        assert_source_owner(&renderer, marker.0.font_id, &run.font);
        let wide = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &spans,
            &style,
            Some(10_000.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let mut wide_scene = Scene::new();
        renderer.draw_rich(&mut wide_scene, &wide, Affine::IDENTITY, Color::WHITE);
        assert_eq!(wide_scene.encoding().resources.glyphs.len(), 4);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn clipped_rich_text_preserves_pixels_and_culls_only_invisible_glyphs() {
    // phase: clipped_rich_text_reduces_encoded_glyphs_and_preserves_exact_pixels
    {
        let mut renderer = renderer();
        let text = "Italic e\u{301} Αλφα fallback. ".repeat(120);
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[RichSpan::new(&text)],
            &named_style("Noto Sans"),
            Some(180.0),
            TextAlign::Left,
            vec![(0, 24)],
            vec![(30, 50)],
        );
        let clip = Rect::new(10.25, 12.75, 198.5, 135.25);
        let mut reference = Scene::new();
        reference.push_clip_layer(vello::peniko::Fill::NonZero, Affine::IDENTITY, &clip);
        renderer.draw_rich(
            &mut reference,
            &layout,
            Affine::translate((12.0, -65.5)),
            Color::WHITE,
        );
        reference.pop_layer();
        let mut candidate = Scene::new();
        candidate.push_clip_layer(vello::peniko::Fill::NonZero, Affine::IDENTITY, &clip);
        renderer.draw_rich_in_clip(
            &mut candidate,
            &layout,
            Affine::translate((12.0, -65.5)),
            Color::WHITE,
            clip,
        );
        candidate.pop_layer();
        assert!(
            candidate.encoding().resources.glyphs.len() * 4
                < reference.encoding().resources.glyphs.len()
        );
        // Exact byte equality requires deterministic path preprocessing. The
        // production GPU path can vary a few antialiasing bytes as atomic segment
        // allocation changes summation order; the matrix test below measures that
        // mode separately against the reference's own observed range.
        let mut gpu =
            crate::offscreen::OffscreenRenderer::try_new_for_pixel_test(220, 160, true).unwrap();
        let expected = gpu.render(&reference, Color::BLACK);
        let actual = gpu.render(&candidate, Color::BLACK);
        assert_eq!(
            actual, expected,
            "clipping optimization must preserve exact RGBA"
        );
    }
    // phase: clipped_rich_text_preserves_transformed_styled_ink_and_invalid_clip_fallback
    {
        clipped_styled_pixel_matrix(true);
    }
    // phase: clipped_rich_text_gpu_pixels_stay_within_summation_noise
    {
        clipped_styled_pixel_matrix(false);
    }
    // phase: clipped_rich_text_preserves_exercised_discretionary_hyphen
    {
        let mut renderer = renderer();
        let style = named_style("Noto Sans").with_weight(TextWeight::Bold);
        let width = renderer.measure_style("ab-", None, &style).0 + 0.5;
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[RichSpan::new("ab\u{ad}cd").bold(true)],
            &style,
            Some(width),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert!(layout.buffer.layout_runs().any(|run| {
            run.glyphs
                .last()
                .is_some_and(|glyph| run.text.get(glyph.start..glyph.end) == Some("\u{ad}"))
        }));
        let mut gpu = crate::offscreen::OffscreenRenderer::new(100, 100);
        for clip in [
            Rect::new(0.25, 1.75, 70.5, 30.125),
            Rect::new(25.25, 5.5, 40.75, 28.5),
        ] {
            let mut reference = Scene::new();
            reference.push_clip_layer(vello::peniko::Fill::NonZero, Affine::IDENTITY, &clip);
            renderer.draw_rich(&mut reference, &layout, Affine::IDENTITY, Color::WHITE);
            reference.pop_layer();
            let mut candidate = Scene::new();
            candidate.push_clip_layer(vello::peniko::Fill::NonZero, Affine::IDENTITY, &clip);
            renderer.draw_rich_in_clip(
                &mut candidate,
                &layout,
                Affine::IDENTITY,
                Color::WHITE,
                clip,
            );
            candidate.pop_layer();
            assert!(gpu.render(&reference, Color::BLACK) == gpu.render(&candidate, Color::BLACK));
        }
    }
    // phase: clipped_rich_text_does_not_cull_color_or_invalid_font_outlines
    {
        let mut renderer = renderer();
        for family in ["Noto Color Emoji", "Noto Sans"] {
            let layout = RichTextLayout::shape_styled(
                &mut renderer.font_system,
                &[RichSpan::new(if family == "Noto Sans" {
                    "A"
                } else {
                    "😀"
                })],
                &named_style(family),
                None,
                TextAlign::Left,
                vec![],
                vec![],
            );
            let run = layout.buffer.layout_runs().next().unwrap();
            let first = &run.glyphs[0];
            let mut font = renderer
                .vello_font(first.font_id, first.font_weight)
                .unwrap();
            if family == "Noto Color Emoji" {
                assert!(
                    FontRef::from_index(font.data.as_ref(), font.index)
                        .unwrap()
                        .table_data(Tag::new(b"COLR"))
                        .is_some()
                );
            } else {
                font.index = u32::MAX;
            }
            assert!(
                !renderer.glyph_batch_outside(
                    &font,
                    &GlyphBatch {
                        face_id: first.font_id,
                        glyphs: run.glyphs,
                        size: first.font_size,
                        color: Color::WHITE,
                        line_y: run.line_y,
                        variations: &[],
                    },
                    &[],
                    Affine::translate((10_000.0, 10_000.0)),
                    Rect::new(0.0, 0.0, 10.0, 10.0),
                ),
                "uncertain or non-outline ink must keep drawing"
            );
        }
    }
}

/// Largest per-channel difference the production GPU path may show between
/// two renders of equivalent scenes. Its atomic segment allocation changes
/// the order in which antialiasing coverage is summed, which moves a byte by
/// at most one step; a real change in ink moves many bytes by far more.
const GPU_SUMMATION_MAX_CHANNEL_DELTA: u8 = 1;
/// How many RGBA bytes of a 220x160 frame may show that one-step difference.
/// Across 64 repeated renders of each reference scene below, the bytes that
/// varied at all stayed within this count; a dropped or shifted glyph
/// changes hundreds, by far more than one step.
const GPU_SUMMATION_MAX_DIFFERING_BYTES: usize = 64;

fn assert_within_gpu_summation_noise(actual: &[u8], expected: &[u8], case: &str) {
    assert_eq!(actual.len(), expected.len(), "frame size for {case}");
    let differing: Vec<_> = actual
        .iter()
        .zip(expected)
        .enumerate()
        .filter(|(_, (a, e))| a != e)
        .map(|(i, (a, e))| (i, *a, *e))
        .collect();
    let beyond: Vec<_> = differing
        .iter()
        .filter(|(_, a, e)| a.abs_diff(*e) > GPU_SUMMATION_MAX_CHANNEL_DELTA)
        .collect();
    assert!(
        beyond.is_empty(),
        "clipped pixels differ by more than summation noise for {case}: {beyond:?}"
    );
    assert!(
        differing.len() <= GPU_SUMMATION_MAX_DIFFERING_BYTES,
        "{} bytes differ by one step for {case}, more than summation noise explains",
        differing.len()
    );
}

fn clipped_styled_pixel_matrix(stable_preprocessing: bool) {
    use crate::richtext::BaselineShift;
    let mut renderer = renderer();
    let mut gpu =
        crate::offscreen::OffscreenRenderer::try_new_for_pixel_test(220, 160, stable_preprocessing)
            .unwrap();
    let spans = vec![
        RichSpan::new("“Italic e\u{301} العربية 日本語—")
            .italic(true)
            .background(Some([40, 70, 90])),
        RichSpan::new("SUPER").baseline(BaselineShift::Super),
        RichSpan::new("sub").baseline(BaselineShift::Sub).bold(true),
        RichSpan::new(" discretionary\u{ad}hyphen punctuation.” ".repeat(35)),
    ];
    for style in [
        named_style("Noto Sans").with_hanging_punctuation(HangingPunctuation::BOTH),
        named_style("Adwaita Sans")
            .with_font_variation(OpenTypeTag::WEIGHT, 675.4)
            .with_optical_sizing(OpticalSizing::Value(32.0)),
    ] {
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &spans,
            &style,
            Some(130.0),
            TextAlign::Left,
            vec![(0, 80)],
            vec![(10, 40)],
        );
        for transform in [
            Affine::translate((30.25, -40.75)),
            Affine::translate((35.5, -80.125)) * Affine::rotate(0.17) * Affine::scale(0.87),
            Affine::new([-0.9, 0.08, 0.14, 1.1, 185.0, -130.5]),
        ] {
            for clip in [
                Rect::new(10.25, 12.75, 198.5, 135.25),
                Rect::new(70.125, 45.875, 100.375, 90.125),
            ] {
                let mut reference = Scene::new();
                reference.push_clip_layer(vello::peniko::Fill::NonZero, Affine::IDENTITY, &clip);
                renderer.draw_rich(&mut reference, &layout, transform, Color::WHITE);
                reference.pop_layer();
                let mut candidate = Scene::new();
                candidate.push_clip_layer(vello::peniko::Fill::NonZero, Affine::IDENTITY, &clip);
                renderer.draw_rich_in_clip(&mut candidate, &layout, transform, Color::WHITE, clip);
                candidate.pop_layer();
                let expected = gpu.render(&reference, Color::BLACK);
                if stable_preprocessing {
                    for _ in 0..4 {
                        assert_eq!(
                            gpu.render(&reference, Color::BLACK),
                            expected,
                            "reference stability"
                        );
                        assert_eq!(
                            gpu.render(&candidate, Color::BLACK),
                            expected,
                            "exact clipped pixels for {transform:?}, {clip:?}"
                        );
                    }
                } else {
                    let actual = gpu.render(&candidate, Color::BLACK);
                    assert_within_gpu_summation_noise(
                        &actual,
                        &expected,
                        &format!("{transform:?}, {clip:?}"),
                    );
                }
            }
        }
        let mut reference = Scene::new();
        renderer.draw_rich(&mut reference, &layout, Affine::IDENTITY, Color::WHITE);
        let mut invalid = Scene::new();
        renderer.draw_rich_in_clip(
            &mut invalid,
            &layout,
            Affine::IDENTITY,
            Color::WHITE,
            Rect::new(f64::NAN, 0.0, 10.0, 10.0),
        );
        assert_eq!(
            reference.encoding().resources.glyphs.len(),
            invalid.encoding().resources.glyphs.len()
        );
    }
    assert!(!renderer.ink_bounds.is_empty());
    assert!(renderer.ink_bounds_entries <= SHAPE_CACHE_LIMIT);
    assert_eq!(
        renderer.ink_bounds_entries,
        renderer
            .ink_bounds
            .values()
            .map(HashMap::len)
            .sum::<usize>()
    );
    renderer.bump_generation();
    assert!(renderer.ink_bounds.is_empty());
    assert_eq!(renderer.ink_bounds_entries, 0);
}

#[test]
fn the_clipped_ink_cache_groups_by_font_instance_and_stays_bounded() {
    // phase: clipped_ink_cache_groups_glyphs_by_font_instance
    {
        let mut renderer = renderer();
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[RichSpan::new("abcdefghijklmno")],
            &named_style("Noto Sans"),
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        // Horizontally outside but vertically overlapping the line, so the line
        // is not culled whole (`rich_run_outside_clip`) and reaches the glyph
        // batch cull, which looks up ink once per font instance.
        renderer.draw_rich_in_clip(
            &mut Scene::new(),
            &layout,
            Affine::IDENTITY,
            Color::WHITE,
            Rect::new(1000.0, 0.0, 1100.0, 100.0),
        );
        assert_eq!(
            renderer.ink_bounds.len(),
            1,
            "one font-instance lookup per batch"
        );
    }
    // phase: clipped_ink_cache_remains_bounded_across_font_instances_and_reuses_after_eviction
    {
        let mut renderer = renderer();
        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[RichSpan::new("a")],
            &named_style("Noto Sans"),
            Some(400.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let glyph = layout.buffer.layout_runs().next().unwrap().glyphs[0].clone();
        let font = renderer
            .vello_font(glyph.font_id, glyph.font_weight)
            .unwrap();
        let mut evicted = false;
        for index in 0..SHAPE_CACHE_LIMIT + 8 {
            let before = renderer.ink_bounds_entries;
            let size = 10.0 + index as f32 / 128.0;
            let outside = renderer.glyph_batch_outside(
                &font,
                &GlyphBatch {
                    face_id: glyph.font_id,
                    glyphs: std::slice::from_ref(&glyph),
                    size,
                    color: Color::WHITE,
                    line_y: 0.0,
                    variations: &[],
                },
                &[],
                Affine::IDENTITY,
                Rect::new(0.0, 1000.0, 400.0, 1100.0),
            );
            assert!(outside, "eviction must not change conservative clipping");
            let count = renderer.ink_bounds_entries;
            evicted |= count < before;
            assert!(count <= SHAPE_CACHE_LIMIT);
            assert!(renderer.ink_bounds.len() <= count);
            assert!(renderer.glyph_batch_outside(
                &font,
                &GlyphBatch {
                    face_id: glyph.font_id,
                    glyphs: std::slice::from_ref(&glyph),
                    size,
                    color: Color::WHITE,
                    line_y: 0.0,
                    variations: &[],
                },
                &[],
                Affine::IDENTITY,
                Rect::new(0.0, 1000.0, 400.0, 1100.0),
            ));
            // A full cache may be cleared before the next batch; otherwise the
            // repeated glyph must be a hit, including immediately after eviction.
            if count < SHAPE_CACHE_LIMIT {
                assert_eq!(renderer.ink_bounds_entries, count);
            } else {
                evicted |= renderer.ink_bounds_entries < count;
            }
        }
        assert!(evicted, "the workload must exercise the capacity boundary");
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn painted_glyphs_match_the_reference_output_and_shaped_offsets() {
    // phase: uniform_rich_runs_borrow_glyphs_with_exact_reference_output
    {
        use crate::richtext::BaselineShift;
        let mut renderer = renderer();
        let mut gpu =
            crate::offscreen::OffscreenRenderer::try_new_for_pixel_test(220, 160, true).unwrap();
        let mut borrowed = 0;
        let mut copied = 0;
        for spans in [
            vec![RichSpan::new("ordinary wrapped text ".repeat(8))],
            vec![RichSpan::new("uniform bold").bold(true)],
            vec![RichSpan::new("raised text").baseline(BaselineShift::Super)],
            vec![RichSpan::new("normal "), RichSpan::new("bold").bold(true)],
            vec![RichSpan::new("soft\u{ad}hyphen")],
        ] {
            let layout = RichTextLayout::shape_styled(
                &mut renderer.font_system,
                &spans,
                &named_style("Noto Sans"),
                Some(180.0),
                TextAlign::Left,
                vec![],
                vec![],
            );
            for clip in [None, Some(Rect::new(10.0, 10.0, 190.0, 100.0))] {
                let transform = Affine::translate((12.25, -4.75));
                let mut reference = Scene::new();
                let mut candidate = Scene::new();
                for run in layout.buffer.layout_runs() {
                    renderer.draw_rich_run_copied(
                        &mut reference,
                        &run,
                        &layout,
                        transform,
                        Color::WHITE,
                        clip,
                    );
                    if renderer.draw_uniform_rich_run(
                        &mut candidate,
                        &run,
                        &layout,
                        transform,
                        Color::WHITE,
                        clip,
                    ) {
                        borrowed += 1;
                    } else {
                        copied += 1;
                        renderer.draw_rich_run_copied(
                            &mut candidate,
                            &run,
                            &layout,
                            transform,
                            Color::WHITE,
                            clip,
                        );
                    }
                }
                let glyphs = |scene: &Scene| {
                    scene
                        .encoding()
                        .resources
                        .glyphs
                        .iter()
                        .map(|g| (g.id, g.x.to_bits(), g.y.to_bits()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(glyphs(&candidate), glyphs(&reference));
                assert_eq!(
                    gpu.render(&candidate, Color::BLACK),
                    gpu.render(&reference, Color::BLACK)
                );
            }
        }
        assert!(borrowed > 0);
        assert!(copied > 0, "fixture must cover conservative fallback");
    }
    // phase: painted_glyphs_keep_shaped_mark_offsets_for_bengali_conjuncts
    // Bengali conjuncts are assembled from several glyphs that the shaper
    // positions relative to each other with GPOS offsets (the below-base forms
    // and vowel signs of নমস্কার and বিশ্ব). Painting only the pen positions
    // leaves those pieces where the base glyph's advance put them and the
    // conjuncts read as broken, so every paint path must place glyphs where the
    // shaper put them.
    {
        let mut renderer = renderer();
        let style = named_style("Noto Sans Bengali");
        let text = "নমস্কার বিশ্ব";

        let buffer = renderer.shape_buffer(text, None, &style);
        assert!(
            buffer
                .buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .any(|g| g.x_offset != 0.0 || g.y_offset != 0.0),
            "fixture must exercise GPOS mark offsets"
        );
        let shaped = shaped_origins(&buffer.buffer);
        let scene = draw(&mut renderer, text, &style);
        assert_same_origins(&painted_origins(&scene), &shaped, "plain text");

        let layout = RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[RichSpan::new(text)],
            &style,
            None,
            TextAlign::Left,
            vec![],
            vec![],
        );
        let shaped = shaped_origins(&layout.buffer);
        for clip in [None, Some(Rect::new(-100.0, -100.0, 1000.0, 1000.0))] {
            let mut uniform = Scene::new();
            let mut copied = Scene::new();
            for run in layout.buffer.layout_runs() {
                assert!(renderer.draw_uniform_rich_run(
                    &mut uniform,
                    &run,
                    &layout,
                    Affine::IDENTITY,
                    Color::WHITE,
                    clip,
                ));
                renderer.draw_rich_run_copied(
                    &mut copied,
                    &run,
                    &layout,
                    Affine::IDENTITY,
                    Color::WHITE,
                    clip,
                );
            }
            assert_same_origins(&painted_origins(&uniform), &shaped, "uniform rich run");
            assert_same_origins(&painted_origins(&copied), &shaped, "copied rich run");
        }
    }
}

/// Where the shaper placed each glyph: its pen position plus the GPOS
/// offset, which cosmic-text keeps in em units apart from the pen position.
fn shaped_origins(buffer: &cosmic_text::Buffer) -> Vec<(u32, f32, f32)> {
    buffer
        .layout_runs()
        .flat_map(|run| {
            run.glyphs.iter().map(move |g| {
                let baseline = run.line_y + g.y;
                (
                    u32::from(g.glyph_id),
                    g.x + g.font_size * g.x_offset,
                    baseline - g.font_size * g.y_offset,
                )
            })
        })
        .collect()
}

fn painted_origins(scene: &Scene) -> Vec<(u32, f32, f32)> {
    scene
        .encoding()
        .resources
        .glyphs
        .iter()
        .map(|g| (g.id, g.x, g.y))
        .collect()
}

fn assert_same_origins(painted: &[(u32, f32, f32)], shaped: &[(u32, f32, f32)], path: &str) {
    assert_eq!(painted.len(), shaped.len(), "{path}: glyph count");
    for (painted, shaped) in painted.iter().zip(shaped) {
        assert_eq!(painted.0, shaped.0, "{path}: glyph ids");
        assert!(
            (painted.1 - shaped.1).abs() < 1e-3 && (painted.2 - shaped.2).abs() < 1e-3,
            "{path}: glyph {} painted at ({}, {}) but shaped at ({}, {})",
            painted.0,
            painted.1,
            painted.2,
            shaped.1,
            shaped.2
        );
    }
}

#[test]
fn matched_fallbacks_paint_at_the_size_they_were_shaped_at() {
    let mut renderer = renderer();
    let style = named_style(UI_SANS_FAMILY);
    let runs = renderer.shaped_runs("Report Отчёт Αναφορά", None, &style);
    let sizes: Vec<f32> = runs.iter().map(|run| run.size).collect();
    assert!(sizes.contains(&style.size), "{sizes:?}");
    assert_eq!(
        sizes.iter().filter(|size| **size != style.size).count(),
        1,
        "Cyrillic is x-height matched, Greek is the UI face's own: {sizes:?}"
    );
    // Measurement and paint come from the same shaped buffer.
    let (width, _) = renderer.measure_style("Report Отчёт Αναφορά", None, &style);
    let last = runs.last().and_then(|run| run.glyphs.last()).unwrap();
    assert!(last.x < width);
}

#[test]
fn condensed_text_is_shaped_at_the_width_it_is_drawn() {
    let mut renderer = renderer();
    let text = "paypa1.com/l0gin SN 0O1I-l0O8";
    let normal = named_style("Tilo Sans");
    let condensed = named_style("Tilo Sans").with_font_variation(OpenTypeTag::WIDTH, 75.0);
    let normal_width = renderer.measure_style(text, None, &normal).0;
    let condensed_width = renderer.measure_style(text, None, &condensed).0;
    // Tilo Sans' condensed `n` is 442 units against 599. Shaped at the
    // default width, condensed text kept the normal advances and drew its
    // narrow glyphs spaced out.
    assert!(
        condensed_width < normal_width * 0.85,
        "condensed {condensed_width} against normal {normal_width}"
    );
    for (width, stretch) in [
        (75.0, cosmic_text::Stretch::Condensed),
        (80.0, cosmic_text::Stretch::Condensed),
        (90.0, cosmic_text::Stretch::SemiCondensed),
        (100.0, cosmic_text::Stretch::Normal),
    ] {
        assert_eq!(stretch_for_width(width), stretch, "{width}");
    }
}

#[test]
fn the_generic_sans_follows_the_theme_ui_face() {
    let mut renderer = renderer();
    let generic = TextStyle::new(20.0);
    let default_face = renderer.measure_style("Hamburgefonstiv", None, &generic).0;
    renderer.set_ui_face(UI_HYPERLEGIBLE_FAMILY);
    let hyperlegible = renderer.measure_style("Hamburgefonstiv", None, &generic).0;
    assert!(
        (default_face - hyperlegible).abs() > 1.0,
        "{default_face} vs {hyperlegible}"
    );
    renderer.set_ui_face("No Such Family");
    let unchanged = renderer.measure_style("Hamburgefonstiv", None, &generic).0;
    assert!((unchanged - hyperlegible).abs() < f32::EPSILON);
}

/// A face with the distinct I and l and slashed zero, so the setting and
/// security surfaces show in the glyphs.
fn features_renderer() -> TextRenderer {
    let mut renderer = renderer();
    renderer.font_system.db_mut().load_font_data(
        include_bytes!(concat!(
            env!("OUT_DIR"),
            "/TiloSansFeaturesFixture-Regular.ttf"
        ))
        .to_vec(),
    );
    renderer
}

fn shaped_ids(renderer: &mut TextRenderer, text: &str, style: &TextStyle) -> Vec<u16> {
    let style = renderer.scaled_style(style);
    let prepared = renderer.shape_buffer(text, None, &style);
    prepared
        .buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
        .collect()
}

fn rich_ids(renderer: &mut TextRenderer, text: &str, style: &TextStyle) -> Vec<u16> {
    // Widgets ask for the layout epoch before shaping on their own.
    let _ = renderer.layout_epoch();
    RichTextLayout::shape_plain(&mut renderer.font_system, text, style, None)
        .buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.glyph_id))
        .collect()
}

#[test]
fn the_distinct_letters_setting_reaches_every_shaping_path() {
    let mut renderer = features_renderer();
    let style = named_style("Tilo Sans Features Fixture");
    let plain = shaped_ids(&mut renderer, "Il0", &style);
    let distinct = shaped_ids(
        &mut renderer,
        "Il0",
        &style.clone().with_kind(TextKind::Url),
    );
    assert_ne!(plain, distinct);
    assert_eq!(rich_ids(&mut renderer, "Il0", &style), plain);

    let epoch = Arc::clone(renderer.layout_epoch());
    renderer.set_distinct_letters(true);
    assert!(renderer.distinguishes_letters());
    assert!(
        !Arc::ptr_eq(&epoch, renderer.layout_epoch()),
        "widget layouts reshape when the setting changes"
    );
    assert_eq!(shaped_ids(&mut renderer, "Il0", &style), distinct);
    assert_eq!(rich_ids(&mut renderer, "Il0", &style), distinct);

    renderer.set_distinct_letters(false);
    assert!(!renderer.distinguishes_letters());
    assert_eq!(shaped_ids(&mut renderer, "Il0", &style), plain);

    // Another renderer on the thread keeps its own setting.
    let mut other = features_renderer();
    other.set_distinct_letters(true);
    assert_eq!(rich_ids(&mut renderer, "Il0", &style), plain);
    assert_eq!(rich_ids(&mut other, "Il0", &style), distinct);
}

#[test]
fn security_surfaces_distinguish_whatever_the_setting() {
    let mut renderer = features_renderer();
    let style = named_style("Tilo Sans Features Fixture");
    let plain = shaped_ids(&mut renderer, "Il0", &style);
    let distinct = shaped_ids(
        &mut renderer,
        "Il0",
        &style.clone().with_kind(TextKind::Url),
    );
    let inside = renderer.security_surface(|renderer| {
        assert!(renderer.distinguishes_letters());
        (
            shaped_ids(renderer, "Il0", &style),
            rich_ids(renderer, "Il0", &style),
        )
    });
    assert_eq!(inside, (distinct.clone(), distinct.clone()));
    assert!(!renderer.distinguishes_letters());
    assert_eq!(shaped_ids(&mut renderer, "Il0", &style), plain);
    // A custom-painted security surface marks its style instead.
    assert_eq!(
        shaped_ids(&mut renderer, "Il0", &style.clone().on_security_surface()),
        distinct
    );
}

#[test]
fn identifiers_in_a_label_are_set_apart_with_no_kind_declared() {
    let mut renderer = features_renderer();
    let style = named_style("Tilo Sans Features Fixture");
    let prose = shaped_ids(&mut renderer, "Il0 ", &style);
    let line = shaped_ids(&mut renderer, "Il0 https://Il0.example", &style);
    let url = shaped_ids(
        &mut renderer,
        "Il0",
        &style.clone().with_kind(TextKind::Url),
    );
    assert_eq!(line[..4], prose[..]);
    // "https://" is eight glyphs; the host that follows starts with Il0.
    assert_eq!(line[12..15], url[..]);
    assert_ne!(line[12..15], prose[..3]);
}
