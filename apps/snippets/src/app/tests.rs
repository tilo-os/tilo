use super::*;
use crate::resource::SnippetsShell;
use automerge::AutoCommit;
use ui::headless::Framed;
use ui::prelude::TestApp;
use ui::tilo_resource::{Id, Invoke, items::item_resource_id};
use ui::widget::Widget;

/// A library holding `snippets`, as the framework presents it.
fn presented(snippets: impl IntoIterator<Item = Snippet>) -> Rc<Snippets> {
    let mut library = Snippets::default();
    for (n, snippet) in snippets.into_iter().enumerate() {
        library
            .snippets
            .push(Id::from_raw(format!("s{n}")), snippet);
    }
    Rc::new(library)
}

/// The bare window showing the starter library as the framework presents
/// it, with a specimen open.
fn demo() -> SnippetsApp {
    let mut app = SnippetsApp::new();
    app.show_library(&presented(crate::seed::starter_library()));
    {
        let mut core = app.core.borrow_mut();
        core.open_specimen();
        core.show_pane = None;
    }
    app.refresh_columns();
    app
}

/// The app on a fresh record holding `snippets`, inserted through the door.
fn launched(snippets: &[Snippet]) -> TestApp<SnippetsShell> {
    let mut app = TestApp::<SnippetsShell>::new().unwrap();
    for (at, snippet) in snippets.iter().enumerate() {
        app.invoke(Invoke::edit(&Snippets::paths().snippets.insert(at, snippet)).unwrap())
            .unwrap();
    }
    app.settle();
    app
}

fn snippet(title: &str, body: &str) -> Snippet {
    Snippet {
        body: body.into(),
        ..Snippet::titled(title, "Ops")
    }
}

/// Activate the control labelled `label` as assistive technology does.
fn activate(app: &mut TestApp<SnippetsShell>, label: &str) {
    let harness = app.harness();
    // A snippet's header heading shares its label with the list row. Select
    // the exported control that actually advertises activation, not the title.
    let nodes = harness.a11y_nodes();
    let actionable: Vec<_> = nodes
        .iter()
        .filter(|(_, node)| {
            node.label() == Some(label) && node.supports_action(ui::accesskit::Action::Click)
        })
        .map(|(id, _)| *id)
        .collect();
    let inventory: Vec<_> = nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(|name| (name, node.role())))
        .collect();
    assert_eq!(
        actionable.len(),
        1,
        "expected one actionable control labelled {label:?} among {inventory:?}"
    );
    harness
        .perform_a11y_action(actionable[0], &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("activating {label:?}: {error}"));
    app.settle();
}

/// The list's item for the snippet titled `title`: the node the list
/// exports around the row, named by the row's language badge and title.
fn list_item(app: &mut TestApp<SnippetsShell>, title: &str) -> ui::accesskit::NodeId {
    let suffix = format!(" {title}");
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::ListItem
                && !node.children().is_empty()
                && node.label().is_some_and(|label| label.ends_with(&suffix))
        })
        .map_or_else(|| panic!("the list item of {title:?}"), |(id, _)| id)
}

