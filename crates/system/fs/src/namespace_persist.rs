//! Writing the namespace to disk when, and only when, it changes.
//!
//! Signed records are durable the moment they are published; a namespace
//! persisted late could lose the placements of records a crash left
//! published, which a restart then reads as deletions. So a change is
//! written once it settles: after [`QUIET`] without further changes, or
//! [`MAX_DELAY`] after the first of a continuous run, so a burst is one
//! write and an intermediate state of it rarely is one.
//!
//! The persister learns of changes from the namespace itself
//! ([`Namespace::subscribe_changes`]) instead of asking on a timer: an idle
//! namespace costs no wakeup at all, where a 100 ms poll cost ten a second
//! for the life of every mount. While changes keep arriving it
//! looks at most every [`SETTLE_STEP`], so a burst of edits wakes it a few
//! times, not once per edit.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use crate::namespace::Namespace;

/// A change is written once this long passes without another.
pub const QUIET: Duration = Duration::from_millis(500);
/// A continuous run of changes is written at least this often.
pub const MAX_DELAY: Duration = Duration::from_secs(3);
/// While changes keep arriving, how often the persister looks again. Bounds
/// how late it can notice the last change of a burst, and so how far past
/// [`QUIET`] after it the write can land.
pub const SETTLE_STEP: Duration = Duration::from_millis(100);
/// The longest wait before retrying a write that failed.
const MAX_RETRY: Duration = Duration::from_secs(60);

/// What a flush has to write: the document when its heads moved since the
/// last write, the sync progress beside it when its generation moved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    pub document: bool,
    pub progress: bool,
}

impl Pending {
    #[must_use]
    pub const fn any(self) -> bool {
        self.document || self.progress
    }
}

/// Persists `namespace` whenever it changes, through `flush`, which writes
/// what [`Pending`] names and returns what it could not write (empty when
/// everything was written). The namespace is unlocked when `flush` is
/// called; it takes the lock itself, as long as writing needs it.
///
/// Whatever the namespace holds when this starts is written once it
/// settles: a document merged or recovered at load was changed before
/// anything could subscribe. A failed write is retried after [`MAX_DELAY`],
/// backing off to a minute while it keeps failing.
#[must_use = "the handle is how a mount stops its persister before the final flush"]
pub fn spawn_persister<F>(namespace: Arc<Mutex<Namespace>>, flush: F) -> tokio::task::JoinHandle<()>
where
    F: FnMut(&Arc<Mutex<Namespace>>, Pending) -> Pending + Send + 'static,
{
    let changes = namespace
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .subscribe_changes();
    tokio::spawn(persist(namespace, changes, flush))
}

async fn persist<F>(
    namespace: Arc<Mutex<Namespace>>,
    mut changes: watch::Receiver<()>,
    mut flush: F,
) where
    F: FnMut(&Arc<Mutex<Namespace>>, Pending) -> Pending,
{
    // What the last successful writes wrote: the document's heads and the
    // sync progress's generation.
    let mut written_heads: Option<Vec<automerge::ChangeHash>> = None;
    let mut written_progress: Option<u64> = None;
    let mut retry = MAX_DELAY;
    // Start dirty: what loading changed was announced before this
    // subscribed.
    let mut dirty = true;
    loop {
        if !dirty && changes.changed().await.is_err() {
            // The namespace is gone; nothing is left to write.
            return;
        }
        settle(&mut changes).await;
        let (heads, progress) = {
            let mut namespace = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (namespace.heads(), namespace.peer_sync_generation())
        };
        let pending = Pending {
            document: written_heads.as_ref() != Some(&heads),
            progress: written_progress != Some(progress),
        };
        let failed = if pending.any() {
            flush(&namespace, pending)
        } else {
            Pending::default()
        };
        // What failed stays pending: its mark keeps what was last written.
        if pending.document && !failed.document {
            written_heads = Some(heads);
        }
        if pending.progress && !failed.progress {
            written_progress = Some(progress);
        }
        dirty = failed.any();
        if dirty {
            tokio::time::sleep(retry).await;
            retry = retry.saturating_mul(2).min(MAX_RETRY);
        } else {
            retry = MAX_DELAY;
        }
    }
}

