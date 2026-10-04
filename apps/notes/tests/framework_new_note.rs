//! Creating notes in the framework-hosted app the way a person does: the
//! empty state's "New note" and the sidebar's "+" each create a page, and the
//! notebook the app publishes once it has pages passes the host's contract
//! check, so the palette and agents keep it.

use notes::actions::new_page_action;
use notes::doc::NotesDoc;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn fresh() -> TestApp<NotesShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<NotesShell>::new().unwrap()
}

fn page_count(app: &TestApp<NotesShell>) -> usize {
    app.read_document(|doc| NotesDoc::list_pages(doc).len())
}

#[test]
fn the_empty_state_button_creates_a_note() {
    let mut app = fresh();
    assert_eq!(page_count(&app), 0);
    app.harness().click_named("notes.first_note").unwrap();
    assert_eq!(page_count(&app), 1, "clicking New note creates a page");
}

#[test]
fn enter_on_the_focused_empty_state_button_creates_a_note() {
    let mut app = fresh();
    app.harness()
        .perform_named("notes.first_note", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().key("Enter");
    assert_eq!(page_count(&app), 1, "Enter on New note creates a page");
}

#[test]
fn activating_new_note_as_assistive_technology_does_opens_a_document() {
    let mut app = fresh();
    app.harness()
        .perform_named("notes.first_note", &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    assert_eq!(
        page_count(&app),
        1,
        "an accessibility activation creates a page"
    );
    let roles: Vec<_> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .map(|(_, node)| (node.role(), node.label().map(str::to_owned)))
        .collect();
    assert!(
        roles
            .iter()
            .any(|(role, _)| *role == ui::accesskit::Role::Document),
        "the new note's document is in the accessibility tree: {roles:?}"
    );
}

#[test]
fn the_sidebar_plus_creates_a_root_note() {
    let mut app = fresh();
    let tree = app.harness().targets()["page_tree"];
    // The heading's "+" is the sidebar's own "New note" button; the empty
    // state's button of the same name sits outside the sidebar.
    let plus = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.label() == Some("New note"))
        .filter_map(|(_, node)| node.bounds())
        .find(|bounds| tree.contains(ui::kurbo::Point::new(bounds.x0, bounds.y0)))
        .expect("the sidebar shows a New note button");
    let (x, y) = (
        f64::midpoint(plus.x0, plus.x1),
        f64::midpoint(plus.y0, plus.y1),
    );
    app.harness().click_at(x, y);
    assert_eq!(page_count(&app), 1, "the sidebar's + creates a page");
    app.harness().click_at(x, y);
    assert_eq!(page_count(&app), 2, "and another on a second click");
}

#[test]
fn the_published_surface_validates_as_the_host_checks_it() {
    let mut app = fresh();
    let empty = app.harness().component_contract_violations();
    assert!(empty.is_empty(), "empty notebook: {empty:#?}");
    // New note opens the note in the editor, which publishes its body and
    // the edits bound to where the caret is.
    app.harness().click_named("notes.first_note").unwrap();
    app.harness().type_text("Groceries");
    app.settle();
    let open = app.harness().component_contract_violations();
    assert!(open.is_empty(), "a note open for editing: {open:#?}");
    app.invoke(Invoke::apply(&new_page_action(PageId::new_v4(), None, 1, "Second")).unwrap())
        .unwrap();
    let two = app.harness().component_contract_violations();
    assert!(two.is_empty(), "notebook with two notes: {two:#?}");
}

/// The headless way to edit a note: nothing is clicked or focused, a script or
/// agent creates a note over the notebook's actions and then edits it over the
/// note's own. The note's edits stay published once it exists, and running
/// one changes the note.
#[test]
fn a_note_made_over_actions_publishes_its_edits_and_they_edit_it() {
    let mut app = fresh();
    app.call("new-page", serde_json::json!({ "value": "Groceries" }))
        .unwrap();
    app.settle();
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "one note: {violations:#?}");

    let page = app.read_document(NotesDoc::list_pages)[0].id;
    let note = notes::actions::page_resource_id(page);
    let surface = app.agent_surface();
    let published = |resource: &str, action: &str| {
        surface.as_array().unwrap().iter().any(|row| {
            row["resource"] == resource && row["action"] == action && row["operation"] == "Document"
        })
    };
    for action in ["rename", "delete", "new-child"] {
        assert!(
            published(&note, action),
            "{note} publishes {action}: {surface:#}"
        );
    }
    let body = format!("notes.notebook/page/{page}/body");
    assert!(
        published(&body, "insert-block"),
        "{body} publishes its edits: {surface:#}"
    );

    app.call_on(
        &note,
        "rename",
        serde_json::json!({ "value": "Groceries for Sunday" }),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        app.read_document(NotesDoc::list_pages)[0].title,
        "Groceries for Sunday"
    );
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "after the edit: {violations:#?}");
}
