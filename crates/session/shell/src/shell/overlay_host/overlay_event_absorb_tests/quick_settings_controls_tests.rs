//! Quick Settings end to end: every root control, pressed with the pointer
//! through the host's real event routing, reaches its backend or its
//! destination; and a window too short for the page scrolls it instead of
//! squeezing its tiles.

use super::*;
use crate::shell::overlay_host::qs_device_state_tests::Recorder;
use crate::shell::quick_settings::QsLevel;
use settings_store::hardware::{QuickSettingsConfig, QuickSettingsControl};

type Launches = Rc<RefCell<Vec<(String, String)>>>;

/// A host with every root control present, writes recorded instead of
/// reaching the machine, app launches recorded instead of spawned, opened
/// the way the top bar opens it.
fn controls_harness(vw: f64, vh: f64) -> (AbsorbHarness, Recorder, Launches) {
    // Every harness starts from the same stored preferences, whatever an
    // earlier press in this test wrote.
    settings_store::hardware::NIGHT_LIGHT.set(false).unwrap();
    settings_store::hardware::ROTATION_LOCK.set(false).unwrap();
    let actions = Recorder::default();
    let mut h = build_harness_sized(vw, vh);
    h.host = h
        .host
        .with_qs_actions(Box::new(actions.clone()))
        .with_quick_settings_config(QuickSettingsConfig {
            controls: vec![
                QuickSettingsControl::Wifi,
                QuickSettingsControl::Bluetooth,
                QuickSettingsControl::Vpn,
                QuickSettingsControl::NightLight,
                QuickSettingsControl::RotationLock,
                QuickSettingsControl::AirplaneMode,
                QuickSettingsControl::DoNotDisturb,
            ],
        });
    let launches: Launches = Rc::default();
    let recorded = Rc::clone(&launches);
    h.host.app_launcher = Rc::new(move |_, package_id, display_name| {
        recorded
            .borrow_mut()
            .push((package_id.to_owned(), display_name.to_owned()));
    });
    {
        let mut st = h.chrome.borrow_mut();
        let qs = &mut st.quick_settings;
        qs.brightness_available = true;
        qs.brightness = 0.2;
        qs.volume_available = true;
        qs.volume = 0.2;
        qs.radios.wifi_available = true;
        qs.wifi = true;
        qs.bluetooth_available = true;
        qs.bluetooth = false;
        qs.rotation_available = true;
        qs.vpns.push(os_actions::VpnConnectionInfo {
            name: "Office".into(),
            uuid: "office-vpn".into(),
            active: false,
            kind: "wireguard".into(),
        });
        st.set_dnd(false);
        st.toggle_quick_settings();
    }
    h.host.force_quick_settings_settled();
    settle(&mut h, vw, vh);
    (h, actions, launches)
}

fn target(h: &AbsorbHarness, name: &str) -> Option<Rect> {
    h.host
        .quick_settings_semantic_rects()
        .into_iter()
        .find_map(|(id, rect)| (id == name).then_some(rect))
}

/// The control's on-screen target, wheeling the page down over the popover
/// until it is shown, as a person would.
fn reveal(h: &mut AbsorbHarness, name: &str, vw: f64, vh: f64) -> Rect {
    for _ in 0..60 {
        if let Some(rect) = target(h, name) {
            return rect;
        }
        let panel = h.host.quick_settings_rect();
        pump(h, &cursor_moved_at(panel.center()));
        pump(h, &ui::synthetic_input::mouse_wheel(0.0, -1.0));
        settle(h, vw, vh);
    }
    panic!(
        "{name} is never shown at {vw}x{vh}: {:?}",
        h.host.quick_settings_semantic_rects()
    );
}

fn tap(h: &mut AbsorbHarness, point: Point, vw: f64, vh: f64) {
    pump(h, &cursor_moved_at(point));
    assert!(
        pump(h, &left_button_pressed()),
        "the popover owns the press"
    );
    pump(h, &left_button_released());
    settle(h, vw, vh);
}

fn level(h: &AbsorbHarness) -> QsLevel {
    h.chrome.borrow().quick_settings.current_level().clone()
}

fn wrote(actions: &Recorder, prefix: &str) -> bool {
    actions.calls().iter().any(|call| call.starts_with(prefix))
}