/// Open the snippet titled `title` from the list, as assistive technology
/// activates a list item.
fn open_row(app: &mut TestApp<SnippetsShell>, title: &str) {
    let item = list_item(app, title);
    app.harness()
        .perform_a11y_action(item, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("opening {title:?}: {error}"));
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

fn by_title(app: &TestApp<SnippetsShell>, title: &str) -> (Id, Snippet) {
    app.state()
        .snippets
        .iter()
        .find(|(_, s)| s.title == title)
        .map_or_else(
            || panic!("a snippet titled {title}"),
            |(id, s)| (id.clone(), s.clone()),
        )
}

fn titles(app: &TestApp<SnippetsShell>) -> Vec<String> {
    let mut titles: Vec<String> = app
        .state()
        .snippets
        .values()
        .map(|s| s.title.clone())
        .collect();
    titles.sort();
    titles
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
    let core = app.core.borrow();
    assert!(
        core.visible
            .iter()
            .all(|id| core.library.get(id).unwrap().favorite)
    );
    drop(core);

    // An empty library has no folder or tag sections.
    let empty = SnippetsApp::new();
    let sections = empty.core.borrow().nav_sections();
    assert_eq!(sections.len(), 1);
}

/// Favorite, Copy, New and Delete submit the library's field edits and
/// verbs: each is one undo step, and the window follows the stored library.
#[test]
fn controls_submit_library_actions_as_one_undo_step_each() {
    let mut app = launched(&[snippet("Plain", "echo hi"), snippet("Templated", "")]);
    open_row(&mut app, "Plain");
    activate(&mut app, "Favorite");
    assert!(by_title(&app, "Plain").1.favorite);
    assert!(app.undo(), "the favorite is one undo step");
    assert!(!by_title(&app, "Plain").1.favorite);

    activate(&mut app, "Copy");
    assert_eq!(
        by_title(&app, "Plain").1.uses.get(),
        1,
        "copying records a use"
    );

    activate(&mut app, "New snippet");
    assert_eq!(app.state().snippets.len(), 3);
    let (made, _) = by_title(&app, "Untitled snippet");
    assert_eq!(
        app.surface_mut::<SnippetsApp>().open_id_for_test(),
        Some(made.clone()),
        "the new snippet opens"
    );
    // The new snippet is the one open, so its menu deletes it, once the
    // person confirms what the host asks.
    activate(&mut app, "More actions");
    activate(&mut app, "Delete");
    assert_eq!(app.state().snippets.len(), 3, "deleting waits for consent");
    app.harness().type_text("CONFIRM");
    app.harness().key("Enter");
    app.settle();
    assert!(
        app.state().snippets.get(&made).is_none(),
        "the confirmed delete applied: {:?}",
        app.harness().control_failures()
    );
    assert_eq!(titles(&app), ["Plain", "Templated"]);
    assert!(app.undo(), "the delete is one undo step");
    assert_eq!(app.state().snippets.len(), 3);
}

/// The language picker sets the chosen language; the overflow menu
/// duplicates the open snippet and opens the copy.
#[test]
fn the_snippet_menus_set_the_language_and_duplicate() {
    let mut app = launched(&[snippet("Deploy", "kubectl apply")]);
    open_row(&mut app, "Deploy");
    // The language is a dropdown: assistive technology sets its value.
    {
        let harness = app.harness();
        let picker = harness
            .a11y_node_named("Language")
            .expect("the language picker");
        harness
            .perform_a11y_action(picker, &ui::a11y::Action::SetValue("Rust".into()))
            .unwrap();
    }
    app.settle();
    assert_eq!(by_title(&app, "Deploy").1.language, Language::Rust);

    activate(&mut app, "More actions");
    activate(&mut app, "Duplicate");
    let (copy_id, copy) = by_title(&app, "Deploy (copy)");
    assert_eq!(copy.body, "kubectl apply");
    assert_eq!(app.state().snippets.len(), 2);
    assert_eq!(
        app.surface_mut::<SnippetsApp>().open_id_for_test(),
        Some(copy_id),
        "the copy opens"
    );
}

/// A copy has no uses, so duplicating from Most used must leave that
/// filtered place before opening it; subsequent typing belongs to the copy.
#[test]
fn duplicating_from_most_used_opens_the_copy_for_editing() {
    let mut original = snippet("Deploy", "kubectl apply");
    original.uses.add(1);
    let mut app = launched(&[original]);
    activate(&mut app, "Most used");
    assert_eq!(
        app.surface_mut::<SnippetsApp>().core.borrow().selection,
        Selection::Smart(SmartFolder::MostUsed)
    );
    open_row(&mut app, "Deploy");
    activate(&mut app, "More actions");
    activate(&mut app, "Duplicate");
    let (copy_id, copy) = by_title(&app, "Deploy (copy)");
    assert_eq!(copy.uses.get(), 0);
    assert_eq!(
        app.surface_mut::<SnippetsApp>().open_id_for_test(),
        Some(copy_id),
        "the unused copy opens even though Most used excluded it"
    );
    {
        let harness = app.harness();
        let editor = harness
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::MultilineTextInput)
            .map(|(id, _)| id)
            .expect("the copied snippet's editor");
        harness
            .perform_a11y_action(editor, &ui::a11y::Action::Focus)
            .unwrap();
        harness.key("End");
        harness.type_text(" --dry-run");
    }
    app.settle();
    assert_eq!(
        by_title(&app, "Deploy (copy)").1.body,
        "kubectl apply --dry-run"
    );
    assert_eq!(by_title(&app, "Deploy").1.body, "kubectl apply");
}

