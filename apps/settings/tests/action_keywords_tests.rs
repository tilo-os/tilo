//! Settings' common actions are found by every word people use for them.

use ui::tilo_resource::{Effect as _, EffectDecl, Resource as _, declaration_spec};

#[test]
fn people_find_settings_actions_by_the_words_they_use() {
    let mut actions: Vec<ui::tilo_actions::ActionSpec> = Vec::new();
    actions.extend(
        <settings::panes::audio::AudioEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<settings::panes::audio::AudioEffect>),
    );
    actions.extend(
        <settings::panes::display::DisplayEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<settings::panes::display::DisplayEffect>),
    );
    actions.extend(
        <settings::accessibility_settings::AccessibilitySettings>::declarations()
            .iter()
            .map(declaration_spec),
    );
    actions.extend(
        <settings::panes::appearance::AppearanceEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<settings::panes::appearance::AppearanceEffect>),
    );
    actions.extend(
        <settings::panes::keyboard_shortcuts::ShortcutEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<settings::panes::keyboard_shortcuts::ShortcutEffect>),
    );
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
            .map(EffectDecl::spec_of::<settings::panes::network::NetworkEffect>),
    );
    actions.extend(
        <settings::panes::bluetooth::BluetoothEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<settings::panes::bluetooth::BluetoothEffect>),
    );
    actions.extend(
        <settings::panes::mouse_keyboard::InputEffect>::declarations()
            .iter()
            .map(EffectDecl::spec_of::<settings::panes::mouse_keyboard::InputEffect>),
    );
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
