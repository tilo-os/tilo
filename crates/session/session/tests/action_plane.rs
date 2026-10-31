//! An app process reads, watches and invokes what the session's services
//! publish, over the one wire it already speaks to the session: the socket
//! the launcher names the app on, for the sound service's `sound` resource.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

use std::sync::{Arc, Mutex};

use media::audio::{AudioDevice, AudioGraph, GraphControl};
use runtime_collab::CollabClient;
use runtime_collab::wire::Receipt;
use serde_json::{Map, Value, json};
use tilo_audio_service::sound::{Graph, SoundService};
use tilo_compositor::OutputConfig;
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Options, Service, Session};

/// A sound card in memory: one output at 40%, in use.
struct Card {
    graph: Mutex<AudioGraph>,
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

/// The session the app talks to: headless, with the sound service hosting a
/// card in memory, and the plane on a socket of its own.
fn session(socket: &std::path::Path) -> Session {
    let wake = LoopWake::default();
    let card: Arc<dyn Graph> = Arc::new(Card {
        graph: Mutex::new(AudioGraph {
            sinks: vec![AudioDevice {
                id: 40,
                serial: 40,
                name: "node-40".into(),
                description: "Speakers".into(),
                is_default: true,
                volume: Some(0.4),
                muted: Some(false),
                channels: 2,
                codec: None,
            }],
            ..AudioGraph::default()
        }),
    });
    let sound = SoundService::new(card, Some(wake.handle())).expect("a sound service");
    let services: Vec<Box<dyn Service>> = vec![Box::new(sound)];
    // The app speaks from this test's own process, so the launch the session
    // would have recorded for it names that process: what the broker admits
    // the connection by.
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    launches.started(std::process::id(), "so.tilo.settings", "the-workspace");
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services,
            collab: false,
            wake,
            launches,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session")
}

/// What the app saw, in the order it saw it.
struct Seen {
    snapshot: Option<String>,
    watched: Option<String>,
    receipt: Receipt,
    changed: String,
}

/// The app's side of the wire: the launcher's handoff on a connection to
/// `socket`, then what it reads, watches and invokes of the session's sound.
fn app(socket: &std::path::Path, seen: &std::sync::mpsc::Sender<Seen>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let mut stream =
            std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
        launcher::services::write_collab_handoff(&mut stream, "so.tilo.settings", "the-workspace")
            .expect("the launcher names the app on the connection");
        stream
            .set_nonblocking(true)
            .expect("the connection is ready for the runtime");
        let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
        let (reader, writer) = stream.into_split();
        let (client, carrying) = CollabClient::over(reader, writer);
        tokio::spawn(carrying);
        let services = client.services();

        let snapshot = services
            .snapshot("sound")
            .await
            .expect("the session reads its sound")
            .reading;
        let readings = services
            .watch("sound")
            .await
            .expect("the session watches it");
        let watched = readings
            .read()
            .await
            .expect("the reading the watch begins at")
            .reading;
        let mut arguments = Map::new();
        arguments.insert("level".into(), json!(0.7));
        let receipt = services
            .invoke("sound", "set-volume", &arguments)
            .await
            .expect("the session invokes the action");
        // The reading moves when the action is taken and again when the
        // device has landed it: the app reads until the card is at 70%.
        let mut changed = None;
        for _ in 0..500 {
            let reading = readings
                .read()
                .await
                .expect("the readings the change lands in")
                .reading;
            let volume = reading
                .as_deref()
                .map(|reading| reading_of(reading)["volume"].clone());
            if volume == Some(json!(0.7_f32)) {
                changed = reading;
                break;
            }
        }
        let _ = seen.send(Seen {
            snapshot,
            watched,
            receipt,
            changed: changed.expect("the volume change reaches the app"),
        });
    });
}

/// The session's pass until the app is done, as the session runs its loop.
fn until_the_app_is_done(session: &mut Session, seen: &std::sync::mpsc::Receiver<Seen>) -> Seen {
    for _ in 0..4000 {
        session.settle_services();
        session.step(std::time::Duration::from_millis(1)).unwrap();
        if let Ok(seen) = seen.try_recv() {
            return seen;
        }
    }
    panic!("the app never finished its calls");
}

