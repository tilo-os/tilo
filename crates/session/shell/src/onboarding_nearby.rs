//! First-use pairing with a nearby trusted device.
//!
//! Onboarding runs `tilo-enroll pair-device` as the person at the screen.
//! Joining needs no administrator: the device-trust group the session user
//! belongs to already owns every directory the join writes (see the image's
//! `tmpfiles.d/tilo.conf`), exactly as when the person creates a new identity
//! here. The process reports through a status file in the identity directory,
//! and that file — not the process's output — is what completes the step.
//!
//! Every way pairing can end is reported: a joined identity, the error the
//! process recorded, its exit without one, or the process outliving every
//! window it has. None of them leaves onboarding waiting.
//!
//! The status file is read by a watcher thread when the identity directory
//! changes, never by the compositor's loop, which only takes the reads in.

use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::onboarding::MintedIdentity;
use crate::shell::i18n::{t, t_args};

/// The enrollment helper the image installs.
const ENROLL: &str = "/usr/bin/tilo-enroll";

/// Each enrollment window the helper may wait through, in seconds.
const PAIRING_WINDOW_SECS: u64 = 270;

/// The helper waits through at most three windows (discovery, the request,
/// and the approver's confirmation). A process still running after all of
/// them and some slack is stuck, and is stopped.
const PROCESS_BOUND: Duration = Duration::from_secs(3 * PAIRING_WINDOW_SECS + 30);

/// What the helper last reported in its status file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NearbyProgress {
    /// Nothing readable yet.
    Starting,
    /// Broadcasting; the trusted device shows the same code.
    Waiting { code: String },
    /// Joined `identity`, whose public record is `record_path`.
    Complete {
        identity: String,
        record_path: PathBuf,
    },
    /// Ended without joining.
    Failed(NearbyFailure),
}

/// What made a pairing attempt end without joining. Each cause the person
/// can act on has its own plain words; only a reason nothing more is known
/// about falls back to the general one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NearbyCause {
    /// This computer was not allowed to write the pairing.
    PermissionDenied,
    /// The disk is full.
    StorageFull,
    /// The storage cannot be written.
    ReadOnly,
    /// The other device could not be reached over the network.
    Unreachable,
    /// No trusted device approved before the window closed.
    TimedOut,
    /// The other device answered with an identity that does not verify.
    InvalidIdentity,
    /// The helper could not be started.
    NotStarted,
    /// The helper stopped without saying why.
    Ended,
    /// A reason that names none of the causes above.
    Unknown,
}

impl NearbyCause {
    /// The cause as `tilo-enroll pair-device` names it in its status file.
    fn from_status(name: &str) -> Option<Self> {
        Some(match name {
            "permission_denied" => Self::PermissionDenied,
            "storage_full" => Self::StorageFull,
            "read_only" => Self::ReadOnly,
            "unreachable" => Self::Unreachable,
            "timed_out" => Self::TimedOut,
            "invalid_identity" => Self::InvalidIdentity,
            "not_started" => Self::NotStarted,
            "ended" => Self::Ended,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    fn status_name(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::StorageFull => "storage_full",
            Self::ReadOnly => "read_only",
            Self::Unreachable => "unreachable",
            Self::TimedOut => "timed_out",
            Self::InvalidIdentity => "invalid_identity",
            Self::NotStarted => "not_started",
            Self::Ended => "ended",
            Self::Unknown => "unknown",
        }
    }

    /// The cause an operating-system error carries.
    fn from_io(kind: std::io::ErrorKind) -> Option<Self> {
        use std::io::ErrorKind;
        match kind {
            ErrorKind::PermissionDenied => Some(Self::PermissionDenied),
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Some(Self::StorageFull),
            ErrorKind::ReadOnlyFilesystem => Some(Self::ReadOnly),
            ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkDown
            | ErrorKind::AddrNotAvailable
            | ErrorKind::ConnectionRefused => Some(Self::Unreachable),
            ErrorKind::TimedOut => Some(Self::TimedOut),
            _ => None,
        }
    }

    /// The cause of a reason known only as text: an operating-system error
    /// as Rust prints it ends in `(os error N)`.
    fn from_text(text: &str) -> Self {
        text.rsplit_once("(os error ")
            .and_then(|(_, code)| code.strip_suffix(')'))
            .and_then(|code| code.parse::<i32>().ok())
            .and_then(|code| Self::from_io(std::io::Error::from_raw_os_error(code).kind()))
            .unwrap_or(Self::Unknown)
    }

    fn keys(self) -> (&'static str, &'static str) {
        match self {
            Self::PermissionDenied => (
                "onboarding-nearby-permission-title",
                "onboarding-nearby-permission",
            ),
            Self::StorageFull => (
                "onboarding-nearby-storage-full-title",
                "onboarding-nearby-storage-full",
            ),
            Self::ReadOnly => (
                "onboarding-nearby-read-only-title",
                "onboarding-nearby-read-only",
            ),
            Self::Unreachable => (
                "onboarding-nearby-unreachable-title",
                "onboarding-nearby-unreachable",
            ),
            Self::TimedOut => (
                "onboarding-nearby-timed-out-title",
                "onboarding-nearby-timed-out",
            ),
            Self::InvalidIdentity => (
                "onboarding-nearby-invalid-identity-title",
                "onboarding-nearby-invalid-identity",
            ),
            Self::NotStarted => (
                "onboarding-nearby-not-started-title",
                "onboarding-nearby-not-started",
            ),
            Self::Ended => ("onboarding-nearby-failed-title", "onboarding-nearby-ended"),
            Self::Unknown => (
                "onboarding-nearby-failed-title",
                "onboarding-nearby-unknown",
            ),
        }
    }
}

/// Why a pairing attempt ended, and the helper's own words when it gave any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NearbyFailure {
    pub(crate) cause: NearbyCause,
    pub(crate) detail: Option<String>,
}

impl NearbyFailure {
    pub(crate) fn new(cause: NearbyCause) -> Self {
        Self {
            cause,
            detail: None,
        }
    }

