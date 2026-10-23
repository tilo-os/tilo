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
//! same file. These tests count the syncs the loop's own thread makes through
//! the file layer's test hooks, and hold each sync or open another thread
//! makes until the loop's calls have returned, so a call that waited for one
//! cannot return in time. Opening the owner is
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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_files::native::atomic::test_support::{
    Checkpoint, ProvisionCheckpoint, with_observer, with_provision_observer,
};

/// How many frame-loop ticks are run.
const TICKS: usize = 8;

/// Whether thread `tid` of this process sleeps in the kernel, as one
/// waiting on a held lock or channel does.
fn thread_is_blocked(tid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/self/task/{tid}/stat"))
        .ok()
        .and_then(|stat| Some(stat.rsplit_once(')')?.1.trim_start().starts_with('S')))
        .unwrap_or(false)
}

/// A sync another thread makes, held at its checkpoint until the test drops
/// the sender [`HeldSync::new`] returned, or until the hang bound passes if a
/// call under test waits for it.
#[derive(Clone)]
struct HeldSync {
    parked: Arc<AtomicBool>,
    released: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
}

impl HeldSync {
    fn new() -> (Self, std::sync::mpsc::Sender<()>) {
        let (release, released) = std::sync::mpsc::channel();
        let held = Self {
            parked: Arc::default(),
            released: Arc::new(Mutex::new(released)),
        };
        (held, release)
    }

    /// Park here until released, telling the test through `announce` once
    /// parked; the sync then goes on. Parked counts from before `announce`,
    /// so a test that heard it and finds the sync no longer parked knows
    /// the sync went on: a call under test waited for it.
    fn park_after(&self, announce: impl FnOnce()) {
        self.parked.store(true, Ordering::SeqCst);
        announce();
        let _ = self
            .released
            .lock()
            .unwrap()
            .recv_timeout(tilo_test_env::hang_bound());
        self.parked.store(false, Ordering::SeqCst);
    }

    fn is_parked(&self) -> bool {
        self.parked.load(Ordering::SeqCst)
    }
}

fn tree_at(path: &Path) -> WorkspaceTreeHandle {
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(path.into());
    tree.shared()
}

