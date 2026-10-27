//! Exercise the production owner-worker backend, not the synchronous unit-test backend.
#![allow(clippy::expect_used, reason = "test assertions")]

use std::time::Duration;
use tilo_content::ContentStore;
use ui::framework::testing::TestApp;

use pdf_viewer::PdfViewerShell;
use pdf_viewer::markup::{ImportedPage, PagePlanEntry};

fn require_raster_worker(path: &std::path::Path) {
    use pdf_viewer::source::{RasterEvent, RasterHandle};
    // Opening text successfully does not qualify the native raster pipeline.
    // Probe the actual owner thread, never bind PDFium from the test thread.
    let worker = RasterHandle::new(ContentStore::memory()).expect("start PDF owner worker");
    let request = worker.open_path(path).expect("queue PDF fixture open");
    let result = tilo_test_env::wait_for("the PDF fixture open", || {
        worker.drain_events().into_iter().find(|event| {
            matches!(event,
                RasterEvent::Opened { request: completed, .. }
                    | RasterEvent::OpenFailed { request: completed, .. }
                    if completed.id == request.id)
        })
    });
    assert!(
        matches!(result, RasterEvent::Opened { .. }),
        "capture test requires the native PDFium backend; install its runtime or set TILO_PDFIUM_LIB to a compatible library (owner result: {result:?})"
    );
}

#[test]
fn capture_waits_for_production_visual_work() {
    let path = std::path::Path::new(env!("OUT_DIR")).join("sample.pdf");
    require_raster_worker(&path);
    let profile = tempfile::tempdir().expect("profile");
    let mut app = TestApp::<PdfViewerShell>::open_data_dir(profile.path())
        .and_then(|app| app.relaunch(&[path.to_str().expect("utf-8 fixture path")]))
        .expect("the reader launches on the fixture");
    app.harness()
        .wait_for_visuals(tilo_test_env::hang_bound())
        .expect("production document open and visible images finish");
    assert!(
        !app.harness().resources().is_empty(),
        "opened PDF exposes its resources"
    );
    let opened = std::fs::read(&path).expect("opened bytes");
    assert_eq!(
        app.state().pinned_source().map(|source| source.hash),
        Some(tilo_content::hash_bytes(&opened)),
        "the record pins the bytes its markup is made against"
    );
    // The profile's content store is the one the reader keeps documents in.
    let content = ContentStore::in_dir(profile.path().join("content"));
    assert!(
        content.has(&tilo_content::hash_bytes(&opened)),
        "the opened document is kept in the content-addressed store"
    );

    // Arrival of an imported payload must invalidate the materialised plan,
    // even though the CRDT plan itself does not change. (The opened file is
    // itself pinned in the store, so the import is a different PDF.)
    let bytes = std::fs::read(std::path::Path::new(env!("OUT_DIR")).join("latin.pdf"))
        .expect("fixture bytes");
    let hash = tilo_content::hash_bytes(&bytes);
    assert!(
        !content.has(&hash),
        "the imported payload must be absent before exercising hydration"
    );
    app.invoke(pdf_viewer::actions::import_pages_action(
        Some("source-1"),
        vec![PagePlanEntry {
            id: "late-import".into(),
            source_page: 1,
            imported: Some(ImportedPage {
                hash: hash.clone(),
                kind: "pdf".into(),
                page: 1,
                width_milli: 612_000,
                height_milli: 792_000,
            }),
            rotation: 0,
        }],
    ))
    .expect("the owner imports pages");
    app.harness().relayout();
    // Without its bytes the imported page cannot finish, however long a
    // frame waited: one frame shows the work still pending.
    assert!(app.harness().wait_for_visuals(Duration::ZERO).is_err());
    assert_eq!(content.put(&bytes).ok(), Some(hash));
    app.harness()
        .wait_for_visuals(tilo_test_env::hang_bound())
        .expect("arrival of imported bytes completes page rendering");
}

#[test]
fn scaled_review_draws_no_header_and_keeps_docked_panels() {
    use ui::Widget;
    let mut app = TestApp::<PdfViewerShell>::capture_at(None, (1440, 810)).expect("review capture");
    let theme = ui::Theme::dark().with_ui_scale(1.5);
    app.harness().set_theme(theme);
    app.harness().relayout();
    let reader = app.surface_mut::<pdf_viewer::PdfApp>();
    assert!(
        reader.get_rect().y0.abs() < f64::EPSILON,
        "no separate host History bar"
    );
    let tree = reader.accessibility().expect("reader semantics");
    let canvas = tree
        .children
        .iter()
        .flat_map(|node| &node.children)
        .find(|node| node.role == ui::a11y::Role::Document)
        .and_then(|node| node.bounds)
        .expect("page canvas")
        .to_rect();
    assert!(
        canvas.y0.abs() < 0.01,
        "the pages start at the top: the controls are the window bar's"
    );
    assert!(
        (canvas.x0 - 168.0).abs() < 0.01,
        "scaled page thumbnails remain docked"
    );
    assert!(
        (canvas.x1 - 990.0).abs() < 0.01,
        "review panel grows once, not twice"
    );
}

#[test]
fn every_scenario_settles_into_a_capture() {
    for scenario in <PdfViewerShell as ui::framework::ResourceApp>::scenarios() {
        TestApp::<PdfViewerShell>::capture(Some(scenario))
            .unwrap_or_else(|error| panic!("scenario {scenario} captures: {error}"));
    }
}