/// The open snippet's folder and tag chips reveal their snippets through the
/// action each place publishes.
#[test]
fn the_folder_and_tag_chips_reveal_their_places() {
    let tagged = Snippet {
        tags: ["k8s".to_owned()].into_iter().collect(),
        ..snippet("Deploy", "kubectl apply")
    };
    let mut app = launched(&[tagged, snippet("Other", "")]);
    open_row(&mut app, "Deploy");
    activate(&mut app, "Tag: k8s");
    assert_eq!(
        app.surface_mut::<SnippetsApp>().core.borrow().selection,
        Selection::Tag("k8s".into())
    );
    activate(&mut app, "Folder: Ops");
    assert_eq!(
        app.surface_mut::<SnippetsApp>().core.borrow().selection,
        Selection::Folder("Ops".into())
    );
}

/// A synthetic row is operated by its measured list owner; a removed row's
/// exported handle must never operate the replacement row.
#[test]
fn exported_list_rows_activate_and_refuse_removed_handles() {
    let mut app = launched(&[
        snippet("First", "first body"),
        snippet("Second", "second body"),
    ]);
    open_row(&mut app, "First");
    assert_eq!(body_shown(&mut app), "first body");
    let (second, _) = by_title(&app, "Second");
    let row = list_item(&mut app, "Second");
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Focus)
        .expect("the measured list owner focuses the addressed row");
    app.settle();
    assert_eq!(body_shown(&mut app), "second body");
    let focused = app
        .harness()
        .focused_access_node()
        .expect("the list remains the actual keyboard focus owner");
    let named = |child: &ui::a11y::AccessNode, title: &str| {
        child
            .name
            .as_deref()
            .is_some_and(|name| name.ends_with(&format!(" {title}")))
    };
    assert!(
        focused
            .children
            .iter()
            .any(|child| named(child, "Second") && child.state.focused),
        "the addressed Second row carries focused semantics"
    );
    assert!(
        focused
            .children
            .iter()
            .all(|child| !named(child, "First") || !child.state.focused),
        "First must no longer carry focused semantics"
    );
    open_row(&mut app, "First");
    assert_eq!(body_shown(&mut app), "first body");
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .expect("the measured list owner handles its row");
    app.settle();
    assert_eq!(body_shown(&mut app), "second body");
    assert_eq!(
        app.state().snippets.len(),
        2,
        "opening a row preserves the library"
    );

    app.invoke_confirmed(Snippets::DELETE.with(second)).unwrap();
    app.settle();
    assert!(
        app.harness()
            .perform_a11y_action(row, &ui::a11y::Action::Activate)
            .is_err(),
        "a removed row's exported handle is refused"
    );
    assert_eq!(app.state().snippets.len(), 1);
    assert_eq!(body_shown(&mut app), "first body");
}

/// Copying a templated snippet fills its variables first.
#[test]
fn copying_a_templated_snippet_opens_the_variables_dialog() {
    let mut app = SnippetsApp::new();
    app.show_library(&presented([snippet("Greet", "Hello ${name}")]));
    app.core.borrow_mut().copy(&Id::from_raw("s0"));
    assert!(app.expand_open_for_test());
}

/// The list is one focus stop whose keys move the open snippet.
#[test]
fn the_list_moves_the_open_snippet_by_keys() {
    let mut app = launched(&[
        snippet("Alpha", "alpha body"),
        snippet("Beta", "beta body"),
        snippet("Gamma", "gamma body"),
    ]);
    let top = list_item(&mut app, "Alpha");
    app.harness()
        .perform_a11y_action(top, &ui::a11y::Action::Focus)
        .expect("the list focuses its row");
    app.settle();
    assert_eq!(body_shown(&mut app), "alpha body");
    for (key, body) in [
        ("Down", "beta body"),
        ("Up", "alpha body"),
        ("Up", "alpha body"),
        ("End", "gamma body"),
        ("Home", "alpha body"),
    ] {
        app.harness().key(key);
        app.settle();
        assert_eq!(body_shown(&mut app), body, "after {key}");
    }
    assert_eq!(
        app.state().snippets.len(),
        3,
        "moving through the list edits nothing"
    );
}