/// What pressing one control must have done.
fn assert_effect(name: &str, h: &AbsorbHarness, actions: &Recorder, launches: &Launches) {
    let st = h.chrome.borrow();
    match name {
        "quick_settings_settings" => {
            assert_eq!(
                launches.borrow().as_slice(),
                &[("so.tilo.settings".to_owned(), "Settings".to_owned())],
                "the Settings tile opens personal Settings through the app launcher"
            );
            assert!(!st.overlays.quick_settings, "and gets out of its way");
            assert!(!st.overlays.ws_settings);
        }
        "quick_settings_privacy" => {
            assert!(st.overlays.ws_settings);
            assert_eq!(st.ws_settings_tab, WsTab::Permissions);
        }
        "quick_settings_history" => {
            assert_eq!(st.quick_settings.current_level(), &QsLevel::Notifications);
        }
        "quick_settings_power" => {
            drop(st);
            assert!(h.host.layers.power_menu.is_open());
        }
        "quick_settings_edit" => {
            drop(st);
            assert!(h.host.qs_root.edit_mode());
        }
        "quick_settings_brightness" => {
            assert!(wrote(actions, "brightness="), "{:?}", actions.calls())
        }
        "quick_settings_volume" => assert!(wrote(actions, "volume="), "{:?}", actions.calls()),
        "quick_settings_network" => assert!(wrote(actions, "wifi=false"), "{:?}", actions.calls()),
        "quick_settings_network_details" => {
            drop(st);
            assert_eq!(level(h), QsLevel::Wifi);
            assert!(actions.calls().is_empty(), "opening details writes nothing");
        }
        "quick_settings_bluetooth" => {
            assert!(wrote(actions, "bluetooth=true"), "{:?}", actions.calls());
        }
        "quick_settings_bluetooth_details" => {
            drop(st);
            assert_eq!(level(h), QsLevel::Bluetooth);
            assert!(actions.calls().is_empty(), "opening details writes nothing");
        }
        "quick_settings_vpn" => assert_eq!(st.quick_settings.current_level(), &QsLevel::Vpn),
        "quick_settings_airplane_mode" => {
            assert!(wrote(actions, "airplane=true"), "{:?}", actions.calls());
        }
        "quick_settings_night_light" => {
            assert!(st.quick_settings.night_light);
            assert!(
                settings_store::hardware::NIGHT_LIGHT.get(),
                "the shared preference is written"
            );
        }
        "quick_settings_rotation_lock" => {
            assert!(st.quick_settings.rotation_lock);
            assert!(
                settings_store::hardware::ROTATION_LOCK.get(),
                "the compositor's lock is written"
            );
        }
        "quick_settings_do_not_disturb" => assert!(st.dnd()),
        "quick_settings_devices" => {
            assert_eq!(st.quick_settings.current_level(), &QsLevel::Devices)
        }
        "quick_settings_clipboard" => {
            assert_eq!(st.quick_settings.current_level(), &QsLevel::Clipboard);
        }
        "quick_settings_power_profile" => {
            assert!(wrote(actions, "power="), "{:?}", actions.calls())
        }
        other => panic!("no expectation for {other}"),
    }
}

const CONTROLS: [&str; 19] = [
    "quick_settings_settings",
    "quick_settings_privacy",
    "quick_settings_history",
    "quick_settings_power",
    "quick_settings_edit",
    "quick_settings_brightness",
    "quick_settings_volume",
    "quick_settings_network",
    "quick_settings_network_details",
    "quick_settings_bluetooth",
    "quick_settings_bluetooth_details",
    "quick_settings_vpn",
    "quick_settings_airplane_mode",
    "quick_settings_night_light",
    "quick_settings_rotation_lock",
    "quick_settings_do_not_disturb",
    "quick_settings_devices",
    "quick_settings_clipboard",
    "quick_settings_power_profile",
];

#[test]
#[serial_test::serial]
fn every_quick_settings_control_reaches_its_backend_or_destination_by_pointer() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    // The Yoga's logical desktop, and a short strip where the page scrolls.
    for (vw, vh) in [(1472.0, 920.0), (640.0, 400.0)] {
        for name in CONTROLS {
            let (mut h, actions, launches) = controls_harness(vw, vh);
            let rect = reveal(&mut h, name, vw, vh);
            tap(&mut h, rect.center(), vw, vh);
            assert_effect(name, &h, &actions, &launches);
        }
    }
}

/// Tiles at a short window's popover keep the height they have where the
/// whole page fits, and the wheel brings the lower controls into view,
/// where they still work.
#[test]
#[serial_test::serial]
fn quick_settings_keeps_natural_tiles_and_scrolls_in_a_640_by_400_window() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let (tall, _, _) = controls_harness(640.0, 1600.0);
    assert!(
        !tall.host.qs_root.scrolls(),
        "a tall window shows the whole page"
    );
    let natural: StdHashMap<String, f64> = tall
        .host
        .quick_settings_semantic_rects()
        .into_iter()
        .map(|(name, rect)| (name, rect.height()))
        .collect();

    let (vw, vh) = (640.0, 400.0);
    let (mut h, actions, _) = controls_harness(vw, vh);
    let window = Rect::new(0.0, 0.0, vw, vh);
    let panel = h.host.quick_settings_rect();
    assert!(
        panel.x0 >= window.x0
            && panel.x1 <= window.x1
            && panel.y0 >= window.y0
            && panel.y1 <= window.y1,
        "the popover {panel:?} stays inside {window:?}"
    );
    assert!(
        h.host.qs_root.scrolls(),
        "the page is taller than a 400px window"
    );
    let natural_size = |h: &AbsorbHarness| {
        for (name, rect) in h.host.quick_settings_semantic_rects() {
            // Rects the popover edge cuts are partly scrolled away.
            if rect.y0 <= panel.y0 + 0.5 || rect.y1 >= panel.y1 - 0.5 {
                continue;
            }
            let want = natural[&name];
            assert!(
                (rect.height() - want).abs() < 0.01,
                "{name} is {} tall in a short window, {want} where it fits",
                rect.height()
            );
        }
    };
    natural_size(&h);
    assert!(target(&h, "quick_settings_settings").is_some());
    assert!(
        target(&h, "quick_settings_power_profile").is_none(),
        "the last row starts out of view"
    );

    let profile = reveal(&mut h, "quick_settings_power_profile", vw, vh);
    assert!(h.host.qs_root.scroll_offset() > 0.0);
    assert!(
        target(&h, "quick_settings_settings").is_none(),
        "the top tiles scrolled away rather than shrinking"
    );
    natural_size(&h);
    tap(&mut h, profile.center(), vw, vh);
    assert!(
        wrote(&actions, "power="),
        "a control revealed by scrolling still works: {:?}",
        actions.calls()
    );
}
