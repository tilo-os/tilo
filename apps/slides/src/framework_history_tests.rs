use ui::widget::{AppModel, ModelPath};
include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
fn test_root() -> Box<dyn ui::widget::Widget> {
    crate::app_root()
}
fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    let mut deck = crate::model::seed_deck();
    deck.title = "Selected deck".into();
    deck.slides[0].notes = "Recorded speaker notes".into();
    crate::doc::SlidesDoc::seed(&mut doc, &deck);
    let selected = doc.get_heads()[0];
    crate::doc::SlidesDoc::set_title(&mut doc, "Live deck");
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "Selected deck".into())
}
fn verify_restored(doc: &AutoCommit) {
    let deck = crate::doc::SlidesDoc::load(doc).unwrap();
    assert_eq!(deck.title, "Selected deck");
    assert_eq!(deck.slides[0].notes, "Recorded speaker notes");
}
fn malformed_fixture(doc: &mut AutoCommit) {
    doc.put(ROOT, "deck", 7).unwrap();
}
/// Empty decks survive sync, malformed ids are refused, exact restore keeps fields and absent versions are refused.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn history_restore_preserves_exact_content_and_refuses_invalid_versions() {
    // Case `existing_empty_deck_survives_production_root_sync_and_reopen`.
    {
        let mut doc = AutoCommit::new();
        let mut deck = crate::model::seed_deck();
        deck.slides.clear();
        crate::doc::SlidesDoc::seed(&mut doc, &deck);
        let heads = doc.get_heads();
        test_root().sync(&mut doc, &ROOT);
        assert_eq!(doc.get_heads(), heads, "existing empty deck must not seed");
        assert_eq!(crate::doc::SlidesDoc::load(&doc), Some(deck.clone()));
        let mut reopened = AutoCommit::load(&doc.save()).unwrap();
        test_root().sync(&mut reopened, &ROOT);
        assert_eq!(crate::doc::SlidesDoc::load(&reopened), Some(deck));
        assert_eq!(reopened.get_heads(), heads);
    }

    // Case `malformed_shape_ids_and_renderer_geometry_are_refused_without_mutation`.
    {
        for case in 0..11 {
            let (mut doc, _, _) = fixture();
            let deck = map(&doc, &ROOT, "deck");
            let slides = map(&doc, &deck, "slides");
            let slide = map(&doc, &slides, 0);
            let elements = map(&doc, &slide, "elements");
            let el = map(&doc, &elements, 0);
            match case {
                0 => doc.put(&deck, "title", "scalar title").unwrap(),
                1 => doc.put(&deck, "slides", "scalar list").unwrap(),
                2 => doc.delete(&slide, "id").unwrap(),
                3 => doc.put(&el, "x", f64::NAN).unwrap(),
                4 => doc.put(&el, "w", -30.).unwrap(),
                5 => doc.put(&slide, "notes", 77).unwrap(),
                6 => doc.put(&el, "bold", "false").unwrap(),
                7 => {
                    let second = map(&doc, &slides, 1);
                    let id = doc
                        .get(&slide, "id")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .unwrap()
                        .to_owned();
                    doc.put(second, "id", id).unwrap();
                }
                8 => {
                    let second = map(&doc, &elements, 1);
                    let id = doc
                        .get(&el, "id")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .unwrap()
                        .to_owned();
                    doc.put(second, "id", id).unwrap();
                }
                9 => doc.put(&el, "font_size", 1e100).unwrap(),
                _ => doc.put(&el, "kind", "surprise").unwrap(),
            }
            let heads = doc.get_heads();
            assert!(
                make_preview(
                    &doc,
                    heads[0],
                    FrozenPreviewAssets::default(),
                    PreviewView::default()
                )
                .is_err(),
                "case {case}"
            );
            assert_eq!(doc.get_heads(), heads);
            assert_eq!(doc.pending_ops(), 0);
        }
    }

    // Case `exact_restore_preserves_unknown_fields_text_marks_and_current_metadata`.
    {
        use automerge::marks::{ExpandMark, Mark};
        let (mut doc, _, _) = fixture();
        let deck = map(&doc, &ROOT, "deck");
        let title = map(&doc, &deck, "title");
        doc.put(&deck, "future_authored_setting", "preserve me")
            .unwrap();
        doc.mark(
            &title,
            Mark::new("strong".into(), true, 0, 4),
            ExpandMark::Both,
        )
        .unwrap();
        let selected = doc.get_heads();
        doc.put(&deck, "future_authored_setting", "live").unwrap();
        doc.unmark(&title, "strong", 0, 4, ExpandMark::Both)
            .unwrap();
        doc.put(ROOT, "runtime_receipt", "current receipt").unwrap();
        let heads = doc.get_heads();
        let plan =
            prepare_content_restore(&doc, &crate::framework_history::SCHEMA, &heads, &selected)
                .unwrap();
        apply_prepared_content_restore(&mut doc, &plan).unwrap();
        assert_eq!(doc.get_changes(&heads).len(), 1);
        let deck = map(&doc, &ROOT, "deck");
        let title = map(&doc, &deck, "title");
        assert_eq!(
            doc.get(deck, "future_authored_setting")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("preserve me")
        );
        assert_eq!(doc.marks(title).unwrap().len(), 1);
        assert_eq!(
            doc.get(ROOT, "runtime_receipt")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("current receipt")
        );
    }

    // Case `absent_version_is_truthful_and_refused_but_empty_restore_undo_redo_remains_empty`.
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "metadata", "exists").unwrap();
        let absent = doc.get_heads()[0];
        let mut deck = crate::model::seed_deck();
        deck.slides.clear();
        crate::doc::SlidesDoc::seed(&mut doc, &deck);
        let empty = doc.get_heads()[0];
        crate::doc::SlidesDoc::seed(&mut doc, &crate::model::seed_deck());
        doc.get_heads();
        let p = make_preview(
            &doc,
            absent,
            FrozenPreviewAssets::default(),
            PreviewView::default(),
        )
        .unwrap();
        assert!(p.accessibility().description.contains("No deck recorded"));
        assert!(
            prepare_content_restore(
                &doc,
                &crate::framework_history::SCHEMA,
                &doc.base_heads(),
                &[absent]
            )
            .is_err()
        );
        let mut driver = managed_driver(doc, empty);
        driver.click_named("history-restore").unwrap();
        driver.key("Enter");
        driver.click_named("history-back-live").unwrap();
        assert!(
            crate::doc::SlidesDoc::load(&driver.state_mut().doc)
                .unwrap()
                .slides
                .is_empty()
        );
        assert!(driver.state_mut().undo(None, None).is_some());
        assert!(
            !crate::doc::SlidesDoc::load(&driver.state_mut().doc)
                .unwrap()
                .slides
                .is_empty()
        );
        assert!(driver.state_mut().redo(None, None).is_some());
        driver.key("Shift");
        assert!(
            crate::doc::SlidesDoc::load(&driver.state_mut().doc)
                .unwrap()
                .slides
                .is_empty()
        );
        let mut reopened = AutoCommit::load(&driver.state_mut().doc.save()).unwrap();
        let heads = reopened.get_heads();
        test_root().sync(&mut reopened, &ROOT);
        assert_eq!(reopened.get_heads(), heads);
        assert!(
            crate::doc::SlidesDoc::load(&reopened)
                .unwrap()
                .slides
                .is_empty()
        );
    }
}