    /// A failure known only by its text; its cause is read from it.
    pub(crate) fn from_detail(detail: String) -> Self {
        Self {
            cause: NearbyCause::from_text(&detail),
            detail: Some(detail),
        }
    }

    /// What went wrong, in plain words: the banner's title.
    pub(crate) fn title(&self) -> String {
        t(self.cause.keys().0)
    }

    /// What to do next, followed by the helper's own words.
    pub(crate) fn message(&self) -> String {
        let next = t(self.cause.keys().1);
        match &self.detail {
            Some(detail) => format!(
                "{next} {}",
                t_args(
                    "onboarding-nearby-detail",
                    &[("detail", detail.clone().into())]
                )
            ),
            None => next,
        }
    }

    /// Whether this says more than that the helper stopped.
    fn is_informative(&self) -> bool {
        self.detail.is_some() || !matches!(self.cause, NearbyCause::Unknown | NearbyCause::Ended)
    }
}

/// Parse the status file at `path`.
pub(crate) fn read_progress(path: &Path) -> NearbyProgress {
    let Some(status) = std::fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() <= 64 * 1024)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    else {
        return NearbyProgress::Starting;
    };
    let field = |name: &str| {
        status
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    match field("state").as_deref() {
        Some("complete") => match (field("identity"), field("record_path")) {
            (Some(identity), Some(record_path)) => NearbyProgress::Complete {
                identity,
                record_path: PathBuf::from(record_path),
            },
            _ => NearbyProgress::Failed(NearbyFailure::new(NearbyCause::InvalidIdentity)),
        },
        Some("failed") => {
            let detail = field("error").filter(|error| !error.is_empty());
            let cause = field("cause")
                .as_deref()
                .and_then(NearbyCause::from_status)
                .or_else(|| detail.as_deref().map(NearbyCause::from_text))
                .unwrap_or(NearbyCause::Unknown);
            NearbyProgress::Failed(NearbyFailure { cause, detail })
        }
        _ => field("verification_code").map_or(NearbyProgress::Starting, |code| {
            NearbyProgress::Waiting { code }
        }),
    }
}

/// What onboarding should show after one [`NearbyPairing::poll`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NearbyEvent {
    /// Nothing changed.
    Idle,
    /// A verification code to show.
    Code(String),
    /// The device joined this identity. Pairing is over.
    Joined(MintedIdentity),
    /// Pairing ended without joining, and why. Pairing is over.
    Failed(NearbyFailure),
}

/// How long the status watcher waits for a change before reading the status
/// file anyway, should no change notification reach it.
const STATUS_FALLBACK: Duration = Duration::from_millis(250);

/// Allow a late verified commit to arrive without waiting forever on disk I/O.
const FINAL_LOOK_GRACE: Duration = Duration::from_secs(2);

/// The status file as the watcher read it, with the identities it names
/// already verified: the loop never reads the identity directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusRead {
    progress: NearbyProgress,
    /// For a completed join, its record, when that verifies.
    joined: Option<MintedIdentity>,
    /// Whether this is the final look, made once the helper ended or ran
    /// out of time.
    final_look: bool,
    /// On the final look, the active identity, when one is selected and
    /// verifies.
    active: Option<MintedIdentity>,
}

impl StatusRead {
    const STARTING: Self = Self {
        progress: NearbyProgress::Starting,
        joined: None,
        final_look: false,
        active: None,
    };
}

/// What wakes the status watcher.
enum WatcherWake {
    /// Something in the identity directory changed.
    Changed,
    /// The helper ended or ran out of time: look once more, including the
    /// identity it may have selected.
    Final,
    /// The attempt is over.
    Stop,
}

/// Reads the helper's status file on its own thread whenever the identity
/// directory changes, and sends each new read back. The helper is another
/// process and reports only through that file; reading it on the
/// compositor's loop held a frame for the disk four times a second.
struct StatusWatcher {
    wakes: mpsc::Sender<WatcherWake>,
    reads: mpsc::Receiver<StatusRead>,
}

