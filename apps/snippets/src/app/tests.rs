use super::*;
use crate::resource::SnippetsShell;
use ui::headless::Framed;
use ui::prelude::TestApp;
use ui::tilo_resource::Invoke;
use ui::widget::Widget;

/// The bare window showing the starter library as the framework presents
/// it, with a specimen open.
fn demo() -> SnippetsApp {
    let mut app = SnippetsApp::new();
    app.show_library(&crate::seed::starter_library());
    {
        let mut core = app.core.borrow_mut();
        core.open_specimen();
        core.show_pane = None;
    }
    app.refresh_columns();
    app
}

fn library_of(app: &SnippetsApp) -> Vec<Snippet> {
    app.core.borrow().library.snippets.clone()
}

/// The app on a fresh record holding `snippets`, created through the door.
fn launched(snippets: Vec<Snippet>) -> TestApp<SnippetsShell> {
    let mut app = TestApp::<SnippetsShell>::new().unwrap();
    for snippet in snippets {
        app.invoke(Invoke::apply(&actions::create_snippet_action(snippet)).unwrap())
            .unwrap();
    }
    app.settle();
    app
}

fn snippet(title: &str, body: &str) -> Snippet {
    let mut s = Snippet::blank("Ops");
    s.title = title.into();
    s.body = body.into();
    s
}

/// Activate the control labelled `label` as assistive technology does.
fn activate(app: &mut TestApp<SnippetsShell>, label: &str) {
    let harness = app.harness();
    let node = harness.a11y_node_named(label).unwrap_or_else(|| {
        let nodes = harness.a11y_nodes();
        let labels: Vec<&str> = nodes.iter().filter_map(|(_, n)| n.label()).collect();
        panic!("no control labelled {label:?} among {labels:?}")
    });
    harness
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
}

/// The value of the snippet body's editor, as assistive technology reads it.
fn body_shown(app: &mut TestApp<SnippetsShell>) -> String {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == ui::accesskit::Role::MultilineTextInput)
        .and_then(|(_, node)| node.value().map(str::to_owned))
        .unwrap_or_default()
}

fn by_title(app: &TestApp<SnippetsShell>, title: &str) -> Snippet {
    app.state().into_iter().find(|s| s.title == title).unwrap()
}

/// The window shows what it is presented: empty until the library arrives.
#[test]
fn presented_library_fills_the_list() {
    let empty = SnippetsApp::new();
    assert_eq!(empty.visible_count_for_test(), 0);
    assert!(
        empty.open_id_for_test().is_none(),
        "nothing is opened on an empty library"
    );

    let app = demo();
    assert!(app.visible_count_for_test() >= 10);
    assert!(app.open_id_for_test().is_some());
}

/// The sidebar lists the library's places with their counts, and choosing
/// one narrows the list.
#[test]
fn sidebar_places_narrow_the_list() {
    let mut app = demo();
    let sections = app.core.borrow().nav_sections();
    let ids: Vec<&str> = sections.iter().map(|section| section.id.as_str()).collect();
    assert_eq!(ids, ["library", "folders", "tags"]);

    let all = app.visible_count_for_test();
    app.requests
        .borrow_mut()
        .push(Action::Select(Selection::Smart(SmartFolder::Favorites)));
    app.settle();
    let favorites = app.visible_count_for_test();
    assert!(favorites > 0 && favorites < all);
    assert_eq!(
        app.titles.0, "Favorites",
        "the list column is titled by its place"
    );
    assert!(
        library_of(&app)
            .iter()
            .filter(|s| app.core.borrow().visible.contains(&s.id))
            .all(|s| s.favorite)
    );

    // An empty library has no folder or tag sections.
    let empty = SnippetsApp::new();
    let sections = empty.core.borrow().nav_sections();
    assert_eq!(sections.len(), 1);
}

/// Favorite, Copy, New and Delete submit the library's actions: each is one
/// undo step, and the window follows the stored library.
#[test]
fn controls_submit_library_actions_as_one_undo_step_each() {
    let mut app = launched(vec![snippet("Plain", "echo hi"), snippet("Templated", "")]);
    activate(&mut app, "Plain");
    activate(&mut app, "Favorite");
    assert!(by_title(&app, "Plain").favorite);
    assert!(app.undo(), "the favorite is one undo step");
    assert!(!by_title(&app, "Plain").favorite);

    activate(&mut app, "Copy");
    assert_eq!(by_title(&app, "Plain").uses, 1, "copying records a use");

    activate(&mut app, "New snippet");
    assert_eq!(app.state().len(), 3);
    // The new snippet is the one open, so its menu deletes it.
    activate(&mut app, "More actions");
    activate(&mut app, "Delete");
    let mut titles: Vec<String> = app.state().into_iter().map(|s| s.title).collect();
    titles.sort();
    assert_eq!(titles, ["Plain", "Templated"]);
    assert!(app.undo(), "the delete is one undo step");
    assert_eq!(app.state().len(), 3);
}

