#![allow(clippy::expect_used, reason = "test assertions")]

use ui::framework::testing::TestApp;
use ui::widget::Widget as _;

use pdf_viewer::PdfViewerShell;
use pdf_viewer::app::{PdfApp, WorkspaceDocument};
use pdf_viewer::markup::PdfMarkup;

fn markup_resource(path: &str) -> ui::resource::DocumentId {
    ui::resource::DocumentId::derived("annotations", &ui::resource::DocumentId::for_file(path))
}

fn paper(pages: usize, hash: [u8; 32], len: u64, name: &str) -> ui::tilo_resource::Doc {
    let mut doc = ui::tilo_resource::Doc::new();
    <PdfMarkup as ui::tilo_resource::Resource>::initialize(&mut doc)
        .expect("an empty markup resource");
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

fn absent_hash() -> [u8; 32] {
    let mut hash = [0xb3_u8; 32];
    hash[1] = 0xa1;
    hash
}

fn apply(doc: &mut ui::tilo_resource::Doc, action: ui::tilo_resource::Invoke<PdfMarkup>) {
    let mut replica = ui::tilo_resource::Replica::<PdfMarkup>::from_doc(std::mem::take(doc));
    replica
        .apply(action)
        .expect("the resource takes the action");
    *doc = std::mem::take(replica.doc_mut());
}

fn application_pdf() -> (
    &'static str,
    ui::resource::DocumentId,
    ui::tilo_resource::Doc,
) {
    (
        "application.pdf",
        markup_resource("/papers/application.pdf"),
        paper(3, absent_hash(), 4096, "application.pdf"),
    )
}

#[test]
fn the_empty_reader_offers_the_workspaces_pdf_and_a_click_opens_it() {
    let (name, id, doc) = application_pdf();
    let mut app = TestApp::<PdfViewerShell>::with_named_resources(&mut [(name, id.clone(), doc)])
        .expect("a window on the workspace the paper is in");
    app.settle();
    assert!(
        app.texts().iter().any(|painted| painted == name),
        "the empty reader lists the paper the workspace holds: {:?}",
        app.texts()
    );

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
        "the row opens its own document"
    );
    app.harness().click_at(at.x, at.y);
    app.settle();
    app.harness().relayout();
    assert_eq!(
        app.record(),
        &id,
        "the click opened the paper in this window, on its own markup resource"
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
