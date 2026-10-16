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
    app.read_document(|doc| read::list_pages(doc).len())
}

#[test]
fn numeric_order_notebook_reopens_through_the_saved_application_view() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().unwrap();
    let page = PageId::new_v4();
    let original_body;
    {
        let mut app = TestApp::<NotesShell>::open_data_dir(directory.path()).unwrap();
        app.invoke(Invoke::apply(&create_page_action(page, None, "Existing note")).unwrap())
            .unwrap();
        let mut peer = app.peer_document();
        let (_, pages) = peer.get(ROOT, "pages").unwrap().unwrap();
        let (_, entry) = peer.get(&pages, page.to_string()).unwrap().unwrap();
        peer.put(&entry, "order", 12_i64).unwrap();
        original_body = read::page_block_snapshots(&peer, page);
        app.merge_peer(&mut peer).unwrap();
        app.flush().unwrap();
    }
    let mut reopened = TestApp::<NotesShell>::open_data_dir(directory.path()).unwrap();
    reopened.settle();
    assert!(reopened.harness().targets().contains_key("block_editor"));
    reopened.read_document(|doc| {
        assert_eq!(read::list_pages(doc)[0].id, page);
        assert_eq!(read::page_block_snapshots(doc, page), original_body);
        let (_, pages) = doc.get(ROOT, "pages").unwrap().unwrap();
        let (_, entry) = doc.get(&pages, page.to_string()).unwrap().unwrap();
        assert!(matches!(doc.get(&entry, "order").unwrap(),
            Some((automerge::Value::Scalar(value), _))
                if value.as_ref() == &automerge::ScalarValue::Int(12)));
    });
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
    app.invoke(Invoke::apply(&create_page_action(PageId::new_v4(), None, "Second")).unwrap())
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
    app.call("new-note", serde_json::json!({ "title": "Groceries" }))
        .unwrap();
    app.settle();
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "one note: {violations:#?}");

    let page = app.read_document(read::list_pages)[0].id;
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
    assert!(
        published("notes.notebook", "field-edit"),
        "the notebook publishes the edits of every note's body: {surface:#}"
    );

    app.call_on(
        &note,
        "rename",
        serde_json::json!({ "title": "Groceries for Sunday" }),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        app.read_document(read::list_pages)[0].title,
        "Groceries for Sunday"
    );
    let violations = app.harness().component_contract_violations();
    assert!(violations.is_empty(), "after the edit: {violations:#?}");
}

/// A script creating several notes in a row, each before the notebook
/// republishes, appends each after the last. Every create used to take the
/// first place, so the second left two notes at one position and the app
/// refused to show the notebook at all.
#[test]
fn notes_made_one_after_another_over_actions_stay_in_order() {
    let mut app = fresh();
    for title in ["First", "Second", "Third"] {
        app.call("new-note", serde_json::json!({ "title": title }))
            .unwrap();
    }
    app.settle();
    let pages = app.read_document(read::list_pages);
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
    let pages = app.read_document(|doc| {
        notes::read::project(doc)
            .expect("the notebook projects")
            .children_of(None)
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
    });
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
    use tilo_actions::Action as _;
    let mut app = fresh();
    app.call("new-note", serde_json::json!({ "title": "Existing" }))
        .unwrap();
    app.settle();
    let mut peer = app.peer_document();
    app.call("new-note", serde_json::json!({ "title": "Mine" }))
        .unwrap();
    notes::actions::create_page_action(PageId::new_v4(), None, "Theirs")
        .apply(&mut peer)
        .unwrap();
    peer.commit();
    app.merge_peer(&mut peer).unwrap();

    let ours = app.read_document(read::list_pages);
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
    app.call("new-note", serde_json::json!({ "title": "Later" }))
        .unwrap();
    assert_shows(&mut app, &["Existing", &first, &second, "Later"]);
}

/// Undoing a note's creation after someone added a note behind it leaves a
/// gap in front of the later note. The notebook stays shown with the later
/// note in it.
#[test]
fn undoing_a_create_after_a_later_note_keeps_the_notebook_shown() {
    use tilo_actions::Action as _;
    let mut app = fresh();
    app.call("new-note", serde_json::json!({ "title": "First" }))
        .unwrap();
    app.call("new-note", serde_json::json!({ "title": "Mine" }))
        .unwrap();
    app.settle();
    let mut peer = app.peer_document();
    notes::actions::create_page_action(PageId::new_v4(), None, "Theirs")
        .apply(&mut peer)
        .unwrap();
    peer.commit();
    app.merge_peer(&mut peer).unwrap();
    assert_shows(&mut app, &["First", "Mine", "Theirs"]);

    assert!(app.undo(), "the create of Mine undoes");
    assert_shows(&mut app, &["First", "Theirs"]);
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(
        app.read_document(read::list_pages),
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
    app.call("new-note", serde_json::json!({ "title": "Inbox" }))
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

    let pages = app.read_document(read::list_pages);
    assert_eq!(pages.len(), before + 1, "naming it made no other note");
    let named = pages
        .iter()
        .find(|page| page.title == "Groceries")
        .expect("the typed title names the note");
    assert!(
        pages
            .iter()
            .all(|page| page.title != notes::notebook::UNTITLED),
        "no Untitled note is left behind: {pages:?}"
    );
    let body = app.read_document(|doc| read::page_block_snapshots(doc, named.id));
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
            && rows.iter().all(|row| row != notes::notebook::UNTITLED),
        "the page tree shows the note under its name: {rows:?}"
    );
}
