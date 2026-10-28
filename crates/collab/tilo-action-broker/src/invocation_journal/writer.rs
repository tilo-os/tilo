//! The journal's one writer: a thread of its own that makes each transition
//! durable in the order the journal staged it.
//!
//! A transition's data and directory syncs take seconds on a loaded disk.
//! The journal is used from the broker's single-threaded loop, which also
//! answers every peer action, catalogue and liveness probe before their
//! deadlines, so the journal never syncs on its caller's thread. It stages a
//! transition in memory, hands its bytes here, and gives the caller a
//! [`Durable`] ticket to wait on before anything acts on the transition:
//! nothing is acknowledged, delivered or answered until the ticket resolves.
//!
//! A started write always finishes, also when nobody waits for its ticket
//! any more. A write that failed before any of its bytes were durable left
//! the store exactly as the last durable write did, so the next write is
//! simply attempted again; a disk that is once full may be free again. Only a
//! write that may be durable although its ticket reports failure (the store
//! renamed but its directory not yet synced) fails every later one: the
//! journal then requires recovery, as a failed barrier always did.

#[cfg(test)]
use super::BarrierFailure;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};

/// What this writer asked of the disk. `elide` skips the barriers themselves
/// for tests that measure size rather than durability.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Barriers {
    pub writes: u64,
    pub file: u64,
    pub directory: u64,
    pub elide: bool,
}

/// Counts every write and barrier, and skips the barriers when a size test asks.
#[derive(Clone, Default)]
pub(super) struct Counted {
    #[cfg(test)]
    barriers: Arc<Mutex<Barriers>>,
}

impl Counted {
    #[cfg(test)]
    pub(super) fn barriers(&self) -> Barriers {
        *self
            .barriers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    #[cfg(test)]
    pub(super) fn elide(&self) {
        self.barriers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .elide = true;
    }
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "counted only under test")
    )]
    fn write(&self) {
        #[cfg(test)]
        {
            let mut barriers = self
                .barriers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            barriers.writes = barriers.writes.saturating_add(1);
        }
    }
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "counted only under test")
    )]
    fn file(&self, barrier: impl FnOnce() -> std::io::Result<()>) -> std::io::Result<()> {
        #[cfg(test)]
        {
            let mut barriers = self
                .barriers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            barriers.file = barriers.file.saturating_add(1);
            if barriers.elide {
                return Ok(());
            }
        }
        barrier()
    }
    #[cfg_attr(
        not(test),
        expect(clippy::unused_self, reason = "counted only under test")
    )]
    fn directory(&self, barrier: impl FnOnce() -> std::io::Result<()>) -> std::io::Result<()> {
        #[cfg(test)]
        {
            let mut barriers = self
                .barriers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            barriers.directory = barriers.directory.saturating_add(1);
            if barriers.elide {
                return Ok(());
            }
        }
        barrier()
    }
}

/// One staged transition's place in the writer's order.
pub(super) struct Ticket {
    state: Mutex<Resolution>,
    settled: Condvar,
    notify: tokio::sync::Notify,
}

type Then = Box<dyn FnOnce(Result<(), String>) + Send>;

#[derive(Default)]
struct Resolution {
    outcome: Option<Result<(), String>>,
    then: Vec<Then>,
}

impl Ticket {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(Resolution::default()),
            settled: Condvar::new(),
            notify: tokio::sync::Notify::new(),
        })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, Resolution> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// Every `then` runs, in the order given, before anyone waiting sees
    /// the outcome: a waiter finds what they did done.
    fn resolve(&self, outcome: &Result<(), String>) {
        loop {
            let then = {
                let mut state = self.lock();
                if state.then.is_empty() {
                    state.outcome = Some(outcome.clone());
                    break;
                }
                std::mem::take(&mut state.then)
            };
            for then in then {
                then(outcome.clone());
            }
        }
        self.settled.notify_all();
        self.notify.notify_waiters();
    }
    pub(super) fn outcome(&self) -> Option<Result<(), String>> {
        self.lock().outcome.clone()
    }
}

/// A transition staged in the journal, durable once this resolves `Ok`.
/// Nothing may act on the transition before: no acknowledgement, delivery or
/// answer. Dropping it unwaited never cancels the write.
#[must_use = "nothing may act on a transition before it is durable"]
pub(crate) struct Durable(Option<Arc<Ticket>>);

