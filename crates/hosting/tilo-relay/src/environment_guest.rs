//! Guests for relay environment sessions, built the way a device builds
//! them: by asking a `ush` supervisor (`tilo-ush serve`) for a session and
//! receiving its pty master.
//!
//! # Isolation between members
//!
//! Every member gets a supervisor of their own, started on their first
//! session and stopped when their last one ends ([`UshGuestLauncher`]).
//! What separates two members on one worker:
//!
//! - **Files.** Each supervisor sees only its member's area
//!   ([`MemberArea`]): its layer store, the homes, and the member's
//!   relay-side files that granted directories map onto. Area directories
//!   are `0700`.
//! - **Uid**, when the worker runs as root and is given a uid range
//!   ([`MemberIsolation::SeparateUids`]): each member's supervisor, and so
//!   every guest it builds, runs as a uid of its own, persisted per member,
//!   and owns its area. Without a range all members share the worker's uid
//!   and the per-guest confinement below is what separates them.
//! - **Namespaces, seccomp, Landlock**, per guest, exactly as on a device:
//!   a guest is a user + mount + pid namespace whose filesystem is built
//!   from the member's area alone, with `ush`'s seccomp filter and Landlock
//!   ruleset.
//! - **CPU, memory, processes**, when the worker is given a delegated
//!   cgroup v2 subtree ([`UshLauncherConfig::cgroup_root`]): one cgroup per
//!   member holding their supervisor and all its guests, with `cpu.max`,
//!   `memory.max` and `pids.max` from the member's quota. Without one these
//!   are not enforced, and the worker logs that once at startup.
//! - **Disk**: the host refuses new sessions once the member's area exceeds
//!   its quota. A hard limit needs a filesystem quota on the area (an XFS
//!   project per member), which is the deployment's to configure.
//!
//! # The guest
//!
//! The worker writes the member's environment, as their device resolved it,
//! as a record in the member's own record directory, and asks the member's
//! supervisor for a session on it. The supervisor builds the guest exactly
//! as it does on a device: images materialised through the package store
//! (fetched from the registry when missing, shared by content address), the
//! environment's home and `/var`, its grants. A tool the member installs in
//! the session is built there, on the worker, and lands in that record; the
//! host reports it to the member's terminal ([`GuestLauncher::current_tools`]),
//! whose device records it.
//!
//! # Granted directories
//!
//! The relay does not hold a member's files as a browsable tree; their
//! synced content lives with their devices and the content plane. A grant
//! (`/home/alice/Projects`) therefore maps to the same path below the
//! member's relay-side file area (`<area>/files/home/alice/Projects`),
//! created on first use and kept between sessions, and the supervisor binds
//! it into the guest at the path the member knows it by
//! (`TILO_USH_GRANT_ROOT`).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};
use std::time::{Duration, Instant};

use tilo_relay_protocol::environment::{
    EnvironmentRecordRef, EnvironmentSpec, MemberIdentity, PtySize, ToolImage,
};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::environment_host::{
    Guest, GuestControl, GuestLauncher, GuestRequest, LaunchError, MemberArea, MemberContext,
    MemberQuota,
};

/// How members are separated by uid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberIsolation {
    /// Every member's supervisor runs as the worker's own uid.
    SharedUid,
    /// Each member gets a uid of their own from `first..first + count`.
    /// Requires the worker to run as root.
    SeparateUids {
        /// First uid of the range.
        first: u32,
        /// Uids in the range.
        count: u32,
    },
}

/// How the worker starts members' supervisors.
#[derive(Clone, Debug)]
pub struct UshLauncherConfig {
    /// Root of the members' areas; the same as the host's.
    pub root: PathBuf,
    /// The `tilo-ush` binary.
    pub ush_binary: PathBuf,
    /// The base the guests shadow (`/`).
    pub base: PathBuf,
    /// Uid separation.
    pub isolation: MemberIsolation,
    /// A delegated cgroup v2 directory the worker may create children in.
    pub cgroup_root: Option<PathBuf>,
    /// How long a supervisor may take to listen.
    pub start_timeout: Duration,
}

