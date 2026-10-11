//! Public callers must share the real singleton observer, not only work alone.

use std::collections::HashMap;
use std::io::BufRead;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use futures_util::StreamExt;
use shell::shell::mpris;
use shell::shell::now_playing::{PlaybackStatus, PlayerSnapshot};
use tokio::runtime::Runtime;
use zbus::zvariant::{OwnedValue, Value};

const CHILD_ENV: &str = "TILO_DBUS_SHARED_OBSERVER_BUS";
const TEST_NAME: &str =
    "dbus_runtime_context::shared::public_callers_share_one_observer_without_a_caller_runtime";
const PLAYER: &str = "org.mpris.MediaPlayer2.shared_probe";
const PLAYER_PATH: &str = "/org/mpris/MediaPlayer2";

#[test]
fn public_callers_share_one_observer_without_a_caller_runtime() {
    let Ok(address) = std::env::var(CHILD_ENV) else {
        let bus = PrivateBus::start();
        let log = bus.directory.path().join("child.stdout");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_ENV, &bus.address)
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .stdout(std::fs::File::create(&log).unwrap())
            .status()
            .unwrap();
        let stdout = std::fs::read_to_string(log).unwrap();
        assert!(
            status.success(),
            "shared public callers failed: {status}\n{stdout}"
        );
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "child ran no test: {stdout}"
        );
        return;
    };
    verify_shared_public_callers(&address);
}

fn verify_shared_public_callers(address: &str) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (commands, received) = mpsc::channel();
    let (player, calls) = runtime.block_on(tilo_test_env::within("private player setup", async {
        let calls = record_calls(address).await;
        let player = zbus::connection::Builder::address(address)
            .unwrap()
            .name(PLAYER)
            .unwrap()
            .serve_at(
                PLAYER_PATH,
                ProbeRoot {
                    identity: "Shared probe".into(),
                },
            )
            .unwrap()
            .serve_at(
                PLAYER_PATH,
                ProbePlayer {
                    status: "Playing".into(),
                    position_us: 12_000_000,
                    title: "Shared song".into(),
                    controllable: true,
                    commands,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        (player, calls)
    }));

    // All three public callers run together on this plain libtest thread.
    // The fixture runtime above serves only the private player and monitor.
    assert!(tokio::runtime::Handle::try_current().is_err());
    assert!(mpris::send(mpris::Command::PlayPause, |_| {}));
    assert!(mpris::pause_named(PLAYER));
    mpris::refresh_handoff_reports();
    mpris::refresh_now_playing();
    let first = tilo_test_env::recv("first actual player command", &received);
    let last = tilo_test_env::recv("second actual player command", &received);
    assert_ne!(first.0, last.0, "both public commands reached the player");
    assert!([first.0, last.0].contains(&"PlayPause"));
    assert!([first.0, last.0].contains(&"Pause"));
    let snapshot = wait_for_player(&last.1, 12_000_000);
    assert_eq!(snapshot.identity, "Shared probe");
    assert_eq!(snapshot.title.as_deref(), Some("Shared song"));
    assert!(snapshot.can_toggle());
    monitor_fence(&runtime, &player, &calls);
    assert_shared_seed_and_command_connection(&calls);
    verify_signals_and_idle_reuse(&runtime, &player, &calls, &last.1);

    let before_departure = production_calls(&calls);
    runtime
        .block_on(tilo_test_env::within(
            "player departure",
            player.release_name(PLAYER),
        ))
        .unwrap();
    tilo_test_env::wait_until("public observer sees player departure", || {
        mpris::now_playing_players().is_empty()
    });
    monitor_fence(&runtime, &player, &calls);
    assert_eq!(
        production_calls(&calls),
        before_departure,
        "departure is a signal, not another bus read"
    );
}

fn verify_signals_and_idle_reuse(
    runtime: &Runtime,
    player: &zbus::Connection,
    calls: &Calls,
    previous: &str,
) {
    let status = if previous == "Playing" {
        "Paused"
    } else {
        "Playing"
    };
    signal_status(runtime, player, status, 13_000_000);
    wait_for_player(status, 13_000_000);
    monitor_fence(runtime, player, calls);
    let startup = production_calls(calls);

    let status = if status == "Playing" {
        "Paused"
    } else {
        "Playing"
    };
    signal_status(runtime, player, status, 14_000_000);
    wait_for_player(status, 14_000_000);
    monitor_fence(runtime, player, calls);
    assert_eq!(
        production_calls(calls).get(startup.len()..),
        Some(&["Get".to_string()][..]),
        "a status signal re-reads only the stale playhead"
    );

    // Repeated public refreshes join the existing observer. Idle observation
    // must retain the signal-driven implementation's zero polling calls.
    for _ in 0..8 {
        mpris::refresh_handoff_reports();
        mpris::refresh_now_playing();
    }
    std::thread::sleep(Duration::from_millis(1_500));
    monitor_fence(runtime, player, calls);
    assert_eq!(
        production_calls(calls).get(startup.len()..),
        Some(&["Get".to_string()][..]),
        "shared public refreshes do not re-seed or poll an idle player"
    );
    assert_shared_seed_and_command_connection(calls);
}

fn wait_for_player(status: &str, position_us: i64) -> PlayerSnapshot {
    tilo_test_env::wait_for(
        "signal state reaching public now-playing projection",
        || {
            mpris::now_playing_players().into_iter().find(|player| {
                player.bus_name == PLAYER
                    && player.position_us == position_us
                    && player.status
                        == if status == "Playing" {
                            PlaybackStatus::Playing
                        } else {
                            PlaybackStatus::Paused
                        }
            })
        },
    )
}

fn signal_status(runtime: &Runtime, player: &zbus::Connection, status: &str, position_us: i64) {
    runtime.block_on(tilo_test_env::within("player property signal", async {
        let interface = player
            .object_server()
            .interface::<_, ProbePlayer>(PLAYER_PATH)
            .await
            .unwrap();
        {
            let mut state = interface.get_mut().await;
            state.status = status.into();
            state.position_us = position_us;
        }
        interface
            .get()
            .await
            .playback_status_changed(interface.signal_emitter())
            .await
            .unwrap();
    }));
}

#[derive(Clone, Debug)]
struct Call {
    sender: String,
    member: String,
}
type Calls = Arc<Mutex<Vec<Call>>>;

async fn record_calls(address: &str) -> Calls {
    let monitor = zbus::connection::Builder::address(address)
        .unwrap()
        .build()
        .await
        .unwrap();
    let mut messages = zbus::MessageStream::from(&monitor);
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::MethodCall)
        .build();
    zbus::fdo::MonitoringProxy::new(&monitor)
        .await
        .unwrap()
        .become_monitor(&[rule], 0)
        .await
        .unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&calls);
    drop(tokio::spawn(async move {
        let _monitor = monitor;
        while let Some(Ok(message)) = messages.next().await {
            if message.message_type() == zbus::message::Type::MethodCall
                && let Some(member) = message.header().member()
                && let Some(sender) = message.header().sender()
            {
                recorded.lock().unwrap().push(Call {
                    sender: sender.to_string(),
                    member: member.to_string(),
                });
            }
        }
    }));
    calls
}

