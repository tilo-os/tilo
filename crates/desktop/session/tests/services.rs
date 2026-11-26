//! The headless session hosting its system services: their resources are published beside the
//! desktop's, and their actions are invoked through the session's API by address, as quick
//! settings, Settings, a script or an agent invokes them. The audio service is reached over the
//! lines a `tilo-audio --serve` speaks.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use crate::common::audio::Card;
use std::io::BufReader;
use std::sync::{Arc, Mutex};

use media::audio::{AudioDevice, AudioGraph};
use runtime_collab::{SpeakerCommand, SpeakerMemberSpec, SpeakerReply, SpeakerRoleSpec};
use serde_json::{Map, Value, json};
use tilo_actions::ApiError;
use tilo_audio_service::sound::{self, Graph, SoundService};
use tilo_compositor::OutputConfig;
use tilo_connectivity_service::Connectivity;
use tilo_connectivity_service::device::{Change, Device, Readings};
use tilo_service::lines::{self, Remote};
use tilo_session::platform::fabric::EndpointSelector;
use tilo_session::platform::fabric::{
    DeviceAbout, DeviceId, DeviceKind, FabricCapabilities, PersonalFabricAction,
};
use tilo_session::platform::fabric::{PeerLink, PersonalFabricSendError};
use tilo_session::{Options, Service, Session};
use tilo_speaker_service::devices::{Listing, Record, Speakers, known_from};
use tilo_ui_base::host_operation::ResourceOperationStart;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;
use tilo_workspace_service::workspaces::person_workspace::speaker::SpeakerEndpoint;

/// A laptop's radios in memory.
#[derive(Default)]
struct Radios {
    readings: Mutex<Readings>,
}

impl Device for Radios {
    fn read(&self) -> Readings {
        self.readings.lock().unwrap().clone()
    }

    fn change(&self, change: &Change) -> Result<(), String> {
        match change {
            Change::Wifi(on) => self.readings.lock().unwrap().network.wifi.on = *on,
            Change::Bluetooth(on) => self.readings.lock().unwrap().bluetooth.on = *on,
            other => return Err(format!("{other:?} is not part of this laptop")),
        }
        Ok(())
    }

    fn scan_operators(&self, _modem: &str) -> Result<Vec<os_actions::Operator>, String> {
        Err("this laptop has no modem".into())
    }

    fn send_code(&self, _modem: &str, _command: &str) -> Result<os_actions::CodeAnswer, String> {
        Err("this laptop has no modem".into())
    }

    fn answer_code(&self, _modem: &str, _response: &str) -> Result<os_actions::CodeAnswer, String> {
        Err("this laptop has no modem".into())
    }

    fn cancel_code(&self, _modem: &str) -> Result<(), String> {
        Err("this laptop has no modem".into())
    }
}

/// A device the card reports, as the audio service reads one.
fn output(id: u32, label: &str, default: bool) -> AudioDevice {
    AudioDevice {
        id,
        serial: id,
        name: format!("node-{id}"),
        description: label.into(),
        is_default: default,
        volume: Some(0.4),
        muted: Some(false),
        channels: 2,
        codec: None,
    }
}

/// The sound service served over lines on a thread, as `tilo-audio
/// --serve` serves it on its pipes; the session's end of the lines.
fn audio_over_lines() -> Remote {
    let card: Arc<dyn Graph> = Arc::new(Card::new(AudioGraph {
        sinks: vec![
            output(40, "Speakers", true),
            output(41, "Headphones", false),
        ],
        ..AudioGraph::default()
    }));
    let (requests_in, requests_out) = std::io::pipe().unwrap();
    let (events_in, events_out) = std::io::pipe().unwrap();
    let (sender, incoming, wake) = lines::incoming();
    std::thread::spawn(move || {
        let mut sound = SoundService::new(card, Some(wake)).unwrap();
        sound.settle();
        let reader_sender = sender.clone();
        std::thread::spawn(move || lines::read_lines(BufReader::new(requests_in), &reader_sender));
        lines::serve(&mut sound, &incoming, events_out).unwrap();
    });
    let events = Remote::events(BufReader::new(events_in), None).unwrap();
    let mut remote = Remote::new(sound::publish, events, Box::new(requests_out));
    remote.wait_for_readings();
    remote
}