impl Durable {
    /// Nothing to wait for: the transition was already durable.
    pub(crate) const fn settled() -> Self {
        Self(None)
    }
    pub(super) fn of(ticket: Arc<Ticket>) -> Self {
        Self(Some(ticket))
    }
    pub(super) const fn ticket(&self) -> Option<&Arc<Ticket>> {
        self.0.as_ref()
    }
    /// Whether the write is still being made.
    pub(crate) fn pending(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|ticket| ticket.outcome().is_none())
    }
    /// Wait for the write without holding the thread: the broker's loop
    /// goes on answering meanwhile.
    pub(crate) async fn wait(self) -> Result<(), String> {
        let Some(ticket) = self.0 else {
            return Ok(());
        };
        loop {
            let notified = ticket.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(outcome) = ticket.outcome() {
                return outcome;
            }
            notified.await;
        }
    }
    /// Run `then` with the write's outcome once it resolves: at once if it
    /// has, else on the writer's thread, so it must only hand the outcome on.
    pub(crate) fn then(self, then: impl FnOnce(Result<(), String>) + Send + 'static) {
        let Some(ticket) = self.0 else {
            return then(Ok(()));
        };
        let outcome = {
            let mut state = ticket.lock();
            match state.outcome.clone() {
                Some(outcome) => outcome,
                None => return state.then.push(Box::new(then)),
            }
        };
        then(outcome);
    }
    /// Wait for the write on a thread that may block: never the broker's
    /// loop, whose answers would all wait with it.
    pub(crate) fn wait_blocking(self) -> Result<(), String> {
        let Some(ticket) = self.0 else {
            return Ok(());
        };
        let mut state = ticket.lock();
        loop {
            if let Some(outcome) = state.outcome.clone() {
                return outcome;
            }
            state = ticket
                .settled
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// The bytes of one staged transition.
pub(super) enum Write {
    /// One entry line appended to the store and its data barrier.
    Append(Vec<u8>),
    /// The whole store rewritten as a snapshot through an atomic rename.
    Snapshot(Vec<u8>),
    /// The snapshot a journal recovered from a failed barrier: every write
    /// staged before it was stopped and touched nothing, and once it is
    /// durable the writer goes on with the writes staged after it.
    Recovery(Vec<u8>),
    /// One line appended to the side file `name` beside the store, created
    /// on first use. Its failure is reported, but leaves the store's own
    /// records untouched, so it fails no later write.
    Beside { name: &'static str, line: Vec<u8> },
}

/// Test injections that travel with a write to the thread making it.
#[derive(Default)]
pub(super) struct Injected {
    #[cfg(test)]
    pub fail: Option<BarrierFailure>,
    /// Run once the write is durable, before its ticket resolves, while the
    /// writer is still held.
    #[cfg(test)]
    pub after: Option<Box<dyn FnOnce() + Send>>,
    /// Durable, but the caller is told the process crashed before acting.
    /// The store now holds an entry this writer never recounts, so only a
    /// restart may append to it.
    #[cfg(test)]
    pub crash_after: bool,
    /// The write was made durable but could not be confirmed: a later write
    /// could not tell whether the store already holds it.
    #[cfg(test)]
    pub unconfirmed: bool,
    /// Run before the write starts, as a slow disk holds it.
    #[cfg(test)]
    pub hold: Option<Arc<dyn Fn() + Send + Sync>>,
}

struct Job {
    write: Write,
    ticket: Arc<Ticket>,
    injected: Injected,
}

/// What a write left the writer in.
enum Made {
    Durable,
    /// Failed with nothing of it durable: the next write may go on.
    Withdrawn,
    /// Failed, and whether the store holds it is unknown: nothing may be
    /// written after it.
    Unconfirmed,
    /// Not attempted: an earlier write stopped the writer. It touched nothing.
    Stopped(String),
}

/// The writer thread, its order and its failure.
pub(super) struct Writer {
    jobs: Option<mpsc::Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
    failed: Arc<AtomicBool>,
}

impl Made {
    fn withdrew(written: Result<(), &'static str>) -> Self {
        if written == Err(FAILED) {
            Self::Unconfirmed
        } else {
            Self::Withdrawn
        }
    }
}

impl Writer {
    /// Start the writer of `root`, appending to `tail` (`None`: the first
    /// transition compacts). It holds the store's `lock` until its last
    /// write is done, so no other writer opens the store before then.
    pub(super) fn start(
        root: PathBuf,
        tail: Option<File>,
        lock: advisory_lock::FileLock,
        counted: Counted,
    ) -> Result<Self, String> {
        let (jobs, received) = mpsc::channel::<Job>();
        let failed = Arc::new(AtomicBool::new(false));
        let failing = failed.clone();
        let stopping = Arc::new(Mutex::new(None));
        let thread = std::thread::Builder::new()
            .name("invocation-journal".into())
            .spawn(move || {
                let _lock = lock;
                let mut tail = tail;
                for job in received {
                    let beside = matches!(job.write, Write::Beside { .. });
                    let recovery = matches!(job.write, Write::Recovery(_));
                    let snapshot_written =
                        matches!(job.write, Write::Snapshot(_) | Write::Recovery(_));
                    let made = if failing.load(Ordering::Acquire) && !recovery {
                        // Everything staged before this one kept its own
                        // outcome; this one never touched the disk.
                        Made::Stopped(
                            stopping
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .clone()
                                .unwrap_or_else(|| FAILED.to_string()),
                        )
                    } else {
                        match make_durable(&root, &mut tail, job.write, &job.injected, &counted) {
                            Ok(()) => Made::Durable,
                            Err(error) => Made::withdrew(Err(error)),
                        }
                    };
                    let error = match &made {
                        Made::Durable => None,
                        Made::Withdrawn => Some(WITHDRAWN.to_string()),
                        Made::Unconfirmed => Some(FAILED.to_string()),
                        Made::Stopped(why) => Some(why.clone()),
                    };
                    // A write taken back left the store exactly as the last
                    // durable one did, so the next one still belongs at the
                    // same end (a snapshot write renamed a new store, so its
                    // end is only known from its bytes).
                    if matches!(made, Made::Withdrawn) && snapshot_written {
                        tail = None;
                    }
                    let fatal = matches!(made, Made::Unconfirmed);
                    #[cfg(test)]
                    let (error, fatal) = {
                        let (mut error, mut fatal) = (error, fatal);
                        if error.is_none()
                            && let Some(after) = job.injected.after
                        {
                            after();
                        }
                        if job.injected.crash_after {
                            error = Some("injected admission-before-delivery crash".to_string());
                            fatal = true;
                        }
                        if job.injected.unconfirmed {
                            error = Some(FAILED.to_string());
                            fatal = true;
                        }
                        (error, fatal)
                    };
                    // The recovered snapshot is the store's whole content:
                    // the writes after it are made again. Until it is
                    // durable, the store still holds what the journal cannot
                    // account for, and the writer stays stopped.
                    if recovery && error.is_none() {
                        failing.store(false, Ordering::Release);
                        *stopping
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    }
                    if fatal && !beside {
                        failing.store(true, Ordering::Release);
                        tail = None;
                        if let Some(why) = &error {
                            *stopping
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(why.clone());
                        }
                    }
                    let outcome = error.map_or(Ok(()), Err);
                    job.ticket.resolve(&outcome);
                }
            })
            .map_err(|_| "invocation journal writer unavailable")?;
        Ok(Self {
            jobs: Some(jobs),
            thread: Some(thread),
            failed,
        })
    }

    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Queue `write` after every write staged before it.
    pub(super) fn stage(&self, write: Write, injected: Injected) -> Arc<Ticket> {
        let ticket = Ticket::new();
        let job = Job {
            write,
            ticket: ticket.clone(),
            injected,
        };
        let sent = self.jobs.as_ref().map(|jobs| jobs.send(job));
        if !matches!(sent, Some(Ok(()))) {
            self.failed.store(true, Ordering::Release);
            ticket.resolve(&Err("invocation journal writer unavailable".into()));
        }
        ticket
    }
}

impl Drop for Writer {
    /// Every staged write finishes, and the store's lock is let go, before
    /// the journal is gone: a writer opened after it finds the store whole.
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            // Dropped by a ticket's callback on the writer itself (it held
            // the journal's last owner): that thread cannot wait for itself.
            // It still makes the writes queued behind this one, then ends
            // and lets the lock go.
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

pub(super) const FAILED: &str = "invocation journal requires recovery after a failed barrier";

/// A write that reached no durable byte and was taken back: nothing was
/// acknowledged and the store is unchanged, so the journal carries on.
const WITHDRAWN: &str = "invocation journal write failed";

/// Whether `write` left nothing of itself durable, so the journal goes on as
/// if it had never been staged: a withdrawn write is not the failed barrier
/// that stops the writer.
pub(super) fn withdrew(outcome: &Result<(), String>) -> bool {
    matches!(outcome, Err(error) if error != FAILED)
}

fn make_durable(
    root: &Path,
    tail: &mut Option<File>,
    write: Write,
    injected: &Injected,
    counted: &Counted,
) -> Result<(), &'static str> {
    #[cfg(test)]
    if let Some(hold) = &injected.hold {
        hold();
    }
    match write {
        Write::Snapshot(bytes) | Write::Recovery(bytes) => {
            *tail = None;
            *tail = Some(write_snapshot(root, &bytes, injected, counted)?);
            Ok(())
        }
        Write::Append(bytes) => {
            let file = tail.as_mut().ok_or(FAILED)?;
            // A failed append is taken back when the disk lets it, and the
            // length it was before tells whether it was.
            let len = file.metadata().map_err(|_| FAILED)?.len();
            let appended = append(file, &bytes, injected, counted);
            if appended.is_err() && file.metadata().is_ok_and(|after| after.len() == len) {
                return Err(WITHDRAWN);
            }
            appended
        }
        Write::Beside { name, line } => append_beside(root, name, &line),
    }
}

/// `line` appended to `root`'s side file `name` and its data barrier; the
/// directory's too when this created the file, so its name is durable.
fn append_beside(root: &Path, name: &str, line: &[u8]) -> Result<(), &'static str> {
    let path = root.join(name);
    let (mut file, created) = match OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => (file, true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
            OpenOptions::new()
                .append(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|_| "side file unavailable")?,
            false,
        ),
        Err(_) => return Err("side file unavailable"),
    };
    super::private_file(&file).map_err(|_| "side file unavailable")?;
    file.write_all(line)
        .and_then(|()| file.sync_data())
        .map_err(|_| "side file write not durable")?;
    if created {
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "side file write not durable")?;
    }
    Ok(())
}

