//! The reading faces through the real shaping path: each run lands in the
//! optical cut its size calls for, and the Reading role's figures, small
//! caps and ligatures reach the face the text is set in.

use super::*;
use crate::tokens::{ReadingFace, TextRole, Typography};

fn reading(face: ReadingFace) -> TextStyle {
    Typography::default()
        .with_reading_face(face.name())
        .style(TextRole::Reading)
}

/// `(family, glyph id)` of every glyph `text` shapes to in `style`.
fn shaped(renderer: &mut TextRenderer, text: &str, style: &TextStyle) -> Vec<(String, u16)> {
    let buffer = renderer.shape_buffer(text, None, style).buffer;
    let db = renderer.font_system.db();
    buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .map(|glyph| {
            let family = db
                .face(glyph.font_id)
                .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
                .unwrap_or_default();
            (family, glyph.glyph_id)
        })
        .collect()
}

fn families(glyphs: &[(String, u16)]) -> Vec<&str> {
    let mut out: Vec<&str> = glyphs.iter().map(|(family, _)| family.as_str()).collect();
    out.dedup();
    out
}

#[test]
fn new_computer_modern_is_set_in_the_cut_each_size_calls_for() {
    let mut renderer = crate::font_catalog::fixture_renderer();
    let style = reading(ReadingFace::Default);
    for (size, ground, cut) in [
        (10.0, TextGround::Light, "New Computer Modern 08"),
        (16.0, TextGround::Light, "New Computer Modern Book"),
        (19.5, TextGround::Dark, "New Computer Modern Book"),
        (26.0, TextGround::Light, "New Computer Modern"),
        (26.0, TextGround::Dark, "New Computer Modern Book"),
        (48.0, TextGround::Dark, "New Computer Modern"),
    ] {
        let glyphs = shaped(
            &mut renderer,
            "Chapter",
            &style.clone().with_content_size(size).with_ground(ground),
        );
        assert_eq!(families(&glyphs), [cut], "{size} px on {ground:?}");
    }
    // Small italics take Book's italic: the 8 pt design ships upright only.
    let glyphs = shaped(
        &mut renderer,
        "Park",
        &style.clone().with_content_size(10.0).with_italic(true),
    );
    assert_eq!(families(&glyphs), ["New Computer Modern Book"]);
    // The generic serif is the default reading face, by cut too.
    let generic = TextStyle::content(48.0).with_family(FontChoice::Serif);
    assert_eq!(
        families(&shaped(&mut renderer, "Chapter", &generic)),
        ["New Computer Modern"]
    );
}

/// The SDK's standalone catalog (next to a third-party app's build) has only
/// New Computer Modern's GUST-licensed cuts: Book sets titles there too,
/// while the Tilo OS catalog sets them in Regular.
#[cfg(unix)]
#[test]
fn the_standalone_catalog_sets_titles_in_book() {
    let checkout = crate::font_catalog::checkout_root();
    let tmp = tempfile::tempdir().unwrap();
    let standalone = tmp.path();
    std::os::unix::fs::symlink(
        checkout.join("manifest.toml"),
        standalone.join("manifest.toml"),
    )
    .unwrap();
    for asset in &tilo_font_catalog::manifest().unwrap().font {
        if asset.scope.standalone() {
            std::os::unix::fs::symlink(checkout.join(&asset.file), standalone.join(&asset.file))
                .unwrap();
        }
    }
    assert!(!standalone.join("NewCM10-Regular.otf").exists());

    let style = reading(ReadingFace::Default);
    let title = style.clone().with_content_size(48.0);
    let cases = [
        (standalone, "New Computer Modern Book"),
        (checkout.as_path(), "New Computer Modern"),
    ];
    for (root, cut) in cases {
        let fs = crate::font_catalog::catalog_font_system(root).unwrap();
        let regular = fs
            .db()
            .faces()
            .any(|face| face.post_script_name == "NewCM10-Regular");
        assert_eq!(regular, cut == "New Computer Modern", "{}", root.display());
        let mut renderer = TextRenderer::with_font_system(fs);
        let glyphs = shaped(&mut renderer, "Chapter", &title);
        assert_eq!(families(&glyphs), [cut], "{}", root.display());
        assert!(glyphs.iter().all(|(_, id)| *id != 0));
        // Body text, footnotes and large italics are the same everywhere.
        for (style, cut) in [
            (style.clone(), "New Computer Modern Book"),
            (
                style.clone().with_content_size(10.0),
                "New Computer Modern 08",
            ),
            (title.clone().with_italic(true), "New Computer Modern"),
        ] {
            assert_eq!(
                families(&shaped(&mut renderer, "Chapter", &style)),
                [cut],
                "{} px in {}",
                style.size,
                root.display()
            );
        }
    }
}

