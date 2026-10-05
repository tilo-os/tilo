//! Notes as the framework hosts it, for tests that drive the notebook
//! surface: the app on a fresh profile, a notebook written with its own
//! actions, and the surface itself for its test hooks.
use notes::actions::{self, NotesAction};
use notes::{MainPanel, NotesApp, NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

/// Notes on a fresh profile, its notebook empty.
pub fn notes() -> TestApp<NotesShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<NotesShell>::new().unwrap()
}

/// Apply `action` through the notebook's door, as a control does.
pub fn apply(app: &mut TestApp<NotesShell>, action: &NotesAction) {
    app.invoke(Invoke::apply(action).unwrap()).unwrap();
}

/// A new root note titled `title` holding the Markdown `body`, after the
/// notes the notebook already has.
pub fn add_note(app: &mut TestApp<NotesShell>, title: &str, body: &str) -> PageId {
    let page = PageId::new_v4();
    apply(app, &actions::append_page_action(page, None, title));
    if !body.is_empty() {
        let blocks = MainPanel::seed_blocks(body, |_| None);
        apply(app, &actions::write_body_action(page, &blocks));
    }
    page
}

/// Notes with one note, `title`, open.
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
    app.read_document(|doc| {
        notes::doc::NotesDoc::page_block_snapshots(doc, page)
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

/// The notes at the notebook's root, in their order.
pub fn root_order(app: &TestApp<NotesShell>) -> Vec<PageId> {
    app.state()
        .children_of(None)
        .iter()
        .map(|page| page.id)
        .collect()
}
