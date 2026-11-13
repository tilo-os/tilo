use super::*;
use std::time::Duration;
use taffy::Size;
use ui::search::SearchProvider as _;

fn searched(app: &PdfApp) -> impl ui::search::SearchProvider + use<> {
    ui::framework::testing::search_source::<crate::PdfMarkup>(app.shown_search_entries())
}
use crate::model::Reply;
use serial_test::serial;
use ui::headless::{Framed, viewport};

use tilo_automerge::ApplyIncoming;

#[test]
fn a_note_is_the_window_bars_notice() {
    let mut app =
        ui::framework::testing::TestApp::<crate::PdfViewerShell>::new().expect("the reader opens");
    app.with_surface::<PdfApp, _>(|pdf| {
        pdf.status_note
            .show("Saved annotated PDF to report.pdf".to_owned());
    });
    app.settle();
    assert_eq!(
        ui::window_toolbar::notice().map(|notice| notice.text),
        Some("Saved annotated PDF to report.pdf".to_owned())
    );
    app.with_surface::<PdfApp, _>(|pdf| pdf.status_note.dismiss());
    app.settle();
    assert_eq!(ui::window_toolbar::notice(), None);
}

fn two_documents() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("a.pdf");
    let b = dir.path().join("b.pdf");
    let fixture = concat!(env!("OUT_DIR"), "/sample.pdf");
    std::fs::copy(fixture, &a).expect("copy fixture to a.pdf");
    std::fs::copy(fixture, &b).expect("copy fixture to b.pdf");
    (dir, a, b)
}

fn sync_fixture(app: &mut PdfApp, st: &mut ui::State) {
    app.sync_settled(&mut st.doc, &st.root);
}

pub(super) fn launched(
    path: &std::path::Path,
) -> ui::framework::testing::TestApp<crate::PdfViewerShell> {
    settled(
        ui::framework::testing::TestApp::launched(&[path.to_str().expect("utf-8 path")])
            .expect("the reader launches on the file"),
    )
}

fn settled(
    mut app: ui::framework::testing::TestApp<crate::PdfViewerShell>,
) -> ui::framework::testing::TestApp<crate::PdfViewerShell> {
    settle_document(&mut app);
    app
}

fn settle_document(app: &mut ui::framework::testing::TestApp<crate::PdfViewerShell>) {
    tilo_test_env::wait_until("the launched document to open", || {
        let shown = app
            .with_surface::<PdfApp, _>(|pdf| {
                pdf.has_document
                    && pdf.pending_open.is_none()
                    && pdf.pending_reopen.is_none()
                    && !pdf.seed_pending
            })
            .unwrap_or(false);
        if shown && app.state().imported_source().is_some() {
            return true;
        }
        app.settle();
        false
    });
}

#[test]
fn the_open_pdf_reads_as_its_pages_text() {
    let (_dir, path, _) = two_documents();
    let mut app = launched(&path);
    let read = app
        .effect(&PdfEffect::ReadText {
            from_page: None,
            to_page: Some(1),
        })
        .expect("the text reads")
        .value
        .expect("a read answers");
    assert_eq!(read["pages"][0]["page"], 1);
    assert!(
        !read["pages"][0]["text"].as_str().unwrap().trim().is_empty(),
        "{read}"
    );
    let count = read["page_count"].as_u64().unwrap();
    assert_eq!(
        read["next"],
        if count > 1 {
            serde_json::json!(2)
        } else {
            serde_json::Value::Null
        }
    );
    assert!(
        app.effect(&PdfEffect::ReadText {
            from_page: Some(usize::try_from(count).unwrap() + 1),
            to_page: None,
        })
        .is_err()
    );
}

#[test]
fn a_pinned_pdf_reads_and_is_seen_from_the_content_store_without_a_file() {
    let bytes = std::fs::read(concat!(env!("OUT_DIR"), "/sample.pdf")).expect("the fixture");
    let content = ContentStore::memory();
    let hash = content.put(&bytes).expect("the store keeps the PDF");
    let mut app = PdfApp::empty_with_content(content);
    assert_eq!(
        app.read_text(None, None).unwrap_err(),
        content_import::NO_PDF
    );
    app.record_source = Some(crate::markup::PinnedSource {
        source: ui::tilo_resource::Bytes::kept_as(&bytes, &hash),
        name: "report.pdf".into(),
    });
    let read = app.read_text(None, Some(1)).expect("the pinned PDF reads");
    assert_eq!(read["file"], "report.pdf");
    assert_eq!(read["pages"][0]["page"], 1);
    assert!(
        !read["pages"][0]["text"].as_str().unwrap().trim().is_empty(),
        "{read}"
    );
    let look = app.look_page(1, Some(200)).expect("the page draws");
    let address = look.resources[0].clone();
    let value = look.value.unwrap();
    assert_eq!(value["content"], address.as_str());
    assert_eq!(value["page"], 1);
    assert_eq!(
        value["width"]
            .as_u64()
            .unwrap()
            .max(value["height"].as_u64().unwrap()),
        200
    );
    let picture = app
        .content
        .read(ui::tilo_actions::content_hash(&address).unwrap())
        .expect("the picture is kept as content");
    assert!(picture.starts_with(b"\x89PNG"));
    assert!(app.look_page(0, None).is_err());
    assert!(app.look_page(1, Some(4000)).is_err());
    app.record_source.as_mut().unwrap().source = ui::tilo_resource::Bytes::Blob {
        hash: [0; 32],
        len: u64::try_from(bytes.len()).unwrap(),
        sealed: None,
    };
    assert_eq!(
        app.read_text(None, None).unwrap_err(),
        "report.pdf is not on this device yet"
    );
}

#[test]
fn a_shared_document_not_on_this_device_opens_once_it_arrives() {
    use ui::collaboration::record_content::{ContentLane, RecordContent};
    let bytes = std::fs::read(concat!(env!("OUT_DIR"), "/sample.pdf")).expect("the fixture");
    let hash = tilo_content::hash_bytes(&bytes);
    let lane = ContentLane::of("so.tilo.pdf-viewer.shared-test", "shared-record");
    let content = RecordContent::store(ContentStore::memory(), std::sync::Arc::clone(&lane));
    let mut app = PdfApp::empty_with_content(content);
    app.record_source = Some(crate::markup::PinnedSource {
        source: ui::tilo_resource::Bytes::kept_as(&bytes, &hash),
        name: "shared.pdf".into(),
    });
    let title = |app: &PdfApp| app.source_wait_text().map(|(title, ..)| title);

    app.reopen_pinned_source();
    assert_eq!(app.load_error, None, "nothing failed yet");
    assert_eq!(title(&app), Some(crate::t("pdf-empty-arriving-title")));
    assert_eq!(
        lane.take_requests().wants,
        vec![hash.clone()],
        "the document is asked of the workspace"
    );

    lane.stands(hash.clone(), ContentState::Waiting);
    app.reopen_pinned_source();
    assert_eq!(title(&app), Some(crate::t("pdf-empty-waiting-title")));
    lane.stands(hash.clone(), ContentState::Unavailable);
    app.reopen_pinned_source();
    let (waited, message, error) = app.source_wait_text().unwrap();
    assert_eq!(waited, crate::t("pdf-empty-elsewhere-title"));
    assert!(error && message.contains("shared.pdf"), "{message}");

    lane.arrive(hash, bytes);
    app.reopen_pinned_source();
    assert_eq!(title(&app), None);
    assert!(app.pending_open.is_some(), "the arrived document opens");
    drain_until(&mut app, |app| app.pending_open.is_none());
    if matches!(
        pdfium_gate(app.raster_active(), require_pdfium()),
        PdfiumGate::Skip
    ) {
        return;
    }
    assert!(app.has_document, "{:?}", app.load_error);
}

#[test]
fn a_file_for_another_window_says_where_it_opens() {
    let mut app = PdfApp::empty_with_content(ContentStore::memory());
    app.record_source = Some(crate::markup::PinnedSource {
        source: ui::tilo_resource::Bytes::Blob {
            hash: [0; 32],
            len: 1,
            sealed: None,
        },
        name: "shared.pdf".into(),
    });
    assert_eq!(
        app.another_window_refusal("mine.pdf"),
        "This window shows shared.pdf and its markup. Open mine.pdf in a new PDF Viewer window."
    );
}

fn highlight_on_first_page() -> ui::tilo_resource::Invoke<crate::PdfMarkup> {
    crate::actions::add_annotation_on(
        &Annotation {
            id: "launched-highlight".into(),
            ..Annotation::new(
                1,
                AnnoKind::highlight(PtRect::new(70.0, 150.0, 290.0, 15.0)),
                HighlightColor::Yellow,
                "Ada",
            )
        },
        "source-1",
    )
}

#[serial(pdfium)]
#[test]
fn each_document_keeps_its_markup_in_a_record_of_its_own() {
    eprintln!("phase launching_on_another_document_shows_none_of_the_first");
    {
        let (_dir, a, b) = two_documents();
        let mut app = launched(&a);
        app.invoke(highlight_on_first_page())
            .expect("the owner annotates");
        assert_eq!(app.state().annotations.len(), 1);
        let a_record = app.record().clone();
        assert_eq!(
            app.state().pinned_source().map(|source| source.name),
            Some("a.pdf".to_string()),
            "the record pins the document it marks up"
        );

        let other = settled(app.relaunch(&[b.to_str().unwrap()]).unwrap());
        assert_ne!(other.record(), &a_record, "b.pdf is a record of its own");
        assert!(
            other.state().annotations.is_empty(),
            "a.pdf's markup is not b.pdf's"
        );

        let again = settled(other.relaunch(&[a.to_str().unwrap()]).unwrap());
        assert_eq!(again.record(), &a_record);
        assert_eq!(again.state().annotations.len(), 1, "a.pdf keeps its markup");

        let mut bare = again.reopen().unwrap();
        assert_eq!(bare.record(), &a_record);
        assert!(
            bare.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| {
                    harness.a11y_node_named("Page 1 of 3").is_some()
                }),
            "the pinned document reopens"
        );
    }

    eprintln!("phase the_apps_open_shows_the_chosen_document_on_its_own_record");
    {
        let open = |path: &std::path::Path| PdfEffect::OpenPdf {
            path: Some(path.try_into().expect("an absolute path")),
        };
        let (_dir, a, b) = two_documents();
        let mut app = launched(&a);
        app.invoke(highlight_on_first_page())
            .expect("the owner annotates");
        let a_record = app.record().clone();

        app.effect_confirmed(&open(&b))
            .expect("the app opens another document");
        settle_document(&mut app);
        app.harness().relayout();
        assert_ne!(
            app.record(),
            &a_record,
            "b.pdf shows on a record of its own"
        );
        assert!(app.state().annotations.is_empty(), "without a.pdf's markup");
        assert_eq!(
            app.state().pinned_source().map(|source| source.name),
            Some("b.pdf".to_string())
        );
        let b_record = app.record().clone();

        app.effect_confirmed(&open(&a)).expect("and back");
        settle_document(&mut app);
        app.harness().relayout();
        assert_eq!(app.record(), &a_record);
        assert_eq!(app.state().annotations.len(), 1, "a.pdf kept its markup");

        let missing = a.with_file_name("gone.pdf");
        let failure = app
            .effect_confirmed(&open(&missing))
            .expect_err("a missing document fails the open");
        assert!(
            failure.to_string().contains("gone.pdf"),
            "the operation fails with the reason: {failure}"
        );
        app.harness().relayout();
        assert_eq!(app.record(), &a_record, "the window keeps its record");
        let note = app
            .with_surface::<PdfApp, _>(|pdf| pdf.status_note.get().cloned())
            .flatten()
            .expect("the person is told");
        assert!(note.contains("gone.pdf"), "{note}");

        let mut bare = app.relaunch(&[]).expect("a bare window");
        assert!(
            bare.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| {
                    harness.a11y_node_named("Page 1 of 3").is_some()
                }),
            "the bare window finishes reopening its pinned document"
        );
        bare.effect_confirmed(&open(&b))
            .expect("the bare window opens b.pdf");
        bare.harness().relayout();
        assert_eq!(bare.record(), &b_record);
    }

    eprintln!("phase the_reader_on_its_own_opens_the_chosen_document_itself");
    {
        let (_dir, a, _b) = two_documents();
        let mut app = PdfApp::empty();
        let mut st = crate::scenarios::record_state();
        app.open_chosen(&a);
        settle_record(&mut app, &mut st);
        assert!(app.has_document);
        assert_eq!(
            PdfMarkup::of(&st.doc)
                .pinned_source()
                .map(|source| source.hash()),
            app.source_blob
                .as_ref()
                .map(|source| hex::encode(source.hash())),
            "the window's record now marks up the chosen document"
        );
    }
}

#[serial(pdfium)]
#[test]
fn owner_worker_opens_settle_and_ignore_stale_results() {
    eprintln!("phase owner_worker_failure_settles_the_matching_open_without_blocking");
    {
        let dir = tempfile::tempdir().expect("temporary PDFs");
        let corrupt = dir.path().join("corrupt.pdf");
        std::fs::write(&corrupt, b"%PDF-1.7\nnot a readable document")
            .expect("write corrupt fixture");
        for path in [dir.path().join("missing.pdf"), corrupt] {
            let mut app = PdfApp::empty();
            let state = AutoCommit::new();
            app.queue_worker_open(&path);
            assert!(app.pending_open.is_some(), "accepted request is loading");
            tilo_test_env::wait_until("the worker failure to settle loading", || {
                app.drain_raster_events(&state);
                app.pending_open.is_none()
            });
            assert!(!app.has_document);
            assert!(app.load_error.as_deref().is_some_and(|error| {
                error.contains("Could not open") && error.contains(&path.display().to_string())
            }));
            let tree = app.accessibility().expect("failed-open accessibility");
            let detail = app
                .open_failure_text()
                .expect("the empty state says why the open failed");
            assert!(
                any_access_node(&tree, &|node| node.name.as_deref() == Some(detail.as_str())),
                "the words on screen are the words read out: {detail:?}"
            );
        }
    }

    eprintln!("phase owner_worker_stale_open_result_cannot_replace_the_latest_request");
    {
        let mut app = PdfApp::empty();
        let state = AutoCommit::new();
        let first = std::path::Path::new("/definitely/not/a/tilo-pdf-a.pdf");
        let latest = std::path::Path::new("/definitely/not/a/tilo-pdf-b.pdf");
        app.queue_worker_open(first);
        app.queue_worker_open(latest);
        tilo_test_env::wait_until("the latest open to settle", || {
            app.drain_raster_events(&state);
            app.pending_open.is_none()
        });
        assert!(
            app.load_error
                .as_deref()
                .is_some_and(|error| error.contains("tilo-pdf-b.pdf"))
        );
    }
}

