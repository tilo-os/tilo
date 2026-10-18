//! Root-authenticated retained Home admission for an ordinary signed GUI.
//! Wire declarations select an already admitted operation. They are never
//! person, source, workspace or package authority. The installed root actor
//! independently verifies those before issuing its sealed view receipt.

use crate::{
    capabilities::Participation,
    ordinary_namespace::{ConnectedNamespace, Handoff, OrdinaryNamespaceOwner},
    owned_service::{ServiceAuthority, ServiceCancellation},
    run::{RunError, RunOptions},
};
use getrandom::rand_core::TryRng as _;
use runtime_portal::process_binding::{self, ExecutingPeer};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileExt, MetadataExt, OpenOptionsExt},
            net::UnixStream,
        },
    },
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub mod installed_copy;

pub const HELPER_PATH: &str = "/usr/libexec/tilo-prepared-setup";
pub const SANDBOX_HOME: &str = "/run/tilo/retained-home";
pub(crate) const VIEW_FD: RawFd = 55;
pub(crate) const ALIAS_VIEW_FD: RawFd = 58;
pub(crate) const ALIAS_ARGS_FD: RawFd = 59;

/// Original root's approved native Home spelling and held source identity.
/// This is shape-only data; authority comes from the protected source record,
/// encrypted setup binding and the authenticated root view receipt.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalHomeAlias {
    path: Vec<u8>,
    source_dev: u64,
    source_ino: u64,
}
impl OriginalHomeAlias {
    /// # Errors
    /// Refuses unreadable metadata, a non-directory source, or a noncanonical Home path.
    pub fn capture(path: &Path, source: &File) -> io::Result<Self> {
        let metadata = source.metadata()?;
        if !metadata.is_dir() {
            return Err(refused("original Home is not a directory"));
        }
        let value = Self {
            path: path.as_os_str().as_bytes().to_vec(),
            source_dev: metadata.dev(),
            source_ino: metadata.ino(),
        };
        value.validate()?;
        Ok(value)
    }
    /// # Errors
    /// Refuses missing source identity or a noncanonical, relative or NUL-containing path.
    pub fn validate(&self) -> io::Result<()> {
        if self.source_dev == 0
            || self.source_ino == 0
            || self.path.len() > 4095
            || self.path.first() != Some(&b'/')
            || self.path.contains(&0)
            || self.path.len() < 2
            || self.path.last() == Some(&b'/')
            || self
                .path
                .get(1..)
                .ok_or_else(|| refused("original Home alias is empty"))?
                .split(|byte| *byte == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b"..")
        {
            return Err(refused(
                "original Home alias is not canonical native absolute path",
            ));
        }
        Ok(())
    }
    pub fn source_ino(&self) -> u64 {
        self.source_ino
    }
    fn path(&self) -> &Path {
        Path::new(std::ffi::OsStr::from_bytes(&self.path))
    }
    /// Shape-only native fixture access to the production argument writer.
    /// # Errors
    /// Refuses invalid aliases, truncated or conflicting mounts and opaque
    /// overlay arguments; reports argument-file creation, write or seal errors.
    #[cfg(feature = "native-home-alias-qualification")]
    pub fn qualification_arguments(&self, argv: &[String], writable: bool) -> io::Result<File> {
        self.args(argv, writable)
    }
    fn args(&self, argv: &[String], writable: bool) -> io::Result<File> {
        self.validate()?;
        let alias = self.path();
        // A destination under an existing host-backed mount would expose or
        // mutate its parents. An ancestor would hide already admitted state.
        for (index, flag) in argv.iter().enumerate() {
            if matches!(
                flag.as_str(),
                "--args" | "--overlay-src" | "--overlay" | "--tmp-overlay" | "--ro-overlay"
            ) {
                return Err(refused(
                    "original Home alias cannot inspect opaque or overlay mounts",
                ));
            }
            let offset = match flag.as_str() {
                "--bind" | "--ro-bind" | "--dev-bind" | "--bind-try" | "--ro-bind-try"
                | "--dev-bind-try" | "--bind-fd" | "--ro-bind-fd" | "--symlink" | "--file"
                | "--bind-data" | "--ro-bind-data" => 2,
                "--tmpfs" | "--proc" | "--dev" | "--mqueue" => 1,
                _ => continue,
            };
            let destination = argv
                .get(
                    index
                        .checked_add(offset)
                        .ok_or(io::ErrorKind::InvalidInput)?,
                )
                .ok_or_else(|| refused("alias mount argv truncated"))?;
            let destination = Path::new(destination);
            if alias.starts_with(destination) || destination.starts_with(alias) {
                return Err(refused("original Home alias collides with admitted mount"));
            }
        }
        // Never admit an alias at system/private roots, even if this particular
        // package's mount list happens to omit one of them.
        for reserved in [
            "/run", "/usr", "/etc", "/proc", "/sys", "/dev", "/bin", "/sbin", "/lib", "/lib64",
        ] {
            if alias.starts_with(reserved) || Path::new(reserved).starts_with(alias) {
                return Err(refused(
                    "original Home alias collides with system authority",
                ));
            }
        }
        let mut parts = Vec::<Vec<u8>>::new();
        let mut ancestors: Vec<_> = alias
            .ancestors()
            .skip(1)
            .filter(|path| *path != Path::new("/"))
            .collect();
        ancestors.reverse();
        for parent in ancestors {
            parts.push(b"--dir".to_vec());
            parts.push(parent.as_os_str().as_bytes().to_vec());
        }
        parts.push(if writable {
            b"--bind-fd".to_vec()
        } else {
            b"--ro-bind-fd".to_vec()
        });
        parts.push(ALIAS_VIEW_FD.to_string().into_bytes());
        parts.push(self.path.clone());
        let mut bytes = Vec::new();
        for part in parts {
            bytes.extend(part);
            bytes.push(0);
        }
        let descriptor = rustix::fs::memfd_create(
            c"tilo-original-home-alias-args",
            rustix::fs::MemfdFlags::CLOEXEC | rustix::fs::MemfdFlags::ALLOW_SEALING,
        )?;
        let mut file = File::from(descriptor);
        file.write_all(&bytes)?;
        file.seek(SeekFrom::Start(0))?;
        rustix::fs::fcntl_add_seals(
            &file,
            rustix::fs::SealFlags::WRITE
                | rustix::fs::SealFlags::GROW
                | rustix::fs::SealFlags::SHRINK
                | rustix::fs::SealFlags::SEAL,
        )?;
        Ok(file)
    }
}
const LIMIT: usize = 16 * 1024;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

