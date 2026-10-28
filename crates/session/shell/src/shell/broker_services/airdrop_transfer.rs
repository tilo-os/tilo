//! Per-attempt external receiver authority. `AirDrop` never enrolls a Tilo peer.
//!
//! The local Files route supplies a source and literal endpoint, never a pin.
//! The compositor asks the present person for an independently obtained leaf
//! fingerprint and approval of immutable staged bytes. Both TLS connections
//! and the eventual upload are pinned; no trust survives the attempt.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use runtime_collab::NearbyTransferPhase;
use sha2::{Digest, Sha256};

use super::nearby_transfer::{FILES_APP_ID, files_grants, nearby_source_from_grants};
use crate::shell::continuity_policy::{ContinuityCapability, ContinuityPolicySnapshot};
use crate::shell::nearby_transfer::SecureNearbySource;
use crate::shell::portal_dialog::{FieldReviewRequest, PortalDialogQueue, ShortcutStepDecision};
use crate::shell::state::WorkspaceTreeHandle;

const SENDER: &str = include_str!("../../../../../../tools/protocols/airdrop_sender.py");
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_JOBS: usize = 32;
const MAX_REPLY_BYTES: u64 = 1024;
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
const PROCESS_TIMEOUT: Duration = Duration::from_secs(65);

type Admission = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone)]
pub(super) struct AirDropScope {
    pub peer_id: u64,
    pub channel: String,
    pub workspace: String,
    pub person: String,
}

pub(super) struct AirDropRequest {
    pub scope: AirDropScope,
    pub address: String,
    pub port: u16,
    pub source_path: Vec<u8>,
}

struct Job {
    scope: AirDropScope,
    name: String,
    endpoint: String,
    bytes: u64,
    phase: NearbyTransferPhase,
    cancel: Arc<AtomicBool>,
    created: Instant,
}

#[derive(Clone)]
pub(super) struct AirDropStatus {
    pub id: [u8; 16],
    pub name: String,
    pub endpoint: String,
    pub bytes: u64,
    pub phase: NearbyTransferPhase,
}

#[derive(Clone)]
pub(super) struct NativeAirDropTransfer {
    membership: WorkspaceTreeHandle,
    queue: PortalDialogQueue,
    attended: Admission,
    enabled: bool,
    jobs: Arc<Mutex<HashMap<[u8; 16], Job>>>,
    active: Arc<AtomicBool>,
    /// How long one helper process may run, interpreter start included.
    process_timeout: Duration,
}

impl NativeAirDropTransfer {
    pub(super) fn session(membership: WorkspaceTreeHandle) -> Self {
        Self::new(
            membership,
            PortalDialogQueue::global(),
            Arc::new(crate::shell::app_action_consent::session_attended),
            cfg!(debug_assertions)
                && std::env::var("TILO_AIRDROP_LAB").as_deref() == Ok("1")
                && std::path::Path::new("/usr/bin/python3").is_file(),
        )
    }

    fn new(
        membership: WorkspaceTreeHandle,
        queue: PortalDialogQueue,
        attended: Admission,
        enabled: bool,
    ) -> Self {
        Self {
            membership,
            queue,
            attended,
            enabled,
            jobs: Arc::default(),
            active: Arc::default(),
            process_timeout: PROCESS_TIMEOUT,
        }
    }

    /// Give each helper `limit` instead of [`PROCESS_TIMEOUT`]: a loaded test
    /// runner can take longer than that just to start the interpreter.
    #[cfg(test)]
    fn with_process_timeout(mut self, limit: Duration) -> Self {
        self.process_timeout = limit;
        self
    }

    pub(super) fn available(&self) -> bool {
        self.enabled
            && (self.attended)()
            && ContinuityPolicySnapshot::current().permits(ContinuityCapability::FileTransfer)
    }

    pub(super) fn available_to(&self, workspace: &str, person: Option<&str>) -> bool {
        self.available()
            && person.is_some_and(|person| {
                crate::shell::state::RoomId::parse(workspace).is_some_and(|room| {
                    crate::shell::state::local_user_id_for_room(&room) == person
                })
            })
    }

    pub(super) fn contains(&self, id: [u8; 16]) -> bool {
        self.jobs.lock().is_ok_and(|jobs| jobs.contains_key(&id))
    }

