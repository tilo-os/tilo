//! Asynchronous publication of workspace file capabilities to `quick-fs`.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Instant;

const DEFAULT_FS_AUTHORITY_PUBLISHER: &str = "/usr/bin/fs";
/// `fs publish-authority`'s exit status on a device without the workspace
/// governor key: a joined device never signs authority, so there is nothing
/// to publish and nothing to retry.
const PUBLISHER_NOT_GOVERNOR_EXIT: i32 = 77;
const MAX_FS_AUTHORITY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
/// A publisher that is not installed will not appear within seconds; a host
/// session without `quick-fs` would otherwise spawn and warn every five.
const MAX_MISSING_PUBLISHER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(300);
/// Recent publisher attempts kept so a stalled publication can say why.
const MAX_RECORDED_FS_AUTHORITY_ATTEMPTS: usize = 16;
/// Tail of the publisher's stderr kept per recorded attempt.
const MAX_RECORDED_FS_AUTHORITY_STDERR: usize = 2048;

#[derive(Clone, Debug)]
struct FsAuthorityPublication {
    acl: PathBuf,
    publisher: PathBuf,
}

#[derive(Debug)]
struct FsAuthorityPublicationAttempt {
    at: Instant,
    acl: PathBuf,
    outcome: String,
}

#[derive(Default)]
struct FsAuthorityPublicationQueue {
    pending: Option<FsAuthorityPublication>,
    running: bool,
    attempts: VecDeque<FsAuthorityPublicationAttempt>,
}

impl FsAuthorityPublicationQueue {
    fn record(&mut self, acl: PathBuf, outcome: String) {
        if self.attempts.len() == MAX_RECORDED_FS_AUTHORITY_ATTEMPTS {
            self.attempts.pop_front();
        }
        self.attempts.push_back(FsAuthorityPublicationAttempt {
            at: Instant::now(),
            acl,
            outcome,
        });
    }

    fn report(&self) -> String {
        let now = Instant::now();
        let mut report = format!(
            "running={} pending={}; recent publisher attempts (oldest first):",
            self.running,
            self.pending.as_ref().map_or_else(
                || "none".to_string(),
                |pending| pending.acl.display().to_string()
            ),
        );
        if self.attempts.is_empty() {
            report.push_str(" none");
        }
        for attempt in &self.attempts {
            let _ = write!(
                report,
                "\n  {:?} ago, acl {}: {}",
                now.saturating_duration_since(attempt.at),
                attempt.acl.display(),
                attempt.outcome
            );
        }
        report
    }
}

fn stderr_tail(stderr: &[u8]) -> String {
    let start = stderr
        .len()
        .saturating_sub(MAX_RECORDED_FS_AUTHORITY_STDERR);
    String::from_utf8_lossy(stderr.get(start..).unwrap_or_default())
        .trim()
        .to_string()
}

fn fs_authority_publication_queue() -> &'static Mutex<FsAuthorityPublicationQueue> {
    static QUEUE: OnceLock<Mutex<FsAuthorityPublicationQueue>> = OnceLock::new();
    QUEUE.get_or_init(|| Mutex::new(FsAuthorityPublicationQueue::default()))
}

/// Signalled whenever the publication worker retires, so a waiter observes
/// the idle transition itself instead of sampling the queue on a timer.
fn fs_authority_publication_retired() -> &'static Condvar {
    static RETIRED: OnceLock<Condvar> = OnceLock::new();
    RETIRED.get_or_init(Condvar::new)
}

fn fs_authority_publisher() -> PathBuf {
    std::env::var_os("TILO_FS_AUTHORITY_PUBLISHER")
        .filter(|value| !value.is_empty())
        .map_or_else(
            || PathBuf::from(DEFAULT_FS_AUTHORITY_PUBLISHER),
            PathBuf::from,
        )
}

/// Coalesce exports and submit them outside the compositor's mutation path.
/// The filesystem authority socket may be unavailable while its service is
/// starting; capped-backoff retries cover that normal boot ordering and a
/// later capability mutation always replaces the queued projection.
pub fn schedule_fs_authority_publication(acl: PathBuf) {
    // Unit tests exercise this only with an explicit harmless publisher;
    // never let an isolated tree submit a fixture ACL to a real service.
    if cfg!(test) && std::env::var_os("TILO_FS_AUTHORITY_PUBLISHER").is_none() {
        return;
    }
    let publication = FsAuthorityPublication {
        acl,
        publisher: fs_authority_publisher(),
    };
    let Ok(mut queue) = fs_authority_publication_queue().lock() else {
        tracing::warn!("filesystem authority publication queue is unavailable");
        return;
    };
    queue.pending = Some(publication);
    if queue.running {
        // A worker waiting for a missing publisher to be installed takes
        // this newer projection at once rather than at its next retry.
        #[cfg(target_os = "linux")]
        publisher_wait::wake_worker();
        return;
    }
    queue.running = true;
    let spawned = std::thread::Builder::new()
        .name("tilo-fs-authority-publish".to_string())
        .spawn(run_fs_authority_publication_queue);
    if let Err(error) = spawned {
        queue.running = false;
        fs_authority_publication_retired().notify_all();
        tracing::warn!(%error, "could not start filesystem authority publisher");
    }
}

