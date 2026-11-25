//! A person's settings follow them between their devices: what they choose on one is on the
//! other, also when both change something at the same moment, and what a machine keeps of its
//! own stays on it. Two real sessions of one person, each changing its settings through the
//! settings' own published setters (what Settings and Quick Settings run), and each read back
//! as its session reads them, and a step the person took on one is taken back on both. Their
//! shell's choices follow them too: what they set of their shell on one device is the shell their
//! other device draws.

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
    Device, at, set_setting, setting, setting_section, two_devices_of_one_person,
    two_devices_of_one_person_running_apps, waited,
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

/// The shell's field `field` as `device`'s shell draws it now.
fn shell_setting(device: &Device, field: &str) -> Value {
    device.ask(&json!({"op": "shell_setting", "field": field}))["answer"].take()
}

/// Run the shell's own action `action` on `device`, as pressing its control there does.
fn shell_action(device: &Device, action: &str, arguments: &Value) {
    let answer =
        device.ask(&json!({"op": "shell_action", "action": action, "arguments": arguments}));
    assert!(
        answer.get("ok").is_some(),
        "{} runs the shell's {action}: {answer}",
        device.label
    );
}

/// The person moves their dock, turns the frames' grid off and picks the desktop style on their
/// laptop, and turns the strips' tabs off on their phone: each choice is the person's call of the
/// shell's action on their own record, so both devices draw all four. The laptop then restarts,
/// and its new session draws them again: the choices are the person's workspace's, not a copy
/// the session kept.
#[test]
fn a_persons_shell_choices_follow_them_to_their_other_device() {
    let (site, _ada, mut laptop, phone) = two_devices_of_one_person_running_apps();

    let docked = shell_setting(&laptop, "dock_edge");
    shell_action(&laptop, "dock_on", &json!(["left"]));
    shell_action(&laptop, "set_frames_snap", &json!([false]));
    shell_action(&laptop, "show_style", &json!([DESKTOP]));
    let devices = [&laptop, &phone];
    // The laptop draws its own choice once its record holds it, as the phone does.
    let mut moved = Value::Null;
    waited("the laptop draws its dock moved", &site, &devices, || {
        moved = shell_setting(&laptop, "dock_edge");
        !moved.is_null() && moved != docked
    });
    waited(
        "the laptop's dock and grid are on the phone",
        &site,
        &devices,
        || {
            shell_setting(&phone, "dock_edge") == moved
                && shell_setting(&phone, "frames_snap") == json!(false)
                && shell_setting(&phone, "style") == json!(DESKTOP)
        },
    );

    shell_action(&phone, "set_strips_tabs", &json!([false]));
    let all_four = |device: &Device| {
        shell_setting(device, "strips_tabs") == json!(false)
            && shell_setting(device, "dock_edge") == moved
            && shell_setting(device, "frames_snap") == json!(false)
            && shell_setting(device, "style") == json!(DESKTOP)
    };
    waited(
        "the phone's strip tabs are on the laptop, and the laptop's choices stay on both",
        &site,
        &devices,
        || devices.iter().all(|device| all_four(device)),
    );

    // A restart: the laptop's session and daemon end, and start again over what the device kept.
    laptop.stop();
    laptop.start();
    waited(
        "the laptop draws every choice again after it restarts",
        &site,
        &[&laptop, &phone],
        || all_four(&laptop),
    );
}

/// The desktop style the shell ships, as the style switch names it.
const DESKTOP: &str = "so.tilo.shell:desktop";
