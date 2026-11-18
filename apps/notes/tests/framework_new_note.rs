//! Creating notes in the framework-hosted app the way a person does: the
//! empty state's "New note" and the sidebar's "+" each create a page, and the
//! notebook the app publishes once it has pages passes the host's contract
//! check, so the palette and agents keep it.

use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn fresh() -> TestApp<NotesShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<NotesShell>::new().unwrap()
}

fn page_count(app: &TestApp<NotesShell>) -> usize {
    crate::framework_support::shown(app).pages().len()
}

/// Every control that presses one of the notebook's actions says which one.
/// That declared binding is what a presence marker lands on: an agent running
/// the notebook's own action finds the control a person would have pressed
/// (`ui::action_locus`), rather than a place it had to guess.
#[test]
fn the_controls_that_press_the_notebooks_actions_declare_them() {
    fn pressed(app: &mut TestApp<NotesShell>) -> Vec<tilo_actions::ActionAddress> {
        crate::framework_support::surface(app, |notes| {
            ui::widget::controls::control_action_bindings(notes)
                .into_iter()
                .filter_map(|binding| match binding.kind {
                    ui::widget::ControlBindingKind::Action(address) => Some(address),
                    ui::widget::ControlBindingKind::CollaborativeDocument(_)
                    | ui::widget::ControlBindingKind::SemanticAction(_) => None,
                })
                .collect()
        })
    }

    let mut app = fresh();
    app.settle();
    let empty = pressed(&mut app);
    assert!(
        empty
            .iter()
            .any(|address| address.resource == notes::part::chain_resource()
                && address.action == "new-note"),
        "the page tree's + presses the notebook's New note: {empty:?}"
    );

    // The marker an agent's action draws rests on that control: the action
    // resolves to the "+" and to nothing else.
    let address = notes::PagePart::NEW_NOTE.on(notes::part::chain_resource());
    let routes = app.harness().action_ui_routes(&address);
    let [route] = routes.as_slice() else {
        panic!("the window binds one control to New note: {routes:?}");
    };
    let control = route.bounds.expect("the + is laid out");
    let plus = app.harness().targets()["new-note"];
    assert!(
        control.contains(plus.center()) && plus.contains(control.center()),
        "the agent's marker lands on the page tree's +: {control:?} vs {plus:?}"
    );
    // The window holds the notebook in its component, as the session's
    // window does: the marker finds the + through the component's node.
    let locus = app
        .harness()
        .semantic_locus(&ui::headless::agent_acting(
            &notes::part::chain_resource(),
            "new-note",
        ))
        .expect("the agent's invocation finds the control it presses");
    assert!(
        plus.contains(locus.center()),
        "the agent acts at the +: {locus:?} in {plus:?}"
    );

    crate::framework_support::add_note(&mut app, "Inbox", "");
    app.settle();
    let with_a_row = pressed(&mut app);
    assert!(
        with_a_row
            .iter()
            .any(|address| address.action == "new-child"),
        "a row's + presses the notebook's New child: {with_a_row:?}"
    );
    assert!(
        with_a_row
            .iter()
            .any(|address| address.resource == notes::part::chain_resource()
                && address.action == "new-note"),
        "and the page tree's + still presses New note: {with_a_row:?}"
    );
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
    // The heading action is a retained control outside the page tree's body.
    let plus = app.harness().targets()["new-note"];
    let (x, y) = (plus.center().x, plus.center().y);
    app.harness().click_at(x, y);
    assert_eq!(page_count(&app), 1, "the sidebar's + creates a page");
    app.harness().click_at(x, y);
    assert_eq!(page_count(&app), 2, "and another on a second click");
}