/// Run the publisher once. Its stderr is captured so a failing publisher
/// explains itself in the journal and in the attempt history, not only by
/// exit status.
fn run_publisher(publication: &FsAuthorityPublication) -> std::io::Result<std::process::Output> {
    std::process::Command::new(&publication.publisher)
        .arg("publish-authority")
        .arg("--acl")
        .arg(&publication.acl)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
}

fn run_fs_authority_publication_queue() {
    loop {
        // Taking the last publication and retiring happen under one lock: a
        // schedule in between would see `running` still set, leave its ACL
        // pending and return, and the retiring worker would drop it.
        let publication = match fs_authority_publication_queue().lock() {
            Ok(mut queue) => {
                let publication = queue.pending.take();
                if publication.is_none() {
                    queue.running = false;
                }
                publication
            }
            Err(error) => {
                tracing::warn!(%error, "filesystem authority publication queue is poisoned");
                return;
            }
        };
        let Some(mut publication) = publication else {
            fs_authority_publication_retired().notify_all();
            return;
        };
        let mut retry_delay = std::time::Duration::from_millis(100);
        // One warning per publication; the retries after it are the same story.
        let mut failures = 0_u32;
        loop {
            let result = run_publisher(&publication);
            let mut publisher_missing = false;
            let (succeeded, outcome) = match result {
                Ok(output) if output.status.success() => {
                    if failures > 0 {
                        tracing::info!(
                            failures,
                            acl = %publication.acl.display(),
                            "filesystem authority published after retrying"
                        );
                    }
                    (true, "published".to_string())
                }
                Ok(output) if output.status.code() == Some(PUBLISHER_NOT_GOVERNOR_EXIT) => {
                    tracing::debug!(
                        acl = %publication.acl.display(),
                        "this device does not govern the workspace; its authority is published by the primary device"
                    );
                    (
                        true,
                        "not the workspace governor; nothing to publish".to_string(),
                    )
                }
                Ok(output) => {
                    let stderr = stderr_tail(&output.stderr);
                    if failures == 0 {
                        tracing::warn!(
                            status = ?output.status,
                            %stderr,
                            acl = %publication.acl.display(),
                            "filesystem authority publication failed; retrying quietly"
                        );
                    } else {
                        tracing::debug!(status = ?output.status, %stderr, "filesystem authority publication failed again");
                    }
                    (false, format!("{}; stderr: {stderr:?}", output.status))
                }
                Err(error) => {
                    publisher_missing = error.kind() == std::io::ErrorKind::NotFound;
                    if failures == 0 {
                        tracing::warn!(
                            %error,
                            publisher = %publication.publisher.display(),
                            "filesystem authority publisher could not start; retrying quietly"
                        );
                    } else {
                        tracing::debug!(%error, "filesystem authority publisher could not start again");
                    }
                    (
                        false,
                        format!(
                            "publisher {} could not start: {error}",
                            publication.publisher.display()
                        ),
                    )
                }
            };
            if let Ok(mut queue) = fs_authority_publication_queue().lock() {
                queue.record(publication.acl.clone(), outcome);
            }
            if succeeded {
                break;
            }
            // A new mutation supersedes the failed projection immediately;
            // otherwise retain this exact latest ACL until the daemon is
            // ready. The delay is bounded, while the background worker is
            // deliberately persistent rather than silently losing a grant.
            failures = failures.saturating_add(1);
            if !publisher_missing || !wait_for_missing_publisher(&publication.publisher) {
                std::thread::sleep(retry_delay);
            }
            retry_delay = next_retry_delay(retry_delay, publisher_missing);
            if let Ok(mut queue) = fs_authority_publication_queue().lock()
                && let Some(newer) = queue.pending.take()
            {
                publication = newer;
                retry_delay = std::time::Duration::from_millis(100);
                failures = 0;
            }
        }
    }
}

