//! Exercise the production owner-worker backend, not the synchronous unit-test backend.
#![allow(clippy::expect_used, reason = "test assertions")]

use std::time::Duration;
use ui::headless::FrameHarness;

fn require_raster_worker(path: &std::path::Path) {
    use pdf_viewer::source::{RasterEvent, RasterHandle};
    // Opening text successfully does not qualify the native raster pipeline.
    // Probe the actual owner thread, never bind PDFium from the test thread.
    let worker = RasterHandle::new().expect("start PDF owner worker");
    let request = worker.open_path(path).expect("queue PDF fixture open");
    let started = std::time::Instant::now();
    let result = loop {
        if let Some(event) = worker.drain_events().into_iter().find(|event| {
            matches!(event,
                RasterEvent::Opened { request: completed, .. }
                    | RasterEvent::OpenFailed { request: completed, .. }
                    if completed.id == request.id)
        }) {
            break event;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "PDF fixture open did not settle"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(
        matches!(result, RasterEvent::Opened { .. }),
        "capture test requires the native PDFium backend; install its runtime or set TILO_PDFIUM_LIB to a compatible library (owner result: {result:?})"
    );
}

#[test]
fn production_open_completes_visual_work() {
    use qdoc::blobstore::{self, MemoryBlobStore};
    blobstore::set_store(std::sync::Arc::new(MemoryBlobStore::default()));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf");
    require_raster_worker(&path);
    let mut harness = FrameHarness::new_with_theme(
        ui::State::new(),
        pdf_viewer::app_root_open(&path),
        (1280, 800),
        ui::theme::Theme::dark(),
    )
    .with_records(ui::RecordSet::in_memory(), automerge::AutoCommit::new());
    harness
        .wait_for_visuals(Duration::from_secs(15))
        .expect("production document open and visible images finish");
    assert!(
        !harness.resources().is_empty(),
        "opened PDF exposes its resources"
    );
    // Arrival of an imported payload must invalidate the materialised plan,
    // even though the CRDT plan itself does not change. (The opened file is
    // itself pinned in the store, so the import is a different PDF.)
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/doc/pdf/tests/fixtures/latin.pdf"),
    )
    .expect("fixture bytes");
    assert!(
        blobstore::store().has(&blobstore::hash_bytes(
            &std::fs::read(&path).expect("opened bytes")
        )),
        "the opened document is kept in the content-addressed store"
    );
    let hash = blobstore::hash_bytes(&bytes);
    assert!(
        !blobstore::store().has(&hash),
        "the imported payload must be absent before exercising hydration"
    );
    pdf_viewer::PdfDoc::import_pages(
        &mut harness.state_mut().doc,
        Some("source-1"),
        &[pdf_viewer::doc::PagePlanEntry {
            id: "late-import".into(),
            source_page: 1,
            imported: Some(pdf_viewer::doc::ImportedPage {
                hash: hash.clone(),
                kind: "pdf".into(),
                page: 1,
                width_milli: 612_000,
                height_milli: 792_000,
            }),
            rotation: 0,
        }],
    );
    harness.relayout();
    assert!(harness.wait_for_visuals(Duration::from_millis(30)).is_err());
    assert_eq!(blobstore::put(&bytes), Some(hash));
    harness
        .wait_for_visuals(Duration::from_secs(15))
        .expect("arrival of imported bytes completes page rendering");
}

#[test]
fn unfinished_visual_work_fails_capture() {
    let mut harness = FrameHarness::new_with_theme(
        ui::State::new(),
        pdf_viewer::app_root_loading(),
        (1280, 800),
        ui::theme::Theme::dark(),
    )
    .with_records(ui::RecordSet::in_memory(), automerge::AutoCommit::new());
    assert!(harness.wait_for_visuals(Duration::from_millis(5)).is_err());
}