/// The header's "+" opens the new note with its title taking the keyboard,
/// as Ctrl+N does, and what is typed names the note as it is typed: the
/// notebook, the page tree and the window's bar read it before Enter, and
/// Escape keeps it.
#[test]
fn the_header_plus_opens_the_new_note_and_typing_names_it() {
    use crate::framework_support::surface;
    const TITLE: &str = "Friday plan: book the room and order lunch.";
    let mut app = fresh();
    ui::window_toolbar::set_hosted(true);
    let plus = app.harness().targets()["new-note"];
    app.harness().click_at(plus.center().x, plus.center().y);
    app.settle();
    assert!(
        surface(&mut app, |notes| notes.main_title_editing_for_test()),
        "the new note's title takes the keyboard"
    );
    app.harness().type_text(TITLE);
    app.settle();
    let shown = |app: &mut TestApp<NotesShell>| {
        let notebook = crate::framework_support::shown(app).data.pages[0]
            .title
            .clone();
        let row = surface(app, |notes| {
            notes.sidebar_row_title_for_test(0).map(str::to_owned)
        });
        let bar = ui::window_toolbar::declared()
            .and_then(|bar| bar.location)
            .map(|location| location.label);
        (notebook, row, bar)
    };
    let typed = (
        TITLE.to_owned(),
        Some(TITLE.to_owned()),
        Some(TITLE.to_owned()),
    );
    assert_eq!(
        shown(&mut app),
        typed,
        "the note is named while it is typed"
    );
    app.harness().key("Escape");
    app.settle();
    assert!(!surface(&mut app, |notes| notes.main_title_editing_for_test()));
    assert_eq!(shown(&mut app), typed, "Escape keeps the typed title");
}

/// A note another member makes never opens this person's title. The
/// agent's note is not the person's gesture, and the window being presented
/// again — following it brings the window forward — does not reopen the
/// title field either: a title edit is only ever the person's own New note
/// or rename.
#[test]
fn a_note_another_actor_makes_never_opens_this_windows_title() {
    use crate::framework_support::surface;
    use ui::tilo_actions::ActionInvoker;
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().unwrap();
    let mut app = TestApp::<NotesShell>::open_data_dir(directory.path()).unwrap();
    let plus = app.harness().targets()["new-note"];
    app.harness().click_at(plus.center().x, plus.center().y);
    app.settle();
    assert!(
        surface(&mut app, |notes| notes.main_title_editing_for_test()),
        "the person's own new note opens its title"
    );
    let agent = ActionInvoker::agent("scribe");
    let mine =
        surface(&mut app, |notes| notes.selected_for_test()).expect("the person's note is open");
    app.call_by(
        agent,
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Launch notes" }),
    )
    .expect("the notebook lets the agent make a note");
    app.settle();
    assert_eq!(page_count(&app), 2, "the agent made its note");
    assert_eq!(
        surface(&mut app, |notes| notes.selected_for_test()),
        Some(mine),
        "the agent's note is made beside the person's view, not in front of it"
    );
    assert!(
        surface(&mut app, |notes| notes.main_title_editing_for_test()),
        "nothing the agent does closes the person's own field"
    );
    // The window is presented again, as following someone brings it forward.
    let mut app = app.reopen().expect("the window opens again");
    app.settle();
    assert!(
        !surface(&mut app, |notes| notes.main_title_editing_for_test()),
        "a window presented again does not open its title for typing"
    );
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
    app.invoke_child::<notes::PagePart>(
        &notes::part::chain_resource(),
        Invoke::apply(&create_page_action(PageId::new_v4(), None, "Second")).unwrap(),
    )
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
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Groceries" }),
    )
    .unwrap();
    app.settle();
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "one note: {violations:#?}");

    let page = crate::framework_support::shown(&app).data.pages[0].id;
    let note = notes::actions::page_resource_id(page);
    let surface = app.agent_surface();
    // A note's verbs are carried out on the part keeping it, through the
    // notes' published chain.
    let published = |resource: &str, action: &str| {
        surface
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["resource"] == resource && row["action"] == action)
    };
    for action in ["rename", "delete", "new-child"] {
        assert!(
            published(&note, action),
            "{note} publishes {action}: {surface:#}"
        );
    }
    assert!(
        published(&notes::part::chain_resource(), "field-edit"),
        "the notes publish the edits of every note's body: {surface:#}"
    );

    app.call_on(
        &note,
        "rename",
        serde_json::json!({ "title": "Groceries for Sunday" }),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        crate::framework_support::shown(&app).data.pages[0].title,
        "Groceries for Sunday"
    );
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "after the edit: {violations:#?}");
}