    pub(super) fn start(
        &self,
        request: AirDropRequest,
        source: SecureNearbySource,
    ) -> Result<[u8; 16], String> {
        if !self.available() || !Self::local_person(&request.scope) {
            return Err(error("airdrop-error-unavailable"));
        }
        let metadata = source.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(error("airdrop-error-source"));
        }
        let permit = ActiveTransfer::reserve(&self.active)?;
        let id = crate::random::bytes::<16>();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut jobs = self
                .jobs
                .lock()
                .map_err(|_| error("airdrop-error-unavailable"))?;
            jobs.retain(|_, job| !terminal(job.phase) || job.created.elapsed() < CONSENT_TIMEOUT);
            if jobs.len() >= MAX_JOBS || jobs.values().any(|job| !terminal(job.phase)) {
                return Err(error("airdrop-error-busy"));
            }
            jobs.insert(
                id,
                Job {
                    scope: request.scope.clone(),
                    name: source.name().into(),
                    endpoint: format!("AirDrop · {}:{}", request.address, request.port),
                    bytes: metadata.len(),
                    phase: NearbyTransferPhase::AwaitingConsent,
                    cancel: Arc::clone(&cancel),
                    created: Instant::now(),
                },
            );
            drop(jobs);
        }
        crate::shell::device_fabric_runtime::note_nearby_changed();
        let service = self.clone();
        if std::thread::Builder::new()
            .name("tilo-airdrop".into())
            .spawn(move || {
                let _permit = permit;
                let result = service.run(id, &request, source, &cancel);
                let phase = if cancel.load(Ordering::Acquire) {
                    NearbyTransferPhase::Cancelled
                } else if result.is_ok() {
                    NearbyTransferPhase::Completed
                } else {
                    NearbyTransferPhase::Failed
                };
                service.phase(id, phase);
            })
            .is_err()
        {
            self.jobs
                .lock()
                .map_err(|_| error("airdrop-error-unavailable"))?
                .remove(&id);
            return Err(error("airdrop-error-unavailable"));
        }
        Ok(id)
    }

    fn local_person(scope: &AirDropScope) -> bool {
        crate::shell::state::RoomId::parse(&scope.workspace)
            .is_some_and(|room| crate::shell::state::local_user_id_for_room(&room) == scope.person)
    }

    fn admitted(
        &self,
        request: &AirDropRequest,
        cancel: &AtomicBool,
        identity: &std::fs::Metadata,
    ) -> bool {
        !cancel.load(Ordering::Acquire)
            && self.available()
            && Self::local_person(&request.scope)
            && nearby_source_from_grants(
                &request.source_path,
                &files_grants(&self.membership, &request.scope.workspace),
            )
            .and_then(|source| source.metadata())
            .is_ok_and(|current| current.dev() == identity.dev() && current.ino() == identity.ino())
    }

    fn run(
        &self,
        id: [u8; 16],
        request: &AirDropRequest,
        source: SecureNearbySource,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        let identity = source.metadata()?;
        let still_admitted = || self.admitted(request, cancel, &identity);
        let staged = StagedFile::new(source, &still_admitted, self.process_timeout)?;
        if !still_admitted() {
            return Err(error("airdrop-error-unavailable"));
        }
        let observed = helper(
            request,
            &staged,
            None,
            &still_admitted,
            self.process_timeout,
        )?;
        let observed = observed
            .get("certificate_sha256")
            .and_then(serde_json::Value::as_str)
            .and_then(pin)
            .ok_or_else(|| error("airdrop-error-failed"))?;
        let handle = format!("airdrop:{}", hex::encode(id));
        let (answer, receiver) = mpsc::channel();
        self.queue.enqueue_field_review(
            FieldReviewRequest {
                handle: handle.clone(),
                app_id: FILES_APP_ID.into(),
                title: error("airdrop-verify-title"),
                detail: review_lines(&crate::shell::i18n::t_args(
                    "airdrop-verify-detail",
                    &[
                        ("name", staged.name.as_str().into()),
                        ("bytes", staged.bytes.to_string().into()),
                        (
                            "endpoint",
                            format!("{}:{}", request.address, request.port).into(),
                        ),
                    ],
                )),
                confirm_label: error("airdrop-verify-send-once"),
            },
            move |value| {
                if pin(value) == Some(observed) {
                    Ok(())
                } else {
                    Err(error("airdrop-error-verification"))
                }
            },
            move |decision| {
                let _ = answer.send(decision);
            },
        )?;
        let decision = wait_for_consent(&receiver, &still_admitted);
        // Also withdraw a timed-out, disconnected, locked or revoked request.
        self.queue.cancel(&handle);
        let ShortcutStepDecision::Confirmed { text, .. } = decision.inspect_err(|_| {
            cancel.store(true, Ordering::Release);
        })?
        else {
            cancel.store(true, Ordering::Release);
            return Err(error("airdrop-error-declined"));
        };
        if pin(&text) != Some(observed) {
            return Err(error("airdrop-error-verification"));
        }
        if !still_admitted() {
            return Err(error("airdrop-error-unavailable"));
        }
        self.phase(id, NearbyTransferPhase::Transferring);
        let reply = helper(
            request,
            &staged,
            Some(&hex::encode(observed)),
            &still_admitted,
            self.process_timeout,
        )?;
        if reply.get("bytes").and_then(serde_json::Value::as_u64) != Some(staged.bytes)
            || reply
                .get("content_hash")
                .and_then(serde_json::Value::as_str)
                != Some(&staged.hash)
        {
            return Err(error("airdrop-error-failed"));
        }
        Ok(())
    }

    fn phase(&self, id: [u8; 16], phase: NearbyTransferPhase) {
        if let Ok(mut jobs) = self.jobs.lock()
            && let Some(job) = jobs.get_mut(&id)
        {
            job.phase = phase;
        }
        // Files learns of the new phase by asking for a snapshot.
        crate::shell::device_fabric_runtime::note_nearby_changed();
    }

    pub(super) fn statuses(&self, peer: u64, channel: &str) -> Vec<AirDropStatus> {
        self.jobs
            .lock()
            .map(|jobs| {
                jobs.iter()
                    .filter(|(_, job)| job.scope.peer_id == peer && job.scope.channel == channel)
                    .map(|(id, job)| AirDropStatus {
                        id: *id,
                        name: job.name.clone(),
                        endpoint: job.endpoint.clone(),
                        bytes: job.bytes,
                        phase: job.phase,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(super) fn cancel(&self, id: [u8; 16]) -> bool {
        let Ok(jobs) = self.jobs.lock() else {
            return false;
        };
        let Some(job) = jobs.get(&id) else {
            return false;
        };
        job.cancel.store(true, Ordering::Release);
        true
    }

    pub(super) fn disconnect(&self, peer: u64) {
        if let Ok(mut jobs) = self.jobs.lock() {
            for job in jobs.values().filter(|job| job.scope.peer_id == peer) {
                job.cancel.store(true, Ordering::Release);
            }
            jobs.retain(|_, job| job.scope.peer_id != peer);
        }
    }
}

fn terminal(phase: NearbyTransferPhase) -> bool {
    matches!(
        phase,
        NearbyTransferPhase::Completed
            | NearbyTransferPhase::Failed
            | NearbyTransferPhase::Cancelled
    )
}

fn error(key: &str) -> String {
    crate::shell::i18n::t(key)
}

fn pin(value: &str) -> Option<[u8; 32]> {
    let value = value.trim();
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(value, &mut bytes).ok()?;
    Some(bytes)
}

fn wait_for_consent(
    answer: &mpsc::Receiver<ShortcutStepDecision>,
    admitted: &impl Fn() -> bool,
) -> Result<ShortcutStepDecision, String> {
    let started = Instant::now();
    loop {
        if !admitted() || started.elapsed() >= CONSENT_TIMEOUT {
            return Err(error("airdrop-error-unavailable"));
        }
        match answer.recv_timeout(Duration::from_millis(100)) {
            Ok(answer) => return Ok(answer),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(error("airdrop-error-declined"));
            }
        }
    }
}

struct StagedFile {
    file: File,
    // Keep the approved inode alive for every admission check. Closing it
    // after staging could let an unlink/recreate reuse its inode number.
    _source: File,
    name: String,
    bytes: u64,
    hash: String,
}

impl StagedFile {
    fn new(
        source: SecureNearbySource,
        admitted: &impl Fn() -> bool,
        limit: Duration,
    ) -> Result<Self, String> {
        let before = source.metadata()?;
        let name = source.name().to_owned();
        if !before.is_file() || before.len() > MAX_BYTES {
            return Err(error("airdrop-error-source"));
        }
        let input = source.into_file();
        // SAFETY: constant terminated name and flags create a new owned fd.
        let fd = unsafe {
            libc::memfd_create(
                c"tilo-airdrop".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(error("airdrop-error-source"));
        }
        // SAFETY: memfd_create returned a descriptor exclusively owned here.
        let writable = unsafe { File::from_raw_fd(fd) };
        let mut command = command_base(SENDER);
        command
            .args([
                "127.0.0.1",
                "1",
                "stage",
                "--source-fd",
                "9",
                "--staged-fd",
                "10",
            ])
            .arg(format!("--name={name}"));
        inherit_descriptors(
            &mut command,
            Some(input.as_raw_fd()),
            Some(writable.as_raw_fd()),
        );
        let reply = supervise(command, admitted, limit)?;
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        // SAFETY: fcntl operates only on our anonymous staging descriptor.
        if unsafe { libc::fcntl(writable.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
            return Err(error("airdrop-error-source"));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(format!("/proc/self/fd/{}", writable.as_raw_fd()))
            .map_err(|_| error("airdrop-error-source"))?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| error("airdrop-error-source"))?;
        let hash = hex::encode(Sha256::digest(&bytes));
        if u64::try_from(bytes.len()).ok() != Some(before.len())
            || reply.get("bytes").and_then(serde_json::Value::as_u64) != Some(before.len())
            || reply
                .get("content_hash")
                .and_then(serde_json::Value::as_str)
                != Some(hash.as_str())
        {
            return Err(error("airdrop-error-source"));
        }
        Ok(Self {
            file,
            _source: input,
            name,
            bytes: before.len(),
            hash,
        })
    }
}

fn helper(
    request: &AirDropRequest,
    source: &StagedFile,
    certificate: Option<&str>,
    admitted: &impl Fn() -> bool,
    limit: Duration,
) -> Result<serde_json::Value, String> {
    let mut command = command_base(SENDER);
    command.args([&request.address, &request.port.to_string()]);
    if let Some(certificate) = certificate {
        command
            .args([
                "send",
                "--certificate-sha256",
                certificate,
                "--source-fd",
                "9",
            ])
            .arg(format!("--name={}", source.name));
        inherit_descriptors(&mut command, Some(source.file.as_raw_fd()), None);
    } else {
        command.arg("inspect");
        inherit_descriptors(&mut command, None, None);
    }
    supervise(command, admitted, limit)
}

fn command_base(script: &str) -> Command {
    let mut command = Command::new("/usr/bin/python3");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["-I", "-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

fn inherit_descriptors(command: &mut Command, source: Option<i32>, staging: Option<i32>) {
    // SAFETY: only async-signal-safe syscalls run between fork and exec. All
    // inherited descriptors become CLOEXEC. The fixed helper receives only
    // a read-only source and (during staging only) its anonymous staging fd.
    unsafe {
        command.pre_exec(move || {
            if libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            // Duplicate first so an original fd9/fd10 cannot be overwritten.
            let source_copy = match source {
                Some(source) => {
                    let copy = libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 11);
                    if copy < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Some(copy)
                }
                None => None,
            };
            let staging_copy = match staging {
                Some(staging) => {
                    let copy = libc::fcntl(staging, libc::F_DUPFD_CLOEXEC, 11);
                    if copy < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Some(copy)
                }
                None => None,
            };
            if let Some(copy) = source_copy
                && libc::dup2(copy, 9) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if let Some(copy) = staging_copy
                && libc::dup2(copy, 10) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Run `command` while `admitted` holds, for at most `limit`.
fn supervise(
    mut command: Command,
    admitted: &impl Fn() -> bool,
    limit: Duration,
) -> Result<serde_json::Value, String> {
    if !admitted() {
        return Err(error("airdrop-error-unavailable"));
    }
    let mut child = command.spawn().map_err(|_| error("airdrop-error-failed"))?;
    let started = Instant::now();
    let result = loop {
        let withdrawn = !admitted();
        if withdrawn || started.elapsed() >= limit {
            let _ = child.kill();
            let _ = child.wait();
            // A helper that ran out of time failed; only a withdrawn
            // admission is the session, permission or Files access.
            break Err(error(if withdrawn {
                "airdrop-error-unavailable"
            } else {
                "airdrop-error-failed"
            }));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(error("airdrop-error-failed"))
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error("airdrop-error-failed"));
            }
        }
    };
    result?;
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| error("airdrop-error-failed"))?
        .take(MAX_REPLY_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(|_| error("airdrop-error-failed"))?;
    if output.len() > usize::try_from(MAX_REPLY_BYTES).unwrap_or(0) {
        return Err(error("airdrop-error-failed"));
    }
    serde_json::from_slice(&output).map_err(|_| error("airdrop-error-failed"))
}

// Keep exact review text inside the trusted sheet, including long basenames and
// IPv6 addresses. Security-surface shaping disables deceptive text ligatures.
fn review_lines(value: &str) -> String {
    value
        .lines()
        .map(|line| {
            let mut output = String::new();
            for (index, ch) in line.chars().enumerate() {
                if index != 0 && index % 32 == 0 {
                    output.push('\n');
                }
                output.push(ch);
            }
            output
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct ActiveTransfer(Arc<AtomicBool>);

impl ActiveTransfer {
    fn reserve(active: &Arc<AtomicBool>) -> Result<Self, String> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| error("airdrop-error-busy"))?;
        Ok(Self(Arc::clone(active)))
    }
}

impl Drop for ActiveTransfer {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