/// One entry and its data barrier. A failed entry is taken back if the disk
/// lets us: no caller was told of it.
pub(super) fn append(
    file: &mut File,
    bytes: &[u8],
    injected: &Injected,
    counted: &Counted,
) -> Result<(), &'static str> {
    #[cfg(test)]
    let fail = injected.fail;
    #[cfg(not(test))]
    let _ = injected;
    let len = file
        .metadata()
        .map_err(|_| "invocation journal write failed")?
        .len();
    counted.write();
    let mut durable = file
        .write_all(bytes)
        .map_err(|_| "invocation journal write failed");
    #[cfg(test)]
    if durable.is_ok() && fail == Some(BarrierFailure::File) {
        durable = Err("injected file barrier failure");
    }
    if durable.is_ok() {
        durable = counted
            .file(|| file.sync_data())
            .map_err(|_| "invocation journal file barrier failed");
    }
    if let Err(error) = durable {
        let _ = file.set_len(len);
        // Bytes the disk kept are durable whatever the caller is told, so only
        // a store that reads exactly as before the write is taken back.
        if !file.metadata().is_ok_and(|after| after.len() == len) {
            return Err(FAILED);
        }
        return Err(error);
    }
    // The entry is in the store; only the barrier that would confirm it for
    // good did not happen, so it may be durable and nothing was acknowledged.
    #[cfg(test)]
    if fail == Some(BarrierFailure::Directory) {
        return Err(FAILED);
    }
    Ok(())
}