fn snapshot_input(
    doc: &AutoCommit,
    selected: automerge::ChangeHash,
    assets: FrozenPreviewAssets,
    view: PreviewView,
) -> PreviewInput {
    let mut controller = HistoryController::new(
        HistoryResource::new("test", "deck", "so.tilo.slides", ui::RecordId::for_tab(55)),
        doc,
    )
    .unwrap();
    controller
        .select(
            doc,
            &[ui::history_source::entry_id_from_hash(
                &selected.to_string(),
            )],
        )
        .unwrap();
    PreviewInput {
        snapshot: controller.snapshot().unwrap().clone(),
        initial_view: view,
        assets,
    }
}
fn make_preview(
    doc: &AutoCommit,
    selected: automerge::ChangeHash,
    assets: FrozenPreviewAssets,
    view: PreviewView,
) -> Result<Box<dyn HistoryPreview>, String> {
    definition().scopes[0]
        .presentation
        .preview_factory()
        .unwrap()(snapshot_input(doc, selected, assets, view))
}
fn map(
    doc: &AutoCommit,
    parent: &automerge::ObjId,
    key: impl Into<automerge::Prop>,
) -> automerge::ObjId {
    doc.get(parent, key).unwrap().unwrap().1
}

fn png(color: [u8; 4]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba(color)))
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
    out.into_inner()
}
/// Historical images use frozen pixels and the frozen renderer never fetches live blobs.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn frozen_images_and_renderer_never_use_live_state() {
    // Case `historical_image_change_uses_exact_frozen_pixels_missing_and_corrupt_stay_unavailable`.
    {
        use std::{collections::BTreeMap, sync::Arc};
        let old = png([255, 0, 0, 255]);
        let new = png([0, 0, 255, 255]);
        // The frozen preview reads only the assets it is handed; the live asset
        // source below reads this store.
        let content = tilo_content::ContentStore::memory();
        let old_id = crate::assets::store_asset(&content, &old).unwrap();
        let new_id = crate::assets::store_asset(&content, &new).unwrap();
        let mut deck = crate::model::seed_deck();
        deck.slides.truncate(1);
        deck.slides[0].elements = vec![crate::model::Element::image(&old_id, 10., 10., 300., 200.)];
        let mut doc = AutoCommit::new();
        crate::doc::SlidesDoc::seed(&mut doc, &deck);
        let selected = doc.get_heads()[0];
        deck.slides[0].elements[0].src = new_id.clone();
        crate::doc::SlidesDoc::seed(&mut doc, &deck);
        let live = doc.get_heads()[0];
        for (hash, id, bytes, color) in [
            (selected, &old_id, &old, [255, 0, 0, 255]),
            (live, &new_id, &new, [0, 0, 255, 255]),
        ] {
            let assets =
                FrozenPreviewAssets::new(BTreeMap::from([(id.clone(), Arc::from(bytes.clone()))]));
            let mut p = make_preview(&doc, hash, assets, PreviewView::default()).unwrap();
            assert!(p.accessibility().description.contains("Recorded image"));
            assert!(!p.accessibility().description.contains("unavailable"));
            let mut tr = ui::TextRenderer::new();
            let theme = ui::Theme::dark();
            p.layout(vello::kurbo::Rect::new(0., 0., 900., 650.), &mut tr, &theme)
                .unwrap();
            let mut scene = vello::Scene::new();
            p.draw(&mut scene, vello::kurbo::Affine::IDENTITY, &mut tr, &theme);
            assert!(scene.encoding().resources.patches.iter().any(|patch| matches!(patch, vello_encoding::Patch::Image { image, .. } if image.data.data().starts_with(&color))));
        }
        for assets in [
            FrozenPreviewAssets::default(),
            FrozenPreviewAssets::new(BTreeMap::from([(old_id, Arc::from(b"corrupt".as_slice()))])),
            FrozenPreviewAssets::new(BTreeMap::from([(new_id, Arc::from(new))])),
        ] {
            let p = make_preview(&doc, selected, assets, PreviewView::default()).unwrap();
            assert!(
                p.accessibility()
                    .description
                    .contains("Recorded image unavailable")
            );
        }
        // The shared registration resolves recorded images through the app's asset
        // source, reading the content store the app imported them into.
        let registration = HistoryRegistration::new(
            &crate::framework_history::HISTORY,
            vec![HistorySourceRegistration::HostDocument {
                scope: "deck",
                resource: HistoryResource::new(
                    "temporary",
                    "fixture",
                    "so.tilo.slides",
                    ui::RecordId::for_tab(55),
                ),
            }],
        )
        .unwrap()
        .with_asset_source(
            "deck",
            std::rc::Rc::new(crate::framework_history::DeckAssets::new(content)),
        )
        .unwrap();
        let mut driver = ui::headless::Driver::from_managed_root(
            ui::State::from_doc(doc),
            ManagedRoot::new(test_root(), registration),
            (1000, 750),
        )
        .unwrap();
        driver.click_named("history-open").unwrap();
        select_version(&mut driver, selected);
        driver.key("Shift");
        let names = format!("{:?}", driver.a11y_nodes());
        assert!(names.contains("Recorded image"));
        assert!(!names.contains("Recorded image unavailable"));
    }

    // Case `frozen_renderer_never_touches_the_live_content_store`.
    {
        use std::{
            collections::BTreeMap,
            sync::{
                Arc, Mutex,
                atomic::{AtomicUsize, Ordering},
            },
        };
        use tilo_content::{
            ContentBackend, ContentError, ContentReader, ContentStore, MemoryBackend, Stored,
        };
        /// Every access to the live store: writes (announcing content) and reads
        /// (requesting it by address).
        #[derive(Default)]
        struct Probe {
            puts: AtomicUsize,
            opens: Mutex<Vec<String>>,
            local: MemoryBackend,
        }
        struct ProbeBackend(Arc<Probe>);
        impl ContentBackend for ProbeBackend {
            fn put(
                &self,
                reader: &mut dyn std::io::Read,
                mime: &str,
            ) -> Result<Stored, ContentError> {
                self.0.puts.fetch_add(1, Ordering::SeqCst);
                self.0.local.put(reader, mime)
            }
            fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
                self.0.opens.lock().unwrap().push(hash.into());
                self.0.local.open(hash)
            }
            fn has(&self, hash: &str) -> bool {
                self.0.local.has(hash)
            }
        }
        let probe = Arc::new(Probe::default());
        let content = ContentStore::new(ProbeBackend(probe.clone()));
        let mut doc = AutoCommit::new();
        let mut deck = crate::model::seed_deck();
        deck.slides.truncate(1);
        let bytes = png([255, 127, 0, 255]);
        let id = format!("asset:blake3:{}", tilo_content::hash_bytes(&bytes));
        // Bytes exist only in the immutable preview input, never the live store.
        deck.slides[0].elements = vec![crate::model::Element::image(&id, 0., 0., 100., 100.)];
        crate::doc::SlidesDoc::seed(&mut doc, &deck);
        let selected = doc.get_heads()[0];
        for (assets, available) in [
            (FrozenPreviewAssets::default(), false),
            (
                FrozenPreviewAssets::new(BTreeMap::from([(id.clone(), Arc::from(bytes))])),
                true,
            ),
        ] {
            let mut p = make_preview(&doc, selected, assets, PreviewView::default()).unwrap();
            let mut tr = ui::TextRenderer::new();
            let theme = ui::Theme::dark();
            p.layout(vello::kurbo::Rect::new(0., 0., 900., 650.), &mut tr, &theme)
                .unwrap();
            p.draw(
                &mut vello::Scene::new(),
                vello::kurbo::Affine::IDENTITY,
                &mut tr,
                &theme,
            );
            assert_eq!(
                !p.accessibility().description.contains("unavailable"),
                available
            );
        }
        assert!(
            probe.opens.lock().unwrap().is_empty() && probe.puts.load(Ordering::SeqCst) == 0,
            "presentation-only renderer cannot touch the live content store"
        );
        assert!(!content.has(id.strip_prefix(crate::assets::ASSET_PREFIX).unwrap()));
        request_selected_provider(doc, deck, selected, content);
        assert_eq!(
            probe.opens.lock().unwrap().as_slice(),
            &[id.strip_prefix(crate::assets::ASSET_PREFIX)
                .unwrap()
                .to_owned()],
            "one selected read request"
        );
        assert_eq!(probe.puts.load(Ordering::SeqCst), 0, "nothing is stored");
    }
}

