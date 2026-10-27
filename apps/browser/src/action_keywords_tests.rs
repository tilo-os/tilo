//! The browser's common actions are found by every word people use for them.

use ui::tilo_resource::{Resource as _, declaration_spec};

/// Each word people use for one of the app's common actions ranks that
/// action among the app's first three, on the fields every search ranks an
/// app's action by: the words come from the action's declaration, so an
/// action that loses them drops out here.
#[test]
fn people_find_browser_actions_by_the_words_they_use() {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(
        <crate::resource::Browser>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <crate::history::BrowsingHistory>::declarations()
            .iter()
            .map(declaration_spec),
    );
    let mut missed = Vec::new();
    for (query, id) in [
        ("bookmark", "add-bookmark"),
        ("save page", "add-bookmark"),
        ("ad blocker", "set-site-protection"),
        ("tracking protection", "set-site-protection"),
        ("clear browsing data", "clear-history"),
        ("delete history", "clear-history"),
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
