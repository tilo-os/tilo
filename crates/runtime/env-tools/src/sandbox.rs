//! The build sandbox: a runtime tree with a writable upper, entered as uid 0
//! of a fresh user namespace.
//!
//! ```text
//! /usr, /etc   overlay: lower = the runtime tree, upper = <scratch>/upper/…
//! /var         <scratch>/var (dnf's history, logs, lock files)
//! /var/cache/libdnf5   the builder's metadata cache for this runtime
//! /proc /dev /tmp /run fresh
//! ```
//!
//! uid 0 is needed only here, at build time: rpm chowns and scriptlets run
//! as root. The user namespace maps that root to the unprivileged caller
//! ([`Identity`]): with the caller's subordinate ids when it has them, so
//! rpm can also chown to the system users and groups packages create (a
//! `cups` file belongs to group `lp`), and the upper is handed back to the
//! caller afterwards; else that root alone, which is enough for most
//! packages. bwrap drops capabilities unless asked; the transaction needs
//! them inside its own namespace (a copied-up `/usr/lib64` is mode 0555), so
//! they are added back there and confer nothing outside.
//!
//! What the sandbox may reach is its [`Network`]: for a build, the
//! runtime's package repositories and nothing else.

use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use crate::network::Network;

/// Top-level entries of the runtime tree that are mounted rather than
/// linked. Everything else at the top that is a symlink (`bin -> usr/bin`) is
/// recreated as one.
const MOUNTED: &[&str] = &["usr", "etc", "var", "proc", "dev", "tmp", "run"];

/// The links a merged-`/usr` root has at its top, when the target exists.
const MERGED_USR: &[(&str, &str)] = &[
    ("bin", "usr/bin"),
    ("sbin", "usr/sbin"),
    ("lib", "usr/lib"),
    ("lib64", "usr/lib64"),
];

/// The subtrees the upper covers; a tool's files live under these.
pub const UPPER_SUBTREES: &[&str] = &["usr", "etc"];

/// Who the build's root is, outside its namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// The caller is already root (a disposable CI container): no user
    /// namespace is needed.
    Root,
    /// `unshare --map-root-user --map-auto`: root is the caller, ids 1… are
    /// the caller's subordinate ids from `/etc/subuid` and `/etc/subgid`.
    SubordinateIds,
    /// bwrap's own single mapping: root is the caller, nothing else exists.
    CallerOnly,
}

impl Identity {
    /// The best this host offers, probed once per process.
    pub fn detect() -> Self {
        static DETECTED: std::sync::OnceLock<Identity> = std::sync::OnceLock::new();
        *DETECTED.get_or_init(|| {
            if rustix::process::geteuid().as_raw() == 0 {
                return Self::Root;
            }
            let mut probe = Command::new("unshare");
            probe.args(["--user", "--map-root-user", "--map-auto", "--", "true"]);
            match bounded_command::run_prepared(probe, None, 64 * 1024, Duration::from_secs(10)) {
                Ok(out) if out.status.success() => Self::SubordinateIds,
                _ => Self::CallerOnly,
            }
        })
    }
}

/// The `unshare` prefix that enters a namespace with subordinate ids.
const UNSHARE_SUBORDINATE: &[&str] = &["--user", "--map-root-user", "--map-auto", "--"];

/// Output of a sandboxed command beyond which it is treated as runaway.
const OUTPUT_LIMIT: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("runtime tree {0} has no usr/ — not an env-runtime")]
    NotARuntime(PathBuf),
    #[error("build scratch {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("running bwrap: {0}")]
    Spawn(#[from] bounded_command::Error),
    /// The build's network could not be set up; the build did not run.
    #[error("the build's network could not be set up, so it did not run: {0}")]
    Egress(String),
    /// This platform cannot enforce the requested gated network.
    #[error("gated build networking is unsupported on this platform; the build did not run")]
    UnsupportedGatedNetwork,
}

