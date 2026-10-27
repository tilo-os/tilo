//! The code editor' actions wear pictures of their own in every list of actions.

use ui::tilo_resource::{Effect as _, Resource as _, declaration_spec};

fn actions() -> Vec<ui::tilo_actions::ActionSpec> {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(
        <crate::project::Project>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <crate::effects::CodeEditorEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<crate::effects::CodeEditorEffect>::OPERATION)),
    );
    actions
}

/// Every icon an action declares is one the toolkit draws, and the actions
/// people reach for most each declare one: an app's search shows the
/// action's own picture beside its label, not only the app's.
#[test]
fn actions_wear_icons_the_toolkit_draws() {
    let actions = actions();
    for action in &actions {
        if let Some(icon) = &action.icon {
            assert!(
                ui::icons::Icon::from_name(icon).is_some(),
                "{}: the toolkit draws no icon named {icon}",
                action.id
            );
        }
    }
    for id in [
        "new-file",
        "new-folder",
        "rename",
        "delete",
        "new-buffer",
        "save",
        "save-as",
        "find",
        "replace",
        "search-project",
        "go-to-line",
        "toggle-comment",
        "format-file",
        "toggle-sidebar",
        "run",
    ] {
        assert!(
            actions
                .iter()
                .any(|action| action.id == id && action.icon.is_some()),
            "{id} declares its icon"
        );
    }
}

/// Toggling a line comment acts where the caret is (its line, or the lines
/// of a selection when there is one): its contract asks for a caret, not
/// for a selection, so it is offered whenever a file is open and never
/// promised as an action on a selection that may not exist.
#[test]
fn toggling_a_comment_asks_for_a_caret_not_a_selection() {
    let actions = actions();
    let action = actions
        .iter()
        .find(|action| action.id == "toggle-comment")
        .expect("Toggle line comment is declared");
    let requirements = action.contract.requirements();
    assert!(
        requirements.contains(&ui::tilo_actions::ContextRequirement::Caret),
        "Toggle line comment acts at the caret: {requirements:?}"
    );
    assert!(
        !requirements.contains(&ui::tilo_actions::ContextRequirement::Selection),
        "Toggle line comment runs without a selection: {requirements:?}"
    );
}

/// Code's editing primitives address text by offsets or replace a file's
/// whole text: agents and scripts call them, a person edits in the editor.
/// They are never offered in a person's search or menus.
#[test]
fn editing_primitives_are_for_agents() {
    let actions = actions();
    for id in ["replace-range", "replace-text", "rerun", "rerun-test"] {
        let action = actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("{id} is declared"));
        assert_eq!(
            action.audience(),
            ui::tilo_actions::Audience::Agents,
            "{id} is offered to agents only"
        );
    }
    let run = actions
        .iter()
        .find(|action| action.id == "run")
        .expect("Run in terminal is declared");
    assert!(run.reaches_people(), "a person runs a command line too");
}
