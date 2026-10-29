//! The panel as a person meets it, headless: what each page shows, where
//! things sit, and which effect each control invokes on the device.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::rc::Rc;

use tilo_quick_settings::host::{FixtureHost, Lent};
use tilo_quick_settings::{
    Page, QsEffect, QsSection, QuickSettings, QuickSettingsApp, RowPart, scenarios,
};
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (360, 900);

fn staged(scenario: &str) -> HeadlessApp<QuickSettingsApp> {
    HeadlessApp::<QuickSettingsApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

/// The panel on a desktop: the scenario's readings lent by a host that
/// records what it is asked to do.
fn hosted(scenario: &str) -> (HeadlessApp<QuickSettingsApp>, Rc<FixtureHost>) {
    let host = Rc::new(FixtureHost::new(scenarios::readings(Some(scenario))));
    let lent: Rc<dyn tilo_quick_settings::host::PanelHost> = host.clone();
    let mut app =
        HeadlessApp::<QuickSettingsApp>::for_person_with_lent("person", Lent(lent)).unwrap();
    for invoke in scenarios::actions(Some(scenario)) {
        app.invoke(invoke).unwrap();
    }
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, host)
}

fn target(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) -> Rect {
    let targets = app.harness().targets();
    *targets.get(name).unwrap_or_else(|| {
        let mut names: Vec<&String> = targets.keys().collect();
        names.sort();
        panic!("no target named {name}; there are {names:?}")
    })
}

/// Press `name` as a person does, and let the panel answer.
fn click(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) {
    app.harness().click_named(name).unwrap();
    app.settle();
}

/// Press a tile's chevron, at the tile's trailing end.
fn open_details(app: &mut HeadlessApp<QuickSettingsApp>, tile: &str) {
    let rect = target(app, tile);
    app.harness().click_at(rect.x1 - 8.0, rect.center().y);
    app.settle();
}

fn has(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) -> bool {
    app.harness().targets().contains_key(name)
}

#[test]
fn the_controls_page_has_the_sliders_then_the_tiles_two_to_a_row_then_the_rows_and_the_footer() {
    let mut app = staged("controls");
    let volume = target(&mut app, "qs-volume");
    let wifi = target(&mut app, "qs-tile-wifi");
    let bluetooth = target(&mut app, "qs-tile-bluetooth");
    let vpn = target(&mut app, "qs-tile-vpn");
    let battery = target(&mut app, "qs-row-battery");
    let notifications = target(&mut app, "qs-row-notifications");
    let power = target(&mut app, "qs-power");
    assert!(
        target(&mut app, "qs-brightness").y1 <= volume.y0,
        "brightness, then volume"
    );
    assert!(volume.y1 <= wifi.y0, "the sliders come first");
    assert!(
        (wifi.y0 - bluetooth.y0).abs() < 1.0 && wifi.x1 <= bluetooth.x0,
        "two tiles to a row: {wifi:?} {bluetooth:?}"
    );
    assert!(vpn.y0 >= wifi.y1, "the third tile starts the next row");
    assert!(battery.y0 >= vpn.y1);
    assert!(notifications.y0 > battery.y0);
    assert!(power.y0 >= notifications.y1, "the footer comes last");
    assert!(
        !has(&mut app, "qs-section-wifi"),
        "nothing is open when the panel opens"
    );
}

#[test]
fn a_tiles_chevron_opens_its_section_under_its_row_and_the_page_tells_the_host() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("controls");
    open_details(&mut app, "qs-tile-wifi");
    let wifi = target(&mut app, "qs-tile-wifi");
    let section = target(&mut app, "qs-section-wifi");
    let vpn = target(&mut app, "qs-tile-vpn");
    assert!(section.y0 >= wifi.y1 && vpn.y0 >= section.y1, "{section:?}");
    assert_eq!(
        host.showing().map(|showing| showing.section),
        Some(Some(QsSection::Wifi)),
        "the host reads the networks while their section is open"
    );
    // The networks: the one in use first, then the known ones.
    let home = target(&mut app, "qs-network-Home");
    let office = target(&mut app, "qs-network-Office");
    assert!(home.y1 <= office.y0);
    open_details(&mut app, "qs-tile-wifi");
    assert!(
        !has(&mut app, "qs-section-wifi"),
        "the chevron closes it again"
    );
}

