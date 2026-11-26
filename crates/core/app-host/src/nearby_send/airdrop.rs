//! A file handed to a receiver that is not one of the person's devices, the `AirDrop` way: a
//! literal endpoint and the receiver's leaf certificate, approved by the person for one attempt
//! on staged bytes.
//!
//! An attempt stages the file beneath the workspace's own Files grant into a sealed anonymous
//! object, observes the receiver's leaf certificate, and asks the person to verify it on the
//! session's sheets ([`super::VERIFY_QUESTION`]): they type the certificate's SHA-256 fingerprint,
//! read off the receiver's console or another channel that did not carry the file. The staged
//! bytes leave once, and only on a fingerprint that matches what the receiver showed; a wrong one
//! verifies nothing and the question stands again, and declining it, or leaving it for two
//! minutes, sends nothing.
//!
//! An attempt runs on a thread of its own, since its helper may wait on a receiver for a minute.
//! It is offered only to a development host that asked for it ([`lab`]).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use runtime_collab::NearbyTransferPhase;
use sha2::{Digest as _, Sha256};
use tilo_actions::asks::{AskSender, Question};
use tilo_history::FluentArgs;
use tilo_lock::MutexRecover as _;

/// The interpreter the wire helper runs under, the system's own.
const HELPER: &str = "/usr/bin/python3";
const SENDER: &str = include_str!("../../../../../tools/protocols/airdrop_sender.py");
/// The largest file one attempt carries.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// The most the helper may answer with.
const REPLY_BYTES: u64 = 1024;
/// How long one helper run may take, unless the host lends the attempts another bound.
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(65);
/// How long an attempt waits for the person's verification.
const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long an attempt that ended stays listed, with how it ended.
const FORGET: Duration = Duration::from_secs(120);

/// Whether this host offers the path: a development build that asked for it, on a machine where
/// the wire helper is installed. A release host hands files to the person's own devices only.
#[must_use]
pub fn lab() -> bool {
    cfg!(debug_assertions)
        && std::env::var("TILO_AIRDROP_LAB").as_deref() == Ok("1")
        && Path::new(HELPER).is_file()
}

/// What the host the attempts run on lends them.
pub struct Host {
    /// Whether this host offers the path at all.
    pub enabled: bool,
    /// Opens the file a path names beneath a workspace's Files grant: the file, and the name it
    /// travels under.
    #[allow(
        clippy::type_complexity,
        reason = "one closure the host lends, named here once"
    )]
    pub open: Box<dyn Fn(&str, &Path) -> Result<(std::fs::File, String), String> + Send + Sync>,
    /// Whether the person allows file transfer now.
    pub permitted: Box<dyn Fn() -> bool + Send + Sync>,
    /// Says an attempt moved, so whoever lists them asks again.
    pub changed: Box<dyn Fn() + Send + Sync>,
    /// The person's language, for the words a refusal is said in.
    pub language: Box<dyn Fn() -> Option<String> + Send + Sync>,
    /// How long one helper run may take: a receiver that never answers fails within this bound
    /// rather than holding the attempt open.
    pub helper_timeout: Duration,
}

/// One attempt as it is listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub id: [u8; 16],
    pub name: String,
    /// The receiver, named by its endpoint.
    pub endpoint: String,
    pub bytes: u64,
    pub phase: NearbyTransferPhase,
}

/// The attempts this host runs.
#[derive(Clone)]
pub struct AirDrops {
    inner: Arc<Inner>,
}

struct Inner {
    host: Host,
    attempts: Mutex<BTreeMap<[u8; 16], Job>>,
    /// Where the person is asked to verify a receiver, once the session put its sheets up.
    questions: Mutex<Option<AskSender>>,
}

