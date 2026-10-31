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
use serde_json::{Map, Value, json};
use tilo_actions::DoorError;
use tilo_audio_service::sound::{self, Graph, SoundService};
use tilo_compositor::OutputConfig;
use tilo_connectivity_service::Connectivity;
use tilo_connectivity_service::device::{Change, Device, Readings};
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