impl StatusWatcher {
    /// Start watching `status_path`. `None` when the thread cannot start.
    fn start(status_path: PathBuf, identity_dir: PathBuf) -> Option<Self> {
        let (wakes, woken) = mpsc::channel();
        let (sent, reads) = mpsc::channel();
        // Made on the loop's thread, so a new read wakes the loop it is for.
        let wake = crate::shell::search::invocation_wake::waker();
        let changes = wakes.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-nearby-status".into())
            .spawn(move || {
                // Change notifications only hurry the next read; without
                // them the fallback still reads.
                let watcher =
                    notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                        // An open is no change, and the watcher's own reads
                        // open the files it watches.
                        let opened = |event: &notify::Event| {
                            matches!(
                                event.kind,
                                notify::EventKind::Access(notify::event::AccessKind::Open(_))
                            )
                        };
                        if event.is_ok_and(|event| !opened(&event)) {
                            let _ = changes.send(WatcherWake::Changed);
                        }
                    })
                    .and_then(|mut watcher| {
                        notify::Watcher::watch(
                            &mut watcher,
                            &identity_dir,
                            notify::RecursiveMode::NonRecursive,
                        )
                        .map(|()| watcher)
                    });
                if let Err(error) = &watcher {
                    tracing::debug!(%error, "pairing status changes are not notified; reading on a timer");
                }
                watch_status(&status_path, &identity_dir, &woken, &sent, &wake);
                drop(watcher);
            });
        match spawned {
            Ok(_) => Some(Self { wakes, reads }),
            Err(error) => {
                tracing::warn!(%error, "pairing status watcher did not start");
                None
            }
        }
    }
}

impl Drop for StatusWatcher {
    fn drop(&mut self) {
        let _ = self.wakes.send(WatcherWake::Stop);
    }
}

