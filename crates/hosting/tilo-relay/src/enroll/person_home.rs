//! Access to what lives in the person's home: the identity directory (device
//! fleet, joined identity record, identity registry), the room list their
//! session exports, and a first-use pairing status file.
//!
//! Approving a device with a seed under `/var/lib/tilo`, and a headless join
//! (`sudo tilo-enroll pair-device`), run as root, but the person (and anything
//! that can write to their home) controls every path in it. Root therefore
//! never opens those paths. Each access runs in a fresh exec of this binary
//! that has dropped to the person's uid and primary group before exec, so a
//! link planted in the home resolves with the person's own permissions and
//! reaches nothing they could not already change.
//!
//! The accessing process, whether that child or a run already going as the
//! person, also opens every component it touches without following a final
//! symbolic link. Files are reached through directory descriptors opened with
//! `O_NOFOLLOW | O_DIRECTORY` whose owner must be the accessing uid, and
//! replacements are renamed into place inside the descriptor that was
//! checked, so swapping a directory for a link mid-way cannot redirect the
//! write. Nothing is chowned: what the person's uid writes is theirs.
//!
//! The person's directory of direct peers and shared rooms below the device's
//! peers directory is written the same way: the child merges into it as the
//! person, with the device-trust group they belong to, which the shared
//! peers directory needs.

use std::ffi::{CStr, CString, OsString};
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};

/// Largest file read from or written to the person's home.
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
/// Largest exported room list an approval reads.
pub const MAX_ROOMS_BYTES: usize = 64 * 1024;
/// The identity directory's subdirectory holding device fleets.
pub const FLEETS_DIR: &str = "fleets";
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the person's own files are read and written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonHome {
    /// This process runs as the person.
    Direct,
    /// This process is root; every access runs in `program` as `person`.
    Delegated { program: PathBuf, person: Person },
}

/// The credentials a delegated access runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Person {
    pub uid: libc::uid_t,
    pub gid: libc::gid_t,
}

/// One file under a directory of the person's: `name`, optionally inside a
/// single subdirectory of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeFile<'a> {
    pub subdir: Option<&'a str>,
    pub name: &'a str,
}

impl<'a> HomeFile<'a> {
    pub const fn at(name: &'a str) -> Self {
        Self { subdir: None, name }
    }
}

fn fleet_name(user: [u8; 32]) -> String {
    format!("{}.json", hex::encode(user))
}

/// `path` split into its directory and file name.
fn split(path: &Path) -> anyhow::Result<(&Path, &str)> {
    match (
        path.parent(),
        path.file_name().and_then(|name| name.to_str()),
    ) {
        (Some(parent), Some(name)) => Ok((parent, name)),
        _ => bail!("{} has no directory and file name", path.display()),
    }
}

impl PersonHome {
    /// How this process, running with effective uid `euid`, reaches the home
    /// of `person_uid`, the account the caller acts for.
    ///
    /// Root told whose home it is touches it only as that account. Root
    /// acting for itself reaches only directories and files root owns, never
    /// following a final link, so a person's directory is refused rather than
    /// written as root. A process that is not root can only act for itself.
    pub fn for_process(
        euid: libc::uid_t,
        person_uid: Option<libc::uid_t>,
        lookup_gid: impl FnOnce(libc::uid_t) -> anyhow::Result<libc::gid_t>,
    ) -> anyhow::Result<Self> {
        match (euid, person_uid) {
            (0, Some(uid)) if uid != 0 => Ok(Self::Delegated {
                program: PathBuf::from("/proc/self/exe"),
                person: Person {
                    uid,
                    gid: lookup_gid(uid)?,
                },
            }),
            (euid, Some(uid)) if uid != euid => bail!(
                "--person-uid {uid} is not the account this runs as; run this as that account"
            ),
            (_, _) => Ok(Self::Direct),
        }
    }

    /// The home of the account this process runs as.
    pub fn for_this_process(person_uid: Option<libc::uid_t>) -> anyhow::Result<Self> {
        Self::for_process(local::own_uid(), person_uid, primary_gid)
    }

    /// Whether every access runs in a child as the person.
    pub const fn is_delegated(&self) -> bool {
        matches!(self, Self::Delegated { .. })
    }

    /// The bytes of `file` under `dir`, or `None` when it or `dir` does not
    /// exist.
    pub fn read_file(
        &self,
        dir: &Path,
        file: HomeFile<'_>,
        limit: usize,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let limit = limit.min(MAX_FILE_BYTES);
        match self {
            Self::Direct => Ok(local::read_file(dir, file, local::own_uid(), limit)?),
            Self::Delegated { program, person } => {
                let output = run_child(
                    program,
                    *person,
                    None,
                    &child_args(&ChildRequest::read(dir, file, limit)),
                    None,
                )?;
                decode_read(&output, limit)
            }
        }
    }

    /// Replace `file` under `dir` with `bytes`, mode `mode`. With `create_dir`
    /// the directory (and its missing parents) is created first, private to
    /// the person.
    pub fn write_file(
        &self,
        dir: &Path,
        file: HomeFile<'_>,
        bytes: &[u8],
        mode: libc::mode_t,
        create_dir: bool,
    ) -> anyhow::Result<()> {
        if bytes.len() > MAX_FILE_BYTES {
            bail!("{} exceeds 1 MiB", file.name);
        }
        match self {
            Self::Direct => Ok(local::write_file(
                dir,
                file,
                bytes,
                local::own_uid(),
                mode,
                create_dir,
            )?),
            Self::Delegated { program, person } => {
                run_child(
                    program,
                    *person,
                    None,
                    &child_args(&ChildRequest::write(dir, file, mode, create_dir)),
                    Some(bytes),
                )?;
                Ok(())
            }
        }
    }

    /// The identity's device fleet in `identity_dir`, or `None` when there is
    /// none yet.
    pub fn read_fleet(
        &self,
        identity_dir: &Path,
        user: [u8; 32],
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let name = fleet_name(user);
        self.read_file(
            identity_dir,
            HomeFile {
                subdir: Some(FLEETS_DIR),
                name: &name,
            },
            MAX_FILE_BYTES,
        )
    }

