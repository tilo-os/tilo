//! The keyboard shortcut a palette row shows beside its action.

use settings_store::hardware::ShortcutAction;

/// The shortcut that does what an action does, as the active keyboard
/// layout spells it, when the desktop has one: the catalogue id in
/// [`settings_store::keyboard_shortcuts`] and the action it matches.
pub(crate) const ACTION_SHORTCUTS: &[(&str, &str)] = &[
    ("overview.toggle", "workspace-overview"),
    ("quick-settings.open", "quick-settings"),
    ("quick-settings.clipboard", "clipboard-history"),
    ("shortcut-help.toggle", "shortcut-help"),
    ("window.focus-previous", "focus-previous-window"),
    ("window.focus-next", "focus-next-window"),
    ("tab.previous", "previous-tab"),
    ("tab.next", "next-tab"),
    ("tab.menu", "tab-menu"),
    ("tab.reopen", "reopen-closed-tab"),
    ("strip.focus-previous", "previous-strip"),
    ("strip.focus-next", "next-strip"),
    ("workspace.previous", "previous-workspace"),
    ("workspace.next", "next-workspace"),
    ("group.move-left", "move-group-left"),
    ("group.move-right", "move-group-right"),
    (
        "group.move-to-strip-previous",
        "move-group-to-previous-strip",
    ),
    ("group.move-to-strip-next", "move-group-to-next-strip"),
    (
        "group.move-to-display-previous",
        "move-group-to-previous-display",
    ),
    ("group.move-to-display-next", "move-group-to-next-display"),
    ("group.maximize", "maximize-window"),
    ("group.toggle-maximize", "toggle-maximize"),
    ("group.restore", "restore-window"),
    ("group.fullscreen", "toggle-fullscreen"),
    ("group.narrower", "resize-narrower"),
    ("group.wider", "resize-wider"),
    ("window.new.terminal", "open-terminal"),
    ("dictation.toggle", "dictation-toggle"),
    ("dictation.finish", "dictation-finish"),
    ("dictation.cancel", "dictation-cancel"),
    ("capture.open", "fixed-capture-screenshot"),
    ("room.activity.open", "fixed-workspace-history"),
    ("lock", "lock"),
    ("emoji.picker", "emoji-picker"),
    ("input.source.next", "next-input-source"),
    ("magnifier.toggle", "toggle-magnifier"),
    ("magnifier.zoom-in", "magnifier-zoom-in"),
    ("magnifier.zoom-out", "magnifier-zoom-out"),
];

/// `shortcut_id`'s current chord as the active layout's keycaps spell it,
/// from `config`.
pub(crate) fn shortcut_text(
    config: &settings_store::hardware::ShortcutConfig,
    shortcut_id: &str,
) -> Option<String> {
    // `entry` constructs every shortcut and its defaults before selecting one.
    // A palette row needs only its current chord, not that entire catalogue.
    if let Some(action) = ShortcutAction::ALL
        .into_iter()
        .find(|action| action.id() == shortcut_id)
    {
        return config
            .bindings
            .get(&action)
            .map(crate::shell::keyboard_shortcuts::chord_text);
    }
    settings_store::keyboard_shortcuts::FIXED_SHORTCUTS
        .iter()
        .find(|shortcut| shortcut.id == shortcut_id)?
        .chords()
        .first()
        .map(crate::shell::keyboard_shortcuts::chord_text)
}

/// The shortcut shown beside the action `id`, if it has one.
pub(crate) fn action_shortcut(
    config: &settings_store::hardware::ShortcutConfig,
    id: &str,
) -> Option<String> {
    ACTION_SHORTCUTS
        .iter()
        .find(|(action, _)| *action == id)
        .and_then(|(_, shortcut)| shortcut_text(config, shortcut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_text_matches_rebound_fixed_and_unassigned_chords() {
        let mut config = settings_store::hardware::ShortcutConfig::default();
        config.bindings.insert(
            ShortcutAction::NextWorkspace,
            settings_store::hardware::KeyChord::parse("Super+Alt+n").expect("valid test chord"),
        );
        config.bindings.remove(&ShortcutAction::PreviousWorkspace);
        for entry in settings_store::keyboard_shortcuts::catalogue(&config) {
            assert_eq!(
                shortcut_text(&config, entry.id),
                entry
                    .chords
                    .first()
                    .map(crate::shell::keyboard_shortcuts::chord_text),
                "{} keeps its configured, fixed or unassigned chord",
                entry.id,
            );
        }
        assert_eq!(shortcut_text(&config, "no.such.shortcut"), None);
    }
    #[test]
    fn every_shortcut_that_names_an_action_names_one_this_desktop_publishes() {
        use super::super::Action;
        use crate::shell::search::providers::actions::ActionsProvider;
        use crate::shell::search::providers::actions::tests::action_results;
        let provider = ActionsProvider::new();
        let chrome = crate::shell::search::providers::actions::tests::chrome_with_room();
        let results = action_results(&provider, &chrome);
        let mut chords = 0;
        for shortcut in ShortcutAction::ALL {
            let Some((provider_id, id)) = shortcut.action() else {
                continue;
            };
            chords += 1;
            if provider_id != "actions" {
                // Lock and the like live on another provider's door, which
                // this test has no registry to query.
                continue;
            }
            assert!(
                Action::ALL.iter().any(|action| action.id == id),
                "the shortcut catalogue says {shortcut:?} is {id}, which this provider does not publish"
            );
            assert!(
                results.iter().any(|result| result.id == id),
                "{id} is published but a person searching cannot see it"
            );
        }
        assert!(chords >= 20, "the catalogue names actions for most chords");
        assert!(
            ShortcutAction::Search.action().is_none(),
            "the search chord keeps the host's own handling until a row publishes it"
        );
    }

    #[test]
    fn magnifier_steps_and_the_maximize_toggle_run_from_the_action_door() {
        use crate::shell::search::providers::actions::tests::{
            chrome_with_room, invoke_shell_action,
        };
        let chrome = chrome_with_room();
        for id in [
            "magnifier.toggle",
            "magnifier.zoom-in",
            "magnifier.zoom-out",
        ] {
            let before = chrome.borrow().window_requests_queued();
            invoke_shell_action(&chrome, id, &serde_json::json!({}))
                .unwrap_or_else(|error| panic!("{id} runs from the action door: {error}"));
            assert!(
                chrome.borrow().window_requests_queued() > before,
                "{id} asks the compositor for the change the chord asks for"
            );
        }
    }

    #[test]
    fn moving_a_group_to_another_display_refuses_on_a_single_display_desk() {
        use crate::shell::search::action_api::ActionError;
        use crate::shell::search::providers::actions::tests::{
            chrome_with_room, invoke_shell_action,
        };
        let chrome = chrome_with_room();
        assert_eq!(chrome.borrow().tree().desktop.outputs.displays.len(), 0);
        for id in [
            "group.move-to-display-previous",
            "group.move-to-display-next",
        ] {
            let error = invoke_shell_action(&chrome, id, &serde_json::json!({}))
                .expect_err("there is nowhere to move the group to");
            assert!(
                matches!(error, ActionError::Explained(_)),
                "{id} says why it cannot act in the person's words, got {error:?}"
            );
        }
    }
}