#[serial(pdfium)]
#[test]
fn markup_follows_the_file_through_rename_and_move_but_not_a_copy() {
    let dir = tempfile::tempdir().expect("temp dir");
    let original = dir.path().join("paper.pdf");
    let fixture = concat!(env!("OUT_DIR"), "/latin.pdf");
    std::fs::copy(fixture, &original).expect("copy the Typst fixture");

    let mut app = launched(&original);
    app.invoke(highlight_on_first_page())
        .expect("the owner annotates");
    let markup = app.record().clone();

    let moved_dir = dir.path().join("reviewed");
    std::fs::create_dir(&moved_dir).unwrap();
    let moved = moved_dir.join("paper (final).pdf");
    std::fs::rename(&original, &moved).unwrap();
    let reopened = settled(app.relaunch(&[moved.to_str().unwrap()]).unwrap());
    assert_eq!(reopened.record(), &markup);
    assert_eq!(
        reopened.state().annotations.len(),
        1,
        "the renamed file shows its annotations"
    );

    let copy = dir.path().join("paper copy.pdf");
    std::fs::copy(&moved, &copy).unwrap();
    let copied = settled(reopened.relaunch(&[copy.to_str().unwrap()]).unwrap());
    assert_ne!(copied.record(), &markup);
    assert!(
        copied.state().annotations.is_empty(),
        "a copy of the file starts with no markup"
    );
}

fn resolve(app: &PdfApp, doc: &AutoCommit, id: &str) -> Option<tilo_actions::ResourceSpec> {
    fn find(resource: &tilo_actions::ResourceSpec, id: &str) -> Option<tilo_actions::ResourceSpec> {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        resource.children.iter().find_map(|child| find(child, id))
    }
    let state = <crate::PdfMarkup as ui::tilo_resource::Resource>::read(doc).ok()?;
    let published = ui::tilo_resource::publish::<crate::PdfMarkup>("pdf.document", "PDF", &state);
    find(&published, id)
        .or_else(|| <crate::PdfMarkup as ui::tilo_resource::Resource>::child(&state, id))
        .or_else(|| {
            app.document_row(id)
                .map(ui::framework::LiveRow::into_resource)
        })
}

#[test]
fn a_scanned_pdf_is_known_to_have_no_text() {
    let (mut app, _st) = app_with_state();
    let pages = app.doc.page_count();
    assert!(pages > 1);
    assert!(!app.document_has_no_text(), "layers still on the way");
    for page in 1..=pages {
        app.raster_text.insert(page, Arc::new(PageText::default()));
    }
    assert!(app.document_has_no_text(), "every page is a picture");
    app.raster_text.insert(
        1,
        Arc::new(PageText::from_glyphs([(
            'A',
            PtRect::new(0.0, 0.0, 5.0, 5.0),
        )])),
    );
    assert!(
        !app.document_has_no_text(),
        "one page with words is searchable"
    );
}

#[test]
fn with_no_document_the_zoom_reads_100_percent_at_any_window_size() {
    for (w, h) in [(1440.0, 900.0), (1708.0, 1400.0)] {
        let mut app = PdfApp::empty();
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.recompute_regions();
        app.sync_viewport_frame();
        assert!(!app.has_document);
        assert!((app.shown_zoom_percent() - 100.0).abs() < f64::EPSILON);
    }
    let (mut app, _st) = app_with_state();
    assert!((app.shown_zoom_percent() - (app.zoom() * 100.0).round()).abs() < f64::EPSILON);

    app.follow_page_line();
    assert!(!app.page_notice.is_shown(), "opening is not a change");
    let line = app.page_line();
    assert!(line.ends_with(&format!("{:.0}%", (app.zoom() * 100.0).round())));
    app.zoom_by(1.5);
    app.follow_page_line();
    let said = app.page_notice.get().cloned().expect("a zoom says itself");
    assert_ne!(said, line, "the line follows the zoom");
    assert!(said.ends_with(&format!("{:.0}%", (app.zoom() * 100.0).round())));
}

pub(super) fn staged_empty(scenario: &str) -> PdfApp {
    let mut app = PdfApp::empty();
    app.stage(Some(scenario));
    app
}

pub(super) fn app_with_state() -> (PdfApp, ui::State) {
    let mut app = crate::scenarios::sample_reader();
    let mut st = crate::scenarios::sample_state();
    sync_fixture(&mut app, &mut st);
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.recompute_regions();
    app.sync_viewport_frame();
    (app, st)
}

#[test]
fn opening_seeds_the_record_once_not_every_frame() {
    let mut app = crate::scenarios::sample_reader();
    let mut st = crate::scenarios::record_state();
    sync_fixture(&mut app, &mut st);
    assert!(
        PdfMarkup::of(&st.doc).page_plan_seeded,
        "opening adds the page plan"
    );
    let seeded = st.doc.get_heads();
    for _ in 0..3 {
        sync_fixture(&mut app, &mut st);
    }
    assert_eq!(st.doc.get_heads(), seeded, "frames write nothing");

    app.set_read_only(true);
    app.set_read_only(false);
    sync_fixture(&mut app, &mut st);
    assert_eq!(
        st.doc.get_heads(),
        seeded,
        "a record that has its plan gets nothing more when writing is allowed again"
    );
}

pub(super) fn any_access_node(
    node: &ui::a11y::AccessNode,
    predicate: &impl Fn(&ui::a11y::AccessNode) -> bool,
) -> bool {
    predicate(node)
        || node
            .children
            .iter()
            .any(|child| any_access_node(child, predicate))
}

#[allow(
    clippy::too_many_lines,
    reason = "this lifecycle test intentionally verifies canonical page, bookmark, markup, reply, and find-result addresses together"
)]
#[serial(pdfium)]
#[test]
fn semantic_targets_and_global_search_address_stable_pages() {
    eprintln!("phase accessibility_exposes_reader_state_and_routes_page_stepping");
    {
        use ui::a11y::Action;
        use ui::widget::Widget as _;

        let (mut app, mut state) = app_with_state();
        let tree = app.accessibility().expect("reader semantics");
        assert_eq!(tree.role, ui::a11y::Role::Window);
        assert!(any_access_node(&tree, &|node| node
            .name
            .as_deref()
            .is_some_and(|value| value.contains("Page"))));
        app.go_to_page(1);
        assert!(
            app.bar_entry("Next page").enabled,
            "Next page is the window bar's"
        );

        app.go_to_page(1);
        assert!(app.perform_semantic_action(
            "pdf.next-page",
            &Action::Activate,
            &mut Edits::lend(&mut state.doc),
            &state.root,
        ));
        assert_eq!(app.current_page, 2, "Next page steps through the document");
    }

    eprintln!("phase stable_page_semantic_target_follows_a_reordered_page");
    {
        use ui::a11y::Action;

        let (mut app, mut state) = app_with_state();
        let target_id = app.resource_page_ids[0].clone();
        let last = app.resource_page_ids[2].clone();
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            crate::actions::move_page_plan_action(&target_id, Some(&last)),
        );
        app.sync_settled(&mut state.doc, &state.root);
        assert_eq!(app.resource_page_ids[2], target_id);

        assert!(app.perform_semantic_action(
            &format!("pdf.page:{target_id}"),
            &Action::Activate,
            &mut Edits::lend(&mut state.doc),
            &state.root,
        ));
        assert_eq!(app.current_page, 3, "the addressed page, not ordinal 1");
        assert!(
            !app.perform_semantic_action(
                "pdf.page:deleted-page",
                &Action::Activate,
                &mut Edits::lend(&mut state.doc),
                &state.root,
            ),
            "unknown targets fail closed"
        );
    }

    eprintln!("phase global_search_pages_canonically_and_stale_page_hits_fail_closed");
    'phase: {
        let Some((mut app, mut state)) = open_sample_review() else {
            break 'phase;
        };
        let provider = searched(&app);
        assert!(
            provider
                .search_page(&ui::search::SearchQuery::new(""), None, 0, 1)
                .results
                .is_empty()
        );
        let first = provider.search_page(&ui::search::SearchQuery::new(""), None, 1, 1);
        assert_eq!(first.inspected, 1);
        let stale_cursor = first.next.expect("catalogue paging stays bounded");
        let item = |field: &str, id: &str| {
            ui::tilo_resource::items::item_resource_id::<PdfMarkup>(
                field,
                &ui::tilo_resource::Id::from_raw(id),
            )
        };
        let page_prefix = item("pages", "");
        let hit = provider
            .search("spotting scope")
            .into_iter()
            .map(|result| result.anchor())
            .find(|anchor| anchor.starts_with(&page_prefix))
            .expect("page text is globally searchable at its page");
        assert!(resolve(&app, &state.doc, &hit).is_some());

        let markup = <crate::PdfMarkup as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the record reads");
        let entries = <crate::PdfMarkup as ui::tilo_resource::Resource>::search(&markup);
        let markup_hit = |needle: &str| {
            entries
                .iter()
                .find(|entry| {
                    let line = format!("{} {}", entry.title, entry.text).to_lowercase();
                    line.contains(&needle.to_lowercase())
                })
                .and_then(|entry| entry.anchor.clone())
                .unwrap_or_else(|| panic!("{needle:?} should produce a search hit at its item"))
        };
        for (needle, delete) in [
            ("highlight", "delete-annotation"),
            ("species", "delete-comment"),
        ] {
            let anchor = markup_hit(needle);
            let resource = resolve(&app, &state.doc, &anchor)
                .expect("search door resolves the anchored annotation/comment item");
            assert!(resource.actions.iter().any(|action| action.id == delete));
        }
        let reply_anchor = markup_hit("northern harrier");
        assert!(
            reply_anchor.starts_with(&item("comments", "")),
            "a reply is found in its thread"
        );
        assert!(
            resolve(&app, &state.doc, &reply_anchor).is_some(),
            "a reply search hit resolves to the durable thread"
        );

        app.stage_find_for_test("godwit");
        app.sync_settled(&mut state.doc, &state.root);
        let find_provider = searched(&app);
        let find_address = find_provider
            .search("match 1")
            .into_iter()
            .filter_map(|result| result.action_address())
            .find(|address| address.resource.starts_with("pdf.search-result:"))
            .expect("bounded find result has a canonical global-search action");
        let find_resource = resolve(&app, &state.doc, &find_address.resource)
            .expect("current find-result address resolves");
        assert!(
            find_resource
                .actions
                .iter()
                .any(|action| action.id == "go-to-match")
        );
        app.stage_find_for_test("tide");
        assert!(
            resolve(&app, &state.doc, &find_address.resource).is_none(),
            "a stale find ordinal cannot be rebound to the next query"
        );
        app.sync_settled(&mut state.doc, &state.root);
        let refreshes = app.search_projection_refreshes;
        app.sync_settled(&mut state.doc, &state.root);
        assert_eq!(
            app.search_projection_refreshes, refreshes,
            "unchanged sync avoids corpus traversal"
        );

        let stale_resource = item("pages", "source-1");
        let action = crate::actions::delete_page_plan_action("source-1");
        app.perform_action(&mut Edits::lend(&mut state.doc), action);
        app.sync_settled(&mut state.doc, &state.root);
        let restarted = searched(&app).search_page(
            &ui::search::SearchQuery::new(""),
            Some(&stale_cursor),
            1,
            1,
        );
        assert!(
            restarted.results.len() <= 1,
            "a stale cursor restarts against the new generation"
        );
        assert!(resolve(&app, &state.doc, &stale_resource).is_none());
    }
}

#[serial(pdfium)]
#[test]
fn duplicate_pages_are_found_by_their_sources_text() {
    let Some((mut app, mut state)) = open_sample_review() else {
        return;
    };
    let pages = |app: &PdfApp| -> Vec<String> {
        searched(app)
            .search("Dunlin")
            .into_iter()
            .map(|result| result.anchor())
            .collect()
    };
    assert!(
        pages(&app)
            .iter()
            .any(|anchor| anchor.ends_with("/source-2")),
        "page 2 is found by its text"
    );
    app.perform_action(
        &mut Edits::lend(&mut state.doc),
        PdfMarkup::DUPLICATE_PAGE.with((
            ui::tilo_resource::Id::from_raw("source-2"),
            Some("copy-2".to_owned()),
        )),
    );
    app.sync_settled(&mut state.doc, &state.root);
    let found = pages(&app);
    assert!(
        found.iter().any(|anchor| anchor.ends_with("/source-2"))
            && found.iter().any(|anchor| anchor.ends_with("/copy-2")),
        "duplicate occurrences retain the source page's searchable text: {found:?}"
    );
}