fn refused(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn digest(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn uuid(text: &str) -> bool {
    uuid::Uuid::parse_str(text).is_ok_and(|id| id.to_string() == text)
}

struct QpeWorker {
    finished: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl QpeWorker {
    fn finish(&mut self) -> io::Result<()> {
        let (lock, wake) = &*self.finished;
        if let Ok(mut done) = lock.lock() {
            *done = true;
            wake.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| io::Error::other("QPE deadline worker failed"))?;
        }
        Ok(())
    }
}
impl Drop for QpeWorker {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Absolute cancellation/deadline fence for canonical blocking QPE routines.
/// Their internal per-read timeouts may be reset; this owned socket clone still
/// shuts down the exact connection and wakes every blocked read/write. The
/// private closure is used only for the known QPE socket exchange below.
pub(crate) fn qpe_exchange<T>(
    stream: &mut UnixStream,
    cancellation: &ServiceCancellation,
    budget: Duration,
    exchange: impl FnOnce(&mut UnixStream) -> io::Result<T>,
) -> io::Result<T> {
    use std::sync::{
        Condvar, Mutex,
        atomic::{AtomicU8, Ordering},
    };
    if cancellation.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let exact = stream.try_clone()?;
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    let reason = Arc::new(AtomicU8::new(0));
    let completion = Arc::clone(&finished);
    let outcome = Arc::clone(&reason);
    let watch_cancellation = cancellation.clone();
    let deadline = std::time::Instant::now()
        .checked_add(budget)
        .ok_or(io::ErrorKind::InvalidInput)?;
    let worker = std::thread::spawn(move || {
        let (lock, wake) = &*completion;
        let Ok(mut done) = lock.lock() else {
            outcome.store(3, Ordering::Release);
            let _ = exact.shutdown(std::net::Shutdown::Both);
            return;
        };
        while !*done {
            let now = std::time::Instant::now();
            if watch_cancellation.is_cancelled() || now >= deadline {
                outcome.store(
                    if watch_cancellation.is_cancelled() {
                        1
                    } else {
                        2
                    },
                    Ordering::Release,
                );
                let _ = exact.shutdown(std::net::Shutdown::Both);
                return;
            }
            let Ok((next, _)) = wake.wait_timeout(
                done,
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(25)),
            ) else {
                outcome.store(3, Ordering::Release);
                let _ = exact.shutdown(std::net::Shutdown::Both);
                return;
            };
            done = next;
        }
    });
    let mut worker = QpeWorker {
        finished,
        thread: Some(worker),
    };
    let result = exchange(stream);
    worker.finish()?;
    if cancellation.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if std::time::Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    match reason.load(Ordering::Acquire) {
        0 => result,
        1 => Err(io::ErrorKind::Interrupted.into()),
        2 => Err(io::ErrorKind::TimedOut.into()),
        _ => Err(io::Error::other("QPE socket fence failed")),
    }
}

/// Root-selected existing certified Shell operation; not an authority token.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerChallenge {
    pub operation_id: String,
    pub shell_context_sha256: String,
    pub person: [u8; 32],
    pub resource_record: String,
    pub publisher_hex: String,
    pub command: ConsumerCommand,
}
impl ConsumerChallenge {
    fn validate(&self) -> io::Result<()> {
        if !uuid(&self.operation_id)
            || !digest(&self.shell_context_sha256)
            || self.person == [0; 32]
            || self.resource_record.is_empty()
            || self.resource_record.len() > 512
            || self.resource_record.chars().any(char::is_control)
        {
            return Err(refused("retained consumer challenge differs"));
        }
        if !digest(&self.publisher_hex) {
            return Err(refused("retained publisher invalid"));
        }
        self.command.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}
impl ConsumerCommand {
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn validate(&self) -> io::Result<()> {
        if !Path::new(&self.program).is_absolute()
            || self.args.len() > 256
            || self.env.len() > 128
            || self.program.as_bytes().contains(&0)
            || self.args.iter().any(|value| value.as_bytes().contains(&0))
            || self.env.iter().any(|(key, value)| {
                key.is_empty()
                    || key.contains('=')
                    || key.as_bytes().contains(&0)
                    || value.as_bytes().contains(&0)
            })
            || serde_json::to_vec(self).map_err(io::Error::other)?.len() > LIMIT
        {
            return Err(refused("retained normal command invalid"));
        }
        let mut keys = std::collections::BTreeSet::new();
        if self.env.iter().any(|(key, _)| !keys.insert(key)) {
            return Err(refused("retained command environment duplicated"));
        }
        Ok(())
    }
    fn matches_current(&self) -> io::Result<()> {
        self.validate()?;
        if std::env::args_os().skip(1).collect::<Vec<_>>()
            != self
                .args
                .iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
            || self.env.iter().any(|(key, value)| {
                std::env::var_os(key).as_deref() != Some(std::ffi::OsStr::new(value))
            })
        {
            return Err(refused(
                "retained normal command differs from executing launcher",
            ));
        }
        let expected = File::open(&self.program)?.metadata()?;
        let actual = File::open("/proc/self/exe")?.metadata()?;
        if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
            return Err(refused("retained executing launcher image differs"));
        }
        Ok(())
    }
}

/// Exact ordinary launch declaration, compared against the root actor's
/// independently admitted certified Shell selection and actual launcher peer.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerLaunchBinding {
    pub operation_id: String,
    pub shell_context_sha256: String,
    pub app_id: String,
    pub package_sha256: String,
    pub publisher_hex: String,
    pub command: ConsumerCommand,
    pub manifest_path: String,
    pub payload_root: String,
    pub room: String,
    pub resource_record: String,
    pub intent_sha256: String,
    pub policy_sha256: String,
    pub writable: bool,
    pub launcher_pid: u32,
    pub launcher_start: u64,
    pub uid: u32,
    pub gid: u32,
}
impl ConsumerLaunchBinding {
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn validate(&self) -> io::Result<()> {
        if !uuid(&self.operation_id)
            || !uuid(&self.room)
            || self.resource_record.is_empty()
            || self.resource_record.len() > 512
            || self.resource_record.chars().any(char::is_control)
            || !digest(&self.shell_context_sha256)
            || !digest(&self.package_sha256)
            || !digest(&self.publisher_hex)
            || !Path::new(&self.manifest_path).is_absolute()
            || !Path::new(&self.payload_root).is_absolute()
            || !digest(&self.intent_sha256)
            || !digest(&self.policy_sha256)
            || self.app_id.is_empty()
            || self.app_id.len() > 256
            || self.app_id.chars().any(char::is_control)
            || self.launcher_pid < 2
            || self.launcher_start == 0
            || self.uid == 0
        {
            return Err(refused("retained consumer binding invalid"));
        }
        self.command.validate()?;
        Ok(())
    }
    /// Canonical existing Shell proof domain, reconstructed by root after it
    /// independently replays signed GUI package, publisher and actual command.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn signed_shell_context_bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        self.command.validate()?;
        let mut bytes = b"tilo-ordinary-retained-home-context-v1\0".to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                &self.operation_id,
                &self.app_id,
                &self.publisher_hex,
                &self.package_sha256,
                &self.room,
                "retained-home",
                &self.command.program,
                &self.command.args,
                &self.command.env,
            ))
            .map_err(io::Error::other)?,
        );
        Ok(bytes)
    }
    /// Reconstruct command bytes and compare the independently certified Shell
    /// digest. Supplying a digest does not itself establish Shell authority.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn verify_shell_context(&self, certified_digest: &str) -> io::Result<()> {
        if !digest(certified_digest)
            || self.shell_context_sha256 != certified_digest
            || sha(&self.signed_shell_context_bytes()?) != certified_digest
        {
            return Err(refused("retained declared Shell context differs"));
        }
        Ok(())
    }
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn context_sha256(&self) -> io::Result<String> {
        self.validate()?;
        Ok(sha(&serde_json::to_vec(self).map_err(io::Error::other)?))
    }
    /// Declaration only. Root must compare its actual QPE peer and independently
    /// replay signed package, certified person, personal room and current policy.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn matches_peer(&self, peer: &ExecutingPeer) -> io::Result<()> {
        self.validate()?;
        if peer.pid() != self.launcher_pid
            || peer.uid() != self.uid
            || peer.gid() != self.gid
            || !peer.belongs_to(self.launcher_pid, self.launcher_start)
        {
            return Err(refused("retained actual launcher differs"));
        }
        Ok(())
    }
}

/// Observation sent exclusively by the pinned native Shell on its private gate.
/// These serializable fields have no authority without that actual Shell peer.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootHelperExpectation {
    pub pid: u32,
    pub start: u64,
    pub helper_dev: u64,
    pub helper_ino: u64,
    /// Restriction from the Shell's actual registered managed launch, not the
    /// consuming helper's command or application-supplied metadata. Ordinary
    /// launches keep the earlier wire shape; older receivers must reject the
    /// new field on protected launches instead of silently losing protection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected_model: Option<crate::registration_gate::ProtectedModelLaunch>,
}

