//! Notes' actions wear pictures of their own in every list of actions.

use ui::tilo_resource::{Effect as _, Resource as _, declaration_spec};

fn actions() -> Vec<ui::tilo_actions::ActionSpec> {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(
        <crate::notebook::Notebook>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <crate::effect::NotesEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<crate::effect::NotesEffect>::OPERATION)),
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
        "new-note",
        "new-child",
        "rename",
        "delete",
        "duplicate",
        "move-up",
        "move-down",
        "indent",
        "outdent",
        "new-tag",
        "tag",
        "back",
        "forward",
        "copy-link",
        "insert-image",
        "export-markdown",
    ] {
        assert!(
            actions
                .iter()
                .any(|action| action.id == id && action.icon.is_some()),
            "{id} declares its icon"
        );
    }
}