/// Captured result of one command inside the sandbox.
#[derive(Debug)]
pub struct Ran {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// One build's sandbox. The scratch directory is the caller's and is
/// removed by [`BuildSandbox::discard`] (or left for inspection).
#[derive(Debug)]
pub struct BuildSandbox {
    runtime: PathBuf,
    scratch: PathBuf,
    dnf_cache: Option<PathBuf>,
    bwrap: PathBuf,
    identity: Identity,
    network: Network,
}

impl BuildSandbox {
    /// A sandbox over `runtime` (a materialised env-runtime: `usr/`, `etc/`,
    /// top-level symlinks) whose writable state lives in `scratch`, which
    /// must be empty or absent and on a filesystem overlayfs accepts as an
    /// upper (not tmpfs-in-a-quota, not overlay itself).
    ///
    /// # Errors
    ///
    /// [`SandboxError::NotARuntime`] when `runtime` has no `usr/`;
    /// [`SandboxError::Io`] when the scratch layout can't be created.
    pub fn create(runtime: &Path, scratch: &Path) -> Result<Self, SandboxError> {
        if !runtime.join("usr").is_dir() {
            return Err(SandboxError::NotARuntime(runtime.to_owned()));
        }
        for sub in UPPER_SUBTREES {
            for side in ["upper", "work"] {
                let dir = scratch.join(side).join(sub);
                std::fs::create_dir_all(&dir).map_err(|source| SandboxError::Io {
                    path: dir.clone(),
                    source,
                })?;
            }
        }
        let var = scratch.join("var");
        std::fs::create_dir_all(&var).map_err(|source| SandboxError::Io {
            path: var.clone(),
            source,
        })?;
        Ok(Self {
            runtime: runtime.to_owned(),
            scratch: scratch.to_owned(),
            dnf_cache: None,
            bwrap: PathBuf::from("bwrap"),
            identity: Identity::detect(),
            network: Network::Offline,
        })
    }

    /// Keep dnf's repository metadata in `dir` across builds on the same
    /// runtime, so a resolve doesn't re-download it every time.
    #[must_use]
    pub fn with_dnf_cache(mut self, dir: PathBuf) -> Self {
        self.dnf_cache = Some(dir);
        self
    }

    /// Use a specific bubblewrap binary instead of `bwrap` on `PATH`.
    #[must_use]
    pub fn with_bwrap(mut self, bwrap: PathBuf) -> Self {
        self.bwrap = bwrap;
        self
    }

    /// What the sandbox may reach; [`Network::Offline`] unless set.
    #[must_use]
    pub fn with_network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    /// Enter the sandbox as `identity` instead of the detected one.
    #[must_use]
    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    /// The upper: `usr/` and `etc/` holding everything the transaction wrote.
    pub fn upper(&self) -> PathBuf {
        self.scratch.join("upper")
    }

    pub fn runtime(&self) -> &Path {
        &self.runtime
    }

