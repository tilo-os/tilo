//! What the frame loop asks the state file's writer may never wait for a
//! disk.
//!
//! The loop asks for the writer's owner every tick (`viewer_hosts` on the
//! headless backend, hosted legacy windows every half second, launches and
//! search through `installed_native_owner`) and checks its accepted
//! authority before app actions. The state file's directory and data syncs
//! happen on the state writer's thread, and can take seconds on a loaded
//! disk. A CI run once caught a single 6.7 s frame-loop pump that used
//! 26 ms of CPU, beside "file lease is busy" from a second writer of the
//! same file. These tests make each sync slow through the file layer's own
//! test hooks and hold the loop's calls to one frame.

use super::*;
use crate::shell::{persist, state::WorkspaceTree};
use std::{
    cell::Cell,
    path::Path,
    rc::Rc,
    time::{Duration, Instant},
};
use tilo_files::native::atomic::test_support::{
    Checkpoint, ProvisionCheckpoint, with_observer, with_provision_observer,
};

/// One frame at 60 Hz: what a call the loop makes every tick may take.
const FRAME: Duration = Duration::from_millis(16);

/// One directory sync on a disk under write load.
const SLOW_DIRECTORY_SYNC: Duration = Duration::from_millis(250);

/// One state-file data sync on a disk under write load, long enough that
/// every lookup below runs while it is still in progress.
const SLOW_PUBLICATION: Duration = Duration::from_secs(2);

/// How many frame-loop ticks are timed.
const TICKS: usize = 8;

fn tree_at(path: &Path) -> WorkspaceTreeHandle {
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(path.into());
    tree.shared()
}

fn active_room(tree: &WorkspaceTreeHandle) -> RoomId {
    tree.read()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room)
        .expect("the desktop has an active workspace")
}

fn timed<T>(call: impl FnOnce() -> T) -> (T, Duration) {
    let started = Instant::now();
    let value = call();
    (value, started.elapsed())
}

#[test]
fn a_writer_lease_held_elsewhere_costs_the_frame_loop_no_disk_sync() {
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    // Another writer of the same file, which keeps its lease: a second
    // session on the profile, or a process that has not let go yet.
    let elsewhere = WorkspaceTree::bootstrap_default();
    persist::save_tree_to(&elsewhere, &path).expect("the other writer saves the file");
    let tree = tree_at(&path);
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let room = active_room(&tree);

    let syncs = Rc::new(Cell::new(0_usize));
    let counted = Rc::clone(&syncs);
    let (slowest, refused, save) = with_provision_observer(
        move |event| {
            if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                counted.set(counted.get().saturating_add(1));
                std::thread::sleep(SLOW_DIRECTORY_SYNC);
            }
        },
        || {
            let slowest = (0..TICKS)
                .map(|_| timed(|| network.viewer_hosts(room)).1)
                .max()
                .unwrap_or_default();
            let refused = network.native_owner().map(drop);
            let save = persist::save_tree_to(&tree.read(), &path);
            (slowest, refused, save)
        },
    );

    let refused = refused.expect_err("the other writer still holds the state file");
    assert!(
        refused.contains("busy"),
        "refused for another reason: {refused}"
    );
    let save = save.expect_err("a save must not publish while another writer holds the file");
    assert!(
        save.to_string().contains("busy"),
        "refused for another reason: {save}"
    );
    assert_eq!(
        syncs.get(),
        0,
        "a writer refused the lease synced directories anyway, which a loaded disk turns into seconds"
    );
    assert!(
        slowest <= FRAME,
        "the frame loop's owner lookup took {slowest:?} while another writer held the lease, over one frame ({FRAME:?})"
    );

    // The lease still decides: once the other writer lets go, this one owns
    // the file.
    drop(elsewhere);
    network
        .native_owner()
        .expect("the file is free once the other writer lets go");
}

#[test]
fn owner_and_authority_lookups_never_wait_for_a_publication_in_progress() {
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    network
        .native_owner()
        .expect("this writer owns its state file");
    let room = active_room(&tree);
    let (witness, authority) = {
        let tree = tree.read();
        (
            tree.native_runtime.authority_witness(),
            tree.native_authority.clone(),
        )
    };

    // A save as `persist::queue_save` runs it on the state writer's thread:
    // snapshot taken, tree released, then the publication, whose data sync
    // is slow.
    let snapshot = persist::TreeSnapshot::of(&tree.read());
    let (syncing, synced) = std::sync::mpsc::channel();
    let saving = path.clone();
    let saver = std::thread::spawn(move || {
        with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::BeforeDataFlush {
                    let _ = syncing.send(());
                    std::thread::sleep(SLOW_PUBLICATION);
                }
            },
            || snapshot.write_to(&saving),
        )
    });
    synced
        .recv_timeout(Duration::from_secs(30))
        .expect("the save reached its data sync");

    let (owner, owner_took) = timed(|| tree.read().installed_native_owner());
    let (current, authority_took) = timed(|| witness.authority_is_current(&authority, room));
    let (_, viewers_took) = timed(|| network.viewer_hosts(room));
    let in_progress = !saver.is_finished();

    assert!(
        owner.is_some(),
        "the opened owner is attached to its writer"
    );
    assert!(current, "the accepted authority is the tree's");
    assert!(
        in_progress,
        "the publication finished before the lookups ran, so they did not race it"
    );
    for (what, took) in [
        ("installed_native_owner", owner_took),
        ("authority_is_current", authority_took),
        ("viewer_hosts", viewers_took),
    ] {
        assert!(
            took <= FRAME,
            "{what} took {took:?} while a publication was syncing, over one frame ({FRAME:?})"
        );
    }
    let published = saver
        .join()
        .expect("the save thread")
        .expect("the save publishes");
    assert_eq!(published, store::Publication::Written);
}