/// Returns once the namespace has been quiet for [`QUIET`], or
/// [`MAX_DELAY`] after this was called if changes keep arriving.
async fn settle(changes: &mut watch::Receiver<()>) {
    let began = Instant::now();
    let mut last = began;
    changes.borrow_and_update();
    loop {
        let due = last
            .checked_add(QUIET)
            .unwrap_or(last)
            .min(began.checked_add(MAX_DELAY).unwrap_or(began));
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(due) => return,
            moved = changes.changed() => {
                if moved.is_err() {
                    return;
                }
                last = Instant::now();
                // Edits of a burst arriving meanwhile are seen at the next
                // look; a burst wakes this once a step, not once an edit.
                tokio::time::sleep_until(
                    last.checked_add(SETTLE_STEP).unwrap_or(last).min(due),
                )
                .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namespace::ROOT_INO;

    type Flushes = Arc<Mutex<Vec<(Instant, Pending)>>>;

    fn recording(
        failing: Arc<std::sync::atomic::AtomicBool>,
    ) -> (
        Flushes,
        impl FnMut(&Arc<Mutex<Namespace>>, Pending) -> Pending + Send + 'static,
    ) {
        let flushes: Flushes = Arc::default();
        let log = Arc::clone(&flushes);
        let flush = move |_: &Arc<Mutex<Namespace>>, pending: Pending| {
            log.lock().unwrap().push((Instant::now(), pending));
            if failing.load(std::sync::atomic::Ordering::SeqCst) {
                pending
            } else {
                Pending::default()
            }
        };
        (flushes, flush)
    }

    fn change(namespace: &Arc<Mutex<Namespace>>, name: &str) {
        namespace
            .lock()
            .unwrap()
            .create_dir(ROOT_INO, name, 1)
            .unwrap();
    }

    /// A change is on disk within the bound the 100 ms poll kept
    /// (500 ms after it, plus one look), a continuous run at least every
    /// 3 s, and an idle namespace is never written.
    #[tokio::test(start_paused = true)]
    async fn a_change_is_written_within_the_quiet_bound_and_idle_writes_nothing() {
        let namespace = Arc::new(Mutex::new(Namespace::new("persist").unwrap()));
        let (flushes, flush) = recording(Arc::default());
        let started = Instant::now();
        let persister = spawn_persister(Arc::clone(&namespace), flush);

        // What loading left is written once it settles.
        tokio::time::sleep(Duration::from_secs(1)).await;
        {
            let flushes = flushes.lock().unwrap().clone();
            assert_eq!(flushes.len(), 1, "the loaded namespace was written once");
            assert!(flushes[0].0.duration_since(started) <= QUIET.saturating_add(SETTLE_STEP));
            assert_eq!(
                flushes[0].1,
                Pending {
                    document: true,
                    progress: true
                }
            );
        }

        // Idle: an hour passes and nothing is written.
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(
            flushes.lock().unwrap().len(),
            1,
            "an idle namespace was written"
        );

        // One change: written within QUIET (+ a look) of it.
        let changed_at = Instant::now();
        change(&namespace, "one");
        tokio::time::sleep(Duration::from_secs(2)).await;
        {
            let flushes = flushes.lock().unwrap().clone();
            assert_eq!(flushes.len(), 2);
            let (at, pending) = flushes[1];
            assert!(
                at.duration_since(changed_at) >= QUIET,
                "written before it settled"
            );
            assert!(
                at.duration_since(changed_at) <= QUIET.saturating_add(SETTLE_STEP),
                "written {:?} after the change",
                at.duration_since(changed_at)
            );
            assert_eq!(
                pending,
                Pending {
                    document: true,
                    progress: false
                }
            );
        }

        // A continuous run (a change every 200 ms for 10 s): every change
        // is written within MAX_DELAY (+ a look), and the last within QUIET
        // (+ a look).
        let mut changed = Vec::new();
        for index in 0..50 {
            changed.push(Instant::now());
            change(&namespace, &format!("run-{index}"));
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        let run: Vec<Instant> = flushes.lock().unwrap()[2..]
            .iter()
            .map(|(at, _)| *at)
            .collect();
        assert!(run.len() >= 3, "a 10 s run was written {} times", run.len());
        assert!(run.len() <= 6, "a 10 s run was written {} times", run.len());
        for at in &changed {
            let written = run
                .iter()
                .find(|written| *written >= at)
                .expect("a change of the run was never written");
            assert!(
                written.duration_since(*at) <= MAX_DELAY.saturating_add(SETTLE_STEP),
                "a change of a continuous run waited {:?} for its write",
                written.duration_since(*at)
            );
        }
        let last = changed.last().unwrap();
        assert!(
            run.last().unwrap().duration_since(*last) <= QUIET.saturating_add(SETTLE_STEP),
            "the run's last change was written late"
        );
        persister.abort();
    }

    /// Sync progress lives beside the document; a change to it alone is
    /// written without rewriting the document.
    #[tokio::test(start_paused = true)]
    async fn sync_progress_alone_is_written_alone() {
        let namespace = Arc::new(Mutex::new(Namespace::new("persist").unwrap()));
        let (flushes, flush) = recording(Arc::default());
        let persister = spawn_persister(Arc::clone(&namespace), flush);
        tokio::time::sleep(Duration::from_secs(1)).await;
        {
            let mut namespace = namespace.lock().unwrap();
            let mut state = automerge::sync::State::new();
            state.shared_heads = namespace.heads();
            namespace.finish_sync_with([7; 32], &state);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        let flushes = flushes.lock().unwrap();
        assert_eq!(flushes.len(), 2);
        assert_eq!(
            flushes[1].1,
            Pending {
                document: false,
                progress: true
            }
        );
        drop(flushes);
        persister.abort();
    }

    /// A write that failed is retried without another change, backing off
    /// while it keeps failing, and stops once it succeeds.
    #[tokio::test(start_paused = true)]
    async fn a_failed_write_is_retried_until_it_succeeds() {
        let namespace = Arc::new(Mutex::new(Namespace::new("persist").unwrap()));
        let failing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (flushes, flush) = recording(Arc::clone(&failing));
        let persister = spawn_persister(Arc::clone(&namespace), flush);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let attempts = flushes.lock().unwrap().len();
        assert!(attempts >= 2, "a failed write was not retried");
        assert!(
            attempts <= 4,
            "a failing write was retried {attempts} times in 10 s"
        );
        failing.store(false, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(120)).await;
        let succeeded = flushes.lock().unwrap().len();
        assert!(
            succeeded > attempts,
            "the write was not retried after failing"
        );
        assert!(
            flushes.lock().unwrap().last().unwrap().1.document,
            "the retry did not write the document"
        );
        tokio::time::sleep(Duration::from_secs(3600)).await;
        assert_eq!(
            flushes.lock().unwrap().len(),
            succeeded,
            "the persister kept writing after a success"
        );
        persister.abort();
    }

    /// The thread the persister runs on is not scheduled at all while the
    /// namespace is idle (the poll it replaces woke it ten times a second).
    #[cfg(target_os = "linux")] // the thread's wakeups are read from /proc
    #[test]
    fn an_idle_persister_never_wakes() {
        let namespace = Arc::new(Mutex::new(Namespace::new("persist").unwrap()));
        let (flushes, flush) = recording(Arc::default());
        let (written_tx, written_rx) = std::sync::mpsc::channel();
        let (_thread, tid) = tilo_test_env::idle::spawned_thread("fs-persist-idle", || {
            let namespace = Arc::clone(&namespace);
            std::thread::Builder::new()
                .name("fs-persist-idle".into())
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    runtime.block_on(async move {
                        let _persister = spawn_persister(namespace, flush);
                        tokio::time::sleep(QUIET.saturating_add(SETTLE_STEP.saturating_mul(2)))
                            .await;
                        written_tx.send(()).unwrap();
                        std::future::pending::<()>().await;
                    });
                })
                .unwrap();
        });
        written_rx
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        assert_eq!(flushes.lock().unwrap().len(), 1);
        tilo_test_env::idle::assert_stays_asleep(&tid, "namespace persister");

        // Still alive: a change wakes it and is written.
        change(&namespace, "after-idle");
        let deadline = std::time::Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .unwrap();
        while flushes.lock().unwrap().len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the change was never written"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
