// Use the framework-attached Dispatcher and the editor's real body dispatcher,
// not a replacement apply closure. No broker consumes this queue: the document
// must already contain each edit while its observation is still pending.
fn notes_capture_framework() -> (
    ui::prelude::TestApp<crate::NotesShell>,
    Dispatcher<crate::notebook::Notebook>,
    AutoCommit,
) {
    let mut framework = ui::prelude::TestApp::<crate::NotesShell>::new().unwrap();
    let dispatcher = framework
        .with_widget::<MainPanel, _>(|panel| panel.notebook.clone())
        .flatten()
        .expect("the framework attaches the editor's notebook dispatcher");
    let doc = framework.read_document(Clone::clone);
    (framework, dispatcher, doc)
}

fn assert_notes_capture<R: ui::tilo_resource::Resource>(
    queue: &ui::local_document_action::Queue,
    doc: &mut AutoCommit,
    before: &[automerge::ChangeHash],
    resource: &str,
    expected: ui::tilo_resource::Invoke<R>,
) -> ui::local_document_action::CapturedAction {
    let captures = queue.drain();
    assert_eq!(captures.len(), 1);
    let capture = &captures[0];
    assert_eq!(capture.revision, "notes-pre-input-directory");
    let published = capture.published.as_ref().expect("canonical intent");
    assert_eq!(published.resource, resource);
    assert_eq!(published.action, expected.id);
    assert_eq!(published.arguments, expected.arguments);
    let mut original = before.iter().map(|hash| hash.0).collect::<Vec<_>>();
    original.sort_unstable();
    assert_eq!(capture.original_heads, original);
    let mut mutations = doc
        .get_changes(before)
        .iter()
        .map(|change| change.hash().0)
        .collect::<Vec<_>>();
    mutations.sort_unstable();
    assert_eq!(capture.mutation_hashes, mutations);
    assert!(queue.take_refusals().is_empty());
    capture.clone()
}

#[test]
fn local_capture_notes_create_rename_and_body_are_immediate() {
    use crate::{actions, notebook::Notebook};
    use ui::tilo_resource::Invoke;

    let (_framework, dispatcher, mut doc) = notes_capture_framework();
    let page = fresh_page_id();
    let before = doc.get_heads();
    let queue = ui::local_document_action::Queue::new();
    // This opaque revision stands for the directory frozen before the input;
    // neither draining the observation nor a broker reply applies the edit.
    let _scope = queue.enter("notes-pre-input-directory".into());
    let create = actions::create_page_action(page, None, "Draft");
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &create)
        .unwrap();
    assert_eq!(crate::read::list_pages(&doc)[0].title, "Draft");
    let created = assert_notes_capture(
        &queue,
        &mut doc,
        &before,
        "notes.notebook",
        Invoke::<Notebook>::apply(&create).unwrap(),
    );
    assert!(!created.mutation_hashes.is_empty());
    assert_eq!(created.encoded, actions::encode(&create));

    let before = doc.get_heads();
    let rename = actions::rename_page_action(page, "Published");
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &rename)
        .unwrap();
    assert_eq!(crate::read::list_pages(&doc)[0].title, "Published");
    let renamed = assert_notes_capture(
        &queue,
        &mut doc,
        &before,
        "notes.notebook",
        Invoke::<Notebook>::apply(&rename).unwrap(),
    );
    assert!(!renamed.mutation_hashes.is_empty());
    assert_eq!(renamed.encoded, actions::encode(&rename));
    assert_eq!(renamed.original_heads, created.mutation_hashes);
    assert_ne!(renamed.invocation, created.invocation);

    let before = doc.get_heads();
    let op = tilo_document::RichTextAction::AppendParagraph {
        text: "Already visible".into(),
    };
    body_dispatcher(&dispatcher, page)
        .apply_now(&mut Edits::lend(&mut doc), &op)
        .unwrap();
    assert!(
        crate::read::page_block_snapshots(&doc, page)
            .iter()
            .any(|(_, text)| text == "Already visible")
    );
    let body = assert_notes_capture(
        &queue,
        &mut doc,
        &before,
        &actions::page_resource_id(page),
        Invoke::<tilo_document::RichText>::apply(&op).unwrap(),
    );
    assert!(!body.mutation_hashes.is_empty());
    assert_eq!(body.original_heads, renamed.mutation_hashes);
    assert_ne!(body.invocation, renamed.invocation);
    // The published child verb must survive wrapping in a parent field edit.
    assert_eq!(body.encoded.type_id, actions::encode(&create).type_id);
    assert_eq!(
        body.published.as_ref().unwrap().action,
        ui::tilo_resource::APPLY
    );
    let applied: serde_json::Value = serde_json::from_slice(&body.encoded.payload).unwrap();
    let expected: serde_json::Value =
        serde_json::from_slice(&actions::encode(&actions::edit_body_action(page, &op)).payload)
            .unwrap();
    assert_eq!(
        applied["op"], expected["op"],
        "the encoded action edits this note's body"
    );
}

#[test]
fn local_capture_notes_same_title_and_reads_do_not_invent_mutations() {
    use crate::{actions, notebook::Notebook};
    use ui::tilo_resource::Invoke;

    let (_framework, dispatcher, mut doc) = notes_capture_framework();
    let page = fresh_page_id();
    dispatcher
        .apply_now(
            &mut Edits::lend(&mut doc),
            &actions::create_page_action(page, None, "Unchanged"),
        )
        .unwrap();
    let before = doc.get_heads();
    let queue = ui::local_document_action::Queue::new();
    let _scope = queue.enter("notes-pre-input-directory".into());
    assert_eq!(crate::read::list_pages(&doc)[0].title, "Unchanged");
    assert_eq!(doc.get_heads(), before);
    assert!(queue.drain().is_empty(), "reading is not a local action");

    let noop = actions::rename_page_action(page, "Unchanged");
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &noop)
        .unwrap();
    assert_eq!(doc.get_heads(), before);
    let captured = assert_notes_capture(
        &queue,
        &mut doc,
        &before,
        "notes.notebook",
        Invoke::<Notebook>::apply(&noop).unwrap(),
    );
    assert!(captured.mutation_hashes.is_empty());
}
