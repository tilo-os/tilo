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
//! test hooks and hold the loop's calls to one frame. Opening the owner is
//! the loop's business neither: it binds and publishes off the loop, and
//! never with the tree held, which the loop takes every tick. Inbound
//! network events wait for it, in the order they arrived, rather than the
//! loop waiting; and the governed-room reads of chrome's window transfers
//! take only an owner already open. The session broker, built on the loop
//! at session start and usually the owner's first user, only starts the
//! open: its synchronous answers say the owner is opening, and its commits
//! wait for it in their own futures. The rest of binding that broker (its
//! invocation journal, MLS vault and channel state) runs on the
//! session-start worker: the loop only starts it, polls it and installs it.

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
    let saver = std::thread::spawn(move || {
        with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::BeforeDataFlush {
                    let _ = syncing.send(());
                    std::thread::sleep(SLOW_PUBLICATION);
                }
            },
            || snapshot.write_to(&path),
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

#[test]
fn the_frame_loop_never_opens_the_owner_on_its_own_thread() {
    // A state file no writer has bound yet: opening its owner takes the
    // lease, syncs every directory up to `/` and publishes the reopened
    // generation. The desktop pressure smoke once froze with the loop in
    // exactly that, reached from `viewer_hosts`.
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let room = active_room(&tree);

    let directory_syncs = Rc::new(Cell::new(0_usize));
    let data_syncs = Rc::new(Cell::new(0_usize));
    let counted_directories = Rc::clone(&directory_syncs);
    let counted_data = Rc::clone(&data_syncs);
    let slowest = with_provision_observer(
        move |event| {
            if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                counted_directories.set(counted_directories.get().saturating_add(1));
                std::thread::sleep(SLOW_DIRECTORY_SYNC);
            }
        },
        || {
            with_observer(
                move |event| {
                    if event.checkpoint == Checkpoint::BeforeDataFlush {
                        counted_data.set(counted_data.get().saturating_add(1));
                        std::thread::sleep(SLOW_DIRECTORY_SYNC);
                    }
                },
                || {
                    (0..TICKS)
                        .map(|_| timed(|| network.viewer_hosts(room)).1)
                        .max()
                        .unwrap_or_default()
                },
            )
        },
    );

    assert_eq!(
        directory_syncs.get(),
        0,
        "the frame loop synced the state file's directory chain itself"
    );
    assert_eq!(
        data_syncs.get(),
        0,
        "the frame loop published the state file itself"
    );
    assert!(
        slowest <= FRAME,
        "the frame loop's owner lookup took {slowest:?} with no owner open yet, over one frame ({FRAME:?})"
    );

    // The owner still opens, off the loop, and the loop then finds it.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("deadline");
    while network.opened_native_owner().is_none() {
        assert!(
            Instant::now() < deadline,
            "the owner never opened off the frame loop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        tree.read().installed_native_owner().is_some(),
        "the owner opened off the loop is attached to its writer"
    );
}

#[test]
fn opening_the_owner_never_holds_the_tree_across_a_disk_sync() {
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);

    // Whoever opens the owner (an action, enrollment, the loop's background
    // open), the loop reads and writes the tree every tick meanwhile.
    let (reached, stages) = std::sync::mpsc::channel::<&'static str>();
    let opening = tree.clone();
    let opener = std::thread::spawn(move || {
        let directories = reached.clone();
        with_provision_observer(
            move |event| {
                if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                    let _ = directories.send("directory");
                    std::thread::sleep(SLOW_DIRECTORY_SYNC);
                }
            },
            || {
                with_observer(
                    move |event| {
                        if event.checkpoint == Checkpoint::BeforeDataFlush {
                            let _ = reached.send("data");
                            std::thread::sleep(SLOW_DIRECTORY_SYNC);
                        }
                    },
                    || open(opening),
                )
            },
        )
    });

    let mut seen = std::collections::BTreeMap::<&str, usize>::new();
    while let Ok(stage) = stages.recv_timeout(Duration::from_secs(30)) {
        let read_took = timed(|| drop(tree.read())).1;
        let write_took = timed(|| drop(tree.write())).1;
        let in_progress = !opener.is_finished();
        assert!(
            in_progress,
            "the open finished before the tree was taken, so they did not race"
        );
        for (what, took) in [("read", read_took), ("write", write_took)] {
            assert!(
                took <= FRAME,
                "taking the tree to {what} took {took:?} while opening its owner waited for a {stage} sync, over one frame ({FRAME:?})"
            );
        }
        let count = seen.entry(stage).or_default();
        *count = count.saturating_add(1);
    }
    assert!(
        seen.contains_key("directory") && seen.contains_key("data"),
        "the open's directory and data syncs were not both observed: {seen:?}"
    );
    // The writer holds its owner weakly: keep this one alive to look it up.
    let owner = opener
        .join()
        .expect("the opening thread")
        .expect("the owner opens");
    assert!(
        tree.read().installed_native_owner().is_some(),
        "the opened owner is attached to its writer"
    );
    drop(owner);
}

