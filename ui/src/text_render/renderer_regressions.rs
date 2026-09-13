use super::*;
use crate::font_catalog::{FontCatalog, configured_font_system_with_catalog};
use cosmic_text::fontdb::{ID, Source};
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
