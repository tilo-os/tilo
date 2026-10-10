//! What a converged, idle two-desktop session costs on the wire: two devices
//! of one person share a workspace room (and the person's own continuity
//! topic), one of them with its pointer resting over a shared tab. Both run
//! their network tick and pointer presence the way the compositor's frame
//! loop does, ten times a second, as a loop woken by something unrelated (a
//! clock, a blinking caret) would. Nobody does anything for a minute, so
//! nothing needs saying: every application message either device receives
//! in that minute, and every repaint a peer's pointer causes, is waste.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use ui::presence::PresenceCtx;

use super::{HostIdentity, NetCmd, Reach, ShellNetwork};
use crate::shell::peer_presence::{Desk, PeerPresence};
use crate::shell::state::{RoomId, WorkspaceTree, WorkspaceTreeHandle};

/// How often the simulated frame loop runs.
const FRAME: Duration = Duration::from_millis(100);
/// How long the session runs after converging before it is measured, so the
/// join's own catch-up and the pointer's last move are behind it.
const SETTLE: Duration = Duration::from_secs(10);
/// The idle stretch measured.
const IDLE: Duration = Duration::from_secs(60);
/// Most context switches per second the two desktops' own threads (their
/// network actors, endpoints and drains; not the thread driving their frame
/// loops) may make while idle, together.
///
/// Measured 9.9/s, 10.5/s and 11.6/s on CI (focused runs 89705, 89410,
/// 89703) while gossip swept its caches every second; 6.9/s, 7.0/s and
/// 7.5/s on the builder (11.9/s before) once it sweeps every 10 s. What
/// remains is the two network runtimes' workers (`tilo-net-rt-*`) over both
/// desktops, mostly the transport's 5 s keep-alives, everything else under
/// 0.1/s.
///
/// The gate runs in the pull-request shell shard, which measures more than a
/// focused runner on the same code: 15.18 to 16.80/s across five PRs before
/// the sweep change, the extra being voluntary switches, not preemptions
/// (TIL-690, `docs/benchmarks/idle-wakeups-2026-10-09.md`). The sweep change
/// removes about 4.9/s, so the shard's p95 is now near 11.9/s; the budget is
/// that plus a quarter. Lower it with each fix that removes a waker (TIL-702).
const IDLE_WAKEUP_BUDGET: f64 = 15.0;

/// How long a lone desktop runs after it starts before it is measured, so
/// its endpoint and its room's topic are up, and how long it is measured.
const LONE_SETTLE: Duration = Duration::from_secs(15);
const LONE_IDLE: Duration = Duration::from_secs(20);
/// Most context switches per second a lone desktop's own threads may make
/// while idle: one room of its own, no peer, and no frame loop driving it,
/// as on a desktop nobody touches, whose compositor never ticks the network
/// (TIL-702's target, under 2/s).
///
/// Measured on CI (focused, loopback): 2.79/s while gossip swept the room's
/// topic every second (run 91402), 0.95/s and 1.05/s once it sweeps every
/// 10 s (runs 91459, 91528). The rest is the 5 s upkeep tick and the sweep.
const LONE_WAKEUP_BUDGET: f64 = 2.0;

/// Every thread of this process except the one driving the desktops'
/// frame loops, whose 10 Hz is the simulation's and not the session's.
fn session_threads(driver: Option<u32>) -> Vec<tilo_test_env::idle::ThreadSample> {
    let mut threads = tilo_test_env::idle::sample_process(std::process::id())
        .expect("sample this process's threads");
    threads.retain(|thread| Some(thread.tid) != driver);
    threads
}

fn device(
    tree: WorkspaceTreeHandle,
    dir: &std::path::Path,
    root: &SigningKey,
    capabilities: DeviceCapabilities,
) -> Arc<ShellNetwork> {
    let root = root.clone();
    Arc::new(
        ShellNetwork::spawn_with(
            tree,
            &dir.join("device-private/keypair.bin"),
            &dir.join("peer-sync"),
            Reach::Loopback,
            move |secret, _| {
                let key = SigningKey::from_bytes(&secret.to_bytes());
                let mut cert = DeviceCert {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: key.verifying_key().to_bytes(),
                    device_label: "Device".into(),
                    capabilities,
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                sign_device_cert(&mut cert, &root).unwrap();
                Some(HostIdentity {
                    device_cert: cert,
                    device_signing_key: key,
                    workspace_device_certs: std::collections::HashMap::new(),
                    workspace_revocations: std::collections::HashMap::new(),
                    identity_paths: None,
                })
            },
        )
        .unwrap(),
    )
}

/// One desktop as its frame loop drives it.
struct Desktop {
    net: Arc<ShellNetwork>,
    tree: WorkspaceTreeHandle,
    presence: PeerPresence,
    ctx: PresenceCtx,
    desk: Desk,
    repaints: u64,
}

impl Desktop {
    fn new(net: Arc<ShellNetwork>, tree: WorkspaceTreeHandle) -> Self {
        let presence = PeerPresence::for_network(&net, Vec::new());
        Self {
            net,
            tree,
            presence,
            ctx: PresenceCtx::default(),
            // Both show the room's one shared tab in the same place.
            desk: Desk {
                tiles: vec![(SHARED_TAB, vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0))],
                pointer: None,
            },
            repaints: 0,
        }
    }

    /// One frame: the network tick, then pointer presence, as the native
    /// compositor runs them.
    fn frame(&mut self) {
        self.net.tick();
        if self.presence.frame(&mut self.ctx, &self.desk, &self.tree) {
            self.repaints = self.repaints.saturating_add(1);
        }
    }
}

