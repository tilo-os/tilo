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
//! The network is shared with the caller. Package downloads are the build's
//! only traffic, and dnf has no way to reach the launcher's egress gate,
//! which is a Unix socket.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

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
            // SAFETY: geteuid has no preconditions and cannot fail.
            if unsafe { libc::geteuid() } == 0 {
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
    pub fn bwrap_args(&self, program: &[&str]) -> Result<Vec<OsString>, SandboxError> {
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
            &"--share-net",
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
        // Name resolution for the downloads. The mount point it creates in
        // the upper is dropped by the seal.
        extend(&[
            &"--ro-bind-try",
            &"/etc/resolv.conf",
            &"/etc/resolv.conf",
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
        let mut command = if self.identity == Identity::SubordinateIds {
            let mut command = Command::new("unshare");
            command.args(UNSHARE_SUBORDINATE).arg(&self.bwrap);
            command
        } else {
            Command::new(&self.bwrap)
        };
        command.args(self.bwrap_args(argv)?);
        tracing::debug!(?argv, runtime = %self.runtime.display(), "build sandbox");
        let out = bounded_command::run_prepared_with_limits(
            command,
            None,
            OUTPUT_LIMIT,
            OUTPUT_LIMIT,
            timeout,
        );
        self.reclaim()?;
        let out = out?;
        Ok(Ran {
            success: out.status.success(),
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
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

/// Whether this host can run a build sandbox: bwrap is present and may
/// create a user namespace with an overlay. A cheap probe, for callers and
/// tests that should skip rather than fail where it can't.
pub fn available() -> bool {
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
