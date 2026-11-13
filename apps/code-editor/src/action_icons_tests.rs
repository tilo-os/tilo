//! The code editor' actions wear pictures of their own in every list of actions.

use ui::tilo_resource::{Effect as _, EffectDecl, Resource as _, declaration_spec};

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
            .map(EffectDecl::spec_of::<crate::effects::CodeEditorEffect>),
    );
    actions
}

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
