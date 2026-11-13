//! Files' common actions are found by the words people use for them.

use ui::tilo_resource::{Effect as _, EffectDecl, Resource as _, declaration_spec};

#[test]
fn people_find_files_actions_by_the_words_they_use() {
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
    let mut missed = Vec::new();
    for (query, id) in [
        ("mkdir", "new-folder"),
        ("make directory", "new-folder"),
        ("create folder", "new-folder"),
        ("new folder", "new-folder"),
        ("delete", "trash"),
        ("throw away", "trash"),
        ("reload", "refresh"),
        ("parent directory", "up"),
        ("dotfiles", "toggle-hidden"),
        ("icon view", "grid"),
        ("details view", "list"),
        ("empty bin", "empty-trash"),
        ("command line", "open-terminal"),
        ("recent files", "recent"),
        ("open path", "go-to"),
        ("bookmark", "add-bookmark"),
    ] {
        let found: Vec<&str> = ui::search::system::actions_found_by(query, &actions)
            .into_iter()
            .take(3)
            .map(|action| action.id.as_str())
            .collect();
        if !found.contains(&id) {
            missed.push(format!("{query} -> {id}, found {found:?}"));
        }
    }
    assert!(missed.is_empty(), "{missed:#?}");
}