/// Wait, with no timer, until the missing `publisher` is created or a newer
/// projection is queued. `false` when that cannot be waited for (the
/// publisher's directory cannot be watched, the path names something that
/// exists but would not start, or this is not Linux): the caller keeps its
/// doubling timer then.
fn wait_for_missing_publisher(publisher: &std::path::Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let superseded = || {
            fs_authority_publication_queue()
                .lock()
                .map_or(true, |queue| queue.pending.is_some())
        };
        let Some(wake) = publisher_wait::worker_wake() else {
            return false;
        };
        !matches!(
            publisher_wait::wait(publisher, wake, &superseded),
            publisher_wait::Wait::Unwatchable
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = publisher;
        false
    }
}

/// Waiting for a missing publisher to be installed.
///
/// A creation-only inotify watch on the publisher's directory: creating or
/// renaming an entry into it is all it reports. Opening, reading or running
/// the directory's other files (`/usr/bin` is busy) reports nothing, which
/// is why this is not the `notify` crate, whose watch includes opens: that
/// woke a busy shard's compositor 47 times a second.
#[cfg(target_os = "linux")]
mod publisher_wait {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::OnceLock;

    /// Created, moved in, or the directory itself going away (which ends
    /// the watch: the caller falls back to its timer).
    const MASK: u32 = libc::IN_CREATE
        | libc::IN_MOVED_TO
        | libc::IN_ONLYDIR
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF;

    /// Why a wait ended.
    #[derive(Debug, PartialEq, Eq)]
    pub(super) enum Wait {
        /// An entry with the publisher's name appeared: try it now.
        Created,
        /// A newer projection is queued: publish that instead.
        Superseded,
        /// Nothing can be waited for; keep the timer.
        Unwatchable,
    }

    /// The worker's wake, written when a newer projection is queued.
    pub(super) fn worker_wake() -> Option<BorrowedFd<'static>> {
        static WAKE: OnceLock<Option<OwnedFd>> = OnceLock::new();
        WAKE.get_or_init(new_eventfd)
            .as_ref()
            .map(std::os::fd::AsFd::as_fd)
    }

    pub(super) fn new_eventfd() -> Option<OwnedFd> {
        // SAFETY: plain syscall; a non-negative result is a new descriptor we own.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        // SAFETY: `fd` was just returned by eventfd and is owned by nothing else.
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    }

    pub(super) fn wake_worker() {
        if let Some(wake) = worker_wake() {
            signal(wake);
        }
    }

    pub(super) fn signal(wake: BorrowedFd<'_>) {
        let one: u64 = 1;
        // SAFETY: writes 8 bytes from a live u64 to an eventfd we hold open.
        let _ = unsafe { libc::write(wake.as_raw_fd(), (&raw const one).cast(), 8) };
    }

    fn drain(wake: BorrowedFd<'_>) {
        let mut count: u64 = 0;
        // SAFETY: reads 8 bytes into a live u64 from a nonblocking eventfd.
        let _ = unsafe { libc::read(wake.as_raw_fd(), (&raw mut count).cast(), 8) };
    }

    /// Block until an entry named like `publisher` is created in its
    /// directory, or `wake` is signalled while `superseded` holds.
    pub(super) fn wait(
        publisher: &Path,
        wake: BorrowedFd<'_>,
        superseded: &dyn Fn() -> bool,
    ) -> Wait {
        let (Some(directory), Some(name)) = (publisher.parent(), publisher.file_name()) else {
            return Wait::Unwatchable;
        };
        // A bare name is looked up on PATH: no one directory to watch.
        if !publisher.is_absolute() {
            return Wait::Unwatchable;
        }
        let Ok(directory) = CString::new(directory.as_os_str().as_bytes()) else {
            return Wait::Unwatchable;
        };
        // SAFETY: plain syscall; a non-negative result is a new descriptor we own.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if fd < 0 {
            return Wait::Unwatchable;
        }
        // SAFETY: `fd` was just returned by inotify_init1 and is owned by nothing else.
        let inotify = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: a valid inotify descriptor and a NUL-terminated path.
        if unsafe { libc::inotify_add_watch(inotify.as_raw_fd(), directory.as_ptr(), MASK) } < 0 {
            return Wait::Unwatchable;
        }
        // Watched from here on: anything created after this is reported.
        // Something that already exists but did not start (a dangling link,
        // a script whose interpreter is missing) is not waited for.
        if publisher.symlink_metadata().is_ok() {
            return if publisher.exists() {
                Wait::Created
            } else {
                Wait::Unwatchable
            };
        }
        if superseded() {
            return Wait::Superseded;
        }
        let mut buffer = [0_u8; 4096];
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: inotify.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: two initialized pollfds; no timeout.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if ready < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Wait::Unwatchable;
            }
            if fds[1].revents != 0 {
                drain(wake);
                if superseded() {
                    return Wait::Superseded;
                }
            }
            if fds[0].revents & !libc::POLLIN != 0 {
                return Wait::Unwatchable;
            }
            if fds[0].revents & libc::POLLIN == 0 {
                continue;
            }
            // SAFETY: reads into a live buffer of the given length.
            let read = unsafe {
                libc::read(
                    inotify.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            let Ok(read) = usize::try_from(read) else {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                    continue;
                }
                return Wait::Unwatchable;
            };
            if let Some(wait) = events(buffer.get(..read).unwrap_or_default(), name.as_bytes()) {
                return wait;
            }
        }
    }

    /// What a batch of inotify events means for a wait on `name`.
    fn events(mut bytes: &[u8], name: &[u8]) -> Option<Wait> {
        const HEADER: usize = std::mem::size_of::<libc::inotify_event>();
        let mut outcome = None;
        while bytes.len() >= HEADER {
            // SAFETY: at least HEADER bytes remain; read_unaligned copes with
            // the buffer's alignment.
            let event: libc::inotify_event =
                unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast()) };
            let length = usize::try_from(event.len).unwrap_or(usize::MAX);
            let Some(entry) = bytes.get(HEADER..HEADER.saturating_add(length)) else {
                break;
            };
            let entry = entry.split(|byte| *byte == 0).next().unwrap_or_default();
            if event.mask
                & (libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF
                    | libc::IN_IGNORED
                    | libc::IN_Q_OVERFLOW)
                != 0
            {
                return Some(Wait::Unwatchable);
            }
            if entry == name {
                outcome = Some(Wait::Created);
            }
            bytes = bytes
                .get(HEADER.saturating_add(length)..)
                .unwrap_or_default();
        }
        outcome
    }
}