/// Run `body` with every directory and data sync on this thread slowed to
/// [`SLOW_DIRECTORY_SYNC`], and answer what it returned and how many syncs
/// this thread made.
fn on_a_slow_disk<T>(body: impl FnOnce() -> T) -> (T, usize) {
    let syncs = Rc::new(Cell::new(0_usize));
    let directories = Rc::clone(&syncs);
    let data = Rc::clone(&syncs);
    let value = with_provision_observer(
        move |event| {
            if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                directories.set(directories.get().saturating_add(1));
                std::thread::sleep(SLOW_DIRECTORY_SYNC);
            }
        },
        || {
            with_observer(
                move |event| {
                    if event.checkpoint == Checkpoint::BeforeDataFlush {
                        data.set(data.get().saturating_add(1));
                        std::thread::sleep(SLOW_DIRECTORY_SYNC);
                    }
                },
                body,
            )
        },
    );
    (value, syncs.get())
}

/// Another open of `tree`'s writer, in progress on a thread of its own
/// until the returned sender is dropped (or [`SLOW_PUBLICATION`] passed):
/// every other open of the writer waits behind it, so the owner stays
/// opening meanwhile.
fn open_in_progress(
    tree: &WorkspaceTreeHandle,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    test_open_in_progress(tree, SLOW_PUBLICATION)
}

fn departed(seed: u8) -> iroh::EndpointId {
    iroh::SecretKey::from_bytes(&[seed; 32]).public()
}

fn depart(network: &crate::shell::network::ShellNetwork, seed: u8) {
    network
        .event_tx
        .try_send(crate::network::actor::NetEvent::PeerLeft {
            ws: crate::shell::network::DEFAULT_WS_TOPIC,
            peer: departed(seed),
            cause: crate::network::actor::PeerDepartureCause::LivenessTimedOut,
            membership_epoch: None,
        })
        .expect("the event queue has room");
}

