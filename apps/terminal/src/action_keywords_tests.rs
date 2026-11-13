//! The terminal's common actions are found by every word people use for them.

use ui::tilo_resource::{Effect as _, EffectDecl, Resource as _, declaration_spec};

#[test]
fn people_find_terminal_actions_by_the_words_they_use() {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(
        <crate::resource::Terminal>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <crate::resource::TerminalEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<crate::resource::TerminalEffect>),
    );
    let mut missed = Vec::new();
    for (query, id) in [
        ("ctrl c", "interrupt"),
        ("kill process", "interrupt"),
        ("cls", "clear"),
        ("run again", "rerun"),
        ("copy output", "copy-visible"),
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