/// The whole store as one snapshot through an atomic rename, left open for
/// appends: later entries append to exactly the renamed file.
pub(super) fn write_snapshot(
    root: &Path,
    bytes: &[u8],
    injected: &Injected,
    counted: &Counted,
) -> Result<File, &'static str> {
    #[cfg(test)]
    let fail = injected.fail;
    #[cfg(not(test))]
    let _ = injected;
    let temporary = root.join(format!(".write-{}", uuid::Uuid::new_v4()));
    let result: Result<File, &'static str> = (|| {
        let mut file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|_| "invocation journal temporary file unavailable")?;
        counted.write();
        file.write_all(bytes)
            .map_err(|_| "invocation journal write failed")?;
        #[cfg(test)]
        if fail == Some(BarrierFailure::File) {
            return Err("injected file barrier failure");
        }
        counted
            .file(|| file.sync_all())
            .map_err(|_| "invocation journal file barrier failed")?;
        std::fs::rename(&temporary, root.join("records.json"))
            .map_err(|_| "invocation journal rename failed")?;
        // From here on the new snapshot may be the store for good: a failure
        // to confirm it leaves later appends unable to know what it holds.
        #[cfg(test)]
        if fail == Some(BarrierFailure::Directory) {
            return Err(FAILED);
        }
        counted
            .directory(|| File::open(root).and_then(|directory| directory.sync_all()))
            .map_err(|_| FAILED)?;
        Ok(file)
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}