/// Opaque admission minted only from actual pinned Shell execution and its
/// privately held root child. No public label/path/descriptor constructor.
pub struct RetainedLaunchGate {
    endpoint: UnixStream,
    shell: ExecutingPeer,
    shell_image: File,
    helper: File,
    root_pidfd: OwnedFd,
    expected: RootHelperExpectation,
}
impl RetainedLaunchGate {
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn receive(
        mut gate: UnixStream,
        endpoint: UnixStream,
        cancellation: &ServiceCancellation,
    ) -> io::Result<Self> {
        let uid = rustix::process::getuid().as_raw();
        if uid == 0 {
            return Err(refused("retained gate requires nonroot launcher"));
        }
        let (shell, shell_image) =
            qpe_exchange(&mut gate, cancellation, Duration::from_secs(10), |gate| {
                gate.set_read_timeout(Some(Duration::from_secs(10)))?;
                gate.set_write_timeout(Some(Duration::from_secs(10)))?;
                process_binding::arm(gate)?;
                let mut challenge = [0; 32];
                getrandom::SysRng
                    .try_fill_bytes(&mut challenge)
                    .map_err(io::Error::other)?;
                gate.write_all(&process_binding::MAGIC)?;
                gate.write_all(&challenge)?;
                let shell = process_binding::receive(gate, &challenge, uid)?;
                let shell_image = protected_program("/usr/bin/shell")?;
                check_executing_image(&shell, &shell_image)?;
                gate.write_all(&[1])?;
                process_binding::complete(gate)?;
                Ok((shell, shell_image))
            })?;
        let (expected, root_pidfd) = {
            let mut io = Handoff::new(&mut gate, cancellation, Duration::from_secs(10))?;
            let expected: RootHelperExpectation = receive_json_bound(&mut io, 4096)?;
            let fd = receive_fd(&mut io, 0)?;
            (expected, fd)
        };
        let result = Self {
            endpoint,
            shell,
            shell_image,
            helper: protected_helper()?,
            root_pidfd,
            expected,
        };
        result.check()?;
        // The installed Shell and its exact root child have both been checked.
        // Install before main can resolve any service, just as the stdin gate
        // does. Missing context preserves ordinary retained launches; malformed
        // context must fail rather than select the legacy model gateway.
        if let Some(context) = result.expected.protected_model.clone() {
            crate::registration_gate::install_protected_model_launch(context)?;
        }
        Ok(result)
    }
    fn check(&self) -> io::Result<()> {
        check_executing_image(&self.shell, &self.shell_image)?;
        let current = protected_program("/usr/bin/shell")?.metadata()?;
        let held = self.shell_image.metadata()?;
        if (current.dev(), current.ino()) != (held.dev(), held.ino()) {
            return Err(refused("retained native Shell image changed"));
        }
        let current = protected_helper()?.metadata()?;
        let held = self.helper.metadata()?;
        if self.expected.pid < 2
            || self.expected.start == 0
            || (held.dev(), held.ino()) != (self.expected.helper_dev, self.expected.helper_ino)
            || (current.dev(), current.ino()) != (held.dev(), held.ino())
        {
            return Err(refused("retained owned helper image differs"));
        }
        let info =
            std::fs::read_to_string(format!("/proc/self/fdinfo/{}", self.root_pidfd.as_raw_fd()))?;
        let pid = info
            .lines()
            .find_map(|line| line.strip_prefix("Pid:"))
            .and_then(|pid| pid.trim().parse::<u32>().ok());
        let mut poll = libc::pollfd {
            fd: self.root_pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if pid != Some(self.expected.pid) || unsafe { libc::poll(&raw mut poll, 1, 0) } != 0 {
            return Err(refused("retained owned root child stopped or differs"));
        }
        Ok(())
    }
}

/// Exact QPE root task matching the native Shell's actual protected child.
struct RootRole {
    peer: ExecutingPeer,
    gate: RetainedLaunchGate,
}
impl RootRole {
    fn authenticate(
        gate: RetainedLaunchGate,
        cancellation: &ServiceCancellation,
    ) -> io::Result<(Self, UnixStream)> {
        gate.check()?;
        let mut stream = gate.endpoint.try_clone()?;
        let peer = qpe_exchange(
            &mut stream,
            cancellation,
            Duration::from_secs(10),
            |stream| {
                process_binding::complete(stream)?;
                process_binding::arm(stream)?;
                let mut challenge = [0; 32];
                getrandom::SysRng
                    .try_fill_bytes(&mut challenge)
                    .map_err(io::Error::other)?;
                stream.write_all(&process_binding::MAGIC)?;
                stream.write_all(&challenge)?;
                let peer = process_binding::receive(stream, &challenge, 0)?;
                gate.check()?;
                if peer.pid() != gate.expected.pid
                    || !peer.belongs_to(gate.expected.pid, gate.expected.start)
                {
                    return Err(refused("retained root differs from owned helper"));
                }
                stream.write_all(&[1])?;
                Ok(peer)
            },
        )?;
        let role = Self { peer, gate };
        role.check()?;
        Ok((role, stream))
    }
    fn check(&self) -> io::Result<()> {
        self.gate.check()?;
        if self.peer.uid() != 0
            || self.peer.pid() != self.gate.expected.pid
            || !self
                .peer
                .belongs_to(self.gate.expected.pid, self.gate.expected.start)
        {
            return Err(refused("retained root task differs from owned helper"));
        }
        Ok(())
    }
}
fn protected_program(path: &str) -> io::Result<File> {
    for ancestor in Path::new(path).ancestors().skip(1) {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(refused("retained executable ancestry is not protected"));
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || metadata.len() == 0
    {
        return Err(refused("retained executable is not protected"));
    }
    Ok(file)
}
fn check_executing_image(peer: &ExecutingPeer, expected: &File) -> io::Result<()> {
    if !peer.current() {
        return Err(refused("retained native task stopped"));
    }
    let actual = File::open(format!("/proc/{}/exe", peer.pid()))?.metadata()?;
    let expected = expected.metadata()?;
    if (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) || !peer.current() {
        return Err(refused("retained native executable differs"));
    }
    Ok(())
}
fn protected_helper() -> io::Result<File> {
    for path in ["/", "/usr", "/usr/libexec"] {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(refused("retained helper ancestry is not protected"));
        }
    }
    let helper = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(HELPER_PATH)?;
    let metadata = helper.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || metadata.len() == 0
    {
        return Err(refused("retained installed helper is not protected"));
    }
    Ok(helper)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewReceipt {
    schema: u32,
    binding: ConsumerLaunchBinding,
    context_sha256: String,
    root_pid: u32,
    view_dev: u64,
    view_ino: u64,
    mount_id: u64,
    original_home_alias: OriginalHomeAlias,
}

/// Inseparable root role, held view and live root lease. No public path/FD grant
/// constructor exists; only the authenticated installed helper can issue it.
pub struct RetainedViewAdmission {
    role: RootRole,
    view: File,
    lease: UnixStream,
    binding: ConsumerLaunchBinding,
    manifest_path: PathBuf,
    payload_root: PathBuf,
    manifest: crate::manifest::ManifestView,
    opts: RunOptions,
    original_home_alias: OriginalHomeAlias,
    alias_args: std::sync::Mutex<Option<File>>,
}
impl RetainedViewAdmission {
    /// Receive only on the private endpoint inherited through Shell opcode 2.
    /// The normal signed GUI package is replayed before any root view is taken.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn receive(
        gate: RetainedLaunchGate,
        manifest_path: &Path,
        payload_root: &Path,
        opts: &RunOptions,
        cancellation: &ServiceCancellation,
    ) -> Result<Arc<Self>, RunError> {
        if !matches!(opts.mode, crate::run::LaunchMode::Gui)
            || !opts.landlock_enforce
            || opts.landlock_best_effort
            || opts.allow_unsigned_runtime
            || opts.sandbox_shim_bin.is_some()
            || opts
                .bwrap_bin
                .as_deref()
                .is_some_and(|path| path != Path::new("/usr/bin/bwrap"))
            || (rustix::process::getuid().as_raw() == 0 || rustix::process::geteuid().as_raw() == 0)
        {
            return Err(RunError::InvalidServiceLaunch(
                "retained view requires ordinary confined non-root GUI".into(),
            ));
        }
        let _bwrap = protected_program("/usr/bin/bwrap")?;
        let package = opts.expected_package_hash.as_deref().ok_or_else(|| {
            RunError::InvalidServiceLaunch("retained view requires signed package pin".into())
        })?;
        let manifest =
            crate::run::verified_original_manifest(manifest_path, payload_root, opts, package)?;
        if manifest.kind != "app" || manifest.entrypoint.gui_bin().is_none() {
            return Err(RunError::InvalidServiceLaunch(
                "retained view requires signed GUI app".into(),
            ));
        }
        let (role, mut stream) = RootRole::authenticate(gate, cancellation)?;
        let (binding, view, original_home_alias) = {
            let mut io = Handoff::new(&mut stream, cancellation, Duration::from_secs(30))?;
            let challenge: ConsumerChallenge = receive_json(&mut io)?;
            challenge.validate()?;
            challenge.command.matches_current()?;
            let room = collab_types::person_room(&challenge.person).to_string();
            if opts.workspace_id.as_deref() != Some(room.as_str()) {
                return Err(RunError::InvalidServiceLaunch(
                    "retained view requires certified personal room".into(),
                ));
            }
            if normal_publisher(manifest_path, payload_root, opts)? != challenge.publisher_hex {
                return Err(refused("retained signed normal publisher differs").into());
            }
            let binding = binding_for(&manifest, opts, &challenge, manifest_path, payload_root)?;
            binding.verify_shell_context(&challenge.shell_context_sha256)?;

            send_json(&mut io, &binding)?;
            let receipt = receive_fd(&mut io, 0)?;
            let receipt: ViewReceipt = read_receipt(&receipt)?;
            if receipt.schema != 2
                || receipt.binding != binding
                || receipt.context_sha256 != binding.context_sha256()?
                || receipt.root_pid != role.peer.pid()
            {
                return Err(refused("retained root receipt launch binding differs").into());
            }
            let view = File::from(receive_fd(&mut io, 1)?);
            check_view(&view, &receipt)?;
            role.check()?;
            receipt.original_home_alias.validate()?;
            (binding, view, receipt.original_home_alias)
        };
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        let admission = Arc::new(Self {
            role,
            view,
            lease: stream,
            binding,
            manifest_path: manifest_path.into(),
            payload_root: payload_root.into(),
            manifest,
            opts: opts.clone(),
            original_home_alias,
            alias_args: std::sync::Mutex::new(None),
        });
        admission.check()?;
        Ok(admission)
    }
    #[must_use]
    pub fn binding(&self) -> &ConsumerLaunchBinding {
        &self.binding
    }
    pub(crate) fn descriptor_pairs(&self) -> io::Result<Vec<(RawFd, RawFd)>> {
        self.check()?;
        let arguments_fd = {
            let arguments = self
                .alias_args
                .lock()
                .map_err(|_| refused("alias argument custody poisoned"))?;
            arguments
                .as_ref()
                .ok_or_else(|| refused("alias arguments not prepared"))?
                .as_raw_fd()
        };
        Ok(vec![
            (self.view.as_raw_fd(), VIEW_FD),
            (self.view.as_raw_fd(), ALIAS_VIEW_FD),
            (arguments_fd, ALIAS_ARGS_FD),
        ])
    }
    pub(crate) fn splice(
        &self,
        argv: &mut Vec<String>,
        participation: Participation,
    ) -> io::Result<()> {
        self.check()?;
        if self.binding.writable && participation != Participation::Author {
            return Err(refused("retained write participation retired"));
        }
        let entry = argv
            .iter()
            .position(|arg| arg == "--")
            .ok_or_else(|| refused("retained view bwrap entrypoint unavailable"))?;
        let arguments = self.original_home_alias.args(
            argv.get(..entry).ok_or(io::ErrorKind::InvalidInput)?,
            self.binding.writable,
        )?;
        let mut held_args = self
            .alias_args
            .lock()
            .map_err(|_| refused("alias argument custody poisoned"))?;
        if held_args.is_some() {
            return Err(refused("alias argv already prepared"));
        }
        *held_args = Some(arguments);
        drop(held_args);
        argv.splice(entry..entry, ["--args".into(), ALIAS_ARGS_FD.to_string()]);
        argv.splice(
            entry..entry,
            [
                if self.binding.writable {
                    "--bind-fd"
                } else {
                    "--ro-bind-fd"
                }
                .into(),
                VIEW_FD.to_string(),
                SANDBOX_HOME.into(),
            ],
        );
        let position = argv
            .iter()
            .position(|arg| arg == crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND)
            .ok_or_else(|| refused("retained view Landlock shim unavailable"))?;
        argv.splice(
            position.saturating_add(1)..position.saturating_add(1),
            [
                if self.binding.writable {
                    "--rw-noexec"
                } else {
                    "--ro-noexec"
                }
                .into(),
                SANDBOX_HOME.into(),
            ],
        );
        Ok(())
    }
    pub(crate) fn namespace_owner(
        self: &Arc<Self>,
        cancellation: ServiceCancellation,
    ) -> io::Result<OrdinaryNamespaceOwner> {
        let connected = ConnectedNamespace::new(self.lease.try_clone()?);
        OrdinaryNamespaceOwner::with_retained(self.clone(), cancellation, connected, self.clone())
    }
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn check(&self) -> io::Result<()> {
        self.role.check()?;
        let mut byte = [0];
        let count = unsafe {
            libc::recv(
                self.lease.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if count >= 0 {
            return Err(refused("retained root lease stopped or changed"));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let manifest = &self.manifest;
        if manifest.package_id != self.binding.app_id
            || policy_hash(&self.opts)? != self.binding.policy_sha256
            || self.binding.writable
                && crate::run::resolve_capabilities(manifest, &self.opts).participation
                    != Participation::Author
        {
            return Err(refused("retained ordinary policy changed"));
        }
        Ok(())
    }
}
impl ServiceAuthority for RetainedViewAdmission {
    fn check(&self) -> io::Result<()> {
        RetainedViewAdmission::check(self)
    }
}

fn binding_for(
    manifest: &crate::manifest::ManifestView,
    opts: &RunOptions,
    challenge: &ConsumerChallenge,
    manifest_path: &Path,
    payload_root: &Path,
) -> io::Result<ConsumerLaunchBinding> {
    let intent = serde_json::to_vec(&(
        opts.open_document
            .as_ref()
            .map(|p| p.as_os_str().as_bytes().to_vec()),
        &opts.open_uri,
    ))
    .map_err(io::Error::other)?;
    let pid = std::process::id();
    let process_stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let start = process_stat
        .rsplit_once(") ")
        .and_then(|(_, fields)| fields.split_whitespace().nth(19))
        .and_then(|value| value.parse().ok())
        .ok_or(io::ErrorKind::InvalidData)?;
    let binding = ConsumerLaunchBinding {
        operation_id: challenge.operation_id.clone(),
        shell_context_sha256: challenge.shell_context_sha256.clone(),
        app_id: manifest.package_id.clone(),
        package_sha256: opts
            .expected_package_hash
            .clone()
            .ok_or(io::ErrorKind::InvalidData)?,
        resource_record: challenge.resource_record.clone(),
        publisher_hex: challenge.publisher_hex.clone(),
        command: challenge.command.clone(),
        manifest_path: manifest_path
            .to_str()
            .ok_or(io::ErrorKind::InvalidData)?
            .into(),
        payload_root: payload_root
            .to_str()
            .ok_or(io::ErrorKind::InvalidData)?
            .into(),
        room: opts
            .workspace_id
            .clone()
            .ok_or(io::ErrorKind::InvalidData)?,
        intent_sha256: sha(&intent),
        policy_sha256: policy_hash(opts)?,
        writable: crate::run::resolve_capabilities(manifest, opts).participation
            == Participation::Author,
        launcher_pid: pid,
        launcher_start: start,
        uid: rustix::process::getuid().as_raw(),
        gid: rustix::process::getgid().as_raw(),
    };
    binding.validate()?;
    Ok(binding)
}
fn policy_hash(opts: &RunOptions) -> io::Result<String> {
    let path = opts
        .capabilities_path
        .as_deref()
        .ok_or_else(|| refused("retained ordinary policy missing"))?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > LIMIT {
        return Err(refused("retained ordinary policy bound exceeded"));
    }
    let _: Vec<collab_types::caps::CapOverride> =
        serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    Ok(sha(&bytes))
}
fn check_view(view: &File, receipt: &ViewReceipt) -> io::Result<()> {
    let metadata = view.metadata()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(view.as_raw_fd(), &raw mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if !metadata.is_dir()
        || metadata.uid() != receipt.binding.uid
        || metadata.gid() != receipt.binding.gid
        || stat.f_type != 0x6573_5546
        || (metadata.dev(), metadata.ino()) != (receipt.view_dev, receipt.view_ino)
        || mount_id(view)? != receipt.mount_id
    {
        return Err(refused("retained view is not exact admitted FUSE root"));
    }
    Ok(())
}
fn mount_id(view: &File) -> io::Result<u64> {
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            view.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_STATX_DONT_SYNC,
            libc::STATX_MNT_ID,
            &raw mut stat,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 {
        return Err(refused("retained FUSE mount identity unavailable"));
    }
    Ok(stat.stx_mnt_id)
}
fn send_json<T: serde::Serialize>(io: &mut Handoff<'_>, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.is_empty() || bytes.len() > LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    io.write_all(
        &u32::try_from(bytes.len())
            .map_err(io::Error::other)?
            .to_be_bytes(),
    )?;
    io.write_all(&bytes)
}
fn receive_json<T: serde::de::DeserializeOwned>(io: &mut Handoff<'_>) -> io::Result<T> {
    receive_json_bound(io, LIMIT)
}
fn receive_json_bound<T: serde::de::DeserializeOwned>(
    io: &mut Handoff<'_>,
    limit: usize,
) -> io::Result<T> {
    let mut length = [0; 4];
    io.read_exact(&mut length)?;
    let length = usize::try_from(u32::from_be_bytes(length)).map_err(io::Error::other)?;
    if length == 0 || length > limit {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; length];
    io.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
fn receive_fd(io: &mut Handoff<'_>, index: u8) -> io::Result<std::os::fd::OwnedFd> {
    let (bytes, fd) = io.retry(libc::POLLIN, |stream| {
        runtime_portal::fd_transport::recv_with_fd(stream.as_fd())
    })?;
    if bytes != [index] {
        return Err(refused("retained view descriptor order differs"));
    }
    let fd = fd.ok_or_else(|| refused("retained view descriptor missing"))?;
    io.write_all(&[1])?;
    Ok(fd)
}
fn read_receipt(file: &std::os::fd::OwnedFd) -> io::Result<ViewReceipt> {
    let file = File::from(file.try_clone()?);
    let metadata = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let name = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.len() == 0
        || metadata.len() > LIMIT as u64
        || seals < 0
        || seals & SEALS != SEALS
        || !name
            .as_os_str()
            .as_bytes()
            .starts_with(b"/memfd:tilo-retained-view-receipt")
    {
        return Err(refused("retained view lacks sealed root receipt"));
    }
    let mut bytes = vec![0; usize::try_from(metadata.len()).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IssueProgress {
    NoView,
    PossibleView,
}

/// Failure evidence minted only by the canonical issue path. A receipt or
/// other metadata exchange is never evidence that the view was exported.
/// No public constructor or wire representation exists.
#[derive(Debug)]
pub struct FailedViewIssue {
    error: io::Error,
    progress: IssueProgress,
    stream: UnixStream,
}
impl FailedViewIssue {
    #[must_use]
    pub fn view_may_have_been_delivered(&self) -> bool {
        self.progress == IssueProgress::PossibleView
    }
    #[must_use]
    pub fn into_parts(self) -> (io::Error, UnixStream) {
        (self.error, self.stream)
    }
}
impl std::fmt::Display for FailedViewIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::error::Error for FailedViewIssue {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

fn send_issue_fd(
    io: &mut Handoff<'_>,
    index: u8,
    fd: std::os::fd::BorrowedFd<'_>,
    progress: &mut IssueProgress,
) -> io::Result<()> {
    if index > 1 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    io.retry(libc::POLLOUT, |stream| {
        // Record possible delivery immediately BEFORE the actual FD1 syscall.
        // Receipt-only transport, cancellation and timeout before this point
        // positively establish that this path never attempted a view transfer.
        if index == 1 {
            *progress = IssueProgress::PossibleView;
        }
        runtime_portal::fd_transport::send_with_fd(stream.as_fd(), &[index], fd)
    })
}

/// Fixed helper's transport only. Root must bind the returned declaration to
/// its independently admitted certified Shell/person/source before `issue()`.
/// This type contains no person or source authority and grants no view itself.
pub struct RootConsumerChannel {
    stream: UnixStream,
    peer: Arc<ExecutingPeer>,
}
impl RootConsumerChannel {
    /// Authenticate the actual non-root launcher, then prove this exact fixed
    /// installed root helper back to it. Role and source admission remain distinct.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn authenticate(
        mut stream: UnixStream,
        cancellation: &ServiceCancellation,
    ) -> io::Result<Self> {
        own_helper()?;
        if cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let peer = qpe_exchange(
            &mut stream,
            cancellation,
            Duration::from_secs(10),
            |stream| {
                stream.set_read_timeout(Some(Duration::from_secs(10)))?;
                stream.set_write_timeout(Some(Duration::from_secs(10)))?;
                process_binding::arm(stream)?;
                let mut challenge = [0; 32];
                getrandom::SysRng
                    .try_fill_bytes(&mut challenge)
                    .map_err(io::Error::other)?;
                stream.write_all(&process_binding::MAGIC)?;
                stream.write_all(&challenge)?;
                let peer = process_binding::receive_nonroot_for_authentication(stream, &challenge)?;
                stream.write_all(&[1])?;
                process_binding::complete(stream)?;
                Ok(peer)
            },
        )?;
        own_helper()?;
        if cancellation.is_cancelled() || !peer.current() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(Self {
            stream,
            peer: Arc::new(peer),
        })
    }
    /// Fixed helper checks before connecting its inherited rendezvous. This
    /// observes current executable custody and constructs no authority token.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn verify_current_helper() -> io::Result<()> {
        own_helper()
    }
    #[must_use]
    pub fn peer(&self) -> &ExecutingPeer {
        &self.peer
    }
    /// Shares this already authenticated kernel task; no wire PID constructor.
    #[must_use]
    pub fn retain_peer(&self) -> Arc<ExecutingPeer> {
        Arc::clone(&self.peer)
    }
    /// Return a declaration for private installed actor admission; never issue a
    /// view merely because it matches the executing task or this challenge.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn request(
        &mut self,
        challenge: &ConsumerChallenge,
        cancellation: &ServiceCancellation,
    ) -> io::Result<ConsumerLaunchBinding> {
        own_helper()?;
        challenge.validate()?;
        let mut io = Handoff::new(&mut self.stream, cancellation, Duration::from_secs(30))?;
        send_json(&mut io, challenge)?;
        let binding: ConsumerLaunchBinding = receive_json(&mut io)?;
        binding.verify_peer_program(&self.peer)?;
        binding.verify_shell_context(&challenge.shell_context_sha256)?;
        if binding.command != challenge.command || binding.publisher_hex != challenge.publisher_hex
        {
            return Err(refused("retained normal selection differs"));
        }
        if binding.operation_id != challenge.operation_id
            || binding.shell_context_sha256 != challenge.shell_context_sha256
            || binding.resource_record != challenge.resource_record
            || binding.room != collab_types::person_room(&challenge.person).to_string()
        {
            return Err(refused(
                "retained consumer declaration differs from protected challenge",
            ));
        }
        Ok(binding)
    }
    /// Call ONLY after the private root actor has admitted the exact consumer,
    /// current certified Shell/person, signed app, policy and source. The caller
    /// retains its entire joint projection owner until the returned stream's
    /// actual `NamespaceWitness` proves the consuming namespace stopped.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn issue(
        mut self,
        grant: ReviewedViewIssue<'_>,
        binding: &ConsumerLaunchBinding,
        cancellation: &ServiceCancellation,
    ) -> Result<(UnixStream, Arc<ExecutingPeer>), FailedViewIssue> {
        let mut progress = IssueProgress::NoView;
        let result = (|| {
            own_helper()?;
            binding.verify_peer_program(&self.peer)?;
            if grant.uid != binding.uid
                || grant.gid != binding.gid
                || grant.context_sha256 != binding.context_sha256()?
                || grant.mode != RetainedViewMode::Home
            {
                return Err(refused("retained reviewed target projection differs"));
            }
            grant.original_home_alias.validate()?;
            let observation = grant.observation.ok_or_else(|| {
                refused("Retained Home issue requires actual dropped-worker observation")
            })?;
            observation.check()?;
            if hex::encode(observation.context_sha256()?) != grant.context_sha256
                || grant.context_sha256 != binding.context_sha256()?
                || grant.expected_source_view_sha256 == [0; 32]
                || observation.source_view_sha256()? != grant.expected_source_view_sha256
            {
                return Err(refused(
                    "Retained observed endpoint launch/source binding differs",
                ));
            }
            if observation.file()?.as_raw_fd() != grant.view.as_raw_fd()
                || !observation.is_directory()?
                || (observation.uid()?, observation.gid()?) != (binding.uid, binding.gid)
                || observation.filesystem_type()? != 0x6573_5546
            {
                return Err(refused("Retained Home observed target differs"));
            }
            let exposure = grant
                .exposure
                .ok_or_else(|| refused("Retained issue requires placed clone custody"))?;
            exposure.matches(observation)?;

            let receipt = ViewReceipt {
                schema: 2,
                binding: binding.clone(),
                context_sha256: binding.context_sha256()?,
                root_pid: std::process::id(),
                view_dev: observation.dev()?,
                view_ino: observation.ino()?,
                mount_id: exposure.mount_id()?,
                original_home_alias: grant.original_home_alias.clone(),
            };
            own_helper()?;
            binding.verify_peer_program(&self.peer)?;
            observation.check()?;
            let sealed = seal_receipt(&receipt)?;
            exposure.mark_possible_delivery()?;
            let view = exposure.file()?;
            {
                let mut io = Handoff::new(&mut self.stream, cancellation, Duration::from_secs(30))?;
                for (index, fd) in [sealed.as_fd(), view.as_fd()].into_iter().enumerate() {
                    own_helper()?;
                    binding.verify_peer_program(&self.peer)?;
                    observation.check()?;
                    exposure.check()?;
                    send_issue_fd(
                        &mut io,
                        u8::try_from(index).map_err(io::Error::other)?,
                        fd,
                        &mut progress,
                    )?;
                    let mut ack = [0];
                    io.read_exact(&mut ack)?;
                    if ack != [1] {
                        return Err(refused("retained consumer view handoff refused"));
                    }
                }
            }
            own_helper()?;
            binding.matches_peer(&self.peer)?;
            own_helper()?;
            self.stream.set_read_timeout(None)?;
            self.stream.set_write_timeout(None)?;
            Ok(())
        })();
        match result {
            Ok(()) => Ok((self.stream, self.peer)),
            Err(error) => Err(FailedViewIssue {
                error,
                progress,
                stream: self.stream,
            }),
        }
    }
}
fn own_helper() -> io::Result<()> {
    if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
        return Err(refused("retained view issuer is not root"));
    }
    let expected = protected_helper()?.metadata()?;
    let actual = File::open("/proc/self/exe")?.metadata()?;
    if (actual.dev(), actual.ino()) != (expected.dev(), expected.ino()) {
        return Err(refused("retained view issuer is not installed helper"));
    }
    Ok(())
}
fn seal_receipt(receipt: &ViewReceipt) -> io::Result<File> {
    let bytes = serde_json::to_vec(receipt).map_err(io::Error::other)?;
    if bytes.is_empty() || bytes.len() > LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-retained-view-receipt".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(&bytes)?;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

/// Root actor's already reviewed projection observation. This is not an
/// admission constructor: only `RootConsumerChannel` inside the exact protected
/// installed helper can issue bytes, after its private actor's actual review.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RetainedViewMode {
    Home,
}
pub struct ReviewedViewIssue<'a> {
    pub exposure: Option<&'a mut crate::owned_service::target_projection::HeldViewExposure>,
    pub original_home_alias: &'a OriginalHomeAlias,
    pub expected_source_view_sha256: [u8; 32],
    pub observation: Option<&'a crate::owned_service::target_projection::ObservedTargetObject<'a>>,
    pub view: &'a File,
    pub uid: u32,
    pub gid: u32,
    pub mode: RetainedViewMode,
    pub context_sha256: String,
}
fn normal_publisher(
    manifest_path: &Path,
    payload_root: &Path,
    opts: &RunOptions,
) -> io::Result<String> {
    let app = opts.app_id.as_deref().ok_or(io::ErrorKind::InvalidData)?;
    let expected = opts
        .expected_package_hash
        .as_deref()
        .ok_or(io::ErrorKind::InvalidData)?;
    let installed = package_store::find_installed(app)
        .map_err(io::Error::other)?
        .ok_or(io::ErrorKind::NotFound)?;
    if installed.manifest_path != manifest_path
        || installed.payload_root != payload_root
        || installed.package_id_hex != expected
    {
        return Err(refused("retained signed installation changed"));
    }
    let record = package_store::integrity::read_record(
        &package_store::integrity::record_dir(&installed),
        app,
        expected,
    )
    .map_err(io::Error::other)?;
    Ok(hex::encode(record.root.publisher_identity))
}

/// Continue the ordinary verified GUI launch with its inseparable root lease
/// and canonical namespace owner. No service-mode policy is substituted.
/// # Errors
/// Refuses missing, stale or incompatible kernel or signed launch evidence.
pub fn run_admitted_gui(
    admission: Arc<RetainedViewAdmission>,
    cancellation: ServiceCancellation,
) -> Result<std::process::ExitStatus, RunError> {
    let owner = admission.namespace_owner(cancellation)?;
    let result = crate::run::run_supervised_gui(
        &admission.manifest_path,
        &admission.payload_root,
        &admission.opts,
        &owner,
    );
    drop(admission);
    result
}

impl ConsumerLaunchBinding {
    /// Cryptographic target installation replay, not person/source admission.
    /// Root must first bind these declared paths to the certified Shell's actual
    /// ordinary selection and its canonical target installation. Never use a
    /// root-ambient app catalog in place of that independent path admission.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn verify_signed_gui_package(&self) -> io::Result<[u8; 32]> {
        self.validate()?;
        let row: package_store::InstalledPackage = serde_json::from_value(serde_json::json!({
            "id": self.app_id, "package_id_hex": self.package_sha256,
            "manifest_path": self.manifest_path, "payload_root": self.payload_root,
        }))
        .map_err(io::Error::other)?;
        let report =
            package_store::integrity::verify(&row, &row.payload_root).map_err(io::Error::other)?;
        if !report.is_intact() {
            return Err(refused("retained declared signed package is not intact"));
        }
        let record = package_store::integrity::read_record(
            &package_store::integrity::record_dir(&row),
            &row.id,
            &row.package_id_hex,
        )
        .map_err(io::Error::other)?;
        let manifest: crate::manifest::ManifestView =
            toml::from_str(std::str::from_utf8(&record.manifest_bytes).map_err(io::Error::other)?)
                .map_err(io::Error::other)?;
        let publisher = hex::encode(record.root.publisher_identity);
        if manifest.package_id != self.app_id
            || manifest.kind != "app"
            || manifest.entrypoint.gui_bin().is_none()
            || publisher != self.publisher_hex
        {
            return Err(refused(
                "retained declared GUI package or publisher differs",
            ));
        }
        Ok(record.root.publisher_identity)
    }
}

impl ConsumerLaunchBinding {
    /// Root-only replay of the actual executing launcher image and frozen command.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn verify_peer_program(&self, peer: &ExecutingPeer) -> io::Result<()> {
        self.matches_peer(peer)?;
        if self.command.program != "/usr/bin/tilo-launcher" {
            return Err(refused("retained command is not fixed installed launcher"));
        }
        check_executing_image(peer, &protected_program("/usr/bin/tilo-launcher")?)?;
        let mut actual = Vec::new();
        File::open(format!("/proc/{}/cmdline", peer.pid()))?
            .take((LIMIT + 1) as u64)
            .read_to_end(&mut actual)?;
        let arguments: Vec<_> = actual
            .strip_suffix(&[0])
            .ok_or_else(|| refused("retained executing argv terminator missing"))?
            .split(|byte| *byte == 0)
            .collect();
        if actual.len() > LIMIT
            || arguments.get(1..)
                != Some(
                    self.command
                        .args
                        .iter()
                        .map(String::as_bytes)
                        .collect::<Vec<_>>()
                        .as_slice(),
                )
        {
            return Err(refused("retained executing launcher command differs"));
        }
        let mut environment = Vec::new();
        File::open(format!("/proc/{}/environ", peer.pid()))?
            .take((256 * 1024 + 1) as u64)
            .read_to_end(&mut environment)?;
        if environment.len() > 256 * 1024
            || self.command.env.iter().any(|(key, value)| {
                let expected = format!("{key}={value}");
                !environment
                    .split(|byte| *byte == 0)
                    .any(|entry| entry == expected.as_bytes())
            })
        {
            return Err(refused("retained executing launcher environment differs"));
        }
        self.matches_peer(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actual_current_peer() -> ExecutingPeer {
        let (mut receiver, mut sender) = UnixStream::pair().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        receiver
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let worker = std::thread::spawn(move || process_binding::complete(&mut sender).unwrap());
        process_binding::arm(&receiver).unwrap();
        let challenge = [73; 32];
        receiver.write_all(&process_binding::MAGIC).unwrap();
        receiver.write_all(&challenge).unwrap();
        let peer =
            process_binding::receive(&receiver, &challenge, rustix::process::geteuid().as_raw())
                .unwrap();
        receiver.write_all(&[1]).unwrap();
        worker.join().unwrap();
        assert_eq!(peer.pid(), std::process::id());
        assert!(peer.current());
        peer
    }

    #[test]
    fn canonical_pre_view_role_refusal_returns_the_actual_live_lease() {
        // Genuine kernel task and actual socket; this unqualified test image
        // must never gain installed root-helper authority or export any view.
        let peer = Arc::new(actual_current_peer());
        let (stream, mut remote) = UnixStream::pair().unwrap();
        let original = stream.as_raw_fd();
        let channel = RootConsumerChannel { stream, peer };
        let declaration = ConsumerLaunchBinding {
            operation_id: String::new(),
            shell_context_sha256: String::new(),
            app_id: String::new(),
            package_sha256: String::new(),
            publisher_hex: String::new(),
            command: ConsumerCommand {
                program: String::new(),
                args: vec![],
                env: vec![],
            },
            manifest_path: String::new(),
            payload_root: String::new(),
            room: String::new(),
            resource_record: String::new(),
            intent_sha256: String::new(),
            policy_sha256: String::new(),
            writable: false,
            launcher_pid: 0,
            launcher_start: 0,
            uid: 0,
            gid: 0,
        };
        let candidate = File::open("/dev/null").unwrap();
        let alias_source = File::open("/").unwrap();
        let alias = OriginalHomeAlias::capture(Path::new("/home/fixture"), &alias_source).unwrap();
        let failure = channel
            .issue(
                ReviewedViewIssue {
                    exposure: None,
                    original_home_alias: &alias,
                    expected_source_view_sha256: [0; 32],
                    observation: None,
                    view: &candidate,
                    uid: 0,
                    gid: 0,
                    mode: RetainedViewMode::Home,
                    context_sha256: String::new(),
                },
                &declaration,
                &ServiceCancellation::default(),
            )
            .err()
            .expect("unqualified root-helper image must be refused");
        assert!(!failure.view_may_have_been_delivered());
        let (_error, mut retained) = failure.into_parts();
        assert_eq!(retained.as_raw_fd(), original);
        retained
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        remote.write_all(&[37]).unwrap();
        let mut byte = [0];
        retained.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [37]);
    }

