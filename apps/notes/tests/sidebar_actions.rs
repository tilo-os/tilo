//! Every page-tree control reaches the notebook by the action it publishes:
//! driving the control and running the same published action by its id leave
//! the notebook identical, so the palette, agents, history, undo and the
//! permission check reach the tree's work.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::framework_support::{add_note, apply, notes, open, root_order, send, shown, surface};
use notes::{Notebook, NotesShell, PageId};
use serde_json::json;
use ui::framework::testing::TestApp;
use ui::synthetic_input::{char_key, cursor_moved, named_key};
use ui::tilo_actions::{ActionInvoker, ActorKind};
use vello::kurbo::Point;
use winit::event::{ElementState, MouseButton};
use winit::keyboard::NamedKey;

type App = TestApp<NotesShell>;

/// An agent changing notes, one call each, as one connection does.
fn agent() -> ActionInvoker {
    ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada")
}

/// Call `id` on `resource` with the arguments a palette or an agent passes.
fn call(app: &mut App, resource: &str, id: &str, args: serde_json::Value) {
    app.call_by(agent(), resource, id, args)
        .unwrap_or_else(|error| panic!("{id} on {resource}: {error:?}"));
}

/// The notebook's published resource.
fn kind() -> &'static str {
    <Notebook as ui::tilo_resource::Resource>::KIND
}

/// The published resource of note `page`.
fn item(page: PageId) -> String {
    notes::actions::page_resource_id(page)
}

/// Notes with the root notes Welcome, One and Two, Two open.
fn seeded() -> (App, PageId, PageId, PageId) {
    let mut app = notes();
    let welcome = add_note(&mut app, "Welcome", "");
    let one = add_note(&mut app, "One", "");
    let two = add_note(&mut app, "Two", "");
    open(&mut app, two);
    (app, welcome, one, two)
}

/// The notebook as the window shows it: every note's title at its depth, in
/// order.
fn outline(app: &App) -> Vec<(String, usize)> {
    fn walk(
        workspace: &notes::Workspace,
        parent: Option<PageId>,
        depth: usize,
        out: &mut Vec<(String, usize)>,
    ) {
        for page in workspace.children_of(parent) {
            out.push((page.title.clone(), depth));
            walk(workspace, Some(page.id), depth + 1, out);
        }
    }
    let workspace = shown(app);
    let mut out = Vec::new();
    walk(&workspace, None, 0, &mut out);
    out
}

/// The title of the note the window has open.
fn open_title(app: &mut App) -> Option<String> {
    let selected = surface(app, |notes| notes.selected_for_test())?;
    shown(app).get(selected).map(|page| page.title.clone())
}

fn renaming(app: &mut App) -> bool {
    surface(app, |notes| notes.sidebar_is_renaming_for_test())
}

/// The centre of visible row `index`.
fn row(app: &mut App, index: usize) -> Point {
    surface(app, |notes| notes.sidebar_row_center_for_test(index))
        .unwrap_or_else(|| panic!("row {index} is laid out"))
}

/// The centre of affordance `action` on visible row `index`, shown as the
/// pointer resting on the row shows it.
fn row_action(app: &mut App, index: usize, action: usize) -> Point {
    let center = row(app, index);
    send(app, &cursor_moved(center.x, center.y));
    surface(app, |notes| {
        notes.sidebar_row_action_center_for_test(index, action)
    })
    .unwrap_or_else(|| panic!("affordance {action} on row {index} is laid out"))
}

/// Click visible row `index`, which focuses and selects its note.
fn click_row(app: &mut App, index: usize) {
    let center = row(app, index);
    app.harness().click_at(center.x, center.y);
}

/// Open the page menu on visible row `index`.
fn open_menu(app: &mut App, index: usize) {
    let center = row(app, index);
    send(app, &cursor_moved(center.x, center.y));
    send(
        app,
        &ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
    );
    assert!(
        surface(app, |notes| notes.sidebar_menu_open_for_test()),
        "right-click opens the page menu"
    );
}

/// Move the open menu's highlight `downs` rows from nothing and fire the row
/// it lands on: the first `ArrowDown` lands on the menu's first row.
fn choose_menu_row(app: &mut App, downs: usize) {
    for _ in 0..downs {
        send(app, &named_key(NamedKey::ArrowDown));
    }
    send(app, &named_key(NamedKey::Enter));
}

