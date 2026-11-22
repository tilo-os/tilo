//! A person's settings follow them between their devices: what they choose on one is on the
//! other, also when both change something at the same moment, and what a machine keeps of its
//! own stays on it. Two real sessions of one person, each changing its settings through the
//! settings' own published `field-edit` (what Settings and Quick Settings run), and each read
//! back as any app beside the session reads them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use settings_store::{appearance, power};

use crate::common::people::{Device, two_devices_of_one_person, waited};

/// The settings `device` 's session holds, as an app beside it follows them.
fn settings_of(device: &Device) -> Arc<settings_store::Store> {
    let home = device.directory();
    settings_store::Store::open_device_in(&home.join("data"), &home.join("config/tilo"))
        .unwrap_or_else(|error| panic!("{}'s settings: {error}", device.label))
}

/// The `field-edit` of the settings section `section` , as `device` 's session publishes it.
fn field_edit(device: &Device, section: &str) -> (String, String) {
    let resource = format!("settings:settings.{section}");
    let listed = device.ask(&json!({"op": "actions_enumerate", "query": section}));
    let row = listed["answer"]["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["resource"] == json!(resource) && row["operation"] == json!("field-edit"))
        .unwrap_or_else(|| {
            panic!(
                "{} offers no field-edit of {resource}: {listed}",
                device.label
            )
        });
    (
        row["provider"].as_str().expect("a provider").to_owned(),
        row["id"].as_str().expect("an action's id").to_owned(),
    )
}

/// Start setting `field` of `section` to `value` on `device` , as the person does in Settings:
/// the invocation to follow with [`settled`].
fn start_setting(device: &Device, section: &str, field: &str, value: &Value) -> u64 {
    let (provider, id) = field_edit(device, section);
    let started = device.ask(&json!({
        "op": "actions_invoke",
        "provider": provider,
        "id": id,
        "args": {"edit": {"path": [{"field": field}], "op": "set", "value": value}},
        "confirmed": true,
    }));
    started["answer"]["receipt"]["broker_invocation_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("{} sets {section}.{field}: {started}", device.label))
}

/// Follow `invocation` on `device` until it settles, and fail unless it completed.
fn settled(device: &Device, invocation: u64) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let answer = device.ask(&json!({
            "op": "actions_status",
            "invocation_id": invocation,
            "wait_ms": 30_000,
        }));
        match answer["answer"]["receipt"]["update"]["state"].as_str() {
            Some("accepted" | "progress") if Instant::now() < deadline => {}
            Some("completed") => return,
            _ => panic!("{}'s setting did not complete: {answer}", device.label),
        }
    }
}

/// Set `field` of `section` to `value` on `device` .
fn set(device: &Device, section: &str, field: &str, value: &Value) {
    settled(device, start_setting(device, section, field, value));
}

#[test]
fn a_persons_settings_follow_them_and_a_machines_stay_on_it() {
    let (site, _ada, laptop, phone) = two_devices_of_one_person();
    let (on_laptop, on_phone) = (settings_of(&laptop), settings_of(&phone));
    let devices = [&laptop, &phone];

    // What the person chooses on one device is on the other; how soon this machine locks is its
    // own.
    set(&laptop, "appearance", "mode", &json!("dark"));
    set(&laptop, "power", "lock_minutes", &json!(1));
    waited(
        "the laptop's dark mode is on the phone",
        &site,
        &devices,
        || appearance::MODE.get_in(on_phone.as_ref()).as_deref() == Some("dark"),
    );
    assert_eq!(power::LOCK_MINUTES.get_in(on_laptop.as_ref()), 1);
    assert_eq!(
        power::LOCK_MINUTES.get_in(on_phone.as_ref()),
        power::DEFAULT_LOCK_MINUTES,
        "how soon the laptop locks stays on the laptop"
    );

    // Both devices change a setting at the same moment, neither having heard of the other's:
    // both changes are on both, and what was there before stays.
    let on_the_laptop = start_setting(&laptop, "appearance", "high_contrast", &json!(true));
    let on_the_phone = start_setting(&phone, "appearance", "translucency", &json!(0.25));
    settled(&laptop, on_the_laptop);
    settled(&phone, on_the_phone);
    waited("both changes are on both devices", &site, &devices, || {
        [&on_laptop, &on_phone].iter().all(|settings| {
            appearance::HIGH_CONTRAST.get_in(settings.as_ref())
                && appearance::TRANSLUCENCY
                    .get_in(settings.as_ref())
                    .is_some_and(|value| (value - 0.25).abs() < f32::EPSILON)
        })
    });
    for settings in [&on_laptop, &on_phone] {
        assert_eq!(
            appearance::MODE.get_in(settings.as_ref()).as_deref(),
            Some("dark"),
            "the earlier choice stays on both"
        );
    }
}
