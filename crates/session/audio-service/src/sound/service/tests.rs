use std::sync::{Arc, Mutex};

use media::audio::{AudioDevice, AudioGraph, AudioStream, GraphControl};
use serde_json::{Map, Value, json};
use tilo_actions::DoorError;
use tilo_service::{Service, Started};

use super::super::SOUND;
use super::{Graph, SoundService};

fn device(id: u32, label: &str, default: bool) -> AudioDevice {
    AudioDevice {
        id,
        serial: id,
        name: format!("node-{id}"),
        description: label.into(),
        is_default: default,
        volume: Some(0.5),
        muted: Some(false),
        channels: 2,
        codec: None,
    }
}

/// A graph in memory: the control's calls are recorded and applied.
struct Fake {
    graph: Mutex<AudioGraph>,
    calls: Mutex<Vec<String>>,
}

impl Fake {
    fn laptop() -> Arc<Self> {
        Arc::new(Self {
            graph: Mutex::new(AudioGraph {
                sinks: vec![
                    device(40, "Speakers", true),
                    device(41, "Headphones", false),
                    AudioDevice {
                        name: media::audio::SPEAKER_GROUP_SINK.into(),
                        ..device(42, "Living room", false)
                    },
                ],
                sources: vec![device(50, "Microphone", true)],
                playback_streams: vec![AudioStream {
                    id: 70,
                    serial: 70,
                    app: "Music".into(),
                    media_name: None,
                    playback: true,
                    target_id: Some(40),
                    volume: None,
                    muted: None,
                }],
                ..AudioGraph::default()
            }),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("calls").clone()
    }

    /// Record `call` and make `change`: what the control answers.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "it answers for the control, whose calls answer a result"
    )]
    fn call(&self, call: String, change: impl FnOnce(&mut AudioGraph)) -> Result<(), String> {
        self.calls.lock().expect("calls").push(call);
        change(&mut self.graph.lock().expect("graph"));
        Ok(())
    }
}

impl Graph for Fake {
    fn read(&self) -> AudioGraph {
        self.graph.lock().expect("graph").clone()
    }

    fn control(&self) -> &dyn GraphControl {
        self
    }
}

impl GraphControl for Fake {
    fn set_default_sink(&self, node_id: u32) -> Result<(), String> {
        self.call(format!("default-sink {node_id}"), |graph| {
            for sink in &mut graph.sinks {
                sink.is_default = sink.id == node_id;
            }
        })
    }

    fn set_default_source(&self, node_id: u32) -> Result<(), String> {
        self.call(format!("default-source {node_id}"), |_| {})
    }

    fn route_stream(&self, stream_id: u32, target_serial: u32) -> Result<(), String> {
        self.call(format!("route {stream_id} {target_serial}"), |_| {})
    }

    fn unroute_stream(&self, stream_id: u32) -> Result<(), String> {
        self.call(format!("unroute {stream_id}"), |_| {})
    }

    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String> {
        self.call(format!("volume {node_id} {perceptual:.2}"), |graph| {
            for sink in &mut graph.sinks {
                if sink.id == node_id {
                    sink.volume = Some(perceptual);
                }
            }
        })
    }

    fn set_node_mute(&self, node_id: u32, muted: bool) -> Result<(), String> {
        self.call(format!("mute {node_id} {muted}"), |graph| {
            for device in graph.sinks.iter_mut().chain(&mut graph.sources) {
                if device.id == node_id {
                    device.muted = Some(muted);
                }
            }
        })
    }

    fn set_device_profile(&self, device_id: u32, profile_index: u32) -> Result<(), String> {
        self.call(format!("profile {device_id} {profile_index}"), |_| {})
    }
}

