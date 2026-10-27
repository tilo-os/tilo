//! The code editor's common actions are found by every word people use for them.

use ui::tilo_resource::{Effect as _, Resource as _, declaration_spec};

/// Each word people use for one of the app's common actions ranks that
/// action among the app's first three, on the fields every search ranks an
/// app's action by: the words come from the action's declaration, so an
/// action that loses them drops out here.
#[test]
fn people_find_code_editor_actions_by_the_words_they_use() {
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
    let mut missed = Vec::new();
    for (query, id) in [
        ("mkdir", "new-folder"),
        ("create file", "new-file"),
        ("grep", "search-project"),
        ("find in files", "search-project"),
        ("jump to line", "go-to-line"),
        ("diagnostics", "go-to-problems"),
        ("search and replace", "replace-text"),
        ("save changes", "save"),
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
