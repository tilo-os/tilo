use super::*;
use cosmic_text::fontdb::{Query, Style, Weight};
use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping};

/// The probe a capture asks before it draws: it must answer about the roots it
/// says it searched, and a run that cannot find a catalog must learn how to
/// point one at itself.
#[test]
#[cfg(feature = "native")]
fn the_catalog_probe_answers_about_the_roots_it_searched() {
    let roots = catalog_roots();
    assert!(!roots.is_empty(), "a run always has somewhere to look");
    if let Some(root) = catalog_root() {
        assert!(
            roots.contains(&root),
            "the catalog came from outside the searched roots: {}",
            root.display()
        );
    }
    assert!(
        catalog_missing_message().contains(tilo_font_catalog::OVERRIDE_ENV),
        "a run with no catalog must be told how to name one"
    );
}

fn fixture() -> FontCatalog {
    crate::font_catalog::fixture_catalog()
}

tilo_test_env::scenario! {
    /// What a catalog loads from its root or from bytes: the declared faces, verified, under their
    /// families.
    /// Each phase was a test of its own and builds its own fixture.
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn the_catalog_loads_only_the_faces_it_declares_and_verifies;

    phase the_controlled_catalog_admits_only_manifest_and_named_host_faces;
    {
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

    phase catalog_scripts_and_axes_resolve_the_declared_families;
    {
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

    phase byte_catalog_rejects_missing_and_corrupt_assets;
    // A browser must verify the same asset bytes and produce actual toolkit shaping.
    // Skipping a missing/corrupt asset or using ambient fonts makes these fail.
    {
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

    phase byte_catalog_preserves_canonical_faces_and_locale;
    {
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

    phase a_face_installed_twice_resolves_the_same_whatever_the_discovery_order;
    {
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

    phase catalog_root_trust_is_verify_for_checkout_and_writable_roots;
    // Only the image's root-owned, read-only catalog skips hashing. A checkout,
    // an executable `fonts/` and any directory this user can write are
    // verified byte for byte, and a modified font there is refused.
    {
        let roots = catalog_roots();
        let checkout = crate::font_catalog::checkout_root();
        assert!(roots.contains(&checkout));
        assert_eq!(Trust::of_root(&checkout), Trust::Verify);
        let adjacent = roots.first().unwrap();
        assert!(adjacent.starts_with(std::env::current_exe().unwrap().parent().unwrap()));
        assert_eq!(Trust::of_root(adjacent), Trust::Verify);

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("manifest.toml"), MANIFEST).unwrap();
        assert_eq!(Trust::of_root(tmp.path()), Trust::Verify);
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

    phase new_computer_modern_cuts_are_their_own_families;
    // Every New Computer Modern file names itself `NewComputerModern` at weight
    // 400; the catalog sets each cut under its family so text can ask for
    // one, and shares the Bold among them.
    {
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
}

tilo_test_env::scenario! {
    /// The fonts a browser starts with, installs later and fetches for a style, each checked
    /// against its digest.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_browser_installs_its_fonts_by_digest_and_fetches_the_cuts_it_needs;

    phase browser_startup_catalog_requires_only_the_core_family_assets;
    {
        let catalog = FontCatalog::product("", "en-US");
        let root = fixture().root().to_path_buf();
        let mut requested = Vec::new();
        let mut read = super::test_font_files(&root);
        let fonts = catalog
            .load_browser_startup_bytes(|name| {
                requested.push(name.to_owned());
                read(name)
            })
            .unwrap();
        assert_eq!(
            requested,
            ["TiloSans[wdth,wght].ttf", "Onest-Cyrillic[wght].ttf"]
        );
        assert_eq!(
            requested
                .iter()
                .map(|name| std::fs::metadata(root.join(name)).unwrap().len())
                .sum::<u64>(),
            406_096
        );
        assert_eq!(fonts.db().len(), 2);
    }

    phase a_startup_font_whose_page_digest_or_size_differs_from_the_build_is_refused;
    {
        let catalog = FontCatalog::product("", "en-US");
        let root = fixture().root().to_path_buf();
        let mut read = super::test_font_files(&root);
        let Err(error) = catalog.load_browser_startup_bytes(|name| {
            let (bytes, mut digest) = read(name)?;
            digest.replace_range(..1, if digest.starts_with('0') { "1" } else { "0" });
            Some((bytes, digest))
        }) else {
            panic!("a wrong digest is refused");
        };
        let error = error.to_string();
        assert!(error.contains("SHA-256 mismatch"), "{error}");
        let Err(error) = catalog.load_browser_startup_bytes(|name| {
            let mut bytes = std::fs::read(root.join(name)).ok()?;
            bytes.pop();
            let digest = super::test_sha256(&bytes);
            Some((bytes, digest))
        }) else {
            panic!("a wrong size is refused");
        };
        let error = error.to_string();
        assert!(error.contains("SHA-256 mismatch"), "{error}");
        // A startup font the host cannot supply is refused too, not skipped.
        let Err(error) = catalog.load_browser_startup_bytes(|_| None) else {
            panic!("a missing startup font is refused");
        };
        let error = error.to_string();
        assert!(error.contains("missing font bytes"), "{error}");
    }

    phase incremental_browser_asset_install_is_idempotent_and_integrity_checked;
    {
        let catalog = FontCatalog::product("", "en-US");
        let root = fixture().root().to_path_buf();
        let mut fonts = catalog
            .load_browser_startup_bytes(super::test_font_files(&root))
            .unwrap();
        let name = "NotoSansArabic[wght].ttf";
        let bytes = std::fs::read(root.join(name)).unwrap();
        let digest = super::test_sha256(&bytes);
        // The catalog's own bytes are refused under any digest but the pinned one, compared as
        // written: no digest, the pinned digest in upper case, the digest of other bytes.
        let faces = fonts.db().len();
        for (case, wrong) in [
            ("no digest", String::new()),
            ("the pinned digest in upper case", digest.to_uppercase()),
            ("the digest of other bytes", super::test_sha256(&bytes[1..])),
        ] {
            let Err(error) = catalog.install_browser_asset(&mut fonts, name, bytes.clone(), &wrong)
            else {
                panic!("{case}: a digest other than the pinned one is refused");
            };
            let error = error.to_string();
            assert!(error.contains("SHA-256 mismatch"), "{case}: {error}");
        }
        assert_eq!(fonts.db().len(), faces, "a refused asset adds no face");
        assert!(
            catalog
                .install_browser_asset(&mut fonts, name, bytes.clone(), &digest)
                .unwrap()
        );
        let installed = fonts.db().len();
        assert!(installed > faces, "an installed asset adds its faces");
        assert!(
            !catalog
                .install_browser_asset(&mut fonts, name, bytes.clone(), &digest)
                .unwrap()
        );
        assert_eq!(
            fonts.db().len(),
            installed,
            "installing it again adds nothing"
        );
        let mut corrupt = bytes;
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(
            catalog
                .install_browser_asset(
                    &mut fonts,
                    "NotoSansThai[wght].ttf",
                    corrupt.clone(),
                    &super::test_sha256(&corrupt)
                )
                .is_err()
        );
        assert!(
            catalog
                .install_browser_asset(
                    &mut fonts,
                    "../font.ttf",
                    vec![1],
                    &super::test_sha256(&[1])
                )
                .is_err()
        );
        let Err(error) =
            catalog.install_browser_asset(&mut fonts, "", vec![1], &super::test_sha256(&[1]))
        else {
            panic!("an asset with no name is refused");
        };
        let error = error.to_string();
        assert!(error.contains("unknown canonical font asset"), "{error}");
        assert_eq!(
            fonts.db().len(),
            installed,
            "a corrupt, escaping or unnamed asset adds no face"
        );
    }

    phase a_browser_fetches_the_cuts_a_reading_style_can_take;
    {
        let catalog = FontCatalog::product("", "en-US");
        let root = fixture().root().to_path_buf();
        let fonts = catalog
            .load_browser_startup_bytes(super::test_font_files(&root))
            .unwrap();
        let mut renderer = crate::TextRenderer::with_browser_font_system(fonts);
        let reading = crate::tokens::Typography::default().style(crate::tokens::TextRole::Reading);
        // Body text: the page starts with the UI face alone, so it fetches Book
        // and the 8 pt cut its footnotes take.
        let cuts = ["NewCM08-Book.otf", "NewCM10-Book.otf"];
        renderer.measure_style("Chapter", None, &reading);
        assert_eq!(renderer.take_browser_font_demands(), cuts);
        let title = reading.clone().with_content_size(48.0);
        renderer.measure_style("Chapter", None, &title);
        assert_eq!(renderer.take_browser_font_demands(), cuts);
        let body = crate::tokens::Typography::default().style(crate::tokens::TextRole::Body);
        renderer.measure_style("Settings", None, &body.strong());
        assert!(renderer.take_browser_font_demands().is_empty());
    }

    phase a_browser_fetches_the_monospace_a_rich_layout_asks_for;
    {
        // An editor's code line is a rich layout: its glyphs shape in the
        // startup face until the monospace arrives, so the layout itself must
        // say which family it wanted.
        let catalog = FontCatalog::product("", "en-US");
        let root = fixture().root().to_path_buf();
        let fonts = catalog
            .load_browser_startup_bytes(super::test_font_files(&root))
            .unwrap();
        let mut renderer = crate::TextRenderer::with_browser_font_system(fonts);
        let code = crate::tokens::Typography::default().style(crate::tokens::TextRole::Code);
        let layout = crate::richtext::RichTextLayout::shape_styled(
            &mut renderer.font_system,
            &[crate::richtext::RichSpan::new("ls -la")],
            &code,
            None,
            crate::richtext::TextAlign::Left,
            Vec::new(),
            Vec::new(),
        );
        renderer.note_browser_rich_layout(&layout);
        let demands = renderer.take_browser_font_demands();
        assert!(
            demands.iter().any(|file| file.starts_with("NotoSansMono")),
            "{demands:?}"
        );
    }
}

/// What a process run from a checkout or a screenshot gets on a machine
/// without Tilo Sans installed: the product catalog, found past roots that do
/// not hold one this build accepts, sets UI text in Tilo Sans. The host
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

tilo_test_env::scenario! {
    /// The database built from a catalog's recorded faces or held mapped, against the one parsing
    /// builds.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_recorded_or_mapped_catalog_is_the_database_parsing_builds;

    phase a_catalog_s_recorded_faces_build_the_database_parsing_builds;
    // A catalog's build records the faces of its files
    // (`tilo_font_catalog::faces`), and every process builds the catalog's part
    // of its font database from that record instead of parsing each font. The
    // database the record makes must be the one parsing makes, face for face,
    // in the same order, under the same Tilo families: nothing a process
    // shapes or falls back to may depend on which way it was built.
    {
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

    phase a_recorded_catalog_on_writable_storage_is_parsed;
    {
        let root = tilo_font_catalog::checkout_root().canonicalize().unwrap();
        assert_eq!(Trust::of_root(&root), Trust::Verify);
        assert!(tilo_font_catalog::faces::FaceIndex::read_trusted(&root, Trust::Verify).is_none());
    }

    phase a_mapped_database_reads_and_shapes_without_its_files;
    // Shaping on the compositor's main loop must never open a font by path: a
    // path lookup there waits on the disk with the desktop frozen. A mapped
    // database has every face's bytes, so it still shapes and reads its faces
    // with the files gone, in the order and generic families it had.
    {
        let tmp = tempfile::tempdir().unwrap();
        let source = fixture().root().canonicalize().unwrap();
        let mut db = fontdb::Database::new();
        for name in ["NotoSans[wght].ttf", "NotoSansMono[wght].ttf"] {
            let copy = tmp.path().join(name);
            std::fs::copy(source.join(name), &copy).unwrap();
            db.load_font_file(&copy).unwrap();
        }
        db.set_monospace_family("Noto Sans Mono");
        let held = mapped(&db);
        for entry in std::fs::read_dir(tmp.path()).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }

        let describe = |db: &fontdb::Database| {
            db.faces()
                .map(|face| (face.post_script_name.clone(), face.index, face.weight))
                .collect::<Vec<_>>()
        };
        assert_eq!(describe(&held), describe(&db));
        assert_eq!(
            held.family_name(&fontdb::Family::Monospace),
            "Noto Sans Mono"
        );
        for face in held.faces() {
            assert!(
                matches!(face.source, fontdb::Source::SharedFile(..)),
                "{} is held mapped",
                face.post_script_name
            );
            let bytes = held.with_face_data(face.id, |data, _| data.len());
            assert!(bytes.unwrap_or(0) > 0, "{} reads", face.post_script_name);
        }

        let mut system = system_over("en-US".into(), held);
        let mut buffer = Buffer::new(&mut system, Metrics::new(14.0, 18.0));
        buffer.set_text(
            "Shaped without a file",
            &Attrs::new()
                .family(Family::Name("Noto Sans"))
                .weight(Weight(600)),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut system, false);
        let glyphs: usize = buffer.layout_runs().map(|run| run.glyphs.len()).sum();
        assert!(glyphs > 0, "the text shaped");
        assert!(
            buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .all(|glyph| glyph.glyph_id != 0),
            "every glyph found in a mapped face"
        );
    }
}
