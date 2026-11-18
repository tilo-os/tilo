//! Notes as the framework hosts it, for tests that drive the notebook
//! surface: the app on a fresh profile, a notebook written with its own
//! actions, and the surface itself for its test hooks.
use notes::actions::{self, Action};
use notes::{MainPanel, NotesApp, NotesShell, PageId, PagePart};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

/// Notes on a fresh profile, its notebook empty.
pub fn notes() -> TestApp<NotesShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<NotesShell>::new().unwrap()
}

/// Apply a note's `action` through the notes' chain, as a control does.
pub fn apply(app: &mut TestApp<NotesShell>, action: &Action) {
    app.invoke_child::<PagePart>(&chain_resource(), Invoke::apply(action).unwrap())
        .unwrap();
}

/// Where the notes' chain is published: what a page's action is invoked on.
pub fn chain_resource() -> String {
    notes::part::chain_resource()
}

/// The parts of the notes the window holds, oldest first.
pub fn parts(app: &TestApp<NotesShell>) -> Vec<(u64, std::rc::Rc<PagePart>)> {
    app.chain_parts::<PagePart>()
}

/// Read the document of the part keeping note `page`, as the window holds it.
pub fn part_doc<T>(
    app: &TestApp<NotesShell>,
    page: PageId,
    read: impl FnOnce(&automerge::AutoCommit) -> T,
) -> T {
    let parts = parts(app);
    let index = notes::Pages::of(&parts)
        .part_of(&notes::notebook::page_id(page))
        .expect("a part keeps the note");
    app.read_document_of(&notes::part::chain().part(index), read)
        .expect("the part is held")
}

/// Read the document of the first part of the notes, where a notebook with
/// fewer notes than a part holds keeps them all; an empty part before the
/// first note is made.
pub fn notes_doc<T>(
    app: &TestApp<NotesShell>,
    read: impl FnOnce(&automerge::AutoCommit) -> T,
) -> T {
    if let Some(held) = app.read_document_of(&notes::part::chain().part(0), Clone::clone) {
        return read(&held);
    }
    let mut empty = automerge::AutoCommit::new();
    notes::read::initialize_part(&mut empty);
    read(&empty)
}

/// The heads and unsaved operations of the notebook and each part of the
/// notes: what changes when anything of the notes is written.
pub fn written(app: &TestApp<NotesShell>) -> Vec<(Vec<automerge::ChangeHash>, usize)> {
    use automerge::transaction::Transactable as _;
    let mut held = vec![app.read_document(|doc| (doc.base_heads(), doc.pending_ops()))];
    for (index, _) in parts(app) {
        held.extend(
            app.read_document_of(&notes::part::chain().part(index), |doc| {
                (doc.base_heads(), doc.pending_ops())
            }),
        );
    }
    held
}

/// The versions of the notebook and of every part of the notes, oldest
/// first: what History lists for the notes.
pub fn versions(app: &TestApp<NotesShell>) -> Vec<ui::tilo_resource::history::Version> {
    let mut versions = app.versions();
    for (index, _) in parts(app) {
        versions.extend(app.versions_of(&notes::part::chain().part(index)));
    }
    versions.sort_by_key(|version| version.time_ms);
    versions
}

/// What each of the notes' [`versions`] did through an action, oldest first.
pub fn history(app: &TestApp<NotesShell>) -> Vec<tilo_actions::ActionContext> {
    versions(app)
        .into_iter()
        .filter_map(|version| version.context)
        .collect()
}

/// A new root note titled `title` holding the Markdown `body`, after the
/// notes the notebook already has.
pub fn add_note(app: &mut TestApp<NotesShell>, title: &str, body: &str) -> PageId {
    let page = PageId::new_v4();
    apply(app, &actions::create_page_action(page, None, title));
    if !body.is_empty() {
        let blocks = MainPanel::seed_blocks(body, |_| None);
        apply(app, &actions::write_body_action(page, &blocks));
    }
    page
}

pub fn with_note(title: &str, body: &str) -> (TestApp<NotesShell>, PageId) {
    let mut app = notes();
    let page = add_note(&mut app, title, body);
    open(&mut app, page);
    (app, page)
}

