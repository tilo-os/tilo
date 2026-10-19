use super::*;
use cosmic_text::fontdb::{Query, Style, Weight};
use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping};

fn fixture() -> FontCatalog {
    FontCatalog::controlled_fixture(crate::font_catalog::checkout_root(), "en-US")
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn the_controlled_catalog_admits_only_manifest_and_named_host_faces() {
    // phase: controlled_catalog_has_only_the_manifest_faces
    {
        let catalog = fixture();
        let fs = catalog.load().unwrap();
        assert_eq!(fs.db().len(), 41); // 36 single faces plus five TTC faces.
        let root = catalog.root().canonicalize().unwrap();
        for face in fs.db().faces() {
            match &face.source {
                fontdb::Source::File(path) => assert!(path.starts_with(&root)),
                _ => panic!("canonical file must stay file-backed"),
            }
        }
        for (generic, expected) in [
            (Family::SansSerif, crate::text_render::UI_SANS_FAMILY),
            (Family::Serif, crate::text_render::UI_SERIF_FAMILY),
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
            "New Computer Modern",
            "New Computer Modern Book",
            "Libertinus Serif",
            "Literata",
            "EB Garamond",
            "Source Serif 4 Variable",
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
        assert!(error.to_string().contains("TiloSans[wdth,wght].ttf"));
        let mut bytes = std::fs::read(fixture().root().join("TiloSans[wdth,wght].ttf")).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(root.join("TiloSans[wdth,wght].ttf"), bytes).unwrap();
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
            "TiloSans[wdth,wght].ttf",
            "Onest-Cyrillic[wght].ttf",
            "NotoSans[wght].ttf",
        ]
    );
    assert_eq!(
        requested
            .iter()
            .map(|name| std::fs::metadata(root.join(name)).unwrap().len())
            .sum::<u64>(),
        1_118_556
    );
    assert_eq!(fonts.db().len(), 3);
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

/// Only the image's root-owned, read-only catalog skips hashing. A checkout,
/// an executable's own `fonts/` and any directory this user can write are
/// verified byte for byte, and a modified font there is refused.
#[test]
fn catalog_root_trust_is_verify_for_checkout_and_writable_roots() {
    let roots = catalog_roots();
    let checkout = crate::font_catalog::checkout_root();
    assert!(roots.contains(&checkout));
    assert_eq!(Trust::of_root(&checkout), Trust::Verify);
    // The test binary's own `fonts/` sits in the writable build tree.
    let adjacent = roots.first().unwrap();
    assert!(adjacent.starts_with(std::env::current_exe().unwrap().parent().unwrap()));
    assert_eq!(Trust::of_root(adjacent), Trust::Verify);

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("manifest.toml"), MANIFEST).unwrap();
    assert_eq!(Trust::of_root(tmp.path()), Trust::Verify);
    // Verification goes in manifest order, so the first product file decides.
    let first = tilo_font_catalog::manifest()
        .unwrap()
        .font
        .iter()
        .find(|asset| asset.scope == Scope::Product)
        .unwrap();
    let mut bytes = std::fs::read(first.path_in(&checkout).unwrap()).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(first.path_in(tmp.path()).unwrap(), bytes).unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let error = verify_bundle(&root, false, Trust::of_root(&root), None)
        .err()
        .expect("a modified font in a writable root is refused");
    assert!(error.problem.contains("SHA-256"), "{error}");
}

/// What a process run from a checkout or a screenshot gets on a machine
/// without Tilo Sans installed: the product catalog, found past roots that do
/// not hold one this build accepts, sets UI text in Tilo Sans. The host's own
/// copy of a catalog family sorts before the catalog's path and must still
/// not displace the catalog's face.
#[cfg(unix)]
#[test]
fn a_machine_without_tilo_sans_sets_ui_text_in_the_bundled_catalog() {
    let source = crate::font_catalog::checkout_root();
    assert_eq!(catalog_roots().last(), Some(&source));

    let tmp = tempfile::tempdir().unwrap();
    let host = tmp.path().join("a-host");
    std::fs::create_dir(&host).unwrap();
    std::fs::copy(source.join("NotoSans[wght].ttf"), host.join("NotoSans.ttf")).unwrap();
    let stale = tmp.path().join("b-stale");
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("manifest.toml"), "# another Tilo build\n").unwrap();
    // The catalog under a path that sorts after the host copy, as /usr does
    // before a checkout.
    let catalog = tmp.path().join("c-catalog");
    std::fs::create_dir(&catalog).unwrap();
    for entry in std::fs::read_dir(&source).unwrap() {
        let entry = entry.unwrap().path();
        std::os::unix::fs::symlink(&entry, catalog.join(entry.file_name().unwrap())).unwrap();
    }

    let roots = [tmp.path().join("missing"), stale, catalog.clone()];
    let mut fs = load_first_catalog(&roots, "en-US", |db| {
        db.load_fonts_dir(&host);
    })
    .unwrap();
    let catalog = catalog.canonicalize().unwrap();

    for (family, expected) in [
        (Family::SansSerif, crate::text_render::UI_SANS_FAMILY),
        (Family::Name("Noto Sans"), "Noto Sans"),
    ] {
        let id = fs
            .db()
            .query(&Query {
                families: &[family],
                ..Query::default()
            })
            .unwrap();
        let face = fs.db().face(id).unwrap();
        assert!(face.families.iter().any(|f| f.0 == expected), "{expected}");
        assert!(
            matches!(&face.source, fontdb::Source::File(path) if path.starts_with(&catalog)),
            "{expected} must come from the catalog: {:?}",
            face.source
        );
    }

    let mut buffer = Buffer::new(&mut fs, Metrics::new(14.0, 20.0));
    buffer.set_text(
        "Settings",
        &Attrs::new().family(Family::SansSerif),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(&mut fs, false);
    let glyphs: Vec<_> = buffer.layout_runs().flat_map(|run| run.glyphs).collect();
    assert_eq!(glyphs.len(), "Settings".len());
    for glyph in glyphs {
        assert!(
            fs.db()
                .face(glyph.font_id)
                .unwrap()
                .families
                .iter()
                .any(|f| f.0 == crate::text_render::UI_SANS_FAMILY)
        );
    }
}

/// Every New Computer Modern file names itself `NewComputerModern` at weight
/// 400; the catalog sets each cut under its own family so text can ask for
/// one, and shares the Bold among them.
#[test]
fn new_computer_modern_cuts_are_their_own_families() {
    let fs = fixture().load().unwrap();
    let face_of = |family: &str, weight: u16, style: Style| {
        let id = fs
            .db()
            .query(&Query {
                families: &[Family::Name(family)],
                weight: Weight(weight),
                style,
                ..Query::default()
            })
            .unwrap_or_else(|| panic!("{family} {weight} {style:?}"));
        fs.db().face(id).unwrap().post_script_name.clone()
    };
    for (family, weight, style, expected) in [
        (
            "New Computer Modern Book",
            400,
            Style::Normal,
            "NewCM10-Book",
        ),
        (
            "New Computer Modern Book",
            400,
            Style::Italic,
            "NewCM10-BookItalic",
        ),
        (
            "New Computer Modern Book",
            700,
            Style::Normal,
            "NewCM10-Bold",
        ),
        ("New Computer Modern", 400, Style::Normal, "NewCM10-Regular"),
        ("New Computer Modern", 400, Style::Italic, "NewCM10-Italic"),
        (
            "New Computer Modern",
            600,
            Style::Italic,
            "NewCM10-BoldItalic",
        ),
        ("New Computer Modern 08", 400, Style::Normal, "NewCM08-Book"),
        ("New Computer Modern 08", 700, Style::Normal, "NewCM10-Bold"),
        (
            "Libertinus Serif Display",
            400,
            Style::Normal,
            "LibertinusSerifDisplay-Regular",
        ),
    ] {
        assert_eq!(
            face_of(family, weight, style),
            expected,
            "{family} {weight} {style:?}"
        );
    }
    assert!(
        fs.db().faces().all(|face| face
            .families
            .iter()
            .all(|(name, _)| name != "NewComputerModern")),
        "the files' shared family name is never queryable"
    );
}

#[test]
fn a_browser_fetches_the_cuts_a_reading_style_can_take() {
    let catalog = FontCatalog::product("", "en-US");
    let root = fixture().root().to_path_buf();
    let fonts = catalog
        .load_browser_startup_bytes(|name| std::fs::read(root.join(name)).ok())
        .unwrap();
    let mut renderer = crate::TextRenderer::with_browser_font_system(fonts);
    let reading = crate::tokens::Typography::default().style(crate::tokens::TextRole::Reading);
    // Body text: the page starts with the UI face alone, so it fetches Book
    // and the 8 pt cut its footnotes take.
    let cuts = ["NewCM08-Book.otf", "NewCM10-Book.otf"];
    renderer.measure_style("Chapter", None, &reading);
    assert_eq!(renderer.take_browser_font_demands(), cuts);
    // A browser serves the standalone catalog, which has no Regular cut:
    // Book sets a title too. A UI role in a startup face adds nothing.
    let title = reading.clone().with_content_size(48.0);
    renderer.measure_style("Chapter", None, &title);
    assert_eq!(renderer.take_browser_font_demands(), cuts);
    let body = crate::tokens::Typography::default().style(crate::tokens::TextRole::Body);
    renderer.measure_style("Settings", None, &body.strong());
    assert!(renderer.take_browser_font_demands().is_empty());
}

/// A catalog's build records the faces of its files
/// (`tilo_font_catalog::faces`), and every process builds the catalog's part
/// of its font database from that record instead of parsing each font. The
/// database the record makes must be the one parsing makes, face for face,
/// in the same order, under the same Tilo families: nothing a process
/// shapes or falls back to may depend on which way it was built.
#[test]
fn a_catalog_s_recorded_faces_build_the_database_parsing_builds() {
    let root = tilo_font_catalog::checkout_root().canonicalize().unwrap();
    let recorded = tilo_font_catalog::faces::FaceIndex::build(&root).unwrap();
    let parsed = verify_bundle(&root, false, Trust::Verify, None).unwrap();
    let read = verify_bundle(&root, false, Trust::Verify, Some(&recorded)).unwrap();
    let faces = |bundle: &VerifiedBundle| {
        bundle
            .db
            .faces()
            .map(|face| {
                let path = match &face.source {
                    fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                        Some(path.clone())
                    }
                    fontdb::Source::Binary(_) => None,
                };
                (
                    path,
                    face.index,
                    face.families
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect::<Vec<_>>(),
                    face.post_script_name.clone(),
                    face.style,
                    face.weight,
                    face.stretch,
                    face.monospaced,
                )
            })
            .collect::<Vec<_>>()
    };
    assert!(!faces(&parsed).is_empty());
    assert_eq!(faces(&read), faces(&parsed));
    assert_eq!(read.canonical, parsed.canonical);
    assert_eq!(read.required, parsed.required);
}

/// The record is believed only where the image's integrity pins it like the
/// fonts: in a catalog on writable storage (a checkout, a per-user runtime)
/// a record anyone could have written is passed over and the fonts parsed.
#[test]
fn a_recorded_catalog_on_writable_storage_is_parsed() {
    let root = tilo_font_catalog::checkout_root().canonicalize().unwrap();
    assert_eq!(Trust::of_root(&root), Trust::Verify);
    assert!(tilo_font_catalog::faces::FaceIndex::read_trusted(&root, Trust::Verify).is_none());
}