fn until_opened(network: &crate::shell::network::ShellNetwork) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("deadline");
    while network.opened_native_owner().is_none() {
        assert!(
            Instant::now() < deadline,
            "the owner never opened off the frame loop"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn inbound_events_wait_in_order_while_the_owner_opens_off_the_loop() {
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let (release, holder) = open_in_progress(&tree);
    depart(&network, 41);
    depart(&network, 42);

    let (slowest, syncs) = on_a_slow_disk(|| {
        (0..TICKS)
            .map(|_| timed(|| network.frame_tick()).1)
            .max()
            .unwrap_or_default()
    });
    let early = network.peer_departures().len();
    drop(release);
    holder.join().expect("the other open");

    assert_eq!(
        syncs, 0,
        "the frame loop synced the state file while handling inbound events"
    );
    assert!(
        slowest <= FRAME,
        "a frame-loop network tick took {slowest:?} while the owner was opening, over one frame ({FRAME:?})"
    );
    assert_eq!(
        early, 0,
        "an inbound event was handled before the owner it may read had opened"
    );

    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("deadline");
    while network.peer_departures().len() < 2 {
        assert!(
            Instant::now() < deadline,
            "the held inbound events were never handled once the owner opened"
        );
        network.frame_tick();
        std::thread::sleep(Duration::from_millis(5));
    }
    let order: Vec<_> = network
        .peer_departures()
        .into_iter()
        .map(|departure| departure.endpoint_id)
        .collect();
    assert_eq!(
        order,
        [departed(41), departed(42)],
        "the held inbound events were handled out of the order they arrived in"
    );
    assert!(
        tree.read().installed_native_owner().is_some(),
        "the owner the events waited for opened off the loop"
    );
}

#[test]
fn the_loop_reads_governed_rooms_without_opening_the_owner() {
    use crate::shell::document_import::{HostedRooms as _, NativeHostedRooms};
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let network = std::sync::Arc::new(network);
    let room = active_room(&tree);
    let hosted = NativeHostedRooms(std::sync::Arc::clone(&network));
    let (release, holder) = open_in_progress(&tree);

    let ((hosts, awaiting, slowest), syncs) = on_a_slow_disk(|| {
        // What the loop asks with no owner open yet: hosted legacy windows
        // every half second, and the window-transfer consent scan.
        let ((), windows) = timed(|| network.publish_hosted_legacy_windows(room, &tree));
        let (hosts, scan) = timed(|| hosted.hosts(room));
        let (awaiting, listing) = timed(|| hosted.awaiting_me(room));
        (hosts, awaiting, windows.max(scan).max(listing))
    });
    drop(release);
    holder.join().expect("the other open");

    assert_eq!(
        syncs, 0,
        "the frame loop opened the owner itself to read a governed room"
    );
    assert!(
        slowest <= FRAME,
        "reading a governed room on the frame loop took {slowest:?} with no owner open yet, over one frame ({FRAME:?})"
    );
    assert!(
        hosts,
        "a room was taken as ungoverned before the owner that knows had opened"
    );
    assert!(awaiting.is_none(), "proposals were listed without an owner");

    // The open the loop started finishes off it, and the loop then reads it.
    until_opened(&network);
    let governed = hosted.hosts(room);
    drop(hosted);
    assert!(
        !governed,
        "a room no owner governs is ungoverned once the owner is open"
    );
}

#[test]
fn building_the_session_brokers_owner_services_never_waits_for_the_open() {
    use crate::shell::broker_services::{NATIVE_OWNER_OPENING, NativeOwnerCommitService};
    use std::sync::Arc;
    use tilo_action_broker::services::WorkspaceMemberships;
    use tilo_owner_commit::{CommitOperation, OwnerCommitRequest, OwnerCommitService};
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let room = active_room(&tree).to_string();
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let network = Arc::new(network);
    // The session starts with no owner open: the broker's services, built
    // on the frame loop, are its first users. Each of the broker's owner
    // services (commits, document reads, memberships, tasks) is a
    // `NativeOwnerCommitService`; the task review resolves its own.
    let (release, holder) = open_in_progress(&tree);

    let (built, syncs) = on_a_slow_disk(|| {
        timed(|| {
            let commit = NativeOwnerCommitService::new(tree.clone(), Some(Arc::clone(&network)));
            let review = crate::shell::task_review::NativeTaskReview::new(
                tree.clone(),
                Some(Arc::clone(&network)),
            );
            (commit, review)
        })
    });
    let ((commit, review), took) = built;

    assert_eq!(
        syncs, 0,
        "building the session broker's services synced the state file on the frame loop"
    );
    assert!(
        took <= FRAME,
        "building the session broker's services took {took:?} with the owner still opening, over one frame ({FRAME:?})"
    );
    let review = review.expect("an owner still opening is not a missing one");
    // A synchronous question says the owner is opening, never that there
    // is none, and never answers from a guessed owner.
    assert_eq!(
        OwnerCommitService::health(&commit).expect_err("health while the owner opens"),
        NATIVE_OWNER_OPENING
    );
    assert_eq!(
        WorkspaceMemberships::task_snapshot(&commit, &room)
            .expect_err("a task snapshot while the owner opens"),
        NATIVE_OWNER_OPENING
    );
    assert!(
        review.requires_room(&room),
        "a room read as needing no task review before the owner that knows had opened"
    );

    // A commit waits for the open in its own future, instead of failing
    // closed or running without the owner.
    let pending = commit.commit(OwnerCommitRequest {
        room: "not a room".into(),
        operation: CommitOperation::RetiredAgentRuntime,
    });
    let committing = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime for the commit")
            .block_on(pending)
    });
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !committing.is_finished(),
        "a commit gave up on an owner still opening"
    );
    drop(release);
    holder.join().expect("the other open");
    let committed = committing.join().expect("the commit's thread");
    assert_eq!(
        committed.map(drop),
        Err("invalid native room".to_string()),
        "the commit did not go on against the opened owner"
    );

    until_opened(&network);
    OwnerCommitService::health(&commit).expect("the broker answers from the owner once it opened");
    WorkspaceMemberships::health(&commit)
        .expect("membership answers from the owner once it opened");
}

