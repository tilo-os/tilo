use super::*;
use crate::shell::chrome_state::{ChromeState, SearchScope};
use crate::shell::overlay_host::shortcuts::shortcut_reference_rows;
use crate::shell::state::WorkspaceTree;
use ui::Widget;
use ui::widget::Container;
use winit::event::Modifiers;
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn host() -> ShellOverlayHost {
    ShellOverlayHost::with_fixture_content(Box::new(Container::new()))
        .with_chrome(ChromeState::new(WorkspaceTree::bootstrap_demo()))
}

fn press(host: &mut ShellOverlayHost, key: NamedKey, modifiers: ModifiersState) -> bool {
    let mut handled = false;
    host.handle_shell_keyboard_event(
        &Key::Named(key),
        Modifiers::from(modifiers),
        false,
        &automerge::AutoCommit::new(),
        &automerge::ROOT,
        &mut handled,
    )
}

#[test]
fn nested_entry_points_dispatch_and_advertise_the_same_chords() {
    let mut host = host();
    let saved = host.shortcut_config.clone();
    host.set_nested_shortcuts(true);
    let control = ModifiersState::CONTROL;
    let overview = control | ModifiersState::SHIFT;
    let space = Key::Named(NamedKey::Space);
    assert!(host.owns_configured_chord(&space, Modifiers::from(control)));
    assert!(press(&mut host, NamedKey::Space, control));
    assert!(host.palette_visible);
    assert_eq!(
        host.chrome.as_ref().unwrap().borrow().search_scope,
        SearchScope::Workspace
    );
    assert!(press(&mut host, NamedKey::Space, control));
    assert!(!host.palette_visible);
    assert!(press(&mut host, NamedKey::Space, overview));
    assert!(host.overlay_flags().overview);
    assert!(press(&mut host, NamedKey::Space, overview));
    assert!(!host.overlay_flags().overview);
    let rows = shortcut_reference_rows(host.shortcut_config());
    assert!(rows.iter().any(|(_, keys)| keys.contains("Ctrl+Space")));
    assert!(
        rows.iter()
            .any(|(_, keys)| keys.contains("Ctrl+Shift+Space"))
    );
    assert!(press(&mut host, NamedKey::Space, ModifiersState::SUPER));
    assert!(
        host.palette_visible,
        "the original search chord remains usable"
    );
    assert!(press(&mut host, NamedKey::Space, ModifiersState::SUPER));
    assert!(press(&mut host, NamedKey::Tab, ModifiersState::SUPER));
    assert!(
        host.overlay_flags().overview,
        "the original overview chord remains usable"
    );
    assert!(press(&mut host, NamedKey::Tab, ModifiersState::SUPER));
    assert!(press(&mut host, NamedKey::F1, ModifiersState::empty()));
    assert!(host.overlay_flags().shortcut_help);
    let accessibility = host.accessibility();
    let description = format!("{accessibility:?}");
    assert!(description.contains("Ctrl+Space"));
    assert!(description.contains("Ctrl+Shift+Space"));
    assert_eq!(
        host.shortcut_config, saved,
        "session defaults are not preferences"
    );
    drop(host);
}

#[test]
fn native_sessions_keep_their_shortcuts_and_application_control_input() {
    let mut host = host();
    let defaults = ShortcutConfig::default();
    assert_eq!(host.shortcut_config(), &defaults);
    for modifiers in [
        ModifiersState::CONTROL,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
    ] {
        assert!(
            !host.owns_configured_chord(&Key::Named(NamedKey::Space), Modifiers::from(modifiers),)
        );
        assert!(!press(&mut host, NamedKey::Space, modifiers));
    }
    assert!(!host.palette_visible);
    assert!(press(&mut host, NamedKey::Space, ModifiersState::SUPER));
    assert!(host.palette_visible);
    host.set_nested_shortcuts(true);
    host.set_nested_shortcuts(false);
    assert_eq!(host.shortcut_config(), &defaults);
    drop(host);
}

#[test]
fn only_nested_entry_point_defaults_change() {
    let saved = ShortcutConfig::default();
    let effective = nested_config(&saved);
    assert_eq!(effective.validate(), Ok(()));
    assert_eq!(effective.actions, saved.actions);
    for (action, chord) in &saved.bindings {
        let expected = match action {
            ShortcutAction::Search => KeyChord::parse("Ctrl+Space").unwrap(),
            ShortcutAction::WorkspaceOverview => KeyChord::parse("Ctrl+Shift+Space").unwrap(),
            _ => chord.clone(),
        };
        assert_eq!(effective.bindings.get(action), Some(&expected));
    }
    assert_eq!(saved, ShortcutConfig::default());
}

#[test]
fn user_bindings_and_published_actions_win_over_nested_defaults() {
    let mut saved = ShortcutConfig::default();
    saved
        .set(ShortcutAction::Search, KeyChord::parse("Ctrl+j").unwrap())
        .unwrap();
    assert_eq!(
        nested_config(&saved).bindings[&ShortcutAction::Search],
        saved.bindings[&ShortcutAction::Search]
    );
    let mut saved = ShortcutConfig::default();
    saved
        .set(
            ShortcutAction::QuickSettings,
            KeyChord::parse("Ctrl+Space").unwrap(),
        )
        .unwrap();
    let effective = nested_config(&saved);
    assert_eq!(
        effective.bindings[&ShortcutAction::Search],
        saved.bindings[&ShortcutAction::Search]
    );
    assert_eq!(effective.validate(), Ok(()));
    let mut saved = ShortcutConfig::default();
    saved
        .set_action(
            KeyChord::parse("Ctrl+Space").unwrap(),
            settings_store::hardware::ActionChordTarget {
                provider: "example".into(),
                resource: "document".into(),
                action: "open".into(),
            },
            "Open document",
        )
        .unwrap();
    let effective = nested_config(&saved);
    assert_eq!(
        effective.bindings[&ShortcutAction::Search],
        saved.bindings[&ShortcutAction::Search]
    );
    assert_eq!(effective.actions, saved.actions);
    assert_eq!(effective.validate(), Ok(()));
}

#[test]
fn reloading_and_editing_preferences_never_persist_nested_defaults() {
    let mut host = host();
    host.set_nested_shortcuts(true);
    host.capture_shortcut_chord(
        ShortcutAction::QuickSettings,
        &Key::Character("q".into()),
        Modifiers::from(ModifiersState::CONTROL | ModifiersState::SHIFT),
    );
    assert_eq!(
        host.shortcut_config.bindings[&ShortcutAction::Search],
        KeyChord::parse("Super+Space").unwrap(),
    );
    assert_eq!(
        host.shortcut_config().bindings[&ShortcutAction::Search],
        KeyChord::parse("Ctrl+Space").unwrap(),
    );
    let mut saved = host.shortcut_config.clone();
    saved
        .set(ShortcutAction::Search, KeyChord::parse("Ctrl+j").unwrap())
        .unwrap();
    host.set_shortcut_config(saved).unwrap();
    assert!(!host.owns_configured_chord(
        &Key::Named(NamedKey::Space),
        Modifiers::from(ModifiersState::CONTROL),
    ));
    assert_eq!(
        host.shortcut_config().bindings[&ShortcutAction::Search],
        KeyChord::parse("Ctrl+j").unwrap()
    );
    host.set_shortcut_config(ShortcutConfig::default()).unwrap();
    assert!(host.owns_configured_chord(
        &Key::Character(" ".into()),
        Modifiers::from(ModifiersState::CONTROL),
    ));
    drop(host);
}