#[test]
fn a_section_opens_in_place_on_the_disclosure_motion() {
    let (mut app, _host) = hosted("controls");
    let shut = target(&mut app, "qs-tile-vpn");
    // The clock stands still where the section starts to open.
    let frozen = ui::motion::testing::frozen();
    open_details(&mut app, "qs-tile-wifi");
    let wifi = target(&mut app, "qs-tile-wifi");
    let section = target(&mut app, "qs-section-wifi");
    let vpn = target(&mut app, "qs-tile-vpn");
    assert!(
        section.y0 >= wifi.y1,
        "it unrolls from under its row: {section:?}"
    );
    assert!(
        (vpn.y0 - shut.y0).abs() < 0.5,
        "what follows moves with its edge, not to its end at once: {vpn:?}"
    );
    drop(frozen);
    let _settled = ui::motion::testing::settled();
    app.settle();
    let section = target(&mut app, "qs-section-wifi");
    assert!(
        target(&mut app, "qs-tile-vpn").y0 >= section.y1,
        "open, it takes its place in the page"
    );
}

#[test]
fn each_control_invokes_its_effect_on_the_device() {
    let (mut app, host) = hosted("controls");
    for name in [
        "qs-tile-wifi",
        "qs-tile-bluetooth",
        "qs-tile-night_light",
        "qs-volume-mute",
    ] {
        click(&mut app, name);
    }
    assert_eq!(
        host.ran(),
        [
            QsEffect::SetWifi { on: false },
            QsEffect::SetBluetooth { on: false },
            QsEffect::SetNightLight { on: true },
            QsEffect::SetMuted { muted: true },
        ]
    );
}

#[test]
fn the_effects_are_published_for_the_palette_scripts_and_agents() {
    let (mut app, host) = hosted("controls");
    app.effect(&QsEffect::SetVolume { level: 0.25 }).unwrap();
    app.effect(&QsEffect::SetPowerMode {
        mode: tilo_quick_settings::PowerProfile::Saver,
    })
    .unwrap();
    assert_eq!(
        host.ran(),
        [
            QsEffect::SetVolume { level: 0.25 },
            QsEffect::SetPowerMode {
                mode: tilo_quick_settings::PowerProfile::Saver
            },
        ]
    );
}

#[test]
fn a_protected_network_never_joined_asks_for_its_password_under_it() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("wifi");
    click(&mut app, "qs-network-Cafe guest");
    assert_eq!(
        host.ran(),
        [QsEffect::AskWifiPassword {
            network: "Cafe guest".into()
        }]
    );
    let mut app = staged("wifi-password");
    let network = target(&mut app, "qs-network-Cafe guest");
    let prompt = target(&mut app, "qs-wifi-password");
    let next = target(&mut app, "qs-network-Library");
    assert!(
        prompt.y0 >= network.y1 && next.y0 >= prompt.y1,
        "{prompt:?}"
    );
    assert!(
        has(&mut app, "qs-wifi-password-error"),
        "a wrong password says so"
    );
}

#[test]
fn typing_the_password_and_pressing_connect_joins_with_it() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("wifi-password");
    let field = app
        .harness()
        .targets()
        .into_keys()
        .find(|name| name.starts_with("qs-wifi-password-") && name.ends_with(char::is_numeric))
        .expect("the password field");
    app.harness().click_named(&field).unwrap();
    app.harness().type_text("hunter22");
    click(&mut app, "qs-wifi-password-connect");
    assert_eq!(
        host.ran(),
        [QsEffect::ConnectWifi {
            network: "Cafe guest".into(),
            password: "hunter22".into(),
        }]
    );
}

#[test]
fn opened_to_paste_the_panel_is_the_clipboard_alone_and_typing_searches_it() {
    let (mut app, host) = hosted("paste");
    assert!(!has(&mut app, "qs-volume"), "no controls while pasting");
    assert!(has(&mut app, "qs-copy-a1") && has(&mut app, "qs-copy-c3"));
    app.harness().click_named("qs-clipboard-search").unwrap();
    app.harness().type_text("grocery");
    app.settle();
    assert!(has(&mut app, "qs-copy-c3"));
    assert!(
        !has(&mut app, "qs-copy-a1"),
        "a copy that does not match goes"
    );
    click(&mut app, "qs-copy-c3");
    assert_eq!(host.ran(), [QsEffect::PasteCopy { copy: "c3".into() }]);
}

#[test]
fn a_copy_is_pinned_and_removed_from_its_own_buttons() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("clipboard");
    click(&mut app, "qs-copy-b2-pin");
    click(&mut app, "qs-copy-a1-remove");
    assert_eq!(
        host.ran(),
        [
            QsEffect::PinCopy {
                copy: "b2".into(),
                pinned: true
            },
            QsEffect::RemoveCopy { copy: "a1".into() },
        ]
    );
}

