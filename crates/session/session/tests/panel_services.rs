//! A panel the session starts is an ordinary app: it reaches what the
//! session's services publish the way Settings does, as the launch the session
//! recorded for it.
//!
//! These start a surface app through the session's own surface machinery — the
//! same call that starts Quick Settings and the top bar — and let it read and
//! change the sound service's `sound` resource. It is admitted only because the
//! session started it: a process of the same app that the session did not start
//! reaches nothing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it starts is done"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use media::audio::{AudioDevice, AudioGraph, GraphControl};
use serde_json::{Value, json};
use tilo_audio_service::sound::{Graph, SoundService};
use tilo_compositor::OutputConfig;
use tilo_service::LoopWake;
use tilo_session::surfaces::Surface;
use tilo_session::{ActionPlane, Options, Service, Session};
use tilo_test_env::ScopedEnv;

/// How long the panel may take to report what it read.
const REPORTING: Duration = Duration::from_secs(90);
/// One display's refresh: how often the session asks its clients for their
/// next frame.
const REFRESH: Duration = Duration::from_millis(16);

/// A sound card in memory: one output at 40%.
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

/// The sound service over the card in memory, as the session hosts it.
fn sound(wake: &LoopWake) -> Vec<Box<dyn Service>> {
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
    vec![Box::new(sound)]
}

/// The panel's binary: the probe is the session package's own, built beside
/// it.
fn panel_binary() -> PathBuf {
    let binary =
        tilo_test_env::package_binary!("tilo-session").with_file_name("tilo-surface-probe");
    assert!(
        binary.exists(),
        "expected the panel at {}; build the workspace first",
        binary.display()
    );
    binary
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

/// The session the panel talks to: headless, hosting the sound service, with
/// its plane on `socket` and the panel started the way it starts Quick
/// Settings.
fn session_with_panel(socket: &Path) -> Session {
    let wake = LoopWake::default();
    let services: Vec<Box<dyn Service>> = sound(&wake);
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services,
            collab: false,
            wake,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            surfaces: vec![Surface::new("probe", panel_binary())],
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session")
}

/// A session with the sound service and its plane, starting no panel.
fn session_alone(socket: &Path) -> Session {
    let wake = LoopWake::default();
    let services: Vec<Box<dyn Service>> = sound(&wake);
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            services,
            collab: false,
            wake,
            action_plane: ActionPlane::At(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
    )
    .expect("session")
}

/// The session's turns until the panel has reported, as the session runs its
/// loop.
fn until_reported(session: &mut Session, report: &Path) -> String {
    let deadline = Instant::now() + REPORTING;
    loop {
        session.settle_services();
        session.step(REFRESH).expect("a turn of the session");
        if let Ok(report) = std::fs::read_to_string(report) {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "the panel reported nothing: {:?}",
            session.surfaces().names()
        );
    }
}

/// The reading named on the line that starts with `prefix`.
fn reading(report: &str, prefix: &str) -> Value {
    let line = report
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("the panel reported no `{prefix}`: {report}"));
    serde_json::from_str(line).expect("a reading is JSON")
}

#[test]
fn a_panel_the_session_starts_reads_and_changes_what_its_services_publish() {
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let mut environment = ScopedEnv::new();
    // What the panel is to read and change, beside what the session writes on
    // its environment when it starts it.
    environment.set("SURFACE_PROBE_REPORT", &report);
    environment.set("SURFACE_PROBE_RESOURCE", "sound");
    environment.set("SURFACE_PROBE_ACTION", "set-volume");
    environment.set("SURFACE_PROBE_ARGUMENTS", r#"{"level":0.7}"#);
    let mut session = session_with_panel(&socket);

    let report = until_reported(&mut session, &report);

    let first = reading(&report, "read: ");
    assert_eq!(first["output"], json!(40), "the card the session hosts");
    assert_eq!(first["volume"], json!(0.4_f32), "as the card stands");
    assert!(
        report.lines().any(|line| line == "taken"),
        "the panel's action on the session's sound is taken: {report}"
    );
    let changed = reading(&report, "changed: ");
    assert_eq!(
        changed["volume"],
        json!(0.7_f32),
        "the change reaches the panel as it is drawn: {report}"
    );
    let published = session.read("sound").expect("the session publishes sound");
    assert_eq!(
        published["volume"],
        json!(0.7_f32),
        "and it is the device's change, not the wire's"
    );
}

#[test]
fn a_process_the_session_did_not_start_reaches_no_service() {
    let root = private_home();
    let socket = root.path().join("collab.sock");
    let report = root.path().join("report");
    let mut session = session_alone(&socket);

    // The same panel binary, by the same app id, in the same room, on the same
    // socket — started by anyone but the session, so no launch of the
    // session's names its process.
    let launched_in = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.room().to_string())
        })
        .expect("the session shows a workspace");
    let mut panel = std::process::Command::new(panel_binary())
        .env("SURFACE_PROBE_REPORT", &report)
        .env("SURFACE_PROBE_RESOURCE", "sound")
        .env("TILO_SURFACE_ACTIONS_FD", "3")
        .env("TILO_SURFACE_APP_ID", "so.tilo.probe")
        .env("TILO_SURFACE_WORKSPACE", &launched_in)
        .env("TILO_SURFACE_COLLAB_SOCKET", &socket)
        .spawn()
        .expect("a panel outside the session");
    let said = until_reported(&mut session, &report);
    let _ = panel.wait();

    assert!(
        said.starts_with("error: ") && said.contains("launch"),
        "a process the session did not start reaches no service, for want of the \
         launch that would name it: {said}"
    );
}
