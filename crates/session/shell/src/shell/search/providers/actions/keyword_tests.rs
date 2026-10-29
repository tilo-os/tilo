//! Every action the shell offers declares the other words people use for it.

use super::*;

/// Every action the shell declares names the other words people use
/// for it beside its title, and every language lists some: search finds
/// an action by them ("power off", "dark mode"), and one that
/// loses them is found by its label alone.
#[test]
fn every_shell_action_declares_the_other_words_people_use_for_it() {
    let mut declared: Vec<(&str, &str)> = Action::ALL
        .iter()
        .map(|action| (action.id, action.keywords))
        .collect();
    declared.extend([
        (
            "workspace.archived.restore",
            "search-kw-workspace-archived-restore",
        ),
        (
            "workspace.archived.delete",
            "search-kw-workspace-archived-delete",
        ),
    ]);
    declared.extend(super::super::session_actions::declared_keywords());
    assert!(
        declared.len() > 140,
        "the shell declares {} actions",
        declared.len()
    );
    let catalogues = [
        ("en-US", include_str!("../../../../../locales/en-US.ftl")),
        ("de-DE", include_str!("../../../../../locales/de-DE.ftl")),
    ];
    let mut missing = Vec::new();
    for (locale, source) in catalogues {
        let messages = tilo_copy_lint::catalog::messages(source);
        for (id, key) in &declared {
            let words = messages.get(*key).map_or(0, |list| {
                list.split(',')
                    .filter(|word| !word.trim().is_empty())
                    .count()
            });
            if words == 0 {
                missing.push(format!("{id} ({locale}: {key})"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "these actions declare no other words: {missing:#?}"
    );
}
