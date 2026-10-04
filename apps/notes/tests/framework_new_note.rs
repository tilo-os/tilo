//! Creating notes in the framework-hosted app the way a person does: the
//! empty state's "New note" and the sidebar's "+" each create a page, and the
//! notebook the app publishes once it has pages passes the host's contract
//! check, so the palette and agents keep it.

use notes::actions::new_page_action;
use notes::doc::NotesDoc;
use notes::{Notebook, NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Invoke, Resource};

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
fn a_notebook_with_pages_publishes_a_valid_surface() {
    let mut app = fresh();
    app.invoke(Invoke::apply(&new_page_action(PageId::new_v4(), None, 0, "First")).unwrap())
        .unwrap();
    let resources = app.harness().bound_resources(None).unwrap();
    let handlers = app.harness().root_widget().resource_operation_handlers();
    let mut registry = tilo_actions::ActionRegistry::new();
    Notebook::register(&mut registry);
    let violations = tilo_actions::validate_surface(&resources, &registry, &handlers);
    assert!(violations.is_empty(), "{violations:#?}");
}
