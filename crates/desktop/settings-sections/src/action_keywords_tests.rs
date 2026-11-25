//! Settings' actions are found by the words people use for them: a verb declares the aliases
//! beside its label, and the shared action search ranks a query by both, so the person's word
//! names the verb it means in the top three.

use ui::tilo_resource::{Resource as _, declaration_spec};

use crate::accessibility::AccessibilitySettings;
use crate::control_hints::ControlHintsPrefs;
use crate::network_usage::NetworkUsage;
use crate::prefs::NotificationPrefs;
use crate::shortcuts::ShortcutPrefs;

/// Every verb the sections declare, as the session publishes them to search.
fn verbs() -> Vec<tilo_actions::ActionSpec> {
    let mut verbs = Vec::new();
    verbs.extend(
        AccessibilitySettings::declarations()
            .iter()
            .map(declaration_spec),
    );
    verbs.extend(
        ControlHintsPrefs::declarations()
            .iter()
            .map(declaration_spec),
    );
    verbs.extend(
        NotificationPrefs::declarations()
            .iter()
            .map(declaration_spec),
    );
    verbs.extend(NetworkUsage::declarations().iter().map(declaration_spec));
    verbs.extend(ShortcutPrefs::declarations().iter().map(declaration_spec));
    verbs
}

#[test]
fn people_find_settings_actions_by_the_words_they_use() {
    let verbs = verbs();
    for (query, wanted) in [
        ("narrator", "set-screen-reader"),
        ("hotkey", "rebind"),
        ("data usage", "reset-data-used"),
        ("app notifications", "set-app-kinds"),
        ("teach me again", "show-control-hints-again"),
        ("mute an app", "mute-app"),
    ] {
        let found: Vec<&str> = ui::search::system::actions_found_by(query, &verbs)
            .into_iter()
            .take(3)
            .map(|verb| verb.id.as_str())
            .collect();
        assert!(
            found.contains(&wanted),
            "`{query}` should find {wanted}, found {found:?}"
        );
    }
}
