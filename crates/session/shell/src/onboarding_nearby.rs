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

use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
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

/// One running pairing attempt. Dropping it stops the helper's whole process
/// group.
pub(crate) struct NearbyPairing {
    child: Option<Child>,
    deadline: Instant,
    status_path: PathBuf,
    identity_dir: PathBuf,
    shown_code: Option<String>,
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
            identity_dir: identity_dir.to_path_buf(),
            shown_code: None,
        })
    }

    /// Advance from what the helper reported. After [`NearbyEvent::Joined`]
    /// or [`NearbyEvent::Failed`] the caller drops this attempt.
    pub(crate) fn poll(&mut self, now: Instant) -> NearbyEvent {
        let exited = self
            .child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        let progress = read_progress(&self.status_path);
        if let NearbyProgress::Complete {
            identity,
            record_path,
        } = &progress
        {
            return match self.verified(record_path, Some(identity)) {
                Some(joined) => NearbyEvent::Joined(joined),
                None => self.fail(NearbyFailure::new(NearbyCause::InvalidIdentity)),
            };
        }
        // The join selects its identity as active once its commit has written
        // everything else. A verified active identity is a completed join even
        // when its status update was lost, but only once the helper has ended
        // (or run out of time): ending this attempt stops the helper, and one
        // still running is still finishing its commit and its status.
        if (exited.is_some() || now >= self.deadline)
            && let Some(joined) = runtime_identity::resolve_active_identity(&self.identity_dir)
                .and_then(|path| self.verified(&path, None))
        {
            return NearbyEvent::Joined(joined);
        }
        if let Some(status) = exited {
            return self.exited(status, progress);
        }
        if now >= self.deadline {
            return self.fail(NearbyFailure::new(NearbyCause::TimedOut));
        }
        if let NearbyProgress::Waiting { code } = progress
            && self.shown_code.as_ref() != Some(&code)
        {
            self.shown_code = Some(code.clone());
            return NearbyEvent::Code(code);
        }
        NearbyEvent::Idle
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
        if !matches!(read_progress(&self.status_path), NearbyProgress::Failed(_)) {
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

    /// `path` as a joined identity, when it is a valid record directly in
    /// this identity directory (and of `expected`, when given).
    fn verified(&self, path: &Path, expected: Option<&str>) -> Option<MintedIdentity> {
        if path.parent() != Some(self.identity_dir.as_path()) {
            return None;
        }
        let identity = crate::onboarding::existing_identity_at(path).ok()?;
        expected
            .is_none_or(|expected| identity.fingerprint == expected)
            .then_some(identity)
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
        let deadline = tilo_test_env::hang_deadline();
        loop {
            match pairing.poll(Instant::now()) {
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