#[serial(pdfium)]
#[test]
fn pdf_bookmarks_are_globally_searchable() {
    let Some(mut app) = open_settled(concat!(env!("OUT_DIR"), "/multipage.pdf")) else {
        return;
    };
    let mut state = crate::scenarios::record_state();
    settle_record(&mut app, &mut state);
    assert!(!app.doc.outline.is_empty(), "the fixture has bookmarks");
    let provider = searched(&app);
    let bookmark_address = provider
        .search("Second Chapter")
        .into_iter()
        .filter_map(|result| result.action_address())
        .find(|address| address.resource.starts_with("pdf.outline:"))
        .expect("PDF bookmarks are globally searchable by their canonical action");
    let bookmark_resource = resolve(&app, &state.doc, &bookmark_address.resource)
        .expect("bookmark search result resolves to the current outline resource");
    assert!(
        bookmark_resource
            .actions
            .iter()
            .any(|action| action.id == bookmark_address.action)
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "this integration test intentionally keeps replay, convergence, resource, and undo assertions in one lifecycle"
)]
#[test]
fn shared_page_plan_keeps_markup_and_addresses_through_edits() {
    eprintln!("phase shared_page_plan_replays_merges_undoes_and_keeps_addresses_stable");
    {
        use crate::actions::{delete_page_plan_action, move_page_plan_action};
        use ui::tilo_resource::{Id, Invoke, Replica};

        let author_and_replay =
            |a: &mut AutoCommit, b: &mut AutoCommit, invoke: Invoke<PdfMarkup>| {
                let mut author = Replica::<PdfMarkup>::from_doc(std::mem::take(a));
                let mut peer = Replica::<PdfMarkup>::from_doc(std::mem::take(b));
                let envelope = author.apply(invoke).expect("author").clone();
                peer.replay(&envelope).expect("replay");
                *a = std::mem::take(author.doc_mut());
                *b = std::mem::take(peer.doc_mut());
            };
        let duplicate = |id: &str, copy: &str| {
            PdfMarkup::DUPLICATE_PAGE.with((Id::from_raw(id), Some(copy.to_owned())))
        };

        let (mut a, mut state_a) = app_with_state();
        state_a.doc.commit();
        let base = state_a.doc.save();
        let mut state_b = crate::scenarios::record_state();
        state_b.doc = AutoCommit::load(&base).expect("clone shared base");
        let (mut b, _) = app_with_state();

        author_and_replay(
            &mut state_a.doc,
            &mut state_b.doc,
            duplicate("source-2", "copy-of-source-2"),
        );
        author_and_replay(
            &mut state_a.doc,
            &mut state_b.doc,
            move_page_plan_action("source-1", Some("copy-of-source-2")),
        );
        assert_eq!(
            PdfMarkup::of(&state_a.doc).page_plan(),
            PdfMarkup::of(&state_b.doc).page_plan(),
            "the exact same action envelopes reproduce durable ids and order"
        );

        a.sync_settled(&mut state_a.doc, &state_a.root);
        b.sync_settled(&mut state_b.doc, &state_b.root);
        assert_eq!(a.resource_page_ids, b.resource_page_ids);
        let page_id = "source-1";
        let resource_id = ui::tilo_resource::items::item_resource_id::<PdfMarkup>(
            "pages",
            &Id::from_raw(page_id),
        );
        assert!(
            resolve(&a, &state_a.doc, &resource_id).is_some(),
            "the durable page id resolves through the provider"
        );
        assert_eq!(
            a.resource_page_ids
                .iter()
                .position(|id| id == page_id)
                .map(|index| index + 1),
            Some(3),
            "the stable page id moved while retaining its address"
        );

        author_and_replay(
            &mut state_a.doc,
            &mut state_b.doc,
            delete_page_plan_action(page_id),
        );
        a.sync_settled(&mut state_a.doc, &state_a.root);
        b.sync_settled(&mut state_b.doc, &state_b.root);
        assert!(resolve(&a, &state_a.doc, &resource_id).is_none());
        assert!(resolve(&b, &state_b.doc, &resource_id).is_none());

        let mut left = AutoCommit::load(&base).expect("left replica");
        let mut right = AutoCommit::load(&base).expect("right replica");
        PdfMarkup::apply_to(&mut left, duplicate("source-1", "copy-left"));
        PdfMarkup::apply_to(&mut right, duplicate("source-3", "copy-right"));
        left.merge_incoming(&mut right)
            .expect("merge concurrent page edits");
        let merged_ids: Vec<String> = PdfMarkup::of(&left)
            .page_plan()
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert!(merged_ids.contains(&"copy-left".to_string()));
        assert!(merged_ids.contains(&"copy-right".to_string()));

        let mut undo_app =
            ui::framework::testing::TestApp::<crate::PdfViewerShell>::new().expect("the reader");
        undo_app
            .invoke(PdfMarkup::START_PAGE_PLAN.with(3))
            .expect("the plan");
        let before = undo_app.state().page_plan();
        undo_app
            .invoke(duplicate("source-1", "copy-undo"))
            .expect("the owner duplicates a page");
        assert_eq!(undo_app.state().page_plan().len(), before.len() + 1);
        assert!(undo_app.undo(), "shared page action is an undo step");
        assert_eq!(undo_app.state().page_plan(), before);
    }

    eprintln!("phase imported_occurrence_keeps_markup_through_move_duplicate_and_delete");
    {
        use crate::actions::{add_annotation_on, move_page_plan_action};
        use crate::markup::{ImportedPage, PagePlanEntry};

        let (mut app, mut state) = app_with_state();
        let imported = ImportedPage {
            hash: "c".repeat(64),
            kind: "image".into(),
            page: 1,
            width_milli: 612_000,
            height_milli: 408_000,
        };
        let page = PagePlanEntry {
            id: "imported-occurrence".into(),
            source_page: 1,
            imported: Some(imported.clone()),
            rotation: 0,
        };
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            crate::actions::import_pages_action(Some("source-1"), vec![page.clone()]),
        );
        app.sync_settled(&mut state.doc, &state.root);
        assert_eq!(app.resource_page_ids[1], "imported-occurrence");
        assert_eq!(
            app.status_note.as_deref(),
            Some(crate::t("pdf-note-imported-unshown").as_str()),
            "the reader says it cannot show the imported page"
        );

        let annotation = Annotation {
            id: "annotation-on-import".into(),
            ..Annotation::new(
                2,
                AnnoKind::Note { x: 12.0, y: 24.0 },
                HighlightColor::Yellow,
                "You",
            )
        };
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            add_annotation_on(&annotation, "imported-occurrence"),
        );
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            move_page_plan_action("imported-occurrence", Some("source-3")),
        );
        app.sync_settled(&mut state.doc, &state.root);
        assert_eq!(
            app.annotations
                .iter()
                .find(|annotation| annotation.id == "annotation-on-import")
                .map(|annotation| annotation.page),
            Some(4),
            "anchor follows the imported occurrence"
        );

        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            PdfMarkup::DUPLICATE_PAGE.with((
                ui::tilo_resource::Id::from_raw(page.id),
                Some("imported-copy".to_owned()),
            )),
        );
        assert_eq!(
            PdfMarkup::of(&state.doc)
                .page_plan()
                .into_iter()
                .find(|entry| entry.id == "imported-copy")
                .and_then(|entry| entry.imported),
            Some(imported),
            "duplicate retains content hash and source page"
        );
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            crate::actions::delete_page_plan_action("imported-occurrence"),
        );
        app.sync_settled(&mut state.doc, &state.root);
        assert!(
            app.annotations
                .iter()
                .all(|annotation| annotation.id != "annotation-on-import"),
            "deleting one occurrence removes only its markup"
        );
        assert!(app.resource_page_ids.iter().any(|id| id == "imported-copy"));
    }
}

#[serial(pdfium)]
#[test]
fn worker_reader_says_when_imported_page_bytes_are_missing() {
    let Some(mut app) = open_raster_sample() else {
        return;
    };
    let mut state = crate::scenarios::record_state();
    settle_record(&mut app, &mut state);
    app.perform_action(
        &mut Edits::lend(&mut state.doc),
        crate::actions::import_pages_action(
            Some("source-1"),
            vec![crate::markup::PagePlanEntry {
                id: "missing-import".into(),
                source_page: 1,
                imported: Some(crate::markup::ImportedPage {
                    hash: "d".repeat(64),
                    kind: "image".into(),
                    page: 1,
                    width_milli: 612_000,
                    height_milli: 408_000,
                }),
                rotation: 0,
            }],
        ),
    );
    app.sync_settled(&mut state.doc, &state.root);
    assert!(
        app.status_note.as_deref().is_some_and(|note| [
            "pdf-note-imported-arriving",
            "pdf-note-imported-waiting",
            "pdf-note-imported-unavailable",
        ]
        .into_iter()
        .any(|key| note == crate::t(key))),
        "{:?}",
        app.status_note
    );
    assert!(
        app.pending_worker_plan.is_none(),
        "a plan whose bytes are missing is not sent to the owner"
    );
}

#[serial(pdfium)]
#[test]
fn shot_drivers_render_every_review_and_product_state() {
    eprintln!("phase drive_import_unavailable_shot");
    {
        let mut app = crate::scenarios::sample_reader();
        let mut state = crate::scenarios::record_state();
        app.sync_settled(&mut state.doc, &state.root);
        app.perform_action(
            &mut Edits::lend(&mut state.doc),
            crate::actions::import_pages_action(
                Some("source-1"),
                vec![crate::markup::PagePlanEntry {
                    id: "review-unavailable-import".into(),
                    source_page: 1,
                    imported: Some(crate::markup::ImportedPage {
                        hash: "d".repeat(64),
                        kind: "image".into(),
                        page: 1,
                        width_milli: 612_000,
                        height_milli: 408_000,
                    }),
                    rotation: 0,
                }],
            ),
        );
        let app = framed(
            app,
            state,
            1280.0,
            800.0,
            Some("/tmp/pdf_viewer_release_shots/import_unavailable.png"),
        );
        assert_eq!(
            app.status_note.as_deref(),
            Some(crate::t("pdf-note-imported-unshown").as_str()),
            "a missing imported blob is never rendered as a source page"
        );
    }

    eprintln!("phase drive_review_zoom_pan_annotate_shots");
    'phase: {
        let Some(app) = open_raster_sample() else {
            break 'phase;
        };

        let mut app = framed_at_zoom(
            app,
            crate::scenarios::record_state(),
            0.5,
            Some("/tmp/pdf_repro/zoom_50.png"),
        );
        let w50 = app.pw() * app.zoom();
        let w100 = set_zoom_and_frame(&mut app, 1.0, Some("/tmp/pdf_repro/zoom_100.png"));
        let w200 = set_zoom_and_frame(&mut app, 2.0, Some("/tmp/pdf_repro/zoom_200.png"));
        eprintln!("DRIVE page-card width px: 50%={w50:.1}  100%={w100:.1}  200%={w200:.1}");
        assert!(
            (w100 - 2.0 * w50).abs() < 1.0,
            "100% page must be twice the 50% page"
        );
        assert!(
            (w200 - 2.0 * w100).abs() < 1.0,
            "200% page must be twice the 100% page"
        );
        app.go_to_page(1);
        let scroll_y = app.scroll_y();
        app.viewport.set_scroll(0.0, scroll_y);
        full_frame(
            &mut app,
            1280.0,
            800.0,
            Some("/tmp/pdf_repro/zoom_200_page_top.png"),
        );
        assert_eq!(app.nearest_page(), 1, "page-top zoom view remains usable");
        assert!(
            app.scroll_x().abs() < f64::EPSILON,
            "review frame starts at the leading column"
        );

        let before = app.zoom();
        let c = app.canvas_rect.center();
        app.zoom_at(1.0 + 56.0 / 240.0, Point::new(c.x, c.y));
        assert!(
            app.zoom() > before + 1e-3,
            "ctrl+wheel-equivalent zoom must change scale"
        );

        app.fit = FitMode::Custom(2.0);
        app.viewport.set_zoom(2.0);
        full_frame(&mut app, 1280.0, 800.0, None);
        assert!(
            app.is_overflowing_x(),
            "200% must overflow the viewport horizontally"
        );
        let sx0 = app.scroll_x();
        app.viewport.pan_by(240.0, 0.0); // arrow/drag-equivalent rightward pan
        let sx1 = app.scroll_x();
        assert!(
            sx1 > sx0 + 1.0,
            "panning right must increase scroll_x ({sx0} -> {sx1})"
        );
        full_frame(
            &mut app,
            1280.0,
            800.0,
            Some("/tmp/pdf_repro/panned_200.png"),
        );
        eprintln!(
            "DRIVE pan: scroll_x {sx0:.1} -> {sx1:.1} (max {:.1})",
            app.viewport.max_scroll_x()
        );

        app.fit = FitMode::Page;
        let fit_page = app.fit_page_zoom();
        app.viewport.set_zoom(fit_page);
        full_frame(&mut app, 1280.0, 800.0, None);
        let a0 = app.annotations.len();
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (70.0, 150.0),
            cur: (360.0, 165.0),
            points: vec![],
        });
        let (reader, st) = Framed::split(&mut app);
        reader.commit_drag(&mut Edits::lend(&mut st.doc));
        assert!(
            app.annotations.len() > a0,
            "a highlight drag adds an annotation"
        );
        app.go_to_page(1); // bring the freshly-placed highlight into view
        eprintln!(
            "DRIVE annotate scroll_y after go_to_page(1) = {:.1}",
            app.scroll_y()
        );
        full_frame(
            &mut app,
            1280.0,
            800.0,
            Some("/tmp/pdf_repro/annotated.png"),
        );
        eprintln!(
            "DRIVE annotate: {} -> {} annotations; scroll_y after frame = {:.1}",
            a0,
            app.annotations.len(),
            app.scroll_y()
        );
    }

    eprintln!("phase drive_review_features_shots");
    {
        let app = crate::scenarios::sample_reader();
        let st = crate::scenarios::sample_state();

        let app = framed(
            app,
            st,
            1280.0,
            800.0,
            Some("/tmp/pdf_repro/sample_markup.png"),
        );
        assert!(
            !app.annotations
                .iter()
                .any(|a| matches!(a.kind, AnnoKind::Ink { .. })),
            "no seeded ink 'emphasis loop' should remain"
        );
        assert!(
            app.annotations
                .iter()
                .any(|a| matches!(a.kind, AnnoKind::Markup { .. })),
            "seeded highlights remain"
        );

        eprintln!("DRIVE features: {} annotations", app.annotations.len());
    }

    eprintln!("phase drive_product_state_matrix_shots");
    {
        let shot_dir = std::env::var("PDF_SHOT_DIR")
            .unwrap_or_else(|_| "/tmp/tilo_pdf_product_states".to_string());
        let shot = |name: &str| format!("{shot_dir}/{name}");

        if let Some((narrow, narrow_state)) = open_sample_review() {
            let mut narrow = framed(
                narrow,
                narrow_state,
                390.0,
                844.0,
                Some(&shot("narrow-document.png")),
            );
            assert!(narrow.overlay_mode, "compact viewport uses overlay panels");

            narrow.stage_find_for_test("godwit");
            full_frame(&mut narrow, 390.0, 844.0, Some(&shot("narrow-search.png")));
            assert!(narrow.match_count() > 0, "find remains usable when compact");
        }

        let loading = staged_empty("loading");
        let loading_state = crate::scenarios::record_state();
        let loading = framed(
            loading,
            loading_state,
            1280.0,
            800.0,
            Some(&shot("loading.png")),
        );
        let loading_a11y = loading.accessibility().expect("loading semantics");
        assert!(
            !any_access_node(&loading_a11y, &|node| {
                node.action_target.as_deref() == Some("pdf.open")
            }),
            "a pending open is progress, not a duplicate Open action"
        );
        let error = staged_empty("error");
        let error_state = crate::scenarios::record_state();
        let error = framed(error, error_state, 1280.0, 800.0, Some(&shot("error.png")));
        assert!(
            error.canvas_rect.height() > 700.0,
            "the persistent error keeps the full empty-state canvas"
        );
        let narrow_error = staged_empty("error");
        let narrow_error_state = crate::scenarios::record_state();
        framed(
            narrow_error,
            narrow_error_state,
            390.0,
            844.0,
            Some(&shot("narrow-error.png")),
        );

        let contrast = crate::scenarios::sample_reader();
        let contrast_state = crate::scenarios::sample_state();
        framed_with_theme(
            contrast,
            contrast_state,
            1280.0,
            800.0,
            Some(&shot("high-contrast.png")),
            ui::theme::Theme::high_contrast(),
        );

        let mut reduced = crate::scenarios::sample_reader();
        let reduced_state = crate::scenarios::sample_state();
        reduced.arm_shots_page_bubble();
        reduced.set_motion_reduced(true);
        let reduced = framed(
            reduced,
            reduced_state,
            390.0,
            844.0,
            Some(&shot("reduced-motion.png")),
        );
        assert!(
            !reduced.needs_animation_frame(),
            "reduced motion settles the page bubble without a timer loop"
        );
    }

    eprintln!("phase drive_real_pdf_search_highlight_save_shots");
    'phase: {
        let shot_dir = std::env::var("PDF_SHOT_DIR").unwrap_or_else(|_| {
            std::env::temp_dir()
                .join("tilo_pdf_review")
                .to_string_lossy()
                .into_owned()
        });
        let shot = |name: &str| format!("{shot_dir}/{name}");

        let Some(app) = open_raster_sample() else {
            break 'phase;
        };
        let mut app = framed(app, crate::scenarios::record_state(), 1280.0, 800.0, None);

        let word = {
            let (reader, st) = Framed::split(&mut app);
            let layer = text_layer(reader, st, 1);
            first_word(&layer.text, 4).expect("a searchable word")
        };
        app.stage_find_for_test(&word);
        {
            let (reader, st) = Framed::split(&mut app);
            settle_until(reader, st, |app| app.raster_text_pending.is_empty());
        }
        assert!(
            !app.raster_matches.is_empty(),
            "search finds {word:?} on the real PDF"
        );
        let p_search = shot("real_search_hit.png");
        full_frame(&mut app, 1280.0, 800.0, Some(p_search.as_str()));
        ui::widget::Findable::clear_find(&mut *app);

        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (70.0, 150.0),
            cur: (360.0, 165.0),
            points: vec![],
        });
        let (reader, st) = Framed::split(&mut app);
        reader.commit_drag(&mut Edits::lend(&mut st.doc));
        reader.signer_name = "Grace Hopper".into();
        reader.place_signature(&mut Edits::lend(&mut st.doc), 1, (360.0, 520.0));
        app.go_to_page(1);
        let p_anno = shot("real_annotated.png");
        full_frame(&mut app, 1280.0, 800.0, Some(p_anno.as_str()));

        let out = std::env::temp_dir()
            .join("tilo_pdf_review")
            .join("annotated.pdf");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let (reader, _) = Framed::split(&mut app);
        assert!(
            save_settled(reader, &out),
            "save succeeds: {:?}",
            reader.status_note
        );

        let app2 = open_settled(&out).expect("the saved file reopens via pdfium");
        let p_reopen = shot("real_reopened_saved.png");
        framed(
            app2,
            crate::scenarios::record_state(),
            1280.0,
            800.0,
            Some(p_reopen.as_str()),
        );
        let signed = crate::source::open(&out, &ContentStore::memory())
            .expect("the saved file reopens")
            .raster
            .page_annotations(0)
            .iter()
            .any(|a| {
                a.kind == tilo_pdf::AnnotationKind::Stamp && a.contents.contains("Grace Hopper")
            });
        assert!(signed, "reopened saved file carries the signature stamp");
        eprintln!(
            "DRIVE real-pdf: word={word:?}  hits={}  saved={}  reopened-has-signature={signed}",
            app.raster_matches.len(),
            out.display(),
        );
    }
}