#[test]
fn sidebar_new_note_controls_create_the_notebooks_note() {
    eprintln!("phase header_new_note_creates_a_root_note_and_names_it");
    {
        let (mut app, ..) = seeded();
        let (mut twin, ..) = seeded();
        app.harness().relayout_at((1200, 800)).unwrap();

        let button = surface(&mut app, |notes| {
            notes.sidebar_new_page_btn_center_for_test()
        });
        assert!(button.x > 0.0, "the sidebar header lays the control out");
        app.harness().click_at(button.x, button.y);

        assert_eq!(
            open_title(&mut app).as_deref(),
            Some(notes::notebook::NO_TITLE),
            "the header control opens the note it made"
        );
        assert!(
            surface(&mut app, |notes| notes.main_title_editing_for_test()),
            "and leaves the title taking the keyboard"
        );

        call(
            &mut twin,
            kind(),
            "new-note",
            json!({ "title": notes::notebook::NO_TITLE }),
        );
        assert_eq!(
            outline(&app),
            outline(&twin),
            "the control's note is the notebook's own new-note"
        );
        assert_eq!(outline(&app).len(), 4);
    }

    eprintln!("phase row_plus_creates_a_child_of_its_row");
    {
        let (mut app, _welcome, one, _two) = seeded();
        let (mut twin, _twin_welcome, twin_one, _twin_two) = seeded();

        let plus = row_action(&mut app, 1, 0);
        app.harness().click_at(plus.x, plus.y);

        let made = surface(&mut app, |notes| notes.selected_for_test()).expect("the child opens");
        let workspace = shown(&app);
        assert_eq!(
            workspace.get(made).map(|page| page.parent),
            Some(Some(one)),
            "the row's + puts the note inside its own note"
        );
        assert_eq!(
            open_title(&mut app).as_deref(),
            Some(notes::notebook::NO_TITLE)
        );

        call(
            &mut twin,
            &item(twin_one),
            "new-child",
            json!({ "title": notes::notebook::NO_TITLE }),
        );
        assert_eq!(
            outline(&app),
            outline(&twin),
            "the row's + is the note's own new-child"
        );
    }
}

#[test]
fn sidebar_rename_commits_the_notebooks_rename() {
    eprintln!("phase inline_rename_renames_the_note");
    {
        let (mut app, _welcome, one, _two) = seeded();
        let (mut twin, _twin_welcome, twin_one, _twin_two) = seeded();

        click_row(&mut app, 1);
        send(&mut app, &named_key(NamedKey::F2));
        assert!(renaming(&mut app), "F2 opens the row's rename field");
        for ch in [" ", "r", "e", "p", "o", "r", "t"] {
            send(&mut app, &char_key(ch));
        }
        send(&mut app, &named_key(NamedKey::Enter));
        assert!(!renaming(&mut app), "Enter commits and ends the edit");
        assert_eq!(
            surface(&mut app, |notes| notes
                .sidebar_row_title_for_test(1)
                .map(str::to_owned))
            .as_deref(),
            Some("One report"),
            "the row shows the committed title"
        );
        assert_eq!(
            shown(&app).get(one).map(|page| page.title.as_str()),
            Some("One report")
        );

        call(
            &mut twin,
            &item(twin_one),
            "rename",
            json!({ "title": "One report" }),
        );
        assert_eq!(
            outline(&app),
            outline(&twin),
            "the field's commit is the notebook's own rename"
        );
    }

    eprintln!("phase menu_rename_renames_the_row_it_was_opened_on");
    {
        let (mut app, welcome, _one, _two) = seeded();
        let (mut twin, twin_welcome, _twin_one, _twin_two) = seeded();

        open_menu(&mut app, 0);
        choose_menu_row(&mut app, 2);
        assert!(
            renaming(&mut app),
            "the menu's Rename opens the field once it has closed"
        );
        for ch in [" ", "h", "o", "m", "e"] {
            send(&mut app, &char_key(ch));
        }
        assert_eq!(
            surface(&mut app, |notes| notes.sidebar_rename_draft_for_test()),
            Some("Welcome home".to_owned()),
            "the fresh field takes the typed name"
        );
        send(&mut app, &named_key(NamedKey::Enter));
        app.settle();
        let workspace = shown(&app);
        assert_eq!(
            workspace.get(welcome).map(|page| page.title.as_str()),
            Some("Welcome home")
        );

        call(
            &mut twin,
            &item(twin_welcome),
            "rename",
            json!({ "title": "Welcome home" }),
        );
        assert_eq!(
            outline(&app),
            outline(&twin),
            "the menu's Rename is the notebook's own rename"
        );
    }
}

