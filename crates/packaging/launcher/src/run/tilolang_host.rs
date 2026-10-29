//! The launch kind "tilolang host": one process per room and author that runs that author's
//! tilolang apps in that room, each as an instance (`docs/designs/app-language/ui-host.md`).
//!
//! It is no app launch. It has no manifest, no app id and no app storage, and its sandbox is
//! narrower than any app's:
//!
//! - the tilolang runtime bundle, verified as an app launch on it verifies it, read-only and
//!   executable, its binary `bin/tilolang-host` the only program;
//! - the Tilo runtime its apps name, verified the same way, read-only: the loader and libraries
//!   that binary runs on, as an app's entrypoint binary runs on its app's runtime;
//! - one directory of its own, read-write: the storage of its room and author, which the session
//!   names, keyed by package inside. Landlock can only narrow, so it holds that directory from
//!   its start;
//! - its door, the one descriptor it inherits (at [`DOOR_CHILD_FD`]);
//! - nothing else: no display, no session bus, no service socket, no network (every namespace is
//!   its own, and Landlock refuses TCP), no home, no device but the minimal `/dev`; the baseline
//!   seccomp filter with write-xor-execute.
//!
//! Everything an instance is arrives over the door from the session, attested as that app's
//! launch (`runtime_portal::tilolang_host`).

use super::{RunError, RunOptions, compile_app_baseline, find_runtime_bundle, stage_seccomp_blob};
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The subcommand the session starts a tilolang host with.
pub const SUBCOMMAND: &str = "tilolang-host";

/// The host's binary in the tilolang runtime bundle.
pub const BIN: &str = "bin/tilolang-host";

/// Where the host sees its storage.
pub const STORAGE_MOUNT: &str = "/run/tilolang-host/storage";

/// The descriptor the host finds its door at.
pub const DOOR_CHILD_FD: RawFd = 3;

/// The seccomp filters' descriptors in the sandbox's setup.
const CLONE3_COMPAT_FD: RawFd = crate::services::CLONE3_COMPAT_SECCOMP_CHILD_FD;
const BASELINE_FD: RawFd = crate::services::BASELINE_SECCOMP_CHILD_FD;

/// What a tilolang host is started with.
#[derive(Clone, Debug)]
pub struct HostLaunch {
    /// The tilolang runtime, `tilolang/<version>`.
    pub runtime: String,
    /// The Tilo runtime the host's binary runs on, `tilo-runtime/<version>`.
    pub app_runtime: String,
    /// The storage of the host's workspace and author.
    pub storage: PathBuf,
}

/// The bubblewrap arguments of a host on the verified tilolang runtime bundle `runtime_root` of
/// `version`, running on the verified Tilo runtime `app_runtime` (its root and version), holding
/// `storage`, with the launcher's own binary at `shim` for Landlock.
#[must_use]
pub fn bwrap_argv(
    (runtime_root, version): (&Path, &str),
    app_runtime: (&Path, &str),
    storage: &Path,
    shim: &Path,
) -> Vec<String> {
    let runtime_mount = crate::sandbox::runtime_mount_for(version);
    let mut argv: Vec<String> = [
        "--unshare-all",
        "--die-with-parent",
        "--new-session",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    argv.push("--clearenv".to_owned());
    for (name, value) in [
        ("PATH", crate::sandbox::SANDBOX_PATH.to_owned()),
        (
            runtime_portal::tilolang_host::DOOR_FD_ENV,
            DOOR_CHILD_FD.to_string(),
        ),
        (
            runtime_portal::tilolang_host::STORAGE_ENV,
            STORAGE_MOUNT.to_owned(),
        ),
    ] {
        argv.extend(["--setenv".to_owned(), name.to_owned(), value]);
    }
    for name in ["LANG", "LC_ALL", "LC_MESSAGES", "RUST_LOG"] {
        if let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            argv.extend([
                "--setenv".to_owned(),
                name.to_owned(),
                value.to_string_lossy().into_owned(),
            ]);
        }
    }
    // The Tilo runtime: the loader, `LD_LIBRARY_PATH` and libraries, as for an app; the
    // tilolang bundle beside it, as an app's entrypoint runtime is.
    crate::sandbox::push_runtime_bundle(&mut argv, Some(app_runtime));
    let _ = crate::sandbox::push_required_runtime(&mut argv, runtime_root, version);
    argv.extend([
        "--bind".to_owned(),
        storage.display().to_string(),
        STORAGE_MOUNT.to_owned(),
    ]);
    // Landlock inside the namespace, through the launcher's own binary: read and execute the
    // runtime, write the storage, no TCP; every descriptor but the door closed.
    argv.extend([
        "--ro-bind".to_owned(),
        shim.display().to_string(),
        super::SHIM_MOUNT_PATH.to_owned(),
        super::SHIM_MOUNT_PATH.to_owned(),
        crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND.to_owned(),
        "--ro".to_owned(),
        runtime_mount.display().to_string(),
    ]);
    super::push_runtime_landlock_rules(&mut argv, Some(app_runtime));
    argv.extend([
        "--rw-noexec".to_owned(),
        STORAGE_MOUNT.to_owned(),
        "--net-restricted".to_owned(),
        "--keep-fd".to_owned(),
        DOOR_CHILD_FD.to_string(),
        "--".to_owned(),
        runtime_mount.join(BIN).display().to_string(),
    ]);
    argv
}

