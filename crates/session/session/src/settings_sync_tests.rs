#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use super::*;

#[test]
fn only_the_persons_sections_are_offered() {
    let document: Map<String, Value> = serde_json::from_value(serde_json::json!({
        "appearance": {"mode": "dark", "wallpaper": "dunes"},
        "app_personal": {"so.tilo.monitor": {"confirm_kill": true}},
        "app_device_local": {"so.tilo.monitor": {"device": 1}},
        "power": {"lock_minutes": 1},
    }))
    .unwrap();
    let offered = personal_leaves(&document);
    assert_eq!(
        offered.keys().collect::<Vec<_>>(),
        [
            "tilo.app_personal.so.tilo.monitor",
            "tilo.appearance.mode",
            "tilo.appearance.wallpaper",
        ],
        "this device's sections stay on it"
    );
    assert_eq!(
        leaf_of(&document, "tilo.app_personal.so.tilo.monitor"),
        offered.get("tilo.app_personal.so.tilo.monitor"),
        "an app's entry is one value, as it travels"
    );
}
