//! A copy follows the person to their other devices: what was copied here is
//! offered to the fabric, and what they copied there joins the clipboard
//! here, saying which device it came from.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_compositor::clipboard_history::TEXT_MIME;
use tilo_compositor::{OutputConfig, Selection};
use tilo_desktop::desktop_actions::CHROME_RESOURCE;
use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError, SharedLink};
use tilo_device_fabric::speaker_group::SpeakerFabricState;
use tilo_device_fabric::unlock::UnlockFabricState;
use tilo_device_fabric::{
    ClipboardEntry, ClipboardPayload, DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind,
    DevicePresence, FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, OwnerSession,
    PersonalFabricAction,
};
use tilo_quick_settings::device::Device;
use tilo_quick_settings::device::testing::{Kept, Machine};
use tilo_session::{DeviceHost, Options, QuickSettings, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// This device's fabric id.
fn this_device() -> DeviceId {
    DeviceId::new("this-pc").expect("a device id")
}

/// The fabric as a stand-in transport keeps it, and what the session offered
/// it.
#[derive(Default)]
struct Fabric {
    converged: DeviceFabric,
    shared: Vec<ClipboardEntry>,
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

    /// What the session offered the person's other devices.
    fn shared(&self) -> Vec<ClipboardEntry> {
        self.fabric.lock().expect("the fabric").shared.clone()
    }

    /// Another of the person's devices shared something.
    fn shared_from(&self, device: &str, text: &str, created_unix_ms: u64) {
        let origin = DeviceId::new(device).expect("a device id");
        let entry = ClipboardEntry::shareable(
            ClipboardPayload::Text(text.to_owned()),
            origin.clone(),
            created_unix_ms,
            FabricStamp {
                counter: created_unix_ms,
                device: origin.clone(),
            },
        )
        .expect("a shareable copy");
        let snapshot = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: origin.clone(),
            presence: DevicePresence {
                device: origin,
                label: device.to_owned(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities {
                    clipboard: true,
                    ..FabricCapabilities::default()
                },
                online: true,
                last_seen_unix_ms: created_unix_ms,
                wake_routes: Vec::new(),
                session: OwnerSession::default(),
                stamp: FabricStamp {
                    counter: created_unix_ms,
                    device: DeviceId::new(device).expect("a device id"),
                },
            },
            media: Vec::new(),
            clipboard: vec![entry],
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            speakers: SpeakerFabricState::default(),
            unlock: UnlockFabricState::default(),
        };
        self.fabric
            .lock()
            .expect("the fabric")
            .converged
            .upsert(snapshot)
            .expect("the snapshot is valid");
    }
}

impl FabricLink for FakeLink {
    fn device_fabric(&self) -> DeviceFabric {
        self.fabric.lock().expect("the fabric").converged.clone()
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

    fn share_clipboard(&self, entries: Vec<ClipboardEntry>) {
        self.fabric.lock().expect("the fabric").shared = entries;
    }
}

fn session(link: &FakeLink) -> Session {
    let reads = Machine::laptop();
    let fabric: SharedLink = Arc::new(link.clone());
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            quick_settings: Some(QuickSettings::Device(Box::new(move |wake| {
                DeviceHost::new(
                    Device {
                        reads: std::sync::Arc::new(reads.clone()),
                        dispatcher: std::sync::Arc::new(reads),
                        preferences: Box::new(Kept::default()),
                    },
                    wake,
                )
            }))),
            fabric: Some(fabric),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// Step the loop until `done` holds, for a couple of seconds: the fabric is
/// read a few times a second, not every turn.
fn when(session: &mut Session, done: impl Fn(&Session) -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        step(session);
        if done(session) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the fabric was never read");
}

fn step(session: &mut Session) {
    session.step(Duration::from_millis(1)).expect("step");
}

fn copy(session: &mut Session, text: &str) {
    session
        .compositor()
        .set_selection(
            Selection::Clipboard,
            vec![(TEXT_MIME.to_owned(), text.as_bytes().to_vec())],
        )
        .expect("the app copies");
}

/// The Clipboard section, open in quick settings.
fn open_clipboard(session: &mut Session) {
    if !session.quick_settings_open() {
        session
            .invoke(
                CHROME_RESOURCE,
                "toggle-quick-settings",
                &serde_json::Map::new(),
            )
            .expect("open quick settings");
    }
    session
        .invoke(
            tilo_session::QUICK_SETTINGS,
            "open-section",
            &serde_json::from_value(serde_json::json!({ "section": "clipboard" }))
                .expect("arguments"),
        )
        .expect("open the Clipboard section");
    for _ in 0..3 {
        step(session);
    }
}

#[test]
fn a_copy_is_offered_to_the_person_s_other_devices_and_theirs_joins_the_clipboard() {
    let _settled = ui::motion::testing::settled();
    let link = FakeLink::joined();
    let mut session = session(&link);

    // What is copied here is offered to the fabric, as this device's copy.
    copy(&mut session, "made here");
    for _ in 0..4 {
        step(&mut session);
    }
    let shared = link.shared();
    let texts: Vec<&str> = shared
        .iter()
        .filter_map(|entry| match &entry.payload {
            ClipboardPayload::Text(text) => Some(text.as_str()),
            ClipboardPayload::Binary { .. } => None,
        })
        .collect();
    assert_eq!(texts, ["made here"], "the copy was offered: {shared:?}");
    assert!(
        shared.iter().all(|entry| entry.origin == this_device()),
        "a device only shares its own copies"
    );

    // What they copied on their laptop joins the history here, saying which
    // device it came from.
    link.shared_from("laptop", "made there", ui::time::unix_now_ms());
    open_clipboard(&mut session);
    when(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.contains("made there"))
    });
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label.contains("made there")),
        "their copy is listed: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "2 copies"),
        "the section counts what they shared: {labels:?}"
    );

    // Their copy is theirs: it is not offered back to the fabric.
    let shared = link.shared();
    assert!(
        !shared.iter().any(|entry| matches!(
            &entry.payload,
            ClipboardPayload::Text(text) if text == "made there"
        )),
        "a copy from another device is never shared again: {shared:?}"
    );
    assert!(
        shared.iter().any(
            |entry| matches!(&entry.payload, ClipboardPayload::Text(text) if text == "made here")
        ),
        "what was copied here is still shared"
    );
}
