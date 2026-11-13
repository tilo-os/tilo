//! A reader opened on no document offers the documents its workspace holds:
//! a paper an agent filled in a hosted workspace is one click away, and a
//! click opens it in this window on its own markup record.

#![allow(clippy::expect_used, reason = "test assertions")]

use ui::framework::testing::TestApp;
use ui::widget::Widget as _;

use pdf_viewer::PdfViewerShell;
use pdf_viewer::app::{PdfApp, WorkspaceDocument};
use pdf_viewer::markup::PdfMarkup;

/// The identity a document's markup record takes, as the reader derives it.
fn markup_record(path: &str) -> ui::record::RecordId {
    ui::record::RecordId::derived("annotations", &ui::record::RecordId::for_file(path))
}

/// A paper as the workspace holds it: the markup record its file gets, three
/// pages of plan, its source pinned under its name — a record the relay
/// published, which no window of this device started.
fn paper(pages: usize, hash: [u8; 32], len: u64, name: &str) -> ui::tilo_resource::Doc {
    let mut doc = ui::tilo_resource::Doc::new();
    <PdfMarkup as ui::tilo_resource::Resource>::initialize(&mut doc)
        .expect("an empty markup record");
    let source = ui::tilo_resource::Bytes::Blob {
        hash,
        len,
        sealed: None,
    };
    for action in [
        PdfMarkup::START_PAGE_PLAN.with(pages),
        pdf_viewer::actions::pin_source_action(&source, name),
    ] {
        apply(&mut doc, action);
    }
    doc
}

/// The content hash some test record pins: bytes no device here holds.
fn absent_hash() -> [u8; 32] {
    let mut hash = [0xb3_u8; 32];
    hash[1] = 0xa1;
    hash
}

/// Apply `action` the way the record's own window would.
fn apply(doc: &mut ui::tilo_resource::Doc, action: ui::tilo_resource::Invoke<PdfMarkup>) {
    let mut replica = ui::tilo_resource::Replica::<PdfMarkup>::from_doc(std::mem::take(doc));
    replica.apply(action).expect("the record takes the action");
    *doc = std::mem::take(replica.doc_mut());
}

/// `application.pdf`, as the workspace holds it, and its identity.
fn application_pdf() -> (&'static str, ui::record::RecordId, ui::tilo_resource::Doc) {
    (
        "application.pdf",
        markup_record("/papers/application.pdf"),
        paper(3, absent_hash(), 4096, "application.pdf"),
    )
}

#[test]
fn the_empty_reader_offers_the_workspaces_pdf_and_a_click_opens_it() {
    let (name, id, doc) = application_pdf();
    let mut app = TestApp::<PdfViewerShell>::with_named_records(&mut [(name, id.clone(), doc)])
        .expect("a window on the workspace the paper is in");
    // The rows are where the draw pass put them: draw one.
    app.settle();
    assert!(
        app.texts().iter().any(|painted| painted == name),
        "the empty reader lists the paper the workspace holds: {:?}",
        app.texts()
    );

    // The row a person presses is the one the accessibility tree names.
    let (at, target) = app
        .with_widget::<PdfApp, _>(|reader| {
            let offers = reader
                .accessibility()
                .expect("the reader says what it shows");
            let row = offers
                .children
                .iter()
                .find(|node| node.name.as_deref() == Some(name))
                .expect("the listed paper is a control a person can press");
            (
                row.bounds
                    .expect("the row is drawn somewhere")
                    .to_rect()
                    .center(),
                row.action_target.clone(),
            )
        })
        .expect("the reader is up");
    assert_eq!(
        target.as_deref(),
        Some(format!("pdf.empty.document:{}", id.uri()).as_str()),
        "the row opens its own record"
    );
    app.harness().click_at(at.x, at.y);
    app.settle();
    app.harness().relayout();
    assert_eq!(
        app.record(),
        &id,
        "the click opened the paper in this window, on its own markup record"
    );
}

#[test]
fn an_empty_workspace_keeps_the_empty_state_it_had() {
    let mut app = TestApp::<PdfViewerShell>::new().expect("a reader");
    let listed: Vec<WorkspaceDocument> = app
        .with_widget::<PdfApp, _>(|reader| reader.workspace_documents().to_vec())
        .expect("the reader is up");
    assert!(listed.is_empty(), "a workspace with no papers lists none");
    let said = app.texts();
    assert!(
        said.iter().any(|name| name == "No document open"),
        "and the empty state says what it is: {said:?}"
    );
    assert!(
        said.iter().any(|name| name == "Open PDF"),
        "and offers the one action it had: {said:?}"
    );
}

/// The two states a person meets, written as PNGs for looking at the pixels:
/// the reader with two papers in the workspace, and with none. Their paths
/// are printed; they land in this test's target directory.
#[test]
#[ignore = "writes the empty state's PNGs for review, it asserts nothing"]
fn capture_the_empty_state_with_and_without_the_workspaces_documents() {
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
    let (name, id, doc) = application_pdf();
    let mut with = TestApp::<PdfViewerShell>::with_named_records(&mut [
        (name, id, doc),
        (
            "contract-2026.pdf",
            markup_record("/papers/contract-2026.pdf"),
            paper(12, [0xc0; 32], 96_512, "contract-2026.pdf"),
        ),
    ])
    .expect("a reader on the workspace the papers are in");
    with.harness().resize((1440, 900)).expect("a window");
    with.settle();
    with.harness().render_png(&out.join("pdf-empty-two.png"));
    let mut none = TestApp::<PdfViewerShell>::new().expect("a reader on an empty workspace");
    none.harness().resize((1440, 900)).expect("a window");
    none.settle();
    none.harness().render_png(&out.join("pdf-empty-none.png"));
    eprintln!("captures in {}", out.display());
}
