//! The person's settings follow them to their other devices, and this
//! device's stay on it: what the person sets here is offered to the peer
//! fabric, what they set on their laptop arrives here, and neither carries a
//! setting of the machine it was set on.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tilo_compositor::OutputConfig;
use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError, SharedLink};
use tilo_device_fabric::speaker_group::SpeakerFabricState;
use tilo_device_fabric::unlock::UnlockFabricState;
use tilo_device_fabric::{
    DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
    RoamingSetting,
};
use tilo_session::{Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// This device's fabric id.
fn this_device() -> DeviceId {
    DeviceId::new("this-pc").expect("a device id")
}

/// The peer fabric as a stand-in transport keeps it.
#[derive(Default)]
struct Fabric {
    converged: DeviceFabric,
    /// What this device offered for its next snapshot: a transport publishes
    /// what it was offered when it next carries the snapshot, not in the
    /// pass it was offered in.
    pending: BTreeMap<DeviceId, Vec<RoamingSetting>>,
}

#[derive(Clone, Default)]
struct FakeLink {
    fabric: Arc<Mutex<Fabric>>,
    identity: Arc<Mutex<Option<DeviceId>>>,
}

impl FakeLink {
    /// A link on a fabric this device has joined.
    fn joined() -> Self {
        let link = Self::default();
        *link.identity.lock().expect("the identity") = Some(this_device());
        link
    }

    /// The fabric as every device of the person reads it: what this device
    /// offered last pass is carried as its own snapshot.
    fn converged(&self) -> DeviceFabric {
        let mut fabric = self.fabric.lock().expect("the fabric");
        let pending = std::mem::take(&mut fabric.pending);
        for (device, settings) in pending {
            let counter = settings
                .iter()
                .map(|record| record.stamp.counter)
                .max()
                .unwrap_or(1);
            fabric
                .converged
                .upsert(snapshot(device, counter, settings))
                .expect("a device offers records of its own person's settings");
        }
        fabric.converged.clone()
    }

    /// What the person's other devices resolve from the fabric.
    fn what_their_devices_read(&self) -> BTreeMap<String, Value> {
        self.fabric
            .lock()
            .expect("the fabric")
            .converged
            .merged_settings()
    }

    /// The records this device's snapshot carries, by key.
    fn offered(&self) -> BTreeMap<String, RoamingSetting> {
        self.fabric
            .lock()
            .expect("the fabric")
            .converged
            .snapshots()
            .get(&this_device())
            .map(|snapshot| {
                snapshot
                    .settings
                    .iter()
                    .map(|record| (record.key.clone(), record.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Another of the person's devices set `values` at `counter`, as its own
    /// records, beside whatever it already offered.
    fn set_on(&self, device: &str, counter: u64, values: &[(&str, Value)]) -> Result<(), String> {
        let origin = DeviceId::new(device).expect("a device id");
        let stamp = FabricStamp {
            counter,
            device: origin.clone(),
        };
        let mut fabric = self.fabric.lock().expect("the fabric");
        let earlier: Vec<RoamingSetting> = fabric
            .converged
            .snapshots()
            .get(&origin)
            .map(|snapshot| {
                snapshot
                    .settings
                    .iter()
                    .filter(|record| !values.iter().any(|(key, _)| *key == record.key))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let settings: Vec<RoamingSetting> = earlier
            .into_iter()
            .chain(values.iter().map(|(key, value)| RoamingSetting {
                key: (*key).to_owned(),
                value: value.clone(),
                stamp: stamp.clone(),
            }))
            .collect();
        fabric
            .converged
            .upsert(snapshot(origin, stamp.counter, settings))
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// A snapshot of `device` carrying `settings`, as its session publishes one.
fn snapshot(device: DeviceId, counter: u64, settings: Vec<RoamingSetting>) -> DeviceFabricSnapshot {
    DeviceFabricSnapshot {
        schema_version: FABRIC_SCHEMA_VERSION,
        owner: device.clone(),
        presence: DevicePresence {
            device: device.clone(),
            label: device.as_str().to_owned(),
            kind: DeviceKind::Laptop,
            capabilities: FabricCapabilities {
                roaming_settings: true,
                ..FabricCapabilities::default()
            },
            online: true,
            last_seen_unix_ms: counter,
            wake_routes: Vec::new(),
            session: OwnerSession::default(),
            stamp: FabricStamp { counter, device },
        },
        media: Vec::new(),
        clipboard: Vec::new(),
        settings,
        wifi: Vec::new(),
        display: None,
        speakers: SpeakerFabricState::default(),
        unlock: UnlockFabricState::default(),
    }
}

impl FabricLink for FakeLink {
    fn device_fabric(&self) -> DeviceFabric {
        self.converged()
    }

    fn online_device_ids(&self) -> BTreeSet<DeviceId> {
        BTreeSet::new()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        self.identity
            .lock()
            .expect("the identity")
            .clone()
            .map(|device| (device, "this computer".to_owned()))
    }

    fn send_personal_fabric_action(
        &self,
        _: &DeviceId,
        _: PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        Err("no transport in a test".to_owned())
    }

    fn try_send_personal_fabric_action(
        &self,
        _: &DeviceId,
        _: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        Err(PersonalFabricSendError::Rejected("no transport".to_owned()))
    }

    fn require_media(&self) -> Result<(), String> {
        Ok(())
    }

    fn share_settings(&self, settings: Vec<RoamingSetting>) {
        let Some(device) = self.identity.lock().expect("the identity").clone() else {
            return;
        };
        self.fabric
            .lock()
            .expect("the fabric")
            .pending
            .insert(device, settings);
    }
}

fn session(link: &FakeLink) -> Session {
    let fabric: SharedLink = Arc::new(link.clone());
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            fabric: Some(fabric),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("step");
}

/// Step the loop until `done` holds, for a couple of seconds: the fabric is
/// read a few times a second, not on every turn.
fn when(session: &mut Session, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        step(session);
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the fabric was never read");
}

#[test]
fn a_personal_setting_set_here_arrives_on_the_other_device_and_a_device_setting_does_not() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined();
    let mut session = session(&link);

    // The person turns the dark theme on, as the Settings pane and the
    // palette do, and sets a thing or two that belong to this machine: the
    // lock's deadline, and an app's entry that stays on this device.
    settings_store::appearance::MODE
        .set(Some("dark".to_owned()))
        .expect("the theme is kept");
    settings_store::power::LOCK_MINUTES.set(1).expect("kept");
    settings_store::update_section(
        settings_store::placement::APP_DEVICE_LOCAL_SECTION,
        serde_json::Map::from_iter([(
            "so.tilo.monitor".to_owned(),
            serde_json::json!({ "confirm_kill": true }),
        )]),
    )
    .expect("kept");

    // The other device hears what the person holds: their theme, and nothing
    // of this machine's.
    when(&mut session, || {
        link.what_their_devices_read().get("tilo.appearance.mode")
            == Some(&serde_json::json!("dark"))
    });
    let theirs = link.what_their_devices_read();
    assert_eq!(
        theirs.get("tilo.appearance.mode"),
        Some(&serde_json::json!("dark")),
        "the theme follows them: {theirs:?}"
    );
    assert!(
        !theirs
            .keys()
            .any(|key| key.starts_with("tilo.power.") || key.starts_with("tilo.app_device_local")),
        "this machine's settings stay on it: {theirs:?}"
    );
    let offered = link.offered();
    assert!(
        offered
            .values()
            .all(|record| record.stamp.device == this_device()),
        "a device only offers its own records"
    );

    // The theme keeps the stamp it was offered with: a setting that did not
    // change is not offered again as though the person had just set it.
    let stamp = offered["tilo.appearance.mode"].stamp.clone();
    for _ in 0..4 {
        step(&mut session);
    }
    assert_eq!(link.offered()["tilo.appearance.mode"].stamp, stamp);

    // What they set on their laptop arrives here, in the document every pane
    // and app reads; a setting of that machine's cannot travel at all.
    link.set_on(
        "laptop",
        1,
        &[("tilo.locale.region", serde_json::json!("AT"))],
    )
    .expect("the person's own setting travels");
    assert!(
        link.set_on(
            "laptop",
            1,
            &[("tilo.clock.time_format", serde_json::json!("24h"))]
        )
        .is_err(),
        "a clock format belongs to the machine it is set on"
    );
    when(&mut session, || {
        settings_store::load_section("locale").get("region") == Some(&serde_json::json!("AT"))
    });
    assert_eq!(
        settings_store::load()["locale"]["region"],
        serde_json::json!("AT")
    );
    assert_eq!(
        settings_store::power::LOCK_MINUTES.get(),
        1,
        "nothing of this machine's settings changed"
    );
    assert_eq!(
        settings_store::load_section(settings_store::placement::APP_DEVICE_LOCAL_SECTION)["so.tilo.monitor"]
            ["confirm_kill"],
        serde_json::json!(true)
    );

    // A value the person sets here is not undone by one their laptop still
    // holds: the laptop's wallpaper was the newest the fabric had, they pick
    // another here, and this desktop keeps theirs.
    link.set_on(
        "laptop",
        100,
        &[("tilo.appearance.wallpaper", serde_json::json!("reef"))],
    )
    .expect("the person's own setting travels");
    settings_store::appearance::WALLPAPER
        .set(Some("canyon".to_owned()))
        .expect("the wallpaper is kept");
    when(&mut session, || {
        link.what_their_devices_read()
            .get("tilo.appearance.wallpaper")
            == Some(&serde_json::json!("canyon"))
    });
    assert_eq!(
        settings_store::appearance::WALLPAPER.get(),
        Some("canyon".to_owned()),
        "a change made here is not undone by an older one from the fabric"
    );
    assert_eq!(
        link.what_their_devices_read()
            .get("tilo.appearance.wallpaper"),
        Some(&serde_json::json!("canyon")),
        "the newest change the person made is the one their devices read"
    );

    // And it is theirs, not a change made here: this device does not offer it
    // back as its own.
    let offered = link.offered();
    assert!(
        offered
            .get("tilo.locale.region")
            .is_none_or(|record| record.stamp.device != this_device()),
        "a setting taken from another device is not claimed here: {offered:?}"
    );
}