/// Open note `page`, as clicking it in the tree does.
pub fn open(app: &mut TestApp<NotesShell>, page: PageId) {
    surface(app, |notes| notes.navigate_to(page));
}

/// Run `visit` on the notebook surface, then the frames that follow.
pub fn surface<T>(app: &mut TestApp<NotesShell>, visit: impl FnOnce(&mut NotesApp) -> T) -> T {
    app.surface(visit).expect("the notebook surface is mounted")
}

/// The text of `page`'s blocks, in order.
pub fn body(app: &TestApp<NotesShell>, page: PageId) -> Vec<String> {
    part_doc(app, page, |doc| {
        notes::read::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    })
}

/// Deliver `event` to the window, as the live host delivers input.
pub fn send(app: &mut TestApp<NotesShell>, event: &winit::event::WindowEvent) -> bool {
    app.harness()
        .dispatch_to_root_with(event, &winit::event::Modifiers::default())
}

/// Apply rich-text `op` to `page`'s body, as an agent does: one field edit.
pub fn edit_body(app: &mut TestApp<NotesShell>, page: PageId, op: &tilo_document::RichTextAction) {
    let edit = PagePart::paths()
        .pages
        .item(&notes::notebook::page_id(page))
        .body
        .perform(op)
        .unwrap();
    app.invoke_child::<PagePart>(&chain_resource(), Invoke::edit(&edit).unwrap())
        .unwrap();
}

/// The notebook `app` shows, as its window shows it.
pub fn shown(app: &TestApp<NotesShell>) -> notes::Workspace {
    notes::Workspace::of(&app.state(), &parts(app), &notes::Workspace::default())
}

/// The notes at the notebook's root, in their order.
pub fn root_order(app: &TestApp<NotesShell>) -> Vec<PageId> {
    shown(app)
        .children_of(None)
        .iter()
        .map(|page| page.id)
        .collect()
}

/// Deliver what `from` wrote to the notebook and to every part of the notes
/// it holds to `to`, a window on the same notebook, as the workspace's sync
/// does. Whether `to` changed.
pub fn deliver(from: &mut TestApp<NotesShell>, to: &mut TestApp<NotesShell>) -> bool {
    let mut changed = from.deliver_changes_to(to).unwrap();
    for (index, _) in parts(from) {
        changed |= from
            .deliver_resource_to(to, &notes::part::chain().part(index))
            .unwrap();
    }
    changed
}

/// The part keeping note `page`, as another member's replica of it: for a
/// test to edit and [`merge_part`] back.
pub fn peer_part(
    app: &TestApp<NotesShell>,
    page: PageId,
) -> (ui::resource::DocumentId, automerge::AutoCommit) {
    let parts = parts(app);
    let index = notes::Pages::of(&parts)
        .part_of(&notes::notebook::page_id(page))
        .expect("a part keeps the note");
    let id = notes::part::chain().part(index);
    let peer = app.peer_document_of(&id).expect("the part is held");
    (id, peer)
}

/// Exchange edits with a member's replica of part `id`, as sync does.
pub fn merge_part(
    app: &mut TestApp<NotesShell>,
    id: &ui::resource::DocumentId,
    peer: &mut automerge::AutoCommit,
) {
    app.merge_peer_document_of(id, peer).unwrap();
}

/// A member's edit of the part keeping note `page` arriving: `edit` writes
/// on their own replica of it, and the change merges in as sync merges it,
/// never onto this person's undo stack.
pub fn part_edit(
    app: &mut TestApp<NotesShell>,
    page: PageId,
    edit: impl FnOnce(&mut automerge::AutoCommit),
) {
    let (part, _) = peer_part(app, page);
    app.replica_arrives(&part, edit).unwrap();
    app.harness().relayout();
}

/// The heads of the notebook and of every part of the notes: what any
/// write moves.
pub fn stored(app: &TestApp<NotesShell>) -> Vec<Vec<automerge::ChangeHash>> {
    use automerge::transaction::Transactable as _;
    let mut heads = vec![app.read_document(automerge::AutoCommit::base_heads)];
    for (index, _) in parts(app) {
        heads.push(
            app.read_document_of(
                &notes::part::chain().part(index),
                automerge::AutoCommit::base_heads,
            )
            .unwrap(),
        );
    }
    heads
}