#[test]
fn a_devices_row_says_which_of_its_buttons_was_pressed() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("devices");
    click(&mut app, "qs-device-speaker-request-allow");
    click(&mut app, "qs-device-phone-control");
    assert_eq!(
        host.ran(),
        [
            QsEffect::PressDevice {
                row: "speaker-request".into(),
                part: RowPart::Allow,
            },
            QsEffect::PressDevice {
                row: "phone".into(),
                part: RowPart::Control,
            },
        ]
    );
}

#[test]
fn the_notifications_page_groups_by_app_and_says_why_banners_are_held() {
    let mut app = staged("notifications");
    assert!(has(&mut app, "qs-notifications-quiet"));
    let chat = target(&mut app, "qs-notification-1");
    let calendar = target(&mut app, "qs-notification-3");
    assert!(chat.y1 <= calendar.y0, "newest group first");
    assert!(has(&mut app, "qs-notifications-clear"));
}

#[test]
fn a_notification_nothing_can_open_is_read_in_full_and_back_returns_to_the_list() {
    let (mut app, host) = hosted("notifications");
    click(&mut app, "qs-notification-3");
    assert!(host.ran().is_empty(), "nothing opens it");
    assert!(has(&mut app, "qs-notification-body"));
    click(&mut app, "qs-back");
    assert!(has(&mut app, "qs-notification-1"));
    click(&mut app, "qs-notification-1");
    assert_eq!(host.ran(), [QsEffect::OpenNotification { notification: 1 }]);
}

#[test]
fn an_accessory_moves_only_to_a_device_that_is_online_and_not_this_one() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let (mut app, host) = hosted("accessory");
    // Offline, and this one: nothing to move to.
    let _ = app.harness().click_named("qs-accessory-to-tablet");
    app.settle();
    let _ = app.harness().click_named("qs-accessory-to-laptop");
    app.settle();
    click(&mut app, "qs-accessory-to-desk");
    assert_eq!(
        host.ran(),
        [QsEffect::HandOffBluetooth {
            address: "00:1B:66:A1:B2:C3".into(),
            device: "desk".into(),
        }]
    );
}

#[test]
fn arranging_hides_adds_and_moves_controls_but_never_wifi() {
    let (mut app, _host) = hosted("edit");
    assert!(has(&mut app, "qs-edit-wifi"));
    app.effect(&QsEffect::HideControl {
        control: "wifi".into(),
    })
    .unwrap();
    app.effect(&QsEffect::HideControl {
        control: "vpn".into(),
    })
    .unwrap();
    app.settle();
    assert!(
        has(&mut app, "qs-edit-wifi"),
        "Wi-Fi is how a desktop gets online"
    );
    assert!(!has(&mut app, "qs-edit-vpn"));
    assert!(
        has(&mut app, "qs-edit-add-vpn"),
        "a hidden control can come back"
    );
    app.effect(&QsEffect::MoveControl {
        control: "bluetooth".into(),
        to: 0,
    })
    .unwrap();
    app.settle();
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let wifi = target(&mut app, "qs-edit-wifi");
    assert!(bluetooth.y1 <= wifi.y0, "moved to the top");
    click(&mut app, "qs-edit-done");
    assert!(has(&mut app, "qs-tile-bluetooth") && !has(&mut app, "qs-tile-vpn"));
}

/// The control the keyboard is on, as assistive technology reads it.
fn focused(app: &mut HeadlessApp<QuickSettingsApp>) -> Option<ui::a11y::AccessNode> {
    app.harness().focused_access_node()
}

/// Tab until the keyboard is on the arranged row named `name`.
fn tab_to_row(app: &mut HeadlessApp<QuickSettingsApp>, name: &str) {
    for _ in 0..40 {
        app.harness().tab(false);
        if focused(app).is_some_and(|node| {
            node.role == ui::a11y::Role::ListItem && node.name.as_deref() == Some(name)
        }) {
            return;
        }
    }
    panic!("Tab never reaches the {name} row");
}

/// Press `key` with Ctrl held, and let the panel answer.
fn ctrl(app: &mut HeadlessApp<QuickSettingsApp>, key: &str) {
    app.harness().set_modifiers(&["ctrl".to_owned()]);
    app.harness().key(key);
    app.harness().set_modifiers(&[]);
    app.settle();
}

