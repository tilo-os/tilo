//! The person's input choices as Settings sets them: what Caps Lock and the compose key do as the
//! XKB options the session applies to the keyboard, the choices a keyboard does not have, and the
//! options a keyboard keeps when its layouts change.

use std::collections::BTreeMap;

use settings_store::keyboard_sources::{KeyboardProfile, SourceOptions};

use super::prefs::{InputDevicePrefs, InputPrefs};

/// The options the person's choices put on the keyboard.
fn options(input: &InputPrefs) -> Vec<String> {
    input.rich_input(&InputDevicePrefs::default()).xkb_options()
}

/// Each choice reaches the keyboard as the XKB option it names, and Compose on Caps Lock, the
/// more specific choice, gives Caps Lock that instead.
#[test]
fn caps_lock_and_compose_reach_the_keyboard_as_xkb_options() {
    let mut input = InputPrefs::default();
    assert!(options(&input).is_empty(), "a new person changes nothing");

    input
        .set_caps_lock("escape".to_owned())
        .expect("Caps Lock can be Escape");
    assert_eq!(options(&input), ["caps:escape"]);

    input
        .set_compose_key("ralt".to_owned())
        .expect("Right Alt can be the compose key");
    assert_eq!(options(&input), ["caps:escape", "compose:ralt"]);

    input
        .set_compose_key("caps".to_owned())
        .expect("Caps Lock can be the compose key");
    assert_eq!(
        options(&input),
        ["compose:caps"],
        "Caps Lock composes rather than escaping"
    );

    input
        .set_caps_lock("caps".to_owned())
        .expect("Caps Lock can be itself");
    input
        .set_compose_key("disabled".to_owned())
        .expect("the compose key can be off");
    assert!(
        options(&input).is_empty(),
        "both choices are back to the key"
    );
}

/// A choice a keyboard does not have is refused, and nothing changes.
#[test]
fn a_choice_the_keyboard_does_not_have_is_refused() {
    let mut input = InputPrefs::default();
    assert!(input.set_caps_lock("hyper".to_owned()).is_err());
    assert!(input.set_compose_key("../evil".to_owned()).is_err());
    assert_eq!(input.caps_lock, None);
    assert_eq!(input.compose_key, None);
    assert!(options(&input).is_empty());
}

/// One keyboard's own layouts, with Caps Lock made Escape for the layout it types.
fn a_keyboard_with_its_own_options() -> (InputDevicePrefs, KeyboardProfile) {
    let mut device = InputDevicePrefs::default();
    let profile = KeyboardProfile {
        sources: vec!["de".into(), "fr".into(), "us".into()],
        active: "fr".into(),
        options: BTreeMap::from([(
            "fr".to_owned(),
            SourceOptions {
                caps_lock: Some("escape".into()),
                customised: true,
                ..SourceOptions::default()
            },
        )]),
    };
    device
        .set_keyboard_layouts("kbd-a".to_owned(), profile.clone())
        .expect("the keyboard's layouts are given");
    (device, profile)
}

/// Changing a keyboard's list of layouts keeps the options it holds for the layouts it keeps:
/// adding a layout or switching to another one never changes what Caps Lock or the compose key
/// does there.
#[test]
fn a_keyboards_layouts_keep_the_options_of_the_layouts_they_keep() {
    let (mut device, _) = a_keyboard_with_its_own_options();
    device
        .set_keyboard_sources(
            "kbd-a".to_owned(),
            vec!["de".into(), "fr".into(), "us".into(), "gb".into()],
            "us".into(),
        )
        .expect("another layout joins the keyboard");
    let held = device
        .keyboard_assignments
        .get("kbd-a")
        .expect("its profile");
    assert_eq!(
        held.sources,
        ["de", "fr", "us", "gb"],
        "the layout joins last"
    );
    assert_eq!(held.active, "us", "it types with the one chosen");
    assert_eq!(
        held.options
            .get("fr")
            .and_then(|options| options.caps_lock.as_deref()),
        Some("escape"),
        "French keeps what its Caps Lock did"
    );
}

/// A layout taken out of a keyboard's list takes its options with it, and a keyboard that had none
/// of its own starts from the shared layouts' options.
#[test]
fn a_removed_layout_takes_its_options_and_a_new_one_starts_from_the_shared() {
    let (mut device, _) = a_keyboard_with_its_own_options();
    let mut shared = KeyboardProfile {
        sources: vec!["de".into(), "us".into()],
        active: "de".into(),
        options: BTreeMap::new(),
    };
    shared.options.insert(
        "us".to_owned(),
        SourceOptions {
            compose_key: Some("ralt".into()),
            customised: true,
            ..SourceOptions::default()
        },
    );
    device
        .set_keyboard_layouts("all".to_owned(), shared)
        .expect("the shared layouts are given");

    device
        .set_keyboard_sources(
            "kbd-b".to_owned(),
            vec!["de".into(), "us".into()],
            "de".into(),
        )
        .expect("a keyboard takes the shared layouts as its own");
    let fresh = device
        .keyboard_assignments
        .get("kbd-b")
        .expect("its profile");
    assert_eq!(
        fresh
            .options
            .get("us")
            .and_then(|options| options.compose_key.as_deref()),
        Some("ralt"),
        "it starts from what the shared layouts do"
    );

    device
        .set_keyboard_sources(
            "kbd-a".to_owned(),
            vec!["de".into(), "us".into()],
            "de".into(),
        )
        .expect("a layout leaves the keyboard");
    let held = device
        .keyboard_assignments
        .get("kbd-a")
        .expect("its profile");
    assert!(
        held.options.is_empty(),
        "French is gone, and its options with it"
    );
}

/// A keyboard that is not one, or a list without the layout typed with, is refused.
#[test]
fn a_layout_list_the_keyboard_cannot_type_is_refused() {
    let mut device = InputDevicePrefs::default();
    assert!(
        device
            .set_keyboard_sources("all".to_owned(), vec!["de".into()], "de".into())
            .is_err(),
        "the shared layouts are not a keyboard's own"
    );
    assert!(
        device
            .set_keyboard_sources("kbd-a".to_owned(), vec!["de".into()], "fr".into())
            .is_err(),
        "it cannot type with a layout it does not have"
    );
    assert!(device.keyboard_assignments.is_empty());
}

/// The pressed shape and the level chosen are what the drawing inspects, and choosing another
/// model starts the inspection over; a level no keyboard has is refused.
#[test]
fn the_pressed_shape_and_the_level_are_what_is_inspected() {
    let mut device = InputDevicePrefs::default();
    device
        .inspect_keyboard(4, 2, true)
        .expect("a pressed shape at a level is inspected");
    assert_eq!(device.keyboard_inspected_shape, 4);
    assert_eq!(device.keyboard_preview_level, 2);
    assert!(device.keyboard_preview_extra_levels);

    device
        .set_keyboard_model("jp106".to_owned())
        .expect("another model is chosen");
    assert_eq!(
        device.keyboard_inspected_shape, 0,
        "another model is another drawing"
    );
    assert_eq!(device.keyboard_preview_level, 0);
    assert!(!device.keyboard_preview_extra_levels);

    assert!(
        device.inspect_keyboard(0, 32, false).is_err(),
        "no keyboard has that many levels"
    );
}