    /// The bwrap argument vector that enters the sandbox and runs `program`.
    ///
    /// # Errors
    ///
    /// [`SandboxError::Io`] when the runtime's top level can't be listed or
    /// the dnf cache can't be created.
    pub fn bwrap_args(
        &self,
        program: &[&str],
        network: &[OsString],
    ) -> Result<Vec<OsString>, SandboxError> {
        let mut args: Vec<OsString> = Vec::new();
        let mut extend = |items: &[&dyn AsRef<std::ffi::OsStr>]| {
            for item in items {
                args.push(item.as_ref().to_owned());
            }
        };
        if self.identity == Identity::CallerOnly {
            extend(&[&"--unshare-user", &"--uid", &"0", &"--gid", &"0"]);
        }
        extend(&[
            &"--cap-add",
            &"ALL",
            &"--unshare-pid",
            &"--unshare-ipc",
            &"--unshare-uts",
            &"--unshare-cgroup-try",
            &"--die-with-parent",
            &"--new-session",
            &"--clearenv",
        ]);
        for sub in UPPER_SUBTREES {
            let lower = self.runtime.join(sub);
            if !lower.is_dir() {
                continue;
            }
            let upper = self.scratch.join("upper").join(sub);
            let work = self.scratch.join("work").join(sub);
            let target = format!("/{sub}");
            extend(&[
                &"--overlay-src",
                &lower,
                &"--overlay",
                &upper,
                &work,
                &target,
            ]);
        }
        for (name, link) in self.top_level_links()? {
            let target = format!("/{name}");
            extend(&[&"--symlink", &link, &target]);
        }
        let var = self.scratch.join("var");
        extend(&[
            &"--bind", &var, &"/var", &"--proc", &"/proc", &"--dev", &"/dev", &"--tmpfs", &"/tmp",
            &"--tmpfs", &"/run",
        ]);
        if let Some(cache) = &self.dnf_cache {
            std::fs::create_dir_all(cache).map_err(|source| SandboxError::Io {
                path: cache.clone(),
                source,
            })?;
            extend(&[&"--bind", cache, &"/var/cache/libdnf5"]);
        }
        // After the mounts: the network's resolv.conf binds over the overlay.
        for arg in network {
            extend(&[arg]);
        }
        extend(&[
            &"--setenv",
            &"PATH",
            &"/usr/local/bin:/usr/bin:/usr/sbin",
            &"--setenv",
            &"HOME",
            &"/root",
            &"--setenv",
            &"LC_ALL",
            &"C.UTF-8",
            &"--setenv",
            &"TERM",
            &"dumb",
            &"--chdir",
            &"/",
            &"--",
        ]);
        for arg in program {
            args.push(OsString::from(arg));
        }
        Ok(args)
    }

    /// Run `argv` in the sandbox, capturing its output, for at most
    /// `timeout`.
    ///
    /// # Errors
    ///
    /// [`SandboxError::Spawn`] when bwrap can't be started, exceeds the
    /// output ceiling or the deadline. A command that runs and fails is an
    /// `Ok` with `success == false`.
    pub fn run(&self, argv: &[&str], timeout: Duration) -> Result<Ran, SandboxError> {
        self.run_on(&self.network, argv, timeout)
    }

    /// [`Self::run`] with no network, whatever the sandbox's is: for
    /// queries of the rpm database.
    ///
    /// # Errors
    ///
    /// As [`Self::run`].
    pub fn run_offline(&self, argv: &[&str], timeout: Duration) -> Result<Ran, SandboxError> {
        self.run_on(&Network::Offline, argv, timeout)
    }

