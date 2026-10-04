use super::*;
use crate::resource::SnippetsShell;
use std::rc::Rc;
use ui::framework::ResourceSurface;
use ui::headless::Framed;
use ui::prelude::TestApp;
use ui::tilo_resource::Invoke;

/// The app on a fresh record holding `snippets`, created through the door.
fn launched(snippets: Vec<Snippet>) -> TestApp<SnippetsShell> {
    let mut app = TestApp::<SnippetsShell>::new().unwrap();
    for snippet in snippets {
        app.invoke(Invoke::apply(&actions::create_snippet_action(snippet)).unwrap())
            .unwrap();
    }
    app
}

fn snippet(title: &str, body: &str) -> Snippet {
    let mut s = Snippet::blank("Ops");
    s.title = title.into();
    s.body = body.into();
    s
}

/// Click the control labelled `label` where the window shows it.
fn click(app: &mut TestApp<SnippetsShell>, label: &str) {
    let harness = app.harness();
    let nodes = harness.a11y_nodes();
    let bounds = nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label))
        .and_then(|(_, node)| node.bounds())
        .unwrap_or_else(|| {
            let labels: Vec<&str> = nodes.iter().filter_map(|(_, n)| n.label()).collect();
            panic!("no control labelled {label:?} among {labels:?}")
        });
    harness.click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    app.settle();
}

/// Activate the control labelled `label` as assistive technology does.
fn activate(app: &mut TestApp<SnippetsShell>, label: &str) {
    let harness = app.harness();
    let node = harness
        .a11y_node_named(label)
        .unwrap_or_else(|| panic!("no control labelled {label:?}"));
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

/// The toolbar's Star, Copy, New and Delete submit the library's
/// actions: each is one undo step, and the view follows the stored
/// library.
#[test]
fn affordances_submit_library_actions_as_one_undo_step_each() {
    let mut app = launched(vec![snippet("Plain", "echo hi"), snippet("Templated", "")]);
    let id = |app: &TestApp<SnippetsShell>, title: &str| {
        app.state().into_iter().find(|s| s.title == title).unwrap()
    };
    click(&mut app, "Plain");
    // A screen reader reaches the custom-drawn toolbar as a click does.
    activate(&mut app, "Add to favorites");
    assert!(id(&app, "Plain").favorite);
    assert!(app.undo(), "the star is one undo step");
    assert!(!id(&app, "Plain").favorite);

    click(&mut app, "Copy");
    assert_eq!(id(&app, "Plain").uses, 1, "copying records a use");

    click(&mut app, "New snippet");
    assert_eq!(app.state().len(), 3);
    // The new snippet is the one open, so the toolbar deletes it.
    activate(&mut app, "More actions");
    activate(&mut app, "Delete");
    let titles: Vec<String> = app.state().into_iter().map(|s| s.title).collect();
    assert_eq!(titles, ["Plain", "Templated"]);
    assert!(app.undo(), "the delete is one undo step");
    assert_eq!(app.state().len(), 3);
}

/// Typing in the body splices the stored text through the body's child
/// resource; the editor follows a collaborator's edit and an undo.
#[test]
fn body_edits_are_text_actions_and_the_editor_follows_the_document() {
    let mut app = launched(vec![snippet("Only", "")]);
    let id = app.state()[0].id;
    // Into the code card, below the detail pane's toolbar.
    let node = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.role() == ui::accesskit::Role::MultilineTextInput)
        .map(|(id, _)| id)
        .expect("body editor");
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().type_text("Grün");
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
    app.present(&Rc::new(vec![templated.clone()]));
    let mut doc = AutoCommit::new();
    let heads = doc.get_heads();
    app.core.borrow_mut().copy(templated.id, &mut doc);
    assert!(
        app.expand_open_for_test(),
        "a templated copy fills its variables first"
    );
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
    let mut app = TestApp::<SnippetsShell>::new().unwrap();
    let node = app
        .harness()
        .a11y_node_named("New snippet")
        .expect("new control");
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Focus)
        .unwrap();
    app.settle();
    assert!(app.state().is_empty(), "focusing a button authors nothing");
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    assert_eq!(app.state().len(), 1, "activating it creates a snippet");
}