fn service(fake: &Arc<Fake>) -> SoundService {
    let graph: Arc<dyn Graph> = Arc::clone(fake) as Arc<dyn Graph>;
    let mut service = SoundService::new(graph, None).expect("service");
    service.settle();
    service
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn read(service: &SoundService) -> Value {
    service.read(SOUND).expect("published")
}

/// The volume and mute act on the output in use and land in the reading;
/// a step moves from where the volume reads now.
#[test]
fn volume_and_mute_act_on_the_output_in_use() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    assert_eq!(read(&service)["output"], json!(40));
    assert_eq!(
        service.invoke(SOUND, "set-volume", &arguments(json!({"level": 0.8}))),
        Ok(Started::Accepted)
    );
    service.settle();
    assert_eq!(read(&service)["volume"], json!(0.8_f32));
    service
        .invoke(SOUND, "volume-down", &Map::new())
        .expect("taken");
    service.settle();
    service
        .invoke(SOUND, "toggle-muted", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(read(&service)["muted"], json!(true));
    assert_eq!(
        fake.calls(),
        ["volume 40 0.80", "volume 40 0.75", "mute 40 true"]
    );

    let refused = service.invoke(SOUND, "set-volume", &arguments(json!({"level": 3})));
    assert!(
        matches!(refused, Err(DoorError::InvalidArguments(_))),
        "{refused:?}"
    );
    assert_eq!(
        fake.calls().len(),
        3,
        "nothing out of range reached the graph"
    );
}

/// Turning the sound up out of silence brings it back; turning it down
/// while muted leaves the silence to its own control.
#[test]
fn raising_the_volume_unmutes_and_lowering_it_does_not() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    service
        .invoke(SOUND, "set-muted", &arguments(json!({"muted": true})))
        .expect("taken");
    service.settle();
    assert_eq!(read(&service)["muted"], json!(true));

    service
        .invoke(SOUND, "volume-down", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(
        read(&service)["muted"],
        json!(true),
        "a quieter sound stays silent"
    );

    service
        .invoke(SOUND, "volume-up", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(read(&service)["muted"], json!(false));

    service
        .invoke(SOUND, "set-muted", &arguments(json!({"muted": true})))
        .expect("taken");
    service.settle();
    service
        .invoke(SOUND, "set-volume", &arguments(json!({"level": 0.9})))
        .expect("taken");
    service.settle();
    assert_eq!(
        read(&service)["muted"],
        json!(false),
        "a slider moved above the level it read unmutes too"
    );
}

/// Each output is a child that plays through it: it becomes the default
/// and every stream is released to follow the default.
#[test]
fn an_output_is_chosen_from_its_child() {
    let fake = Fake::laptop();
    let mut service = service(&fake);
    let resources = service.resources();
    let children: Vec<&str> = resources[0]
        .children
        .iter()
        .map(|child| child.id.as_str())
        .collect();
    assert_eq!(
        children,
        ["sound-output:40", "sound-output:41", "sound-input:50"],
        "a speaker group's sink is no output of its own"
    );
    service
        .invoke("sound-output:41", "route-output", &Map::new())
        .expect("taken");
    service.settle();
    assert_eq!(fake.calls(), ["default-sink 41", "unroute 70"]);
    let sound = read(&service);
    assert_eq!(sound["output"], json!(41));
    assert_eq!(sound["outputs"][1]["chosen"], json!(true));
    assert_eq!(sound["pending"], json!([]));
}

/// Without an output, the volume is published unavailable and refused.
#[test]
fn without_an_output_the_volume_is_unavailable() {
    let fake = Arc::new(Fake {
        graph: Mutex::new(AudioGraph::default()),
        calls: Mutex::new(Vec::new()),
    });
    let mut service = service(&fake);
    for action in ["set-muted", "volume-up"] {
        let refused = service.invoke(SOUND, action, &arguments(json!({"muted": true})));
        assert!(
            matches!(
                refused,
                Err(DoorError::Unavailable { .. } | DoorError::InvalidArguments(_))
            ),
            "{action}: {refused:?}"
        );
    }
    assert!(fake.calls().is_empty());
}