const SHARED_TAB: u64 = 7;

fn workspace_name(tree: &WorkspaceTreeHandle, room: RoomId) -> Option<String> {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .map(|workspace| workspace.name.clone())
}

/// Run both desktops' frame loops for `span`, or until `done` holds.
fn run(
    desktops: &mut [&mut Desktop],
    span: Duration,
    mut done: impl FnMut(&[&mut Desktop]) -> bool,
) -> bool {
    let started = Instant::now();
    let deadline = started.checked_add(span).unwrap_or(started);
    loop {
        for desktop in desktops.iter_mut() {
            desktop.frame();
        }
        if done(&*desktops) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(FRAME);
    }
}

fn received(net: &ShellNetwork) -> BTreeMap<&'static str, u64> {
    net.wire_tally()
        .expect("the actor answers for its tally")
        .snapshot()
}

/// What arrived between two tallies, by kind.
fn since(
    before: &BTreeMap<&'static str, u64>,
    after: &BTreeMap<&'static str, u64>,
) -> BTreeMap<&'static str, u64> {
    after
        .iter()
        .filter_map(|(kind, count)| {
            let delta = count.saturating_sub(before.get(kind).copied().unwrap_or(0));
            (delta > 0).then_some((*kind, delta))
        })
        .collect()
}

fn room_change_count(tree: &WorkspaceTreeHandle, room: RoomId) -> usize {
    tree.write()
        .room_docs
        .get_mut(&room)
        .map_or(0, |doc| doc.get_changes(&[]).len())
}

/// What `room`'s document gained on `tree` since it held `before` changes:
/// the newest few, so a report names what an idle desktop keeps writing.
fn newest_room_changes(tree: &WorkspaceTreeHandle, room: RoomId, before: usize) -> String {
    let Some(changes) = tree
        .write()
        .room_docs
        .get_mut(&room)
        .map(|doc| doc.get_changes(&[]))
    else {
        return "none".into();
    };
    let mut out = format!(
        "{} changes while idle",
        changes.len().saturating_sub(before)
    );
    for change in changes.iter().skip(before).rev().take(3) {
        let change = change.decode();
        let ops: String = serde_json::to_string(&change.operations)
            .unwrap_or_default()
            .chars()
            .take(800)
            .collect();
        let _ = write!(
            out,
            "\n    by {} at {}: {ops}",
            change.actor_id, change.time
        );
    }
    out
}

