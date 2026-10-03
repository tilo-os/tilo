use super::*;
use cosmic_text::fontdb::{Query, Style, Weight};
use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping};

fn fixture() -> FontCatalog {
    FontCatalog::controlled_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
        "en-US",
    )
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn the_controlled_catalog_admits_only_manifest_and_named_host_faces() {
    // phase: controlled_catalog_has_only_the_twenty_manifest_faces
    {
        let catalog = fixture();
        let fs = catalog.load().unwrap();
        assert_eq!(fs.db().len(), 22); // 17 single faces plus five TTC faces.
        let root = catalog.root().canonicalize().unwrap();
        for face in fs.db().faces() {
            match &face.source {
                fontdb::Source::File(path) => assert!(path.starts_with(&root)),
                _ => panic!("canonical file must stay file-backed"),
            }
        }
        for (generic, expected) in [
            (Family::SansSerif, crate::text_render::UI_SANS_FAMILY),
            (Family::Serif, "Noto Serif"),
            (Family::Monospace, "Noto Sans Mono"),
        ] {
            for weight in [Weight::NORMAL, Weight(600), Weight::BOLD] {
                let id = fs
                    .db()
                    .query(&Query {
                        families: &[generic],
                        weight,
                        ..Query::default()
                    })
                    .unwrap();
                assert!(
                    fs.db()
                        .face(id)
                        .unwrap()
                        .families
                        .iter()
                        .any(|f| f.0 == expected)
                );
            }
        }
        for expected in [
            crate::text_render::UI_SANS_FAMILY,
            "Noto Sans",
            "Noto Serif",
        ] {
            let id = fs
                .db()
                .query(&Query {
                    families: &[Family::Name(expected)],
                    style: Style::Italic,
                    ..Query::default()
                })
                .unwrap();
            assert_eq!(fs.db().face(id).unwrap().style, Style::Italic);
        }
        for expected in ["Noto Sans Math", "Adwaita Sans", "Noto Sans Symbols 2"] {
            assert!(
                fs.db()
                    .query(&Query {
                        families: &[Family::Name(expected)],
                        ..Query::default()
                    })
                    .is_some()
            );
        }
    }
    // phase: named_host_fonts_survive_but_reserved_claimants_do_not
    {
        let catalog = fixture();
        let mut called = false;
        let mut named_id = None;
        let mut duplicate_id = None;
        let mut alias_id = None;
        let mut ambient_adwaita_id = None;
        let product = FontCatalog::product(catalog.root(), "en-US");
        let fs = product
            .load_with_system(|db| {
                called = true;
                let face = db.faces().next().unwrap().clone();
                let mut duplicate = face.clone();
                duplicate.families = vec![("Noto Sans".into(), face.families[0].1)];
                duplicate_id = Some(db.push_face_info(duplicate));
                let mut alias = face.clone();
                alias.families = vec![("Noto Sans Hebrew Light".into(), face.families[0].1)];
                alias_id = Some(db.push_face_info(alias));
                let mut ambient = face.clone();
                ambient.families = vec![("Adwaita Sans".into(), face.families[0].1)];
                ambient_adwaita_id = Some(db.push_face_info(ambient));
                let mut other = face;
                other.families = vec![("Local Named Test".into(), other.families[0].1)];
                named_id = Some(db.push_face_info(other));
            })
            .unwrap();
        assert!(called);
        assert!(fs.db().face(duplicate_id.unwrap()).is_none());
        assert!(fs.db().face(alias_id.unwrap()).is_none());
        assert!(fs.db().face(named_id.unwrap()).is_some());
        assert_eq!(
            fs.db().query(&Query {
                families: &[Family::Name("Adwaita Sans")],
                ..Query::default()
            }),
            ambient_adwaita_id
        );
        catalog
            .load_with_system(|_| panic!("controlled catalog must never discover ambient fonts"))
            .unwrap();
    }
    // phase: missing_or_corrupt_resources_fail_before_system_discovery
    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let catalog = FontCatalog::product(root, "en-US");
        let error = catalog
            .load_with_system(|_| panic!("must not mask missing manifest"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("manifest.toml"));
        std::fs::write(root.join("manifest.toml"), MANIFEST).unwrap();
        let error = catalog
            .load_with_system(|_| panic!("must not mask missing font"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("InstrumentSans[wdth,wght].ttf"));
        let mut bytes =
            std::fs::read(fixture().root().join("InstrumentSans[wdth,wght].ttf")).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(root.join("InstrumentSans[wdth,wght].ttf"), bytes).unwrap();
        let error = catalog
            .load_with_system(|_| panic!("must not mask corruption"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("SHA-256"));
    }
}

#[test]
fn catalog_scripts_and_axes_resolve_the_declared_families() {
    // phase: canonical_scripts_resolve_the_declared_families
    {
        let mut fs = fixture().load().unwrap();
        for (text, expected) in [
            ("مرحبا", "Noto Sans Arabic"),
            ("שלום", "Noto Sans Hebrew"),
            ("नमस्ते", "Noto Sans Devanagari"),
            ("বাংলা", "Noto Sans Bengali"),
            ("สวัสดี", "Noto Sans Thai"),
            ("日本語", "Noto Sans CJK SC"),
            ("ひらがな", "Noto Sans CJK JP"),
            ("한글", "Noto Sans CJK KR"),
            ("🙂", "Noto Color Emoji"),
        ] {
            let mut buffer = Buffer::new(&mut fs, Metrics::new(18.0, 24.0));
            buffer.set_text(
                text,
                &Attrs::new().family(Family::SansSerif),
                Shaping::Advanced,
                None,
            );
            buffer.shape_until_scroll(&mut fs, false);
            let glyphs: Vec<_> = buffer.layout_runs().flat_map(|run| run.glyphs).collect();
            assert!(!glyphs.is_empty(), "{text}");
            for glyph in glyphs {
                assert_ne!(glyph.glyph_id, 0, "{text}");
                assert!(
                    fs.db()
                        .face(glyph.font_id)
                        .unwrap()
                        .families
                        .iter()
                        .any(|f| f.0 == expected),
                    "{text}: expected {expected}"
                );
            }
        }
    }
    // phase: adwaita_axis_order_is_part_of_the_fixture_contract
    {
        use skrifa::{FontRef, MetadataProvider};
        let fs = fixture().load().unwrap();
        let id = fs
            .db()
            .query(&Query {
                families: &[Family::Name("Adwaita Sans")],
                ..Query::default()
            })
            .unwrap();
        let tags = fs
            .db()
            .with_face_data(id, |bytes, index| {
                FontRef::from_index(bytes, index)
                    .unwrap()
                    .axes()
                    .iter()
                    .map(|axis| axis.tag().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(tags, ["opsz", "wght"]);
    }
}

// A browser must verify the same asset bytes and produce actual toolkit shaping.
// Skipping a missing/corrupt asset or using ambient fonts makes these fail.
#[test]
fn byte_catalog_rejects_missing_and_corrupt_assets() {
    let catalog = fixture_catalog();
    assert!(catalog.load_bytes(|_| None).is_err());
    assert!(
        catalog
            .load_bytes(|name| {
                let mut bytes = std::fs::read(catalog.root().join(name)).ok()?;
                bytes[0] ^= 1;
                Some(bytes)
            })
            .is_err()
    );
}

#[test]
fn byte_catalog_preserves_canonical_faces_and_locale() {
    let catalog = fixture_catalog();
    let fonts = catalog
        .load_bytes(|name| std::fs::read(catalog.root().join(name)).ok())
        .unwrap();
    assert_eq!(fonts.locale(), "en-US");
    assert!(fonts.db().faces().any(|f| {
        f.families
            .iter()
            .any(|n| n.0 == crate::text_render::UI_SANS_FAMILY)
    }));
    let _renderer = crate::TextRenderer::with_font_system(fonts);
}

#[test]
fn browser_startup_catalog_requires_only_the_core_family_assets() {
    let catalog = FontCatalog::product("", "en-US");
    let root = fixture().root().to_path_buf();
    let mut requested = Vec::new();
    let fonts = catalog
        .load_browser_startup_bytes(|name| {
            requested.push(name.to_owned());
            std::fs::read(root.join(name)).ok()
        })
        .unwrap();
    assert_eq!(
        requested,
        [
            "InstrumentSans[wdth,wght].ttf",
            "InstrumentSans-Italic[wdth,wght].ttf",
            "NotoSans[wght].ttf",
            "NotoSans-Italic[wght].ttf",
            "NotoSerif[wght].ttf",
            "NotoSerif-Italic[wght].ttf",
            "NotoSansMono[wght].ttf",
        ]
    );
    assert_eq!(
        requested
            .iter()
            .map(|name| std::fs::metadata(root.join(name)).unwrap().len())
            .sum::<u64>(),
        4_078_136
    );
    assert_eq!(fonts.db().len(), 7);
}

#[test]
fn incremental_browser_asset_install_is_idempotent_and_integrity_checked() {
    let catalog = FontCatalog::product("", "en-US");
    let root = fixture().root().to_path_buf();
    let mut fonts = catalog
        .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
        .unwrap();
    let name = "NotoSansArabic[wght].ttf";
    let bytes = std::fs::read(root.join(name)).unwrap();
    assert!(
        catalog
            .install_browser_asset(&mut fonts, name, bytes.clone())
            .unwrap()
    );
    assert!(
        !catalog
            .install_browser_asset(&mut fonts, name, bytes.clone())
            .unwrap()
    );
    let mut corrupt = bytes;
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(
        catalog
            .install_browser_asset(&mut fonts, "NotoSansThai[wght].ttf", corrupt)
            .is_err()
    );
    assert!(
        catalog
            .install_browser_asset(&mut fonts, "../font.ttf", vec![1])
            .is_err()
    );
}

#[test]
fn a_face_installed_twice_resolves_the_same_whatever_the_discovery_order() {
    let tmp = tempfile::tempdir().unwrap();
    let font = std::fs::read(fixture().root().join("NotoSans[wght].ttf")).unwrap();
    for dir in ["a", "b"] {
        std::fs::create_dir(tmp.path().join(dir)).unwrap();
        std::fs::write(tmp.path().join(dir).join("NotoSans.ttf"), &font).unwrap();
    }
    for order in [["a", "b"], ["b", "a"]] {
        let mut db = fontdb::Database::new();
        for dir in order {
            db.load_font_file(tmp.path().join(dir).join("NotoSans.ttf"))
                .unwrap();
        }
        drop_duplicate_faces(&mut db);
        let query = Query {
            families: &[fontdb::Family::Name("Noto Sans")],
            ..Query::default()
        };
        let id = db.query(&query).unwrap();
        let face = db.face(id).unwrap();
        assert_eq!(db.len(), 1);
        assert!(
            matches!(&face.source, fontdb::Source::File(path) if path.starts_with(tmp.path().join("a")))
        );
    }
}