#[derive(Clone)]
struct Job {
    /// The connection that started it, when one did: it ends with that connection.
    owner: Option<u64>,
    workspace: String,
    address: String,
    port: u16,
    /// The file named, as raw bytes: each helper run is admitted against the descriptor it was
    /// staged from, reopened beneath the workspace's own Files grant.
    source_path: Vec<u8>,
    name: String,
    endpoint: String,
    bytes: u64,
    phase: NearbyTransferPhase,
    created: Instant,
    /// Set to stop the attempt at its next check, whoever stops it.
    cancelled: bool,
}

fn over(phase: NearbyTransferPhase) -> bool {
    matches!(
        phase,
        NearbyTransferPhase::Completed
            | NearbyTransferPhase::Failed
            | NearbyTransferPhase::Cancelled
    )
}

impl AirDrops {
    #[must_use]
    pub fn new(host: Host) -> Self {
        Self {
            inner: Arc::new(Inner {
                host,
                attempts: Mutex::new(BTreeMap::new()),
                questions: Mutex::new(None),
            }),
        }
    }

    /// Ask the person to verify receivers through `questions`: until then no file is handed to
    /// one, since nobody could be asked.
    pub fn ask_with(&self, questions: AskSender) {
        *self.inner.questions.lock_recover() = Some(questions);
    }

    /// Whether a file can be handed to a receiver now: the host offers the path, the person allows
    /// file transfer, and there is somewhere to ask them.
    #[must_use]
    pub fn available(&self) -> bool {
        self.inner.available()
    }

    /// Every attempt still listed, newest last.
    #[must_use]
    pub fn attempts(&self) -> Vec<Attempt> {
        let mut attempts = self.inner.attempts.lock_recover();
        prune(&mut attempts);
        attempts
            .iter()
            .map(|(id, job)| Attempt {
                id: *id,
                name: job.name.clone(),
                endpoint: job.endpoint.clone(),
                bytes: job.bytes,
                phase: job.phase,
            })
            .collect()
    }

    /// Hand the file `path` names, beneath `workspace`'s Files grant, to the receiver at
    /// `address`:`port`. The bytes are staged here and the receiver's certificate is put to the
    /// person; only their answer sends anything.
    ///
    /// # Errors
    /// The path is not offered, the file is not one an attempt carries, or another attempt runs:
    /// one runs at a time.
    pub fn start(
        &self,
        owner: Option<u64>,
        workspace: &str,
        path: &[u8],
        address: &str,
        port: u16,
    ) -> Result<[u8; 16], String> {
        let inner = &self.inner;
        if !inner.available() {
            return Err(inner.say("airdrop-error-unavailable"));
        }
        let requested = Path::new(std::ffi::OsStr::from_bytes(path));
        let (file, name) = (inner.host.open)(workspace, requested)?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return Err(inner.say("airdrop-error-source"));
        }
        let id = id();
        {
            let mut attempts = inner.attempts.lock_recover();
            prune(&mut attempts);
            if attempts.values().any(|job| !over(job.phase)) {
                return Err(inner.say("airdrop-error-busy"));
            }
            attempts.insert(
                id,
                Job {
                    owner,
                    workspace: workspace.to_owned(),
                    address: address.to_owned(),
                    port,
                    source_path: path.to_vec(),
                    name: name.clone(),
                    endpoint: format!("AirDrop · {address}:{port}"),
                    bytes: metadata.len(),
                    phase: NearbyTransferPhase::AwaitingConsent,
                    created: tilo_clock::now(),
                    cancelled: false,
                },
            );
            drop(attempts);
        }
        (inner.host.changed)();
        let working = Arc::clone(inner);
        let spawned = std::thread::Builder::new()
            .name("tilo-airdrop".into())
            .spawn(move || {
                let outcome = attempt(&working, id, file, name);
                if let Err(reason) = &outcome {
                    tracing::warn!(%reason, "an AirDrop attempt is not sent");
                }
                let mut attempts = working.attempts.lock_recover();
                if let Some(job) = attempts.get_mut(&id) {
                    job.phase = match (job.cancelled, outcome) {
                        (true, _) => NearbyTransferPhase::Cancelled,
                        (false, Ok(())) => NearbyTransferPhase::Completed,
                        (false, Err(_)) => NearbyTransferPhase::Failed,
                    };
                }
                drop(attempts);
                (working.host.changed)();
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "no thread takes an AirDrop attempt");
            inner.attempts.lock_recover().remove(&id);
            return Err(inner.say("airdrop-error-unavailable"));
        }
        Ok(id)
    }

    /// Stop the attempt `id` at its next check, whatever it is doing. Whether there was one.
    pub fn cancel(&self, id: [u8; 16]) -> bool {
        let mut attempts = self.inner.attempts.lock_recover();
        let Some(job) = attempts.get_mut(&id) else {
            return false;
        };
        job.cancelled = true;
        drop(attempts);
        (self.inner.host.changed)();
        true
    }

    /// The connection `owner` went: the attempts it started stop, as the next check finds them
    /// gone, and are forgotten.
    pub fn disconnect(&self, owner: u64) {
        let mut attempts = self.inner.attempts.lock_recover();
        attempts.retain(|_, job| job.owner != Some(owner));
    }
}