/// Navigation at both widths, the real autosaver and admitted resource actions.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn history_navigation_autosave_and_admitted_actions() {
    // Body of case `empty_restore_uses_real_autosaver_and_fresh_process_opener` (a function so its early `return`/`?` stays local).
    fn empty_restore_uses_real_autosaver_and_fresh_process_opener() {
        if let Ok(stage) = std::env::var("TILO_SLIDES_EMPTY_STAGE") {
            let (mut state, mut saver) = ui::persist::try_open("so.tilo.slides").unwrap();
            if stage == "write" {
                let mut deck = crate::model::seed_deck();
                deck.slides.clear();
                crate::doc::SlidesDoc::seed(&mut state.doc, &deck);
                let empty = state.doc.get_heads()[0];
                crate::doc::SlidesDoc::seed(&mut state.doc, &crate::model::seed_deck());
                state.doc.get_heads();
                let mut driver = managed_driver(state.doc, empty);
                driver.click_named("history-restore").unwrap();
                driver.key("Enter");
                driver.click_named("history-back-live").unwrap();
                assert!(
                    crate::doc::SlidesDoc::load(&driver.state_mut().doc)
                        .unwrap()
                        .slides
                        .is_empty()
                );
                saver.request(&mut driver.state_mut().doc);
                saver.try_flush().unwrap();
                std::fs::write(
                    saver.document_path().with_extension("empty-heads"),
                    driver.state_mut().doc.base_heads()[0].to_string(),
                )
                .unwrap();
            } else {
                let heads = state.doc.base_heads();
                test_root().sync(&mut state.doc, &state.root);
                assert_eq!(state.doc.get_heads(), heads);
                assert_eq!(state.doc.pending_ops(), 0);
                assert!(
                    crate::doc::SlidesDoc::load(&state.doc)
                        .unwrap()
                        .slides
                        .is_empty()
                );
                assert_eq!(
                    heads[0].to_string(),
                    std::fs::read_to_string(saver.document_path().with_extension("empty-heads"))
                        .unwrap()
                );
            }
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("slides-empty-history-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for stage in ["write", "read"] {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "framework_history_tests::history_navigation_autosave_and_admitted_actions",
                    "--nocapture",
                ])
                .env("TILO_SLIDES_EMPTY_STAGE", stage)
                .env("XDG_DATA_HOME", &dir)
                .env("XDG_CONFIG_HOME", &dir)
                .env("TILO_PACKAGE_ID", "so.tilo.slides")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{stage}: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            // libtest exits 0 when `--exact` selects nothing; require the stage ran.
            assert!(
                String::from_utf8_lossy(&out.stdout).contains("test result: ok. 1 passed"),
                "{stage}: the child ran no test: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    // Child role: each stage process re-enters this test and runs only that case.
    if std::env::var_os("TILO_SLIDES_EMPTY_STAGE").is_some() {
        empty_restore_uses_real_autosaver_and_fresh_process_opener();
        return;
    }

    // Case `navigation_clamps_effective_scroll_reverses_and_recreates_at_both_widths`.
    {
        let (mut doc, _, _) = fixture();
        crate::doc::SlidesDoc::splice_notes(
            &mut doc,
            0,
            0,
            0,
            &"Historical notes line\n".repeat(120),
        );
        let selected = doc.get_heads()[0];
        let mut tr = ui::TextRenderer::new();
        let theme = ui::Theme::dark();
        for width in [360., 1000.] {
            let bounds = vello::kurbo::Rect::new(0., 0., width, 260.);
            let mut p = make_preview(
                &doc,
                selected,
                FrozenPreviewAssets::default(),
                PreviewView::default(),
            )
            .unwrap();
            let pages = p.accessibility().pages;
            assert!(pages.len() > 1);
            p.navigate(PreviewNavigation::SelectPage(pages[0].0.clone()));
            p.layout(bounds, &mut tr, &theme).unwrap();
            p.navigate(PreviewNavigation::Zoom {
                factor: 2.,
                anchor: vello::kurbo::Point::ZERO,
            });
            p.navigate(PreviewNavigation::Scroll {
                dx: -100_000.,
                dy: -100_000.,
            });
            p.layout(bounds, &mut tr, &theme).unwrap();
            let end = p.view();
            assert!(end.scroll.0 > 0. && end.scroll.1 > 100. && end.scroll.1 < 100_000.);
            let mut scene = vello::Scene::new();
            p.draw(&mut scene, vello::kurbo::Affine::IDENTITY, &mut tr, &theme);
            p.navigate(PreviewNavigation::Scroll { dx: 32., dy: 32. });
            p.layout(bounds, &mut tr, &theme).unwrap();
            let reversed = p.view();
            assert!((end.scroll.1 - reversed.scroll.1 - 32.).abs() < 0.01);
            assert!((end.scroll.0 - reversed.scroll.0 - 32.).abs() < 0.01);
            let mut moved = vello::Scene::new();
            p.draw(&mut moved, vello::kurbo::Affine::IDENTITY, &mut tr, &theme);
            assert!(
                scene
                    .encoding()
                    .resources
                    .glyph_runs
                    .iter()
                    .zip(&moved.encoding().resources.glyph_runs)
                    .any(
                        |(a, b)| (b.transform.translation[1] - a.transform.translation[1] - 32.)
                            .abs()
                            < 0.01
                    ),
                "real rendered text moves with scroll"
            );
            let mut recreated = make_preview(
                &doc,
                selected,
                FrozenPreviewAssets::default(),
                reversed.clone(),
            )
            .unwrap();
            for _ in 0..2 {
                recreated.layout(bounds, &mut tr, &theme).unwrap();
                recreated.draw(
                    &mut vello::Scene::new(),
                    vello::kurbo::Affine::IDENTITY,
                    &mut tr,
                    &theme,
                );
                assert_eq!(recreated.view(), reversed);
            }
            recreated.navigate(PreviewNavigation::NextPage);
            assert_eq!(recreated.view().page.as_ref(), Some(&pages[1].0));
            assert!(recreated.accessibility().description.contains("Slide 2"));
            recreated.navigate(PreviewNavigation::PreviousPage);
            assert_eq!(recreated.view().page.as_ref(), Some(&pages[0].0));
            p.layout(
                vello::kurbo::Rect::new(0., 0., width, 660.),
                &mut tr,
                &theme,
            )
            .unwrap();
            assert!(p.view().scroll.1 < reversed.scroll.1);
            assert!(!p.navigate(PreviewNavigation::Zoom {
                factor: f64::NAN,
                anchor: vello::kurbo::Point::ZERO
            }));
        }
    }

    // Case `empty_restore_uses_real_autosaver_and_fresh_process_opener`.
    empty_restore_uses_real_autosaver_and_fresh_process_opener();

    // Case `successive_admitted_resource_actions_stay_usable_and_deleted_target_is_refused`.
    {
        let queue = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let root = ResourceHarness {
            app: crate::SlidesApp::new(),
            queue: queue.clone(),
        };
        let mut driver = ui::headless::Driver::new(ui::State::new(), Box::new(root), (1000, 750));
        crate::actions::register(driver.action_registry_mut());
        for (choice, expected) in [
            ("theme-rose", crate::model::PaletteKind::Rose),
            ("theme-emerald", crate::model::PaletteKind::Emerald),
        ] {
            let resources = driver.resources();
            let deck = resources
                .iter()
                .find(|r| r.actions.iter().any(|a| a.id == choice))
                .expect("canonical resource remains usable");
            queue.borrow_mut().push(tilo_actions::ControlInvocation {
                address: tilo_actions::ActionAddress::new(deck.id.as_str(), choice),
                arguments: serde_json::Map::default(),
            });
            driver.key("Escape");
            assert_eq!(
                crate::doc::SlidesDoc::load(&driver.state_mut().doc)
                    .unwrap()
                    .palette,
                expected
            );
        }
        let resources = driver.resources();
        let slide = resources
            .iter()
            .flat_map(|r| &r.children)
            .find(|r| r.actions.iter().any(|a| a.id == "duplicate"))
            .unwrap();
        let stale = tilo_actions::ActionAddress::new(slide.id.as_str(), "duplicate");
        let mut empty = crate::doc::SlidesDoc::load(&driver.state_mut().doc).unwrap();
        empty.slides.clear();
        crate::doc::SlidesDoc::seed(&mut driver.state_mut().doc, &empty);
        let heads = driver.state_mut().doc.get_heads();
        queue.borrow_mut().push(tilo_actions::ControlInvocation {
            address: stale,
            arguments: serde_json::Map::default(),
        });
        driver.key("Escape");
        assert_eq!(driver.state_mut().doc.base_heads(), heads);
        assert!(
            crate::doc::SlidesDoc::load(&driver.state_mut().doc)
                .unwrap()
                .slides
                .is_empty()
        );
    }
}

struct ResourceHarness {
    app: crate::SlidesApp,
    queue: std::rc::Rc<std::cell::RefCell<Vec<tilo_actions::ControlInvocation>>>,
}
impl ui::widget::Widget for ResourceHarness {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

    fn sync(&mut self, d: &mut AppModel, p: &ModelPath) {
        self.app.sync(d, p);
    }
    fn build_layout(
        &mut self,
        t: &mut taffy::TaffyTree<()>,
        th: &ui::Theme,
        m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        self.app.build_layout(t, th, m)
    }
    fn apply_layout(&mut self, t: &taffy::TaffyTree<()>, n: taffy::NodeId, p: vello::kurbo::Point) {
        self.app.apply_layout(t, n, p);
    }
    fn draw(
        &mut self,
        s: &mut vello::Scene,
        t: vello::kurbo::Affine,
        tr: &mut ui::TextRenderer,
        th: &ui::Theme,
        p: &ui::presence::PresenceCtx,
    ) {
        self.app.draw(s, t, tr, th, p);
    }
    #[allow(
        clippy::many_single_char_names,
        reason = "test adapter forwards the existing Widget event signature"
    )]
    fn handle_event(&mut self, e: &winit::event::WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.app.handle_event(e, ctx);
    }
    fn name(&self) -> &'static str {
        "Slides resource test"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.app.get_rect()
    }
}

