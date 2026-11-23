//! A person's settings follow them between their devices: what they choose on one is on the
//! other, also when both change something at the same moment, and what a machine keeps of its
//! own stays on it. Two real sessions of one person, each changing its settings through the
//! settings' own published setters (what Settings and Quick Settings run), and each read back
//! as its session reads them, and a step the person took on one is taken back on both.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live until their \
              sessions end"
)]

use serde_json::{Value, json};
use settings_store::{appearance, power};

use crate::common::people::{
    Device, at, set_setting, setting, setting_section, two_devices_of_one_person, waited,
};

/// What `device`'s session reads of `key`.
fn read<T: serde::de::DeserializeOwned + serde::Serialize>(
    device: &Device,
    key: &settings_store::Key<T>,
) -> T {
    let Value::Object(document) = device.settings() else {
        panic!("{} answers its settings as an object", device.label)
    };
    key.get_in(&settings_store::HostDocument::new(document, |_| Ok(())))
}

#[test]
fn a_persons_settings_follow_them_and_a_machines_stay_on_it() {
    let (site, _ada, laptop, phone) = two_devices_of_one_person();
    let devices = [&laptop, &phone];

    // What the person chooses on one device is on the other; how soon this machine locks is its
    // own.
    set_setting(&laptop, "appearance", "mode", &json!("dark"));
    set_setting(&laptop, "power", "lock_minutes", &json!(1));
    waited(
        "the laptop's dark mode is on the phone",
        &site,
        &devices,
        || read(&phone, &appearance::MODE).as_deref() == Some("dark"),
    );
    assert_eq!(read(&laptop, &power::LOCK_MINUTES), 1);
    assert_eq!(
        read(&phone, &power::LOCK_MINUTES),
        power::DEFAULT_LOCK_MINUTES,
        "how soon the laptop locks stays on the laptop"
    );

    // Both devices change a setting at the same moment, neither having heard of the other's:
    // both changes are on both, and what was there before stays.
    laptop.act(
        &setting(&laptop, "appearance", "high_contrast"),
        &json!({"value": true}),
    );
    phone.act(
        &setting(&phone, "appearance", "translucency"),
        &json!({"value": 0.25}),
    );
    waited("both changes are on both devices", &site, &devices, || {
        devices.iter().all(|device| {
            read(device, &appearance::HIGH_CONTRAST)
                && read(device, &appearance::TRANSLUCENCY)
                    .is_some_and(|value| (value - 0.25).abs() < f32::EPSILON)
        })
    });
    for device in devices {
        assert_eq!(
            read(device, &appearance::MODE).as_deref(),
            Some("dark"),
            "the earlier choice stays on both"
        );
    }

    // The person takes back their last step, which they took on the laptop: the earlier choice
    // is back on both devices, and what they set before it stays.
    set_setting(&laptop, "appearance", "mode", &json!("light"));
    let undone = laptop.act_wait(
        &at(
            tilo_actions::remote::settings_resources::PROVIDER,
            &setting_section(&laptop, "appearance"),
            "undo",
        ),
        &json!({}),
    );
    assert_eq!(undone["update"]["state"], json!("succeeded"), "{undone}");
    waited(
        "the laptop's undo is on both devices",
        &site,
        &devices,
        || {
            devices.iter().all(|device| {
                read(device, &appearance::MODE).as_deref() == Some("dark")
                    && read(device, &appearance::HIGH_CONTRAST)
            })
        },
    );
}