/// Forget the attempts that ended a while ago.
fn prune(attempts: &mut BTreeMap<[u8; 16], Job>) {
    let now = tilo_clock::now();
    attempts
        .retain(|_, job| !over(job.phase) || now.saturating_duration_since(job.created) < FORGET);
}

/// A fresh attempt id, never all zeroes.
fn id() -> [u8; 16] {
    loop {
        let id: [u8; 16] = rand::random();
        if id.iter().any(|byte| *byte != 0) {
            return id;
        }
    }
}

/// The 32 bytes a 64-digit hexadecimal fingerprint stands for, once it is one.
fn fingerprint(value: &str) -> Option<[u8; 32]> {
    let value = value.trim();
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    hex::decode_to_slice(value, &mut bytes).ok()?;
    Some(bytes)
}

impl Inner {
    fn available(&self) -> bool {
        self.host.enabled && (self.host.permitted)() && self.questions.lock_recover().is_some()
    }

    /// The message `key` in the person's language.
    fn say(&self, key: &str) -> String {
        tilo_workspace_service::i18n::message(
            (self.host.language)().as_deref(),
            key,
            &FluentArgs::new(),
        )
    }

    /// Whether the attempt `id` may still run: nobody stopped it, the host still offers the path
    /// and the person allows it, and the file named is still the one it named, inside the
    /// workspace's own Files grant.
    fn admitted(&self, id: [u8; 16], identity: &std::fs::Metadata) -> bool {
        if !self.available() {
            return false;
        }
        let job = self.attempts.lock_recover().get(&id).cloned();
        let Some(job) = job.filter(|job| !job.cancelled) else {
            return false;
        };
        let requested = Path::new(std::ffi::OsStr::from_bytes(&job.source_path));
        (self.host.open)(&job.workspace, requested)
            .and_then(|(file, _)| file.metadata().map_err(|error| error.to_string()))
            .is_ok_and(|current| current.dev() == identity.dev() && current.ino() == identity.ino())
    }