    /// Run a fleet update with the person's checked fleet-directory descriptor
    /// locked from snapshot through durable publication. A delegated child holds
    /// the descriptor; only bounded public projections cross the IPC channel.
    pub fn with_fleet<T>(
        &self,
        identity_dir: &Path,
        user: [u8; 32],
        operation: impl FnOnce(Option<Vec<u8>>, &mut FleetPublish<'_>) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let name = fleet_name(user);
        match self {
            Self::Direct => with_local_fleet(identity_dir, &name, operation),
            Self::Delegated { program, person } => {
                let request = ChildRequest {
                    operation: ChildOperation::FleetTransaction,
                    path: identity_dir.to_path_buf(),
                    subdir: None,
                    name,
                    mode: 0o644,
                    create_dir: false,
                    limit: MAX_FILE_BYTES,
                };
                run_fleet_child(program, *person, &request, operation)
            }
        }
    }

    /// Replace the identity's device fleet in `identity_dir` with `bytes`.
    pub fn write_fleet(
        &self,
        identity_dir: &Path,
        user: [u8; 32],
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        let name = fleet_name(user);
        self.write_file(
            identity_dir,
            HomeFile {
                subdir: Some(FLEETS_DIR),
                name: &name,
            },
            bytes,
            0o644,
            false,
        )
    }

    /// Merge `direct_peers` and `rooms` into the person's directory of
    /// `identity` in the device's peers directory `peers`, creating it owned
    /// by the person. A delegated merge runs as the person with the peers
    /// directory's group (the device-trust group), and only when they
    /// belong to it.
    pub fn merge_peers(
        &self,
        peers: &Path,
        identity: &[u8; 32],
        direct_peers: &[tilo_relay_protocol::personal_devices::DirectPeer],
        rooms: &[String],
    ) -> anyhow::Result<()> {
        let merge = PeersMerge {
            direct_peers: serde_json::from_slice(
                &tilo_relay_protocol::personal_devices::encode_direct_peers(direct_peers)?,
            )?,
            rooms: rooms.to_vec(),
        };
        match self {
            Self::Direct => merge_peers_here(peers, identity, &merge),
            Self::Delegated { program, person } => {
                let group = device_trust_gid(peers, person.uid)?;
                let request = ChildRequest {
                    operation: ChildOperation::MergePeers,
                    path: peers.to_path_buf(),
                    subdir: None,
                    name: hex::encode(identity),
                    mode: 0,
                    create_dir: false,
                    limit: MAX_FILE_BYTES,
                };
                run_child(
                    program,
                    *person,
                    Some(group),
                    &child_args(&request),
                    Some(&serde_json::to_vec(&merge)?),
                )?;
                Ok(())
            }
        }
    }

    /// The exported room list at `path`, or `None` when there is none.
    pub fn read_rooms(&self, path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
        let (dir, name) = split(path)?;
        self.read_file(dir, HomeFile::at(name), MAX_ROOMS_BYTES)
    }

    /// Replace the file at `path`, whose directory must already exist.
    pub fn write_path(&self, path: &Path, bytes: &[u8], mode: libc::mode_t) -> anyhow::Result<()> {
        let (dir, name) = split(path)?;
        self.write_file(dir, HomeFile::at(name), bytes, mode, false)
    }
}

/// What the delegated child is asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRequest {
    pub operation: ChildOperation,
    pub path: PathBuf,
    pub subdir: Option<String>,
    pub name: String,
    pub mode: libc::mode_t,
    pub create_dir: bool,
    pub limit: usize,
}

impl ChildRequest {
    fn read(dir: &Path, file: HomeFile<'_>, limit: usize) -> Self {
        Self {
            operation: ChildOperation::Read,
            path: dir.to_path_buf(),
            subdir: file.subdir.map(str::to_owned),
            name: file.name.to_owned(),
            mode: 0,
            create_dir: false,
            limit,
        }
    }

    fn write(dir: &Path, file: HomeFile<'_>, mode: libc::mode_t, create_dir: bool) -> Self {
        Self {
            operation: ChildOperation::Write,
            path: dir.to_path_buf(),
            subdir: file.subdir.map(str::to_owned),
            name: file.name.to_owned(),
            mode,
            create_dir,
            limit: MAX_FILE_BYTES,
        }
    }

    fn file(&self) -> HomeFile<'_> {
        HomeFile {
            subdir: self.subdir.as_deref(),
            name: &self.name,
        }
    }
}

/// An access the delegated child performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ChildOperation {
    Read,
    Write,
    /// Merge direct peers and rooms (JSON on stdin) into the person's
    /// directory of identity `name` in the peers directory `path`.
    MergePeers,
    /// Hold the checked fleet directory lock across snapshot and publication.
    FleetTransaction,
}

/// The name of the hidden subcommand the delegated child runs.
pub const CHILD_COMMAND: &str = "person-home";

fn child_args(request: &ChildRequest) -> Vec<OsString> {
    let operation = match request.operation {
        ChildOperation::Read => "read",
        ChildOperation::Write => "write",
        ChildOperation::MergePeers => "merge-peers",
        ChildOperation::FleetTransaction => "fleet-transaction",
    };
    let mut args: Vec<OsString> = vec![
        CHILD_COMMAND.into(),
        operation.into(),
        "--path".into(),
        request.path.clone().into(),
        "--name".into(),
        request.name.clone().into(),
        "--mode".into(),
        format!("{:o}", request.mode).into(),
        "--limit".into(),
        request.limit.to_string().into(),
    ];
    if let Some(subdir) = &request.subdir {
        args.push("--subdir".into());
        args.push(subdir.clone().into());
    }
    if request.create_dir {
        args.push("--create-dir".into());
    }
    args
}

/// The child's command line, environment and credentials. The environment is
/// empty, the working directory is `/`, and before exec the child gives up
/// root for good and marks every inherited descriptor above stderr
/// close-on-exec, so it holds nothing of the approving process but its
/// stdin, stdout and stderr.
fn child_command(
    program: &Path,
    person: Person,
    extra_group: Option<libc::gid_t>,
    args: &[OsString],
) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.args(args).env_clear().current_dir("/");
    let Person { uid, gid } = person;
    // SAFETY: the closure runs in the forked child before exec and calls only
    // async-signal-safe syscalls on values copied before the fork.
    unsafe {
        command.pre_exec(move || drop_to(uid, gid, extra_group));
    }
    command
}

/// Give up root for `uid`/`gid` irrevocably. Runs between fork and exec.
///
/// Until exec this child is a copy of the approving process, secrets
/// included, yet the person may signal it once its uid is theirs. Every
/// catchable signal is blocked and core dumps are off first, so nothing the
/// person sends can make it dump that memory; the exec'd child unblocks them.
fn drop_to(
    uid: libc::uid_t,
    gid: libc::gid_t,
    extra_group: Option<libc::gid_t>,
) -> std::io::Result<()> {
    const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
    // SAFETY: plain syscalls on stack values that outlive each call.
    unsafe {
        let mut all: libc::sigset_t = std::mem::zeroed();
        let no_core = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::sigfillset(&raw mut all) != 0
            || libc::sigprocmask(libc::SIG_SETMASK, &raw const all, std::ptr::null_mut()) != 0
            || libc::setrlimit(libc::RLIMIT_CORE, &raw const no_core) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if libc::syscall(
            libc::SYS_close_range,
            3 as libc::c_uint,
            libc::c_uint::MAX,
            CLOSE_RANGE_CLOEXEC,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let groups = [gid, extra_group.unwrap_or(gid)];
        let count = if extra_group.is_some() { 2 } else { 1 };
        if libc::setgroups(count, groups.as_ptr()) != 0
            || libc::setresgid(gid, gid, gid) != 0
            || libc::setresuid(uid, uid, uid) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if libc::getuid() != uid
            || libc::geteuid() != uid
            || libc::getgid() != gid
            || libc::getegid() != gid
            || libc::setuid(0) == 0
        {
            return Err(std::io::Error::from_raw_os_error(libc::EPERM));
        }
    }
    Ok(())
}

fn run_child(
    program: &Path,
    person: Person,
    extra_group: Option<libc::gid_t>,
    args: &[OsString],
    input: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let command = child_command(program, person, extra_group, args);
    let output = bounded_command::run_prepared_with_input_limit(
        command,
        input,
        MAX_FILE_BYTES,
        MAX_FILE_BYTES + 1,
        16 * 1024,
        CHILD_TIMEOUT,
    )
    .context("reach the person's files as the person")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{}", stderr.trim());
    }
    Ok(output.stdout)
}