impl ui::resources::PublishesResources for ResourceHarness {
    fn resources(&self, d: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui::resources::resources(&self.app, d)
    }
}

impl ui::widget::controls::ActionControls for ResourceHarness {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.queue.borrow_mut().pop()
    }
}

impl ui::history_mode::capability::DocumentHistory for ResourceHarness {
    fn document_action_applied(
        &mut self,
        a: &tilo_actions::EncodedAction,
        b: &[automerge::ChangeHash],
        d: &AutoCommit,
        p: &automerge::ObjId,
    ) {
        self.app.document_action_applied(a, b, d, p);
    }
}

fn request_selected_provider(
    mut doc: AutoCommit,
    mut deck: crate::model::Deck,
    selected: automerge::ChangeHash,
    content: tilo_content::ContentStore,
) {
    // The trusted provider may request exactly the selected immutable hash.
    // It must not announce content, ask for the current replacement, or edit the document.
    deck.slides[0].elements[0].src = format!(
        "asset:blake3:{}",
        tilo_content::hash_bytes(b"current replacement")
    );
    crate::doc::SlidesDoc::seed(&mut doc, &deck);
    let live_heads = doc.get_heads();
    let registration = HistoryRegistration::new(
        &crate::framework_history::HISTORY,
        vec![HistorySourceRegistration::HostDocument {
            scope: "deck",
            resource: HistoryResource::new(
                "test",
                "test",
                "so.tilo.slides",
                ui::RecordId::for_tab(55),
            ),
        }],
    )
    .unwrap()
    .with_asset_source(
        "deck",
        std::rc::Rc::new(crate::framework_history::DeckAssets::new(content)),
    )
    .unwrap();
    let mut driver = ui::headless::Driver::from_managed_root(
        ui::State::from_doc(doc),
        ManagedRoot::new(Box::new(ui::widget::Column::new()), registration),
        (900, 650),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    select_version(&mut driver, selected);
    driver.key("Shift");
    assert_eq!(driver.state_mut().doc.get_heads(), live_heads);
    assert!(format!("{:?}", driver.a11y_nodes()).contains("Recorded image unavailable"));
}