#[test]
fn sidebar_delete_and_duplicate_submit_the_notebooks_verbs() {
    eprintln!("phase menu_delete_removes_the_note_and_an_agent_deletes_the_same_way");
    {
        let (mut app, _welcome, _one, two) = seeded();
        let (mut twin, _twin_welcome, _twin_one, twin_two) = seeded();

        open_menu(&mut app, 2);
        send(&mut app, &named_key(NamedKey::ArrowUp));
        send(&mut app, &named_key(NamedKey::Enter));
        assert!(
            app.asking().is_none(),
            "an undoable delete asks nothing first"
        );
        assert!(
            shown(&app).pages().iter().all(|page| page.id != two),
            "the menu's Delete removed its note"
        );
        let entries = app.history();
        let last = entries.len() - 1;
        assert_eq!(
            app.sentences(&entries[last..]),
            ["Deleted Two"],
            "the delete is one step of history the person can undo"
        );

        call(&mut twin, &item(twin_two), "delete", json!({}));
        assert_eq!(
            outline(&app),
            outline(&twin),
            "the menu's Delete is the notebook's own delete"
        );
        let entries = twin.history();
        let last = entries.last().expect("the agent's delete is in history");
        assert_eq!(
            last.invoker.as_ref().map(|invoker| invoker.actor.as_str()),
            Some("agent:claude"),
            "an agent reaches the same delete by the verb's id"
        );
    }

    eprintln!("phase menu_duplicate_copies_the_note_and_opens_it");
    {
        let (mut app, _welcome, one, _two) = seeded();
        let (mut twin, _twin_welcome, twin_one, _twin_two) = seeded();

        open_menu(&mut app, 1);
        choose_menu_row(&mut app, 3);
        app.settle();
        assert_eq!(
            open_title(&mut app).as_deref(),
            Some("One copy"),
            "the copy opens, as every made note does"
        );
        assert!(
            shown(&app)
                .pages()
                .iter()
                .any(|page| page.id != one && page.title == "One copy")
        );

        call(&mut twin, &item(twin_one), "duplicate", json!({}));
        assert_eq!(
            outline(&app),
            outline(&twin),
            "the menu's Duplicate is the notebook's own duplicate"
        );
    }
}

#[test]
fn sidebar_copy_link_asks_the_notebooks_own_effect() {
    eprintln!("phase menu_copy_link_asks_the_notebooks_own_effect");
    let (services, _recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let (mut app, welcome, _one, _two) = seeded();

    let title = shown(&app).get(welcome).unwrap().title.clone();
    open_menu(&mut app, 0);
    choose_menu_row(&mut app, 4);
    app.settle();
    assert_eq!(
        ui::clipboard_paste().as_deref(),
        Some(format!("[[{title}]]").as_str()),
        "the menu copies the link of the note it was opened on"
    );
}

#[test]
fn sidebar_drag_moves_the_note_with_the_notebooks_move() {
    let (mut app, welcome, one, two) = seeded();
    let (mut twin, twin_welcome, _twin_one, _twin_two) = seeded();
    assert_eq!(root_order(&app), vec![welcome, one, two]);

    // Drag Welcome onto the last row's foot, where a drop lands beside it
    // rather than inside it, and drop it there.
    let from = row(&mut app, 0);
    let foot = surface(&mut app, |notes| notes.sidebar_row_point_for_test(2, 0.88))
        .expect("the last row is laid out");
    send(&mut app, &cursor_moved(from.x, from.y));
    send(
        &mut app,
        &ui::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Left),
    );
    send(&mut app, &cursor_moved(from.x, foot.y));
    send(
        &mut app,
        &ui::synthetic_input::mouse_input(ElementState::Released, MouseButton::Left),
    );
    app.settle();

    assert_eq!(
        root_order(&app),
        vec![one, two, welcome],
        "the drop moved Welcome to the end of its sibling group"
    );
    apply(
        &mut twin,
        &notes::actions::move_page_action(twin_welcome, None, None),
    );
    assert_eq!(
        outline(&app),
        outline(&twin),
        "the drop is the notebook's own move-note"
    );
}
