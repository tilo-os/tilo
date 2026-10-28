//! How long [`ShellNetwork::frame_tick`] holds the compositor's loop with
//! three of one person's sessions connected over real Iroh endpoints, each
//! running its device fabric on its own thread, while their desktops keep
//! changing and one session restarts and rejoins.
//!
//! A measurement: run it with
//! `cargo test -p shell network_pump_under_load -- --ignored --nocapture`
//! and read the `NETWORK-PUMP-LOAD` line and the `NETWORK-PUMP-PHASE` lines,
//! one per step of the two surviving sessions' turns.

use super::*;
use crate::shell::device_fabric_runtime::{FabricTurn, spawn_worker};
use std::time::{Duration, Instant};

const TURNS: usize = 400;
const RESTART_AT: usize = 150;

/// A desktop of the person's own workspaces, which their other devices
/// join and keep in step with.
fn desktop(root: &ed25519_dalek::SigningKey) -> WorkspaceTree {
    let root = root.verifying_key().to_bytes();
    let person = crate::shell::state::person_id_of(root).unwrap();
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.local_person = Some(root);
    for workspace in &mut tree.desktop.workspaces {
        workspace.set_owner(person.clone());
    }
    tree.seed_doc_from_cache();
    tree
}

async fn session(root: &ed25519_dalek::SigningKey, tag: u8, state: &Path) -> Session {
    let mut session = Box::pin(Session::new(root, tag, desktop(root))).await;
    let device = session.cert.device_public_key;
    let network = session.network_mut();
    network.personal_continuity_path = Some(state.join(format!("continuity-{tag}.postcard")));
    network.personal_workspaces = Some(Mutex::new(
        crate::shell::personal_workspaces::PersonalWorkspaces::new(
            root.verifying_key().to_bytes(),
            device,
        ),
    ));
    session.network.start_peer_sync_worker();
    session
}

fn turn(selection: &SharedSelection) -> FabricTurn {
    FabricTurn {
        selection: selection.clone(),
        local: LocalFabricCapabilities {
            display: None,
            pointer: false,
            keyboard: false,
            audio_output: false,
        },
        app_focus: None,
        realtime_adapter: None,
        remote_capture: None,
    }
}

/// A turn's local change: the first group with tabs to spare rotates them,
/// so the desktop keeps its size while its layout keeps changing.
fn move_a_tab(network: &ShellNetwork) {
    let Some(tree) = network
        .replica
        .lock()
        .ok()
        .map(|replica| replica.tree.clone())
    else {
        return;
    };
    let mut tree = tree.write();
    let Some(group) = tree
        .desktop
        .workspaces
        .iter_mut()
        .flat_map(|workspace| workspace.strips.iter_mut())
        .flat_map(|strip| strip.window_groups.iter_mut())
        .find(|group| group.tabs.len() > 1)
    else {
        return;
    };
    group.tabs.rotate_left(1);
    tree.seed_doc_from_cache();
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let last = sorted.len().saturating_sub(1);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an index into a short sample list"
    )]
    let index = ((last as f64) * fraction).round() as usize;
    sorted[index.min(last)]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial]
#[ignore = "measurement; run with --ignored --nocapture"]
#[allow(
    clippy::significant_drop_tightening,
    reason = "every session runs to the end of the measurement, which closes each"
)]
async fn network_pump_under_load() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    home.env().set(
        "DBUS_SESSION_BUS_ADDRESS",
        "unix:path=/nonexistent/tilo-continuity-test",
    );
    let paths = settings_store::SessionPaths::beside(home.config().join("settings.json"));
    let _settings = crate::shell::test_support::install_settings(Arc::new(
        settings_store::SessionSettings::open(paths).unwrap(),
    ));
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target");
    std::fs::create_dir_all(&target).unwrap();
    let state = tempfile::tempdir_in(&target).unwrap();
    let root = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
    let mut sessions = vec![
        session(&root, 0x62, state.path()).await,
        session(&root, 0x63, state.path()).await,
        session(&root, 0x64, state.path()).await,
    ];
    pair(&sessions[0], &sessions[1]).await;
    pair(&sessions[0], &sessions[2]).await;
    pair(&sessions[1], &sessions[2]).await;
    let selections: Vec<SharedSelection> = (0..3).map(|_| selection()).collect();
    let mut fabrics: Vec<_> = sessions
        .iter()
        .map(|session| {
            spawn_worker(
                Arc::clone(&session.network),
                crate::notifications::NotificationBridge::new(),
            )
            .unwrap()
        })
        .collect();

    let mut samples = Vec::with_capacity(TURNS * 2);
    let mut phases = std::collections::HashMap::<&str, (u32, Duration, Duration)>::new();
    for step in 0..TURNS {
        if step == RESTART_AT {
            // The third session restarts: a new process, same device.
            drop(fabrics.pop());
            let mut gone = sessions.pop().unwrap();
            gone.close().await;
            sessions.push(session(&root, 0x64, state.path()).await);
            pair(&sessions[0], &sessions[2]).await;
            pair(&sessions[1], &sessions[2]).await;
            fabrics.push(
                spawn_worker(
                    Arc::clone(&sessions[2].network),
                    crate::notifications::NotificationBridge::new(),
                )
                .unwrap(),
            );
        }
        move_a_tab(&sessions[step % 3].network);
        for (index, session) in sessions.iter().enumerate() {
            let started = Instant::now();
            let steps = session.network.timed_frame_tick();
            let elapsed = started.elapsed();
            if index < 2 {
                samples.push(elapsed);
                for (name, took) in steps {
                    let phase = phases.entry(name).or_default();
                    phase.0 = phase.0.saturating_add(1);
                    phase.1 = phase.1.saturating_add(took);
                    phase.2 = phase.2.max(took);
                }
            }
            fabrics[index].tick(turn(&selections[index]));
        }
        tokio::time::sleep(Duration::from_millis(8)).await;
    }
    samples.sort_unstable();
    println!(
        "NETWORK-PUMP-LOAD ticks={} p50_us={} p90_us={} p99_us={} max_us={}",
        samples.len(),
        percentile(&samples, 0.5).as_micros(),
        percentile(&samples, 0.9).as_micros(),
        percentile(&samples, 0.99).as_micros(),
        samples.last().map_or(0, Duration::as_micros),
    );
    let mut phases: Vec<_> = phases.into_iter().collect();
    phases.sort_by_key(|(_, (_, total, _))| std::cmp::Reverse(*total));
    for (name, (count, total, max)) in phases {
        println!(
            "NETWORK-PUMP-PHASE {name} count={count} total_us={} max_us={}",
            total.as_micros(),
            max.as_micros()
        );
    }
    drop(fabrics);
    for session in &mut sessions {
        session.close().await;
    }
}