    #[test]
    fn actual_receipt_fd_transport_does_not_mark_the_view_exported() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let held = File::open("/dev/null").unwrap();
        let cancellation = ServiceCancellation::default();
        let mut progress = IssueProgress::NoView;
        let mut io = Handoff::new(&mut sender, &cancellation, Duration::from_secs(1)).unwrap();
        send_issue_fd(&mut io, 0, held.as_fd(), &mut progress).unwrap();
        let (tag, descriptor) =
            runtime_portal::fd_transport::recv_with_fd(receiver.as_fd()).unwrap();
        assert_eq!(tag, [0]);
        let descriptor = File::from(descriptor.unwrap());
        assert_eq!(
            (
                descriptor.metadata().unwrap().dev(),
                descriptor.metadata().unwrap().ino()
            ),
            (
                held.metadata().unwrap().dev(),
                held.metadata().unwrap().ino()
            )
        );
        assert_eq!(progress, IssueProgress::NoView);
        send_issue_fd(&mut io, 1, held.as_fd(), &mut progress).unwrap();
        let (tag, descriptor) =
            runtime_portal::fd_transport::recv_with_fd(receiver.as_fd()).unwrap();
        assert_eq!(tag, [1]);
        assert!(descriptor.is_some());
        assert_eq!(progress, IssueProgress::PossibleView);
    }
    #[test]
    fn failed_actual_view_fd_attempt_preserves_possible_delivery_evidence() {
        let held = File::open("/dev/null").unwrap();
        for (index, expected) in [(0, IssueProgress::NoView), (1, IssueProgress::PossibleView)] {
            let (mut sender, receiver) = UnixStream::pair().unwrap();
            drop(receiver);
            let cancellation = ServiceCancellation::default();
            let mut progress = IssueProgress::NoView;
            let mut io = Handoff::new(&mut sender, &cancellation, Duration::from_secs(1)).unwrap();
            assert!(send_issue_fd(&mut io, index, held.as_fd(), &mut progress).is_err());
            assert_eq!(progress, expected);
        }
    }
    #[test]
    fn cancelled_before_view_send_never_marks_delivery_possible() {
        let (mut sender, _receiver) = UnixStream::pair().unwrap();
        let held = File::open("/dev/null").unwrap();
        let cancellation = ServiceCancellation::default();
        cancellation.cancel();
        let mut progress = IssueProgress::NoView;
        let mut io = Handoff::new(&mut sender, &cancellation, Duration::from_secs(1)).unwrap();
        assert_eq!(
            send_issue_fd(&mut io, 1, held.as_fd(), &mut progress)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(progress, IssueProgress::NoView);
    }

    #[test]
    fn stalled_initial_qpe_is_cancelled_on_the_exact_connection() {
        let (mut stream, _stalled) = UnixStream::pair().unwrap();
        let cancellation = ServiceCancellation::default();
        let trigger = cancellation.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            trigger.cancel();
        });
        let began = std::time::Instant::now();
        let error = qpe_exchange(
            &mut stream,
            &cancellation,
            Duration::from_secs(5),
            process_binding::complete,
        )
        .unwrap_err();
        worker.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(began.elapsed() < Duration::from_secs(2));
        assert_eq!(
            stream.read(&mut [0]).unwrap(),
            0,
            "cancelled exact socket must be closed"
        );
    }
    #[test]
    fn stalled_initial_qpe_uses_one_total_deadline() {
        let (mut stream, _stalled) = UnixStream::pair().unwrap();
        let began = std::time::Instant::now();
        let error = qpe_exchange(
            &mut stream,
            &ServiceCancellation::default(),
            Duration::from_millis(60),
            process_binding::complete,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(began.elapsed() < Duration::from_secs(2));
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn actual_authenticated_task_cannot_claim_another_protected_executable() {
        let peer = actual_current_peer();
        let expected = protected_program("/usr/bin/true").unwrap();
        let error = check_executing_image(&peer, &expected).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(peer.current(), "refusal must not signal an unadmitted task");
    }

    struct ContextFixtureChild(Option<std::process::Child>);
    impl Drop for ContextFixtureChild {
        fn drop(&mut self) {
            let Some(mut child) = self.0.take() else {
                return;
            };
            // Child Drop does not stop an owned task. Cover observation errors
            // and every unwind; never claim stopped after a failed wait.
            loop {
                let _ = child.kill();
                match child.wait() {
                    Ok(_) => return,
                    Err(error) => {
                        eprintln!("context fixture stop remains unobserved: {error}");
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        }
    }

    fn run_context_fixture_as_original_user() {
        use std::os::unix::process::CommandExt;
        const EXEC_FD: i32 = 198;
        // Hold the actual test image, including deleted-memfd qualification
        // images; no checkout ancestor permissions or executable discovery.
        let image = File::open("/proc/self/exe").unwrap();
        let image_fd = image.as_raw_fd();
        let mut command = std::process::Command::new("/proc/self/fd/198");
        command.args([
            "retained_view::tests::actual_launcher_peer_cannot_replay_a_different_launch_context",
            "--exact",
            "--nocapture",
        ]);
        command.env("TILO_RETAINED_CONTEXT_UNPRIVILEGED_TEST", "1");
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(image_fd, EXEC_FD) == -1
                    || libc::fcntl(EXEC_FD, libc::F_SETFD, 0) == -1
                    || libc::setgroups(0, std::ptr::null()) == -1
                    || libc::setgid(1005) == -1
                    || libc::setuid(1004) == -1
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = ContextFixtureChild(Some(command.spawn().unwrap()));
        let started = std::time::Instant::now();
        loop {
            if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                child.0.take(); // try_wait observed and reaped this exact child.
                assert!(status.success(), "unprivileged context fixture: {status}");
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "owned unprivileged context fixture timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn actual_launcher_peer_cannot_replay_a_different_launch_context() {
        let child = std::env::var_os("TILO_RETAINED_CONTEXT_UNPRIVILEGED_TEST").is_some();
        if rustix::process::geteuid().as_raw() == 0 {
            assert!(!child, "fixture failed to drop actual credentials");
            run_context_fixture_as_original_user();
            return;
        }
        if child {
            assert_eq!(rustix::process::getuid().as_raw(), 1004);
            assert_eq!(rustix::process::geteuid().as_raw(), 1004);
            assert_eq!(rustix::process::getgid().as_raw(), 1005);
            assert_eq!(rustix::process::getegid().as_raw(), 1005);
            assert_eq!(unsafe { libc::getgroups(0, std::ptr::null_mut()) }, 0);
        }
        let peer = actual_current_peer();
        let process_stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let start = process_stat
            .rsplit_once(") ")
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap()
            .parse()
            .unwrap();
        let mut binding = ConsumerLaunchBinding {
            operation_id: "00000000-0000-4000-8000-000000000001".into(),
            shell_context_sha256: "11".repeat(32),
            app_id: "org.tilo.Files".into(),
            package_sha256: "22".repeat(32),
            publisher_hex: "33".repeat(32),
            command: ConsumerCommand {
                program: "/usr/bin/tilo-launcher".into(),
                args: vec!["run".into()],
                env: vec![],
            },
            manifest_path: "/var/lib/tilo/apps/files/app.toml".into(),
            payload_root: "/var/lib/tilo/apps/files/payload".into(),
            room: "00000000-0000-4000-8000-000000000002".into(),
            resource_record: "retained-home".into(),
            intent_sha256: "44".repeat(32),
            policy_sha256: "55".repeat(32),
            writable: false,
            launcher_pid: std::process::id(),
            launcher_start: start,
            uid: rustix::process::getuid().as_raw(),
            gid: rustix::process::getgid().as_raw(),
        };
        // Neither wire declaration is admitted. Exercise canonical production
        // replay bytes with a genuine kernel peer, without inventing root rights.
        let certified_digest = sha(&binding.signed_shell_context_bytes().unwrap());
        binding.shell_context_sha256 = certified_digest.clone();
        binding.verify_shell_context(&certified_digest).unwrap();
        binding.matches_peer(&peer).unwrap();
        let mut root_declaration = binding.clone();
        root_declaration.uid = 0;
        assert_eq!(
            root_declaration.validate().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied,
            "root remains forbidden by the production binding validator"
        );
        binding.command.args.push("--landlock-best-effort".into());
        let error = binding.verify_shell_context(&certified_digest).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        binding.launcher_start += 1;
        assert!(binding.matches_peer(&peer).is_err());
        assert!(peer.current());
    }
}

#[cfg(test)]
mod original_home_alias_tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;
    #[test]
    fn approved_native_alias_preserves_bytes_in_sealed_production_bwrap_args() {
        let source = tempfile::tempdir().unwrap();
        let held = File::open(source.path()).unwrap();
        let raw = b"/home/original-\xff";
        let path = PathBuf::from(std::ffi::OsString::from_vec(raw.to_vec()));
        let alias = OriginalHomeAlias::capture(&path, &held).unwrap();
        let roundtrip: OriginalHomeAlias =
            serde_json::from_slice(&serde_json::to_vec(&alias).unwrap()).unwrap();
        assert_eq!(roundtrip, alias);
        assert_eq!(alias.source_ino(), held.metadata().unwrap().ino());
        let file = alias
            .args(&["--ro-bind".into(), "/usr".into(), "/usr".into()], true)
            .unwrap();
        let mut bytes = Vec::new();
        (&file).read_to_end(&mut bytes).unwrap();
        let mut expected = b"--dir\0/home\0--bind-fd\x0058\0".to_vec();
        expected.extend(raw);
        expected.push(0);
        assert_eq!(bytes, expected);
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } & SEALS,
            SEALS
        );
        assert!(file.write_at(b"changed", 0).is_err());
    }
    #[test]
    fn alias_refuses_host_parents_system_roots_and_existing_mount_collisions() {
        let source = tempfile::tempdir().unwrap();
        let held = File::open(source.path()).unwrap();
        for path in ["/", "/home/../other", "/home//other", "/home/other/"] {
            assert!(OriginalHomeAlias::capture(Path::new(path), &held).is_err());
        }
        let alias = OriginalHomeAlias::capture(Path::new("/home/original"), &held).unwrap();
        for argv in [
            vec!["--bind".into(), "/host/home".into(), "/home".into()],
            vec![
                "--ro-bind".into(),
                "/host/file".into(),
                "/home/original/file".into(),
            ],
            vec!["--symlink".into(), "/outside".into(), "/home".into()],
        ] {
            assert!(alias.args(&argv, true).is_err());
        }
        for path in [
            "/run/tilo/app",
            "/usr/home",
            "/etc/home",
            "/proc/home",
            "/sys/home",
            "/dev/home",
        ] {
            let alias = OriginalHomeAlias::capture(Path::new(path), &held).unwrap();
            assert!(alias.args(&[], true).is_err());
        }
    }
}
fn selected_edit_request_id(resource: &str) -> io::Result<&str> {
    let id = resource
        .strip_prefix("selected-open-v2:edit-in-parent:")
        .ok_or_else(|| refused("Selected edit requires exact signed v2 access"))?;
    if !uuid(id)
        || uuid::Uuid::parse_str(id)
            .map_err(io::Error::other)?
            .is_nil()
    {
        return Err(refused("Selected edit request invalid"));
    }
    Ok(id)
}
fn selected_destination(destination: &str) -> io::Result<()> {
    let leaf = destination
        .strip_prefix("/run/tilo/selected-open/")
        .ok_or_else(|| refused("Selected destination differs"))?;
    if leaf.is_empty()
        || leaf.len() > 255
        || leaf == "."
        || leaf == ".."
        || leaf.as_bytes().contains(&0)
        || leaf.contains('/')
    {
        return Err(refused("Selected destination leaf invalid"));
    }
    Ok(())
}
const SELECTED_PARENT_ROOT: &str = "/run/tilo/selected-open";
pub(crate) const SELECTED_PARENT_FD: RawFd = 57;
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedParentReceipt {
    schema: u32,
    binding: ConsumerLaunchBinding,
    context_sha256: String,
    root_pid: u32,
    request_id: String,
    destination: String,
    leaf: Vec<u8>,
    leaf_sha256: String,
    source_context_sha256: String,
    source_view_sha256: String,
    parent_dev: u64,
    parent_ino: u64,
    mount_id: u64,
    selected_dev: u64,
    selected_ino: u64,
    native_parent: [u64; 2],
    native_selected: [u64; 2],
    max_owned_temporaries: u32,
}
fn parent_destination(leaf: &[u8]) -> io::Result<String> {
    // The signed ConsumerCommand currently has String argv. Preserve raw source
    // bytes in the receipt, but refuse dispatch rather than write a lossy alias.
    let leaf = std::str::from_utf8(leaf).map_err(io::Error::other)?;
    let destination = format!("{SELECTED_PARENT_ROOT}/{leaf}");
    selected_destination(&destination)?;
    Ok(destination)
}
fn check_parent_view(view: &File, receipt: &SelectedParentReceipt) -> io::Result<()> {
    selected_destination(&receipt.destination)?;
    if receipt.schema != 1
        || receipt.max_owned_temporaries != 64
        || receipt.native_parent[1] == 0
        || receipt.native_selected[1] == 0
        || receipt.native_parent == receipt.native_selected
        || !receipt.binding.writable
        || selected_edit_request_id(&receipt.binding.resource_record)? != receipt.request_id
        || receipt.leaf_sha256 != sha(&receipt.leaf)
        || receipt.destination != parent_destination(&receipt.leaf)?
        || !digest(&receipt.source_context_sha256)
        || !digest(&receipt.source_view_sha256)
        || receipt.source_context_sha256 == "0".repeat(64)
        || receipt.source_view_sha256 == "0".repeat(64)
    {
        return Err(refused(
            "Selected parent sealed source/write binding differs",
        ));
    }
    let metadata = view.metadata()?;
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(view.as_raw_fd(), &raw mut fs) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if !metadata.is_dir()
        || fs.f_type != 0x6573_5546
        || (metadata.dev(), metadata.ino()) != (receipt.parent_dev, receipt.parent_ino)
        || mount_id(view)? != receipt.mount_id
    {
        return Err(refused(
            "Selected parent is not exact admitted FUSE directory",
        ));
    }
    Ok(())
}
fn check_initial_parent_child(parent: &File, receipt: &SelectedParentReceipt) -> io::Result<()> {
    let name = std::ffi::CString::new(receipt.leaf.as_slice()).map_err(io::Error::other)?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let child = unsafe { File::from_raw_fd(fd) };
    let metadata = child.metadata()?;
    if !metadata.is_file()
        || (metadata.dev(), metadata.ino()) != (receipt.selected_dev, receipt.selected_ino)
        || mount_id(&child)? != receipt.mount_id
    {
        return Err(refused("Selected parent initial child differs"));
    }
    Ok(())
}
fn read_parent_receipt(fd: &OwnedFd) -> io::Result<SelectedParentReceipt> {
    let file = File::from(fd.try_clone()?);
    let metadata = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let name = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.len() == 0
        || metadata.len() > LIMIT as u64
        || seals < 0
        || seals & SEALS != SEALS
        || !name
            .as_os_str()
            .as_bytes()
            .starts_with(b"/memfd:tilo-selected-parent-receipt")
    {
        return Err(refused("Selected parent lacks sealed root receipt"));
    }
    let mut bytes = vec![0; usize::try_from(metadata.len()).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
/// Distinct parent-directory admission minted only from authenticated source
/// write rights; direct edits remain operations of the canonical `FsSession`.
pub struct SelectedParentAdmission {
    role: RootRole,
    view: File,
    lease: UnixStream,
    binding: ConsumerLaunchBinding,
    receipt: SelectedParentReceipt,
    manifest_path: PathBuf,
    payload_root: PathBuf,
    manifest: crate::manifest::ManifestView,
    opts: RunOptions,
}
impl SelectedParentAdmission {
    /// Receive only on the private endpoint inherited through Shell opcode 4.
    /// The normal signed GUI package is replayed before any selected parent is taken.
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn receive(
        gate: RetainedLaunchGate,
        manifest_path: &Path,
        payload_root: &Path,
        opts: &RunOptions,
        cancellation: &ServiceCancellation,
    ) -> Result<Arc<Self>, RunError> {
        if !matches!(opts.mode, crate::run::LaunchMode::Gui)
            || !opts.landlock_enforce
            || opts.landlock_best_effort
            || opts.allow_unsigned_runtime
            || opts.sandbox_shim_bin.is_some()
            || opts
                .bwrap_bin
                .as_deref()
                .is_some_and(|path| path != Path::new("/usr/bin/bwrap"))
            || (rustix::process::getuid().as_raw() == 0 || rustix::process::geteuid().as_raw() == 0)
        {
            return Err(RunError::InvalidServiceLaunch(
                "selected parent requires ordinary confined non-root GUI".into(),
            ));
        }
        let _bwrap = protected_program("/usr/bin/bwrap")?;
        let package = opts.expected_package_hash.as_deref().ok_or_else(|| {
            RunError::InvalidServiceLaunch("selected parent requires signed package pin".into())
        })?;
        let manifest =
            crate::run::verified_original_manifest(manifest_path, payload_root, opts, package)?;
        if manifest.kind != "app" || manifest.entrypoint.gui_bin().is_none() {
            return Err(RunError::InvalidServiceLaunch(
                "selected parent requires signed GUI app".into(),
            ));
        }
        let (role, mut stream) = RootRole::authenticate(gate, cancellation)?;
        let (binding, view, receipt) = {
            let mut io = Handoff::new(&mut stream, cancellation, Duration::from_secs(30))?;
            let challenge: ConsumerChallenge = receive_json(&mut io)?;
            challenge.validate()?;
            selected_edit_request_id(&challenge.resource_record)?;
            challenge.command.matches_current()?;
            let room = collab_types::person_room(&challenge.person).to_string();
            if opts.workspace_id.as_deref() != Some(room.as_str()) {
                return Err(RunError::InvalidServiceLaunch(
                    "selected parent requires certified personal room".into(),
                ));
            }
            if normal_publisher(manifest_path, payload_root, opts)? != challenge.publisher_hex {
                return Err(refused("retained signed normal publisher differs").into());
            }
            let binding = binding_for(&manifest, opts, &challenge, manifest_path, payload_root)?;
            if !binding.writable {
                return Err(refused("Selected parent requires normal Author policy").into());
            }
            binding.verify_shell_context(&challenge.shell_context_sha256)?;

            send_json(&mut io, &binding)?;
            let receipt = receive_fd(&mut io, 0)?;
            let receipt: SelectedParentReceipt = read_parent_receipt(&receipt)?;
            if receipt.schema != 1
                || receipt.binding != binding
                || receipt.context_sha256 != binding.context_sha256()?
                || receipt.root_pid != role.peer.pid()
            {
                return Err(refused("selected root receipt launch binding differs").into());
            }
            if opts.open_document.as_deref() != Some(Path::new(&receipt.destination))
                || opts.open_uri.is_some()
                || selected_edit_request_id(&binding.resource_record)? != receipt.request_id
            {
                return Err(refused("selected exact open intent differs").into());
            }
            let view = File::from(receive_fd(&mut io, 1)?);
            check_parent_view(&view, &receipt)?;
            check_initial_parent_child(&view, &receipt)?;
            role.check()?;
            (binding, view, receipt)
        };
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        let admission = Arc::new(Self {
            role,
            view,
            lease: stream,
            binding,
            receipt,
            manifest_path: manifest_path.into(),
            payload_root: payload_root.into(),
            manifest,
            opts: opts.clone(),
        });
        admission.check()?;
        Ok(admission)
    }
    #[must_use]
    pub fn binding(&self) -> &ConsumerLaunchBinding {
        &self.binding
    }
    pub(crate) fn descriptor_pair(&self) -> io::Result<(RawFd, RawFd)> {
        self.check()?;
        Ok((self.view.as_raw_fd(), SELECTED_PARENT_FD))
    }
    pub(crate) fn splice(
        &self,
        argv: &mut Vec<String>,
        participation: Participation,
    ) -> io::Result<()> {
        self.check()?;
        if participation != Participation::Author {
            return Err(refused("Selected parent Author policy retired"));
        }
        let entry = argv
            .iter()
            .position(|arg| arg == "--")
            .ok_or_else(|| refused("selected parent bwrap entrypoint unavailable"))?;
        argv.splice(
            entry..entry,
            [
                "--bind-fd".into(),
                SELECTED_PARENT_FD.to_string(),
                SELECTED_PARENT_ROOT.into(),
            ],
        );
        let position = argv
            .iter()
            .position(|arg| arg == crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND)
            .ok_or_else(|| refused("selected parent Landlock shim unavailable"))?;
        argv.splice(
            position.saturating_add(1)..position.saturating_add(1),
            ["--rw-noexec".into(), SELECTED_PARENT_ROOT.into()],
        );
        Ok(())
    }
    pub(crate) fn namespace_owner(
        self: &Arc<Self>,
        cancellation: ServiceCancellation,
    ) -> io::Result<OrdinaryNamespaceOwner> {
        let connected = ConnectedNamespace::new(self.lease.try_clone()?);
        OrdinaryNamespaceOwner::with_selected_parent(
            self.clone(),
            cancellation,
            connected,
            self.clone(),
        )
    }
    /// # Errors
    /// Refuses missing, stale or incompatible kernel or signed launch evidence.
    pub fn check(&self) -> io::Result<()> {
        self.role.check()?;
        check_parent_view(&self.view, &self.receipt)?;
        let mut byte = [0];
        let count = unsafe {
            libc::recv(
                self.lease.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if count >= 0 {
            return Err(refused("selected root lease stopped or changed"));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let manifest = &self.manifest;
        if manifest.package_id != self.binding.app_id
            || policy_hash(&self.opts)? != self.binding.policy_sha256
            || self.binding.writable
                && crate::run::resolve_capabilities(manifest, &self.opts).participation
                    != Participation::Author
        {
            return Err(refused("retained ordinary policy changed"));
        }
        Ok(())
    }
}
impl ServiceAuthority for SelectedParentAdmission {
    fn check(&self) -> io::Result<()> {
        SelectedParentAdmission::check(self)
    }
}

impl RootConsumerChannel {
    /// Consume only the exact endpoint-minted live edit issue. The private root
    /// session independently validates its genuine editor/source admission.
    /// Failure retains the exact stream and conservative possible-view state.
    /// # Errors
    /// Refuses cancelled or unauthenticated channels, invalid handoff evidence,
    /// and failed or timed-out socket transfers.
    pub fn issue_selected_parent(
        mut self,
        grant: crate::owned_service::target_projection::SelectedParentIssue<'_>,
        exposure: &mut crate::owned_service::target_projection::HeldViewExposure,
        cancellation: &ServiceCancellation,
    ) -> Result<(UnixStream, Arc<ExecutingPeer>), FailedViewIssue> {
        let mut progress = IssueProgress::NoView;
        let result = (|| {
            if cancellation.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            own_helper()?;
            grant.check()?;
            let binding = grant.binding();
            binding.verify_peer_program(&self.peer)?;
            binding.matches_peer(grant.peer())?;
            let parent = grant.parent();
            exposure.matches(parent)?;
            let selected = grant.selected()?;
            let receipt = SelectedParentReceipt {
                schema: 1,
                binding: binding.clone(),
                context_sha256: binding.context_sha256()?,
                root_pid: std::process::id(),
                request_id: grant.request_id().into(),
                destination: parent_destination(grant.leaf())?,
                leaf: grant.leaf().to_vec(),
                leaf_sha256: sha(grant.leaf()),
                source_context_sha256: hex::encode(grant.origin_context()),
                source_view_sha256: hex::encode(parent.source_view_sha256()?),
                parent_dev: parent.dev()?,
                parent_ino: parent.ino()?,
                mount_id: exposure.mount_id()?,
                selected_dev: selected.dev()?,
                selected_ino: selected.ino()?,
                native_parent: grant.native_parent(),
                native_selected: grant.native_selected(),
                max_owned_temporaries: 64,
            };
            if selected.mount_id()? != parent.mount_id()? {
                return Err(refused("Selected edit child mount differs"));
            }
            // Root never fstats or path-reopens the Owner-only FUSE object.
            grant.check()?;
            let sealed = seal_parent_receipt(&receipt)?;
            exposure.mark_possible_delivery()?;
            let mut io = Handoff::new(&mut self.stream, cancellation, Duration::from_secs(30))?;
            for (index, fd) in [sealed.as_fd(), exposure.file()?.as_fd()]
                .into_iter()
                .enumerate()
            {
                own_helper()?;
                binding.verify_peer_program(&self.peer)?;
                grant.check()?;
                exposure.check()?;
                send_issue_fd(
                    &mut io,
                    u8::try_from(index).map_err(io::Error::other)?,
                    fd,
                    &mut progress,
                )?;
                let mut ack = [0];
                io.read_exact(&mut ack)?;
                if ack != [1] {
                    return Err(refused("Selected edit handoff refused"));
                }
            }
            drop(io);
            own_helper()?;
            binding.matches_peer(&self.peer)?;
            self.stream.set_read_timeout(None)?;
            self.stream.set_write_timeout(None)?;
            Ok(())
        })();
        drop(grant);
        match result {
            Ok(()) => Ok((self.stream, self.peer)),
            Err(error) => Err(FailedViewIssue {
                error,
                progress,
                stream: self.stream,
            }),
        }
    }
}
fn seal_parent_receipt(receipt: &SelectedParentReceipt) -> io::Result<File> {
    let bytes = serde_json::to_vec(receipt).map_err(io::Error::other)?;
    if bytes.is_empty() || bytes.len() > LIMIT {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-selected-parent-receipt".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(&bytes)?;
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}
/// Ordinary app policy and canonical namespace custody remain unchanged.
/// # Errors
/// Refuses retired typed parent admission or ordinary launch failure.
pub fn run_selected_parent_gui(
    admission: Arc<SelectedParentAdmission>,
    cancellation: ServiceCancellation,
) -> Result<std::process::ExitStatus, RunError> {
    let owner = admission.namespace_owner(cancellation)?;
    let result = crate::run::run_supervised_gui(
        &admission.manifest_path,
        &admission.payload_root,
        &admission.opts,
        &owner,
    );
    drop(admission);
    result
}

#[cfg(test)]
mod selected_parent_receipt_tests {
    use super::*;
    #[test]
    fn selected_parent_command_refuses_lossy_raw_leaf_alias() {
        assert!(parent_destination(&[b'b', 0xff, b'x']).is_err());
        assert_eq!(
            parent_destination(b"book.txt").unwrap(),
            "/run/tilo/selected-open/book.txt"
        );
    }
}

/// Closed private launch registration. Existing plain and Home tags retain
/// their meaning; edit uses its own admission and never upgrades Home.
pub enum OrdinaryLaunchGate {
    Home(RetainedLaunchGate),
    SelectedParent(RetainedLaunchGate),
}
impl RetainedLaunchGate {
    /// # Errors
    /// Refuses cancelled or unauthenticated channels, invalid handoff evidence,
    /// and failed or timed-out socket transfers.
    pub fn receive_registration(
        mut gate: UnixStream,
        cancellation: &ServiceCancellation,
    ) -> io::Result<Option<OrdinaryLaunchGate>> {
        let (bytes, endpoint) =
            qpe_exchange(&mut gate, cancellation, Duration::from_secs(10), |gate| {
                runtime_portal::fd_transport::recv_with_fd(gate.as_fd())
            })?;
        match (bytes.as_slice(), endpoint) {
            ([1], None) => Ok(None),
            ([2], Some(endpoint)) => Self::receive(gate, UnixStream::from(endpoint), cancellation)
                .map(|gate| Some(OrdinaryLaunchGate::Home(gate))),
            ([4], Some(endpoint)) => Self::receive(gate, UnixStream::from(endpoint), cancellation)
                .map(|gate| Some(OrdinaryLaunchGate::SelectedParent(gate))),
            _ => Err(refused("Invalid private launch registration frame")),
        }
    }
}

#[cfg(test)]
mod selected_edit_gate_guards {
    use super::*;
    #[test]
    fn edit_resource_cannot_upgrade_readonly_or_legacy_selection() {
        let id = "52f63bf1-5967-4eaf-a4d9-3c6575e872e7";
        assert!(selected_edit_request_id(&format!("selected-open-v2:edit-in-parent:{id}")).is_ok());
        for value in [
            format!("selected-open:{id}"),
            format!("selected-open-v2:read-only:{id}"),
            "selected-open-v2:edit-in-parent:00000000-0000-0000-0000-000000000000".into(),
        ] {
            assert!(selected_edit_request_id(&value).is_err());
        }
    }
    #[test]
    fn cancelled_registration_performs_no_initial_socket_io() {
        let (gate, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let cancel = ServiceCancellation::default();
        cancel.cancel();
        assert!(
            matches!(RetainedLaunchGate::receive_registration(gate,&cancel),Err(error) if error.kind()==io::ErrorKind::Interrupted)
        );
        let mut byte = [0];
        assert_eq!(peer.read(&mut byte).unwrap(), 0);
    }
    #[test]
    fn edit_gate_without_owned_endpoint_is_refused_before_qpe() {
        let (gate, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(&[4]).unwrap();
        assert!(
            matches!(RetainedLaunchGate::receive_registration(gate,&ServiceCancellation::default()),Err(error) if error.kind()==io::ErrorKind::PermissionDenied)
        );
    }
    #[test]
    fn actual_stalled_socket_preserves_possible_directory_delivery() {
        let (mut stream, _peer) = UnixStream::pair().unwrap();
        let held = tempfile::tempdir().unwrap();
        let directory = File::open(held.path()).unwrap();
        let cancel = ServiceCancellation::default();
        let mut handoff = Handoff::new(&mut stream, &cancel, Duration::from_millis(20)).unwrap();
        let mut progress = IssueProgress::NoView;
        send_issue_fd(&mut handoff, 1, directory.as_fd(), &mut progress).unwrap();
        let mut ack = [0];
        assert_eq!(
            handoff.read_exact(&mut ack).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(progress, IssueProgress::PossibleView);
    }
}
