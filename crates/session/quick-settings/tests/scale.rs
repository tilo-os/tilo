//! The panel at a `HiDPI` display's scale: every control the old panel offered
//! is there, drawn at the size a 2× panel needs, and driving each one changes
//! the setting it names.
//!
//! The panel lays out in logical pixels and the display draws them at 2×, so
//! a control's target is the same logical rect at either scale and its
//! painted size doubles. What this holds is that the controls exist, that
//! their targets are where the layout put them at 2×, and that a press at
//! each changes the device effect the control names.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::rc::Rc;

mod common;

use common::{click, drawn_on_each_other, has, hosted_at, target};
use tilo_quick_settings::host::FixtureHost;
use tilo_quick_settings::{QsEffect, QsSection, QuickSettings, QuickSettingsApp};
use ui::framework::testing::HeadlessApp;

/// The panel on a 2944×1840 2× laptop panel: its own logical width, and the
/// room under the bar that that display leaves.
const VIEWPORT: (u32, u32) = (380, 880);
/// The density the display draws at.
const SCALE: f64 = 2.0;

fn staged(scenario: &str) -> HeadlessApp<QuickSettingsApp> {
    let mut app = HeadlessApp::<QuickSettingsApp>::capture_at(Some(scenario), VIEWPORT).unwrap();
    app.harness().set_input_scale(SCALE).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    app
}

fn hosted(scenario: &str) -> (HeadlessApp<QuickSettingsApp>, Rc<FixtureHost>) {
    hosted_at(scenario, VIEWPORT, SCALE)
}

/// Whether the panel on `state` offers `control`'s tile.
fn visible(
    state: &tilo_quick_settings::QuickSettingsState,
    control: settings_store::hardware::QuickSettingsControl,
) -> bool {
    state.arrangement.controls.contains(&control)
        && tilo_quick_settings::control_available(control, state)
}

/// Every control the old panel offered is on the controls page at scale 2,
/// with a target a 2× display can be pressed on.
#[test]
fn every_control_the_old_panel_offered_is_there_at_scale_2() {
    let mut app = staged("controls");
    for name in [
        // The tiles, two to a row.
        "qs-tile-wifi",
        "qs-tile-bluetooth",
        "qs-tile-vpn",
        "qs-tile-night_light",
        "qs-tile-do_not_disturb",
        "qs-tile-airplane_mode",
        "qs-tile-power_mode",
        // Rotation lock is on a machine with an accelerometer, as the old
        // panel had it: `every_control_reaches_its_setting_at_scale_2`
        // shows a machine that has one.
        // The sliders and their controls.
        "qs-brightness-row",
        "qs-brightness",
        "qs-volume-row",
        "qs-volume",
        "qs-volume-mute",
        // The rows under them.
        "qs-row-battery",
        "qs-row-devices",
        "qs-row-clipboard",
        "qs-row-notifications",
        // The footer.
        "qs-account",
        "qs-edit",
        "qs-settings",
        "qs-lock",
        "qs-power",
    ] {
        assert!(has(&mut app, name), "{name} is missing at scale 2");
    }
}

/// A tile is a target a finger reaches at 2×: its logical height is the
/// design language's Quick Settings minimum, and its row is the panel's
/// width.
#[test]
fn a_tile_is_a_reachable_target_at_scale_2() {
    let mut app = staged("controls");
    let wifi = target(&mut app, "qs-tile-wifi");
    // Large is 40 logical pixels (docs/design-language.md, "Space and
    // density").
    assert!(
        wifi.height() >= 39.5,
        "a tile is the Large control height, not a row: {wifi:?}"
    );
    // The panel fills the window the desk gives a popover, so the app's root
    // area is the card's rect (as `layout.rs` reads it too).
    let card = target(&mut app, "BoundComponent[0]/ResourceApp[0]");
    assert!(
        (wifi.x0 - card.x0).abs() < 40.0 || (wifi.x1 - card.x1).abs() < 40.0,
        "a tile stands inside the card's inset: {wifi:?} in {card:?}"
    );
}

/// Driving the tiles at 2× runs the effects that change the setting each one
/// names: the radios, the lights, the quiet, the modes.
#[test]
fn every_tile_press_changes_the_setting_it_names_at_scale_2() {
    let (mut app, host) = hosted("controls");
    for name in [
        "qs-tile-wifi",
        "qs-tile-bluetooth",
        "qs-tile-night_light",
        "qs-tile-do_not_disturb",
        "qs-tile-airplane_mode",
    ] {
        click(&mut app, name);
    }
    assert_eq!(
        host.ran(),
        [
            QsEffect::SetWifi { on: false },
            QsEffect::SetBluetooth { on: false },
            QsEffect::SetNightLight { on: true },
            QsEffect::SetDoNotDisturb { on: true },
            QsEffect::SetAirplaneMode { on: true },
        ],
        "each tile is its own setting"
    );
}

