// Use the framework-attached Dispatcher and the editor's real body dispatcher,
// not a replacement apply closure. No broker consumes this queue: the document
// must already contain each edit while its observation is still pending.
fn notes_capture_framework() -> (
    ui::prelude::TestApp<crate::NotesShell>,
    Dispatcher<crate::notebook::Notebook>,
    AutoCommit,
) {
    let mut framework = ui::prelude::TestApp::<crate::NotesShell>::new().unwrap();
    // NotesApp is the window the notebook's view lays out; its main panel is
    // not the root widget exposed by with_widget. Read the attached window
    // directly, just as the framework's input tests do.
    let dispatcher = framework
        .surface_mut::<crate::app::NotesApp>()
        .main_panel_mut()
        .notebook
        .clone()
        .expect("the framework attaches the editor's notebook dispatcher");
    let doc = framework.read_document(Clone::clone);
    (framework, dispatcher, doc)
}

fn assert_notes_capture<R: ui::tilo_resource::Resource>(
    queue: &ui::local_document_action::Queue,
    doc: &mut AutoCommit,
    before: &[automerge::ChangeHash],
    resource: &str,
    expected: &ui::tilo_resource::Invoke<R>,
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
fn local_capture_notes_tag_edits_are_immediate() {
    use crate::{actions, notebook::Notebook};
    use ui::tilo_resource::Invoke;

    let (_framework, dispatcher, mut doc) = notes_capture_framework();
    let tag = fresh_page_id();
    let before = doc.get_heads();
    let queue = ui::local_document_action::Queue::new();
    // This opaque revision stands for the directory frozen before the input;
    // neither draining the observation nor a broker reply applies the edit.
    let _scope = queue.enter("notes-pre-input-directory".into());
    let create = actions::new_tag_action(tag, "Draft");
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &create)
        .unwrap();
    assert_eq!(crate::read::list_tags(&doc)[0].name, "Draft");
    let created = assert_notes_capture(
        &queue,
        &mut doc,
        &before,
        "notes.notebook",
        &Invoke::<Notebook>::apply(&create).unwrap(),
    );
    assert!(!created.mutation_hashes.is_empty());
    assert_eq!(created.encoded, actions::encode_tag(&create));

    let before = doc.get_heads();
    let delete = actions::delete_tag_action(tag);
    dispatcher
        .apply_now(&mut Edits::lend(&mut doc), &delete)
        .unwrap();
    assert!(crate::read::list_tags(&doc).is_empty());
    let deleted = assert_notes_capture(
        &queue,
        &mut doc,
        &before,
        "notes.notebook",
        &Invoke::<Notebook>::apply(&delete).unwrap(),
    );
    assert!(!deleted.mutation_hashes.is_empty());
    assert_eq!(deleted.original_heads, created.mutation_hashes);
    assert_ne!(deleted.invocation, created.invocation);
}

#[test]
fn local_capture_notes_reads_do_not_invent_mutations() {
    let (_framework, _dispatcher, doc) = notes_capture_framework();
    let queue = ui::local_document_action::Queue::new();
    let _scope = queue.enter("notes-pre-input-directory".into());
    assert!(crate::read::list_tags(&doc).is_empty());
    assert!(queue.drain().is_empty(), "reading is not a local action");
}
