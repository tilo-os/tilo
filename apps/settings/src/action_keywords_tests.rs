//! Settings' common actions are found by every word people use for them.

use ui::tilo_resource::Effect as _;

/// Each word people use for one of the app's common actions ranks that
/// action among the app's first three, on the fields every search ranks an
/// app's action by: the words come from the action's declaration, so an
/// action that loses them drops out here.
#[test]
fn people_find_settings_actions_by_the_words_they_use() {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(<crate::panes::audio::AudioEffect>::specs());
    actions.extend(<crate::panes::display::DisplayEffect>::specs());
    actions.extend(<crate::panes::accessibility::AccessibilityEffect>::specs());
    actions.extend(<crate::panes::appearance::AppearanceEffect>::specs());
    actions.extend(<crate::panes::keyboard_shortcuts::ShortcutEffect>::specs());
    actions.extend(<crate::panes::network::NetworkEffect>::specs());
    actions.extend(<crate::panes::bluetooth::BluetoothEffect>::specs());
    actions.extend(<crate::panes::mouse_keyboard::InputEffect>::specs());
    let mut missed = Vec::new();
    for (query, id) in [
        ("loudness", "set-output-volume"),
        ("resolution", "set-display-mode"),
        ("refresh rate", "set-display-mode"),
        ("portrait", "rotate-display"),
        ("narrator", "set-screen-reader"),
        ("wallpaper", "choose-wallpaper"),
        ("hotkey", "bind-shortcut"),
        ("tethering", "set-hotspot"),
        ("microphone", "select-input"),
        ("compose", "set-compose-key"),
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