pub(super) fn framed(
    app: PdfApp,
    st: ui::State,
    w: f64,
    h: f64,
    out: Option<&str>,
) -> Framed<PdfApp> {
    framed_with_theme(app, st, w, h, out, Theme::dark())
}

fn framed_with_theme(
    app: PdfApp,
    st: ui::State,
    w: f64,
    h: f64,
    out: Option<&str>,
    theme: Theme,
) -> Framed<PdfApp> {
    let mut app = Framed::new(st, app, viewport((w, h)), theme);
    settle_and_shoot(&mut app, viewport((w, h)), out);
    app
}

#[test]
fn opened_pdf_defaults_to_width_fit() {
    let (_dir, path, _) = two_documents();
    let mut app = launched(&path);
    let reader = app.surface_mut::<PdfApp>();
    assert!(matches!(reader.fit, FitMode::Width));
    assert!((reader.zoom() - reader.viewport.fit_width_zoom()).abs() < 0.01);
}

#[test]
fn default_width_fit_tracks_the_canvas_and_preserves_explicit_zoom() {
    let state = crate::scenarios::sample_state();
    let mut app = framed_with_theme(
        crate::scenarios::sample_reader(),
        state,
        1440.0,
        810.0,
        None,
        Theme::dark().with_ui_scale(1.5),
    );
    assert!(matches!(app.fit, FitMode::Width));
    assert!((app.rail_rect.width() - 168.0).abs() < 0.01);
    assert!(
        app.zoom() > 1.15,
        "default paper is enlarged, not fit to height"
    );
    for (width, height) in [(1440.0, 810.0), (1600.0, 900.0), (1200.0, 810.0)] {
        full_frame(&mut app, width, height, None);
        let origin = app
            .page_origins()
            .first()
            .copied()
            .expect("first page origin");
        let card = render::page_card_rect(origin, app.zoom(), app.pw(), app.ph());
        let gutter = f64::from(app.layout_theme.tokens.spacing.s3);
        assert!((card.x0 - app.canvas_rect.x0 - gutter).abs() < 0.01);
        assert!((app.canvas_rect.x1 - card.x1 - gutter).abs() < 0.01);
        assert!((card.y0 - app.canvas_rect.y0 - gutter).abs() < 0.01);
        assert!(!app.is_overflowing_x());
    }
    app.fit_page();
    full_frame(&mut app, 1440.0, 810.0, None);
    assert!(app.ph() * app.zoom() < app.canvas_rect.height());
    let zoom = app.zoom();
    app.zoom_by(1.75 / zoom);
    full_frame(&mut app, 1600.0, 900.0, None);
    assert!((app.zoom() - 1.75).abs() < 0.01);
    app.fit_width();
    full_frame(&mut app, 1440.0, 810.0, None);
    assert!((app.pw() * app.zoom() - (app.canvas_rect.width() - 36.0)).abs() < 0.01);
}

#[test]
fn thumbnails_keep_the_pages_markup_at_any_ui_scale() {
    let (app, _) = app_with_state();
    let theme = Theme::dark().with_ui_scale(1.5);
    let mut text = TextRenderer::new();
    for page in &app.doc.pages {
        let markup = render::Markup {
            annotations: &app.annotations,
            signatures: &app.signatures,
            selected: None,
        };
        let mut page_scene = Scene::new();
        text.set_text_scale(1.0);
        render::draw_page(
            &mut PaintCtx::new(&mut page_scene, Affine::IDENTITY, &mut text, &theme),
            page,
            render::PageFrame {
                origin: Point::ZERO,
                scale: 1.0,
                page_w: app.pw(),
                page_h: app.ph(),
            },
            render::PageRaster {
                image: None,
                tiles: None,
            },
            markup,
        );
        let mut thumbnail = Scene::new();
        text.set_text_scale(1.5);
        render::Thumbnail {
            page,
            page_w: app.pw(),
            page_h: app.ph(),
            raster: None,
            markup,
        }
        .draw(
            &mut PaintCtx::new(&mut thumbnail, Affine::IDENTITY, &mut text, &theme),
            Rect::new(10.0, 20.0, 122.0, 20.0 + 112.0 * app.ph() / app.pw()),
        );
        assert!((text.text_scale() - 1.5).abs() < f32::EPSILON);
        let glyphs = |scene: &Scene| {
            scene
                .encoding()
                .resources
                .glyphs
                .iter()
                .map(|g| (g.id, g.x.to_bits(), g.y.to_bits()))
                .collect::<Vec<_>>()
        };
        if page.number == 1 {
            assert!(
                !glyphs(&page_scene).is_empty(),
                "the signature's name is set on page 1"
            );
        }
        assert_eq!(glyphs(&thumbnail), glyphs(&page_scene));
        assert!(
            thumbnail
                .encoding()
                .path_data
                .ends_with(&page_scene.encoding().path_data),
            "the miniature keeps the highlights and signature paths",
        );
    }
}

fn framed_at_zoom(mut app: PdfApp, st: ui::State, z: f64, out: Option<&str>) -> Framed<PdfApp> {
    app.fit = FitMode::Custom(z);
    app.viewport.set_zoom(z);
    framed(app, st, 1280.0, 800.0, out)
}

pub(super) fn full_frame(app: &mut Framed<PdfApp>, w: f64, h: f64, out: Option<&str>) {
    Framed::harness_mut(app)
        .relayout_at(viewport((w, h)))
        .expect("pdf frame lays out");
    settle_and_shoot(app, viewport((w, h)), out);
}

fn settle_and_shoot(app: &mut Framed<PdfApp>, (w, h): (u32, u32), out: Option<&str>) {
    {
        let (reader, st) = Framed::split(app);
        reader.sync_settled(&mut st.doc, &st.root);
    }
    let deadline = tilo_test_env::hang_deadline();
    while app.pending_visuals && ui::time::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        Framed::harness_mut(app).relayout();
    }
    let want_png = std::env::var("PDF_SHOTS").as_deref() == Ok("1");
    if let (Some(path), true) = (out, want_png) {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        ui::offscreen::OffscreenRenderer::new(w, h).render_png(
            Framed::harness(app).scene(),
            vello::peniko::Color::from_rgb8(10, 12, 16),
            std::path::Path::new(path),
        );
    }
}

fn set_zoom_and_frame(app: &mut Framed<PdfApp>, z: f64, out: Option<&str>) -> f64 {
    app.fit = FitMode::Custom(z);
    app.viewport.set_zoom(z);
    full_frame(app, 1280.0, 800.0, out);
    app.pw() * app.zoom()
}

#[test]
fn review_markup_reads_back_once() {
    eprintln!("phase the_review_reads_back");
    {
        let (app, _st) = app_with_state();
        assert!(!app.annotations.is_empty(), "seed should add annotations");
        assert!(!app.comments.is_empty(), "seed should add comments");
        assert!(!app.signatures.is_empty(), "seed should add a signature");
        let note = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .expect("a sticky note");
        assert!(
            app.comments
                .iter()
                .any(|c| c.pinned() == Some(note.id.as_str()))
        );
    }

    eprintln!("phase second_sync_does_not_duplicate");
    {
        let (mut app, mut st) = app_with_state();
        let n = app.annotations.len();
        sync_fixture(&mut app, &mut st);
        assert_eq!(
            app.annotations.len(),
            n,
            "re-sync must not duplicate markup"
        );
    }
}

fn seed_selection(app: &mut PdfApp, chars: &str, upto: usize) {
    let glyphs: Vec<(char, PtRect)> = chars
        .chars()
        .enumerate()
        .map(|(i, c)| (c, PtRect::new(72.0 + i as f64 * 9.0, 120.0, 8.0, 12.0)))
        .collect();
    app.raster_text
        .insert(1, std::sync::Arc::new(PageText::from_glyphs(glyphs)));
    app.selection = Some(TextSelection {
        anchor: TextPos::new(1, 0),
        caret: TextPos::new(1, upto + 1),
        dragging: false,
    });
}

#[test]
fn selection_hud_actions_highlight_note_and_copy() {
    eprintln!("phase hud_highlight_action_highlights_the_text_selection");
    {
        let (mut app, mut st) = app_with_state();
        seed_selection(&mut app, "Hello", 4);
        let hl = |st: &ui::State| {
            PdfMarkup::of(&st.doc)
                .list_annotations()
                .into_iter()
                .filter(|a| matches!(a.kind, AnnoKind::Markup { .. }))
                .count()
        };
        let before = hl(&st);
        app.apply_hud_action(ui::HudAction::Highlight(1), &mut Edits::lend(&mut st.doc));
        assert!(
            hl(&st) > before,
            "highlighting the selection adds a highlight annotation"
        );
        assert!(
            app.selection.is_none(),
            "the selection is consumed by the highlight"
        );
        assert_eq!(
            app.swatch,
            HighlightColor::ALL[1],
            "the armed swatch follows the HUD choice"
        );
    }

    eprintln!("phase hud_note_action_attaches_a_note_to_the_selection");
    {
        let (mut app, mut st) = app_with_state();
        seed_selection(&mut app, "AB", 1);
        let notes = |st: &ui::State| {
            PdfMarkup::of(&st.doc)
                .list_annotations()
                .into_iter()
                .filter(|a| matches!(a.kind, AnnoKind::Note { .. }))
                .count()
        };
        let before = notes(&st);
        app.apply_hud_action(ui::HudAction::Note, &mut Edits::lend(&mut st.doc));
        assert_eq!(
            notes(&st),
            before + 1,
            "a note is attached to the selection"
        );
        assert!(
            app.editing.is_some(),
            "the note's comment editor opens for typing"
        );
    }

    eprintln!("phase hud_copy_action_reports_success_on_a_selection");
    {
        let (mut app, mut st) = app_with_state();
        seed_selection(&mut app, "Copyme", 5);
        assert!(
            app.selection_text().is_some(),
            "the synthetic run yields text"
        );
        app.apply_hud_action(ui::HudAction::Copy, &mut Edits::lend(&mut st.doc));
        assert!(
            app.status_note.as_deref().unwrap_or("").contains("Copied"),
            "the HUD Copy action copies + toasts ({:?})",
            app.status_note
        );
    }
}

fn seed_words(app: &mut PdfApp, text: &str) {
    let glyphs: Vec<(char, PtRect)> = text
        .chars()
        .enumerate()
        .map(|(i, c)| (c, PtRect::new(72.0 + i as f64 * 9.0, 120.0, 8.0, 12.0)))
        .collect();
    app.raster_text
        .insert(1, std::sync::Arc::new(PageText::from_glyphs(glyphs)));
}
fn glyph_pt(i: usize) -> (f64, f64) {
    (72.0 + i as f64 * 9.0 + 4.0, 126.0)
}

fn on_screen(app: &PdfApp, page: usize, pt: (f64, f64)) -> Point {
    let origin = app.page_origins()[page - 1];
    Point::new(origin.x + pt.0 * app.zoom(), origin.y + pt.1 * app.zoom())
}

fn glyphs_of(sel: TextSelection) -> (usize, usize) {
    (sel.start().glyph, sel.end().glyph)
}

