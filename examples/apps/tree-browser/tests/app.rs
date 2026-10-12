use example_tree_browser::{App, Notebook, SECTIONS};
use ui::a11y::{Action, Role};
use ui::headless::SemanticExpectation;
use ui::prelude::{Invoke, Replica, TestApp};

#[test]
fn the_detail_pane_follows_the_selection_and_edits_its_own_section() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    let targets = harness.targets();
    for target in [
        "tree-selected-item",
        "tree-title-input",
        "tree-notes-input",
        "tree-share",
    ] {
        let rect = targets
            .get(target)
            .unwrap_or_else(|| panic!("missing {target}"));
        assert!(
            rect.width() > 0.0 && rect.height() > 0.0,
            "{target}: {rect:?}"
        );
    }
    harness
        .perform_semantic_named("section-tree", Some("node:4"), &Action::Activate)
        .expect("select the meetings section");
    harness
        .expect(
            &SemanticExpectation::named("tree-selected-item")
                .role(Role::Heading { level: 1 })
                .name("Meetings"),
        )
        .expect("the detail pane follows the selection");
    harness
        .perform_named("tree-title-input", &Action::SetValue("Weekly sync".into()))
        .expect("edit the selected section's title");
    harness
        .expect(
            &SemanticExpectation::named("tree-title-preview")
                .role(Role::Heading { level: 2 })
                .name("Weekly sync"),
        )
        .expect("the preview shares the resource binding");
    harness
        .perform_semantic_named("section-tree", Some("node:3"), &Action::Activate)
        .expect("switch to another section");
    harness
        .expect(
            &SemanticExpectation::named("tree-title-input")
                .role(Role::TextInput)
                .value(""),
        )
        .expect("each section has its own fields");
    harness
        .perform_named("tree-title-input", &Action::SetValue("Roadmap".into()))
        .expect("edit the second section");
    assert!(app.undo(), "detail edits are undoable actions");
    let notebook = app.state();
    assert_eq!(
        notebook.section(4).map(|section| section.title.as_str()),
        Some("Weekly sync")
    );
    assert_eq!(
        notebook
            .section(3)
            .map_or("", |section| section.title.as_str()),
        ""
    );

    let harness = app.harness();
    harness
        .perform_semantic_named("section-tree", Some("node:4"), &Action::Activate)
        .expect("return to the first section");
    harness
        .expect(
            &SemanticExpectation::named("tree-title-input")
                .role(Role::TextInput)
                .value("Weekly sync"),
        )
        .expect("the first section's fields are intact");
    harness.click_named("tree-share").expect("share");
    assert!(app.state().section(4).is_some_and(|section| section.shared));
}

#[test]
fn compact_windows_navigate_between_full_width_panes() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let harness = app.harness();
    harness.resize((460, 820)).expect("compact layout");
    harness
        .expect(
            &SemanticExpectation::named("nav-shell-back")
                .role(Role::Button)
                .name("Back"),
        )
        .expect("the compact detail exposes an accessible Back");
    harness
        .perform_named("nav-shell-back", &Action::Activate)
        .expect("return to compact navigation");
    harness
        .expect(&SemanticExpectation::named("section-tree").role(Role::Tree))
        .expect("compact navigation shows the full-width tree pane");
    harness
        .perform_semantic_named("section-tree", Some("node:4"), &Action::Activate)
        .expect("choose a compact navigation row");
    harness
        .expect(
            &SemanticExpectation::named("tree-selected-item")
                .role(Role::Heading { level: 1 })
                .name("Meetings"),
        )
        .expect("compact selection navigates to the full-width detail");
    let issues = harness.accessibility_audit();
    assert!(issues.issues.is_empty(), "compact audit: {issues:#?}");
}

/// Only sections hold notes; the grouping nodes refuse edits.
#[test]
fn only_sections_take_notes() {
    let mut notebook = Replica::<Notebook>::new();
    let notes = Notebook::section_paths(6).notes.replace("Walked the dog.");
    notebook
        .apply(Invoke::edit(&notes).expect("an edit"))
        .expect("the journal is a section");
    let grouping = Notebook::section_paths(2).notes.replace("Not a section.");
    assert!(
        notebook
            .apply(Invoke::edit(&grouping).expect("an edit"))
            .is_err()
    );
    assert!(notebook.apply(Notebook::SHARE.with(2)).is_err());
    let state = notebook.state().expect("notebook");
    assert_eq!(
        state.section(6).map(|section| section.notes.as_str()),
        Some("Walked the dog.")
    );
    assert_eq!(
        state.sections.len(),
        SECTIONS.len(),
        "every section exists from the start"
    );
}

#[test]
fn initial_native_app_contract_is_certified() {
    let mut app = TestApp::<App>::new().expect("the app opens");
    let audit = app.harness().native_app_audit();
    assert!(audit.is_certified(), "{audit:#?}");
}