#[test]
fn ctrl_up_and_down_move_the_focused_control_and_it_keeps_the_keyboard() {
    let (mut app, host) = hosted("edit");
    tab_to_row(&mut app, "Bluetooth");
    assert!(
        focused(&mut app)
            .and_then(|node| node.description)
            .is_some_and(|description| description.contains("Control plus Arrow keys")),
        "the row says how to move it"
    );
    ctrl(&mut app, "ArrowUp");
    assert_eq!(
        host.ran(),
        [QsEffect::MoveControl {
            control: "bluetooth".into(),
            to: 0,
        }]
    );
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let wifi = target(&mut app, "qs-edit-wifi");
    assert!(bluetooth.y1 <= wifi.y0, "moved to the top");
    assert_eq!(
        focused(&mut app).and_then(|node| node.name).as_deref(),
        Some("Bluetooth"),
        "the keyboard stays on the control it moved"
    );
    ctrl(&mut app, "ArrowUp");
    assert_eq!(
        host.ran().len(),
        1,
        "the first control has nowhere up to go"
    );
    ctrl(&mut app, "ArrowDown");
    assert_eq!(
        host.ran().last(),
        Some(&QsEffect::MoveControl {
            control: "bluetooth".into(),
            to: 1,
        })
    );
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let wifi = target(&mut app, "qs-edit-wifi");
    assert!(wifi.y1 <= bluetooth.y0, "back under Wi-Fi");
    assert_eq!(
        focused(&mut app).and_then(|node| node.name).as_deref(),
        Some("Bluetooth")
    );
}

#[test]
fn a_control_dragged_part_way_waits_for_the_release_and_lands_there() {
    let (mut app, host) = hosted("edit");
    let bluetooth = target(&mut app, "qs-edit-bluetooth");
    let (x, y) = (bluetooth.x0 + 20.0, bluetooth.center().y);
    let travel = bluetooth.height() * 1.6;
    let harness = app.harness();
    harness.send(&ui::synthetic_input::cursor_moved(x, y));
    harness.send(&ui::synthetic_input::mouse_press());
    for step in 1..=4_u8 {
        harness.send(&ui::synthetic_input::cursor_moved(
            x,
            y + travel * f64::from(step) / 4.0,
        ));
    }
    app.settle();
    assert!(
        host.ran().is_empty(),
        "nothing moves while the control is held"
    );
    app.harness().send(&ui::synthetic_input::mouse_release());
    app.settle();
    assert_eq!(
        host.ran(),
        [QsEffect::MoveControl {
            control: "bluetooth".into(),
            to: 2,
        }],
        "let go past the next control, it lands after it"
    );
}

#[test]
fn back_closes_a_section_leaves_a_page_and_finishes_arranging() {
    let mut panel = QuickSettings::default();
    panel.open_section(QsSection::Bluetooth);
    panel.show_accessory("00:11".into());
    assert_eq!(panel.page, Page::Accessory);
    panel.back();
    assert_eq!(
        (panel.page, panel.section),
        (Page::Root, Some(QsSection::Bluetooth))
    );
    panel.back();
    assert_eq!(panel.section, None);
    assert!(!panel.showing().goes_back(), "now Back closes the panel");
    panel.show_notifications();
    panel.read_notification(3);
    panel.back();
    assert_eq!(panel.page, Page::Notifications);
    panel.paste();
    assert!(
        !panel.showing().goes_back(),
        "Back closes a panel opened to paste"
    );
}

#[test]
fn a_panel_taller_than_its_room_scrolls_rather_than_dropping_a_control() {
    // Where things rest: the motion on the way is its own test.
    let _settled = ui::motion::testing::settled();
    let mut app = HeadlessApp::<QuickSettingsApp>::capture_at(Some("battery"), (360, 420)).unwrap();
    assert!(has(&mut app, "qs-power"), "the footer is still on the page");
    let power = target(&mut app, "qs-power");
    assert!(
        power.y0 > 420.0,
        "below the fold, reached by scrolling: {power:?}"
    );
}

#[test]
fn every_scenario_renders() {
    for scenario in scenarios::NAMES {
        let mut app = staged(scenario);
        assert!(
            !app.harness().targets().is_empty(),
            "{scenario} shows nothing"
        );
    }
}

#[test]
fn tab_walks_the_controls_from_brightness() {
    let mut app = staged("controls");
    let d = app.harness();
    assert!(
        d.focus_order().len() > 10,
        "every control takes the keyboard"
    );
    d.tab(false);
    assert_eq!(
        d.focused_access_node()
            .and_then(|node| node.name)
            .as_deref(),
        Some("Brightness"),
        "the first Tab reaches brightness"
    );
}