fn reading_of(reading: &str) -> Value {
    serde_json::from_str(reading).expect("a reading is JSON")
}

/// A directory for the plane's socket that only this user reaches: the
/// broker refuses to serve one anybody else could open.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    root
}

#[test]
fn an_app_reads_watches_and_invokes_the_sound_the_session_publishes() {
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let mut session = session(&socket);

    let (seen, received) = std::sync::mpsc::channel();
    let app = std::thread::spawn(move || app(&socket, &seen));
    let seen = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");

    let snapshot = reading_of(seen.snapshot.as_deref().expect("a reading"));
    assert_eq!(snapshot["output"], json!(40), "the card the session hosts");
    assert_eq!(snapshot["volume"], json!(0.4_f32), "as the card stands");
    assert_eq!(
        reading_of(seen.watched.as_deref().expect("the watch's first reading")),
        snapshot,
        "a watch begins where the resource stands"
    );
    assert!(
        matches!(seen.receipt, Receipt::Accepted),
        "the action is taken on the device: {:?}",
        seen.receipt
    );
    let changed = reading_of(&seen.changed);
    assert_eq!(
        changed["volume"],
        json!(0.7_f32),
        "the change reaches the app"
    );
    let published = session.read("sound").expect("the session publishes sound");
    assert_eq!(
        published["volume"],
        json!(0.7_f32),
        "and it is the device's change, not the wire's"
    );
}

#[test]
fn an_app_reads_what_the_session_does_not_publish_as_nothing() {
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let mut session = session(&socket);

    let (seen, received) = std::sync::mpsc::channel();
    let app = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the app");
        runtime.block_on(async {
            let mut stream =
                std::os::unix::net::UnixStream::connect(&socket).expect("the plane's socket");
            launcher::services::write_collab_handoff(
                &mut stream,
                "so.tilo.settings",
                "the-workspace",
            )
            .expect("the launcher names the app on the connection");
            stream
                .set_nonblocking(true)
                .expect("the connection is ready for the runtime");
            let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
            let (reader, writer) = stream.into_split();
            let (client, carrying) = CollabClient::over(reader, writer);
            tokio::spawn(carrying);
            let snapshot = client
                .services()
                .snapshot("nowhere")
                .await
                .expect("the session answers a read of what it does not publish");
            let _ = seen.send(Seen {
                snapshot: snapshot.reading.is_none().then(String::new),
                watched: None,
                receipt: Receipt::Accepted,
                changed: String::new(),
            });
        });
    });
    let seen = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");

    assert_eq!(
        seen.snapshot.as_deref(),
        Some(""),
        "a resource the session does not publish reads as nothing"
    );
}

#[test]
fn an_app_that_invokes_what_the_session_cannot_do_is_told_why() {
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let mut session = session(&socket);

    let (seen, received) = std::sync::mpsc::channel();
    let app = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the app");
        runtime.block_on(async {
            let mut stream =
                std::os::unix::net::UnixStream::connect(&socket).expect("the plane's socket");
            launcher::services::write_collab_handoff(
                &mut stream,
                "so.tilo.settings",
                "the-workspace",
            )
            .expect("the launcher names the app on the connection");
            stream
                .set_nonblocking(true)
                .expect("the connection is ready for the runtime");
            let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
            let (reader, writer) = stream.into_split();
            let (client, carrying) = CollabClient::over(reader, writer);
            tokio::spawn(carrying);
            let mut arguments = Map::new();
            arguments.insert("level".into(), json!("loud"));
            let receipt = client
                .services()
                .invoke("sound", "set-volume", &arguments)
                .await
                .expect("the session answers the invocation");
            let _ = seen.send(Seen {
                snapshot: None,
                watched: None,
                receipt,
                changed: String::new(),
            });
        });
    });
    let seen = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");

    match seen.receipt {
        Receipt::Refused { code, message } => {
            assert!(!code.is_empty(), "a refusal names what it refused with");
            assert!(!message.is_empty(), "and says why: {code}");
        }
        other => panic!("a volume of \"loud\" is refused: {other:?}"),
    }
}
