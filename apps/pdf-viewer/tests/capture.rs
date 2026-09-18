//! Exercise the production owner-worker backend, not the synchronous unit-test backend.
#![allow(clippy::expect_used, reason = "test assertions")]

use std::time::Duration;
use ui::headless::FrameHarness;

#[test]
fn production_open_completes_visual_work() {
    use qdoc::blobstore::{self, MemoryBlobStore};
    blobstore::set_store(std::sync::Arc::new(MemoryBlobStore::default()));
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf");
    let mut harness = FrameHarness::new_with_theme(
        ui::State::new(),
        pdf_viewer::app_root_open(&path),
        (1280, 800),
        ui::theme::Theme::dark(),
    );
    harness
        .wait_for_visuals(Duration::from_secs(15))
        .expect("production document open and visible images finish");
    assert!(
        !harness.resources().is_empty(),
        "opened PDF exposes its resources"
    );
    // Arrival of an imported payload must invalidate the materialised plan,
    // even though the CRDT plan itself does not change.
    let bytes = std::fs::read(path).expect("fixture bytes");
    let hash = blobstore::hash_bytes(&bytes);
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
    );
    assert!(harness.wait_for_visuals(Duration::from_millis(5)).is_err());
}
