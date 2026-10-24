use super::{PowerKeyEffect, PriorityRoute, power_key_effect, priority_route, vt_switch_chord};
use crate::shell::media_keys::MediaKey;
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, ModifiersState, NamedKey};

fn key(named: NamedKey, state: ElementState) -> WindowEvent {
    ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
        Key::Named(named),
        None,
        state,
    ))
}

fn mods(state: ModifiersState) -> Modifiers {
    Modifiers::from(state)
}

#[test]
fn ctrl_alt_f_press_alone_is_the_vt_switch_chord() {
    // Phase `ctrl_alt_f_maps_to_the_matching_vt`.
    {
        let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F1, ElementState::Pressed), m),
            Some(1)
        );
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F3, ElementState::Pressed), m),
            Some(3),
            "Ctrl+Alt+F3 is the one the seat diagnosis tells people to use"
        );
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F12, ElementState::Pressed), m),
            Some(12)
        );
    }
    // Phase `either_modifier_alone_is_not_the_chord`.
    // Alt+F4 closes a window on every desktop; claiming it as a VT switch would be a spectacular regression.
    assert_eq!(
        vt_switch_chord(
            &key(NamedKey::F4, ElementState::Pressed),
            mods(ModifiersState::ALT)
        ),
        None
    );
    assert_eq!(
        vt_switch_chord(
            &key(NamedKey::F4, ElementState::Pressed),
            mods(ModifiersState::CONTROL)
        ),
        None
    );
    assert_eq!(
        vt_switch_chord(
            &key(NamedKey::F4, ElementState::Pressed),
            mods(ModifiersState::empty())
        ),
        None
    );
    // Phase `only_the_press_switches`.
    {
        // Acting on the release too would switch twice per chord.
        let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
        assert_eq!(
            vt_switch_chord(&key(NamedKey::F2, ElementState::Released), m),
            None
        );
    }
    // Phase `other_keys_with_the_modifiers_held_are_not_vt_switches`.
    {
        let m = mods(ModifiersState::CONTROL | ModifiersState::ALT);
        assert_eq!(
            vt_switch_chord(&key(NamedKey::Delete, ElementState::Pressed), m),
            None
        );
        assert_eq!(
            vt_switch_chord(&key(NamedKey::Tab, ElementState::Pressed), m),
            None
        );
    }
}

fn character(c: &str) -> WindowEvent {
    let mut event = key(NamedKey::F1, ElementState::Pressed);
    if let WindowEvent::KeyboardInput { event, .. } = &mut event {
        event.logical_key = Key::Character(c.into());
    }
    event
}

fn route(event: &WindowEvent, state: ModifiersState, locked: bool) -> PriorityRoute {
    priority_route(
        event,
        mods(state),
        locked,
        &settings_store::hardware::ShortcutConfig::default(),
    )
}

#[test]
fn media_keys_act_while_the_lock_captures_everything_else() {
    let none = ModifiersState::empty();
    for (named, media) in [
        (NamedKey::AudioVolumeUp, MediaKey::VolumeUp),
        (NamedKey::AudioVolumeDown, MediaKey::VolumeDown),
        (NamedKey::AudioVolumeMute, MediaKey::VolumeMute),
        (NamedKey::BrightnessUp, MediaKey::BrightnessUp),
        (NamedKey::BrightnessDown, MediaKey::BrightnessDown),
        (NamedKey::MediaPlayPause, MediaKey::PlayPause),
    ] {
        let press = key(named, ElementState::Pressed);
        assert_eq!(route(&press, none, true), PriorityRoute::MediaKey(media));
        assert_eq!(route(&press, none, false), PriorityRoute::MediaKey(media));
        assert_eq!(
            route(&key(named, ElementState::Released), none, true),
            PriorityRoute::LockScreen,
            "a media key release is inert and stays with the lock"
        );
    }

    for captured in [NamedKey::LaunchApplication2, NamedKey::Standby] {
        assert_eq!(
            route(&key(captured, ElementState::Pressed), none, true),
            PriorityRoute::LockScreen,
            "{captured:?} opens UI or suspends, so the lock keeps it"
        );
        assert!(matches!(
            route(&key(captured, ElementState::Pressed), none, false),
            PriorityRoute::MediaKey(_)
        ));
    }

    assert_eq!(
        route(&character("a"), none, true),
        PriorityRoute::LockScreen
    );
    assert_eq!(
        route(&character("l"), ModifiersState::SUPER, true),
        PriorityRoute::LockScreen,
        "the lock chord is inert while already locked"
    );
    assert_eq!(
        route(
            &key(NamedKey::F2, ElementState::Pressed),
            ModifiersState::CONTROL | ModifiersState::ALT,
            true
        ),
        PriorityRoute::LockScreen,
        "a VT switch cannot bypass the lock"
    );
}