#[test]
fn multi_click_selection_snaps_to_words_and_lines() {
    eprintln!("phase double_click_selects_the_word_through_the_selection_path");
    {
        let (mut app, _st) = app_with_state();
        seed_words(&mut app, "hello world");
        let pt = glyph_pt(1); // over the 'e' in "hello"
        let scr = Point::new(pt.0, pt.1);

        assert_eq!(app.begin_selection(1, pt, scr), SelectionStart::Armed);
        assert!(
            !app.selection.unwrap().is_active(),
            "one click selects no text"
        );
        app.finish_selection();

        assert_eq!(app.begin_selection(1, pt, scr), SelectionStart::Armed);
        let sel = app.selection.expect("the double-click armed a selection");
        let (lo, hi) = glyphs_of(sel);
        assert_eq!(
            app.raster_text[&1].text_for_range(lo, hi),
            "hello",
            "double-click selects the whole word, not one glyph"
        );
        assert!(sel.is_active(), "the word selection is active (HUD shows)");
    }

    eprintln!("phase double_click_then_drag_extends_by_whole_words");
    {
        let (mut app, _st) = app_with_state();
        seed_words(&mut app, "hello world again");
        let pt = glyph_pt(1);
        let scr = Point::new(pt.0, pt.1);
        app.begin_selection(1, pt, scr);
        app.finish_selection();
        app.begin_selection(1, pt, scr); // double-click "hello"
        app.update_selection_at(on_screen(&app, 1, glyph_pt(7)));
        let sel = app.selection.expect("selection live");
        let (lo, hi) = glyphs_of(sel);
        assert_eq!(
            app.raster_text[&1].text_for_range(lo, hi),
            "hello world",
            "the word drag spans whole words and keeps the first word in"
        );
    }

    eprintln!("phase triple_click_selects_the_line");
    {
        let (mut app, _st) = app_with_state();
        seed_words(&mut app, "hello world");
        let pt = glyph_pt(1);
        let scr = Point::new(pt.0, pt.1);
        app.begin_selection(1, pt, scr);
        app.finish_selection();
        app.begin_selection(1, pt, scr);
        app.finish_selection();
        app.begin_selection(1, pt, scr); // third click
        let sel = app.selection.expect("selection live");
        let (lo, hi) = glyphs_of(sel);
        assert_eq!(
            app.raster_text[&1].text_for_range(lo, hi),
            "hello world",
            "triple-click selects the whole line"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn chrome_layout_reflows_contents_bubble_and_rail() {
    eprintln!("phase pages_take_the_whole_height_under_the_window_bar");
    {
        let (mut app, _st) = app_with_state();
        let canvas = app.canvas_rect;
        assert!(
            (canvas.y0 - app.rect.y0).abs() < 1e-9,
            "the pages start at the top"
        );
        assert!(
            (app.rail_rect.y0 - app.rect.y0).abs() < 1e-9
                && (app.sidebar_rect.y0 - app.rect.y0).abs() < 1e-9,
            "the panels start at the top"
        );
        app.stage_find_for_test("sandbox");
        app.recompute_regions();
        assert_eq!(app.canvas_rect, canvas, "finding adds no band");
    }

    eprintln!("phase contents_sidebar_scrolls_and_scales_all_outline_entries");
    {
        let (mut app, _st) = app_with_state();
        app.sidebar_tab = SidebarTab::Outline;
        app.layout_theme = Theme::dark().with_text_scale(2.0);
        app.doc.outline = (0..80)
            .map(|index| model::OutlineItem {
                title: format!("Section {index}: an intentionally descriptive heading"),
                page: index % app.doc.page_count() + 1,
                level: u8::from(index % 3 != 0),
            })
            .collect();
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);

        let (items, content_h) = app.sidebar_item_layout(&mut tr);
        assert_eq!(items.len(), 80, "every bookmark participates in scrolling");
        assert!(
            items[1].1 - items[0].1 > ui::chrome::nav_row_height(&Theme::dark()),
            "outline rows grow with the text"
        );

        app.sidebar_scroll = f64::MAX;
        app.prepare_sidebar_scroll(&mut tr);
        let body = app.sidebar_body();
        let origin = app.outline_top(body);
        let max_scroll = (content_h - (body.y1 - origin)).max(0.0);
        assert!(max_scroll > 0.0, "the test outline exceeds the viewport");
        assert!(
            (app.sidebar_scroll - max_scroll).abs() < 0.1,
            "wheel state clamps to the final bookmark instead of resetting"
        );
        let (_, last_top, last_h) = items.last().expect("last bookmark");
        assert!(
            last_top + last_h - app.sidebar_scroll <= body.y1 + 0.1,
            "the final bookmark can be brought fully into view"
        );
    }

    eprintln!("phase page_change_is_said_in_the_bar");
    {
        let (mut app, _st) = app_with_state();
        app.go_to_page(1);
        app.follow_page_line();
        app.page_notice.dismiss();
        app.go_to_page(2);
        app.follow_page_line();
        assert_eq!(
            app.page_notice.get().cloned(),
            Some(app.page_line()),
            "the new page is news"
        );
        assert!(app.page_line().starts_with("Page 2 of "));
    }

    eprintln!("phase rail_toggle_and_resize_reclaim_space");
    {
        let (mut app, _st) = app_with_state();
        let canvas_with_rail = app.canvas_rect.width();
        app.toggle_rail();
        assert!(!app.show_rail);
        assert!(
            app.canvas_rect.width() > canvas_with_rail,
            "hiding the rail reclaims space"
        );
        app.toggle_rail();
        assert!(app.show_rail);
        app.rail_w = 999.0;
        app.recompute_regions();
        assert!(
            (app.rail_width() - app.rail_width_range().1).abs() < 1e-6,
            "rail width clamps to max"
        );
    }

    eprintln!("phase responsive_mode_reflows_on_resize_not_cached");
    {
        let (mut app, _st) = app_with_state(); // starts wide (1280)
        app.reconcile_overlay();
        app.recompute_regions();
        assert!(!app.overlay_mode, "wide window docks panels");
        assert!(
            app.show_sidebar && app.show_rail,
            "docked panels are visible when wide"
        );
        assert!(
            app.canvas_rect.width() < app.rect.width() - 100.0,
            "docked panels reserve columns"
        );

        app.rect = Rect::new(0.0, 0.0, 560.0, 720.0);
        app.reconcile_overlay();
        app.recompute_regions();
        assert!(app.overlay_mode, "narrow window switches to overlay mode");
        assert!(
            !app.show_sidebar && !app.show_rail,
            "narrow window auto-closes the panels"
        );
        assert!(
            (app.canvas_rect.width() - app.rect.width()).abs() < 1.0,
            "canvas spans the full width in overlay mode"
        );

        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.reconcile_overlay();
        app.recompute_regions();
        assert!(!app.overlay_mode, "returning to wide re-docks");
        assert!(
            app.show_sidebar && app.show_rail,
            "panels come back when wide again"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn markup_tools_commit_notes_highlights_and_ink() {
    eprintln!("phase create_note_adds_annotation_and_comment");
    {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        let c0 = app.comments.len();
        app.create_note(&mut Edits::lend(&mut st.doc), 2, (120.0, 200.0));
        assert_eq!(app.annotations.len(), a0 + 1);
        assert_eq!(app.comments.len(), c0 + 1);
        assert_eq!(
            PdfMarkup::of(&st.doc).list_comments().len(),
            c0 + 1,
            "creating a note landed in the shared document"
        );
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
    }

    eprintln!("phase dragging_a_note_marker_repositions_and_persists");
    {
        let (mut app, mut st) = app_with_state();
        app.create_note(&mut Edits::lend(&mut st.doc), 1, (100.0, 100.0));
        app.reload(&st.doc);
        let id = app.selected_anno.clone().expect("the new note is selected");
        assert!(
            app.note_marker_at(1, (100.0, 100.0)).is_some(),
            "the marker is grabbable"
        );
        let mut mdrag = ui::MarkerDrag::begin(id.clone(), (100.0, 100.0), (100.0, 100.0));
        let (nx, ny) = mdrag.update((180.0, 140.0));
        assert!(mdrag.moved(), "a real drag latches moved");
        app.move_note_local(&id, nx, ny);
        assert_eq!(
            app.note_pos(&id),
            Some((nx, ny)),
            "the marker follows the pointer live"
        );
        let note = PdfMarkup::paths()
            .annotations
            .item(&ui::tilo_resource::Id::from_raw(id.clone()));
        PdfMarkup::apply_to(
            &mut st.doc,
            ui::tilo_resource::Invoke::group([
                ui::tilo_resource::Invoke::from(note.kind.set(AnnoKind::Note { x: nx, y: ny })),
                ui::tilo_resource::Invoke::from(note.space.set(crate::model::GeomSpace::Page)),
            ]),
        );
        app.reload(&st.doc);
        let (x, y) = app.note_pos(&id).expect("the note survives the reposition");
        assert!(
            (x - nx).abs() < 1e-6 && (y - ny).abs() < 1e-6,
            "the reposition persisted to the CRDT"
        );
        assert!(
            (x - 100.0).abs() > 1.0,
            "the note actually moved from its origin"
        );
    }

    eprintln!("phase highlight_drag_commits_annotation");
    {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (64.0, 100.0),
            cur: (300.0, 116.0),
            points: vec![],
        });
        app.commit_drag(&mut Edits::lend(&mut st.doc));
        assert_eq!(
            app.annotations.len(),
            a0 + 1,
            "drag should add one highlight"
        );
    }

    eprintln!("phase tiny_highlight_drag_is_ignored");
    {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (64.0, 100.0),
            cur: (66.0, 101.0),
            points: vec![],
        });
        app.commit_drag(&mut Edits::lend(&mut st.doc));
        assert_eq!(app.annotations.len(), a0, "sub-threshold drag adds nothing");
    }

    eprintln!("phase highlight_falls_back_to_freeform_without_text");
    {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.tool = Tool::Highlight;
        app.drag = Some(DragState {
            tool: Tool::Highlight,
            page: 1,
            start: (80.0, 500.0),
            cur: (300.0, 540.0),
            points: vec![],
        });
        app.commit_drag(&mut Edits::lend(&mut st.doc));
        assert_eq!(
            app.annotations.len(),
            a0 + 1,
            "no nearby text -> single free-form band"
        );
    }

    eprintln!("phase ink_drag_commits_a_polyline_annotation");
    {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.tool = Tool::Ink;
        app.drag = Some(DragState {
            tool: Tool::Ink,
            page: 1,
            start: (80.0, 300.0),
            cur: (240.0, 360.0),
            points: vec![
                (80.0, 300.0),
                (140.0, 330.0),
                (200.0, 320.0),
                (240.0, 360.0),
            ],
        });
        app.commit_drag(&mut Edits::lend(&mut st.doc));
        assert_eq!(
            app.annotations.len(),
            a0 + 1,
            "an ink drag adds one annotation"
        );
        let ink = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Ink { .. }))
            .expect("an ink stroke was created");
        match &ink.kind {
            AnnoKind::Ink { strokes } => {
                assert!(strokes[0].len() >= 2, "the polyline retains its points");
            }
            _ => unreachable!(),
        }
        assert!(
            PdfMarkup::of(&st.doc)
                .list_annotations()
                .iter()
                .any(|a| matches!(a.kind, AnnoKind::Ink { .. })),
            "and the ink stroke landed in the shared document"
        );
    }

    eprintln!("phase ink_drag_with_a_single_point_is_discarded");
    {
        let (mut app, mut st) = app_with_state();
        let a0 = app.annotations.len();
        app.tool = Tool::Ink;
        app.drag = Some(DragState {
            tool: Tool::Ink,
            page: 1,
            start: (80.0, 300.0),
            cur: (80.0, 300.0),
            points: vec![(80.0, 300.0)],
        });
        app.commit_drag(&mut Edits::lend(&mut st.doc));
        assert_eq!(
            app.annotations.len(),
            a0,
            "a degenerate single-point ink stroke adds nothing"
        );
    }
}

#[serial(pdfium)]
#[test]
fn find_jumps_to_matches_and_toggles_change_match_logic() {
    let Some(mut app) = open_raster_sample() else {
        return;
    };
    let mut st = crate::scenarios::record_state();
    for page in 1..=app.doc.page_count() {
        text_layer(&mut app, &mut st, page);
    }
    let find = |app: &mut PdfApp, query: &str, whole_word: bool, match_case: bool| {
        let options = ui::widget::FindOptions {
            match_case,
            whole_word,
            ..ui::widget::FindOptions::default()
        };
        ui::widget::Findable::find(app, query, options);
        app.raster_matches
            .iter()
            .map(|hit| hit.page)
            .collect::<Vec<_>>()
    };
    assert_eq!(find(&mut app, "godwit", false, false), vec![2, 2, 3]);
    assert_eq!(app.current_page, 2, "the first match is shown");
    ui::widget::Findable::reveal(&mut app, 2);
    assert_eq!(app.active_match, 2);
    assert_eq!(app.current_page, 3, "stepping goes to the match's page");
    assert_eq!(find(&mut app, "godwit", true, false), vec![2]);
    assert_eq!(find(&mut app, "Godwit", false, true), Vec::<usize>::new());
    assert_eq!(find(&mut app, "GODWIT", false, false).len(), 3);
}

#[test]
fn placing_markup_clamps_into_the_page_and_focuses_its_sidebar() {
    eprintln!("phase place_signature_clamps_into_page");
    {
        let (mut app, mut st) = app_with_state();
        let s0 = app.signatures.len();
        app.signer_name = "Tester".into();
        app.place_signature(&mut Edits::lend(&mut st.doc), 3, (10.0, 10.0)); // near corner
        assert_eq!(app.signatures.len(), s0 + 1);
        let last = app.signatures.iter().find(|s| s.page == 3).unwrap();
        assert!(last.rect.x >= MARGIN - 0.001);
        assert!(last.rect.y >= MARGIN - 0.001);
    }

    eprintln!("phase placing_signature_focuses_signatures_sidebar");
    {
        let (mut app, mut st) = app_with_state();
        app.sidebar_tab = SidebarTab::Comments;
        app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Signatures as u8);
        assert!(app.show_sidebar);
        assert!(app.selected_sig.is_some(), "new signature is selected");
        assert!(
            app.sidebar_reveal.is_some(),
            "reveal request queued for the new entry"
        );
    }

    eprintln!("phase creating_note_focuses_comments_sidebar_on_new_thread");
    {
        let (mut app, mut st) = app_with_state();
        app.sidebar_tab = SidebarTab::Signatures;
        app.create_note(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
        assert_eq!(app.sidebar_tab as u8, SidebarTab::Comments as u8);
        assert!(app.show_sidebar);
        assert!(
            app.sidebar_reveal.is_some(),
            "new comment queued to scroll into view"
        );
    }
}

#[test]
fn navigation_and_zoom_clamp_and_keep_the_focal_point() {
    eprintln!("phase page_navigation_clamps");
    {
        let (mut app, _st) = app_with_state();
        app.go_to_page(999);
        assert_eq!(app.current_page, app.doc.page_count());
        app.go_to_page(0);
        assert_eq!(app.current_page, 1);
    }

    eprintln!("phase zoom_switches_to_custom");
    {
        let (mut app, _st) = app_with_state();
        app.zoom_by(1.25);
        assert!(matches!(app.fit, FitMode::Custom(_)));
    }

    eprintln!("phase zoom_at_keeps_focal_point_stationary");
    {
        let (mut app, _st) = app_with_state();
        app.fit = FitMode::Custom(2.0);
        app.recompute_zoom();
        app.go_to_page(1);
        let (half_x, sy) = (app.viewport.max_scroll_x() / 2.0, app.scroll_y());
        app.viewport.set_scroll(half_x, sy);
        let origin = app.page_origins()[0];
        let focal = Point::new(origin.x + 300.0 * app.zoom(), origin.y + 200.0 * app.zoom());
        let before = app.hit_page(focal).expect("focal lands on a page");
        app.zoom_at(1.3, focal);
        let after = app.hit_page(focal).expect("focal still on a page");
        assert_eq!(before.0, after.0, "same page stays under the cursor");
        assert!(
            (before.1.0 - after.1.0).abs() < 2.0,
            "x focal drift {} too large",
            (before.1.0 - after.1.0).abs()
        );
        assert!(
            (before.1.1 - after.1.1).abs() < 2.0,
            "y focal drift {} too large",
            (before.1.1 - after.1.1).abs()
        );
    }

    eprintln!("phase zoom_respects_min_max_bounds");
    {
        let (mut app, _st) = app_with_state();
        for _ in 0..40 {
            app.zoom_by(1.5);
        }
        assert!(app.zoom() <= ZOOM_MAX + 1e-6);
        for _ in 0..40 {
            app.zoom_by(0.5);
        }
        assert!(app.zoom() >= ZOOM_MIN - 1e-6);
    }

    eprintln!("phase horizontal_pan_reaches_full_width_when_zoomed");
    {
        let (mut app, _st) = app_with_state();
        app.fit = FitMode::Custom(3.0);
        app.recompute_zoom();
        app.clamp_scroll();
        assert!(app.is_overflowing_x(), "3x zoom must overflow the viewport");
        let sy = app.scroll_y();
        app.viewport.set_scroll(f64::MAX, sy);
        let expected = (app.viewport.scaled_content_width() - app.canvas_rect.width()).max(0.0);
        assert!(
            (app.scroll_x() - expected).abs() < 1.0,
            "pan should reach the full zoomed width"
        );
    }
}