/// Typing in the body splices the stored text with field edits at the
/// body's path; the editor follows a collaborator's edit and an undo.
#[test]
fn body_edits_are_text_actions_and_the_editor_follows_the_document() {
    let mut app = launched(&[snippet("Only", "")]);
    let (id, _) = by_title(&app, "Only");
    let body = |app: &TestApp<SnippetsShell>| by_title(app, "Only").1.body;
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
    assert_eq!(body(&app), "Grün");
    assert!(app.undo(), "the typed word is one undo step");
    assert_eq!(body(&app), "");
    assert_eq!(body_shown(&mut app), "");

    app.merge_peer_edit(|doc| {
        Snippets::paths()
            .snippets
            .item(&id)
            .body
            .replace("remote 🌍\n")
            .apply_local(doc)
            .unwrap();
    });
    app.settle();
    assert_eq!(body(&app), "remote 🌍\n");
    assert_eq!(body_shown(&mut app), "remote 🌍\n");
}

/// The variable dialog is addressable and peer-local, and holds the body
/// while it is open.
#[test]
fn variable_dialog_is_semantic_and_local() {
    let mut app = SnippetsApp::new();
    app.show_library(&presented([snippet("Greet", "Hello ${name}")]));
    let mut doc = AutoCommit::new();
    let heads = doc.get_heads();
    app.core.borrow_mut().start_expand(&Id::from_raw("s0"));
    assert!(app.perform_semantic_action(
        "snippets.expand.field.name",
        &ui::a11y::Action::SetValue("Grün".into()),
        &mut ui::widget::Edits::lend(&mut doc),
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
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    assert!(app.perform_semantic_action(
        "snippets.expand.cancel",
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    assert!(!app.expand_open_for_test());
    assert_eq!(doc.get_heads(), heads, "the dialog writes nothing");
}

/// Focusing the New button does not author a snippet; activating it does.
#[test]
fn focusing_new_snippet_does_not_activate_it() {
    let mut app = launched(&[snippet("Only", "")]);
    let label = crate::tr("snippets-new");
    let button = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.label() == Some(label.as_str())
                && node.supports_action(ui::accesskit::Action::Click)
        })
        .map(|(id, _)| id)
        .expect("the New button");
    app.harness()
        .perform_a11y_action(button, &ui::a11y::Action::Focus)
        .expect("the New button takes focus");
    app.settle();
    assert_eq!(
        app.state().snippets.len(),
        1,
        "focusing a button authors nothing"
    );
    activate(&mut app, &label);
    assert_eq!(
        app.state().snippets.len(),
        2,
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
    let other = app.core.borrow().visible[2].clone();
    app.requests
        .borrow_mut()
        .push(Action::OpenSnippet(other.clone()));
    Framed::harness_mut(&mut app).relayout();
    let (language, copy) = sizes(&app);
    assert_eq!(app.open_id_for_test(), Some(other));
    assert!(language.width() > language.height() && copy.width() > copy.height());
    let targets = Framed::harness(&app).targets();
    assert!(targets.contains_key("snippets.favorite") && targets.contains_key("snippets.more"));
}

// ---- the published tree, agents and history ------------------------------

fn has_control(app: &mut TestApp<SnippetsShell>, label: &str) -> bool {
    app.harness()
        .a11y_nodes()
        .iter()
        .any(|(_, node)| node.label() == Some(label))
}

/// A new window starts on an empty library; captures stage the starter
/// library through actions and their view state through `stage`.
#[test]
fn captures_stage_the_starter_library_and_a_new_window_is_empty() {
    let live = TestApp::<SnippetsShell>::new().unwrap();
    assert!(live.state().is_empty(), "a fresh install has no snippets");

    let mut empty = TestApp::<SnippetsShell>::capture(Some("empty")).unwrap();
    assert!(empty.state().is_empty());
    assert!(has_control(&mut empty, "New snippet"));

    let mut detail = TestApp::<SnippetsShell>::capture(Some("detail")).unwrap();
    assert_eq!(detail.harness().inspection().viewport, [420, 760]);
    assert!(detail.state().snippets.len() >= 10);
    assert!(has_control(&mut detail, "Back"), "the detail is pushed in");
    detail.harness().click_named("nav-shell-back").unwrap();
    detail.settle();
    assert!(
        !has_control(&mut detail, "Back"),
        "Back returns to the snippet list"
    );

    let mut narrow = TestApp::<SnippetsShell>::capture(Some("narrow")).unwrap();
    assert_eq!(narrow.harness().inspection().viewport, [720, 720]);
    // The drawer floats over the list, its own toggle under it.
    assert!(
        has_control(&mut narrow, "All snippets"),
        "the overlay sidebar is open"
    );
    narrow.harness().key("Escape");
    narrow.settle();
    assert!(
        !has_control(&mut narrow, "All snippets"),
        "Escape puts the drawer away"
    );
}

/// An agent reaches every snippet, folder and tag through the one
/// published tree: each change is one call, a verb on the snippet it acts
/// on or a field edit at a path, and opening a snippet shows it.
#[test]
fn an_agent_edits_and_opens_snippets_one_call_each() {
    let mut app = TestApp::<SnippetsShell>::new().unwrap();
    app.call(
        "new-snippet",
        serde_json::json!({"title": "Deploy", "folder": "Ops"}),
    )
    .unwrap();
    let (deploy, created) = by_title(&app, "Deploy");
    assert_eq!(created.folder.0, "Ops");
    let surface = app.agent_surface().to_string();
    assert!(surface.contains("Create a snippet"), "the tree is labelled");

    app.call_on(
        "snippets.library/derived/folder:Ops",
        "new-snippet",
        serde_json::json!({"title": "Rollback"}),
    )
    .unwrap();
    assert_eq!(
        by_title(&app, "Rollback").1.folder.0,
        "Ops",
        "a folder files a new snippet under itself"
    );

    let paths = Snippets::paths().snippets.item(&deploy);
    for edit in [
        paths.tags.insert(&"k8s".to_owned()),
        paths.language.set(Language::Shell),
        paths.body.replace("kubectl rollout restart deploy/${name}"),
    ] {
        app.call("field-edit", serde_json::json!({ "edit": edit }))
            .unwrap();
    }
    let (_, deploy_now) = by_title(&app, "Deploy");
    assert!(deploy_now.tags.contains(&"k8s".to_owned()));
    assert_eq!(deploy_now.language, Language::Shell);
    assert_eq!(deploy_now.body, "kubectl rollout restart deploy/${name}");
    assert!(app.undo(), "the body edit is one undo step");
    assert_eq!(by_title(&app, "Deploy").1.body, "");

    let item = item_resource_id::<Snippets>("snippets", &deploy);
    app.call_on(&item, "duplicate", serde_json::json!({}))
        .unwrap();
    assert_eq!(
        app.state().snippets.len(),
        3,
        "a copy, titled as its original"
    );

    app.call("open", serde_json::json!({ "anchor": item }))
        .unwrap();
    assert!(
        app.call(
            "open",
            serde_json::json!({ "anchor": item_resource_id::<Snippets>("snippets", &Id::from_raw("gone")) })
        )
        .is_err(),
        "a snippet that does not exist is not opened"
    );
    app.settle();
    assert_eq!(
        app.surface_mut::<SnippetsApp>().open_id_for_test(),
        Some(deploy),
        "opening shows the snippet"
    );
    app.call_on(
        "snippets.library/derived/tag:k8s",
        "reveal",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        app.surface_mut::<SnippetsApp>().core.borrow().selection,
        Selection::Tag("k8s".into()),
        "a tag reveals its snippets"
    );
    app.call_on(
        "snippets.library/derived/tag:k8s",
        "remove-tag",
        serde_json::json!({}),
    )
    .unwrap();
    assert!(by_title(&app, "Deploy").1.tags.is_empty());

    app.call_on(&item, "delete", serde_json::json!({}))
        .expect_err("deleting asks for confirmation");
    app.call_confirmed_on(&item, "delete", serde_json::json!({}))
        .unwrap();
    assert_eq!(app.state().snippets.len(), 2);
}

/// What an agent does for a person is history the person reads in their
/// language: the app's sentence for each field and verb, naming the
/// snippet as it was called, and who did it for whom.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    use ui::tilo_resource::Atom;

    let mut app = launched(&[snippet("Deploy", "")]);
    let (deploy, _) = by_title(&app, "Deploy");
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <Snippets as ui::tilo_resource::Resource>::KIND;
    let item = item_resource_id::<Snippets>("snippets", &deploy);
    let paths = Snippets::paths().snippets.item(&deploy);
    for edit in [
        paths.favorite.set(true),
        paths.favorite.set(false),
        paths.folder.set(Atom("Release".into())),
        paths.language.set(Language::JavaScript),
        paths.body.splice(0, 0, "npm run deploy"),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": edit }),
        )
        .unwrap();
    }
    app.call_by(agent.clone(), &item, "record-use", serde_json::json!({}))
        .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "new-snippet",
        serde_json::json!({"title": "Rollback"}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "field-edit",
        serde_json::json!({ "edit": paths.title.replace("Ship") }),
    )
    .unwrap();
    app.call_by(
        agent,
        &item,
        "duplicate",
        serde_json::json!({"title": "Ship again"}),
    )
    .unwrap();

    let entries: Vec<_> = app
        .history()
        .into_iter()
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences = app.sentences(&entries);
    assert_eq!(
        sentences,
        [
            "Starred Deploy",
            "Unstarred Deploy",
            "Moved Deploy to the folder Release",
            "Set the language of Deploy to JavaScript",
            "Edited the body of Deploy",
            "Copied Deploy",
            "Created Rollback",
            "Renamed Deploy",
            "Duplicated Ship",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada")),
        "the entry names the agent and the person it acted for"
    );
}

/// Click the control assistive technology names `label`, at the centre of
/// where it is drawn, as a pointer does.
fn click_label(app: &mut TestApp<SnippetsShell>, label: &str) {
    let nodes = app.harness().a11y_nodes();
    let names: Vec<_> = nodes.iter().filter_map(|(_, node)| node.label()).collect();
    let bounds = nodes
        .iter()
        .filter(|(_, node)| node.label() == Some(label))
        .find_map(|(_, node)| node.bounds().filter(|b| b.width() > 0.0))
        .unwrap_or_else(|| panic!("no drawn control named {label:?} among {names:?}"));
    let (x, y) = (
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    app.harness().click_at(x, y);
    app.settle();
}

/// A pointer click on Copy copies the open snippet and counts the use.
#[test]
fn a_pointer_click_on_copy_copies() {
    let mut app = launched(&[snippet("Deploy", "kubectl apply")]);
    open_row(&mut app, "Deploy");
    click_label(&mut app, "Copy");
    assert_eq!(
        by_title(&app, "Deploy").1.uses.get(),
        1,
        "a click on Copy copies: {:?}",
        app.harness().control_failures()
    );
}

/// A picked language and a typed name apply for the record's owner and
/// for an editor of a shared workspace alike.
#[test]
fn owners_and_editors_set_the_language_and_the_name() {
    for role in [None, Some(ui::Role::Editor)] {
        let mut app = launched(&[snippet("Deploy", "kubectl apply")]);
        if let Some(role) = role {
            app.set_role(role);
        }
        open_row(&mut app, "Deploy");
        {
            let harness = app.harness();
            let picker = harness.a11y_node_named("Language").unwrap();
            harness
                .perform_a11y_action(picker, &ui::a11y::Action::SetValue("Rust".into()))
                .unwrap();
        }
        app.settle();
        assert_eq!(
            by_title(&app, "Deploy").1.language,
            Language::Rust,
            "{role:?}: the picked language applies: {:?}",
            app.harness().control_failures()
        );
        let (id, _) = by_title(&app, "Deploy");
        app.harness()
            .click_named(&format!("snippets.snippet:{id}.title"))
            .unwrap();
        app.harness().type_text(" prod");
        app.settle();
        assert_eq!(by_title(&app, "Deploy prod").0, id, "{role:?}: renamed");
    }
}