fn monitor_fence(runtime: &Runtime, player: &zbus::Connection, calls: &Calls) {
    let sender = player.unique_name().unwrap().as_str();
    let count = || {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| call.sender == sender && call.member == "GetId")
            .count()
    };
    let before = count();
    runtime.block_on(tilo_test_env::within("monitor fence", async {
        zbus::fdo::DBusProxy::new(player)
            .await
            .unwrap()
            .get_id()
            .await
            .unwrap();
    }));
    tilo_test_env::wait_until("private monitor has consumed the fence", || {
        count() > before
    });
}

fn assert_shared_seed_and_command_connection(calls: &Calls) {
    let calls = calls.lock().unwrap().clone();
    let seeds: Vec<_> = calls
        .iter()
        .filter(|call| call.member == "ListNames")
        .collect();
    assert_eq!(
        seeds.len(),
        1,
        "one singleton observer lists the bus: {calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|call| call.member == "GetAll").count(),
        2,
        "the shared observer reads both interfaces once: {calls:?}"
    );
    for method in ["PlayPause", "Pause"] {
        let command: Vec<_> = calls.iter().filter(|call| call.member == method).collect();
        assert_eq!(command.len(), 1, "one actual {method} call: {calls:?}");
        assert_eq!(
            command[0].sender, seeds[0].sender,
            "public commands reuse the observer's call connection"
        );
    }
}

fn production_calls(calls: &Calls) -> Vec<String> {
    let calls = calls.lock().unwrap();
    let sender = &calls
        .iter()
        .find(|call| call.member == "ListNames")
        .unwrap()
        .sender;
    calls
        .iter()
        .filter(|call| &call.sender == sender)
        .map(|call| call.member.clone())
        .collect()
}

struct ProbeRoot {
    identity: String,
}
#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl ProbeRoot {
    #[zbus(property)]
    fn identity(&self) -> &str {
        &self.identity
    }
}

struct ProbePlayer {
    status: String,
    position_us: i64,
    title: String,
    controllable: bool,
    commands: mpsc::Sender<(&'static str, String)>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl ProbePlayer {
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.status.clone()
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        self.position_us
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.controllable
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.controllable
    }
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        HashMap::from([(
            "xesam:title".into(),
            OwnedValue::try_from(Value::from(self.title.clone())).unwrap(),
        )])
    }
    async fn play_pause(
        &mut self,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.status = if self.status == "Playing" {
            "Paused"
        } else {
            "Playing"
        }
        .into();
        self.playback_status_changed(&emitter).await?;
        self.commands
            .send(("PlayPause", self.status.clone()))
            .unwrap();
        Ok(())
    }
    async fn pause(
        &mut self,
        #[zbus(signal_emitter)] emitter: zbus::object_server::SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        "Paused".clone_into(&mut self.status);
        self.playback_status_changed(&emitter).await?;
        self.commands.send(("Pause", self.status.clone())).unwrap();
        Ok(())
    }
}

struct PrivateBus {
    daemon: std::process::Child,
    address: String,
    directory: tempfile::TempDir,
}
impl PrivateBus {
    fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                directory.path().join("bus").display()
            ))
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("private bus requires the builder's dbus-daemon");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(
            !address.trim().is_empty(),
            "private daemon printed no address"
        );
        Self {
            daemon,
            address: address.trim().into(),
            directory,
        }
    }
}
impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}