    /// Ask the person to verify the receiver that showed the certificate `observed`, and wait for
    /// their answer for at most [`CONSENT_TIMEOUT`], while the attempt stays admitted. Only a
    /// fingerprint that matches `observed` verifies it; an attempt that ends before it is verified
    /// is withdrawn, not failed: nothing was sent.
    fn verified(
        &self,
        id: [u8; 16],
        observed: [u8; 32],
        admitted: &impl Fn() -> bool,
    ) -> Result<(), String> {
        let withdraw = |key: &str| {
            if let Some(job) = self.attempts.lock_recover().get_mut(&id) {
                job.cancelled = true;
            }
            Err(self.say(key))
        };
        let questions = self.questions.lock_recover().clone();
        let job = self.attempts.lock_recover().get(&id).cloned();
        let (Some(questions), Some(job)) = (questions, job) else {
            return withdraw("airdrop-error-unavailable");
        };
        let question = Question {
            handle: format!("airdrop:{}", hex::encode(id)),
            kind: super::VERIFY_QUESTION.to_owned(),
            app_id: None,
            label: job.endpoint.clone(),
            details: serde_json::json!(super::VerifyQuestion {
                name: job.name,
                bytes: job.bytes,
                endpoint: job.endpoint,
                certificate: hex::encode(observed),
            }),
        };
        let started = tilo_clock::now();
        let mut asked = questions.ask(question.clone());
        loop {
            if !admitted() {
                return withdraw("airdrop-error-unavailable");
            }
            let now = tilo_clock::now();
            let left = CONSENT_TIMEOUT.saturating_sub(now.saturating_duration_since(started));
            if left.is_zero() {
                return withdraw("airdrop-error-declined");
            }
            let next = now.checked_add(left.min(Duration::from_millis(100)));
            match asked.until(Some(next.unwrap_or(now))) {
                None => {}
                Some(None) => return withdraw("airdrop-error-declined"),
                Some(Some(answer))
                    if answer.values.first().and_then(|typed| fingerprint(typed))
                        == Some(observed) =>
                {
                    return Ok(());
                }
                // A fingerprint that is not the receiver's verifies nothing: the question stands
                // again.
                Some(Some(_)) => asked = questions.ask(question.clone()),
            }
        }
    }
}

/// One attempt: stage the file, observe the receiver's leaf certificate, put the verification to
/// the person and wait for their answer, then hand the staged bytes over once and check what the
/// receiver says it took.
fn attempt(inner: &Inner, id: [u8; 16], file: std::fs::File, name: String) -> Result<(), String> {
    let Some((address, port)) = inner
        .attempts
        .lock_recover()
        .get(&id)
        .map(|job| (job.address.clone(), job.port))
    else {
        return Err(inner.say("airdrop-error-unavailable"));
    };
    let identity = file.metadata().map_err(|error| error.to_string())?;
    let admitted = || inner.admitted(id, &identity);
    let staged = Staged::stage(inner, file, name, &admitted)?;
    if !admitted() {
        return Err(inner.say("airdrop-error-unavailable"));
    }
    let observed = helper(inner, &address, port, None, &admitted)?
        .get("certificate_sha256")
        .and_then(serde_json::Value::as_str)
        .and_then(fingerprint)
        .ok_or_else(|| inner.say("airdrop-error-failed"))?;
    inner.verified(id, observed, &admitted)?;
    if let Some(job) = inner.attempts.lock_recover().get_mut(&id) {
        job.phase = NearbyTransferPhase::Transferring;
    }
    (inner.host.changed)();
    if !admitted() {
        return Err(inner.say("airdrop-error-unavailable"));
    }
    let pinned = hex::encode(observed);
    let reply = helper(inner, &address, port, Some((&staged, &pinned)), &admitted)?;
    if reply.get("bytes").and_then(serde_json::Value::as_u64) != Some(staged.bytes)
        || reply
            .get("content_hash")
            .and_then(serde_json::Value::as_str)
            != Some(staged.hash.as_str())
    {
        return Err(inner.say("airdrop-error-failed"));
    }
    Ok(())
}

/// One helper run against `address`:`port`: the observation the person verifies, or the one send
/// of the staged bytes that they approved.
fn helper(
    inner: &Inner,
    address: &str,
    port: u16,
    send: Option<(&Staged, &str)>,
    admitted: &impl Fn() -> bool,
) -> Result<serde_json::Value, String> {
    let mut command = command();
    command.args([address, &port.to_string()]);
    if let Some((staged, certificate)) = send {
        command
            .args([
                "send",
                "--certificate-sha256",
                certificate,
                "--source-fd",
                "9",
            ])
            .arg(format!("--name={}", staged.name));
        inherit(&mut command, Some(staged.file.as_raw_fd()), None);
    } else {
        command.arg("inspect");
        inherit(&mut command, None, None);
    }
    supervise(inner, command, admitted, inner.host.helper_timeout)
}