type FleetPublish<'a> = dyn FnMut(&[u8]) -> anyhow::Result<()> + 'a;

fn with_local_fleet<T>(
    identity_dir: &Path,
    name: &str,
    operation: impl FnOnce(Option<Vec<u8>>, &mut FleetPublish<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    use std::os::fd::AsRawFd as _;
    let uid = local::own_uid();
    let identity = local::open_owned_dir(identity_dir, uid)?;
    let directory = local::open_owned_subdir(&identity, FLEETS_DIR, uid, Some(0o755))?
        .context("fleet directory is missing")?;
    // Persist a newly created fleet-directory entry before publishing in it.
    // SAFETY: identity is an open directory descriptor.
    if unsafe { libc::fsync(identity.as_raw_fd()) } != 0 {
        return Err(std::io::Error::last_os_error()).context("sync the identity directory");
    }
    // Same directory flock as the session's DeviceFleetStore. Fail busy rather
    // than letting an account that holds the lock indefinitely stall a signer.
    let _lock = advisory_lock::FileLock::try_exclusive(std::fs::File::from(directory.try_clone()?))
        .context("fleet is busy; retry enrollment")?;
    let snapshot = local::read_owned_file(&directory, name, uid, MAX_FILE_BYTES)?;
    operation(snapshot, &mut |bytes| {
        if bytes.len() > MAX_FILE_BYTES {
            bail!("fleet exceeds 1 MiB");
        }
        local::replace_owned_file(&directory, name, bytes, uid, 0o644)?;
        Ok(())
    })
}

/// Length-prefixed frames have a fixed allocation bound, including the absent
/// file flag. No paths, signing requests or nonce-store operations come back
/// from the child: it supplies a public snapshot and a publication receipt.
fn write_frame(writer: &mut impl std::io::Write, bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() > MAX_FILE_BYTES + 1 {
        bail!("fleet transaction frame exceeds its bound");
    }
    let size = u32::try_from(bytes.len()).context("fleet frame size is invalid")?;
    writer.write_all(&size.to_be_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

fn read_frame(reader: &mut impl std::io::Read) -> anyhow::Result<Vec<u8>> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > MAX_FILE_BYTES + 1 {
        bail!("fleet transaction frame exceeds its bound");
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn serve_fleet_transaction(
    request: &ChildRequest,
    reader: &mut impl std::io::Read,
    writer: &mut impl std::io::Write,
) -> anyhow::Result<()> {
    with_local_fleet(&request.path, &request.name, |snapshot, publish| {
        write_frame(writer, &encode_read(snapshot.as_deref()))?;
        let bytes = read_frame(reader)?;
        publish(&bytes)?;
        write_frame(writer, b"")
    })
}

struct FleetChild {
    process: std::process::Child,
    reaped: bool,
}

impl Drop for FleetChild {
    fn drop(&mut self) {
        // Also reap on malformed output, signer failure, EOF and timeout: no
        // dropped-uid child or directory lock outlives its approving process.
        if !self.reaped {
            bounded_command::kill_tree(&mut self.process);
            let _ = self.process.wait();
        }
    }
}

/// Recompute the remaining total budget before every read/write syscall, so
/// a peer sending one byte at a time cannot extend the 30-second transaction.
struct FleetChannel {
    socket: std::os::unix::net::UnixStream,
    deadline: std::time::Instant,
}

impl FleetChannel {
    fn remaining(&self) -> std::io::Result<Duration> {
        let remaining = self
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "fleet transaction timed out",
            ));
        }
        Ok(remaining)
    }
}

impl std::io::Read for FleetChannel {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        self.socket.read(bytes)
    }
}

impl std::io::Write for FleetChannel {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        self.socket.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.socket.flush()
    }
}

