//! A pointer setting changed in Settings reaches an attached device.
//!
//! Drives the compositor's own reload path — the live settings document and
//! its section follower, the `input` section reader and the per-device reconfiguration — against a
//! recording device instead of libinput, so it runs without a seat.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use settings_store::live::{LiveSettings, SectionFollower};
use shell::compositor::backend::drm::input_config::{
    self, ClickMethod, ConfigurableDevice, DeviceCaps, DevicePlan, InputSettings, ScrollMethod,
};

/// A clickpad as libinput describes a typical laptop touchpad, recording
/// every plan pushed onto it.
struct FakeTouchpad {
    applied: Vec<DevicePlan>,
}

impl ConfigurableDevice for FakeTouchpad {
    fn caps(&self) -> DeviceCaps {
        DeviceCaps {
            supports_tap: true,
            supports_natural_scroll: true,
            supports_accel: true,
            supports_dwt: true,
            supports_middle_emulation: true,
            is_pointer: true,
            supports_left_handed: true,
            supports_click_areas: true,
            supports_clickfinger: true,
            default_click_method: Some(ClickMethod::ButtonAreas),
            supports_two_finger_scroll: true,
            supports_edge_scroll: true,
            ..DeviceCaps::default()
        }
    }

    fn apply(&mut self, plan: &DevicePlan) {
        self.applied.push(*plan);
    }
}

fn scratch_settings() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tilo-input-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("settings.json")
}

/// Replace the document the way every settings writer does: a temp file
/// renamed into place, so the watch sees one complete write.
fn write_settings(path: &Path, body: &serde_json::Value) {
    let staged = path.with_extension("json.tmp");
    std::fs::write(&staged, serde_json::to_vec(body).unwrap()).unwrap();
    std::fs::rename(&staged, path).unwrap();
}

/// Write `body` and wait until the live document has taken it in.
fn write_and_wait(live: &LiveSettings, path: &Path, body: &serde_json::Value) {
    let seen = live.generation();
    write_settings(path, body);
    let snapshot = live.wait_for_change(seen, tilo_test_env::hang_bound());
    assert_ne!(
        snapshot.generation, seen,
        "the write must reach the live document"
    );
}

#[test]
fn changing_a_touchpad_setting_reconfigures_the_attached_device() {
    let path = scratch_settings();
    write_settings(&path, &serde_json::json!({"input": {"tap_to_click": true}}));
    let live = LiveSettings::start_at(path.clone(), || {});
    let mut follower = SectionFollower::new(&live);
    assert!(
        follower.changed_sections(&live).is_empty(),
        "the first look is a baseline"
    );

    // Device added: configured from the settings as they are.
    let mut devices = [FakeTouchpad {
        applied: Vec::new(),
    }];
    input_config::reconfigure_all(&InputSettings::load_at(&path), &mut devices);
    let added = devices[0].applied[0];
    assert_eq!(added.tap, Some(true));
    assert_eq!(added.tap_drag_lock, Some(false));
    assert_eq!(added.click_method, Some(ClickMethod::ButtonAreas));
    assert_eq!(added.scroll_method, Some(ScrollMethod::TwoFinger));

    // Settings writes new choices; the follower notices and the device follows.
    write_and_wait(
        &live,
        &path,
        &serde_json::json!({"input": {
            "tap_to_click": true,
            "tap_drag_lock": true,
            "touchpad_click_method": "clickfinger",
            "touchpad_scroll_method": "edge",
            "disable_while_typing": false,
            "touchpad_natural_scroll": false,
        }}),
    );
    assert_eq!(
        follower.changed_sections(&live),
        BTreeSet::from(["input".to_string()]),
        "the rewrite is noticed, as an input change"
    );
    input_config::reconfigure_all(&InputSettings::load_at(&path), &mut devices);
    assert_eq!(devices[0].applied.len(), 2, "reconfigured exactly once");
    let changed = devices[0].applied[1];
    assert_eq!(changed.tap_drag_lock, Some(true));
    assert_eq!(changed.click_method, Some(ClickMethod::Clickfinger));
    assert_eq!(changed.scroll_method, Some(ScrollMethod::Edge));
    assert_eq!(changed.disable_while_typing, Some(false));
    assert_eq!(changed.natural_scroll, Some(false));

    // Turning tapping off takes the drag lock with it on the same device.
    write_and_wait(
        &live,
        &path,
        &serde_json::json!({"input": {"tap_to_click": false, "tap_drag_lock": true}}),
    );
    assert_eq!(
        follower.changed_sections(&live),
        BTreeSet::from(["input".to_string()])
    );
    input_config::reconfigure_all(&InputSettings::load_at(&path), &mut devices);
    let off = devices[0].applied[2];
    assert_eq!(off.tap, Some(false));
    assert_eq!(off.tap_drag_lock, Some(false));
    // Long enough for every notification of the write and one stamp poll.
    std::thread::sleep(
        settings_store::live::POLL_INTERVAL.saturating_add(Duration::from_millis(300)),
    );
    assert!(
        follower.changed_sections(&live).is_empty(),
        "no further change, no further reconfiguration"
    );

    let _ = std::fs::remove_file(&path);
}