fn session() -> Session {
    let radios = Arc::new(Radios::default());
    {
        let mut readings = radios.readings.lock().unwrap();
        readings.network.wifi.available = true;
        readings.bluetooth.available = true;
    }
    let mut connectivity = Connectivity::new(radios, None).unwrap();
    connectivity.settle();
    let services: Vec<Box<dyn Service>> =
        vec![Box::new(connectivity), Box::new(audio_over_lines())];
    crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
        Options {
            services,
            collab: false,
            ..Options::default()
        },
    )
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Wait until `resource` reads as `done` says, taking in what the services report as the session's
/// loop does.
fn until(session: &mut Session, resource: &str, done: impl Fn(&Value) -> bool) -> Value {
    crate::common::wait_for(
        session,
        format!("{resource} to read as expected"),
        |session| {
            session.settle_services();
            session.read(resource).filter(|value| done(value))
        },
    )
}

#[test]
fn turning_wifi_on_is_an_action_of_the_published_network() {
    let mut session = session();
    let resources = session.resources();
    for resource in ["network", "bluetooth", "modem", "vpn", "tethering", "sound"] {
        assert!(
            resources.iter().any(|spec| spec.id.as_str() == resource),
            "{resource} is published beside the desktop"
        );
    }
    assert_eq!(session.read("network").unwrap()["wifi"]["on"], json!(false));
    let started = session
        .invoke("network", "set-wifi", &arguments(json!({"on": true})))
        .expect("taken");
    assert!(matches!(started, ResourceOperationStart::Accepted));
    until(&mut session, "network", |network| {
        network["wifi"]["on"] == json!(true)
    });

    // What the device refuses is the resource's problem, and an action this laptop cannot do is
    // refused before it reaches the device.
    session
        .invoke("network", "scan-wifi", &Map::new())
        .expect("Wi-Fi is on now, so it scans");
    let network = until(&mut session, "network", |network| {
        !network["problem"].is_null()
    });
    assert!(network["problem"].as_str().unwrap().contains("Scan"));
    assert!(matches!(
        session.invoke(
            "network",
            "set-mobile-data",
            &arguments(json!({"on": true}))
        ),
        Err(ApiError::Unavailable { .. })
    ));
}

#[test]
fn choosing_headphones_is_an_action_of_the_published_sound() {
    let mut session = session();
    let sound = session.read("sound").unwrap();
    assert_eq!(sound["output"], json!(40));
    session
        .invoke("sound-output:41", "route-output", &Map::new())
        .expect("the headphones' own action");
    until(&mut session, "sound", |sound| sound["output"] == json!(41));
    session
        .invoke("sound", "set-volume", &arguments(json!({"level": 0.7})))
        .expect("taken");
    let sound = until(&mut session, "sound", |sound| {
        sound["volume"] == json!(0.7_f32)
    });
    assert_eq!(sound["outputs"][1]["chosen"], json!(true));
    assert!(matches!(
        session.invoke("sound", "set-volume", &arguments(json!({"level": "loud"}))),
        Err(ApiError::InvalidArguments(_))
    ));
}

/// The 32 bytes that name `device` in the record of the person's devices.
fn owner_id(device: &str) -> [u8; 32] {
    let mut raw = [0; 32];
    raw[..device.len()].copy_from_slice(device.as_bytes());
    raw
}

/// The 32 bytes in hex that name `device` in the record of the person's devices.
fn key(device: &str) -> String {
    let mut raw = [0; 32];
    raw[..device.len()].copy_from_slice(device.as_bytes());
    hex::encode(raw)
}

/// The person's devices as a stand-in transport keeps them: which of their devices are on it, each
/// offering one output speaker groups can be made of.
#[derive(Clone)]
struct Fabric {
    record: Arc<Mutex<automerge::AutoCommit>>,
    local: DeviceId,
    root: Arc<ed25519_dalek::SigningKey>,
    online: Arc<Mutex<std::collections::BTreeSet<DeviceId>>>,
}

impl Fabric {
    /// This laptop, alone among the person's devices.
    fn laptop() -> Self {
        let fabric = Self {
            record: Arc::new(Mutex::new(
                tilo_workspace_service::workspaces::person_workspace::genesis::<Devices>("")
                    .expect("the record's first state"),
            )),
            local: DeviceId::new(key("this-laptop")).expect("a device id"),
            root: Arc::new(ed25519_dalek::SigningKey::from_bytes(&[21; 32])),
            online: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
        };
        fabric.publish("this-laptop", "This laptop");
        fabric
    }

    /// The output `device` offers, as it publishes it.
    fn output(device: &str) -> EndpointSelector {
        EndpointSelector::mint(
            &DeviceId::new(key(device)).expect("a device id"),
            "alsa_output.analog-stereo",
        )
    }