/// The watcher thread: read the status, send it when it changed or a final
/// look was asked for, and wait for the next change.
fn watch_status(
    status_path: &Path,
    identity_dir: &Path,
    woken: &mpsc::Receiver<WatcherWake>,
    sent: &mpsc::Sender<StatusRead>,
    wake: &std::task::Waker,
) {
    let mut last = None;
    let mut final_look = false;
    loop {
        let read = read_status(status_path, identity_dir, final_look);
        if final_look || last.as_ref() != Some(&read) {
            if sent.send(read.clone()).is_err() {
                return;
            }
            wake.wake_by_ref();
            last = Some(read);
        }
        final_look = false;
        let first = match woken.recv_timeout(STATUS_FALLBACK) {
            Ok(signal) => Some(signal),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        // One read covers every change queued meanwhile.
        for signal in first.into_iter().chain(woken.try_iter()) {
            match signal {
                WatcherWake::Changed => {}
                WatcherWake::Final => final_look = true,
                WatcherWake::Stop => return,
            }
        }
    }
}

/// Read the status file at `status_path`, verifying the identity a
/// completed join names, and on a `final_look` the active identity too.
/// Reads the disk: never on the compositor's loop.
fn read_status(status_path: &Path, identity_dir: &Path, final_look: bool) -> StatusRead {
    let progress = read_progress(status_path);
    let joined = match &progress {
        NearbyProgress::Complete {
            identity,
            record_path,
        } => verified(identity_dir, record_path, Some(identity.as_str())),
        _ => None,
    };
    let active = final_look
        .then(|| runtime_identity::resolve_active_identity(identity_dir))
        .flatten()
        .and_then(|path| verified(identity_dir, &path, None));
    StatusRead {
        progress,
        joined,
        final_look,
        active,
    }
}

/// `path` as a joined identity, when it is a valid record directly in
/// `identity_dir` (and of `expected`, when given).
fn verified(identity_dir: &Path, path: &Path, expected: Option<&str>) -> Option<MintedIdentity> {
    if path.parent() != Some(identity_dir) {
        return None;
    }
    let identity = crate::onboarding::existing_identity_at(path).ok()?;
    expected
        .is_none_or(|expected| identity.fingerprint == expected)
        .then_some(identity)
}

/// One running pairing attempt. Dropping it stops the helper's whole process
/// group.
pub(crate) struct NearbyPairing {
    child: Option<Child>,
    deadline: Instant,
    status_path: PathBuf,
    shown_code: Option<String>,
    /// Reads the status file off the loop; `None` when disconnected.
    watcher: Option<StatusWatcher>,
    /// What the status file held when last read.
    status: StatusRead,
    /// When the final look was asked for, with its own bounded grace.
    ending: Option<Instant>,
}

impl NearbyPairing {
    /// Start pairing this device into an identity a nearby device approves.
    ///
    /// # Errors
    ///
    /// Why the helper cannot be started.
    pub(crate) fn start(
        identity_root: &Path,
        identity_dir: &Path,
        status_path: &Path,
        label: &str,
    ) -> Result<Self, NearbyFailure> {
        let lease =
            tilo_relay_protocol::ProcessLease::capture(std::process::id()).map_err(|error| {
                NearbyFailure {
                    cause: NearbyCause::NotStarted,
                    detail: Some(error.to_string()),
                }
            })?;
        let mut command = Command::new(ENROLL);
        command
            .arg("pair-device")
            .arg("--identity-root")
            .arg(identity_root)
            .arg("--identity-dir")
            .arg(identity_dir)
            .args([
                "--label",
                label,
                "--timeout",
                &PAIRING_WINDOW_SECS.to_string(),
            ])
            .arg("--status-file")
            .arg(status_path)
            .arg("--requester-pid")
            .arg(lease.pid().to_string())
            .arg("--requester-start-time")
            .arg(lease.start_time().to_string());
        Self::spawn(command, identity_dir, status_path, PROCESS_BOUND)
    }

    pub(crate) fn spawn(
        mut command: Command,
        identity_dir: &Path,
        status_path: &Path,
        bound: Duration,
    ) -> Result<Self, NearbyFailure> {
        let _ = std::fs::remove_file(status_path);
        let watcher = StatusWatcher::start(status_path.to_path_buf(), identity_dir.to_path_buf())
            .ok_or_else(|| NearbyFailure::new(NearbyCause::NotStarted))?;
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // The helper and anything it starts share one revocable
            // lifetime: Cancel, Back and compositor teardown stop the whole
            // group instead of orphaning a descendant.
            .process_group(0)
            .spawn()
            .map_err(|error| NearbyFailure {
                cause: NearbyCause::from_io(error.kind()).unwrap_or(NearbyCause::NotStarted),
                detail: Some(error.to_string()),
            })?;
        Ok(Self {
            child: Some(child),
            deadline: Instant::now()
                .checked_add(bound)
                .unwrap_or_else(Instant::now),
            status_path: status_path.to_path_buf(),
            shown_code: None,
            watcher: Some(watcher),
            status: StatusRead::STARTING,
            ending: None,
        })
    }

    /// Advance from what the helper reported. After [`NearbyEvent::Joined`]
    /// or [`NearbyEvent::Failed`] the caller drops this attempt. Never reads
    /// the disk: the status comes from the watcher.
    pub(crate) fn poll(&mut self, now: Instant) -> NearbyEvent {
        let exited = self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        let ended = exited.is_some() || now >= self.deadline;
        self.take_status(ended, now);
        if matches!(self.status.progress, NearbyProgress::Complete { .. }) {
            return match self.status.joined.clone() {
                Some(joined) => NearbyEvent::Joined(joined),
                None => self.fail(NearbyFailure::new(NearbyCause::InvalidIdentity)),
            };
        }
        if ended {
            // The join selects its identity as active once its commit has
            // written everything else. A verified active identity is a
            // completed join even when its status update was lost, but only
            // once the helper has ended (or run out of time): ending this
            // attempt stops the helper, and one still running is still
            // finishing its commit and its status. The final look reads
            // both after the end, so what is decided below is what the
            // helper left.
            if !self.status.final_look
                && self.watcher.is_some()
                && self.ending.is_some_and(|started| {
                    now.saturating_duration_since(started) < FINAL_LOOK_GRACE
                })
            {
                return NearbyEvent::Idle;
            }
            if let Some(joined) = self.status.active.clone() {
                return NearbyEvent::Joined(joined);
            }
        }
        if let Some(status) = exited {
            let progress = self.status.progress.clone();
            return self.exited(status, progress);
        }
        if now >= self.deadline {
            return self.fail(NearbyFailure::new(NearbyCause::TimedOut));
        }
        if let NearbyProgress::Waiting { code } = &self.status.progress
            && self.shown_code.as_ref() != Some(code)
        {
            self.shown_code = Some(code.clone());
            return NearbyEvent::Code(code.clone());
        }
        NearbyEvent::Idle
    }

    /// Take in the watcher's newest read, asking it for the final look once
    /// the attempt has `ended`. A lost watcher never moves reads onto the loop.
    fn take_status(&mut self, ended: bool, now: Instant) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        loop {
            match watcher.reads.try_recv() {
                Ok(read) => {
                    // A final look stands until the attempt is over.
                    if !self.status.final_look || read.final_look {
                        self.status = read;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.watcher = None;
                    return;
                }
            }
        }
        if ended && self.ending.is_none() {
            self.ending = Some(now);
            if watcher.wakes.send(WatcherWake::Final).is_err() {
                self.watcher = None;
            }
        }
    }

    fn exited(&mut self, status: ExitStatus, progress: NearbyProgress) -> NearbyEvent {
        let stderr = self
            .child
            .as_mut()
            .and_then(|child| child.stderr.as_mut())
            .map(available_output)
            .unwrap_or_default();
        let failure = match progress {
            _ if status.success() => NearbyFailure::new(NearbyCause::InvalidIdentity),
            NearbyProgress::Failed(failure) if failure.is_informative() => failure,
            _ => stderr
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.trim_start_matches("Error: ").to_owned())
                .map_or_else(
                    || NearbyFailure::new(NearbyCause::Ended),
                    NearbyFailure::from_detail,
                ),
        };
        self.fail(failure)
    }

    /// End this attempt with `failure`, recording it in the status file
    /// unless the helper already recorded why it stopped.
    fn fail(&mut self, failure: NearbyFailure) -> NearbyEvent {
        terminate(&mut self.child);
        if !matches!(self.status.progress, NearbyProgress::Failed(_)) {
            let mut status = serde_json::Map::new();
            status.insert("state".into(), "failed".into());
            status.insert("cause".into(), failure.cause.status_name().into());
            if let Some(detail) = &failure.detail {
                status.insert("error".into(), detail.clone().into());
            }
            // The status of a pairing child that no longer runs: removed
            // before every pairing and never read after a restart, so it is
            // not synced. This runs on the compositor's loop, where a sync
            // under I/O pressure froze the desktop.
            if let Ok(bytes) = serde_json::to_vec(&status) {
                let _ = tilo_files::native::atomic::write_regenerable_with_mode(
                    &self.status_path,
                    &bytes,
                    0o600,
                );
            }
        }
        NearbyEvent::Failed(failure)
    }
}