#[test]
fn rich_text_spans_take_the_cut_of_their_own_size() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = reading(ReadingFace::Default).with_content_size(19.5);
    let spans = [
        crate::richtext::RichSpan::new("enough."),
        crate::richtext::RichSpan {
            size: Some(11.0),
            ..crate::richtext::RichSpan::new("1")
        },
    ];
    let layout = crate::richtext::RichTextLayout::shape_styled(
        &mut fs,
        &spans,
        &style,
        None,
        crate::richtext::TextAlign::Left,
        Vec::new(),
        Vec::new(),
    );
    let used: Vec<String> = layout
        .buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .filter_map(|glyph| {
            fs.db()
                .face(glyph.font_id)
                .and_then(|face| face.families.first().map(|(name, _)| name.clone()))
        })
        .collect();
    assert_eq!(
        used.first().map(String::as_str),
        Some("New Computer Modern Book")
    );
    assert_eq!(
        used.last().map(String::as_str),
        Some("New Computer Modern 08")
    );
}

#[test]
fn reading_figures_are_old_style_until_a_kind_asks_for_lining() {
    let mut renderer = crate::font_catalog::fixture_renderer();
    for face in ReadingFace::ALL {
        let style = reading(face);
        let oldstyle = shaped(&mut renderer, "1813", &style);
        let lining = shaped(&mut renderer, "1813", &style.clone().numeric());
        let plain = shaped(
            &mut renderer,
            "1813",
            &style
                .clone()
                .with_numeric_style(NumericStyle::ProportionalLining),
        );
        assert_ne!(oldstyle, lining, "{face:?}: numeric text is lining");
        // EB Garamond's default figures are old-style; every other face's
        // are lining, which the role turns old-style.
        if face != ReadingFace::Garamond {
            assert_ne!(oldstyle, plain, "{face:?}: running text is old-style");
        }
        assert!(
            style
                .font_features
                .iter()
                .any(|feature| feature.tag == OpenTypeTag::OLDSTYLE_NUMERALS)
        );
        assert!(
            !style
                .clone()
                .numeric()
                .font_features
                .iter()
                .any(|feature| feature.tag == OpenTypeTag::OLDSTYLE_NUMERALS)
        );
    }
}

#[test]
fn small_caps_and_ligatures_reach_every_reading_face() {
    let mut renderer = crate::font_catalog::fixture_renderer();
    for face in ReadingFace::ALL {
        let style = reading(face);
        let text = shaped(&mut renderer, "truth", &style);
        let caps = shaped(&mut renderer, "truth", &style.clone().with_small_caps(true));
        assert_ne!(text, caps, "{face:?}: small caps");
        assert!(
            shaped(&mut renderer, "fi", &style).len() == 1,
            "{face:?}: the fi ligature"
        );
        assert_eq!(
            shaped(
                &mut renderer,
                "fi",
                &style
                    .clone()
                    .with_font_feature(OpenTypeTag::STANDARD_LIGATURES, 0)
            )
            .len(),
            2,
            "{face:?}: ligatures can be turned off"
        );
    }
}

#[test]
fn greek_and_cyrillic_are_the_reading_face_s_own() {
    let mut renderer = crate::font_catalog::fixture_renderer();
    for face in [
        ReadingFace::Default,
        ReadingFace::Libertinus,
        ReadingFace::Literata,
    ] {
        let style = reading(face);
        let cut = shaped(&mut renderer, "a", &style);
        for line in ["Η αρχή είναι το ήμισυ", "Хорошее начало", "Tiếng Việt"]
        {
            let glyphs = shaped(&mut renderer, line, &style);
            assert_eq!(families(&glyphs), families(&cut), "{face:?}: {line}");
            assert!(glyphs.iter().all(|(_, id)| *id != 0), "{face:?}: {line}");
        }
    }
}
