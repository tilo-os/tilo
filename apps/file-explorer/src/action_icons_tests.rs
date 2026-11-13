//! Files' actions wear pictures of their own in every language.

use ui::tilo_resource::{Effect as _, EffectDecl, Resource as _, declaration_spec};

fn actions() -> Vec<ui::tilo_actions::ActionSpec> {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(
        <crate::resource::FileExplorer>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <crate::effects::FilesEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<crate::effects::FilesEffect>),
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
        "new-folder",
        "rename",
        "trash",
        "move",
        "copy-into",
        "compress",
        "extract",
        "refresh",
        "paste",
        "go-to",
        "open-trash",
        "up",
        "back",
        "forward",
        "copy-selection",
        "cut-selection",
        "toggle-hidden",
        "list",
        "grid",
        "open-terminal",
        "empty-trash",
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
fn what_acts_on_the_selection_says_so() {
    let actions = actions();
    for id in ["copy-selection", "cut-selection"] {
        let action = actions
            .iter()
            .find(|action| action.id == id)
            .unwrap_or_else(|| panic!("{id} is declared"));
        assert!(
            action
                .contract
                .requirements()
                .contains(&ui::tilo_actions::ContextRequirement::Selection),
            "{id} acts on the selection"
        );
    }
}