    /// Offer `device` among the person's devices as it stands now, online: what it says of
    /// itself goes into the record, which is the only copy.
    fn publish(&self, device: &str, label: &str) {
        let owner = DeviceId::new(key(device)).expect("a device id");
        let mut cert = DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: owner_id(device),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, self.root.as_ref())
            .expect("the certificate signs");
        apply_record(
            &self.record,
            Devices::keeping(&cert).expect("the device certificate encodes"),
        )
        .expect("the device is kept");
        let about = DeviceAbout {
            label: label.into(),
            kind: DeviceKind::Laptop,
            capabilities: FabricCapabilities {
                audio_output: true,
                audio_group: true,
                ..FabricCapabilities::default()
            },
        };
        apply_record(
            &self.record,
            Devices::KEEP_FACT.with((
                key(device),
                hex::encode(postcard::to_allocvec(&cert).expect("the certificate encodes")),
                tilo_network_actor::fabric::ABOUT_FACT.to_owned(),
                hex::encode(postcard::to_allocvec(&about).expect("what it is encodes")),
            )),
        )
        .expect("the device says what it is");
        let endpoints = vec![SpeakerEndpoint {
            selector: Self::output(device).to_hex(),
            microphone: false,
            label: "Speakers".into(),
            is_default: true,
        }];
        apply_record(
            &self.record,
            Devices::SAY_SPEAKER_ENDPOINTS.with((
                key(device),
                hex::encode(postcard::to_allocvec(&endpoints).expect("endpoints encode")),
            )),
        )
        .expect("the device offers its output");
        self.online.lock().unwrap().insert(owner);
    }

    /// Another of the person's devices joins the fabric, online.
    fn arrives(&self, device: &str, label: &str) {
        self.publish(device, label);
    }

    /// The sets Settings' snapshot shows, by name.
    fn groups(&self) -> Vec<String> {
        let reply = tilo_speaker_service::settings::handle(&self.link(), SpeakerCommand::Snapshot);
        let SpeakerReply::Snapshot { snapshot } = reply else {
            panic!("Settings asks for a snapshot: {reply:?}");
        };
        snapshot
            .groups
            .into_iter()
            .map(|group| group.name)
            .collect()
    }

    fn link(&self) -> tilo_session::platform::fabric::SharedLink {
        Arc::new(self.clone())
    }

    /// Which devices Settings' snapshot shows, as they stand.
    fn settings_sees(&self) -> Vec<String> {
        let reply = tilo_speaker_service::settings::handle(&self.link(), SpeakerCommand::Snapshot);
        let SpeakerReply::Snapshot { snapshot } = reply else {
            panic!("Settings asks for a snapshot: {reply:?}");
        };
        let mut labels = snapshot
            .devices
            .into_iter()
            .map(|device| device.label)
            .collect::<Vec<_>>();
        labels.sort();
        labels
    }
}

impl PeerLink for Fabric {
    fn online_device_ids(&self) -> std::collections::BTreeSet<DeviceId> {
        self.online.lock().unwrap().clone()
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        Some((self.local.clone(), "This laptop".into()))
    }

    fn try_send_personal_fabric_action(
        &self,
        _: &DeviceId,
        _: PersonalFabricAction,
    ) -> Result<[u8; 16], PersonalFabricSendError> {
        Err(PersonalFabricSendError::Rejected("no transport".into()))
    }

    fn require_media(&self) -> Result<(), String> {
        Ok(())
    }
}

impl Record for Fabric {
    fn record(&self) -> Option<Devices> {
        read_record(&self.record)
    }

    fn apply(&self, action: tilo_resource::Invoke<Devices>) -> Result<(), String> {
        apply_record(&self.record, action)
    }

    fn person(&self) -> [u8; 32] {
        self.root.verifying_key().to_bytes()
    }
}

impl Speakers for Fabric {
    fn known(&self) -> Listing {
        known_from(
            &read_record(&self.record).unwrap_or_default(),
            &self.person(),
            Some(&self.local),
            &self.online_device_ids(),
        )
    }
}

/// Apply one typed action to the record document `record`.
fn apply_record(
    record: &Arc<Mutex<automerge::AutoCommit>>,
    action: tilo_resource::Invoke<Devices>,
) -> Result<(), String> {
    let mut held = record.lock().unwrap();
    let current = std::mem::replace(&mut *held, automerge::AutoCommit::new());
    let mut replica = tilo_resource::Replica::<Devices>::from_doc(current).in_app(
        tilo_workspace_service::workspaces::person_workspace::devices::DEVICES.0,
        "Devices",
    );
    replica.apply(action).map_err(|error| error.to_string())?;
    *held = std::mem::replace(replica.doc_mut(), automerge::AutoCommit::new());
    Ok(())
}