#[test]
fn notes_made_one_after_another_over_actions_stay_in_order() {
    let mut app = fresh();
    for title in ["First", "Second", "Third"] {
        app.call_on(
            &notes::part::chain_resource(),
            "new-note",
            serde_json::json!({ "title": title }),
        )
        .unwrap();
    }
    app.settle();
    let pages = crate::framework_support::shown(&app).data.pages;
    assert_eq!(
        pages
            .iter()
            .map(|page| page.title.as_str())
            .collect::<Vec<_>>(),
        vec!["First", "Second", "Third"]
    );
    let surface = app.agent_surface();
    for page in &pages {
        let note = notes::actions::page_resource_id(page.id);
        assert!(
            surface
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["resource"] == note.as_str() && row["action"] == "rename"),
            "{} stays published: {surface:#}",
            page.title
        );
    }
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "three notes: {violations:#?}");
}

/// The notebook projects, and the running app publishes every one of `titles`
/// in that order with no contract violation: the notebook is shown, not
/// refused.
fn assert_shows(app: &mut TestApp<NotesShell>, titles: &[&str]) {
    app.settle();
    let pages = crate::framework_support::shown(app)
        .children_of(None)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        pages
            .iter()
            .map(|page| page.title.as_str())
            .collect::<Vec<_>>(),
        titles
    );
    let surface = app.agent_surface();
    for page in &pages {
        let note = notes::actions::page_resource_id(page.id);
        assert!(
            surface
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["resource"] == note.as_str() && row["action"] == "rename"),
            "{} is published: {surface:#}",
            page.title
        );
    }
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "{violations:#?}");
}

/// Two devices each append a note at the same moment. Both get the key after
/// the last note, so the two share it; the page id orders them, the same way
/// on both replicas, and the notebook stays shown on both.
#[test]
fn notes_created_at_once_on_two_replicas_converge_and_stay_shown() {
    let mut app = fresh();
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Existing" }),
    )
    .unwrap();
    app.settle();
    let existing = crate::framework_support::shown(&app).pages()[0].id;
    let (part, mut peer) = crate::framework_support::peer_part(&app, existing);
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Mine" }),
    )
    .unwrap();
    crate::component_support::apply_to(
        &mut peer,
        &notes::actions::create_page_action(PageId::new_v4(), None, "Theirs"),
    )
    .unwrap();
    peer.commit();
    crate::framework_support::merge_part(&mut app, &part, &mut peer);

    let ours = crate::framework_support::shown(&app).data.pages;
    assert_eq!(ours, read::list_pages(&peer), "both replicas converge");
    let [_, first, second] = ours.as_slice() else {
        panic!("three notes: {ours:#?}");
    };
    assert!(
        [first.title.as_str(), second.title.as_str()].contains(&"Theirs"),
        "both appended notes follow the existing one"
    );
    notes::read::project(&peer).expect("the peer's notebook projects");
    let (first, second) = (first.title.clone(), second.title.clone());
    assert_shows(&mut app, &["Existing", &first, &second]);

    // A note made afterwards still goes last.
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Later" }),
    )
    .unwrap();
    assert_shows(&mut app, &["Existing", &first, &second, "Later"]);
}

/// Undoing a note's creation after someone added a note behind it leaves a
/// gap in front of the later note. The notebook stays shown with the later
/// note in it.
#[test]
fn undoing_a_create_after_a_later_note_keeps_the_notebook_shown() {
    let mut app = fresh();
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "First" }),
    )
    .unwrap();
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Mine" }),
    )
    .unwrap();
    app.settle();
    let first = crate::framework_support::shown(&app).pages()[0].id;
    let (part, mut peer) = crate::framework_support::peer_part(&app, first);
    crate::component_support::apply_to(
        &mut peer,
        &notes::actions::create_page_action(PageId::new_v4(), None, "Theirs"),
    )
    .unwrap();
    peer.commit();
    crate::framework_support::merge_part(&mut app, &part, &mut peer);
    assert_shows(&mut app, &["First", "Mine", "Theirs"]);

    assert!(app.undo(), "the create of Mine undoes");
    assert_shows(&mut app, &["First", "Theirs"]);
    crate::framework_support::merge_part(&mut app, &part, &mut peer);
    assert_eq!(
        crate::framework_support::shown(&app).data.pages,
        read::list_pages(&peer),
        "the undo reaches the peer and both converge"
    );
    notes::read::project(&peer).expect("the peer's notebook projects");
}

