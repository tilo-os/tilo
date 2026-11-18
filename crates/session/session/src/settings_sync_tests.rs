#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::Arc;

use super::*;
use crate::personal::linked::{hand, store_of};

/// One of the person's devices: its own settings document, its own store, and what keeps the two
/// in step.
struct Device {
    settings: Arc<settings_store::Store>,
    _dir: tempfile::TempDir,
    store: PersonalSync,
    sync: SettingsSync,
}

impl Device {
    fn new(seed: u8, label: &str) -> Self {
        let person = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
        let dir = tempfile::tempdir().expect("a settings directory");
        Self {
            settings: settings_store::Store::open(dir.path()).expect("a settings document"),
            _dir: dir,
            store: store_of(&cert, Arc::new(key)),
            sync: SettingsSync::new(),
        }
    }

    /// Do `act` with this device's settings document as the one every setting reads and writes.
    fn on<R>(&self, act: impl FnOnce() -> R) -> R {
        let _scope = settings_store::scoped_source(Some(
            Arc::clone(&self.settings) as Arc<dyn settings_store::SettingsSource>
        ));
        act()
    }

    /// A pass of this device's session: what the person changed here goes to their store, and what
    /// their other devices changed there comes here.
    fn pass(&self) -> bool {
        self.on(|| self.sync.sync(&self.store))
    }

    fn read(&self, key: &str) -> Option<Value> {
        self.on(|| leaf_of(&settings_store::load(), key).cloned())
    }
}

/// What a person sets on one device reaches their others through their own store, and what a
/// machine keeps of its own never leaves it.
#[test]
fn a_persons_settings_follow_them_and_a_machines_stay_on_it() {
    let laptop = Device::new(0x21, "Laptop");
    let desktop = Device::new(0x22, "Desktop");

    laptop.on(|| {
        settings_store::appearance::MODE
            .set(Some("dark".to_owned()))
            .expect("the theme is kept");
        settings_store::power::LOCK_MINUTES
            .set(1)
            .expect("the lock is kept");
    });
    laptop.pass();
    hand(&laptop.store, &desktop.store);
    assert!(desktop.pass(), "the theme set on the laptop arrives");
    assert_eq!(
        desktop.read("tilo.appearance.mode"),
        Some(serde_json::json!("dark"))
    );
    assert_eq!(
        desktop.read("tilo.power.lock_minutes"),
        None,
        "a machine's own lock stays on it"
    );
    let held = desktop
        .store
        .read(|store| store.person_resource(SETTINGS_KIND))
        .expect("the person's settings are in their store");
    let held = values(&AutoCommit::load(&held).unwrap());
    assert!(
        !held.keys().any(|key| key.starts_with("tilo.power.")),
        "nothing of a machine's own is in the person's store: {held:?}"
    );
    assert!(!desktop.pass(), "a pass with nothing new changes nothing");

    // Each device changes one setting while apart; both changes land on both.
    desktop.on(|| {
        settings_store::appearance::WALLPAPER
            .set(Some("canyon".to_owned()))
            .expect("the wallpaper is kept");
    });
    laptop.on(|| {
        settings_store::update_section(
            "locale",
            Map::from_iter([("region".to_owned(), serde_json::json!("AT"))]),
        )
        .expect("the region is kept");
    });
    desktop.pass();
    laptop.pass();
    hand(&desktop.store, &laptop.store);
    hand(&laptop.store, &desktop.store);
    laptop.pass();
    desktop.pass();
    for device in [&laptop, &desktop] {
        assert_eq!(
            device.read("tilo.appearance.wallpaper"),
            Some(serde_json::json!("canyon"))
        );
        assert_eq!(
            device.read("tilo.locale.region"),
            Some(serde_json::json!("AT"))
        );
        assert_eq!(
            device.read("tilo.appearance.mode"),
            Some(serde_json::json!("dark"))
        );
    }

    // A device that comes up later takes the person's settings over what it kept, and adds only
    // what their store does not hold.
    let tablet = Device::new(0x23, "Tablet");
    tablet.on(|| {
        settings_store::appearance::MODE
            .set(Some("light".to_owned()))
            .expect("the theme is kept");
        settings_store::update_section(
            "locale",
            Map::from_iter([("language".to_owned(), serde_json::json!("de"))]),
        )
        .expect("the language is kept");
    });
    hand(&laptop.store, &tablet.store);
    tablet.pass();
    assert_eq!(
        tablet.read("tilo.appearance.mode"),
        Some(serde_json::json!("dark")),
        "the person's theme, not the one this device kept"
    );
    hand(&tablet.store, &laptop.store);
    laptop.pass();
    assert_eq!(
        laptop.read("tilo.locale.language"),
        Some(serde_json::json!("de")),
        "what only the new device held reaches the others"
    );
}

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
