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

use serde_json::json;
use settings_store::{appearance, power};

use crate::common::people::{
    Device, set_setting, setting, setting_edit, two_devices_of_one_person, waited,
};

/// The settings `device` 's session holds, as an app beside it follows them.
fn settings_of(device: &Device) -> Arc<settings_store::Store> {
    let home = device.directory();
    settings_store::Store::open_device_in(&home.join("data"), &home.join("config/tilo"))
        .unwrap_or_else(|error| panic!("{}'s settings: {error}", device.label))
}

#[test]
fn a_persons_settings_follow_them_and_a_machines_stay_on_it() {
    let (site, _ada, laptop, phone) = two_devices_of_one_person();
    let (on_laptop, on_phone) = (settings_of(&laptop), settings_of(&phone));
    let devices = [&laptop, &phone];

    // What the person chooses on one device is on the other; how soon this machine locks is its
    // own.
    set_setting(&laptop, "appearance", "mode", &json!("dark"));
    set_setting(&laptop, "power", "lock_minutes", &json!(1));
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
    let appearance = setting("appearance");
    laptop.act(&appearance, &setting_edit("high_contrast", &json!(true)));
    phone.act(&appearance, &setting_edit("translucency", &json!(0.25)));
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