impl UshLauncherConfig {
    /// Defaults for members' areas under `root`.
    #[must_use]
    pub fn at(root: PathBuf) -> Self {
        Self {
            root,
            ush_binary: PathBuf::from("/usr/bin/tilo-ush"),
            base: PathBuf::from("/"),
            isolation: MemberIsolation::SharedUid,
            cgroup_root: None,
            start_timeout: Duration::from_secs(10),
        }
    }
}

/// Starts each member's guests through that member's own `ush` supervisor.
pub struct UshGuestLauncher {
    config: UshLauncherConfig,
    supervisors: Mutex<BTreeMap<[u8; 32], Supervisor>>,
}

struct Supervisor {
    socket: PathBuf,
    child: Child,
    cgroup: Option<PathBuf>,
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(cgroup) = &self.cgroup {
            let _ = std::fs::remove_dir(cgroup);
        }
    }
}

impl UshGuestLauncher {
    /// A launcher with `config`.
    #[must_use]
    pub fn new(config: UshLauncherConfig) -> Self {
        if config.cgroup_root.is_none() {
            tracing::warn!(
                "no delegated cgroup: members' CPU, memory and process quotas are not enforced"
            );
        }
        Self {
            config,
            supervisors: Mutex::new(BTreeMap::new()),
        }
    }

    fn uid_for(&self, member: &MemberIdentity) -> Result<Option<u32>, LaunchError> {
        match self.config.isolation {
            MemberIsolation::SharedUid => Ok(None),
            MemberIsolation::SeparateUids { first, count } => {
                UidAllocator::open(self.config.root.join("uids.json"), first, count)
                    .allocate(&member.user)
                    .map(Some)
                    .map_err(|error| LaunchError::Failed(format!("member uid: {error}")))
            }
        }
    }

    fn supervisor_socket(&self, member: &MemberContext) -> Result<PathBuf, LaunchError> {
        let mut supervisors = self
            .supervisors
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(supervisor) = supervisors.get_mut(&member.identity.user) {
            if matches!(supervisor.child.try_wait(), Ok(None)) {
                return Ok(supervisor.socket.clone());
            }
            supervisors.remove(&member.identity.user);
        }
        let supervisor = self.start_supervisor(member)?;
        let socket = supervisor.socket.clone();
        supervisors.insert(member.identity.user, supervisor);
        drop(supervisors);
        Ok(socket)
    }

