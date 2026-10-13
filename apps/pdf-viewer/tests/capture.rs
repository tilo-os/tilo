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
    assert!(
        app.harness()
            .wait_for_visuals(Duration::from_millis(30))
            .is_err()
    );
    assert_eq!(content.put(&bytes).ok(), Some(hash));
    app.harness()
        .wait_for_visuals(tilo_test_env::hang_bound())
        .expect("arrival of imported bytes completes page rendering");
}

#[test]
fn every_scenario_settles_into_a_capture() {
    for scenario in <PdfViewerShell as ui::framework::ResourceApp>::scenarios() {
        TestApp::<PdfViewerShell>::capture(Some(scenario))
            .unwrap_or_else(|error| panic!("scenario {scenario} captures: {error}"));
    }
}