fn active_room(tree: &WorkspaceTreeHandle) -> RoomId {
    tree.read()
        .desktop
        .active_workspace()
        .map(crate::shell::state::Workspace::room)
        .expect("the desktop has an active workspace")
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
    let (refused, save) = with_provision_observer(
        move |event| {
            if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                counted.set(counted.get().saturating_add(1));
            }
        },
        || {
            for _ in 0..TICKS {
                network.viewer_hosts(room);
            }
            let refused = network.native_owner().map(drop);
            let save = persist::save_tree_to(&tree.read(), &path);
            (refused, save)
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
    let (held, release) = HeldSync::new();
    let holding = held.clone();
    let saver = std::thread::spawn(move || {
        with_observer(
            move |event| {
                if event.checkpoint == Checkpoint::BeforeDataFlush {
                    holding.park_after(|| {
                        let _ = syncing.send(());
                    });
                }
            },
            || snapshot.write_to(&path),
        )
    });
    tilo_test_env::recv("the save to reach its data sync", &synced);

    let owner = tree.read().installed_native_owner();
    let current = witness.authority_is_current(&authority, room);
    network.viewer_hosts(room);
    let in_progress = held.is_parked();
    drop(release);

    assert!(
        owner.is_some(),
        "the opened owner is attached to its writer"
    );
    assert!(current, "the accepted authority is the tree's");
    assert!(
        in_progress,
        "a lookup waited for the publication's data sync to finish"
    );
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
    with_provision_observer(
        move |event| {
            if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                counted_directories.set(counted_directories.get().saturating_add(1));
            }
        },
        || {
            with_observer(
                move |event| {
                    if event.checkpoint == Checkpoint::BeforeDataFlush {
                        counted_data.set(counted_data.get().saturating_add(1));
                    }
                },
                || {
                    for _ in 0..TICKS {
                        network.viewer_hosts(room);
                    }
                },
            );
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

    // The owner still opens, off the loop, and the loop then finds it.
    until_opened(&network);
    assert!(
        tree.read().installed_native_owner().is_some(),
        "the owner opened off the loop is attached to its writer"
    );
}

#[test]
fn opening_the_owner_never_holds_the_tree_across_a_disk_sync() {
    type Stage = (&'static str, HeldSync, std::sync::mpsc::Sender<()>);
    let directory = tempfile::tempdir().expect("a directory for the state file");
    let path = directory.path().join("state.json");
    let tree = tree_at(&path);

    // Whoever opens the owner (an action, enrollment, the loop's background
    // open), the loop reads and writes the tree every tick meanwhile.
    // Each sync parks until the test has taken the tree, read and write,
    // and lets it go on.
    let (reached, stages) = std::sync::mpsc::channel::<Stage>();
    let opening = tree.clone();
    let opener = std::thread::spawn(move || {
        let directories = reached.clone();
        let sync = |stage: &'static str, to: &std::sync::mpsc::Sender<Stage>| {
            let (held, release) = HeldSync::new();
            held.park_after(|| {
                let _ = to.send((stage, held.clone(), release));
            });
        };
        with_provision_observer(
            move |event| {
                if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                    sync("directory", &directories);
                }
            },
            || {
                with_observer(
                    move |event| {
                        if event.checkpoint == Checkpoint::BeforeDataFlush {
                            sync("data", &reached);
                        }
                    },
                    || open(opening),
                )
            },
        )
    });

    let mut seen = std::collections::BTreeMap::<&str, usize>::new();
    while let Ok((stage, held, release)) = stages.recv_timeout(tilo_test_env::hang_bound()) {
        drop(tree.read());
        drop(tree.write());
        assert!(
            held.is_parked(),
            "taking the tree waited for opening its owner to finish a {stage} sync"
        );
        drop(release);
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

/// Run `body` and answer what it returned and how many directory and data
/// syncs this thread made.
fn counting_syncs<T>(body: impl FnOnce() -> T) -> (T, usize) {
    let syncs = Rc::new(Cell::new(0_usize));
    let directories = Rc::clone(&syncs);
    let data = Rc::clone(&syncs);
    let value = with_provision_observer(
        move |event| {
            if event.checkpoint == ProvisionCheckpoint::BeforeSync {
                directories.set(directories.get().saturating_add(1));
            }
        },
        || {
            with_observer(
                move |event| {
                    if event.checkpoint == Checkpoint::BeforeDataFlush {
                        data.set(data.get().saturating_add(1));
                    }
                },
                body,
            )
        },
    );
    (value, syncs.get())
}

/// Another open of `tree`'s writer, in progress on a thread of its own
/// until the returned sender is dropped (or the hang bound passed, should a
/// call under test wait for it): every other open of the writer waits behind
/// it, so the owner stays opening meanwhile.
fn open_in_progress(
    tree: &WorkspaceTreeHandle,
) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    test_open_in_progress(tree, tilo_test_env::hang_bound())
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
    tilo_test_env::wait_until("the owner to open off the frame loop", || {
        network.opened_native_owner().is_some()
    });
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

    let ((), syncs) = counting_syncs(|| {
        for _ in 0..TICKS {
            network.frame_tick();
        }
    });
    let early = network.peer_departures().len();
    let still_opening = !holder.is_finished();
    drop(release);
    holder.join().expect("the other open");

    assert_eq!(
        syncs, 0,
        "the frame loop synced the state file while handling inbound events"
    );
    assert!(
        still_opening,
        "a frame-loop network tick waited for the owner's open to finish"
    );
    assert_eq!(
        early, 0,
        "an inbound event was handled before the owner it may read had opened"
    );

    tilo_test_env::wait_until(
        "the held inbound events to be handled once the owner opened",
        || {
            network.frame_tick();
            network.peer_departures().len() >= 2
        },
    );
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

    let ((hosts, awaiting), syncs) = counting_syncs(|| {
        // What the loop asks with no owner open yet: hosted legacy windows
        // every half second, and the window-transfer consent scan.
        network.publish_hosted_legacy_windows(room, &tree);
        (hosted.hosts(room), hosted.awaiting_me(room))
    });
    let still_opening = !holder.is_finished();
    drop(release);
    holder.join().expect("the other open");

    assert_eq!(
        syncs, 0,
        "the frame loop opened the owner itself to read a governed room"
    );
    assert!(
        still_opening,
        "reading a governed room on the frame loop waited for the owner's open to finish"
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
    let (network, _commands) = crate::shell::network::shell_network_for_test(tree.clone(), 1);
    let network = Arc::new(network);
    // The session starts with no owner open: the broker's services, built
    // on the frame loop, are its first users. Each of the broker's owner
    // services (commits, document reads, memberships, tasks) is a
    // `NativeOwnerCommitService`.
    let (release, holder) = open_in_progress(&tree);

    let (commit, syncs) =
        counting_syncs(|| NativeOwnerCommitService::new(tree.clone(), Some(Arc::clone(&network))));
    let still_opening = !holder.is_finished();

    assert_eq!(
        syncs, 0,
        "building the session broker's services synced the state file on the frame loop"
    );
    assert!(
        still_opening,
        "building the session broker's services waited for the owner's open to finish"
    );
    // A synchronous question says the owner is opening, never that there
    // is none, and never answers from a guessed owner.
    assert_eq!(
        OwnerCommitService::health(&commit).expect_err("health while the owner opens"),
        NATIVE_OWNER_OPENING
    );

    // A commit waits for the open in its own future, instead of failing
    // closed or running without the owner.
    let pending = commit.commit(OwnerCommitRequest {
        room: "not a room".into(),
        operation: CommitOperation::RetiredAgentRuntime,
    });
    let (thread_id, commit_thread) = std::sync::mpsc::channel();
    let committing = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime for the commit");
        let _ = thread_id.send(tilo_test_env::idle::current_thread_id().expect("a thread id"));
        runtime.block_on(pending)
    });
    let commit_thread = tilo_test_env::recv("the commit's thread to start", &commit_thread);
    // Either the commit gives up, or it sleeps waiting for the open.
    let gave_up = tilo_test_env::wait_for("the commit to finish or wait", || {
        if committing.is_finished() {
            Some(true)
        } else {
            thread_is_blocked(commit_thread).then_some(false)
        }
    });
    assert!(!gave_up, "a commit gave up on an owner still opening");
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
/// state, so on a thread that may not sync it fails. Here no loop-side call
/// makes a sync, and the broker still comes up with its journal open,
/// because the binding ran on the session-start worker.
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

    let ((broker, unavailable), syncs) =
        crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
            counting_syncs(|| {
                let wake: crate::session_runtime::Wake = Arc::new(|| {});
                let start = start_session_collab_broker(Some(&network), tree.clone(), wake);
                let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
                    tree.clone(),
                    crate::shell::hosted_inputs::HostedTileInputs::default(),
                );
                let broker = finish_session_collab_broker(
                    &chrome,
                    tilo_test_env::wait_for("the session broker to come up off the loop", || {
                        start.poll()
                    }),
                );
                let unavailable = chrome.borrow().collab_unavailable.clone();
                (broker, unavailable)
            })
        });

    assert_eq!(
        syncs, 0,
        "bringing the session broker up synced on the frame loop's thread"
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
