use std::sync::{Arc, Mutex};

use media::audio::{AudioDevice, AudioGraph, AudioStream, GraphControl};
use serde_json::{Map, Value, json};
use tilo_actions::ApiError;
use tilo_service::{Service, Started};

use super::super::SOUND;
use super::{Graph, Read, SoundService};

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
    /// Who `PipeWire` says is recording; the graph alone does not say it.
    uses: Mutex<Vec<media::audio::capture::MediaUse>>,
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
                    application: "Music".into(),
                    media_name: None,
                    playback: true,
                    target_id: Some(40),
                    volume: None,
                    muted: None,
                }],
                hardware: vec![media::audio::AudioHardwareDevice {
                    id: 60,
                    description: "Headset".into(),
                    active_profile: Some(1),
                    profiles: vec![
                        media::audio::DeviceProfile {
                            index: 1,
                            name: "a2dp".into(),
                            description: "High Fidelity".into(),
                            available: true,
                        },
                        media::audio::DeviceProfile {
                            index: 2,
                            name: "hfp".into(),
                            description: "Headset microphone".into(),
                            available: true,
                        },
                    ],
                }],
                ..AudioGraph::default()
            }),
            // Who records is part of the sound reading: the bar shows it, and
            // the session hands it to whoever draws the bar.
            uses: Mutex::new(vec![media::audio::capture::MediaUse {
                client_id: 61,
                pid: 4242,
                app_id: "so.tilo.meet".into(),
                label: "Meet".into(),
                binary: std::path::PathBuf::from("/usr/bin/meet"),
                kind: media::audio::capture::MediaKind::Camera,
            }]),
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
    fn read(&self) -> Read {
        Read {
            graph: self.graph.lock().expect("graph").clone(),
            uses: self.uses.lock().expect("uses").clone(),
        }
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
            for device in graph.sinks.iter_mut().chain(&mut graph.sources) {
                if device.id == node_id {
                    device.volume = Some(perceptual);
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

tilo_test_env::scenario! {
    fn the_sound_service_reads_the_graph_and_acts_on_it;

    phase a_person_drives_the_output_its_streams_and_the_input;
    // The volume and mute act on the output in use and land in the reading;
    // a step moves from where the volume reads now, raising it out of
    // silence brings the sound back, an application's stream is routed by
    // its node, an output is chosen from its child, and the input's level
    // and a sound card's profile are taken.
    {
        let fake = Fake::laptop();
        let mut service = service(&fake);
        let sound = read(&service);
        assert_eq!(
            sound["uses"],
            json!([{
                "client-id": 61,
                "pid": 4242,
                "app-id": "so.tilo.meet",
                "label": "Meet",
                "binary": "/usr/bin/meet",
                "kind": "camera"
            }])
        );
        assert_eq!(sound["output"], json!(40));
        assert_eq!(sound["input"], json!(50));
        assert_eq!(sound["hardware"][0]["label"], json!("Headset"));
        assert_eq!(sound["hardware"][0]["profile"], json!(1));
        assert_eq!(sound["streams"][0]["application"], json!("Music"));

        service
            .invoke(SOUND, "set-volume", &arguments(json!({"level": 0.8})))
            .expect("taken");
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

        // A quieter sound stays silent; turning it up is asking to hear it.
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
        assert_eq!(
            fake.calls()[3..],
            [
                "volume 40 0.70",
                "volume 40 0.75",
                "mute 40 false",
                "mute 40 true",
                "volume 40 0.90",
                "mute 40 false"
            ]
        );

        let refused = service.invoke(SOUND, "set-volume", &arguments(json!({"level": 3})));
        assert!(
            matches!(refused, Err(ApiError::InvalidArguments(_))),
            "{refused:?}"
        );
        assert_eq!(
            fake.calls().len(),
            9,
            "nothing out of range reached the graph"
        );

        // An application's stream is routed, made louder and muted by its node.
        for (action, params) in [
            ("set-stream-volume", json!({"stream": 70, "level": 0.3})),
            ("set-stream-muted", json!({"stream": 70, "muted": true})),
            ("route-stream", json!({"stream": 70, "device": 41})),
            ("route-stream", json!({"stream": 70, "device": null})),
        ] {
            let taken = service.invoke(SOUND, action, &arguments(params));
            assert_eq!(taken, Ok(Started::Accepted), "{action}");
            service.settle();
        }
        assert_eq!(
            fake.calls()[9..],
            ["volume 70 0.30", "mute 70 true", "route 70 41", "unroute 70"]
        );

        // An output is chosen from its child: it becomes the default and
        // every stream is released to follow the default.
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
        assert_eq!(fake.calls()[13..], ["default-sink 41", "unroute 70"]);
        let chosen = read(&service);
        assert_eq!(chosen["output"], json!(41));
        assert_eq!(chosen["outputs"][1]["chosen"], json!(true));
        assert_eq!(chosen["pending"], json!([]));

        service
            .invoke(
                SOUND,
                "set-input-volume",
                &arguments(json!({"level": 0.4})),
            )
            .expect("taken");
        service.settle();
        assert_eq!(read(&service)["input-volume"], json!(0.4_f32));
        service
            .invoke(
                SOUND,
                "set-profile",
                &arguments(json!({"device": 60, "profile": 2})),
            )
            .expect("taken");
        service.settle();
        assert_eq!(fake.calls()[15..], ["volume 50 0.40", "profile 60 2"]);

        let out_of_range =
            service.invoke(SOUND, "set-input-volume", &arguments(json!({"level": 3})));
        assert!(
            matches!(out_of_range, Err(ApiError::InvalidArguments(_))),
            "{out_of_range:?}"
        );
    }

    phase without_an_output_the_volume_is_unavailable;
    // Without an output, the volume is published unavailable and refused.
    {
        let fake = Arc::new(Fake {
            graph: Mutex::new(AudioGraph::default()),
            uses: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        });
        let mut service = service(&fake);
        for action in ["set-muted", "volume-up"] {
            let refused = service.invoke(SOUND, action, &arguments(json!({"muted": true})));
            assert!(
                matches!(
                    refused,
                    Err(ApiError::Unavailable { .. } | ApiError::InvalidArguments(_))
                ),
                "{action}: {refused:?}"
            );
        }
        assert!(fake.calls().is_empty());
    }
}