/// The language picker sets the chosen language; the overflow menu
/// duplicates the open snippet and opens the copy.
#[test]
fn the_snippet_menus_set_the_language_and_duplicate() {
    let mut app = launched(vec![snippet("Deploy", "kubectl apply")]);
    activate(&mut app, "Deploy");
    activate(&mut app, "Plain text");
    activate(&mut app, "Set language to Rust");
    assert_eq!(by_title(&app, "Deploy").language, Language::Rust);

    activate(&mut app, "More actions");
    activate(&mut app, "Duplicate");
    let copy = by_title(&app, "Deploy (copy)");
    assert_eq!(copy.body, "kubectl apply");
    assert_eq!(app.state().len(), 2);
}

/// Copying a templated snippet fills its variables first.
#[test]
fn copying_a_templated_snippet_opens_the_variables_dialog() {
    let mut app = SnippetsApp::new();
    let templated = snippet("Greet", "Hello ${name}");
    app.show_library(std::slice::from_ref(&templated));
    app.core.borrow_mut().copy(templated.id);
    assert!(app.expand_open_for_test());
}

/// The list is one focus stop whose keys move the open snippet.
#[test]
fn the_list_moves_the_open_snippet_by_keys() {
    let app = demo();
    let mut core = app.core.borrow_mut();
    let first = core.visible[0];
    let second = core.visible[1];
    let last = *core.visible.last().unwrap();
    core.open = Some(first);
    assert!(core.list_key(&Key::Named(NamedKey::ArrowDown)));
    assert_eq!(core.open, Some(second));
    assert!(core.list_key(&Key::Named(NamedKey::ArrowUp)));
    assert!(core.list_key(&Key::Named(NamedKey::ArrowUp)));
    assert_eq!(core.open, Some(first), "Up stops at the top");
    assert!(core.list_key(&Key::Named(NamedKey::End)));
    assert_eq!(core.open, Some(last));
    assert!(core.list_key(&Key::Named(NamedKey::Home)));
    assert_eq!(core.open, Some(first));
    assert!(!core.list_key(&Key::Character("a".into())));
}

/// Typing in the body splices the stored text through the body's child
/// resource; the editor follows a collaborator's edit and an undo.
#[test]
fn body_edits_are_text_actions_and_the_editor_follows_the_document() {
    let mut app = launched(vec![snippet("Only", "")]);
    let id = app.state()[0].id;
    {
        let harness = app.harness();
        let body = harness
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::MultilineTextInput)
            .map(|(id, _)| id)
            .expect("the body editor is on screen");
        harness
            .perform_a11y_action(body, &ui::a11y::Action::Focus)
            .unwrap();
        harness.type_text("Grün");
    }
    app.settle();
    assert_eq!(app.state()[0].body, "Grün");
    assert!(app.undo(), "the typed word is one undo step");
    assert_eq!(app.state()[0].body, "");
    assert_eq!(body_shown(&mut app), "");

    app.merge_peer_edit(|doc| {
        tilo_actions::Action::apply(&actions::set_body_action(id, "remote 🌍\n"), doc).unwrap();
    });
    app.settle();
    assert_eq!(app.state()[0].body, "remote 🌍\n");
    assert_eq!(body_shown(&mut app), "remote 🌍\n");
}

/// The variable dialog is addressable and peer-local, and holds the body
/// while it is open.
#[test]
fn variable_dialog_is_semantic_and_local() {
    let mut app = SnippetsApp::new();
    let templated = snippet("Greet", "Hello ${name}");
    app.show_library(std::slice::from_ref(&templated));
    let mut doc = AutoCommit::new();
    let heads = doc.get_heads();
    app.core.borrow_mut().start_expand(templated.id);
    assert!(app.perform_semantic_action(
        "snippets.expand.field.name",
        &ui::a11y::Action::SetValue("Grün".into()),
        &mut doc,
        &automerge::ROOT
    ));
    assert_eq!(
        app.core
            .borrow()
            .expand
            .as_ref()
            .unwrap()
            .preview_for_test(),
        "Hello Grün"
    );
    assert!(!app.perform_semantic_action(
        "snippets.body",
        &ui::a11y::Action::SetValue("hidden edit".into()),
        &mut doc,
        &automerge::ROOT
    ));
    assert!(app.perform_semantic_action(
        "snippets.expand.cancel",
        &ui::a11y::Action::Activate,
        &mut doc,
        &automerge::ROOT
    ));
    assert!(!app.expand_open_for_test());
    assert_eq!(doc.get_heads(), heads, "the dialog writes nothing");
}