/// The record `record` holds, as the person's devices read it.
fn read_record(record: &Arc<Mutex<automerge::AutoCommit>>) -> Option<Devices> {
    tilo_resource::Replica::<Devices>::from_doc(record.lock().unwrap().clone())
        .in_app("so.tilo.model", "Model")
        .state()
        .ok()
}

fn fabric_session(fabric: &Fabric) -> Session {
    crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
        Options {
            collab: false,
            fabric: Some(fabric.link()),
            ..Options::default()
        },
    )
}

/// The speaker service keeps one view and one revision per process, so the tests that speak to it
/// take turns.
static SPEAKER_SERVICE: Mutex<()> = Mutex::new(());

/// Settings' wait for the next change of the snapshot its Speakers page shows.
struct Wait(std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send>>);

impl Wait {
    /// Wait from `after` , on what `fabric` holds now.
    fn from(fabric: &Fabric, after: u64) -> Self {
        Self(Box::pin(tilo_speaker_service::settings::changed(
            &fabric.link(),
            after,
        )))
    }

    fn poll(&mut self) -> std::task::Poll<u64> {
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        self.0.as_mut().poll(&mut context)
    }
}

/// Step the session's passes until Settings hears of the change they see: a pass is what tells it
/// to read the groups again.
fn when_settings_hears(session: &mut Session, wait: &mut Wait) -> u64 {
    crate::common::wait_for(
        session,
        "the session's passes to tell Settings",
        |session| {
            if let std::task::Poll::Ready(revision) = wait.poll() {
                return Some(revision);
            }
            session.step(std::time::Duration::from_millis(1)).unwrap();
            None
        },
    )
}

/// A device arriving on the person's fabric is visible to the next snapshot Settings takes: the
/// session's passes are what notice it and what resolve the wait Settings keeps while its Speakers
/// page is open.
#[test]
fn a_device_arriving_on_the_fabric_reaches_the_next_settings_snapshot() {
    let _turn = SPEAKER_SERVICE.lock().unwrap();
    let fabric = Fabric::laptop();
    let mut session = fabric_session(&fabric);
    assert_eq!(fabric.settings_sees(), ["This laptop"]);

    // Settings' first wait answers at once, with the revision its reads start from; the next one is
    // for a change that has not happened.
    let started = when_settings_hears(&mut session, &mut Wait::from(&fabric, 0));
    let mut waiting = Wait::from(&fabric, started);
    for _ in 0..8 {
        assert!(
            waiting.poll().is_pending(),
            "nothing moved, so Settings is not told it did"
        );
        session.step(std::time::Duration::from_millis(1)).unwrap();
    }

    // The person's shelf joins the fabric; the session's next passes see it.
    fabric.arrives("Shelf", "Shelf");
    let changed = when_settings_hears(&mut session, &mut waiting);
    assert_ne!(changed, started, "the revision moved");
    assert_eq!(fabric.settings_sees(), ["Shelf", "This laptop"]);
}

/// A set saved from Settings' Speakers page reaches the session's speaker groups and is read back
/// from them: the session admits it against the devices its fabric carries, and the next snapshot
/// it publishes is what Settings lists it from.
#[test]
fn a_speaker_set_saved_in_settings_is_read_back_from_the_session() {
    let _turn = SPEAKER_SERVICE.lock().unwrap();
    let fabric = Fabric::laptop();
    let mut session = fabric_session(&fabric);
    assert!(fabric.groups().is_empty());

    let reply = tilo_speaker_service::settings::handle(
        &fabric.link(),
        SpeakerCommand::Create {
            name: "Living room".into(),
            members: vec![SpeakerMemberSpec {
                device: key("this-laptop"),
                output: Fabric::output("this-laptop").to_hex(),
                role: SpeakerRoleSpec::Room {
                    x_mm: 0,
                    y_mm: 600,
                    z_mm: 1100,
                },
            }],
        },
    );
    let SpeakerReply::Done { group: Some(group) } = reply else {
        panic!("the session saved the set: {reply:?}");
    };

    assert_eq!(fabric.groups(), ["Living room"]);

    let reply = tilo_speaker_service::settings::handle(&fabric.link(), SpeakerCommand::Snapshot);
    let SpeakerReply::Snapshot { snapshot } = reply else {
        panic!("Settings asks for a snapshot: {reply:?}");
    };
    let saved = snapshot
        .groups
        .into_iter()
        .find(|entry| entry.id == group)
        .expect("the saved set is in the snapshot");
    assert_eq!(saved.name, "Living room");
    assert!(saved.led_here, "this device leads it");
    assert_eq!(saved.members.len(), 1);

    // The session keeps running the sets on its fabric passes.
    session.step(std::time::Duration::from_millis(1)).unwrap();
}
