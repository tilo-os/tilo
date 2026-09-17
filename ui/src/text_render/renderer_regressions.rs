#![cfg(test)]

use super::*;
use crate::font_catalog::{FontCatalog, configured_font_system_with_catalog};
use crate::richtext::{RichSpan, RichTextLayout, TextAlign};
use cosmic_text::fontdb::{ID, Source};
use skrifa::{FontRef, MetadataProvider, Tag};
use std::path::PathBuf;

fn catalog() -> FontCatalog {
    FontCatalog::controlled_fixture(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
        "en-US",
    )
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
fn collection_indices_and_shared_owners_reach_the_real_scene() {
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

#[test]
fn binary_font_source_is_shared_with_emitted_runs() {
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
    let bytes = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/NotoSansCJK-VF.ttc"),
    )
    .unwrap();
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

#[test]
fn different_shaped_weights_reuse_bytes_and_system_replacement_refreshes_handles() {
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
fn plain_paint_uses_named_and_rounded_explicit_shaped_weights() {
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

#[test]
fn weight_fix_preserves_optical_axis_order_in_actual_runs() {
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

#[test]
fn rich_same_face_runs_split_at_shaped_weight_and_retain_span_bold() {
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

#[test]
fn rich_equal_weight_adjacent_spans_stay_batched() {
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

#[test]
fn rich_visible_discretionary_hyphen_uses_its_bold_marker_weight() {
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