/// Driving the sliders at 2× sets the level they name.
#[test]
fn dragging_a_slider_sets_its_level_at_scale_2() {
    let (mut app, host) = hosted("controls");
    let brightness = target(&mut app, "qs-brightness");
    // A press at the track's quarter: the slider reports its level on release.
    let quarter = (
        brightness.x0 + brightness.width() * 0.25,
        brightness.center().y,
    );
    app.harness()
        .drag_at((brightness.x1 - 4.0, brightness.center().y), quarter);
    app.settle();
    assert!(
        host.ran()
            .iter()
            .any(|effect| matches!(effect, QsEffect::SetBrightness { .. })),
        "the brightness slider set a level: {:?}",
        host.ran()
    );

    let (mut app, host) = hosted("controls");
    let volume = target(&mut app, "qs-volume");
    app.harness().drag_at(
        (volume.x0 + 4.0, volume.center().y),
        (volume.center().x, volume.center().y),
    );
    app.settle();
    assert!(
        host.ran()
            .iter()
            .any(|effect| matches!(effect, QsEffect::SetVolume { .. })),
        "the volume slider set a level: {:?}",
        host.ran()
    );
}

/// A machine that can tell which way up it is held shows the rotation lock,
/// as the old panel did: the tile is not there on one that cannot.
#[test]
fn the_rotation_lock_is_there_on_a_machine_that_can_turn() {
    use settings_store::hardware::QuickSettingsControl;
    use tilo_quick_settings::QuickSettingsState;

    let mut state = QuickSettingsState::fixture();
    assert!(
        !visible(&state, QuickSettingsControl::RotationLock),
        "a machine with no accelerometer shows no rotation lock"
    );
    state.rotation_available = true;
    assert!(
        visible(&state, QuickSettingsControl::RotationLock),
        "one that can turn does"
    );
}

/// The mute button the old volume row had still mutes, at 2×.
#[test]
fn the_volume_control_mutes_at_scale_2() {
    let (mut app, host) = hosted("controls");
    click(&mut app, "qs-volume-mute");
    assert_eq!(host.ran(), [QsEffect::SetMuted { muted: true }]);
}

/// Each section the old panel had still opens, at 2×, and shows what it
/// lists.
#[test]
fn every_section_opens_at_scale_2() {
    for (section, names) in [
        (QsSection::Wifi, vec!["qs-wired", "qs-network"]),
        (QsSection::Bluetooth, vec!["qs-accessory", "qs-bluetooth"]),
        (QsSection::Vpn, vec!["qs-vpn"]),
        (QsSection::PowerMode, vec!["qs-power-modes"]),
        (QsSection::Battery, vec!["qs-battery-charge"]),
        (QsSection::Sound, vec!["qs-output-", "qs-sound-settings"]),
        (QsSection::Devices, vec!["qs-device", "qs-devices"]),
        (QsSection::Clipboard, vec!["qs-copy", "qs-clipboard"]),
    ] {
        let (mut app, _) = hosted("controls");
        app.invoke(QuickSettings::TOGGLE_SECTION.with(section))
            .unwrap();
        app.settle();
        assert!(
            has(&mut app, "qs-back"),
            "{section:?} opened with a way back"
        );
        let targets: Vec<String> = app.harness().targets().keys().cloned().collect();
        assert!(
            targets
                .iter()
                .any(|target| names.iter().any(|name| target.starts_with(name))),
            "{section:?} lists its own rows ({names:?}): {targets:?}"
        );
        let found = drawn_on_each_other(&mut app);
        assert!(found.is_empty(), "{section:?} at scale 2: {found:#?}");
    }
}

/// The page a section replaces is the controls again, exactly where it
/// stood: Back gives the panel back at 2× as it was.
#[test]
fn back_returns_the_controls_page_at_scale_2() {
    let (mut app, _) = hosted("controls");
    let before = target(&mut app, "qs-tile-wifi");
    app.invoke(QuickSettings::TOGGLE_SECTION.with(QsSection::Wifi))
        .unwrap();
    app.settle();
    assert!(
        !has(&mut app, "qs-tile-wifi"),
        "the section replaced the page"
    );
    app.invoke(QuickSettings::BACK.invoke()).unwrap();
    app.settle();
    let after = target(&mut app, "qs-tile-wifi");
    assert_eq!(
        (before.x0, before.y0),
        (after.x0, after.y0),
        "the controls come back where they stood"
    );
}
