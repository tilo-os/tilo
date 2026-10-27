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

/// What acts on the person's selection says so in its contract, so an app's
/// search lists it before the other actions.
#[test]
fn what_acts_on_the_selection_says_so() {
    let actions = actions();
    let action = actions
        .iter()
        .find(|action| action.id == "toggle-comment")
        .expect("Toggle line comment is declared");
    assert!(
        action
            .contract
            .requirements()
            .contains(&ui::tilo_actions::ContextRequirement::Selection),
        "Toggle line comment acts on the selection"
    );
}