/// Focusing the New button does not author a snippet; activating it does.
#[test]
fn focusing_new_snippet_does_not_activate_it() {
    let mut app = SnippetsApp::new();
    let mut doc = AutoCommit::new();
    app.core
        .borrow_mut()
        .list
        .hits
        .push((Rect::new(0.0, 0.0, 100.0, 40.0), Action::NewSnippet));
    let target = Action::NewSnippet.semantic_id();
    assert!(Widget::perform_semantic_action(
        &mut app,
        &target,
        &ui::a11y::Action::Focus,
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(
        app.core.borrow().pending_open.is_none(),
        "focusing a button authors nothing"
    );
    assert!(Widget::perform_semantic_action(
        &mut app,
        &target,
        &ui::a11y::Action::Activate,
        &mut doc,
        &automerge::ROOT,
    ));
    assert!(
        app.core.borrow().pending_open.is_some(),
        "activating it creates a snippet"
    );
}

// ---- responsive layout ---------------------------------------------

fn framed(app: SnippetsApp, width: u32) -> Framed<SnippetsApp> {
    Framed::new(ui::State::new(), app, (width, 800), Theme::dark())
}

/// The shell docks the sidebar when wide, makes it a drawer when narrow,
/// and pushes the snippet in front of the list when very narrow.
#[test]
fn the_shell_docks_wide_draws_narrow_and_pushes_very_narrow() {
    // Case `wide_keeps_sidebar_docked_and_open`.
    {
        let app = framed(demo(), 1280);
        assert!(
            !app.sidebar_is_overlay_for_test(),
            "wide window docks the sidebar"
        );
        assert!(app.sidebar_open_for_test());
        assert!(
            !app.is_push_for_test(),
            "wide window keeps list and snippet side by side"
        );
    }

    // Case `narrow_makes_the_sidebar_a_closed_drawer`.
    {
        let mut app = framed(demo(), 720);
        assert!(
            app.sidebar_is_overlay_for_test(),
            "narrow window floats the sidebar"
        );
        assert!(!app.sidebar_open_for_test(), "the drawer starts closed");
        app.toggle_sidebar_for_test();
        assert!(app.sidebar_open_for_test(), "the toggle opens the drawer");
        app.toggle_sidebar_for_test();
        assert!(!app.sidebar_open_for_test(), "and closes it again");
    }

    // Case `very_narrow_pushes_the_open_snippet`.
    {
        let mut app = framed(demo(), 480);
        assert!(app.is_push_for_test(), "very narrow window pushes");
        app.open_first_for_test();
        assert!(
            app.push_showing_detail_for_test(),
            "choosing a snippet pushes it in front of the list"
        );
    }

    // Case `wide_choice_does_not_push`.
    {
        let mut app = framed(demo(), 1280);
        app.open_first_for_test();
        assert!(!app.push_showing_detail_for_test());
    }
}

/// The content column's controls keep their measured size across a window
/// resize and a rebuild for another snippet, and its header buttons follow
/// the open snippet.
#[test]
fn the_content_column_lays_out_its_controls() {
    let mut app = framed(demo(), 1280);
    let sizes = |app: &Framed<SnippetsApp>| {
        let id = app.open_id_for_test().unwrap();
        let targets = Framed::harness(app).targets();
        let rect = |name: String| targets.get(&name).copied().unwrap_or(Rect::ZERO);
        (
            rect(format!("snippets.snippet:{id}.language")),
            rect(format!("snippets.snippet:{id}.copy")),
        )
    };
    let (language, copy) = sizes(&app);
    assert!(language.width() > language.height() && copy.width() > copy.height());
    Framed::harness_mut(&mut app).resize((960, 800)).unwrap();
    assert_eq!(sizes(&app).1.size(), copy.size(), "a resize keeps the size");
    let other = app.core.borrow().visible[2];
    app.requests.borrow_mut().push(Action::OpenSnippet(other));
    Framed::harness_mut(&mut app).relayout();
    let (language, copy) = sizes(&app);
    assert_eq!(app.open_id_for_test(), Some(other));
    assert!(language.width() > language.height() && copy.width() > copy.height());
    let targets = Framed::harness(&app).targets();
    assert!(targets.contains_key("snippets.favorite") && targets.contains_key("snippets.more"));
}
