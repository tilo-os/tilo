//! How long [`ShellNetwork::frame_tick`] holds the compositor's loop while the
//! desktop keeps changing: every turn moves a tab, so each tick has room
//! changes to announce and, at every upkeep, a new continuity checkpoint to
//! make durable. The store lives on a real disk (under the target
//! directory, not a tmpfs), so its fsyncs cost what they cost.
//!
//! A measurement, not a check: run it with
//! `cargo test -p shell network_tick_latency -- --ignored --nocapture`
//! and compare the `NETWORK-TICK-BENCH` line across revisions.

use super::{NetCmd, shell_network_for_test};
use crate::shell::state::{AppKind, Tab, TileContent, WorkspaceTree};
use std::time::{Duration, Instant};

const TICKS: usize = 200;

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

#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn network_tick_latency_while_the_desktop_changes() {
    let target = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target");
    std::fs::create_dir_all(&target).expect("target directory");
    let dir = tempfile::tempdir_in(&target).expect("store directory on disk");
    let tree = WorkspaceTree::bootstrap_demo().shared();
    let (mut net, mut commands) = shell_network_for_test(tree.clone(), 0xBE7C);
    net.personal_continuity_user = Some([0x5A; 32]);
    net.personal_continuity_device =
        Some(crate::shell::personal_continuity::DeviceId::new("bench-device").expect("device id"));
    net.personal_continuity_path = Some(dir.path().join("personal-continuity.postcard"));
    net.continuity_policy_override = Some(
        crate::shell::continuity_policy::ContinuityPolicySnapshot::from_document(
            serde_json::json!({ "continuity": { "enabled": true, "app_continuations": true } })
                .as_object()
                .expect("policy object"),
        ),
    );
    net.frame_tick();

    let mut samples = Vec::with_capacity(TICKS);
    for turn in 0..TICKS {
        {
            let mut tree = tree.write();
            let room = tree.rooms()[0];
            let workspace = tree
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
                .expect("room workspace");
            let group = workspace
                .strips
                .iter_mut()
                .flat_map(|strip| strip.window_groups.iter_mut())
                .next()
                .expect("a window group");
            let id = tilo_desktop_model::compose_id(0xBE7C, 1000 + u64::try_from(turn).unwrap());
            group.tabs.push(Tab {
                id,
                title: turn.to_string(),
                tile: TileContent::Stub {
                    kind: AppKind::Other("so.tilo.notes".into()),
                    label: String::new(),
                },
            });
            tree.seed_doc_from_cache();
        }
        // Every turn is an upkeep turn, as one is every two seconds.
        *net.last_sync_kick.lock().expect("sync kick") = None;
        let started = Instant::now();
        net.frame_tick();
        samples.push(started.elapsed());
        while let Ok(command) = commands.try_recv() {
            let _: NetCmd = command;
        }
        // What the tick handed off finishes outside the measured window, as
        // it would between two upkeep rounds.
        crate::shell::persist::wait_for_saves();
    }
    samples.sort_unstable();
    println!(
        "NETWORK-TICK-BENCH ticks={} p50_us={} p90_us={} p99_us={} max_us={}",
        samples.len(),
        percentile(&samples, 0.5).as_micros(),
        percentile(&samples, 0.9).as_micros(),
        percentile(&samples, 0.99).as_micros(),
        samples.last().map_or(0, Duration::as_micros),
    );
}
