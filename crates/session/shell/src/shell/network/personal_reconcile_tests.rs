//! The person's workspace list follows the tree's writes, not the frame
//! tick, and its reconcile never holds the compositor's loop.

use super::tests::{drain, personal_workspaces_network_for_test};
use super::{NeighbourMsg, NetCmd, ShellNetwork};
use crate::shell::state::{
    FreshId as _, RoomId, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// How long a test waits for something that takes milliseconds.
const PATIENCE: Duration = Duration::from_secs(30);

/// One of a person's devices holding one workspace of theirs, with the
/// peer-sync worker running as the compositor runs it.
fn laptop(seed: u8) -> (Arc<ShellNetwork>, tokio::sync::mpsc::Receiver<NetCmd>) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let device = iroh::SecretKey::from_bytes(&[seed.wrapping_add(1); 32]);
    let collab_types::caps::Principal::Person(person) =
        collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
        )
    else {
        panic!("a root names a person");
    };
    let mut tree = WorkspaceTree::default();
    let mut workspace = Workspace::in_room(WorkspaceId::new(), RoomId::new(), "Research");
    workspace.set_owner(person);
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let (net, commands) = personal_workspaces_network_for_test(&root, &device, tree);
    let net = Arc::new(net);
    net.start_peer_sync_worker();
    (net, commands)
}

fn tree(net: &ShellNetwork) -> WorkspaceTreeHandle {
    net.replica.lock().unwrap().tree.clone()
}

fn reconciles(net: &ShellNetwork) -> usize {
    net.personal_reconcile.runs.load(Ordering::Relaxed)
}

/// Wait until every job queued on the peer-sync worker so far has run.
fn flush(net: &ShellNetwork) {
    let (done, finished) = mpsc::channel::<()>();
    assert!(
        net.run_off_loop(Box::new(move || {
            let _ = done.send(());
        }))
        .is_ok(),
        "the worker is running"
    );
    finished
        .recv_timeout(PATIENCE)
        .expect("the worker drains its queue");
}

/// How many workspace lists went out among `commands`.
fn lists_sent(commands: &[NetCmd]) -> usize {
    commands
        .iter()
        .filter(|command| {
            matches!(
                command,
                NetCmd::PublishCheckpoint {
                    msg: NeighbourMsg::PersonalWorkspaces { .. },
                    ..
                }
            )
        })
        .count()
}

/// Tick until what the first ticks wrote has been taken in, and count the
/// lists that went out meanwhile.
fn settle(net: &ShellNetwork, commands: &mut tokio::sync::mpsc::Receiver<NetCmd>) -> usize {
    let mut sent = 0_usize;
    for _ in 0..8 {
        net.frame_tick();
        flush(net);
        sent = sent.saturating_add(lists_sent(&drain(commands)));
    }
    sent
}

/// Tick as an idle compositor does, many frames, and count the lists sent.
fn idle_frames(net: &ShellNetwork, commands: &mut tokio::sync::mpsc::Receiver<NetCmd>) -> usize {
    let mut sent = 0_usize;
    for _ in 0..120 {
        net.frame_tick();
        sent = sent.saturating_add(lists_sent(&drain(commands)));
    }
    flush(net);
    sent.saturating_add(lists_sent(&drain(commands)))
}

#[test]
fn an_idle_desktop_runs_no_reconcile_and_a_rename_runs_it_again() {
    let (net, mut commands) = laptop(0x71);
    assert_eq!(
        settle(&net, &mut commands),
        1,
        "the first ticks list the workspace once"
    );

    let before = reconciles(&net);
    let generation = tree(&net).generation();
    assert_eq!(idle_frames(&net, &mut commands), 0);
    assert_eq!(
        reconciles(&net),
        before,
        "an idle desktop runs no reconcile, however many frames it draws"
    );
    assert_eq!(
        tree(&net).generation(),
        generation,
        "nor does anything write its tree"
    );

    tree(&net).write().desktop.workspaces[0].name = "Reading".into();
    net.frame_tick();
    flush(&net);
    assert!(reconciles(&net) > before, "a rename is reconciled");
    assert_eq!(
        lists_sent(&drain(&mut commands)),
        1,
        "the renamed list goes out at once"
    );

    assert_eq!(settle(&net, &mut commands), 0);
    let settled = reconciles(&net);
    assert_eq!(idle_frames(&net, &mut commands), 0);
    assert_eq!(reconciles(&net), settled, "and then it is idle again");
}

#[test]
fn the_frame_tick_never_waits_for_a_tree_write_held_during_a_reconcile() {
    let (net, mut commands) = laptop(0x75);
    let _ = settle(&net, &mut commands);
    let before = reconciles(&net);

    // Another thread renames the workspace and keeps the tree.
    let handle = tree(&net);
    let (held, holding) = mpsc::channel::<()>();
    let (let_go, release) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        handle.write().desktop.workspaces[0].name = "Held".into();
        let _guard = handle.write();
        held.send(()).unwrap();
        let _ = release.recv();
    });
    holding.recv().unwrap();
    // The periodic upkeep, which reads the tree, is not due on these ticks.
    *net.last_sync_kick.lock().unwrap() = Some(Instant::now());

    let (took, timings) = mpsc::channel::<Duration>();
    let ticker = {
        let net = Arc::clone(&net);
        std::thread::spawn(move || {
            // The first tick asks for the reconcile, which the worker starts
            // and which then waits for the tree; the second tick runs while
            // it waits.
            let started = Instant::now();
            net.frame_tick();
            let deadline = Instant::now() + PATIENCE;
            while net.personal_reconcile.runs.load(Ordering::Relaxed) == before
                && Instant::now() < deadline
            {
                std::thread::yield_now();
            }
            net.frame_tick();
            let _ = took.send(started.elapsed());
        })
    };
    let waited = timings.recv_timeout(Duration::from_secs(5));
    let_go.send(()).unwrap();
    holder.join().unwrap();
    ticker.join().unwrap();
    let waited = waited.expect("the frame ticks returned while the tree was held");
    assert!(
        waited < Duration::from_secs(2),
        "the ticks waited {waited:?} for another thread's write"
    );
    assert!(
        reconciles(&net) > before,
        "the rename was handed to a reconcile off the loop"
    );

    flush(&net);
    assert_eq!(
        lists_sent(&drain(&mut commands)),
        1,
        "once the tree is free the reconcile sends the renamed list"
    );
}