fn run_fleet_child<T>(
    program: &Path,
    person: Person,
    request: &ChildRequest,
    operation: impl FnOnce(Option<Vec<u8>>, &mut FleetPublish<'_>) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    use std::process::Stdio;
    let deadline = std::time::Instant::now()
        .checked_add(CHILD_TIMEOUT)
        .context("fleet transaction deadline overflow")?;
    let (socket, peer) = std::os::unix::net::UnixStream::pair()?;
    let mut command = child_command(program, person, None, &child_args(request));
    command
        .stdin(Stdio::from(OwnedFd::from(peer.try_clone()?)))
        .stdout(Stdio::from(OwnedFd::from(peer)))
        .stderr(Stdio::null());
    bounded_command::own_process_tree(&mut command);
    let mut child = FleetChild {
        process: command
            .spawn()
            .context("start the person's fleet transaction")?,
        reaped: false,
    };
    // Command retains its configured stdin/stdout descriptors after spawn.
    // Release our copies so helper exit is immediately observable as EOF.
    drop(command);
    let mut channel = FleetChannel { socket, deadline };
    let snapshot = decode_read(&read_frame(&mut channel)?, MAX_FILE_BYTES)?;
    if child.process.try_wait()?.is_some() {
        child.reaped = true;
        bail!("fleet transaction child exited before redemption");
    }
    let result = operation(snapshot, &mut |bytes| {
        write_frame(&mut channel, bytes)?;
        if !read_frame(&mut channel)?.is_empty() {
            bail!("fleet transaction publication receipt is malformed");
        }
        Ok(())
    })?;
    channel.socket.shutdown(std::net::Shutdown::Both)?;
    match bounded_command::wait_child_or(&mut child.process, None, Some(deadline))? {
        bounded_command::Wakeup::Exited(status) => {
            // Never signal a process group after reaping its leader: its pid
            // may already have been reused by an unrelated process.
            child.reaped = true;
            if !status.success() {
                bail!("fleet transaction child failed");
            }
            Ok(result)
        }
        _ => bail!("fleet transaction child timed out"),
    }
}

/// A read's answer on the child's stdout: `0` when the file is absent, `1`
/// followed by its bytes otherwise.
fn decode_read(output: &[u8], limit: usize) -> anyhow::Result<Option<Vec<u8>>> {
    match output.split_first() {
        Some((b'0', [])) => Ok(None),
        Some((b'1', bytes)) if bytes.len() <= limit => Ok(Some(bytes.to_vec())),
        Some((b'1', _)) => bail!("the person's file exceeds {limit} bytes"),
        _ => bail!("the person's identity file reader answered malformed output"),
    }
}

fn encode_read(bytes: Option<&[u8]>) -> Vec<u8> {
    bytes.map_or_else(
        || b"0".to_vec(),
        |bytes| {
            let mut out = Vec::with_capacity(bytes.len().saturating_add(1));
            out.push(b'1');
            out.extend_from_slice(bytes);
            out
        },
    )
}

/// The delegated child: perform `request` as this process's own uid and
/// answer on stdout.
pub fn serve_child(request: &ChildRequest) -> anyhow::Result<()> {
    // The parent blocked every signal across the credential change.
    // SAFETY: an empty set is a valid mask and the call has no other inputs.
    unsafe {
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&raw mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &raw const none, std::ptr::null_mut());
    }
    let uid = local::own_uid();
    let limit = request.limit.min(MAX_FILE_BYTES);
    let mut stdout = std::io::stdout().lock();
    match request.operation {
        ChildOperation::Read => {
            let bytes = local::read_file(&request.path, request.file(), uid, limit)?;
            stdout.write_all(&encode_read(bytes.as_deref()))?;
        }
        ChildOperation::Write => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .take((MAX_FILE_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_FILE_BYTES {
                bail!("{} exceeds 1 MiB", request.name);
            }
            local::write_file(
                &request.path,
                request.file(),
                &bytes,
                uid,
                request.mode & 0o777,
                request.create_dir,
            )?;
        }
        ChildOperation::FleetTransaction => {
            // The signer may be waiting on a root-only secret provider while
            // this process waits for publication. Bound the exec'd child's
            // own lifetime too, independently of the parent's IPC deadline:
            // even a stuck signer must not leave a fleet lock held forever.
            let seconds = libc::c_uint::try_from(CHILD_TIMEOUT.as_secs())?;
            // SAFETY: this is the dedicated exec'd helper, with signals
            // unblocked above. SIGALRM's default disposition terminates it.
            unsafe {
                if libc::signal(libc::SIGALRM, libc::SIG_DFL) == libc::SIG_ERR {
                    return Err(std::io::Error::last_os_error()).context("arm fleet child timeout");
                }
                libc::alarm(seconds);
            }
            serve_fleet_transaction(request, &mut std::io::stdin().lock(), &mut stdout)?;
        }
        ChildOperation::MergePeers => {
            let mut bytes = Vec::new();
            std::io::stdin()
                .lock()
                .take((MAX_FILE_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)?;
            let merge: PeersMerge =
                serde_json::from_slice(&bytes).context("the peers to merge are malformed")?;
            let identity: [u8; 32] = hex::decode(&request.name)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .context("--name must be an identity key")?;
            merge_peers_here(&request.path, &identity, &merge)?;
        }
    }
    stdout.flush()?;
    Ok(())
}

/// The person's primary group, from the account database.
pub fn primary_gid(uid: libc::uid_t) -> anyhow::Result<libc::gid_t> {
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter.
    let mut record: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buffer` outlives it.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 || found.is_null() {
        bail!("account {uid} was not found");
    }
    Ok(record.pw_gid)
}

/// Direct peers and rooms a delegated child merges into the person's
/// directory.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PeersMerge {
    direct_peers: serde_json::Value,
    rooms: Vec<String>,
}

/// Merge `merge` into this account's own directory of `identity` in `peers`.
fn merge_peers_here(peers: &Path, identity: &[u8; 32], merge: &PeersMerge) -> anyhow::Result<()> {
    use tilo_relay_protocol::personal_devices;
    let direct_peers =
        personal_devices::parse_direct_peers(&serde_json::to_vec(&merge.direct_peers)?)?;
    let person = personal_devices::PersonPeers::open_or_create_own(peers, identity)
        .with_context(|| format!("open this person's directory in {}", peers.display()))?;
    if !direct_peers.is_empty() {
        person.remember_direct_peers(&direct_peers)?;
    }
    if !merge.rooms.is_empty() {
        person.remember_rooms(&merge.rooms)?;
    }
    Ok(())
}

/// The group of the device's peers directory `peers` (the device-trust
/// group the image gives it), when the account `uid` belongs to it. The
/// directory must be root's, so its group is the image's choice.
fn device_trust_gid(peers: &Path, uid: libc::uid_t) -> anyhow::Result<libc::gid_t> {
    use std::os::unix::fs::MetadataExt as _;
    let directory = std::fs::symlink_metadata(peers)
        .with_context(|| format!("inspect the peers directory {}", peers.display()))?;
    if !directory.is_dir() || directory.uid() != 0 {
        bail!("{} is not root's peers directory", peers.display());
    }
    let device_trust = directory.gid();
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid out-parameter.
    let mut account: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call and `buffer` outlives it.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut account,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 || found.is_null() {
        bail!("account {uid} was not found");
    }
    let mut groups = vec![0 as libc::gid_t; 256];
    let mut count = libc::c_int::try_from(groups.len())?;
    // SAFETY: `pw_name` points into `buffer`, which is alive, and `groups`
    // holds `count` entries.
    let status = unsafe {
        libc::getgrouplist(
            account.pw_name,
            account.pw_gid,
            groups.as_mut_ptr(),
            &raw mut count,
        )
    };
    if status < 0 {
        bail!("account {uid} belongs to too many groups");
    }
    groups.truncate(usize::try_from(count)?);
    if !groups.contains(&device_trust) {
        bail!(
            "account {uid} is not in the group of {}, so it keeps no peers there",
            peers.display()
        );
    }
    Ok(device_trust)
}

/// No-follow, owner-checked file access for the uid this process runs as.
pub mod local {
    use super::{CStr, CString, HomeFile, OwnedFd, Path};
    #[cfg(test)]
    use super::{FLEETS_DIR, MAX_FILE_BYTES, MAX_ROOMS_BYTES};
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{AsRawFd as _, FromRawFd as _};
    use std::os::unix::ffi::OsStrExt as _;

    pub fn own_uid() -> libc::uid_t {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() }
    }

    fn invalid(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message.into())
    }

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("path contains NUL"))
    }

    fn c_name(name: &str) -> io::Result<CString> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(invalid(format!("{name:?} is not a single file name")));
        }
        CString::new(name).map_err(|_| invalid("name contains NUL"))
    }

    pub fn fstat(fd: &OwnedFd) -> io::Result<libc::stat> {
        // SAFETY: an all-zero stat is a valid out-parameter.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open and `stat` is writable.
        if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(stat)
    }

    fn checked(
        fd: OwnedFd,
        kind: libc::mode_t,
        owner: libc::uid_t,
        what: &str,
    ) -> io::Result<OwnedFd> {
        let stat = fstat(&fd)?;
        if stat.st_mode & libc::S_IFMT != kind {
            return Err(invalid(format!("{what} is not a {}", kind_name(kind))));
        }
        if stat.st_uid != owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{what} belongs to uid {}, not {owner}", stat.st_uid),
            ));
        }
        Ok(fd)
    }

    const fn kind_name(kind: libc::mode_t) -> &'static str {
        if kind == libc::S_IFDIR {
            "directory"
        } else {
            "regular file"
        }
    }

    fn from_raw(fd: libc::c_int) -> io::Result<OwnedFd> {
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` was just returned open by the kernel and is owned here.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    const DIR_FLAGS: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    const READ_FLAGS: libc::c_int =
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC;

    /// Open the directory at `path` without following a final link; it must
    /// belong to `owner`.
    pub fn open_owned_dir(path: &Path, owner: libc::uid_t) -> io::Result<OwnedFd> {
        let c = c_path(path)?;
        // SAFETY: `c` is NUL-terminated.
        let fd = from_raw(unsafe { libc::open(c.as_ptr(), DIR_FLAGS) })?;
        checked(fd, libc::S_IFDIR, owner, &path.display().to_string())
    }

    /// Open the directory at `path` without following a final link, whoever
    /// owns it: a shared parent such as a group-writable staging area, whose
    /// entries are then each checked.
    pub fn open_dir(path: &Path) -> io::Result<OwnedFd> {
        let c = c_path(path)?;
        // SAFETY: `c` is NUL-terminated.
        let fd = from_raw(unsafe { libc::open(c.as_ptr(), DIR_FLAGS) })?;
        let stat = fstat(&fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(invalid(format!("{} is not a directory", path.display())));
        }
        Ok(fd)
    }

    /// Open the directory at `path` only to reach entries by name in it,
    /// without following a final link and without needing to list it: a
    /// directory the group may search and write but not read.
    pub fn open_search_dir(path: &Path) -> io::Result<OwnedFd> {
        let c = c_path(path)?;
        // SAFETY: `c` is NUL-terminated.
        let fd = from_raw(unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        let stat = fstat(&fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(invalid(format!("{} is not a directory", path.display())));
        }
        Ok(fd)
    }

    /// Open `name` inside `dir` as a directory without following a link; it
    /// must belong to `owner`. When it does not exist it is created with
    /// `create`'s mode, or `None` is returned without one.
    pub fn open_owned_subdir(
        dir: &OwnedFd,
        name: &str,
        owner: libc::uid_t,
        create: Option<libc::mode_t>,
    ) -> io::Result<Option<OwnedFd>> {
        let c = c_name(name)?;
        loop {
            // SAFETY: `dir` is open and `c` is NUL-terminated.
            let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), DIR_FLAGS) };
            match from_raw(fd) {
                Ok(fd) => return checked(fd, libc::S_IFDIR, owner, name).map(Some),
                Err(error) if error.kind() == io::ErrorKind::NotFound && create.is_some() => {
                    let mode = create.unwrap_or(0o700);
                    // SAFETY: `dir` is open and `c` is NUL-terminated.
                    if unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode) } != 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(error);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            }
        }
    }

    /// Create `name` inside `dir` as a new file of this account holding
    /// `bytes`, with exactly `mode`. `false` when anything already holds the
    /// name: an existing entry is never taken over or followed.
    pub fn create_fresh_file(
        dir: &OwnedFd,
        name: &str,
        owner: libc::uid_t,
        bytes: &[u8],
        mode: libc::mode_t,
    ) -> io::Result<bool> {
        let c = c_name(name)?;
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: `dir` is open and `c` is NUL-terminated.
        let fd = match from_raw(unsafe {
            libc::openat(dir.as_raw_fd(), c.as_ptr(), flags, libc::c_uint::from(mode))
        }) {
            Ok(fd) => fd,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error),
        };
        let fd = checked(fd, libc::S_IFREG, owner, name)?;
        // SAFETY: `fd` is open.
        if unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = std::fs::File::from(fd);
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(true)
    }

    /// Create `name` inside `dir` as a new directory of this account with
    /// exactly `mode`, and open it without following a link. `None` when
    /// anything already holds the name: an existing entry is never taken
    /// over.
    pub fn create_fresh_subdir(
        dir: &OwnedFd,
        name: &str,
        owner: libc::uid_t,
        mode: libc::mode_t,
    ) -> io::Result<Option<OwnedFd>> {
        let c = c_name(name)?;
        // SAFETY: `dir` is open and `c` is NUL-terminated.
        if unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::AlreadyExists {
                return Ok(None);
            }
            return Err(error);
        }
        // SAFETY: `dir` is open and `c` is NUL-terminated.
        let fd = from_raw(unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), DIR_FLAGS) })?;
        let fd = checked(fd, libc::S_IFDIR, owner, name)?;
        // The umask may have narrowed it; nothing may have widened it.
        // SAFETY: `fd` is open.
        if unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(fd))
    }

    /// Read `name` inside `dir`: a regular file belonging to `owner`, opened
    /// without following a link or blocking on a FIFO, at most `limit`
    /// bytes. `None` when it does not exist.
    pub fn read_owned_file(
        dir: &OwnedFd,
        name: &str,
        owner: libc::uid_t,
        limit: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        let c = c_name(name)?;
        // SAFETY: `dir` is open and `c` is NUL-terminated.
        let fd = match from_raw(unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), READ_FLAGS) }) {
            Ok(fd) => fd,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let fd = checked(fd, libc::S_IFREG, owner, name)?;
        // A second name for the file may be one another account linked to
        // something this account owns; only a file with one name is read.
        if fstat(&fd)?.st_nlink != 1 {
            return Err(invalid(format!("{name} has more than one name")));
        }
        let mut bytes = Vec::new();
        std::fs::File::from(fd)
            .take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(invalid(format!("{name} exceeds {limit} bytes")));
        }
        Ok(Some(bytes))
    }

    fn stat_at(dir: &OwnedFd, name: &CStr) -> io::Result<Option<libc::stat>> {
        // SAFETY: an all-zero stat is a valid out-parameter.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `dir` is open, `name` NUL-terminated, `stat` writable.
        let status = unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &raw mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if status == 0 {
            return Ok(Some(stat));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        }
    }

    /// Atomically replace `name` inside `dir` with `bytes`, mode `mode`. An
    /// existing entry must be a regular file belonging to `owner`, and its
    /// group carries over where this account may use it. The new file is
    /// created exclusively inside `dir` and renamed over `name` there, so
    /// neither step resolves a link.
    pub fn replace_owned_file(
        dir: &OwnedFd,
        name: &str,
        bytes: &[u8],
        owner: libc::uid_t,
        mode: libc::mode_t,
    ) -> io::Result<()> {
        replace_entry(dir, name, bytes, Some(owner), mode)
    }

    /// Atomically replace `name` inside a directory several accounts share,
    /// whoever wrote the previous file: as [`replace_owned_file`], but an
    /// existing regular file of any owner is replaced as a directory entry.
    /// A link or anything else in its place is refused.
    pub fn replace_shared_file(
        dir: &OwnedFd,
        name: &str,
        bytes: &[u8],
        mode: libc::mode_t,
    ) -> io::Result<()> {
        replace_entry(dir, name, bytes, None, mode)
    }

    /// Open `name` inside a shared directory `dir` as a directory without
    /// following a link, whoever owns it. With `create`, a missing directory
    /// is created and, when this account owns it, given that mode (set-group-
    /// ID included) whatever the umask; the mode comes from the caller's
    /// trusted parent, never from a directory another account controls.
    /// Without it the directory must exist and keeps its mode. A directory
    /// anyone may write is refused.
    pub fn open_shared_subdir(
        dir: &OwnedFd,
        name: &str,
        create: Option<libc::mode_t>,
    ) -> io::Result<OwnedFd> {
        let c = c_name(name)?;
        let create = create.map(|mode| mode & 0o7777 & !0o002);
        if let Some(mode) = create {
            // SAFETY: `dir` is open and `c` is NUL-terminated.
            unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode & 0o777) };
        }
        // SAFETY: `dir` is open and `c` is NUL-terminated.
        let fd = from_raw(unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), DIR_FLAGS) })?;
        let stat = fstat(&fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(invalid(format!("{name} is not a directory")));
        }
        if stat.st_mode & 0o002 != 0 {
            return Err(invalid(format!("{name} is writable by every account")));
        }
        // Another account's directory swapped in under this name keeps its
        // mode; only this account's own is made reachable like its parent.
        if let Some(mode) = create
            && stat.st_uid == own_uid()
            && stat.st_mode & 0o7777 != mode
        {
            // SAFETY: `fd` is open.
            if unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(fd)
    }

    fn replace_entry(
        dir: &OwnedFd,
        name: &str,
        bytes: &[u8],
        owner: Option<libc::uid_t>,
        mode: libc::mode_t,
    ) -> io::Result<()> {
        let target = c_name(name)?;
        let existing = stat_at(dir, &target)?;
        if let Some(stat) = &existing
            && (stat.st_mode & libc::S_IFMT != libc::S_IFREG
                || owner.is_some_and(|owner| stat.st_uid != owner))
        {
            return Err(invalid(format!(
                "{name} is not a regular file this account may replace"
            )));
        }
        let mode = mode & 0o777;
        let mut random = [0_u8; 8];
        getrandom::rand_core::Rng::fill_bytes(
            &mut getrandom::rand_core::UnwrapErr(getrandom::SysRng),
            &mut random,
        );
        let temp = c_name(&format!(".{name}.{}.tmp", hex::encode(random)))?;
        // SAFETY: `dir` is open and `temp` is NUL-terminated.
        let fd = from_raw(unsafe {
            libc::openat(
                dir.as_raw_fd(),
                temp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                libc::c_uint::from(mode),
            )
        })?;
        let result = (|| {
            if let Some(stat) = &existing {
                // A group this account cannot use is left as the new file's
                // default; the replacement never depends on it.
                // SAFETY: `fd` is open.
                let _ = unsafe { libc::fchown(fd.as_raw_fd(), u32::MAX, stat.st_gid) };
            }
            // The exact mode, whatever the umask.
            // SAFETY: `fd` is open.
            if unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut file = std::fs::File::from(fd);
            file.write_all(bytes)?;
            file.sync_all()?;
            // SAFETY: `dir` is open and both names are NUL-terminated.
            if unsafe {
                libc::renameat(
                    dir.as_raw_fd(),
                    temp.as_ptr(),
                    dir.as_raw_fd(),
                    target.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `dir` is open. A publication receipt must not hide a
            // durability failure after the file's directory entry changed.
            if unsafe { libc::fsync(dir.as_raw_fd()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        })();
        if result.is_err() {
            // SAFETY: `dir` is open and `temp` is NUL-terminated.
            unsafe { libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0) };
        }
        result
    }

    /// `dir`, opened and owned by `owner`; `None` when it does not exist.
    fn existing_owned_dir(dir: &Path, owner: libc::uid_t) -> io::Result<Option<OwnedFd>> {
        match open_owned_dir(dir, owner) {
            Ok(fd) => Ok(Some(fd)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The bytes of `file` under `dir`, read as `owner`; `None` when the file
    /// or a directory on the way does not exist.
    pub fn read_file(
        dir: &Path,
        file: HomeFile<'_>,
        owner: libc::uid_t,
        limit: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        let Some(dir) = existing_owned_dir(dir, owner)? else {
            return Ok(None);
        };
        read_file_at(&dir, file, owner, limit)
    }

    /// The bytes of `file` under the already-checked directory `dir`.
    pub fn read_file_at(
        dir: &OwnedFd,
        file: HomeFile<'_>,
        owner: libc::uid_t,
        limit: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        match file.subdir {
            None => read_owned_file(dir, file.name, owner, limit),
            Some(subdir) => match open_owned_subdir(dir, subdir, owner, None)? {
                Some(sub) => read_owned_file(&sub, file.name, owner, limit),
                None => Ok(None),
            },
        }
    }

    /// Replace `file` under `dir` as `owner`. With `create_dir`, `dir` and
    /// its missing parents are created first, private to `owner`.
    pub fn write_file(
        dir: &Path,
        file: HomeFile<'_>,
        bytes: &[u8],
        owner: libc::uid_t,
        mode: libc::mode_t,
        create_dir: bool,
    ) -> io::Result<()> {
        if create_dir {
            if owner == 0 {
                // Root acting for itself creates only the last component,
                // inside a parent it owns, never through a linked ancestor.
                let (Some(parent), Some(name)) =
                    (dir.parent(), dir.file_name().and_then(|name| name.to_str()))
                else {
                    return Err(invalid("directory has no parent"));
                };
                let parent = open_owned_dir(parent, owner)?;
                open_owned_subdir(&parent, name, owner, Some(0o700))?;
            } else {
                use std::os::unix::fs::DirBuilderExt as _;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
            }
        }
        let dir = open_owned_dir(dir, owner)?;
        write_file_at(&dir, file, bytes, owner, mode)
    }

    /// Replace `file` under the already-checked directory `dir`.
    pub fn write_file_at(
        dir: &OwnedFd,
        file: HomeFile<'_>,
        bytes: &[u8],
        owner: libc::uid_t,
        mode: libc::mode_t,
    ) -> io::Result<()> {
        match file.subdir {
            None => replace_owned_file(dir, file.name, bytes, owner, mode),
            Some(subdir) => {
                let sub = open_owned_subdir(dir, subdir, owner, Some(0o755))?
                    .ok_or_else(|| invalid(format!("{subdir} could not be created")))?;
                replace_owned_file(&sub, file.name, bytes, owner, mode)
            }
        }
    }

    #[cfg(test)]
    fn fleet(name: &str) -> HomeFile<'_> {
        HomeFile {
            subdir: Some(FLEETS_DIR),
            name,
        }
    }

    #[cfg(test)]
    /// The fleet of `user` in `identity_dir`, read as `owner`.
    pub fn read_fleet(
        identity_dir: &Path,
        user: [u8; 32],
        owner: libc::uid_t,
    ) -> io::Result<Option<Vec<u8>>> {
        let name = super::fleet_name(user);
        read_file(identity_dir, fleet(&name), owner, MAX_FILE_BYTES)
    }

    #[cfg(test)]
    /// The fleet of `user` in the already-checked identity directory `dir`.
    pub fn read_fleet_at(
        dir: &OwnedFd,
        user: [u8; 32],
        owner: libc::uid_t,
    ) -> io::Result<Option<Vec<u8>>> {
        let name = super::fleet_name(user);
        read_file_at(dir, fleet(&name), owner, MAX_FILE_BYTES)
    }

    #[cfg(test)]
    /// Replace the fleet of `user` in `identity_dir`, as `owner`.
    pub fn write_fleet(
        identity_dir: &Path,
        user: [u8; 32],
        bytes: &[u8],
        owner: libc::uid_t,
    ) -> io::Result<()> {
        let name = super::fleet_name(user);
        write_file(identity_dir, fleet(&name), bytes, owner, 0o644, false)
    }

    #[cfg(test)]
    /// Replace the fleet of `user` in the already-checked identity directory.
    pub fn write_fleet_at(
        dir: &OwnedFd,
        user: [u8; 32],
        bytes: &[u8],
        owner: libc::uid_t,
    ) -> io::Result<()> {
        let name = super::fleet_name(user);
        write_file_at(dir, fleet(&name), bytes, owner, 0o644)
    }

    #[cfg(test)]
    /// The exported room list at `path`, read as `owner`: its directory and
    /// the file both belong to `owner` and neither is a link.
    pub fn read_rooms(path: &Path, owner: libc::uid_t) -> io::Result<Option<Vec<u8>>> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str()))
        else {
            return Err(invalid("room list path has no directory and file name"));
        };
        read_file(parent, HomeFile::at(name), owner, MAX_ROOMS_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::local::*;
    use super::*;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, symlink};

    const USER: [u8; 32] = [7; 32];

    fn fleet_file(identity: &Path) -> PathBuf {
        identity
            .join(FLEETS_DIR)
            .join(format!("{}.json", hex::encode(USER)))
    }

    fn me() -> libc::uid_t {
        own_uid()
    }

    #[test]
    fn root_only_ever_reaches_the_home_as_the_named_person() {
        let gid = |_| Ok(4242);
        assert_eq!(
            PersonHome::for_process(0, Some(1000), gid).unwrap(),
            PersonHome::Delegated {
                program: PathBuf::from("/proc/self/exe"),
                person: Person {
                    uid: 1000,
                    gid: 4242
                },
            }
        );
        assert_eq!(
            PersonHome::for_process(0, None, gid).unwrap(),
            PersonHome::Direct
        );
        assert_eq!(
            PersonHome::for_process(0, Some(0), gid).unwrap(),
            PersonHome::Direct
        );
        assert!(PersonHome::for_process(0, Some(1000), |_| bail!("no account")).is_err());
        assert_eq!(
            PersonHome::for_process(1000, Some(1000), gid).unwrap(),
            PersonHome::Direct
        );
        assert_eq!(
            PersonHome::for_process(1000, None, gid).unwrap(),
            PersonHome::Direct
        );
        assert!(PersonHome::for_process(1000, Some(1001), gid).is_err());
    }

    #[test]
    fn the_child_runs_this_binary_with_only_its_operation() {
        let name = fleet_name(USER);
        let args = child_args(&ChildRequest::write(
            Path::new("/home/p/.local/share/tilo/identities"),
            HomeFile {
                subdir: Some(FLEETS_DIR),
                name: &name,
            },
            0o644,
            false,
        ));
        let command = child_command(
            Path::new("/proc/self/exe"),
            Person {
                uid: 1000,
                gid: 1000,
            },
            None,
            &args,
        );
        assert_eq!(command.get_program(), "/proc/self/exe");
        let shown: Vec<_> = command.get_args().collect();
        assert_eq!(
            shown,
            [
                CHILD_COMMAND,
                "write",
                "--path",
                "/home/p/.local/share/tilo/identities",
                "--name",
                &name,
                "--mode",
                "644",
                "--limit",
                &MAX_FILE_BYTES.to_string(),
                "--subdir",
                FLEETS_DIR,
            ]
        );
        assert_eq!(command.get_current_dir(), Some(Path::new("/")));
    }

    #[test]
    fn child_answers_round_trip_and_stay_bounded() {
        assert_eq!(decode_read(&encode_read(None), 4).unwrap(), None);
        assert_eq!(
            decode_read(&encode_read(Some(b"abc")), 4).unwrap(),
            Some(b"abc".to_vec())
        );
        assert_eq!(
            decode_read(&encode_read(Some(b"")), 4).unwrap(),
            Some(Vec::new())
        );
        assert!(decode_read(&encode_read(Some(b"abcde")), 4).is_err());
        assert!(decode_read(b"", 4).is_err());
        assert!(decode_read(b"0junk", 4).is_err());
        assert!(decode_read(b"x", 4).is_err());
    }

    #[test]
    fn fleet_reads_and_writes_stay_inside_the_person_owned_directory() {
        let scope = tempfile::tempdir().unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir(&identity).unwrap();
        assert_eq!(read_fleet(&identity, USER, me()).unwrap(), None);
        write_fleet(&identity, USER, b"{\"v\":1}", me()).unwrap();
        assert_eq!(
            read_fleet(&identity, USER, me()).unwrap().unwrap(),
            b"{\"v\":1}"
        );
        write_fleet(&identity, USER, b"{\"v\":2}", me()).unwrap();
        assert_eq!(std::fs::read(fleet_file(&identity)).unwrap(), b"{\"v\":2}");
        // No temporary file is left behind.
        assert_eq!(
            std::fs::read_dir(identity.join(FLEETS_DIR))
                .unwrap()
                .count(),
            1
        );
        // Another account's directory is refused outright.
        let other = me().wrapping_add(1);
        assert!(read_fleet(&identity, USER, other).is_err());
        assert!(write_fleet(&identity, USER, b"x", other).is_err());
        assert_eq!(std::fs::read(fleet_file(&identity)).unwrap(), b"{\"v\":2}");
    }

    #[test]
    fn a_symlinked_identity_directory_is_never_followed() {
        let scope = tempfile::tempdir().unwrap();
        let elsewhere = scope.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        let identity = scope.path().join("identities");
        symlink(&elsewhere, &identity).unwrap();

        assert!(read_fleet(&identity, USER, me()).is_err());
        assert!(write_fleet(&identity, USER, b"planted", me()).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_fleet_directory_is_never_followed() {
        let scope = tempfile::tempdir().unwrap();
        let elsewhere = scope.path().join("etc");
        std::fs::create_dir(&elsewhere).unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir(&identity).unwrap();
        symlink(&elsewhere, identity.join(FLEETS_DIR)).unwrap();

        assert!(read_fleet(&identity, USER, me()).is_err());
        assert!(write_fleet(&identity, USER, b"planted", me()).is_err());
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn a_symlinked_or_hardlinked_fleet_file_is_never_written_through() {
        let scope = tempfile::tempdir().unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir_all(identity.join(FLEETS_DIR)).unwrap();
        let target = scope.path().join("shadow");
        std::fs::write(&target, b"root:secret").unwrap();

        symlink(&target, fleet_file(&identity)).unwrap();
        assert!(read_fleet(&identity, USER, me()).is_err());
        assert!(write_fleet(&identity, USER, b"planted", me()).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"root:secret");
        assert!(
            std::fs::symlink_metadata(fleet_file(&identity))
                .unwrap()
                .file_type()
                .is_symlink()
        );

        // A hard link is replaced as a directory entry; the other name keeps
        // its bytes.
        std::fs::remove_file(fleet_file(&identity)).unwrap();
        std::fs::hard_link(&target, fleet_file(&identity)).unwrap();
        write_fleet(&identity, USER, b"fleet", me()).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"root:secret");
        assert_eq!(std::fs::read(fleet_file(&identity)).unwrap(), b"fleet");
        assert_eq!(std::fs::metadata(&target).unwrap().nlink(), 1);
    }

    #[test]
    fn a_directory_swapped_for_a_link_after_the_check_does_not_redirect_the_write() {
        let scope = tempfile::tempdir().unwrap();
        let identity = scope.path().join("identities");
        std::fs::create_dir_all(identity.join(FLEETS_DIR)).unwrap();
        let elsewhere = scope.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join(FLEETS_DIR)).unwrap();

        let checked = open_owned_dir(&identity, me()).unwrap();
        let moved = scope.path().join("moved");
        std::fs::rename(&identity, &moved).unwrap();
        symlink(&elsewhere, &identity).unwrap();

        write_fleet_at(&checked, USER, b"fleet", me()).unwrap();
        assert_eq!(std::fs::read(fleet_file(&moved)).unwrap(), b"fleet");
        assert_eq!(
            std::fs::read_dir(elsewhere.join(FLEETS_DIR))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            read_fleet_at(&checked, USER, me()).unwrap().unwrap(),
            b"fleet"
        );
    }

    #[test]
    fn a_planted_room_export_is_not_read() {
        let scope = tempfile::tempdir().unwrap();
        let state = scope.path().join("state/tilo");
        std::fs::create_dir_all(&state).unwrap();
        let rooms = state.join("personal-rooms.json");
        assert_eq!(read_rooms(&rooms, me()).unwrap(), None);
        assert_eq!(
            read_rooms(&scope.path().join("missing/personal-rooms.json"), me()).unwrap(),
            None
        );

        std::fs::write(&rooms, b"[]").unwrap();
        assert_eq!(read_rooms(&rooms, me()).unwrap().unwrap(), b"[]");
        assert!(read_rooms(&rooms, me().wrapping_add(1)).is_err());

        // A link to a file only root may read.
        let secret = scope.path().join("secret.json");
        std::fs::write(&secret, b"[\"secret\"]").unwrap();
        std::fs::remove_file(&rooms).unwrap();
        symlink(&secret, &rooms).unwrap();
        assert!(read_rooms(&rooms, me()).is_err());

        // A FIFO is refused without blocking.
        std::fs::remove_file(&rooms).unwrap();
        let fifo = CString::new(rooms.as_os_str().as_bytes()).unwrap();
        // SAFETY: `fifo` is NUL-terminated.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(read_rooms(&rooms, me()).is_err());

        // A linked directory is refused as well.
        let elsewhere = scope.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("personal-rooms.json"), b"[]").unwrap();
        let linked = scope.path().join("linked");
        symlink(&elsewhere, &linked).unwrap();
        assert!(read_rooms(&linked.join("personal-rooms.json"), me()).is_err());
    }

    #[test]
    fn fleet_transaction_locks_snapshot_through_publication() {
        let scope = tempfile::tempdir().unwrap();
        let home = PersonHome::Direct;
        home.write_fleet(scope.path(), USER, b"old").unwrap();
        home.with_fleet(scope.path(), USER, |snapshot, publish| {
            assert_eq!(snapshot.as_deref(), Some(b"old".as_slice()));
            let competitor = open_owned_dir(&scope.path().join(FLEETS_DIR), me())?;
            assert!(matches!(
                advisory_lock::FileLock::try_exclusive(std::fs::File::from(
                    competitor.try_clone()?
                )),
                Err(advisory_lock::TryLockError::WouldBlock)
            ));
            publish(b"new")?;
            assert_eq!(std::fs::read(fleet_file(scope.path()))?, b"new");
            // Publication must not have released the transaction lock.
            assert!(matches!(
                advisory_lock::FileLock::try_exclusive(std::fs::File::from(competitor)),
                Err(advisory_lock::TryLockError::WouldBlock)
            ));
            Ok(())
        })
        .unwrap();
        let next = open_owned_dir(&scope.path().join(FLEETS_DIR), me()).unwrap();
        // The preceding transaction has ended: another holder succeeds.
        let _next_lock = advisory_lock::FileLock::try_exclusive(std::fs::File::from(next)).unwrap();
    }

    #[test]
    fn fleet_transaction_keeps_the_checked_descriptor_and_refuses_linked_snapshots() {
        let scope = tempfile::tempdir().unwrap();
        let identity = scope.path().join("identity");
        let moved = scope.path().join("moved");
        let elsewhere = scope.path().join("elsewhere");
        std::fs::create_dir(&identity).unwrap();
        std::fs::create_dir(&elsewhere).unwrap();
        let home = PersonHome::Direct;
        home.write_fleet(&identity, USER, b"old").unwrap();
        home.with_fleet(&identity, USER, |snapshot, publish| {
            assert_eq!(snapshot.as_deref(), Some(b"old".as_slice()));
            std::fs::rename(&identity, &moved)?;
            symlink(&elsewhere, &identity)?;
            publish(b"new")
        })
        .unwrap();
        assert_eq!(std::fs::read(fleet_file(&moved)).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
        assert!(
            home.with_fleet(&identity, USER, |_, _| -> anyhow::Result<()> {
                bail!("must not run")
            })
            .is_err()
        );
        let target = scope.path().join("target");
        std::fs::hard_link(fleet_file(&moved), &target).unwrap();
        let error = home
            .with_fleet(&moved, USER, |_, _| -> anyhow::Result<()> {
                panic!("a linked snapshot must be refused before redemption")
            })
            .unwrap_err();
        assert!(error.to_string().contains("more than one name"));
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn fleet_child_protocol_acknowledges_only_after_publication() {
        let scope = tempfile::tempdir().unwrap();
        let request = ChildRequest {
            operation: ChildOperation::FleetTransaction,
            path: scope.path().to_path_buf(),
            subdir: None,
            name: fleet_name(USER),
            mode: 0o644,
            create_dir: false,
            limit: MAX_FILE_BYTES,
        };
        let (mut parent, mut child) = std::os::unix::net::UnixStream::pair().unwrap();
        parent.set_read_timeout(Some(CHILD_TIMEOUT)).unwrap();
        parent.set_write_timeout(Some(CHILD_TIMEOUT)).unwrap();
        let worker = std::thread::spawn(move || {
            serve_fleet_transaction(&request, &mut child.try_clone().unwrap(), &mut child)
        });
        assert_eq!(
            decode_read(&read_frame(&mut parent).unwrap(), MAX_FILE_BYTES).unwrap(),
            None
        );
        assert!(!fleet_file(scope.path()).exists());
        write_frame(&mut parent, b"published").unwrap();
        assert!(read_frame(&mut parent).unwrap().is_empty());
        assert_eq!(
            std::fs::read(fleet_file(scope.path())).unwrap(),
            b"published"
        );
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn disconnected_parent_releases_the_fleet_without_publication() {
        let scope = tempfile::tempdir().unwrap();
        let request = ChildRequest {
            operation: ChildOperation::FleetTransaction,
            path: scope.path().to_path_buf(),
            subdir: None,
            name: fleet_name(USER),
            mode: 0o644,
            create_dir: false,
            limit: MAX_FILE_BYTES,
        };
        let (mut parent, mut child) = std::os::unix::net::UnixStream::pair().unwrap();
        parent.set_read_timeout(Some(CHILD_TIMEOUT)).unwrap();
        let worker = std::thread::spawn(move || {
            serve_fleet_transaction(&request, &mut child.try_clone().unwrap(), &mut child)
        });
        read_frame(&mut parent).unwrap();
        drop(parent);
        assert!(worker.join().unwrap().is_err());
        assert!(!fleet_file(scope.path()).exists());
        PersonHome::Direct
            .with_fleet(scope.path(), USER, |snapshot, publish| {
                assert!(snapshot.is_none());
                publish(b"retry")
            })
            .unwrap();
        assert_eq!(std::fs::read(fleet_file(scope.path())).unwrap(), b"retry");
    }

    #[test]
    fn fleet_frames_and_total_channel_deadline_are_bounded() {
        use std::io::{Read as _, Write as _};
        let mut oversized = u32::try_from(MAX_FILE_BYTES + 2)
            .unwrap()
            .to_be_bytes()
            .as_slice()
            .to_vec();
        assert!(read_frame(&mut oversized.as_slice()).is_err());
        assert!(write_frame(&mut oversized, &vec![0; MAX_FILE_BYTES + 2]).is_err());
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut channel = FleetChannel {
            socket,
            deadline: std::time::Instant::now(),
        };
        assert_eq!(
            channel.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert_eq!(
            channel.write(b"x").unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn direct_access_reads_and_writes_the_fleet_within_bounds() {
        let scope = tempfile::tempdir().unwrap();
        let home = PersonHome::Direct;
        assert_eq!(home.read_fleet(scope.path(), USER).unwrap(), None);
        home.write_fleet(scope.path(), USER, b"[]").unwrap();
        assert_eq!(home.read_fleet(scope.path(), USER).unwrap().unwrap(), b"[]");
        assert!(
            home.write_fleet(scope.path(), USER, &vec![0; MAX_FILE_BYTES + 1])
                .is_err()
        );
    }
}