    fn start_supervisor(&self, member: &MemberContext) -> Result<Supervisor, LaunchError> {
        let failed = |what: &str, error: io::Error| LaunchError::Failed(format!("{what}: {error}"));
        let uid = self.uid_for(&member.identity)?;
        let area = &member.area;
        let home = area.root().join("home");
        for directory in [
            area.root().to_path_buf(),
            area.files(),
            area.store(),
            area.runtime(),
            area.records(),
            home.clone(),
        ] {
            private_directory(&directory, uid).map_err(|error| failed("member area", error))?;
        }
        let socket = area.runtime().join("ush.sock");
        let _ = std::fs::remove_file(&socket);
        let cgroup = self
            .config
            .cgroup_root
            .as_deref()
            .map(|root| member_cgroup(root, &member.identity, &member.quota))
            .transpose()
            .map_err(|error| failed("member cgroup", error))?;

        let mut command = Command::new(&self.config.ush_binary);
        command
            .arg("serve")
            .arg("--socket")
            .arg(&socket)
            .arg("--host-root")
            .arg(&self.config.base)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", area.runtime())
            .env(ush::store::STORE_ENV, area.store())
            .env(ush::store::RECORDS_ENV, area.records())
            .env(ush::store::GRANT_ROOT_ENV, area.files())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(uid) = uid {
            use std::os::unix::process::CommandExt;
            command.uid(uid).gid(uid);
            // SAFETY: `setgroups` is async-signal-safe and allocates nothing;
            // it drops the worker's supplementary groups before exec.
            unsafe {
                command.pre_exec(|| {
                    if libc::setgroups(0, std::ptr::null()) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = command
            .spawn()
            .map_err(|error| failed("start the member's supervisor", error))?;
        let supervisor = Supervisor {
            socket: socket.clone(),
            child,
            cgroup,
        };
        if let Some(cgroup) = &supervisor.cgroup {
            std::fs::write(
                cgroup.join("cgroup.procs"),
                supervisor.child.id().to_string(),
            )
            .map_err(|error| failed("place the supervisor in its cgroup", error))?;
        }
        let deadline = Instant::now()
            .checked_add(self.config.start_timeout)
            .unwrap_or_else(Instant::now);
        while UnixStream::connect(&socket).is_err() {
            if Instant::now() >= deadline {
                return Err(LaunchError::Failed(
                    "the member's supervisor did not start listening".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(supervisor)
    }
}

impl GuestLauncher for UshGuestLauncher {
    fn launch(&self, member: &MemberContext, request: &GuestRequest) -> Result<Guest, LaunchError> {
        let grants = grant_mounts(&member.area, &request.spec)
            .map_err(|error| LaunchError::Failed(format!("granted directories: {error}")))?;
        tracing::debug!(grants = grants.len(), "relay environment grants resolved");
        let uid = self.uid_for(&member.identity)?;
        let key = write_record(&member.area, &request.record, &request.spec, uid)?;
        let socket = self.supervisor_socket(member)?;
        open_ush_session(&socket, &key, request)
    }

    fn current_tools(
        &self,
        member: &MemberContext,
        record: &EnvironmentRecordRef,
    ) -> Option<Vec<ToolImage>> {
        let environment = ush::records::Records::at(member.area.records())
            .load(&record_key(record))
            .ok()??;
        Some(
            environment
                .tools
                .into_iter()
                .map(|tool| ToolImage {
                    id: tool.id,
                    packages: tool.packages,
                })
                .collect(),
        )
    }

    fn member_idle(&self, member: &MemberIdentity) {
        let stopped = self
            .supervisors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&member.user);
        drop(stopped);
    }
}

/// Ask a `ush` supervisor for a guest and wrap its pty master.
///
/// # Errors
///
/// Returns an error when the supervisor is unreachable, denies the
/// request, or sends no pty.
pub fn open_ush_session(
    socket: &Path,
    record_key: &str,
    request: &GuestRequest,
) -> Result<Guest, LaunchError> {
    let failed = |error: io::Error| LaunchError::Failed(format!("supervisor: {error}"));
    let mut stream = UnixStream::connect(socket).map_err(failed)?;
    let serve = ush::supervisor::ServeRequest::OpenSession {
        target: tilo_environment::Target::Environment {
            record: Some(record_key.to_owned()),
        },
        cols: request.size.cols,
        rows: request.size.rows,
        term: Some(request.term.clone()),
        command: Vec::new(),
    };
    let mut line = serde_json::to_vec(&serve)
        .map_err(|error| LaunchError::Failed(format!("encode the request: {error}")))?;
    line.push(b'\n');
    stream.write_all(&line).map_err(failed)?;
    let (reply, master) =
        runtime_portal::fd_transport::recv_with_owned_fd(stream.as_raw_fd()).map_err(failed)?;
    let reply = reply
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    let response: ush::supervisor::ServeResponse = serde_json::from_slice(reply)
        .map_err(|error| LaunchError::Failed(format!("supervisor reply: {error}")))?;
    if let ush::supervisor::ServeResponse::Denied { reason } = response {
        return Err(LaunchError::Failed(format!("supervisor denied: {reason}")));
    }
    let master = master.ok_or_else(|| LaunchError::Failed("supervisor sent no pty".into()))?;
    pty_guest(master, Some(stream)).map_err(failed)
}

/// The key a member's environment record has on the worker: one per
/// workspace and record, valid as a record key.
#[must_use]
pub fn record_key(record: &EnvironmentRecordRef) -> String {
    let digest = blake3::hash(format!("{}:{}", record.workspace_id, record.record_id).as_bytes());
    format!(
        "relay-{}",
        hex::encode(digest.as_bytes().get(..12).unwrap_or_default())
    )
}

/// The record a member's supervisor builds the guest from: the member's
/// environment as their device resolved it. Grants keep the paths the
/// member names them by; the supervisor finds their contents below the
/// member's file area. A grant of a path the guest builds itself is left
/// out.
#[must_use]
pub fn environment_record(spec: &EnvironmentSpec) -> tilo_environment::Environment {
    use tilo_relay_protocol::environment::HomePolicy;
    let mut environment = tilo_environment::Environment {
        name: spec.name.clone(),
        runtime: spec.runtime.clone(),
        tools: spec
            .tools
            .iter()
            .map(|tool| tilo_environment::ToolRef {
                id: tool.id.clone(),
                packages: tool.packages.clone(),
            })
            .collect(),
        home: match spec.home {
            HomePolicy::Persistent => tilo_environment::HomePolicy::Persistent,
            HomePolicy::Ephemeral => tilo_environment::HomePolicy::Ephemeral,
        },
        ..tilo_environment::Environment::default()
    };
    environment.grants.0 = spec
        .grants
        .iter()
        .filter_map(|grant| {
            tilo_environment::dir_grant(Path::new(&grant.path), grant.writable).ok()
        })
        .collect();
    environment
}

/// Write the member's environment as the record their supervisor reads,
/// replacing an earlier copy: the member's device holds the record, and
/// each session starts from what it resolved. Returns the record key.
///
/// # Errors
///
/// Returns an error when the record cannot be written.
pub fn write_record(
    area: &MemberArea,
    record: &EnvironmentRecordRef,
    spec: &EnvironmentSpec,
    owner: Option<u32>,
) -> Result<String, LaunchError> {
    let failed = |error: String| LaunchError::Failed(format!("environment record: {error}"));
    let key = record_key(record);
    let records = ush::records::Records::at(area.records());
    let directory = records
        .record_dir(&key)
        .map_err(|error| failed(error.to_string()))?;
    if directory.exists() {
        std::fs::remove_dir_all(&directory).map_err(|error| failed(error.to_string()))?;
    }
    records
        .create(&key, &environment_record(spec))
        .map_err(|error| failed(error.to_string()))?;
    if let Some(uid) = owner {
        chown_tree(&area.records(), uid).map_err(|error| failed(error.to_string()))?;
    }
    Ok(key)
}

fn chown_tree(path: &Path, uid: u32) -> io::Result<()> {
    std::os::unix::fs::lchown(path, Some(uid), Some(uid))?;
    if std::fs::symlink_metadata(path)?.is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree(&entry?.path(), uid)?;
        }
    }
    Ok(())
}

/// Wrap a pty master. `lifetime`, when present, is the connection whose
/// closing ends the guest.
///
/// # Errors
///
/// Returns an error when the descriptor cannot be registered with the
/// runtime.
pub fn pty_guest(master: OwnedFd, lifetime: Option<UnixStream>) -> io::Result<Guest> {
    set_nonblocking(master.as_raw_fd())?;
    let master = Arc::new(AsyncFd::new(master)?);
    Ok(Guest {
        output: Box::pin(PtyHalf(Arc::clone(&master))),
        input: Box::pin(PtyHalf(Arc::clone(&master))),
        control: Box::new(PtyControl { master, lifetime }),
    })
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl on a descriptor this process owns; no memory is passed.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

struct PtyHalf(Arc<AsyncFd<OwnedFd>>);

impl AsyncRead for PtyHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.0.poll_read_ready(context))?;
            let unfilled = buffer.initialize_unfilled();
            let result = guard.try_io(|fd| {
                // SAFETY: reads into the caller's initialised buffer, at most
                // its length.
                let read = unsafe {
                    libc::read(fd.as_raw_fd(), unfilled.as_mut_ptr().cast(), unfilled.len())
                };
                usize::try_from(read).map_err(|_| io::Error::last_os_error())
            });
            match result {
                Ok(Ok(read)) => {
                    buffer.advance(read);
                    return Poll::Ready(Ok(()));
                }
                // The slave side closed: the guest is gone.
                Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => {
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_would_block) => {}
            }
        }
    }
}

impl AsyncWrite for PtyHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.0.poll_write_ready(context))?;
            let result = guard.try_io(|fd| {
                // SAFETY: writes from the caller's buffer, at most its length.
                let written =
                    unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
                usize::try_from(written).map_err(|_| io::Error::last_os_error())
            });
            if let Ok(result) = result {
                return Poll::Ready(result);
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct PtyControl {
    master: Arc<AsyncFd<OwnedFd>>,
    lifetime: Option<UnixStream>,
}

impl GuestControl for PtyControl {
    fn resize(&self, size: PtySize) -> io::Result<()> {
        let size = libc::winsize {
            ws_row: size.rows,
            ws_col: size.cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads one initialised `winsize`.
        let result = unsafe {
            libc::ioctl(
                self.master.get_ref().as_raw_fd(),
                libc::TIOCSWINSZ,
                &raw const size,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn terminate(&self) {
        // The supervisor tears down exactly the guest it built when its
        // client's end of the session connection closes.
        if let Some(lifetime) = &self.lifetime {
            let _ = lifetime.shutdown(std::net::Shutdown::Both);
        }
    }

    fn exit_code(&self) -> Option<i32> {
        None
    }
}

/// A granted directory as the guest binds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantMount {
    /// Where the guest sees it: the path the member knows.
    pub guest_path: PathBuf,
    /// Where it lives on the relay: in the member's file area.
    pub source: PathBuf,
    /// Read-write.
    pub writable: bool,
}

/// Resolve and create the relay-side directories behind `spec`'s grants.
///
/// # Errors
///
/// Returns an error for a grant that is not an absolute normal path, or
/// when a directory cannot be created.
pub fn grant_mounts(area: &MemberArea, spec: &EnvironmentSpec) -> io::Result<Vec<GrantMount>> {
    spec.grants
        .iter()
        .map(|grant| {
            let source = area.grant_source(&grant.path).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "grant is not an absolute path")
            })?;
            std::fs::create_dir_all(&source)?;
            Ok(GrantMount {
                guest_path: PathBuf::from(&grant.path),
                source,
                writable: grant.writable,
            })
        })
        .collect()
}

fn private_directory(path: &Path, owner: Option<u32>) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    if let Some(uid) = owner {
        std::os::unix::fs::chown(path, Some(uid), Some(uid))?;
    }
    Ok(())
}

/// Create the member's cgroup below a delegated root and write their
/// limits into it.
///
/// # Errors
///
/// Returns an error when the cgroup cannot be created or a limit cannot be
/// written.
pub fn member_cgroup(
    root: &Path,
    member: &MemberIdentity,
    quota: &MemberQuota,
) -> io::Result<PathBuf> {
    // Controllers must be enabled for children; a root that already has
    // them (or does not permit changing them) is left as it is.
    let _ = std::fs::write(root.join("cgroup.subtree_control"), "+cpu +memory +pids");
    let cgroup = root.join(format!(
        "member-{}",
        hex::encode(member.user.get(..8).unwrap_or_default())
    ));
    std::fs::create_dir_all(&cgroup)?;
    let period: u64 = 100_000;
    let cpu = u64::from(quota.cpu_percent).saturating_mul(period / 100);
    std::fs::write(cgroup.join("cpu.max"), format!("{cpu} {period}"))?;
    std::fs::write(cgroup.join("memory.max"), quota.memory_bytes.to_string())?;
    std::fs::write(cgroup.join("pids.max"), quota.max_processes.to_string())?;
    Ok(cgroup)
}

/// Persistent per-member uid assignment from a fixed range.
pub struct UidAllocator {
    path: PathBuf,
    first: u32,
    count: u32,
}

impl UidAllocator {
    /// The allocator persisted at `path`, assigning from
    /// `first..first + count`.
    #[must_use]
    pub fn open(path: PathBuf, first: u32, count: u32) -> Self {
        Self { path, first, count }
    }