#[test]
fn sidebar_places_narrow_the_list() {
    let app = SnippetsApp::new_demo();
    let sections = app.core.borrow().nav_sections();
    let ids: Vec<&str> = sections.iter().map(|section| section.id.as_str()).collect();
    assert_eq!(ids, ["library", "folders", "tags"]);

    let mut doc = AutoCommit::new();
    let all = app.visible_count_for_test();
    let mut app = app;
    app.requests
        .borrow_mut()
        .push(Action::Select(Selection::Smart(SmartFolder::Favorites)));
    app.settle(&mut doc);
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

#[test]
fn the_list_moves_the_open_snippet_by_keys() {
    let app = SnippetsApp::new_demo();
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

#[test]
fn the_shell_docks_wide_draws_narrow_and_pushes_very_narrow() {
    // Case `wide_keeps_sidebar_docked_and_open`.
    {
        let app = framed(SnippetsApp::new_demo(), 1280);
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
        let mut app = framed(SnippetsApp::new_demo(), 720);
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
        let mut app = framed(SnippetsApp::new_demo(), 480);
        assert!(app.is_push_for_test(), "very narrow window pushes");
        app.open_first_for_test();
        assert!(
            app.push_showing_detail_for_test(),
            "choosing a snippet pushes it in front of the list"
        );
    }

    // Case `wide_choice_does_not_push`.
    {
        let mut app = framed(SnippetsApp::new_demo(), 1280);
        app.open_first_for_test();
        assert!(!app.push_showing_detail_for_test());
    }
}

#[test]
fn the_content_column_lays_out_its_controls() {
    let mut app = framed(SnippetsApp::new_demo(), 1280);
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

fn framed(app: SnippetsApp, width: u32) -> Framed<SnippetsApp> {
    Framed::new(ui::State::new(), app, (width, 800), Theme::dark())
}

#[test]
fn presented_library_fills_and_clears_without_writing_a_document() {
    let mut app = SnippetsApp::new();
    assert_eq!(app.visible_count_for_test(), 0);
    app.present(&Rc::new(crate::seed::starter_library()));
    assert!(app.visible_count_for_test() >= 10);
    assert!(app.open_id_for_test().is_some());
    let mut doc = AutoCommit::new();
    let heads = doc.get_heads();
    app.sync(&mut doc, &automerge::ROOT);
    assert_eq!(
        doc.get_heads(),
        heads,
        "presentation never seeds a document"
    );
    app.present(&Rc::new(Vec::new()));
    assert_eq!(app.visible_count_for_test(), 0);
    assert!(
        app.open_id_for_test().is_none(),
        "deleting the last snippet clears the view"
    );
}

fn library_of(app: &SnippetsApp) -> Vec<Snippet> {
    app.core.borrow().library.snippets.clone()
}

#[test]
fn presented_library_fills_the_list_and_search_stays_local() {
    let mut app = TestApp::<SnippetsShell>::capture(None).unwrap();
    let before = app.state();
    let heads = app.read_document(|doc| doc.clone().get_heads());
    click(&mut app, "Filter");
    app.harness().commit_text("language:rust");
    app.settle();
    app.harness()
        .with_widget_mut::<SnippetsApp, _>(|view| {
            let core = view.core.borrow();
            assert_eq!(core.filter, "language:rust");
            assert!(!core.visible.is_empty() && core.visible.len() < before.len());
            assert!(
                core.visible
                    .iter()
                    .all(|id| core.library.get(*id).unwrap().language == Language::Rust)
            );
        })
        .unwrap();
    assert_eq!(
        app.read_document(|doc| doc.clone().get_heads()),
        heads,
        "filtering stays per-peer state"
    );
    assert_eq!(app.state(), before, "filtering never edits snippet content");
}

#[test]
fn canonical_tag_reveal_clears_private_filter_and_its_field() {
    let mut rust = snippet("Rust", "fn main() {}");
    rust.language = Language::Rust;
    let mut python = snippet("Python", "print('hi')");
    python.language = Language::Python;
    python.tags = vec!["python-only".into()];
    let python_id = python.id;
    let mut app = launched(vec![rust, python]);
    click(&mut app, "Filter");
    app.harness().commit_text("language:rust");
    app.settle();
    app.call_on(
        &actions::tag_resource_id("python-only"),
        "reveal",
        ui::serde_json::json!({}),
    )
    .unwrap();
    app.harness()
        .with_widget_mut::<SnippetsApp, _>(|view| {
            let core = view.core.borrow();
            assert!(core.filter.is_empty());
            assert_eq!(core.visible, vec![python_id]);
        })
        .unwrap();
    let nodes = app.harness().a11y_nodes();
    let filter = nodes
        .iter()
        .find(|(_, node)| node.label() == Some("Filter"))
        .unwrap();
    assert_eq!(filter.1.value().unwrap_or_default(), "");
}

#[test]
fn assistive_focus_moves_from_body_to_filter_without_authoring_text() {
    for pointer_body_focus in [false, true] {
        let mut source = snippet("Only", "seed");
        source.language = Language::Rust;
        let mut app = launched(vec![source]);
        let body = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::MultilineTextInput)
            .expect("mapped body editor");
        if pointer_body_focus {
            let rect = body.1.bounds().unwrap();
            app.harness()
                .click_at((rect.x0 + rect.x1) / 2.0, (rect.y0 + rect.y1) / 2.0);
        } else {
            app.harness()
                .perform_a11y_action(body.0, &ui::a11y::Action::Focus)
                .unwrap();
        }
        app.settle();
        let focused = app
            .harness()
            .focused_access_node()
            .expect("actual host focus owner");
        assert_eq!(focused.role, ui::a11y::Role::MultilineTextInput);
        assert_eq!(
            focused.semantic_key.as_deref(),
            Some("snippets.body"),
            "pointer focus must identify the body leaf, not the app wrapper"
        );
        app.harness()
            .with_widget_mut::<SnippetsApp, _>(|view| {
                assert!(view.core.borrow().body_editor.is_focused());
                // Native App blurs only the old mapped node; unlike Driver,
                // it does not sweep all competing privately focused editors.
                fn focus_body(widget: &mut dyn Widget, focused: bool) -> bool {
                    if widget.test_id() == Some("snippets.body") {
                        widget.set_focus(focused);
                        return true;
                    }
                    widget
                        .children_mut()
                        .into_iter()
                        .any(|child| focus_body(child, focused))
                }
                assert!(focus_body(view, false));
                assert!(
                    !view.core.borrow().body_editor.is_focused(),
                    "blurring just the outgoing mapped owner clears editor focus"
                );
                assert!(focus_body(view, true));
                assert!(view.core.borrow().body_editor.is_focused());
            })
            .unwrap();
        let before = app.state();
        let heads = app.read_document(|doc| doc.clone().get_heads());
        let filter = app.harness().a11y_node_named("Filter").unwrap();
        app.harness()
            .perform_a11y_action(filter, &ui::a11y::Action::Focus)
            .unwrap();
        app.harness().type_text("language:");
        app.harness().commit_text("rust");
        app.settle();
        app.harness()
            .with_widget_mut::<SnippetsApp, _>(|view| {
                let core = view.core.borrow();
                assert!(!core.body_editor.is_focused());
                assert_eq!(core.filter, "language:rust");
                assert_eq!(core.visible.len(), 1);
            })
            .unwrap();
        assert_eq!(app.state(), before, "filter typing must not edit the body");
        assert_eq!(app.read_document(|doc| doc.clone().get_heads()), heads);
        let body = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::MultilineTextInput)
            .map(|(id, _)| id)
            .unwrap();
        app.harness()
            .perform_a11y_action(body, &ui::a11y::Action::Focus)
            .unwrap();
        app.harness().type_text("Z");
        app.settle();
        assert_eq!(app.state()[0].body.len(), before[0].body.len() + 1);
        assert!(app.state()[0].body.contains('Z'));
        assert!(
            app.undo(),
            "returning to body still authors a normal undoable edit"
        );
        assert_eq!(app.state(), before);
    }
}
