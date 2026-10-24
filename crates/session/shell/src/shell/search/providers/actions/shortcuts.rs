//! The keyboard shortcut a palette row shows beside its action.

use settings_store::hardware::ShortcutAction;

/// The shortcut that does what an action does, as the active keyboard
/// layout spells it, when the desktop has one: the catalogue id in
/// [`settings_store::keyboard_shortcuts`] and the action it matches.
pub(super) const ACTION_SHORTCUTS: &[(&str, &str)] = &[
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
}