impl Drop for NearbyPairing {
    fn drop(&mut self) {
        terminate(&mut self.child);
    }
}

/// What the helper wrote to `stream` so far, without waiting for more.
///
/// The helper has exited, but a worker it started may still hold the pipe
/// open; reading to end of file would block the compositor until that worker
/// is gone. The pipe is switched to non-blocking and drained of what is there.
fn available_output(stream: &mut std::process::ChildStderr) -> String {
    use std::os::fd::AsRawFd as _;

    let fd = stream.as_raw_fd();
    // SAFETY: `fd` is the open read end of this child's stderr pipe, owned by
    // `stream` for the whole call; F_GETFL/F_SETFL only change its flags.
    let nonblocking = unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        flags >= 0 && libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
    };
    if !nonblocking {
        return String::new();
    }
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    while bytes.len() < 64 * 1024 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => bytes.extend_from_slice(chunk.get(..read).unwrap_or_default()),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Stop `child` and every process in its group, bounded.
pub(crate) fn terminate(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        bounded_command::terminate_tree(&mut child, Duration::from_millis(750));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    struct Fixture {
        _dir: tempfile::TempDir,
        identity_dir: PathBuf,
        status: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let identity_dir = dir.path().join("identity");
        std::fs::create_dir(&identity_dir).expect("identity dir");
        std::fs::set_permissions(&identity_dir, std::fs::Permissions::from_mode(0o700))
            .expect("private identity dir");
        let status = identity_dir.join(".nearby-enrollment-status.json");
        Fixture {
            _dir: dir,
            identity_dir,
            status,
        }
    }

    fn helper(fixture: &Fixture, script: &str, bound: Duration) -> NearbyPairing {
        let mut command = Command::new("sh");
        command
            .args(["-c", script, "helper"])
            .arg(&fixture.status)
            .arg(&fixture.identity_dir);
        NearbyPairing::spawn(command, &fixture.identity_dir, &fixture.status, bound)
            .expect("spawn helper")
    }

    fn poll_until_over(pairing: &mut NearbyPairing) -> NearbyEvent {
        poll_until_over_at(pairing, None)
    }

    fn poll_until_over_at(pairing: &mut NearbyPairing, now: Option<Instant>) -> NearbyEvent {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match pairing.poll(now.unwrap_or_else(Instant::now)) {
                NearbyEvent::Idle | NearbyEvent::Code(_) => {}
                event => return event,
            }
            assert!(Instant::now() < deadline, "pairing never ended");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn joined_record(fixture: &Fixture, user: [u8; 32]) -> PathBuf {
        let path = fixture.identity_dir.join("identity-joined.json");
        let bytes =
            runtime_identity::public_member_record("Member", [1, 2, 3], user).expect("record");
        std::fs::write(&path, bytes).expect("write record");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private record");
        path
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one phase per way a pairing attempt can end"
    )]
    fn nearby_pairing_always_ends_with_the_join_or_a_reason() {
        // Phase `the_status_file_completes_the_join_without_reading_output`.
        {
            let fixture = fixture();
            let record = joined_record(&fixture, [7; 32]);
            let status = serde_json::json!({
                "state": "complete",
                "verification_code": "123456",
                "identity": hex::encode([7; 32]),
                "record_path": record,
            });
            // The helper keeps running after reporting: completion must not
            // wait for its exit or parse anything it prints.
            let script = format!("printf '%s' '{status}' > \"$1\"; exec sleep 30");
            let mut pairing = helper(&fixture, &script, PROCESS_BOUND);
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Joined(MintedIdentity {
                    fingerprint: hex::encode([7; 32]),
                    path: record,
                })
            );
        }
        // Phase `a_completed_record_of_another_identity_is_rejected`.
        {
            let fixture = fixture();
            let record = joined_record(&fixture, [7; 32]);
            let status = serde_json::json!({
                "state": "complete",
                "identity": hex::encode([8; 32]),
                "record_path": record,
            });
            let script = format!("printf '%s' '{status}' > \"$1\"");
            let mut pairing = helper(&fixture, &script, PROCESS_BOUND);
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Failed(NearbyFailure::new(NearbyCause::InvalidIdentity))
            );
        }
        // Phase `a_selected_active_identity_completes_a_lost_status_update`.
        // The helper ended without recording completion.
        {
            let fixture = fixture();
            let record = joined_record(&fixture, [9; 32]);
            runtime_identity::set_active_identity(&fixture.identity_dir, "identity-joined")
                .expect("select joined identity");
            let mut pairing = helper(&fixture, "exit 0", PROCESS_BOUND);
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Joined(MintedIdentity {
                    fingerprint: hex::encode([9; 32]),
                    path: record,
                })
            );
        }
        // Phase `a_selected_identity_lets_the_helper_finish_its_commit`.
        // The selection lands while the helper is still committing: the
        // attempt must not end, and stop the helper, before it has finished
        // and recorded its status.
        {
            let fixture = fixture();
            let record = joined_record(&fixture, [10; 32]);
            runtime_identity::set_active_identity(&fixture.identity_dir, "identity-joined")
                .expect("select joined identity");
            let finished = fixture.identity_dir.join("helper-finished");
            let status = serde_json::json!({
                "state": "complete",
                "verification_code": "123456",
                "identity": hex::encode([10; 32]),
                "record_path": record,
            });
            let script = format!(
                "sleep 1; : > '{}'; printf '%s' '{status}' > \"$1\"",
                finished.display()
            );
            let mut pairing = helper(&fixture, &script, PROCESS_BOUND);
            assert_eq!(pairing.poll(Instant::now()), NearbyEvent::Idle);
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Joined(MintedIdentity {
                    fingerprint: hex::encode([10; 32]),
                    path: record,
                })
            );
            assert!(
                finished.exists(),
                "the helper was stopped before it finished"
            );
        }
        // Phase `the_recorded_helper_error_is_shown_with_a_retry`.
        {
            let fixture = fixture();
            let script = r#"printf '%s' '{"state":"failed","error":"Permission denied (os error 13)","cause":"permission_denied"}' > "$1"; echo "Error: Permission denied (os error 13)" >&2; exit 1"#;
            let mut pairing = helper(&fixture, script, PROCESS_BOUND);
            let NearbyEvent::Failed(failure) = poll_until_over(&mut pairing) else {
                panic!("the refused commit is a failure");
            };
            assert_eq!(
                failure,
                NearbyFailure {
                    cause: NearbyCause::PermissionDenied,
                    detail: Some("Permission denied (os error 13)".into()),
                }
            );
            assert!(failure.title().contains("Permission denied"), "{failure:?}");
        }
        // Phase `an_exit_without_a_status_reports_stderr_and_records_it`.
        {
            let fixture = fixture();
            let script = r#"echo "Error: Not authorized" >&2; exit 127"#;
            let mut pairing = helper(&fixture, script, PROCESS_BOUND);
            let expected = NearbyFailure {
                cause: NearbyCause::Unknown,
                detail: Some("Not authorized".into()),
            };
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Failed(expected.clone())
            );
            assert_eq!(
                read_progress(&fixture.status),
                NearbyProgress::Failed(expected)
            );
        }
        // Phase `a_helper_that_never_finishes_is_stopped_and_reported`.
        {
            let fixture = fixture();
            let script = r#"printf '%s' '{"state":"waiting_for_approval","verification_code":"424242"}' > "$1"; trap '' TERM; while :; do sleep 1; done"#;
            let mut pairing = helper(&fixture, script, Duration::from_millis(300));
            let mut saw_code = false;
            let deadline = tilo_test_env::hang_deadline();
            let event = loop {
                match pairing.poll(Instant::now()) {
                    NearbyEvent::Idle => {}
                    NearbyEvent::Code(code) => {
                        assert_eq!(code, "424242");
                        saw_code = true;
                    }
                    event => break event,
                }
                assert!(Instant::now() < deadline, "the bound never fired");
                std::thread::sleep(Duration::from_millis(20));
            };
            assert!(saw_code, "the verification code is shown while waiting");
            let expected = NearbyFailure::new(NearbyCause::TimedOut);
            assert_eq!(event, NearbyEvent::Failed(expected.clone()));
            assert!(pairing.child.is_none(), "the stuck helper was stopped");
            assert_eq!(
                read_progress(&fixture.status),
                NearbyProgress::Failed(expected)
            );
        }
        // Phase `a_worker_holding_stderr_open_never_blocks_the_report`.
        {
            let fixture = fixture();
            // The leader reports and exits while a worker it started keeps
            // the inherited stderr open for far longer than the test runs.
            let script =
                r#"(sleep 600) & echo "Error: Permission denied (os error 13)" >&2; exit 1"#;
            let mut pairing = helper(&fixture, script, PROCESS_BOUND);
            let started = Instant::now();
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Failed(NearbyFailure {
                    cause: NearbyCause::PermissionDenied,
                    detail: Some("Permission denied (os error 13)".into()),
                })
            );
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the report waited on the worker's pipe"
            );
        }
        // Phase `a_successful_exit_without_a_joined_identity_is_an_error`.
        {
            let fixture = fixture();
            let mut pairing = helper(&fixture, "exit 0", PROCESS_BOUND);
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Failed(NearbyFailure::new(NearbyCause::InvalidIdentity))
            );
        }
    }

    /// The onboarding view polls the pairing from the compositor's loop;
    /// recording a failed attempt must not wait for the disk there.
    #[cfg(target_os = "linux")]
    #[test]
    fn recording_a_failed_pairing_never_waits_for_the_disk() {
        let fixture = fixture();
        let mut pairing = helper(&fixture, "exit 0", PROCESS_BOUND);
        let event = crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
            poll_until_over(&mut pairing)
        });
        assert_eq!(
            event,
            NearbyEvent::Failed(NearbyFailure::new(NearbyCause::InvalidIdentity))
        );
        assert!(
            matches!(read_progress(&fixture.status), NearbyProgress::Failed(_)),
            "the failure is recorded for the screen"
        );
        let mode = std::fs::metadata(&fixture.status)
            .expect("status")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    /// The onboarding view polls the pairing from the compositor's loop,
    /// four times a second; the helper's status file is read by the
    /// watcher, never by a poll. While the file holds every reader up (a
    /// FIFO nobody writes yet), each poll returns at once, and the code the
    /// helper then writes reaches the screen through the watcher.
    #[cfg(target_os = "linux")]
    #[test]
    fn polling_a_pairing_never_reads_its_status_file_on_the_loop() {
        let fixture = fixture();
        let mut pairing = helper(&fixture, "exec sleep 30", PROCESS_BOUND);
        // Spawning removed any old status; the helper's file is now one
        // whose bytes have not arrived.
        let status = crate::shell::disk_sync_guard::blocking_source(
            &fixture.identity_dir,
            ".nearby-enrollment-status.json",
        );
        assert_eq!(status, fixture.status);
        for _ in 0..3 {
            let (event, blocked) = std::thread::scope(|scope| {
                let poll = scope.spawn(|| pairing.poll(Instant::now()));
                let deadline = Instant::now()
                    .checked_add(Duration::from_secs(10))
                    .expect("a deadline");
                while !poll.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                let blocked = !poll.is_finished();
                if blocked {
                    // Let the stuck read go, so the test fails instead of
                    // hanging.
                    std::fs::write(&status, b"{}").expect("release the read");
                }
                (poll.join().expect("the poll finished"), blocked)
            });
            assert!(!blocked, "a poll read the status file on the loop");
            assert_eq!(event, NearbyEvent::Idle);
            std::thread::sleep(Duration::from_millis(20));
        }

        std::fs::write(
            &status,
            br#"{"state":"waiting_for_approval","verification_code":"424242"}"#,
        )
        .expect("the watcher reads the status");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match pairing.poll(Instant::now()) {
                NearbyEvent::Idle => {}
                NearbyEvent::Code(code) => {
                    assert_eq!(code, "424242");
                    break;
                }
                event => panic!("the pairing ended: {event:?}"),
            }
            assert!(Instant::now() < deadline, "the code never arrived");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Hold the real disk reader before its first read, without blocking
    /// failure-status writes or requiring a FIFO to be released on failure.
    fn gate_status_watcher(
        pairing: &mut NearbyPairing,
        fixture: &Fixture,
    ) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let (wakes, woken) = mpsc::channel();
        let (sent, reads) = mpsc::channel();
        let (release, held) = mpsc::channel();
        pairing.watcher = Some(StatusWatcher { wakes, reads });
        let status = fixture.status.clone();
        let identity_dir = fixture.identity_dir.clone();
        let thread = std::thread::spawn(move || {
            if held.recv().is_ok() {
                watch_status(
                    &status,
                    &identity_dir,
                    &woken,
                    &sent,
                    std::task::Waker::noop(),
                );
            }
        });
        (release, thread)
    }

    #[test]
    fn pairing_final_look_has_a_bounded_grace() {
        for exited in [false, true] {
            let fixture = fixture();
            let mut pairing = helper(
                &fixture,
                if exited { "exit 7" } else { "exec sleep 30" },
                PROCESS_BOUND,
            );
            let (release, watcher) = gate_status_watcher(&mut pairing, &fixture);
            if exited {
                pairing.child.as_mut().expect("child").wait().expect("exit");
            }
            // Even an identity already on disk cannot be accepted until the
            // watcher verifies it. The reader remains gated throughout.
            joined_record(&fixture, [7; 32]);
            runtime_identity::set_active_identity(&fixture.identity_dir, "identity-joined")
                .expect("active identity");
            let now = if exited {
                Instant::now()
            } else {
                pairing.deadline
            };
            assert_eq!(pairing.poll(now), NearbyEvent::Idle);
            assert_eq!(
                pairing.poll(
                    (now + FINAL_LOOK_GRACE)
                        .checked_sub(Duration::from_millis(1))
                        .expect("instant before grace expires"),
                ),
                NearbyEvent::Idle
            );
            assert_eq!(
                pairing.poll(now + FINAL_LOOK_GRACE),
                NearbyEvent::Failed(NearbyFailure::new(if exited {
                    NearbyCause::Ended
                } else {
                    NearbyCause::TimedOut
                }))
            );
            drop(pairing);
            drop(release);
            watcher.join().expect("watcher stopped");
        }
    }

    #[test]
    fn pairing_final_look_accepts_only_a_verified_late_active_identity() {
        for valid in [false, true] {
            let fixture = fixture();
            let mut pairing = helper(&fixture, "exit 0", PROCESS_BOUND);
            let (release, watcher) = gate_status_watcher(&mut pairing, &fixture);
            pairing.child.as_mut().expect("child").wait().expect("exit");
            // Hold the polling clock within grace while the real worker runs;
            // scheduling delay is not the timeout behavior this test proves.
            let now = Instant::now();
            assert_eq!(pairing.poll(now), NearbyEvent::Idle);
            let record = joined_record(&fixture, [7; 32]);
            runtime_identity::set_active_identity(&fixture.identity_dir, "identity-joined")
                .expect("active identity");
            if !valid {
                std::fs::write(&record, b"{}").expect("invalid record");
            }
            release.send(()).expect("release reader during grace");
            assert_eq!(
                poll_until_over_at(&mut pairing, Some(now)),
                if valid {
                    NearbyEvent::Joined(MintedIdentity {
                        fingerprint: hex::encode([7; 32]),
                        path: record,
                    })
                } else {
                    NearbyEvent::Failed(NearbyFailure::new(NearbyCause::InvalidIdentity))
                }
            );
            drop(pairing);
            watcher.join().expect("watcher stopped");
        }
    }

    #[test]
    fn pairing_lost_watcher_never_falls_back_to_disk_reads() {
        for lost in 0..3 {
            let fixture = fixture();
            let mut pairing = helper(&fixture, "exit 7", PROCESS_BOUND);
            pairing.child.as_mut().expect("child").wait().expect("exit");
            let (wakes, woken) = mpsc::channel();
            let (sent, reads) = mpsc::channel();
            pairing.watcher = Some(StatusWatcher { wakes, reads });
            match lost {
                0 => pairing.watcher = None,
                1 => drop(sent),
                _ => drop(woken),
            }
            // A synchronous final read would incorrectly find and accept
            // this identity instead of preserving the cached helper error.
            joined_record(&fixture, [7; 32]);
            runtime_identity::set_active_identity(&fixture.identity_dir, "identity-joined")
                .expect("active identity");
            let failure = NearbyFailure::new(NearbyCause::PermissionDenied);
            pairing.status.progress = NearbyProgress::Failed(failure.clone());
            assert_eq!(
                pairing.poll(Instant::now()),
                NearbyEvent::Failed(failure),
                "lost watcher case {lost}"
            );
        }
    }

    #[test]
    fn each_failure_cause_reaches_the_screen_in_its_own_words() {
        // The helper names the cause in its status file.
        for (name, cause) in [
            ("permission_denied", NearbyCause::PermissionDenied),
            ("storage_full", NearbyCause::StorageFull),
            ("read_only", NearbyCause::ReadOnly),
            ("unreachable", NearbyCause::Unreachable),
            ("timed_out", NearbyCause::TimedOut),
            ("unknown", NearbyCause::Unknown),
        ] {
            assert_eq!(NearbyCause::from_status(name), Some(cause));
            assert_eq!(cause.status_name(), name);
        }
        // A reason known only as text still carries its operating-system
        // error.
        for (text, cause) in [
            (
                "Permission denied (os error 13)",
                NearbyCause::PermissionDenied,
            ),
            (
                "write commit: Operation not permitted (os error 1)",
                NearbyCause::PermissionDenied,
            ),
            (
                "No space left on device (os error 28)",
                NearbyCause::StorageFull,
            ),
            ("Read-only file system (os error 30)", NearbyCause::ReadOnly),
            (
                "Network is unreachable (os error 101)",
                NearbyCause::Unreachable,
            ),
            ("Not authorized", NearbyCause::Unknown),
            ("Invalid argument (os error 22)", NearbyCause::Unknown),
        ] {
            assert_eq!(NearbyCause::from_text(text), cause, "{text}");
        }
        // A status without a cause field is read from its error.
        let fixture = fixture();
        std::fs::write(
            &fixture.status,
            r#"{"state":"failed","error":"Permission denied (os error 13)"}"#,
        )
        .expect("status");
        assert_eq!(
            read_progress(&fixture.status),
            NearbyProgress::Failed(NearbyFailure {
                cause: NearbyCause::PermissionDenied,
                detail: Some("Permission denied (os error 13)".into()),
            })
        );

        // Every cause the person can act on has its own title; only a reason
        // nothing more is known about uses the general one.
        let general = NearbyFailure::new(NearbyCause::Unknown).title();
        let specific = [
            NearbyCause::PermissionDenied,
            NearbyCause::StorageFull,
            NearbyCause::ReadOnly,
            NearbyCause::Unreachable,
            NearbyCause::TimedOut,
            NearbyCause::InvalidIdentity,
            NearbyCause::NotStarted,
        ];
        let mut titles = std::collections::BTreeSet::new();
        for cause in specific {
            let title = NearbyFailure::new(cause).title();
            assert_ne!(title, general, "{cause:?} is reported generically");
            assert!(
                titles.insert(title),
                "{cause:?} shares another cause's title"
            );
        }
        let denied = NearbyFailure::from_detail("Permission denied (os error 13)".into());
        assert!(
            denied.title().contains("Permission denied"),
            "{}",
            denied.title()
        );
        assert!(
            denied.message().contains("Permission denied (os error 13)"),
            "the helper's words stay available: {}",
            denied.message()
        );
        let unknown = NearbyFailure::from_detail("Not authorized".into());
        assert_eq!(unknown.title(), general);
        assert!(unknown.message().contains("Not authorized"));
    }
}