/// Doubling backoff, capped at five seconds for a publisher that ran and
/// failed and at five minutes for one that is not installed.
fn next_retry_delay(current: std::time::Duration, publisher_missing: bool) -> std::time::Duration {
    let cap = if publisher_missing {
        MAX_MISSING_PUBLISHER_RETRY_DELAY
    } else {
        MAX_FS_AUTHORITY_RETRY_DELAY
    };
    current.saturating_mul(2).min(cap)
}

/// Block until the publication worker has retired with nothing queued, or
/// `timeout` passes. The worker's retirement is signalled, so this returns as
/// soon as publication settles. Exports still queued on the state writer,
/// which schedule publications, are waited for first.
///
/// # Errors
///
/// On timeout, or if the queue is poisoned, returns a report of the queue
/// state and the recent publisher attempts, including their stderr.
pub fn wait_for_fs_authority_publication_idle(timeout: std::time::Duration) -> Result<(), String> {
    if !crate::shell::durable_writer::global().flush_timeout(timeout) {
        return Err("capability exports are still queued on the state writer".into());
    }
    let Ok(queue) = fs_authority_publication_queue().lock() else {
        return Err("filesystem authority publication queue is poisoned".into());
    };
    let Ok((queue, _)) =
        fs_authority_publication_retired().wait_timeout_while(queue, timeout, |queue| {
            queue.running || queue.pending.is_some()
        })
    else {
        return Err("filesystem authority publication queue is poisoned".into());
    };
    if queue.running || queue.pending.is_some() {
        return Err(format!(
            "filesystem authority publication did not settle within {timeout:?}: {}",
            queue.report()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_missing_publisher_backs_off_to_minutes_not_seconds() {
        let settle = |missing| {
            let mut delay = Duration::from_millis(100);
            for _ in 0..32 {
                delay = next_retry_delay(delay, missing);
            }
            delay
        };
        assert_eq!(settle(false), MAX_FS_AUTHORITY_RETRY_DELAY);
        assert_eq!(settle(true), MAX_MISSING_PUBLISHER_RETRY_DELAY);
        assert!(MAX_MISSING_PUBLISHER_RETRY_DELAY >= Duration::from_secs(60));
        assert_eq!(
            next_retry_delay(Duration::from_millis(100), true),
            Duration::from_millis(200),
            "the first retries stay quick in case the daemon is only starting",
        );
    }

    #[cfg(target_os = "linux")]
    mod waiting_for_the_publisher {
        use super::super::publisher_wait::{Wait, new_eventfd, signal, wait};
        use std::os::fd::AsFd;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        /// Run a wait on its own thread, so the test can act while it blocks.
        fn waiting(
            publisher: std::path::PathBuf,
            superseded: Arc<AtomicBool>,
        ) -> (std::sync::mpsc::Receiver<Wait>, Arc<std::os::fd::OwnedFd>) {
            let wake = Arc::new(new_eventfd().expect("an eventfd"));
            let (done, outcome) = std::sync::mpsc::channel();
            let thread_wake = Arc::clone(&wake);
            std::thread::spawn(move || {
                let check = || superseded.load(Ordering::SeqCst);
                let _ = done.send(wait(&publisher, thread_wake.as_fd(), &check));
            });
            (outcome, wake)
        }

        #[test]
        fn creating_the_publisher_ends_the_wait_and_nothing_else_in_its_directory_does() {
            let dir = tempfile::tempdir().unwrap();
            let publisher = dir.path().join("fs");
            let (outcome, _wake) = waiting(publisher.clone(), Arc::default());
            // Wait until the watch is in place: a created file is then
            // reported even if it lands before the poll.
            std::thread::sleep(Duration::from_millis(100));
            // The directory's other entries being created, written, opened
            // and run must not end the wait (the regression: a watch that
            // reports opens woke a busy shard 47 times a second).
            let other = dir.path().join("other-tool");
            std::fs::write(&other, b"#!/bin/sh\n").unwrap();
            let _ = std::fs::read(&other).unwrap();
            let _ = std::fs::read_dir(dir.path()).unwrap().count();
            assert!(
                outcome.recv_timeout(Duration::from_millis(300)).is_err(),
                "an unrelated entry ended the wait for the publisher"
            );
            std::fs::write(&publisher, b"#!/bin/sh\n").unwrap();
            assert_eq!(
                outcome
                    .recv_timeout(tilo_test_env::hang_bound())
                    .expect("creating the publisher ends the wait"),
                Wait::Created
            );
        }

        #[test]
        fn a_publisher_moved_into_place_ends_the_wait() {
            let dir = tempfile::tempdir().unwrap();
            let staging = tempfile::tempdir_in(dir.path()).unwrap();
            let publisher = dir.path().join("fs");
            let (outcome, _wake) = waiting(publisher.clone(), Arc::default());
            std::thread::sleep(Duration::from_millis(100));
            let staged = staging.path().join("fs");
            std::fs::write(&staged, b"#!/bin/sh\n").unwrap();
            std::fs::rename(&staged, &publisher).unwrap();
            assert_eq!(
                outcome.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
                Wait::Created
            );
        }

        #[test]
        fn a_newer_projection_ends_the_wait_at_once() {
            let dir = tempfile::tempdir().unwrap();
            let superseded = Arc::new(AtomicBool::new(false));
            let (outcome, wake) = waiting(dir.path().join("fs"), Arc::clone(&superseded));
            std::thread::sleep(Duration::from_millis(100));
            // A wake with nothing newer queued keeps waiting.
            signal(wake.as_fd());
            assert!(outcome.recv_timeout(Duration::from_millis(200)).is_err());
            superseded.store(true, Ordering::SeqCst);
            signal(wake.as_fd());
            assert_eq!(
                outcome.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
                Wait::Superseded
            );
        }

        #[test]
        fn what_cannot_be_watched_falls_back_to_the_timer() {
            let dir = tempfile::tempdir().unwrap();
            let wake = new_eventfd().unwrap();
            let never = || false;
            // No such directory.
            assert_eq!(
                wait(&dir.path().join("missing/fs"), wake.as_fd(), &never),
                Wait::Unwatchable
            );
            // A bare name is looked up on PATH.
            assert_eq!(
                wait(std::path::Path::new("fs"), wake.as_fd(), &never),
                Wait::Unwatchable
            );
            // A dangling link exists but will not start: no creation to wait for.
            let link = dir.path().join("fs");
            std::os::unix::fs::symlink(dir.path().join("nowhere"), &link).unwrap();
            assert_eq!(wait(&link, wake.as_fd(), &never), Wait::Unwatchable);
            // A publisher that appeared before the watch is tried at once.
            let present = dir.path().join("present");
            std::fs::write(&present, b"").unwrap();
            assert_eq!(wait(&present, wake.as_fd(), &never), Wait::Created);
        }

        #[test]
        fn the_watched_directory_going_away_falls_back_to_the_timer() {
            let parent = tempfile::tempdir().unwrap();
            let dir = parent.path().join("bin");
            std::fs::create_dir(&dir).unwrap();
            let (outcome, _wake) = waiting(dir.join("fs"), Arc::default());
            std::thread::sleep(Duration::from_millis(100));
            std::fs::remove_dir(&dir).unwrap();
            assert_eq!(
                outcome.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
                Wait::Unwatchable
            );
        }
    }
}
