//! The nearby-transfer store's own thread.
//!
//! Receiving appends each chunk to a partial file and fsyncs a checkpoint
//! every few MiB; finishing hashes and installs the whole object; offers,
//! restarts and cancellations replace the index with an fsync. Under I/O or
//! memory pressure any of these takes seconds, and the compositor's loop,
//! which receives the frames and fabric actions that drive them, must never
//! wait for one. The loop hands each store operation to this thread as a
//! job, in arrival order, and never takes the store's lock or opens the store
//! itself. What has to follow on the loop (an acknowledgement to the peer, an
//! event for Files) comes back as a [`FollowUp`] that the loop runs on its
//! next pump, so an acknowledgement that carries a checkpoint is sent only
//! after the operation, and its fsync, finished.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Mutex, OnceLock};

use crate::shell::network::ShellNetwork;

/// Work the loop does once a store job finished: it may talk to peers.
pub(super) type FollowUp = Box<dyn FnOnce(&ShellNetwork) + Send>;

type Job = Box<dyn FnOnce() -> Option<FollowUp> + Send>;

/// Jobs waiting for the thread. A full queue refuses more rather than
/// blocking the loop.
const QUEUED_JOBS: usize = 4096;

/// Received bytes waiting in queued append jobs. Past this the receiver
/// drops chunks rather than hold unbounded memory, and the receive fails as
/// it would on a failed write: its finish does not verify.
const QUEUED_CHUNK_BYTES: usize = 64 * 1024 * 1024;

struct Worker {
    /// `None` when the thread could not be started; jobs then run on the
    /// caller's thread, as they did before this module.
    jobs: Option<SyncSender<Job>>,
    follow_ups: Mutex<VecDeque<FollowUp>>,
    wake: Mutex<Option<crate::session_runtime::Wake>>,
    chunk_bytes: AtomicUsize,
}

fn worker() -> &'static Worker {
    static WORKER: OnceLock<Worker> = OnceLock::new();
    WORKER.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Job>(QUEUED_JOBS);
        let jobs = match std::thread::Builder::new()
            .name("tilo-nearby-store".into())
            .spawn(move || run(&receiver))
        {
            Ok(_) => Some(sender),
            Err(error) => {
                tracing::error!(%error, "could not start the nearby transfer store thread");
                None
            }
        };
        Worker {
            jobs,
            follow_ups: Mutex::new(VecDeque::new()),
            wake: Mutex::new(None),
            chunk_bytes: AtomicUsize::new(0),
        }
    })
}

fn run(receiver: &Receiver<Job>) {
    while let Ok(job) = receiver.recv() {
        finish(job);
    }
}

/// Run `job` here, queue its follow-up for the loop and wake the loop for
/// it. A job with nothing to follow wakes nobody.
fn finish(job: Job) {
    let follow_up =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).unwrap_or_else(|_| {
            tracing::error!("a nearby transfer store job panicked");
            None
        });
    let Some(follow_up) = follow_up else {
        return;
    };
    let worker = worker();
    if let Ok(mut follow_ups) = worker.follow_ups.lock() {
        follow_ups.push_back(follow_up);
    }
    let wake = worker.wake.lock().ok().and_then(|wake| wake.clone());
    if let Some(wake) = wake {
        wake();
    }
}

/// Run `job` on the store's thread, after every job queued before it.
/// Whatever it returns runs on the loop, which the store thread wakes for
/// it. Whether the job was queued: a full queue refuses it.
pub(super) fn on_store_thread(
    network: &ShellNetwork,
    job: impl FnOnce() -> Option<FollowUp> + Send + 'static,
) -> bool {
    submit(network, Box::new(job))
}

/// [`on_store_thread`] for work whose follow-up may wait for the loop's
/// next pump instead of waking it.
pub(super) fn on_store_thread_unhurried(
    job: impl FnOnce() -> Option<FollowUp> + Send + 'static,
) -> bool {
    queue(Box::new(move || {
        if let Some(follow_up) = job()
            && let Ok(mut follow_ups) = worker().follow_ups.lock()
        {
            follow_ups.push_back(follow_up);
        }
        None
    }))
}

/// Run `operation` on the store, on its thread, after every job queued
/// before it; nothing follows on the loop.
pub(super) fn on_store(
    operation: impl FnOnce(
        &mut crate::shell::nearby_transfer::NearbyTransferStore,
    ) -> Result<(), String>
    + Send
    + 'static,
) {
    let _ = queue(Box::new(move || {
        if let Err(error) = super::with_nearby_transfer_store(operation) {
            tracing::debug!(%error, "nearby transfer store operation failed");
        }
        None
    }));
}