#[test]
fn delete_targets_remove_markup_and_anchored_annotations() {
    eprintln!("phase delete_selected_annotation_removes_it");
    {
        let (mut app, mut st) = app_with_state();
        let hl = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Markup { .. }))
            .unwrap()
            .clone();
        let n = app.annotations.len();
        app.selected_anno = Some(hl.id.clone());
        assert!(app.delete_selected(&mut Edits::lend(&mut st.doc)));
        assert_eq!(app.annotations.len(), n - 1);
        assert!(!app.annotations.iter().any(|a| a.id == hl.id));
        assert!(app.selected_anno.is_none());
    }

    eprintln!("phase delete_signature_target_removes_it");
    {
        let (mut app, mut st) = app_with_state();
        let sig = app.signatures[0].clone();
        let n = app.signatures.len();
        app.delete_target(
            &DeleteTarget::Signature(sig.id.clone()),
            &mut Edits::lend(&mut st.doc),
        );
        assert_eq!(app.signatures.len(), n - 1);
        assert!(!app.signatures.iter().any(|s| s.id == sig.id));
    }

    eprintln!("phase delete_comment_target_also_drops_its_annotation");
    {
        let (mut app, mut st) = app_with_state();
        let note = app
            .annotations
            .iter()
            .find(|a| matches!(a.kind, AnnoKind::Note { .. }))
            .unwrap()
            .clone();
        let comment = app
            .comments
            .iter()
            .find(|c| c.pinned() == Some(note.id.as_str()))
            .unwrap()
            .clone();
        app.delete_target(
            &DeleteTarget::Comment(comment.id.clone()),
            &mut Edits::lend(&mut st.doc),
        );
        assert!(!app.comments.iter().any(|c| c.id == comment.id));
        assert!(
            !app.annotations.iter().any(|a| a.id == note.id),
            "orphan note marker removed"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn signatures_stamp_real_dates_and_store_typed_drawn_and_edited_signers() {
    eprintln!("phase today_is_the_local_date");
    {
        let t = today_ymd();
        assert_eq!(t.len(), 10, "today is YYYY-MM-DD: {t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[7..8], "-");
    }

    eprintln!("phase placed_signature_stamps_the_real_date_not_a_constant");
    {
        let (mut app, mut st) = app_with_state();
        app.signer_name = "Grace Hopper".into();
        app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
        let s = app.signatures.iter().find(|s| s.page == 2).unwrap().clone();
        assert_eq!(
            s.date,
            today_ymd(),
            "runtime signature carries today's date"
        );
        assert_ne!(
            s.date,
            crate::scenarios::SIGNED_ON,
            "runtime date is not the hardcoded demo constant"
        );
        let stored = PdfMarkup::of(&st.doc).list_signatures();
        assert!(stored.iter().any(|x| x.id == s.id && x.date == today_ymd()));
    }

    eprintln!("phase placing_a_typed_signature_stores_the_typed_name");
    {
        let (mut app, mut st) = app_with_state();
        app.sig_mode = SigMode::Type;
        app.signer_name = "Grace Hopper".into();
        app.place_signature(&mut Edits::lend(&mut st.doc), 3, (200.0, 300.0));
        let placed = app
            .signatures
            .iter()
            .find(|s| s.page == 3)
            .expect("a signature landed");
        assert_eq!(placed.signer, "Grace Hopper");
        assert_eq!(
            placed.ink,
            SignatureInk::Typed,
            "a typed signature carries typed ink"
        );
        let stored = PdfMarkup::of(&st.doc).list_signatures();
        assert!(
            stored
                .iter()
                .any(|x| x.id == placed.id && x.ink == SignatureInk::Typed)
        );
    }

    eprintln!("phase drawn_signature_places_the_strokes_and_empty_is_refused");
    {
        let (mut app, mut st) = app_with_state();
        app.sig_mode = SigMode::Draw;

        let before = app.signatures.len();
        app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
        assert_eq!(
            app.signatures.len(),
            before,
            "an empty drawn signature is not placed"
        );
        assert!(
            app.status_note
                .as_deref()
                .unwrap_or("")
                .to_lowercase()
                .contains("draw")
        );

        app.sig_strokes = vec![vec![(0.1, 0.5), (0.4, 0.2), (0.7, 0.6), (0.9, 0.4)]];
        app.place_signature(&mut Edits::lend(&mut st.doc), 2, (200.0, 300.0));
        let placed = app
            .signatures
            .iter()
            .find(|s| s.page == 2)
            .expect("a drawn signature landed");
        match &placed.ink {
            SignatureInk::Drawn { strokes } => {
                assert_eq!(strokes.len(), 1);
                assert_eq!(strokes[0].len(), 4, "the drawn polyline is preserved");
            }
            SignatureInk::Typed => panic!("expected drawn ink, got Typed"),
        }
        let stored = PdfMarkup::of(&st.doc).list_signatures();
        assert!(
            stored
                .iter()
                .any(|x| matches!(&x.ink, SignatureInk::Drawn { strokes } if strokes.len() == 1))
        );
    }

    eprintln!("phase signer_name_inline_edit_commits_and_empty_falls_back_to_author");
    {
        let (mut app, mut st) = app_with_state();
        app.begin_edit(EditTarget::Signer, &app.signer());
        app.editing.as_mut().unwrap().set_text("Grace Hopper");
        app.commit_edit(&mut Edits::lend(&mut st.doc));
        assert!(app.editing.is_none(), "committing closes the editor");
        assert_eq!(
            app.signer_name, "Grace Hopper",
            "signer name updates from the editor"
        );
        app.place_signature(&mut Edits::lend(&mut st.doc), 3, (200.0, 300.0));
        assert_eq!(
            app.signatures.iter().find(|s| s.page == 3).unwrap().signer,
            "Grace Hopper"
        );
        app.begin_edit(EditTarget::Signer, "whatever");
        app.editing.as_mut().unwrap().set_text("   ");
        app.commit_edit(&mut Edits::lend(&mut st.doc));
        assert_eq!(
            app.signer(),
            app.author_name(&app.author),
            "an empty signer name falls back to the author"
        );
    }

    eprintln!("phase an_unnamed_participant_never_reads_as_a_hex_id");
    {
        let (mut app, mut st) = app_with_state();
        let me = "94381abe81de6c0a94381abe81de6c0a";
        let peer = "0badc0de0badc0de0badc0de0badc0de";
        app.author = me.into();
        app.signer_name.clear();
        app.attach_people(ui::framework::People::named(Some(me), &[]));
        assert_eq!(app.author_name(me), "You");
        assert_eq!(app.author_name(peer), "Someone");
        assert_eq!(app.signer(), "");
        let signed = app.signatures.len();
        app.sig_mode = SigMode::Type;
        app.place_signature(&mut Edits::lend(&mut st.doc), 1, (200.0, 300.0));
        assert_eq!(app.signatures.len(), signed, "no nameless signature");
        assert!(app.is_editing(&EditTarget::Signer), "the name field opens");
        app.attach_people(ui::framework::People::named(
            Some(me),
            &[(me, "Ada Lovelace")],
        ));
        assert_eq!(app.author_name(me), "Ada Lovelace");
        assert_eq!(app.signer(), "Ada Lovelace");
    }

    eprintln!("phase comment_authors_read_as_the_rosters_names");
    {
        let (mut app, mut st) = app_with_state();
        app.author = "test-author".into();
        app.attach_people(ui::framework::People::named(
            Some("test-author"),
            &[("test-author", "Tess Author"), ("peer-id", "Bob Builder")],
        ));
        app.create_note(&mut Edits::lend(&mut st.doc), 2, (120.0, 200.0));
        let thread = PdfMarkup::of(&st.doc)
            .list_comments()
            .into_iter()
            .find(|comment| comment.page == 2)
            .expect("the new note's thread");
        assert_eq!(
            thread.comment.author, "test-author",
            "the record keeps the id"
        );
        assert_eq!(
            app.author_name(&thread.comment.author),
            "Tess Author",
            "your own note is signed with your name, not \"You\""
        );
        assert_eq!(app.author_name("peer-id"), "Bob Builder");
        assert_eq!(
            app.author_name("Carol"),
            "Carol",
            "an unknown id reads as itself"
        );
        let described = format!("{:?}", Widget::accessibility(&app));
        assert!(
            described.contains("Tess Author on page 2"),
            "assistive technology hears the name, not the id"
        );
    }

    eprintln!("phase the_signature_is_the_persons_own_and_follows_them");
    {
        let mut host =
            ui::framework::testing::TestApp::<crate::PdfViewerShell>::for_person(&"ad".repeat(32))
                .expect("the reader for a person");
        let record = host
            .person::<crate::signature::SignatureProfile>()
            .expect("the person's signature record");
        let (mut app, mut st) = app_with_state();
        app.attach_signature(record.clone());
        app.begin_edit(EditTarget::Signer, "");
        app.editing.as_mut().unwrap().set_text("Ada Lovelace");
        app.commit_edit(&mut Edits::lend(&mut st.doc));
        app.sig_strokes = vec![vec![(0.1, 0.5), (0.9, 0.4)]];
        app.keep_strokes();
        assert_eq!(record.get().signer, "Ada Lovelace", "the signer is kept");
        assert_eq!(
            crate::signature::point_lists(&record.get()),
            app.sig_strokes,
            "the strokes are kept"
        );
        assert!(
            PdfMarkup::of(&st.doc).list_signatures().is_empty()
                || PdfMarkup::of(&st.doc)
                    .list_signatures()
                    .iter()
                    .all(|signature| signature.signer != "Ada Lovelace"),
            "composing a signature writes nothing into the shared document"
        );

        let (mut other, mut other_st) = app_with_state();
        other.attach_signature(record);
        sync_fixture(&mut other, &mut other_st);
        assert_eq!(other.signer_name, "Ada Lovelace");
        assert_eq!(other.sig_strokes, vec![vec![(0.1, 0.5), (0.9, 0.4)]]);
    }
}

#[test]
fn the_signatures_panel_reads_stored_days_in_the_windows_language() {
    let (mut app, mut st) = app_with_state();
    app.place_signature(&mut Edits::lend(&mut st.doc), 1, (200.0, 300.0));
    let placed = app
        .signatures
        .first()
        .cloned()
        .expect("a signature landed on the page");
    app.focus_sidebar(SidebarTab::Signatures, placed.id.clone());

    let tree = Widget::accessibility(&app).expect("reader semantics");
    let heard = std::cell::RefCell::new(Vec::new());
    any_access_node(&tree, &|node: &ui::a11y::AccessNode| {
        let mut heard = heard.borrow_mut();
        heard.extend(node.name.clone());
        heard.extend(node.description.clone());
        false
    });
    let heard = heard.borrow().join("\n");
    assert!(
        heard.contains(&format!(
            "Signed {} on page",
            crate::dates::shown(&placed.date)
        )),
        "the panel reads {placed:?} as the window reads dates"
    );
    assert!(
        !heard.contains(&format!("Signed {} on page", placed.date)),
        "the panel never speaks the record's ISO spelling"
    );
}

const SAMPLE_PDF: &str = concat!(env!("OUT_DIR"), "/sample.pdf");

const NO_PDFIUM: &str = "no usable libpdfium.so — install one (e.g. \
     ~/.local/lib/pdfium/libpdfium.so) or set TILO_PDFIUM_LIB";

pub(super) fn open_settled(path: impl AsRef<std::path::Path>) -> Option<PdfApp> {
    let path = path.as_ref();
    let mut app = PdfApp::open(path);
    drain_until(&mut app, |app| app.pending_open.is_none());
    match pdfium_gate(app.raster_active(), require_pdfium()) {
        PdfiumGate::Run => Some(app),
        PdfiumGate::Fail => panic!(
            "TILO_REQUIRE_PDFIUM is set and there is {NO_PDFIUM} ({:?})",
            app.load_error
        ),
        PdfiumGate::Skip => {
            eprintln!(
                "SKIPPING a pdfium-backend test on {}: {NO_PDFIUM}. The \
                 raster text layer, save-back and page-editing paths went \
                 unverified.",
                path.display()
            );
            None
        }
    }
}

fn open_raster_sample() -> Option<PdfApp> {
    open_settled(SAMPLE_PDF)
}

pub(crate) fn open_sample_review() -> Option<(PdfApp, ui::State)> {
    let mut app = open_raster_sample()?;
    let mut st = crate::scenarios::sample_state();
    settle_record(&mut app, &mut st);
    app.apply_stage(None);
    app.read_text_for_search();
    settle_until(&mut app, &mut st, |app| !app.search_text_pending());
    app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
    app.recompute_regions();
    app.sync_viewport_frame();
    Some((app, st))
}

pub(super) fn settle_until(
    app: &mut PdfApp,
    st: &mut ui::State,
    mut done: impl FnMut(&PdfApp) -> bool,
) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.sync_settled(&mut st.doc, &st.root);
        if done(app) {
            return;
        }
        assert!(
            ui::time::now() < deadline,
            "the PDF worker never answered ({:?})",
            app.status_note
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn settle_record(app: &mut PdfApp, st: &mut ui::State) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        app.sync_settled(&mut st.doc, &st.root);
        let adopted = app.source_blob.as_ref().is_some_and(|source| {
            PdfMarkup::of(&st.doc).imported_source() == Some(hex::encode(source.hash()).as_str())
        });
        if !app.seed_pending && adopted {
            return;
        }
        assert!(
            ui::time::now() < deadline,
            "the record never took the document over ({:?})",
            app.status_note
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(super) fn text_layer(app: &mut PdfApp, st: &mut ui::State, page: usize) -> Arc<PageText> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(layer) = app.ensure_raster_text(page) {
            return layer;
        }
        assert!(
            ui::time::now() < deadline,
            "the PDF worker never answered ({:?})",
            app.status_note
        );
        settle_until(app, st, |app| {
            app.raster_text.contains_key(&page) || !app.raster_text_pending.contains(&(page - 1))
        });
    }
}

pub(super) fn drain_until(app: &mut PdfApp, mut done: impl FnMut(&PdfApp) -> bool) {
    let state = AutoCommit::new();
    let deadline = tilo_test_env::hang_deadline();
    while !done(app) {
        assert!(
            ui::time::now() < deadline,
            "the PDF worker never answered ({:?})",
            app.status_note
        );
        std::thread::sleep(Duration::from_millis(5));
        app.drain_raster_events(&state);
    }
}

pub(super) fn save_settled(app: &mut PdfApp, out: &std::path::Path) -> bool {
    if !app.save_annotated_to(out) {
        return false;
    }
    drain_until(app, |app| app.pending_save.is_none());
    app.status_note
        .as_deref()
        .is_some_and(|note| note.starts_with("Saved"))
}

#[derive(Debug, PartialEq, Eq)]
enum PdfiumGate {
    Run,
    Skip,
    Fail,
}

fn pdfium_gate(has_raster: bool, required: bool) -> PdfiumGate {
    match (has_raster, required) {
        (true, _) => PdfiumGate::Run,
        (false, true) => PdfiumGate::Fail,
        (false, false) => PdfiumGate::Skip,
    }
}

fn require_pdfium() -> bool {
    std::env::var_os("TILO_REQUIRE_PDFIUM").is_some()
}

#[serial(pdfium)]
#[test]
fn raster_open_is_gated_measured_and_budgeted_on_the_first_frame() {
    eprintln!("phase a_missing_pdfium_skips_unless_it_was_required");
    {
        assert_eq!(pdfium_gate(true, false), PdfiumGate::Run);
        assert_eq!(pdfium_gate(true, true), PdfiumGate::Run);
        assert_eq!(
            pdfium_gate(false, false),
            PdfiumGate::Skip,
            "with no backend and no demand for one, say so and stand down"
        );
        assert_eq!(
            pdfium_gate(false, true),
            PdfiumGate::Fail,
            "TILO_REQUIRE_PDFIUM has to turn the absence back into a failure, \
             or the skip has no floor under it"
        );
    }

    eprintln!("phase measure_open_and_render_perf");
    'phase: {
        let t = ui::time::now();
        let loaded = crate::source::open(std::path::Path::new(SAMPLE_PDF), &ContentStore::memory());
        let open_ms = t.elapsed().as_secs_f64() * 1000.0;
        match pdfium_gate(loaded.is_ok(), require_pdfium()) {
            PdfiumGate::Run => {}
            PdfiumGate::Fail => panic!("TILO_REQUIRE_PDFIUM is set and there is {NO_PDFIUM}"),
            PdfiumGate::Skip => {
                eprintln!("SKIPPING the pdfium open/render measurement: {NO_PDFIUM}.");
                break 'phase;
            }
        }
        let mut raster = loaded.expect("the sample opens").raster;
        let pages = raster.page_count();

        let t = ui::time::now();
        let layer = raster.page_text(0);
        let text_ms = t.elapsed().as_secs_f64() * 1000.0;
        let glyphs = layer.chars.len();

        let bucket = 1224u32;
        let t = ui::time::now();
        let img = raster.render(0, bucket);
        let render_cold_ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(img.is_some(), "page 0 rasterises");

        let t = ui::time::now();
        let cached = raster.render(0, bucket);
        let render_cached_ms = t.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "PERF pages={pages} glyphs={glyphs} | open={open_ms:.1}ms \
             text_extract={text_ms:.1}ms render_cold={render_cold_ms:.1}ms \
             render_cached(scroll-frame)={render_cached_ms:.3}ms"
        );
        assert_eq!(
            cached.map(|image| image.data.id()),
            img.map(|image| image.data.id()),
            "a repeated render reuses the rasterised page"
        );
    }

    eprintln!("phase first_frame_defers_text_extraction");
    'phase: {
        use std::collections::HashMap;
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;

        let Some(mut app) = open_raster_sample() else {
            break 'phase;
        };
        let mut st = crate::scenarios::record_state();
        let theme = Theme::dark();
        let (w, h) = (1280.0f64, 800.0f64);

        let mut tr = TextRenderer::new();
        let frame = |app: &mut PdfApp, st: &mut ui::State, tr: &mut TextRenderer| {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            sync_fixture(app, st);
            let child = app.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style {
                        size: Size::from_lengths(w as f32, h as f32),
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let trm = &mut *tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(w as f32),
                        height: taffy::AvailableSpace::Definite(h as f32),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, trm) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
            app.apply_layout(&taffy, child, Point::ORIGIN);
            let mut scene = Scene::new();
            app.draw_child(
                &mut scene,
                Affine::IDENTITY,
                tr,
                &theme,
                &PresenceCtx::empty(),
            );
        };

        frame(&mut app, &mut st, &mut tr);
        app.raster_text.clear();

        let t = ui::time::now();
        frame(&mut app, &mut st, &mut tr);
        let idle_open_ms = t.elapsed().as_secs_f64() * 1000.0;
        let idle_text_pages = app.raster_text.len() + app.raster_text_pending.len();

        let t = ui::time::now();
        frame(&mut app, &mut st, &mut tr);
        let idle_scroll_ms = t.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "PERF-FRAME idle_open={idle_open_ms:.2}ms idle_scroll={idle_scroll_ms:.2}ms \
             idle_text_pages={idle_text_pages}"
        );
        assert_eq!(
            idle_text_pages, 0,
            "an idle frame must defer pdfium text extraction (only selection/search needs it)"
        );
    }

    eprintln!("phase open_rasterises_only_the_budgeted_pages_on_the_first_frame");
    'phase: {
        use std::collections::HashMap;
        use ui::presence::PresenceCtx;
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;

        let Some(mut app) = open_raster_sample() else {
            break 'phase;
        };
        let mut st = crate::scenarios::record_state();
        let theme = Theme::dark();
        let (w, h) = (1280.0f64, 800.0f64);
        let mut tr = TextRenderer::new();

        let frame = |app: &mut PdfApp, st: &mut ui::State, tr: &mut TextRenderer| {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            sync_fixture(app, st);
            let child = app.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(
                    Style {
                        size: Size::from_lengths(w as f32, h as f32),
                        ..Default::default()
                    },
                    &[child],
                )
                .unwrap();
            let measure_map = node_map.clone();
            let trm = &mut *tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(w as f32),
                        height: taffy::AvailableSpace::Definite(h as f32),
                    },
                    |known, avail, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, avail, trm) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
            app.apply_layout(&taffy, child, Point::ORIGIN);
            let mut scene = Scene::new();
            app.draw_child(
                &mut scene,
                Affine::IDENTITY,
                tr,
                &theme,
                &PresenceCtx::empty(),
            );
        };

        frame(&mut app, &mut st, &mut tr);
        let full_pages_first = app
            .raster_render_pending
            .iter()
            .filter(|(_, bucket)| *bucket > 600)
            .count();
        assert!(
            full_pages_first <= MAX_NEW_PAGE_RASTERS_PER_FRAME,
            "first paint asks for at most {MAX_NEW_PAGE_RASTERS_PER_FRAME} full page(s), not a screen-full — got {full_pages_first}"
        );

        tilo_test_env::wait_until("the deferred page backlog to drain", || {
            if !app.pending_visuals {
                return true;
            }
            frame(&mut app, &mut st, &mut tr);
            false
        });
        assert!(
            !app.pending_visuals && app.raster_render_pending.is_empty(),
            "the deferred page backlog drains and the reader settles back to idle"
        );
    }
}

