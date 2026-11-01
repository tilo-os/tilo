//! The headless session hosting its system services: their resources are
//! published beside the desktop's, and their actions are invoked through
//! the session's door by address, as quick settings, Settings, a script or
//! an agent invokes them. The audio service is reached over the lines a
//! `tilo-audio --serve` speaks.

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

use std::io::BufReader;
use std::sync::{Arc, Mutex};

use media::audio::{AudioDevice, AudioGraph, GraphControl};
use runtime_collab::{SpeakerCommand, SpeakerMemberSpec, SpeakerReply, SpeakerRoleSpec};
use serde_json::{Map, Value, json};
use tilo_actions::DoorError;
use tilo_audio_service::sound::{self, Graph, SoundService};
use tilo_compositor::OutputConfig;
use tilo_connectivity_service::Connectivity;
use tilo_connectivity_service::device::{Change, Device, Readings};
use tilo_device_fabric::link::{FabricLink, PersonalFabricSendError};
use tilo_device_fabric::speaker_group::{
    AudioEndpoint, AudioEndpointKind, EndpointSelector, SpeakerFabricState,
};
use tilo_device_fabric::unlock::UnlockFabricState;
use tilo_device_fabric::{
    DeviceFabric, DeviceFabricSnapshot, DeviceId, DeviceKind, DevicePresence,
    FABRIC_SCHEMA_VERSION, FabricCapabilities, FabricStamp, OwnerSession, PersonalFabricAction,
};
use tilo_service::lines::{self, Remote};
use tilo_session::{Options, Service, Session};
use ui::host_operation::ResourceOperationStart;

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
}

/// A sound card in memory: two outputs, the speakers in use.
struct Card {
    graph: Mutex<AudioGraph>,
}

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

impl Graph for Card {
    fn read(&self) -> AudioGraph {
        self.graph.lock().unwrap().clone()
    }

    fn control(&self) -> &dyn GraphControl {
        self
    }
}

impl GraphControl for Card {
    fn set_default_sink(&self, node_id: u32) -> Result<(), String> {
        for sink in &mut self.graph.lock().unwrap().sinks {
            sink.is_default = sink.id == node_id;
        }
        Ok(())
    }