#[test]
#[serial_test::serial]
fn a_converged_idle_pair_exchanges_no_messages_and_repaints_nothing_for_a_minute() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[43; 32]);
    let person = match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    };
    home.env().set("TILO_USER_ID", &person);

    let mut first_tree = WorkspaceTree::default();
    let home_workspace = first_tree.append_empty_workspace("Home");
    first_tree.adopt_ownerless_workspaces(&person);
    // The pointer is said in the room on screen.
    first_tree.desktop.active = home_workspace;
    let room = first_tree.room_of(home_workspace).unwrap();
    let first_tree = first_tree.shared();
    let first_net = device(
        first_tree.clone(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
    );
    assert!(first_net.join_default_workspace());
    let mut second_tree = WorkspaceTree::default();
    assert!(second_tree.join_enrolled_room(room));
    let second_tree = second_tree.shared();
    let second_net = device(
        second_tree.clone(),
        &dir.path().join("second"),
        &root,
        DeviceCapabilities::member(),
    );
    assert!(second_net.join_default_workspace());

    let mut first = Desktop::new(Arc::clone(&first_net), first_tree);
    let mut second = Desktop::new(Arc::clone(&second_net), second_tree.clone());
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    let first_addr = loop {
        if let Some(addr) = first_net.dial_addr() {
            break addr;
        }
        assert!(Instant::now() < deadline, "the first device never listened");
        std::thread::sleep(Duration::from_millis(20));
    };
    second_net
        .cmd_tx
        .try_send(NetCmd::Connect { addr: first_addr })
        .unwrap();
    assert!(
        run(
            &mut [&mut first, &mut second],
            tilo_test_env::hang_bound(),
            |_| { workspace_name(&second_tree, room).as_deref() == Some("Home") }
        ),
        "the two desktops never converged on the shared room"
    );

    // The first desktop's person points at the shared tab, the second sees
    // it arrive, and then the hand comes off the mouse.
    first.desk.pointer = Some((120.0, 80.0));
    let before = second.repaints;
    assert!(
        run(
            &mut [&mut first, &mut second],
            tilo_test_env::hang_bound(),
            |desktops| desktops[1].repaints > before,
        ),
        "the peer never drew the pointer"
    );
    let _ = run(&mut [&mut first, &mut second], SETTLE, |_| false);

    let first_before = received(&first_net);
    let second_before = received(&second_net);
    let repaints_before = (first.repaints, second.repaints);
    let room_changes_before = (
        room_change_count(&first.tree, room),
        room_change_count(&second.tree, room),
    );
    // Wakeups are read from /proc, so only where there is one.
    let linux = cfg!(target_os = "linux");
    let driver = linux.then(|| tilo_test_env::idle::current_thread_id().expect("this thread's id"));
    let threads_before = linux.then(|| session_threads(driver));
    let started = Instant::now();
    let _ = run(&mut [&mut first, &mut second], IDLE, |_| false);
    let measured = started.elapsed();
    let wakeups = threads_before.map(|before| {
        tilo_test_env::idle::Wakeups::between(before, session_threads(driver), measured)
    });
    let first_idle = since(&first_before, &received(&first_net));
    let second_idle = since(&second_before, &received(&second_net));
    let repaints = (
        first.repaints - repaints_before.0,
        second.repaints - repaints_before.1,
    );

    let per_minute = |count: u64| {
        f64::from(u32::try_from(count).unwrap_or(u32::MAX)) * 60.0 / measured.as_secs_f64()
    };
    let mut report = format!("idle two-desktop session over {measured:.1?}:\n");
    let mut total = 0;
    for (name, idle) in [("first", &first_idle), ("second", &second_idle)] {
        for (kind, count) in idle {
            total += count;
            let _ = writeln!(
                report,
                "  {name} received {count} {kind} ({:.1}/min)",
                per_minute(*count)
            );
        }
    }
    let _ = writeln!(
        report,
        "  total {total} application messages ({:.1}/min); repaints first {} second {}",
        per_minute(total),
        repaints.0,
        repaints.1,
    );
    for (name, tree, before) in [
        ("first", &first.tree, room_changes_before.0),
        ("second", &second.tree, room_changes_before.1),
    ] {
        let _ = writeln!(
            report,
            "  {name}'s room document: {}",
            newest_room_changes(tree, room, before)
        );
    }
    if let Some(wakeups) = &wakeups {
        let _ = writeln!(report, "both desktops' threads, {wakeups}");
    }
    eprintln!("{report}");
    assert_eq!(total, 0, "{report}");
    if let Some(wakeups) = &wakeups {
        assert!(
            wakeups.per_second() <= IDLE_WAKEUP_BUDGET,
            "the idle pair's threads woke {:.2} times a second, over {IDLE_WAKEUP_BUDGET}/s:\n{report}",
            wakeups.per_second(),
        );
    }
    assert_eq!(repaints, (0, 0), "{report}");
    drop((first, second, first_net, second_net));
}

/// A desktop alone with a room of its own, as most are, wakes nothing while
/// nobody uses it: no peer to keep alive, nothing to sync, and its
/// compositor parked, so not even a frame tick reaches the network.
#[test]
#[serial_test::serial]
fn a_lone_idle_desktop_keeps_its_network_runtime_asleep() {
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[44; 32]);
    let person = match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    };
    home.env().set("TILO_USER_ID", &person);
    let mut tree = WorkspaceTree::default();
    let workspace = tree.append_empty_workspace("Home");
    tree.adopt_ownerless_workspaces(&person);
    tree.desktop.active = workspace;
    let net = device(
        tree.shared(),
        &dir.path().join("lone"),
        &root,
        DeviceCapabilities::primary(),
    );
    assert!(net.join_default_workspace());
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    while net.dial_addr().is_none() {
        assert!(Instant::now() < deadline, "the desktop never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(LONE_SETTLE);

    // Nothing drives this desktop, so every thread of the process counts.
    let before = session_threads(None);
    let started = Instant::now();
    std::thread::sleep(LONE_IDLE);
    let measured = started.elapsed();
    let wakeups = tilo_test_env::idle::Wakeups::between(before, session_threads(None), measured);
    eprintln!("a lone idle desktop's threads, {wakeups}");
    assert!(
        wakeups.per_second() <= LONE_WAKEUP_BUDGET,
        "a lone idle desktop woke {:.2} times a second, over {LONE_WAKEUP_BUDGET}/s:\n{wakeups}",
        wakeups.per_second(),
    );
    drop(net);
}
