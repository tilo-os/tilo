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
    Failed { error: String },
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
            _ => NearbyProgress::Failed {
                error: t("onboarding-nearby-invalid-identity"),
            },
        },
        Some("failed") => NearbyProgress::Failed {
            error: field("error").unwrap_or_default(),
        },
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
    /// Pairing ended without joining; the message says why and how to retry.
    /// Pairing is over.
    Failed(String),
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
    /// The message to show when the helper cannot be started.
    pub(crate) fn start(
        identity_root: &Path,
        identity_dir: &Path,
        status_path: &Path,
        label: &str,
    ) -> Result<Self, String> {
        let lease = tilo_relay_protocol::ProcessLease::capture(std::process::id())
            .map_err(|error| error.to_string())?;
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

    fn spawn(
        mut command: Command,
        identity_dir: &Path,
        status_path: &Path,
        bound: Duration,
    ) -> Result<Self, String> {
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
            .map_err(|error| error.to_string())?;
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
                None => self.fail(t("onboarding-nearby-invalid-identity")),
            };
        }
        // The join selects its identity as active as part of its commit. A
        // verified active identity is a completed join even when its status
        // update was lost.
        if let Some(joined) = runtime_identity::resolve_active_identity(&self.identity_dir)
            .and_then(|path| self.verified(&path, None))
        {
            return NearbyEvent::Joined(joined);
        }
        if let Some(status) = exited {
            return self.exited(status, progress);
        }
        if now >= self.deadline {
            return self.fail(t("onboarding-nearby-timed-out"));
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
        let detail = match progress {
            NearbyProgress::Failed { error } if !error.is_empty() => Some(error),
            _ => stderr
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| line.trim_start_matches("Error: ").to_owned()),
        };
        let message = if status.success() {
            t("onboarding-nearby-invalid-identity")
        } else {
            detail.map_or_else(
                || t("onboarding-nearby-failed"),
                |detail| {
                    t_args(
                        "onboarding-nearby-failed-detail",
                        &[("detail", detail.into())],
                    )
                },
            )
        };
        self.fail(message)
    }

    /// End this attempt with `message`, recording it in the status file
    /// unless the helper already recorded why it stopped.
    fn fail(&mut self, message: String) -> NearbyEvent {
        terminate(&mut self.child);
        if !matches!(
            read_progress(&self.status_path),
            NearbyProgress::Failed { .. }
        ) {
            let status = serde_json::json!({ "state": "failed", "error": message });
            if let Ok(bytes) = serde_json::to_vec(&status) {
                let _ =
                    tilo_files::native::atomic::write_with_mode(&self.status_path, &bytes, 0o600);
            }
        }
        NearbyEvent::Failed(message)
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
    let Some(mut child) = child.take() else {
        return;
    };
    let Ok(pid) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let group = pid.saturating_neg();
    // SAFETY: the process is spawned with `process_group(0)`, making its
    // validated PID the process-group ID. A negative target is confined to
    // that group and cannot signal the compositor's own group.
    let _ = unsafe { libc::kill(group, libc::SIGTERM) };
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(750))
        .unwrap_or_else(Instant::now);
    loop {
        let leader_finished = child.try_wait().ok().flatten().is_some();
        // Signal 0 only checks whether any descendant still occupies the
        // dedicated group. Do not return just because the leader exited: a
        // worker it started may still be winding down.
        let group_gone = unsafe { libc::kill(group, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if leader_finished && group_gone {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: same dedicated process group as above. This is the bounded
    // fallback for a worker that ignored SIGTERM.
    let _ = unsafe { libc::kill(group, libc::SIGKILL) };
    let _ = child.wait();
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
                NearbyEvent::Failed(t("onboarding-nearby-invalid-identity"))
            );
        }
        // Phase `a_selected_active_identity_completes_a_lost_status_update`.
        {
            let fixture = fixture();
            let record = joined_record(&fixture, [9; 32]);
            runtime_identity::set_active_identity(&fixture.identity_dir, "identity-joined")
                .expect("select joined identity");
            let mut pairing = helper(&fixture, "exec sleep 30", PROCESS_BOUND);
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Joined(MintedIdentity {
                    fingerprint: hex::encode([9; 32]),
                    path: record,
                })
            );
        }
        // Phase `the_recorded_helper_error_is_shown_with_a_retry`.
        {
            let fixture = fixture();
            let script = r#"printf '%s' '{"state":"failed","error":"Permission denied (os error 13)"}' > "$1"; echo "Error: Permission denied (os error 13)" >&2; exit 1"#;
            let mut pairing = helper(&fixture, script, PROCESS_BOUND);
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Failed(t_args(
                    "onboarding-nearby-failed-detail",
                    &[("detail", "Permission denied (os error 13)".into())],
                ))
            );
        }
        // Phase `an_exit_without_a_status_reports_stderr_and_records_it`.
        {
            let fixture = fixture();
            let script = r#"echo "Error: Not authorized" >&2; exit 127"#;
            let mut pairing = helper(&fixture, script, PROCESS_BOUND);
            let expected = t_args(
                "onboarding-nearby-failed-detail",
                &[("detail", "Not authorized".into())],
            );
            assert_eq!(
                poll_until_over(&mut pairing),
                NearbyEvent::Failed(expected.clone())
            );
            assert_eq!(
                read_progress(&fixture.status),
                NearbyProgress::Failed { error: expected }
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
            assert_eq!(event, NearbyEvent::Failed(t("onboarding-nearby-timed-out")));
            assert!(pairing.child.is_none(), "the stuck helper was stopped");
            assert_eq!(
                read_progress(&fixture.status),
                NearbyProgress::Failed {
                    error: t("onboarding-nearby-timed-out")
                }
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
                NearbyEvent::Failed(t_args(
                    "onboarding-nearby-failed-detail",
                    &[("detail", "Permission denied (os error 13)".into())],
                ))
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
                NearbyEvent::Failed(t("onboarding-nearby-invalid-identity"))
            );
        }
    }
}