    fn run_on(
        &self,
        network: &Network,
        argv: &[&str],
        timeout: Duration,
    ) -> Result<Ran, SandboxError> {
        // The previous run's overlay can outlive its process briefly while
        // the kernel tears its mount namespace down; until then the upper
        // is busy. Wait that out rather than fail the build.
        let mut attempts = 0u32;
        loop {
            let ran = self.run_once(network, argv, timeout)?;
            attempts = attempts.saturating_add(1);
            if ran.success || !overlay_busy(&ran.stderr) || attempts >= 20 {
                return Ok(ran);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn command(&self) -> Command {
        if self.identity == Identity::SubordinateIds {
            let mut command = Command::new("unshare");
            command.args(UNSHARE_SUBORDINATE).arg(&self.bwrap);
            command
        } else {
            Command::new(&self.bwrap)
        }
    }

    fn run_once(
        &self,
        network: &Network,
        argv: &[&str],
        timeout: Duration,
    ) -> Result<Ran, SandboxError> {
        tracing::debug!(?argv, runtime = %self.runtime.display(), "build sandbox");
        let out = match network {
            #[cfg(not(target_os = "linux"))]
            Network::Gated { .. } => return Err(SandboxError::UnsupportedGatedNetwork),
            #[cfg(target_os = "linux")]
            Network::Gated { policy, options } => self.run_gated(argv, policy, options, timeout),
            Network::Offline | Network::Host => {
                // Name resolution for the caller's network; the mount point
                // it creates in the upper is dropped by the seal.
                let net_args: Vec<OsString> = if matches!(network, Network::Host) {
                    [
                        "--share-net",
                        "--ro-bind-try",
                        "/etc/resolv.conf",
                        "/etc/resolv.conf",
                    ]
                    .map(OsString::from)
                    .to_vec()
                } else {
                    vec!["--unshare-net".into()]
                };
                let mut command = self.command();
                command.args(self.bwrap_args(argv, &net_args)?);
                bounded_command::run_prepared_with_limits(
                    command,
                    None,
                    OUTPUT_LIMIT,
                    OUTPUT_LIMIT,
                    timeout,
                )
                .map_err(SandboxError::from)
            }
        };
        self.reclaim()?;
        let out = out?;
        Ok(Ran {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    /// Run with gated egress. bwrap makes the network namespace and stops
    /// before the program (`--block-fd`); the egress is attached to that
    /// namespace — stub, filter, then `pasta` — and only then is bwrap let
    /// go. Any failure before that kills bwrap: the program never runs
    /// with a network the policy didn't shape.
    #[cfg(target_os = "linux")]
    fn run_gated(
        &self,
        argv: &[&str],
        policy: &tilo_egress::EgressPolicy,
        options: &tilo_egress::EgressOptions,
        timeout: Duration,
    ) -> Result<bounded_command::Output, SandboxError> {
        let io = |path: &Path| {
            let path = path.to_owned();
            move |source| SandboxError::Io { path, source }
        };
        let resolv = self.scratch.join("resolv.conf");
        std::fs::write(&resolv, tilo_egress::RESOLV_CONF).map_err(io(&resolv))?;
        let (info_read, info_write) = pipe().map_err(io(&self.scratch))?;
        let (block_read, block_write) = pipe().map_err(io(&self.scratch))?;
        let (info_fd, block_fd) = (info_write.as_raw_fd(), block_read.as_raw_fd());
        let network: Vec<OsString> = vec![
            "--unshare-net".into(),
            "--ro-bind".into(),
            resolv.into_os_string(),
            "/etc/resolv.conf".into(),
            "--info-fd".into(),
            info_fd.to_string().into(),
            "--block-fd".into(),
            block_fd.to_string().into(),
        ];
        let mut command = self.command();
        command.args(self.bwrap_args(argv, &network)?);
        // SAFETY: fcntl is async-signal-safe; the descriptors are open (the
        // pipes outlive the spawn) and only lose close-on-exec in the child.
        unsafe {
            command.pre_exec(move || {
                for fd in [info_fd, block_fd] {
                    if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = bounded_command::spawn_prepared(command, None, OUTPUT_LIMIT, timeout)?;
        let pid = match child_pid(&info_read, &mut child) {
            Ok(pid) => pid,
            // bwrap ended before it was ready: its own words say why.
            Err(ChildPid::Exited(out)) => return out.map_err(SandboxError::from),
            Err(ChildPid::Failed(reason)) => return Err(SandboxError::Egress(reason)),
        };
        // Both ends the child holds are in it by now.
        drop((info_write, block_read));
        let attached = tilo_egress::Netns::of_process(pid)
            .map_err(|e| format!("the sandbox's namespaces: {e}"))
            .and_then(|netns| {
                tilo_egress::spawn_egress(&netns, policy.clone(), options)
                    .map(|egress| (netns, egress))
                    .map_err(|e| e.to_string())
            });
        let (netns, egress) = match attached {
            Ok(attached) => attached,
            Err(reason) => {
                // A sandbox that failed to set itself up (its overlay still
                // busy, say) is gone before egress can attach; its own error
                // is the one to report, and the one `run` retries on.
                let deadline = Instant::now()
                    .checked_add(Duration::from_secs(2))
                    .unwrap_or_else(Instant::now);
                while Instant::now() < deadline {
                    match child.try_wait() {
                        Some(Ok(out)) if !out.status.success() => return Ok(out),
                        Some(_) => break,
                        None => {}
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                return Err(SandboxError::Egress(reason));
            }
        };
        // SAFETY: a one-byte write to a pipe this function owns.
        let released =
            unsafe { libc::write(block_write.as_raw_fd(), b"x".as_ptr().cast(), 1) } == 1;
        drop(block_write);
        if !released {
            return Err(SandboxError::Egress("could not release the sandbox".into()));
        }
        let out = loop {
            if let Some(out) = child.try_wait() {
                break out;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(egress);
        drop(netns);
        out.map_err(SandboxError::from)
    }

    /// Give everything in the scratch directory back to the caller: files
    /// rpm chowned to a subordinate id can't otherwise be moved, read or
    /// removed. Ownership is not part of an image anyway.
    ///
    /// # Errors
    ///
    /// `unshare` or `chown` can't be run.
    fn reclaim(&self) -> Result<(), SandboxError> {
        if self.identity != Identity::SubordinateIds {
            return Ok(());
        }
        let mut command = Command::new("unshare");
        command
            .args(UNSHARE_SUBORDINATE)
            .args(["chown", "-R", "-h", "0:0", "--"])
            .arg(&self.scratch);
        let out =
            bounded_command::run_prepared(command, None, 1024 * 1024, Duration::from_secs(600))?;
        if !out.status.success() {
            tracing::warn!(
                stderr = %String::from_utf8_lossy(&out.stderr),
                "could not take the build's files back from its subordinate ids"
            );
        }
        Ok(())
    }

    /// Remove the scratch directory, including the overlay work dirs the
    /// kernel leaves mode 0000 and the copied-up 0555 directories.
    ///
    /// # Errors
    ///
    /// The first removal failure.
    pub fn discard(self) -> std::io::Result<()> {
        remove_tree(&self.scratch)
    }

    /// The root's symlinks: the runtime's own top-level links, and the
    /// merged-`/usr` links an image (which holds only `usr/` and `etc/`)
    /// leaves out — `/lib64/ld-linux-x86-64.so.2` must resolve for anything
    /// to run.
    fn top_level_links(&self) -> Result<Vec<(String, PathBuf)>, SandboxError> {
        let io = |source| SandboxError::Io {
            path: self.runtime.clone(),
            source,
        };
        let mut links = Vec::new();
        for entry in std::fs::read_dir(&self.runtime).map_err(io)? {
            let entry = entry.map_err(io)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if MOUNTED.contains(&name.as_str()) || !entry.file_type().map_err(io)?.is_symlink() {
                continue;
            }
            links.push((name, std::fs::read_link(entry.path()).map_err(io)?));
        }
        for (name, target) in MERGED_USR {
            if !links.iter().any(|(n, _)| n == name) && self.runtime.join(target).is_dir() {
                links.push(((*name).to_owned(), PathBuf::from(target)));
            }
        }
        links.sort();
        Ok(links)
    }
}

/// A close-on-exec pipe: `(read, write)`.
#[cfg(target_os = "linux")]
fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds: [RawFd; 2] = [-1, -1];
    // SAFETY: `fds` is a two-element array, as pipe2 requires.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 just returned these two descriptors, owned by no one.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(target_os = "linux")]
enum ChildPid {
    Exited(Result<bounded_command::Output, bounded_command::Error>),
    Failed(String),
}

/// Read `"child-pid": N` from bwrap's `--info-fd`, or what happened instead.
#[cfg(target_os = "linux")]
fn child_pid(info: &OwnedFd, child: &mut bounded_command::AsyncCommand) -> Result<u32, ChildPid> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(60))
        .unwrap_or_else(Instant::now);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        if let Some(pid) = parse_child_pid(&seen) {
            return Ok(pid);
        }
        if let Some(out) = child.try_wait() {
            return Err(ChildPid::Exited(out));
        }
        let mut poll = libc::pollfd {
            fd: info.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd; a bounded wait.
        let ready = unsafe { libc::poll(&raw mut poll, 1, 50) };
        if ready > 0 {
            let mut buf = [0u8; 256];
            // SAFETY: reading into a live local buffer from an open pipe.
            let n = unsafe { libc::read(info.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if let Ok(n) = usize::try_from(n) {
                seen.extend_from_slice(buf.get(..n).unwrap_or_default());
            }
        }
    }
    Err(ChildPid::Failed(
        "the sandbox never reported its process".into(),
    ))
}

/// bwrap's info is JSON: `{ "child-pid": 1234 }`.
#[cfg(any(target_os = "linux", test))]
fn parse_child_pid(info: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(info).ok()?;
    let rest = text.split_once("\"child-pid\"")?.1;
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    // A number is complete once something follows it.
    (rest.len() > digits.len())
        .then(|| digits.parse().ok())
        .flatten()
}

/// Whether this host can run a build sandbox: bwrap is present and may
/// create a user namespace with an overlay. A cheap probe, for callers and
/// tests that should skip rather than fail where it can't.
pub fn available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(probe_available)
}

fn probe_available() -> bool {
    let probe = std::env::temp_dir().join(format!("tilo-env-tools-probe-{}", std::process::id()));
    let ok = (|| {
        for side in ["lower", "upper", "work"] {
            std::fs::create_dir_all(probe.join(side)).ok()?;
        }
        let mut command = Command::new("bwrap");
        command
            .args([
                "--unshare-user",
                "--uid",
                "0",
                "--gid",
                "0",
                "--cap-add",
                "ALL",
            ])
            .args(["--ro-bind", "/", "/", "--tmpfs", "/run", "--overlay-src"])
            .arg(probe.join("lower"))
            .arg("--overlay")
            .arg(probe.join("upper"))
            .arg(probe.join("work"))
            .args(["/run/probe", "--", "true"]);
        let out = bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(10))
            .ok()?;
        Some(out.status.success())
    })()
    .unwrap_or(false);
    let _ = remove_tree(&probe);
    ok
}

/// bwrap's report that the upper is still held by an overlay being torn down.
fn overlay_busy(stderr: &str) -> bool {
    stderr.contains("Can't make overlay mount") && stderr.contains("Device or resource busy")
}

/// `rm -rf` that first makes every directory writable and searchable, as the
/// overlay leaves some that aren't.
///
/// # Errors
///
/// The first failure other than the path already being gone.
pub fn remove_tree(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if meta.is_dir() {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
        for entry in std::fs::read_dir(path)? {
            remove_tree(&entry?.path())?;
        }
        std::fs::remove_dir(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn gated_builds_are_rejected_before_spawning_on_unsupported_platforms() {
        use std::os::unix::fs::PermissionsExt as _;
        use tilo_egress::{EgressOptions, EgressPolicy};

        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("runtime");
        let scratch = root.path().join("scratch");
        std::fs::create_dir_all(runtime.join("usr")).unwrap();
        let bwrap = root.path().join("bwrap");
        // Any attempted spawn, including a host-network fallback, leaves a marker.
        std::fs::write(
            &bwrap,
            "#!/bin/sh\nprintf started > \"$0.started\"\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&bwrap, std::fs::Permissions::from_mode(0o700)).unwrap();

        for policy in [
            EgressPolicy::none(),
            EgressPolicy::from_grants(["packages.example.test"]),
            EgressPolicy::Any,
        ] {
            let sandbox = BuildSandbox::create(&runtime, &scratch)
                .unwrap()
                .with_identity(Identity::CallerOnly)
                .with_bwrap(bwrap.clone())
                .with_network(Network::Gated {
                    policy,
                    options: EgressOptions::from_host(&runtime),
                });
            let error = sandbox.run(&["true"], Duration::from_secs(1)).unwrap_err();
            assert!(matches!(error, SandboxError::UnsupportedGatedNetwork));
            assert!(!root.path().join("bwrap.started").exists());
            assert!(!scratch.join("resolv.conf").exists());
        }
    }

    #[test]
    fn the_child_pid_is_read_once_it_is_complete() {
        assert_eq!(parse_child_pid(b"{\n    \"child-pid\": 4242"), None);
        assert_eq!(
            parse_child_pid(b"{\n    \"child-pid\": 4242\n}\n"),
            Some(4242)
        );
        assert_eq!(parse_child_pid(b"{ \"child-pid\": 7,"), Some(7));
        assert_eq!(parse_child_pid(b"garbage"), None);
    }
}