const LATIN_PDF: &str = concat!(env!("OUT_DIR"), "/latin.pdf");

fn open_latin_raster() -> Option<PdfApp> {
    open_settled(LATIN_PDF)
}

fn zoom_without_frame(app: PdfApp, zoom: f64) -> Framed<PdfApp> {
    let mut app = framed_at_zoom(app, crate::scenarios::record_state(), 1.0, None);
    rezoom_without_frame(&mut app, zoom);
    app
}

fn rezoom_without_frame(app: &mut PdfApp, zoom: f64) {
    app.fit = FitMode::Custom(zoom);
    app.viewport.set_zoom(zoom);
}

fn settle_tiles(app: &mut Framed<PdfApp>) -> RasterFrame {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let frame = app.rasterize_visible();
        if !app.pending_visuals {
            return frame;
        }
        assert!(
            ui::time::now() < deadline,
            "the viewport's tiles never all arrived: pending renders {:?}, cached images {:?}, queued tiles {}, cached tiles {}, status {:?}",
            app.raster_render_pending,
            app.raster_images.keys().collect::<Vec<_>>(),
            app.raster_tile_batch.len(),
            app.raster_tiles.keys().count(),
            app.status_note
        );
        std::thread::sleep(Duration::from_millis(5));
        let (reader, st) = Framed::split(app);
        reader.sync_settled(&mut st.doc, &st.root);
    }
}

fn tile_levels(app: &PdfApp) -> Vec<crate::tiles::Level> {
    let mut levels: Vec<_> = app.raster_tiles.keys().map(|key| key.level).collect();
    levels.sort_unstable();
    levels.dedup();
    levels
}

fn tile_coverage(app: &PdfApp, tiles: &render::PageTiles) -> (f64, f64) {
    let canvas = app.canvas_rect;
    let p = tiles.placement;
    let (pw, ph) = crate::tiles::page_pixels(p.size, p.level);
    let (px0, py0, px1, py1) = p.screen_rect(crate::tiles::PixelRect {
        x: 0,
        y: 0,
        width: pw,
        height: ph,
    });
    let target = Rect::new(px0, py0, px1, py1).intersect(canvas);
    let covered: f64 = tiles
        .tiles
        .iter()
        .map(|(region, _)| {
            let (x0, y0, x1, y1) = p.screen_rect(*region);
            let r = Rect::new(x0, y0, x1, y1).intersect(target);
            r.width().max(0.0) * r.height().max(0.0)
        })
        .sum();
    (covered, target.width().max(0.0) * target.height().max(0.0))
}

#[serial(pdfium)]
#[test]
fn zoomed_pages_draw_from_reusable_worker_tiles_matching_full_renders() {
    eprintln!("phase zoomed_pages_are_drawn_from_tiles_past_the_old_bitmap_cap");
    'phase: {
        let Some(app) = open_latin_raster() else {
            break 'phase;
        };
        let mut app = zoom_without_frame(app, 8.0);
        let (_, _, settled) = settle_tiles(&mut app);
        let tiles = settled.values().next().expect("the zoomed page is tiled");
        let (pw, _) = crate::tiles::page_pixels(tiles.placement.size, tiles.placement.level);
        assert!(
            pw > 4096,
            "the page is rendered {pw} px wide, past the whole-page cap"
        );
        let (covered, target) = tile_coverage(&app, tiles);
        assert!(target > 0.0);
        assert!(
            (covered - target).abs() / target < 1e-6,
            "visible tiles cover the viewport exactly: {covered} of {target}"
        );
        assert!(tiles.tiles.iter().all(|(r, image)| {
            image.width == r.width && image.height == r.height && r.width <= 512 // allow-breakpoint: a tile raster cap, not a layout breakpoint
        }));
        assert_eq!(tile_levels(&app), vec![tiles.placement.level]);
    }

    eprintln!("phase returning_to_a_zoom_level_reuses_its_tiles");
    'phase: {
        let Some(app) = open_latin_raster() else {
            break 'phase;
        };
        let mut app = zoom_without_frame(app, 8.0);
        let (_, _, at_eight) = settle_tiles(&mut app);
        let count: usize = at_eight.values().map(|t| t.tiles.len()).sum();
        assert!(count > 0, "the zoomed viewport is drawn from tiles");
        let eight = at_eight.values().next().unwrap().placement.level;
        let scroll = (app.scroll_x(), app.scroll_y());

        rezoom_without_frame(&mut app, 5.0);
        let (_, _, at_five) = settle_tiles(&mut app);
        let five = at_five
            .values()
            .next()
            .expect("5x is tiled")
            .placement
            .level;
        assert_ne!(eight, five);
        assert_eq!(tile_levels(&app), {
            let mut levels = vec![eight, five];
            levels.sort_unstable();
            levels
        });

        rezoom_without_frame(&mut app, 8.0);
        app.viewport.set_scroll(scroll.0, scroll.1);
        let (_, _, back) = app.rasterize_visible();
        assert!(!app.pending_visuals, "every tile was already here");
        assert_eq!(back.values().map(|t| t.tiles.len()).sum::<usize>(), count);
    }

    eprintln!("phase the_owner_worker_fills_a_zoomed_viewport_with_tiles");
    'phase: {
        let mut app = PdfApp::empty();
        let mut st = crate::scenarios::record_state();
        app.queue_worker_open(std::path::Path::new(LATIN_PDF));
        tilo_test_env::wait_until("the fixture to open", || {
            if app.pending_open.is_none() {
                return true;
            }
            app.sync_settled(&mut st.doc, &st.root);
            false
        });
        if !app.has_document {
            assert!(!require_pdfium(), "fixture must open: {:?}", app.load_error);
            eprintln!("SKIPPING the worker tiled-zoom test: {NO_PDFIUM}");
            break 'phase;
        }
        let mut app = framed_at_zoom(app, st, 1.0, None);
        tilo_test_env::wait_until("the page at 100 %", || {
            if !app.raster_images.is_empty() {
                return true;
            }
            full_frame(&mut app, 1280.0, 800.0, None);
            false
        });
        app.fit = FitMode::Custom(8.0);
        app.viewport.set_zoom(8.0);

        let mut saw_placeholder_while_pending = false;
        let placement =
            tilo_test_env::wait_for("the worker to fill the viewport with tiles", || {
                let (placeholders, _, tiles) = app.rasterize_visible();
                let page = tiles.values().next().expect("the zoomed page is tiled");
                let (covered, target) = tile_coverage(&app, page);
                if app.pending_visuals && placeholders.contains_key(&(page.placement.page + 1)) {
                    saw_placeholder_while_pending = true;
                }
                if !app.pending_visuals && (covered - target).abs() / target < 1e-6 {
                    return Some(page.placement);
                }
                let (reader, st) = Framed::split(&mut app);
                reader.sync_settled(&mut st.doc, &st.root);
                None
            });
        assert!(
            saw_placeholder_while_pending,
            "the placeholder showed while tiles were on their way"
        );
        let canvas = app.canvas_rect;
        let visible: Vec<_> =
            crate::tiles::wanted_tiles(&[placement], (canvas.x0, canvas.y0, canvas.x1, canvas.y1))
                .into_iter()
                .filter(|t| t.visible)
                .map(|t| (t.col, t.row))
                .collect();
        assert!(
            app.raster_tile_batch
                .iter()
                .all(|queued| !visible.contains(&(queued.col, queued.row)))
        );
    }

    eprintln!("phase a_tile_matches_the_same_region_of_a_whole_page_render");
    'phase: {
        let loaded = crate::source::open(std::path::Path::new(LATIN_PDF), &ContentStore::memory());
        match pdfium_gate(loaded.is_ok(), require_pdfium()) {
            PdfiumGate::Run => {}
            PdfiumGate::Fail => panic!("TILO_REQUIRE_PDFIUM is set and there is {NO_PDFIUM}"),
            PdfiumGate::Skip => {
                eprintln!("SKIPPING the tile-versus-page render check: {NO_PDFIUM}");
                break 'phase;
            }
        }
        let mut rz = loaded.expect("the fixture opens").raster;
        let plan: Vec<_> = (1..=rz.page_count())
            .map(|page| crate::markup::PagePlanEntry {
                id: format!("source-{page}"),
                source_page: page,
                imported: None,
                rotation: if page == 1 { 90 } else { 0 },
            })
            .collect();
        assert!(rz.reconcile_page_plan(&plan));
        let level = 8; // 4 px per point.
        let tile = rz.render_tile(0, level, 1, 1).expect("tile");
        let engine = tilo_pdf::Engine::shared().expect("engine");
        let doc = engine
            .open_path(std::path::Path::new(LATIN_PDF))
            .expect("open fixture");
        let whole = doc
            .render(
                0,
                &tilo_pdf::RenderOptions {
                    scale: crate::tiles::level_scale(level),
                    rotation: tilo_pdf::Rotation::from_degrees(90).unwrap(),
                    ..tilo_pdf::RenderOptions::default()
                },
            )
            .expect("whole page");
        let stride = whole.width() as usize * 4;
        let pixels = tile.data.data();
        let mut diff = 0u64;
        for y in 0..tile.height as usize {
            for x in 0..tile.width as usize * 4 {
                let a = pixels[y * tile.width as usize * 4 + x];
                let b = whole.pixels()[(512 + y) * stride + 512 * 4 + x];
                diff += u64::from(a.abs_diff(b));
            }
        }
        let mean = diff as f64 / f64::from(tile.width * tile.height * 4);
        assert!(mean < 1.0, "tile differs from the page region by {mean}");
        assert!(pixels.chunks_exact(4).any(|px| px[0] < 128));
    }
}