/// Remove one staged outgoing object, on the store's thread. A pass that
/// sees the object again before that ran (every loop pump reads the outbox)
/// queues nothing more.
pub(super) fn cancel_outgoing(transfer_id: [u8; 16]) {
    static QUEUED: Mutex<std::collections::BTreeSet<[u8; 16]>> =
        Mutex::new(std::collections::BTreeSet::new());
    if !QUEUED
        .lock()
        .is_ok_and(|mut queued| queued.insert(transfer_id))
    {
        return;
    }
    let queued = queue(Box::new(move || {
        let _ = super::with_nearby_transfer_store(|store| store.cancel_outgoing(transfer_id));
        if let Ok(mut queued) = QUEUED.lock() {
            queued.remove(&transfer_id);
        }
        None
    }));
    if !queued && let Ok(mut pending) = QUEUED.lock() {
        pending.remove(&transfer_id);
    }
}

/// [`on_store_thread`] for a received chunk of `len` bytes, refused while
/// too many received bytes already wait for the disk.
pub(super) fn append_on_store_thread(
    network: &ShellNetwork,
    len: usize,
    job: impl FnOnce() + Send + 'static,
) -> bool {
    let worker = worker();
    let queued = worker.chunk_bytes.fetch_add(len, Ordering::AcqRel);
    if queued.saturating_add(len) > QUEUED_CHUNK_BYTES {
        worker.chunk_bytes.fetch_sub(len, Ordering::AcqRel);
        tracing::warn!("nearby transfer chunk dropped: the disk is not keeping up");
        return false;
    }
    let accepted = submit(
        network,
        Box::new(move || {
            job();
            worker.chunk_bytes.fetch_sub(len, Ordering::AcqRel);
            None
        }),
    );
    if !accepted {
        worker.chunk_bytes.fetch_sub(len, Ordering::AcqRel);
    }
    accepted
}

/// Open the store on its thread, once, so readers that never wait for it
/// ([`super::try_nearby_entries`]) find it open. Every job opens it too.
pub(super) fn open_store() {
    static REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if REQUESTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let opened = queue(Box::new(|| {
        let _ = super::with_nearby_transfer_store(|_| Ok(()));
        None
    }));
    if !opened {
        REQUESTED.store(false, Ordering::Release);
    }
}

fn submit(network: &ShellNetwork, job: Job) -> bool {
    if let Ok(mut wake) = worker().wake.lock()
        && wake.is_none()
    {
        *wake = Some(network.loop_waker());
    }
    queue(job)
}

fn queue(job: Job) -> bool {
    let worker = worker();
    let Some(jobs) = worker.jobs.as_ref() else {
        finish(job);
        return true;
    };
    match jobs.try_send(job) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            tracing::warn!("nearby transfer store queue is full; dropping a store operation");
            false
        }
        Err(TrySendError::Disconnected(job)) => {
            finish(job);
            true
        }
    }
}

/// Run, here on the loop, every follow-up of a finished store job, oldest
/// first.
pub(super) fn run_follow_ups(network: &ShellNetwork) {
    loop {
        let next = worker()
            .follow_ups
            .lock()
            .ok()
            .and_then(|mut follow_ups| follow_ups.pop_front());
        let Some(follow_up) = next else {
            return;
        };
        follow_up(network);
    }
}