fn command() -> Command {
    let mut command = Command::new(HELPER);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["-I", "-c", SENDER])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

/// Give the helper only the descriptors it needs, each duplicated to the number the script named.
fn inherit(command: &mut Command, source: Option<i32>, staging: Option<i32>) {
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
            Ok(())
        });
    }
    let handed = [(source, 9), (staging, 10)]
        .into_iter()
        .filter_map(|(fd, target)| Some((fd?, target)))
        .collect();
    launcher::inherit::hand_down(command, handed, false);
}

/// Run `command` while `admitted` holds, for at most `limit`, and read back its one JSON answer.
fn supervise(
    inner: &Inner,
    mut command: Command,
    admitted: &impl Fn() -> bool,
    limit: Duration,
) -> Result<serde_json::Value, String> {
    let failed = || inner.say("airdrop-error-failed");
    if !admitted() {
        return Err(inner.say("airdrop-error-unavailable"));
    }
    let mut child = command.spawn().map_err(|_| failed())?;
    let started = tilo_clock::now();
    loop {
        let withdrawn = !admitted();
        if withdrawn || tilo_clock::now().saturating_duration_since(started) >= limit {
            let _ = child.kill();
            let _ = child.wait();
            // A helper that ran out of time failed; only a withdrawn admission is the host,
            // permission or Files access.
            return Err(if withdrawn {
                inner.say("airdrop-error-unavailable")
            } else {
                failed()
            });
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(failed());
                }
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failed());
            }
        }
    }
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(failed)?
        .take(REPLY_BYTES + 1)
        .read_to_end(&mut output)
        .map_err(|_| failed())?;
    if u64::try_from(output.len()).unwrap_or(u64::MAX) > REPLY_BYTES {
        return Err(failed());
    }
    serde_json::from_slice(&output).map_err(|_| failed())
}

/// One approved file, staged into a sealed anonymous object: what the person verified is what the
/// receiver is handed, whatever happens to the file they named meanwhile.
struct Staged {
    file: std::fs::File,
    // Keep the approved inode alive for every admission check. Closing it
    // after staging could let an unlink/recreate reuse its inode number.
    _source: std::fs::File,
    name: String,
    bytes: u64,
    hash: String,
}

impl Staged {
    fn stage(
        inner: &Inner,
        input: std::fs::File,
        name: String,
        admitted: &impl Fn() -> bool,
    ) -> Result<Self, String> {
        let unreadable = || inner.say("airdrop-error-source");
        let before = input.metadata().map_err(|_| unreadable())?;
        if !before.is_file() || before.len() > MAX_BYTES {
            return Err(unreadable());
        }
        // SAFETY: constant terminated name and flags create a new owned fd.
        let fd = unsafe {
            libc::memfd_create(
                c"tilo-airdrop".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(unreadable());
        }
        // SAFETY: memfd_create returned a descriptor exclusively owned here.
        let writable = unsafe { std::fs::File::from_raw_fd(fd) };
        let mut command = command();
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
        inherit(
            &mut command,
            Some(input.as_raw_fd()),
            Some(writable.as_raw_fd()),
        );
        let reply = supervise(inner, command, admitted, inner.host.helper_timeout)?;
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        // SAFETY: fcntl operates only on our anonymous staging descriptor.
        if unsafe { libc::fcntl(writable.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0 {
            return Err(unreadable());
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC)
            .open(format!("/proc/self/fd/{}", writable.as_raw_fd()))
            .map_err(|_| unreadable())?;
        let mut bytes = Vec::new();
        std::io::Read::by_ref(&mut file)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| unreadable())?;
        let hash = hex::encode(Sha256::digest(&bytes));
        if u64::try_from(bytes.len()).ok() != Some(before.len())
            || reply.get("bytes").and_then(serde_json::Value::as_u64) != Some(before.len())
            || reply
                .get("content_hash")
                .and_then(serde_json::Value::as_str)
                != Some(hash.as_str())
        {
            return Err(unreadable());
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