/// Start the tilolang host `launch` on its door `door` and wait for it to end.
///
/// # Errors
/// The runtime is malformed, missing or does not verify, the storage is not a directory, or
/// the sandbox cannot be set up.
pub fn run_tilolang_host(
    launch: &HostLaunch,
    door: OwnedFd,
    opts: &RunOptions,
) -> Result<std::process::ExitStatus, RunError> {
    let (found, searched) = find_runtime_bundle(&launch.runtime, opts)?;
    let Some((runtime_root, version)) = found else {
        return Err(RunError::RuntimeBundleMissing {
            runtime: launch.runtime.clone(),
            paths: searched,
        });
    };
    let (found, app_searched) = find_runtime_bundle(&launch.app_runtime, opts)?;
    let Some((app_root, app_version)) = found else {
        return Err(RunError::RuntimeBundleMissing {
            runtime: launch.app_runtime.clone(),
            paths: app_searched,
        });
    };
    if app_version == version {
        return Err(RunError::RuntimeReferenceMalformed(
            launch.app_runtime.clone(),
        ));
    }
    if !runtime_root.join(BIN).is_file() {
        return Err(RunError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("the runtime {} has no {BIN}", launch.runtime),
        )));
    }
    let storage = std::fs::canonicalize(&launch.storage)?;
    if !storage.is_dir() {
        return Err(RunError::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "the host's storage is no directory",
        )));
    }
    let shim = match &opts.sandbox_shim_bin {
        Some(shim) => shim.clone(),
        None => std::env::current_exe()?,
    };
    let clone3 = stage_seccomp_blob(&crate::seccomp::compile_clone3_compat()?)?;
    let baseline = stage_seccomp_blob(&compile_app_baseline(
        crate::seccomp::SeccompProfile::default(),
        crate::seccomp::SyscallGrants::default(),
    )?)?;
    let mut argv = vec![
        "--add-seccomp-fd".to_owned(),
        CLONE3_COMPAT_FD.to_string(),
        "--add-seccomp-fd".to_owned(),
        BASELINE_FD.to_string(),
    ];
    argv.extend(bwrap_argv(
        (&runtime_root, &version),
        (&app_root, &app_version),
        &storage,
        &shim,
    ));
    let mut command = Command::new(opts.bwrap_bin.clone().unwrap_or_else(|| "bwrap".into()));
    command
        .args(&argv)
        .env_clear()
        .stdin(std::process::Stdio::null());
    let placed = [
        (door.as_raw_fd(), DOOR_CHILD_FD),
        (clone3.as_raw_fd(), CLONE3_COMPAT_FD),
        (baseline.as_raw_fd(), BASELINE_FD),
    ];
    // SAFETY: only async-signal-safe calls between fork and exec: each descriptor is moved
    // above every target slot first, so no dup2 overwrites one not yet placed, then placed at
    // its slot without close-on-exec.
    unsafe {
        command.pre_exec(move || {
            let mut moved = [(0, 0); 3];
            for (slot, (from, to)) in moved.iter_mut().zip(placed) {
                let fd = libc::fcntl(from, libc::F_DUPFD_CLOEXEC, 256);
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                *slot = (fd, to);
            }
            for (from, to) in moved {
                if libc::dup2(from, to) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let status = command.status()?;
    drop((door, clone3, baseline));
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(argv: &[String], flag: &str) -> Vec<(String, String)> {
        argv.windows(3)
            .filter(|window| window[0] == flag)
            .map(|window| (window[1].clone(), window[2].clone()))
            .collect()
    }

    /// The host holds the runtime read-only, its storage, its door, and nothing an app would:
    /// no display, no bus, no service, no network, no home, no app storage.
    #[test]
    fn a_tilolang_host_holds_its_runtime_its_storage_and_its_door_only() {
        let argv = bwrap_argv(
            (
                Path::new("/usr/lib/tilo/runtimes/tilolang/0.1.0"),
                "0.1.0+tilolang",
            ),
            (
                Path::new("/usr/lib/tilo/runtimes/tilo/2026.10.1"),
                "2026.10.1",
            ),
            Path::new("/home/ada/.local/share/tilo/tilolang-hosts/workspace/author"),
            Path::new("/usr/libexec/tilo-launcher"),
        );
        assert!(
            argv.contains(&"--unshare-all".to_owned()),
            "no network, no ipc"
        );
        assert!(argv.contains(&"--clearenv".to_owned()));
        let binds = pair(&argv, "--bind");
        assert_eq!(
            binds,
            [(
                "/home/ada/.local/share/tilo/tilolang-hosts/workspace/author".to_owned(),
                STORAGE_MOUNT.to_owned()
            )],
            "its storage is the one thing it writes"
        );
        let mounted: Vec<String> = pair(&argv, "--ro-bind")
            .into_iter()
            .map(|(_, to)| to)
            .collect();
        assert!(mounted.contains(&"/run/tilo-runtime/0.1.0+tilolang".to_owned()));
        assert!(
            mounted.contains(&"/run/tilo-runtime/2026.10.1".to_owned()),
            "the Tilo runtime its binary runs on"
        );
        for forbidden in ["wayland", "dbus", "bus", "/home", "tilo-svc", "/run/user"] {
            assert!(
                !mounted.iter().any(|to| to.contains(forbidden)),
                "{forbidden} is not in a host's sandbox: {mounted:?}"
            );
        }
        let env = pair(&argv, "--setenv");
        assert!(env.contains(&(
            runtime_portal::tilolang_host::DOOR_FD_ENV.to_owned(),
            "3".to_owned()
        )));
        assert!(
            !env.iter().any(|(name, _)| name.starts_with("TILO_SVC_")
                && name != runtime_portal::tilolang_host::DOOR_FD_ENV
                || name == "WAYLAND_DISPLAY"
                || name == "TILO_WORKSPACE_ID"),
            "no service, display or app identity: {env:?}"
        );
        let shim = argv
            .iter()
            .position(|arg| arg == crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND)
            .expect("Landlock is applied");
        let rest = &argv[shim..];
        assert!(rest.contains(&"--net-restricted".to_owned()));
        assert_eq!(
            rest.windows(2)
                .filter(|w| w[0] == "--keep-fd")
                .map(|w| w[1].clone())
                .collect::<Vec<_>>(),
            ["3"],
            "only the door is kept"
        );
        assert_eq!(
            rest.windows(2)
                .filter(|w| w[0] == "--rw" || w[0] == "--rw-noexec")
                .map(|w| w[1].clone())
                .collect::<Vec<_>>(),
            [STORAGE_MOUNT],
            "it writes only its storage, and runs nothing from it"
        );
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/run/tilo-runtime/0.1.0+tilolang/bin/tilolang-host")
        );
    }
}