    fn load(&self) -> io::Result<BTreeMap<String, u32>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(error),
        }
    }

    /// The member's uid, assigning the lowest free one on first use.
    ///
    /// # Errors
    ///
    /// Returns an error when the range is exhausted or the assignment
    /// cannot be persisted.
    pub fn allocate(&self, user: &[u8; 32]) -> io::Result<u32> {
        let mut assigned = self.load()?;
        let key = hex::encode(user);
        if let Some(uid) = assigned.get(&key) {
            return Ok(*uid);
        }
        let taken: std::collections::BTreeSet<u32> = assigned.values().copied().collect();
        let uid = (0..self.count)
            .filter_map(|offset| self.first.checked_add(offset))
            .find(|uid| !taken.contains(uid))
            .ok_or_else(|| io::Error::other("member uid range exhausted"))?;
        assigned.insert(key, uid);
        let bytes = serde_json::to_vec_pretty(&assigned).map_err(io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        tilo_files::native::atomic::write(&self.path, &bytes)?;
        Ok(uid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_relay_protocol::environment::{DirectoryGrant, EnvironmentRecordRef, HomePolicy};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn spec(grants: &[&str]) -> EnvironmentSpec {
        EnvironmentSpec {
            name: "dev".into(),
            runtime: "so.tilo.env.fedora".into(),
            tools: Vec::new(),
            grants: grants
                .iter()
                .map(|path| DirectoryGrant {
                    path: (*path).into(),
                    writable: true,
                })
                .collect(),
            home: HomePolicy::Persistent,
        }
    }

    #[test]
    fn grants_resolve_inside_the_members_own_file_area() {
        let root = tempfile::tempdir().unwrap();
        let alice = MemberArea::of(root.path(), &[1; 32]);
        let bob = MemberArea::of(root.path(), &[2; 32]);
        let mounts = grant_mounts(&alice, &spec(&["/home/alice/Projects"])).unwrap();
        assert_eq!(
            mounts,
            vec![GrantMount {
                guest_path: PathBuf::from("/home/alice/Projects"),
                source: alice.files().join("home/alice/Projects"),
                writable: true,
            }]
        );
        assert!(mounts[0].source.is_dir());
        // The same grant from another member resolves into their own area.
        let theirs = grant_mounts(&bob, &spec(&["/home/alice/Projects"])).unwrap();
        assert!(theirs[0].source.starts_with(bob.files()));
        assert!(!theirs[0].source.starts_with(alice.root()));
        // Nothing escapes the area.
        assert!(grant_mounts(&alice, &spec(&["/home/../../etc"])).is_err());
        assert!(alice.grant_source("relative").is_none());
        assert_eq!(alice.grant_source("/").unwrap(), alice.files());
    }

    #[test]
    fn the_members_record_is_what_their_supervisor_builds_from() {
        let root = tempfile::tempdir().unwrap();
        let area = MemberArea::of(root.path(), &[1; 32]);
        let record = EnvironmentRecordRef {
            workspace_id: 7,
            record_id: "work".into(),
        };
        let runtime = tilo_environment::PackageId::from_bytes([1; 32]).to_string();
        let mut spec = spec(&["/home/alice/src", "/usr/share"]);
        spec.runtime.clone_from(&runtime);
        let key = write_record(&area, &record, &spec, None).unwrap();
        assert!(tilo_environment::is_record_key(&key));
        let stored = ush::records::Records::at(area.records())
            .load(&key)
            .unwrap()
            .unwrap();
        assert_eq!(stored.runtime, runtime);
        // A guest-owned path is never granted, even if a device sent one.
        assert_eq!(stored.grants.0.len(), 1);
        // A tool the session built lands in the record; the launcher reads
        // it back for the member's terminal.
        let tool = tilo_environment::PackageId::from_bytes([2; 32]).to_string();
        ush::records::Records::at(area.records())
            .apply(
                &key,
                tilo_environment::Environment::ADD_TOOL.with((tool.clone(), vec!["jq".into()])),
            )
            .unwrap();
        let launcher = UshGuestLauncher::new(UshLauncherConfig::at(root.path().to_path_buf()));
        let member = MemberContext {
            identity: MemberIdentity {
                user: [1; 32],
                device: [1; 32],
            },
            area: area.clone(),
            quota: MemberQuota::default(),
        };
        assert_eq!(
            launcher.current_tools(&member, &record).unwrap(),
            vec![ToolImage {
                id: tool,
                packages: vec!["jq".into()],
            }]
        );
        // A new session starts from what the device resolved again.
        write_record(&area, &record, &spec, None).unwrap();
        assert!(launcher.current_tools(&member, &record).unwrap().is_empty());
    }

    #[test]
    fn uids_are_stable_per_member_and_bounded_by_the_range() {
        let root = tempfile::tempdir().unwrap();
        let allocator = UidAllocator::open(root.path().join("uids.json"), 200_000, 2);
        let alice = allocator.allocate(&[1; 32]).unwrap();
        let bob = allocator.allocate(&[2; 32]).unwrap();
        assert_ne!(alice, bob);
        assert_eq!(alice, 200_000);
        let reopened = UidAllocator::open(root.path().join("uids.json"), 200_000, 2);
        assert_eq!(reopened.allocate(&[1; 32]).unwrap(), alice);
        assert!(reopened.allocate(&[3; 32]).is_err());
    }

    #[test]
    fn member_cgroups_carry_the_members_limits() {
        let root = tempfile::tempdir().unwrap();
        let quota = MemberQuota {
            cpu_percent: 150,
            memory_bytes: 1 << 30,
            max_processes: 64,
            ..MemberQuota::default()
        };
        let member = MemberIdentity {
            user: [0xAB; 32],
            device: [1; 32],
        };
        let cgroup = member_cgroup(root.path(), &member, &quota).unwrap();
        assert_eq!(cgroup, root.path().join("member-abababababababab"));
        let read = |file: &str| std::fs::read_to_string(cgroup.join(file)).unwrap();
        assert_eq!(read("cpu.max"), "150000 100000");
        assert_eq!(read("memory.max"), (1u64 << 30).to_string());
        assert_eq!(read("pids.max"), "64");
    }

    fn openpty() -> (OwnedFd, OwnedFd) {
        use std::os::fd::FromRawFd;
        let mut master: RawFd = -1;
        let mut slave: RawFd = -1;
        // SAFETY: out-parameters only.
        let rc = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0);
        // SAFETY: openpty returned two owned descriptors.
        unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
    }

    /// A fake supervisor: answers one request with a pty master over
    /// `SCM_RIGHTS`, as `tilo-ush serve` does, and keeps the slave.
    #[tokio::test]
    async fn a_supervisor_session_becomes_a_pty_guest() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("ush.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let supervisor = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(
                &mut std::io::BufReader::new(stream.try_clone().unwrap()),
                &mut line,
            )
            .unwrap();
            let request: ush::supervisor::ServeRequest = serde_json::from_str(&line).unwrap();
            let ush::supervisor::ServeRequest::OpenSession {
                cols, rows, target, ..
            } = request;
            assert_eq!((cols, rows), (100, 30));
            assert_eq!(
                target,
                tilo_environment::Target::Environment {
                    record: Some("relay-key".into())
                }
            );
            let (master, slave) = openpty();
            let mut reply = serde_json::to_vec(&ush::supervisor::ServeResponse::Session {
                session: "s".into(),
                environment: "relay-key".into(),
            })
            .unwrap();
            reply.push(b'\n');
            runtime_portal::fd_transport::send_with_fd(
                stream.as_raw_fd(),
                &reply,
                master.as_raw_fd(),
            )
            .unwrap();
            drop(master);
            (stream, slave)
        });
        let request = GuestRequest {
            session: [1; 16],
            record: EnvironmentRecordRef {
                workspace_id: 1,
                record_id: "env".into(),
            },
            spec: spec(&[]),
            size: PtySize {
                cols: 100,
                rows: 30,
            },
            term: "xterm-256color".into(),
        };
        let socket_path = socket.clone();
        let guest = tokio::task::spawn_blocking(move || {
            open_ush_session(&socket_path, "relay-key", &request)
        })
        .await
        .unwrap()
        .unwrap();
        let (_lifetime, slave) = supervisor.join().unwrap();
        let mut slave = std::fs::File::from(slave);
        let Guest {
            mut output,
            mut input,
            control,
        } = guest;
        slave.write_all(b"hello").unwrap();
        let mut buffer = [0; 5];
        output.read_exact(&mut buffer).await.unwrap();
        assert_eq!(&buffer, b"hello");
        input.write_all(b"x").await.unwrap();
        control.resize(PtySize { cols: 7, rows: 3 }).unwrap();
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCGWINSZ writes one `winsize`.
        assert_eq!(
            unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCGWINSZ, &raw mut size) },
            0
        );
        assert_eq!((size.ws_col, size.ws_row), (7, 3));
        drop(slave);
        let mut rest = Vec::new();
        output.read_to_end(&mut rest).await.unwrap();
    }
}