#[serial(clipboard, pdfium)]
#[test]
fn raster_text_layer_selects_copies_and_searches_real_glyphs() {
    eprintln!("phase text_selection_drag_copies_the_real_run");
    'phase: {
        let Some(mut app) = open_raster_sample() else {
            break 'phase;
        };
        let mut st = crate::scenarios::record_state();

        assert!(
            app.raster_text.is_empty(),
            "no eager text layer before a selection"
        );
        assert_eq!(
            app.begin_selection(1, (100.0, 100.0), Point::new(100.0, 100.0)),
            SelectionStart::Waiting,
            "the press waits for the layer"
        );
        assert!(
            app.raster_text_pending.contains(&0),
            "the press asked for it"
        );

        let layer = text_layer(&mut app, &mut st, 1);
        let mut printable: Vec<_> = layer
            .chars
            .iter()
            .filter(|c| c.rect.w > 1.0 && c.rect.h > 1.0)
            .copied()
            .collect();
        assert!(printable.len() >= 4, "page 1 has a run of printable glyphs");
        printable.sort_by(|a, b| {
            (a.rect.y, a.rect.x)
                .partial_cmp(&(b.rect.y, b.rect.x))
                .unwrap()
        });
        let start = printable[0].rect.center();
        let end = printable[printable.len().min(6) - 1].rect.center();

        assert_eq!(
            app.begin_selection(1, start, Point::new(start.0, start.1)),
            SelectionStart::Armed,
            "pressing on a glyph starts a selection"
        );
        app.update_selection_at(on_screen(&app, 1, end));
        let sel = app.selection.expect("a selection is armed");
        assert!(
            sel.is_active(),
            "the drag grew the selection past its anchor"
        );

        let text = app.selection_text().expect("the selected run yields text");
        assert!(
            !text.trim().is_empty(),
            "the selected run is real page text: {text:?}"
        );
        assert!(
            app.copy_selection(),
            "copy_selection succeeds on a live selection"
        );
        assert!(
            app.status_note.as_deref().unwrap_or("").contains("Copied"),
            "a copy toast is shown ({:?})",
            app.status_note
        );
    }

    eprintln!("phase raster_text_layer_hit_tests_real_glyphs_and_search_finds_them");
    'phase: {
        let Some(mut app) = open_raster_sample() else {
            break 'phase;
        };
        let mut st = crate::scenarios::record_state();

        let layer = text_layer(&mut app, &mut st, 1);
        assert!(!layer.is_empty(), "page 1 exposes real extracted glyphs");

        let probe = layer
            .chars
            .iter()
            .find(|c| c.rect.w > 1.0 && c.rect.h > 1.0)
            .copied()
            .expect("a printable glyph with a real box");
        let (cx, cy) = probe.rect.center();
        let hit = layer.char_at(cx, cy).expect("hit-test lands on a glyph");
        assert!(
            layer.chars[hit].rect.contains(cx, cy),
            "the hit glyph's box contains the probed point"
        );

        let word = first_word(&layer.text, 4).expect("a searchable word on page 1");
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.recompute_regions();
        app.sync_viewport_frame();
        app.stage_find_for_test(&word);
        settle_until(&mut app, &mut st, |app| app.raster_text_pending.is_empty());
        assert!(
            !app.raster_matches.is_empty(),
            "search finds the real word {word:?} through the pdfium text layer"
        );
        let m = app.raster_matches[0];
        let hit_layer = text_layer(&mut app, &mut st, m.page);
        let got = hit_layer.text_for_range(m.char_start, m.char_start + m.char_len);
        assert_eq!(
            got.to_lowercase(),
            word.to_lowercase(),
            "the hit is the searched word"
        );
        let bands = hit_layer.bands_for_range(m.char_start, m.char_start + m.char_len);
        assert!(
            !bands.is_empty(),
            "the hit yields at least one on-page band"
        );
        let pw = app.pw();
        for b in &bands {
            assert!(
                b.x >= -1.0 && b.x + b.w <= pw + 1.0,
                "band {b:?} sits within the page width {pw}"
            );
            assert!(b.w > 0.0 && b.h > 0.0, "band {b:?} is non-degenerate");
        }
    }
}

fn first_word(text: &str, min_len: usize) -> Option<String> {
    let mut cur = String::new();
    for ch in text.chars() {
        if ch.is_alphabetic() {
            cur.push(ch);
        } else {
            if cur.chars().count() >= min_len {
                return Some(cur);
            }
            cur.clear();
        }
    }
    (cur.chars().count() >= min_len).then_some(cur)
}

mod save_import_tests;

#[serial(pdfium)]
#[test]
fn horizontal_highlight_preview_and_commit_cover_only_the_crossed_line() {
    let Some(mut app) = open_raster_sample() else {
        return;
    };
    let mut state = crate::scenarios::record_state();
    let layer = text_layer(&mut app, &mut state, 1);
    let line = "marsh showed on a given morning.";
    let byte = layer.text.find(line).expect("page 1's closing line");
    let first = layer.text[..byte].chars().count();
    let last = first + line.chars().count() - 1;
    let (a, b) = (layer.chars[first].rect, layer.chars[last].rect);
    let mid = a.y + a.h / 2.0;
    let drag = DragState {
        tool: Tool::Highlight,
        page: 1,
        start: (a.x + 0.5, mid),
        cur: (b.x + b.w - 0.5, mid),
        points: Vec::new(),
    };
    let (preview, anchor) = app.highlight_geometry(&drag);
    assert!(anchor.is_some(), "the highlight is anchored to the text");
    assert_eq!(preview.len(), 1, "nearby lines are not selected");
    assert!(
        preview[0].y <= mid && mid <= preview[0].y + preview[0].h,
        "the band covers the crossed line: {:?}",
        preview[0]
    );
    app.drag = Some(drag);
    app.commit_drag(&mut Edits::lend(&mut state.doc));
    let id = app.selected_anno.as_ref().expect("new highlight selected");
    let stored = PdfMarkup::of(&state.doc)
        .list_annotations()
        .into_iter()
        .find(|annotation| &annotation.id == id)
        .expect("durable highlight");
    let projected = app.page_map(1).annotation_to_card(&stored);
    let AnnoKind::Markup { quads, .. } = projected.kind else {
        unreachable!("the gesture stored a highlight");
    };
    assert_eq!(quads.len(), preview.len());
    for (actual, expected) in quads.iter().zip(&preview) {
        for (a, b) in [
            (actual.x, expected.x),
            (actual.y, expected.y),
            (actual.w, expected.w),
            (actual.h, expected.h),
        ] {
            assert!(
                (a - b).abs() < 1e-3,
                "saved band {actual:?} matches preview {expected:?}"
            );
        }
    }
}

#[test]
fn reader_panels_and_page_fit_apply_ui_scale_once() {
    for scale in [0.75_f32, 1.0, 1.5, 2.0] {
        let (mut app, st) = app_with_state();
        app.fit = FitMode::Page;
        let theme = Theme::dark().with_ui_scale(scale);
        let mut frame = framed_with_theme(
            app,
            st,
            960.0 * f64::from(scale),
            810.0,
            None,
            theme.clone(),
        );
        assert!(!frame.overlay_mode, "panels dock at {scale}×");
        assert!(frame.show_rail && frame.show_sidebar);
        assert!((frame.sidebar_rect.width() - 300.0 * f64::from(scale)).abs() < 0.01);
        assert!((frame.rail_rect.width() - 112.0 * f64::from(scale)).abs() < 0.01);
        let origin = frame.page_origins()[0];
        let card = render::page_card_rect(origin, frame.zoom(), frame.pw(), frame.ph());
        let gutter = f64::from(theme.tokens.spacing.s3);
        assert!(card.x0 >= frame.canvas_rect.x0 + gutter - 0.01);
        assert!(card.x1 <= frame.canvas_rect.x1 - gutter + 0.01);
        assert!(card.y0 >= frame.canvas_rect.y0 + gutter - 0.01);
        assert!(card.y1 <= frame.canvas_rect.y1 - gutter + 0.01);
        for view in frame.comment_views.borrow_mut().values_mut() {
            assert!(
                (view.editor_mut().body_size() - theme.text(TextRole::Body).size).abs() < 0.01,
                "comment body follows UI scale at {scale}×"
            );
        }
        full_frame(&mut frame, 960.0 * f64::from(scale), 810.0, None);
        assert!((frame.rail_rect.width() - 112.0 * f64::from(scale)).abs() < 0.01);
    }
}

#[test]
fn comment_text_starts_on_the_author_name_edge_at_any_scale() {
    for scale in [1.0_f32, 2.0] {
        let mut app = PdfApp::blank_document(3);
        app.layout_theme = Theme::dark().with_text_scale(scale);
        let theme = app.layout_theme.clone();
        let mut tr = ui::text_render::TextRenderer::new();
        let card_w = f64::from(theme.tokens.metrics.inspector_width)
            - 2.0 * f64::from(theme.tokens.spacing.s3);
        let body = CardFace::new().body_rect(&mut tr, &theme, Rect::new(0.0, 0.0, card_w, 400.0));
        let hdr =
            PdfApp::comment_header_layout(body, PdfApp::resolve_button_w(&mut tr, &theme), &theme);
        let rect = app.rich_text_rect(&mut tr, hdr.text_x, hdr.body_top, 200.0);
        let mut view = RichTextView::new();
        view.show(&[ui::widget::BlockContent::paragraph(
            "a",
            "Should we spell it out?",
        )]);
        view.layout(rect, &theme, &mut tr);
        let caret = view
            .editor_mut()
            .blocks()
            .first()
            .map(|b| b.line.caret_rect(&theme, 0).x0)
            .unwrap();
        assert!(
            (caret - hdr.text_x).abs() < 0.5,
            "body text at {caret} starts on the name edge {} at {scale}×",
            hdr.text_x
        );
    }
}

#[test]
fn comment_cards_centre_wrap_and_never_overlap_at_any_scale() {
    eprintln!("phase comment_header_never_overlaps_even_with_a_long_name");
    let long = "ada.van.der.berg.reviewer@really-long-domain.example.com";
    for scale in [1.0_f32, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut tr = ui::text_render::TextRenderer::new();
        let card_w = f64::from(theme.tokens.metrics.inspector_width)
            - 2.0 * f64::from(theme.tokens.spacing.s3);
        let card = Rect::new(0.0, 0.0, card_w, 400.0);
        let body = CardFace::new().body_rect(&mut tr, &theme, card);
        let resolve_w = PdfApp::resolve_button_w(&mut tr, &theme);
        let hdr = PdfApp::comment_header_layout(body, resolve_w, &theme);
        assert!(
            hdr.avatar.x1 <= hdr.name.x0,
            "avatar before the name at {scale}×"
        );
        assert!(
            hdr.name.x1 <= hdr.resolve.x0,
            "name before Resolve at {scale}×"
        );
        assert!(
            hdr.resolve.x1 <= hdr.delete.x0,
            "Resolve before delete at {scale}×"
        );
        assert!(
            hdr.delete.x1 <= body.x1 + 0.01,
            "delete inside the card at {scale}×"
        );
        for r in [hdr.avatar, hdr.resolve, hdr.delete] {
            assert!(
                (r.center().y - hdr.name.center().y).abs() < 0.5,
                "{r:?} shares the header's centre line at {scale}×"
            );
        }
        assert!(
            hdr.body_top >= hdr.delete.y1,
            "the body starts under the header"
        );
        let style = theme.text(TextRole::Label).strong();
        let shown = tr.elide_to_width_style(long, hdr.name.width() as f32, &style);
        let (w, _) = tr.measure_style(&shown, None, &style);
        assert!(
            f64::from(w) <= hdr.name.width() + 0.5,
            "the elided name ({shown:?}, {w}px) fits its {}px at {scale}×",
            hdr.name.width()
        );
        assert!(shown.chars().count() < long.chars().count());
    }

    eprintln!("phase comment_card_measures_what_it_paints");
    {
        let app = crate::scenarios::sample_reader();
        let theme = ui::Theme::dark().with_text_scale(2.0);
        let mut tr = ui::text_render::TextRenderer::new();
        let mut comment = crate::model::Comment::new(
            crate::model::PdfAnchor::default(),
            "An exceptionally long review author name".into(),
            String::new(),
            "A deliberately long comment body that wraps across the narrow review panel at two-times text size.".into(),
        );
        comment.replies.push(
            ui::tilo_resource::Id::from_raw("high-text-reply-one"),
            Reply::new(
                "Bob".into(),
                String::new(),
                "A long reply that also wraps, proving the thread is allotted its own measured space.".into(),
            ),
        );
        let cm = Thread {
            id: "high-text-thread".into(),
            page: 1,
            comment,
        };
        let card_w = 276.0;
        let m = app.comment_metrics(&mut tr, &theme, &cm, card_w);
        let card = Rect::new(0.0, 0.0, card_w, m.card_h);
        let body = CardFace::new().body_rect(&mut tr, &theme, card);
        let hdr =
            PdfApp::comment_header_layout(body, PdfApp::resolve_button_w(&mut tr, &theme), &theme);
        let rhythm = &theme.tokens.spacing.rhythm;
        let label = f64::from(theme.text(TextRole::Label).line_height_px());
        let mut y = hdr.body_top + m.body_h;
        for h in &m.reply_heights {
            y += f64::from(rhythm.sibling) + label + f64::from(rhythm.tight) + h;
        }
        y += m.compose_h;
        assert!(
            y <= body.y1 + 0.5,
            "the reply button ({y}) stays inside the measured card body ({})",
            body.y1
        );
    }
}
#[test]
fn a_recorded_reading_position_is_taken_up_once_and_written_back() {
    let mut app = PdfApp::blank_document(3);
    app.canvas_rect = Rect::new(0.0, 0.0, 800.0, 600.0);
    app.sync_viewport_frame();
    let page_id = app.page_id_of(3).expect("the sample has a third page");
    app.pending_reading = Some((page_id.clone(), 0.5, 1.5));
    app.position_restored = false;
    app.restore_reading();
    assert_eq!(app.current_page, 3);
    assert!((app.zoom() - 1.5).abs() < 1e-6, "the zoom is restored");
    let (recorded_page, fraction, zoom) = app
        .current_reading()
        .expect("the restored place is readable");
    assert_eq!(recorded_page, page_id);
    assert!((fraction - 0.5).abs() < 0.02, "half way down the page");
    assert!((zoom - 1.5).abs() < 1e-6);
    assert!(
        app.pending_reading.is_none() && app.position_restored,
        "one window takes the hint once"
    );
}

#[test]
fn opening_with_no_recorded_place_writes_none_until_the_reader_moves() {
    let mut app = PdfApp::blank_document(3);
    app.canvas_rect = Rect::new(0.0, 0.0, 800.0, 600.0);
    app.sync_viewport_frame();
    app.pending_reading = None;
    app.position_restored = false;
    app.restore_reading();
    assert!(app.current_reading().is_some());
    assert_eq!(
        app.last_reading,
        app.current_reading(),
        "the opening place counts as already kept"
    );
}