/// Ctrl+N, the name, Enter, the note: the new note's title takes the keyboard
/// empty, typing names it, Enter goes on to the start of its body, and the
/// rename leaves one note in the tree under its new name.
#[test]
fn ctrl_n_then_typing_names_the_note_and_enter_goes_on_to_its_body() {
    let mut app = fresh();
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Inbox" }),
    )
    .unwrap();
    app.settle();
    let before = page_count(&app);

    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("n");
    app.harness().set_modifiers(&[]);
    app.settle();
    assert_eq!(page_count(&app), before + 1, "Ctrl+N makes one note");
    let editing = app
        .surface(|notes: &mut notes::NotesApp| {
            let panel = notes.main_panel_mut();
            (
                panel.title_is_editing_for_test(),
                panel.title_draft_for_test(),
            )
        })
        .unwrap();
    assert_eq!(
        editing,
        (true, String::new()),
        "the new note's title is open for typing, empty with its placeholder"
    );

    app.harness().type_text("Groceries");
    app.harness().key("Enter");
    app.harness().type_text("milk");
    app.settle();

    let pages = crate::framework_support::shown(&app).data.pages;
    assert_eq!(pages.len(), before + 1, "naming it made no other note");
    let named = pages
        .iter()
        .find(|page| page.title == "Groceries")
        .expect("the typed title names the note");
    assert!(
        pages
            .iter()
            .all(|page| page.title != notes::notebook::NO_TITLE),
        "no Untitled note is left behind: {pages:?}"
    );
    let body = crate::framework_support::part_doc(&app, named.id, |doc| {
        read::page_block_snapshots(doc, named.id)
    });
    let texts: Vec<_> = body.into_iter().map(|(_, text)| text).collect();
    assert_eq!(
        texts,
        ["milk"],
        "Enter moved on to the body, and typing filled it"
    );
    let rows = app
        .surface(|notes: &mut notes::NotesApp| {
            (0..)
                .map_while(|index| notes.sidebar_row_title_for_test(index).map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert!(
        rows.iter().any(|row| row == "Groceries")
            && rows.iter().all(|row| row != notes::notebook::NO_TITLE),
        "the page tree shows the note under its name: {rows:?}"
    );
}

/// Two quick Ctrl+N notes sit side by side at the top level: the first
/// note being selected does not make the second its child.
#[test]
fn ctrl_n_twice_makes_two_top_level_notes() {
    let mut app = fresh();
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Inbox" }),
    )
    .unwrap();
    app.settle();
    for title in ["One", "Two"] {
        app.harness().set_modifiers(&["ctrl".into()]);
        app.harness().key("n");
        app.harness().set_modifiers(&[]);
        app.settle();
        app.harness().type_text(title);
        app.harness().key("Enter");
        app.settle();
    }
    let pages = crate::framework_support::shown(&app).data.pages;
    for title in ["Inbox", "One", "Two"] {
        let page = pages
            .iter()
            .find(|page| page.title == title)
            .unwrap_or_else(|| panic!("{title} in {pages:?}"));
        assert_eq!(page.parent, None, "{title} is a top-level note");
    }
}

/// "/sub" after a line of text makes a note inside this one, titled with
/// the line, and the line becomes a link to it.
#[test]
fn slash_sub_note_turns_a_line_into_a_linked_note_inside() {
    let mut app = fresh();
    app.call_on(
        &notes::part::chain_resource(),
        "new-note",
        serde_json::json!({ "title": "Trips" }),
    )
    .unwrap();
    app.settle();
    let parent = crate::framework_support::shown(&app).data.pages[0].id;
    app.harness().type_text("Lisbon in May /sub");
    app.harness().key("Enter");
    app.settle();
    let pages = crate::framework_support::shown(&app).data.pages;
    let child = pages
        .iter()
        .find(|page| page.title == "Lisbon in May")
        .unwrap_or_else(|| panic!("the line names the new note: {pages:?}"));
    assert_eq!(child.parent, Some(parent), "inside the note it came from");
    let body = crate::framework_support::part_doc(&app, parent, |doc| {
        read::page_block_snapshots(doc, parent)
    });
    let texts: Vec<_> = body.into_iter().map(|(_, text)| text).collect();
    assert_eq!(texts, ["Lisbon in May"], "the writing stays, now as a link");
}