/// Wait until every job queued before this call has run, then run their
/// follow-ups here. For tests that drive the loop's entry points directly.
#[cfg(test)]
pub(super) fn settle(network: &ShellNetwork) {
    let (done, finished) = std::sync::mpsc::channel::<()>();
    if submit(
        network,
        Box::new(move || {
            let _ = done.send(());
            None
        }),
    ) {
        finished
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("the nearby transfer store thread finished its queue");
    }
    run_follow_ups(network);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::nearby_transfer::NearbyTransferStore;
    use std::time::Duration;
    use tilo_lock::MutexRecover as _;

    fn network() -> ShellNetwork {
        crate::shell::network::shell_network_for_test(
            crate::shell::state::WorkspaceTree::default().shared(),
            0x4e45_4152_4259,
        )
        .0
    }

    /// A store in a fresh spool, put into the session's store cell for a test
    /// that must never open the real one. The returned guard keeps the other
    /// tests that replace the store waiting; the rest use transfer ids of
    /// their own.
    fn temporary_store() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        static REPLACING: Mutex<()> = Mutex::new(());
        let replacing = REPLACING.lock_recover();
        let root = tempfile::tempdir().expect("spool directory");
        let store = NearbyTransferStore::open(root.path().join("spool"), tilo_clock::unix_now_ms())
            .expect("temporary nearby store");
        *super::super::nearby_store_cell()
            .lock()
            .expect("nearby store cell") = Some(store);
        (root, replacing)
    }

    #[test]
    fn a_follow_up_runs_on_the_loop_only_after_its_store_job_finished() {
        let network = network();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let followed = std::sync::Arc::clone(&ran);
        assert!(on_store_thread(&network, move || {
            released
                .recv_timeout(Duration::from_secs(60))
                .expect("the test releases the job");
            Some(Box::new(move |_: &ShellNetwork| {
                followed.store(true, Ordering::SeqCst);
            }))
        }));
        run_follow_ups(&network);
        assert!(
            !ran.load(Ordering::SeqCst),
            "nothing follows a job that is still running"
        );
        release.send(()).expect("job waiting");
        settle(&network);
        assert!(
            ran.load(Ordering::SeqCst),
            "the follow-up ran after its job"
        );
    }

    #[test]
    fn staging_holds_the_store_only_to_admit_and_to_record() {
        let _spool = temporary_store();
        let source_dir = tempfile::tempdir().expect("source directory");
        let source = source_dir.path().join("report.bin");
        std::fs::write(&source, vec![7_u8; 64 * 1024]).expect("source file");
        let transfer_id = crate::random::nonzero::<16>();
        let staging = crate::shell::nearby_transfer::Staging::for_path(
            &source,
            "staging-target".into(),
            tilo_clock::unix_now_ms(),
            transfer_id,
        )
        .expect("portable name");
        let shape =
            crate::shell::nearby_transfer::StageShape::of_path(&source).expect("source shape");
        super::super::stage_outside_store_lock(staging, shape, |reserved| {
            assert!(
                super::super::nearby_store_cell().try_lock().is_ok(),
                "the copy, hash and sync run without the store"
            );
            reserved.write_path(&source)
        })
        .expect("staged");
        let staged = super::super::nearby_store_cell()
            .lock()
            .expect("nearby store cell")
            .as_ref()
            .is_some_and(|store| {
                store
                    .outbox()
                    .iter()
                    .any(|entry| entry.id == transfer_id && entry.total_bytes == 64 * 1024)
            });
        assert!(staged, "the written object is recorded in the outbox");
    }

    /// Frames, fabric actions and the outgoing pass all run on the
    /// compositor's loop. While another thread holds the store across a long
    /// write (here, the test), each must return at once rather than wait for
    /// the store's lock, and none may sync to disk itself.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_loop_never_waits_for_the_nearby_store() {
        let _spool = temporary_store();
        // Started before the sync filter goes on, so its thread can sync.
        let _ = worker();
        let network = network();
        let held = super::super::nearby_store_cell()
            .lock()
            .expect("nearby store cell");
        let (done, finished) = std::sync::mpsc::channel();
        let looping = std::thread::spawn(move || {
            crate::shell::disk_sync_guard::deny_disk_syncs_on_this_thread();
            drive_every_loop_entry_point(&network);
            let _ = done.send(());
            network
        });
        let returned = finished.recv_timeout(Duration::from_secs(10));
        drop(held);
        let network = looping.join().expect("loop thread");
        assert!(
            returned.is_ok(),
            "a loop entry point waited for the nearby store's lock"
        );
        settle(&network);
        if let Ok(mut state) = super::super::nearby_drop_state().lock() {
            state.large_offers.remove(&[0x5a; 16]);
            state
                .media_authorized
                .retain(|(_, transfer_id)| *transfer_id != [0x5a; 16]);
        }
    }

    #[cfg(target_os = "linux")]
    fn drive_every_loop_entry_point(network: &ShellNetwork) {
        use crate::shell::device_fabric::PersonalFabricAction;
        use crate::shell::personal_continuity::DeviceId;

        let source = DeviceId::new("loop-guard-source").expect("device id");
        let transfer_id = [0x5a; 16];
        let request_id = [0x5b; 16];
        let chunk = crate::network::actor::PERSONAL_TRANSFER_CHUNK_BYTES;
        for offset in [0, chunk] {
            super::super::handle_nearby_transfer_frame(
                network,
                &source,
                crate::network::actor::PersonalNearbyTransferFrame::Chunk {
                    transfer_id,
                    request_id,
                    offset: u64::try_from(offset).expect("offset fits"),
                    bytes: vec![1; chunk],
                },
            );
        }
        super::super::handle_nearby_transfer_frame(
            network,
            &source,
            crate::network::actor::PersonalNearbyTransferFrame::Finish {
                transfer_id,
                request_id,
                content_hash: [2; 32],
            },
        );
        if let Ok(mut state) = super::super::nearby_drop_state().lock() {
            state
                .large_offers
                .insert(transfer_id, (source.clone(), request_id, "x.bin".into()));
            state.media_authorized.insert((source.clone(), transfer_id));
        }
        for action in [
            PersonalFabricAction::NearbyTransferReady {
                request_id,
                transfer_id,
                committed_offset: 8,
                prefix_hash: [3; 32],
            },
            PersonalFabricAction::NearbyTransferRestart {
                request_id,
                transfer_id,
            },
            PersonalFabricAction::NearbyTransferResult {
                request_id,
                transfer_id,
                accepted: true,
            },
            PersonalFabricAction::NearbyTransferOffer {
                transfer_id,
                name: "film.mkv".into(),
                kind: crate::shell::nearby_transfer::TransferKind::MediaHandoff,
                total_bytes: 4,
                content_hash: [4; 32],
            },
            PersonalFabricAction::NearbyTransferCancel {
                request_id,
                transfer_id,
            },
        ] {
            super::super::handle_nearby_drop_action(network, request_id, source.clone(), action);
        }
        super::super::resume_nearby_transfers(
            network,
            crate::shell::continuity_policy::ContinuityPolicySnapshot::default(),
        );
    }
}