    fn set_default_source(&self, _node_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn route_stream(&self, _stream_id: u32, _target_serial: u32) -> Result<(), String> {
        Ok(())
    }

    fn unroute_stream(&self, _stream_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String> {
        for sink in &mut self.graph.lock().unwrap().sinks {
            if sink.id == node_id {
                sink.volume = Some(perceptual);
            }
        }
        Ok(())
    }

    fn set_node_mute(&self, _node_id: u32, _muted: bool) -> Result<(), String> {
        Ok(())
    }

    fn set_device_profile(&self, _device_id: u32, _profile_index: u32) -> Result<(), String> {
        Ok(())
    }
}

/// The sound service served over lines on a thread, as `tilo-audio
/// --serve` serves it on its pipes; the session's end of the lines.
fn audio_over_lines() -> Remote {
    let card: Arc<dyn Graph> = Arc::new(Card {
        graph: Mutex::new(AudioGraph {
            sinks: vec![
                output(40, "Speakers", true),
                output(41, "Headphones", false),
            ],
            ..AudioGraph::default()
        }),
    });
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
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session")
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Wait until `resource` reads as `done` says, taking in what the services
/// report as the session's loop does.
fn until(session: &mut Session, resource: &str, done: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..2000 {
        session.settle_services();
        session.step(std::time::Duration::from_millis(1)).unwrap();
        let value = session.read(resource).expect("published");
        if done(&value) {
            return value;
        }
    }
    panic!(
        "{resource} never read as expected: {:?}",
        session.read(resource)
    );
}

#[test]
fn turning_wifi_on_is_an_action_of_the_published_network() {
    let mut session = session();
    let resources = session.resources();
    for resource in ["network", "bluetooth", "vpn", "tethering", "sound"] {
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

    // What the device refuses is the resource's problem, and an action this
    // laptop cannot do is refused before it reaches the device.
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
        Err(DoorError::Unavailable { .. })
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
        Err(DoorError::InvalidArguments(_))
    ));
}

/// The person's device fabric as a stand-in transport keeps it: which of
/// their devices are on it, each publishing one output speaker groups can be
/// made of.
#[derive(Clone)]
struct Fabric {
    converged: Arc<Mutex<DeviceFabric>>,
    local: DeviceId,
}

impl Fabric {
    /// This laptop, alone on the fabric.
    fn laptop() -> Self {
        let fabric = Self {
            converged: Arc::new(Mutex::new(DeviceFabric::default())),
            local: DeviceId::new("this-laptop").expect("a device id"),
        };
        fabric.publish("this-laptop", "This laptop");
        fabric
    }

    /// The output `device` offers, as it publishes it.
    fn output(device: &str) -> EndpointSelector {
        EndpointSelector::mint(
            &DeviceId::new(device).expect("a device id"),
            "alsa_output.analog-stereo",
        )
    }

    /// Publish `device` on the fabric as it stands now.
    fn publish(&self, device: &str, label: &str) {
        let owner = DeviceId::new(device).expect("a device id");
        let snapshot = DeviceFabricSnapshot {
            schema_version: FABRIC_SCHEMA_VERSION,
            owner: owner.clone(),
            presence: DevicePresence {
                device: owner.clone(),
                label: label.into(),
                kind: DeviceKind::Laptop,
                capabilities: FabricCapabilities {
                    audio_output: true,
                    audio_group: true,
                    ..FabricCapabilities::default()
                },
                online: true,
                last_seen_unix_ms: 1,
                wake_routes: Vec::new(),
                session: OwnerSession::default(),
                stamp: FabricStamp {
                    counter: 1,
                    device: owner,
                },
            },
            media: Vec::new(),
            clipboard: Vec::new(),
            settings: Vec::new(),
            wifi: Vec::new(),
            display: None,
            speakers: SpeakerFabricState {
                endpoints: vec![AudioEndpoint {
                    selector: Self::output(device),
                    kind: AudioEndpointKind::Output,
                    label: "Speakers".into(),
                    is_default: true,
                }],
                ..SpeakerFabricState::default()
            },
            unlock: UnlockFabricState::default(),
        };
        self.converged
            .lock()
            .unwrap()
            .upsert(snapshot)
            .expect("the snapshot is valid");
    }

    /// Another of the person's devices joins the fabric, online.
    fn arrives(&self, device: &str, label: &str) {
        self.publish(device, label);
    }

    /// Publish this device's speaker records as its snapshot does: the sets
    /// it leads, as the group core hands them to the snapshot builder.
    fn publishes_its_speaker_sets(&self) {
        let mut converged = self.converged.lock().unwrap();
        let mut snapshot = converged
            .snapshots()
            .get(&self.local)
            .cloned()
            .expect("this device is published");
        snapshot.presence.stamp.counter = snapshot.presence.stamp.counter.saturating_add(1);
        let stamp = snapshot.presence.stamp.clone();
        snapshot.speakers = tilo_speaker_service::groups::authored_state(
            &self.local,
            Some(&snapshot.speakers),
            &stamp,
            true,
        );
        converged.upsert(snapshot).expect("the snapshot is valid");
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

    fn link(&self) -> tilo_device_fabric::link::SharedLink {
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

impl FabricLink for Fabric {
    fn device_fabric(&self) -> DeviceFabric {
        self.converged.lock().unwrap().clone()
    }

    fn online_device_ids(&self) -> std::collections::BTreeSet<DeviceId> {
        std::collections::BTreeSet::from([self.local.clone()])
    }

    fn local_device_fabric_identity(&self) -> Option<(DeviceId, String)> {
        Some((self.local.clone(), "This laptop".into()))
    }

    fn send_personal_fabric_action(
        &self,
        _: &DeviceId,
        _: PersonalFabricAction,
    ) -> Result<[u8; 16], String> {
        Err("no transport in a test".into())
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

fn fabric_session(fabric: &Fabric) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            fabric: Some(fabric.link()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session")
}

/// The speaker service keeps one view and one revision per process, so the
/// tests that speak to it take turns.
static SPEAKER_SERVICE: Mutex<()> = Mutex::new(());

/// Settings' wait for the next change of the snapshot its Speakers page
/// shows.
struct Wait(std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send>>);

impl Wait {
    /// Wait from `after`, on what `fabric` holds now.
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

/// Step the session's passes until Settings hears of the change they see:
/// a pass is what tells it to read the groups again.
fn when_settings_hears(session: &mut Session, wait: &mut Wait) -> u64 {
    for _ in 0..2000 {
        if let std::task::Poll::Ready(revision) = wait.poll() {
            return revision;
        }
        session.step(std::time::Duration::from_millis(1)).unwrap();
    }
    panic!("the session's passes never told Settings of the change");
}

/// A device arriving on the person's fabric is visible to the next snapshot
/// Settings takes: the session's passes are what notice it and what resolve
/// the wait Settings keeps while its Speakers page is open.
#[test]
fn a_device_arriving_on_the_fabric_reaches_the_next_settings_snapshot() {
    let _turn = SPEAKER_SERVICE.lock().unwrap();
    let fabric = Fabric::laptop();
    let mut session = fabric_session(&fabric);
    assert_eq!(fabric.settings_sees(), ["This laptop"]);

    // Settings' first wait answers at once, with the revision its reads
    // start from; the next one is for a change that has not happened.
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

/// A set saved from Settings' Speakers page reaches the session's speaker
/// groups and is read back from them: the session admits it against the
/// devices its fabric carries, and the next snapshot it publishes is what
/// Settings lists it from.
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
                device: "this-laptop".into(),
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

    fabric.publishes_its_speaker_sets();
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
