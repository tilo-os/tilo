//! Settings' common actions are found by every word people use for them.

use ui::tilo_resource::{Effect as _, Resource as _, declaration_spec};

/// Each word people use for one of the app's common actions ranks that
/// action among the app's first three, on the fields every search ranks an
/// app's action by: the words come from the action's declaration, so an
/// action that loses them drops out here.
#[test]
fn people_find_settings_actions_by_the_words_they_use() {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(
        <settings::panes::audio::AudioEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<settings::panes::audio::AudioEffect>::OPERATION)),
    );
    actions.extend(
        <settings::panes::display::DisplayEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<settings::panes::display::DisplayEffect>::OPERATION)),
    );
    // The screen reader is turned on by the `accessibility` section's verb.
    actions.extend(
        <settings::accessibility_settings::AccessibilitySettings>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <settings::panes::appearance::AppearanceEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<settings::panes::appearance::AppearanceEffect>::OPERATION)),
    );
    actions.extend(
        <settings::panes::keyboard_shortcuts::ShortcutEffect>::declarations()
            .iter()
            .map(|effect| {
                effect.spec(<settings::panes::keyboard_shortcuts::ShortcutEffect>::OPERATION)
            }),
    );
    // Shortcuts are rebound by the `shortcuts` section's own verbs.
    actions.extend(
        <settings::shortcut_settings::ShortcutPrefs as ui::tilo_resource::Resource>::declarations()
            .iter()
            .map(ui::tilo_resource::declaration_spec),
    );
    actions.extend(
        <settings::shortcut_settings::ShortcutPrefs>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <settings::panes::network::NetworkEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<settings::panes::network::NetworkEffect>::OPERATION)),
    );
    actions.extend(
        <settings::panes::bluetooth::BluetoothEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<settings::panes::bluetooth::BluetoothEffect>::OPERATION)),
    );
    actions.extend(
        <settings::panes::mouse_keyboard::InputEffect>::declarations()
            .iter()
            .map(|effect| effect.spec(<settings::panes::mouse_keyboard::InputEffect>::OPERATION)),
    );
    // The data used is started again and forgotten by the `network_usage`
    // section's verbs.
    actions.extend(
        <settings::prefs::NetworkUsage>::declarations()
            .iter()
            .map(declaration_spec),
    );
    let mut missed = Vec::new();
    for (query, id) in [
        ("loudness", "set-output-volume"),
        ("resolution", "set-display-mode"),
        ("refresh rate", "set-display-mode"),
        ("portrait", "rotate-display"),
        ("narrator", "set-screen-reader"),
        ("wallpaper", "choose-wallpaper"),
        ("hotkey", "rebind"),
        ("tethering", "set-hotspot"),
        ("microphone", "select-input"),
        ("compose", "set-compose-key"),
        ("data usage", "reset-data-used"),
        ("bandwidth", "show-data-usage"),
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