#[test]
fn unlocked_priority_order_is_power_vt_panic_lock_media_then_clients() {
    let none = ModifiersState::empty();
    assert_eq!(
        route(&key(NamedKey::Power, ElementState::Pressed), none, false),
        PriorityRoute::PowerKey
    );
    assert_eq!(
        route(
            &key(NamedKey::F3, ElementState::Pressed),
            ModifiersState::CONTROL | ModifiersState::ALT,
            false
        ),
        PriorityRoute::VtSwitch(3)
    );
    assert_eq!(
        route(
            &key(NamedKey::Escape, ElementState::Pressed),
            ModifiersState::SUPER,
            false
        ),
        PriorityRoute::RemoteInputKillSwitch
    );
    assert_eq!(
        route(&character("l"), ModifiersState::SUPER, false),
        PriorityRoute::LockSession
    );
    assert_eq!(
        route(
            &key(NamedKey::NumLock, ElementState::Pressed),
            ModifiersState::SHIFT | ModifiersState::ALT,
            false
        ),
        PriorityRoute::ToggleMouseKeys
    );
    assert_eq!(
        route(&key(NamedKey::NumLock, ElementState::Pressed), none, false),
        PriorityRoute::Continue,
        "a bare Num Lock is the client's"
    );
    assert_eq!(
        route(
            &key(NamedKey::NumLock, ElementState::Pressed),
            ModifiersState::SHIFT | ModifiersState::ALT,
            true
        ),
        PriorityRoute::LockScreen,
        "the lock keeps the chord"
    );
    assert_eq!(route(&character("a"), none, false), PriorityRoute::Continue);
}

#[test]
fn the_power_key_reaches_its_handler_while_locked() {
    let none = ModifiersState::empty();
    assert_eq!(
        route(&key(NamedKey::Power, ElementState::Pressed), none, true),
        PriorityRoute::PowerKey,
        "the lock screen would drop it"
    );
    assert_eq!(
        route(&key(NamedKey::Power, ElementState::Released), none, true),
        PriorityRoute::LockScreen,
        "only the press acts"
    );
}

#[test]
fn every_power_key_press_does_something_visible() {
    use tilo_power_mgmt::PowerAction;
    assert_eq!(
        power_key_effect(true, true, true, Some(PowerAction::Suspend)),
        PowerKeyEffect::RelitOutputs,
        "a display that could not be lit comes first"
    );
    assert_eq!(
        power_key_effect(false, true, true, Some(PowerAction::Suspend)),
        PowerKeyEffect::WokeDisplay,
        "a press that woke the display does not also suspend"
    );
    assert_eq!(
        power_key_effect(false, false, true, Some(PowerAction::Suspend)),
        PowerKeyEffect::Suspend
    );
    assert_eq!(
        power_key_effect(false, false, false, Some(PowerAction::Lock)),
        PowerKeyEffect::Lock
    );
    assert_eq!(
        power_key_effect(false, false, true, Some(PowerAction::Lock)),
        PowerKeyEffect::Blank,
        "locking a locked session would show nothing"
    );
    assert_eq!(
        power_key_effect(false, false, true, Some(PowerAction::Ignore)),
        PowerKeyEffect::Nothing
    );
    assert_eq!(
        power_key_effect(false, false, false, None),
        PowerKeyEffect::Nothing
    );
}