/// What the loop does to bring the session broker up, at session start and
/// on the tick first login completes: start binding it, poll the start each
/// tick, install what it produced. Binding opens the broker's invocation
/// journal (a directory sync on every open), its vault and its channel
/// state, so on a thread that may not sync it fails. Here each loop-side
/// call returns within a frame and makes no sync, and the broker still
/// comes up with its journal open, because the binding ran on the
/// session-start worker.
#[test]
#[cfg(target_os = "linux")]
#[serial_test::serial(process_env)]
fn the_loop_brings_the_session_broker_up_without_waiting_for_the_disk() {
    use crate::compositor::session_bootstrap::{
        finish_session_collab_broker, service_socket_is_live, start_session_collab_broker,
    };
    use std::sync::Arc;
    let runtime = tempfile::tempdir().expect("a runtime directory for the broker's socket");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_RUNTIME_DIR", runtime.path());
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let network = Arc::new(network);
    let journal = {
        let mut name = path.into_os_string();
        name.push(".invocations");
        std::path::PathBuf::from(name)
    };

    let ((broker, unavailable, ticks, slowest), syncs) =
        crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
            on_a_slow_disk(|| {
                let wake: crate::session_runtime::Wake = Arc::new(|| {});
                let (start, started) =
                    timed(|| start_session_collab_broker(Some(&network), tree.clone(), wake));
                let mut slowest = started;
                let mut ticks = 0_usize;
                let deadline = Instant::now()
                    .checked_add(Duration::from_secs(30))
                    .expect("deadline");
                let outcome = loop {
                    let (polled, took) = timed(|| start.poll());
                    ticks = ticks.saturating_add(1);
                    slowest = slowest.max(took);
                    if let Some(outcome) = polled {
                        break outcome;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "the session broker never came up off the loop"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                };
                let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
                    tree.clone(),
                    crate::shell::hosted_inputs::HostedTileInputs::default(),
                );
                let (broker, installed) = timed(|| finish_session_collab_broker(&chrome, outcome));
                let unavailable = chrome.borrow().collab_unavailable.clone();
                (broker, unavailable, ticks, slowest.max(installed))
            })
        });

    assert_eq!(
        syncs, 0,
        "bringing the session broker up synced on the frame loop's thread"
    );
    assert!(
        slowest <= FRAME,
        "a loop-side step of bringing the session broker up took {slowest:?} over {ticks} ticks, over one frame ({FRAME:?})"
    );
    let broker =
        broker.unwrap_or_else(|| panic!("the session broker did not come up: {unavailable:?}"));
    assert!(
        unavailable.is_none(),
        "a session whose broker came up reports collaboration unavailable: {unavailable:?}"
    );
    assert!(
        service_socket_is_live(&launcher::services::collab_socket()),
        "the broker that came up is not listening where the launcher dials"
    );
    assert!(
        journal.is_dir(),
        "the broker came up without opening its invocation journal at {}",
        journal.display()
    );
    drop(broker);
}
