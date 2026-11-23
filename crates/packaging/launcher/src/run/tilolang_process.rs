//! The launch kind "tilolang host": one process per workspace and author that runs that
//! author's tilolang apps in that workspace, each as an instance
//! (`docs/designs/app-language/app-window.md`).
//!
//! It is no app launch. The same sandbox runs the window a device shows on a web page
//! ([`WINDOW_BIN`]), which answers the page's link and draws nothing.

use super::{
    LaunchError, LaunchOptions, compile_app_baseline, find_runtime_bundle, stage_seccomp_blob,
};
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The subcommand the session starts a tilolang host with.
pub const SUBCOMMAND: &str = "tilolang-process";

/// The host's binary in the tilolang runtime bundle.
pub const BIN: &str = "bin/tilolang-process";

/// The window binary in the same bundle, which a page window runs
/// ([`runtime_portal::process_link::PAGE_ENV`]).
pub const WINDOW_BIN: &str = "bin/tilolang-app";

/// Where the host sees its storage.
pub const STORAGE_MOUNT: &str = "/run/tilolang-process/storage";

/// Where the host sees what an earlier version of its app kept on this device, read-only: the root
/// of a store holding only the directory the session granted ([`Earlier`]).
pub const EARLIER_MOUNT: &str = "/run/tilolang-process/earlier";

/// The zone database and the device's zone, which the host's clock reads the person's day by.
pub const ZONE_PATHS: [&str; 2] = ["/usr/share/zoneinfo", "/etc/localtime"];

/// The seccomp filters' descriptors in the sandbox's setup.
const CLONE3_COMPAT_FD: RawFd = crate::services::CLONE3_COMPAT_SECCOMP_CHILD_FD;
const BASELINE_FD: RawFd = crate::services::BASELINE_SECCOMP_CHILD_FD;

/// What a tilolang process is started with.
#[derive(Clone, Debug)]
pub struct ProcessLaunch {
    /// The tilolang runtime, `tilolang/<version>`.
    pub runtime: String,
    /// The Tilo runtime the process's binary runs on, `tilo-runtime/<version>`.
    pub app_runtime: String,
    /// The storage of the process's workspace and author.
    pub storage: PathBuf,
    /// What an earlier version of its app kept on this device, granted read-only.
    pub earlier: Option<Earlier>,
}

/// What an earlier version of a process's app kept on this device, which the session grants the
/// process to read and nothing else: `dir`, the documents of that app in the process's workspace,
/// seen at `under` beneath [`EARLIER_MOUNT`], where the store that wrote them kept them.
#[derive(Clone, Debug)]
pub struct Earlier {
    pub dir: PathBuf,
    /// A relative path of plain names.
    pub under: PathBuf,
}

impl Earlier {
    /// Where the process sees `dir`; `None` for an `under` that is not a relative path of plain
    /// names, which would place it elsewhere.
    #[must_use]
    pub fn mount(&self) -> Option<PathBuf> {
        let plain = self.under.components().next().is_some()
            && self
                .under
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_)));
        plain.then(|| Path::new(EARLIER_MOUNT).join(&self.under))
    }
}

/// The bubblewrap arguments of a process on the verified tilolang runtime bundle `runtime_root` of
/// `version`, running on the verified Tilo runtime `app_runtime` (its root and version), holding
/// `storage`, reading `earlier` (the directory and where the process sees it), with the launcher's
/// own binary at `shim` for Landlock. A `page` window runs [`WINDOW_BIN`] on its patch connection
/// and its page link instead of a process link.
#[must_use]
pub fn bwrap_argv(
    (runtime_root, version): (&Path, &str),
    app_runtime: (&Path, &str),
    storage: &Path,
    earlier: Option<(&Path, &Path)>,
    shim: &Path,
    page: bool,
) -> Vec<String> {
    use runtime_portal::process_link::{CHILD_FD, FD_ENV, PAGE_ENV, PAGE_FD};
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
    let (descriptors, descriptors_named) = if page {
        (PAGE_ENV, format!("{CHILD_FD},{PAGE_FD}"))
    } else {
        (FD_ENV, CHILD_FD.to_string())
    };
    for (name, value) in [
        ("PATH", crate::sandbox::SANDBOX_PATH.to_owned()),
        (descriptors, descriptors_named),
        (
            runtime_portal::process_link::STORAGE_ENV,
            STORAGE_MOUNT.to_owned(),
        ),
    ] {
        argv.extend(["--setenv".to_owned(), name.to_owned(), value]);
    }
    if earlier.is_some() {
        argv.extend([
            "--setenv".to_owned(),
            runtime_portal::process_link::EARLIER_ENV.to_owned(),
            EARLIER_MOUNT.to_owned(),
        ]);
    }
    // The person's language and zone, as an app inherits them: `today` an instance writes and filters
    // by is the day their window heads its rows by.
    for &name in crate::sandbox::INHERITED_ENV {
        if let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            argv.extend([
                "--setenv".to_owned(),
                name.to_owned(),
                value.to_string_lossy().into_owned(),
            ]);
        }
    }
    // The Tilo runtime: the loader, `LD_LIBRARY_PATH` and libraries, as for an app; the tilolang
    // bundle beside it, as an app's entrypoint runtime is.
    crate::sandbox::push_runtime_bundle(&mut argv, Some(app_runtime));
    let _ = crate::sandbox::push_required_runtime(&mut argv, runtime_root, version);
    for zone in ZONE_PATHS {
        argv.extend(["--ro-bind-try", zone, zone].map(str::to_owned));
    }
    argv.extend([
        "--bind".to_owned(),
        storage.display().to_string(),
        STORAGE_MOUNT.to_owned(),
    ]);
    // What an earlier version of the app kept, which the host reads and never writes.
    if let Some((dir, mount)) = earlier {
        argv.extend([
            "--ro-bind".to_owned(),
            dir.display().to_string(),
            mount.display().to_string(),
        ]);
    }
    // Landlock inside the namespace, through the launcher's own binary: read and execute the runtime,
    // write the storage, no TCP; every descriptor but the socket closed.
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
    for zone in ZONE_PATHS {
        argv.extend(["--ro-noexec", zone].map(str::to_owned));
    }
    if let Some((_, mount)) = earlier {
        argv.extend(["--ro-noexec".to_owned(), mount.display().to_string()]);
    }
    argv.extend([
        "--rw-noexec".to_owned(),
        STORAGE_MOUNT.to_owned(),
        "--net-restricted".to_owned(),
        "--keep-fd".to_owned(),
        CHILD_FD.to_string(),
    ]);
    if page {
        argv.extend(["--keep-fd".to_owned(), PAGE_FD.to_string()]);
    }
    argv.extend([
        "--".to_owned(),
        runtime_mount
            .join(if page { WINDOW_BIN } else { BIN })
            .display()
            .to_string(),
    ]);
    argv
}

/// What the session hands a tilolang host for one instance of an installed tilolang app, read from the
/// installation verified as a launch of it verifies it.
#[derive(Debug)]
pub struct VerifiedInstance {
    /// The tilolang runtime its entrypoint names, `tilolang/<version>`.
    pub runtime: String,
    /// The Tilo runtime it names, `tilo-runtime/<version>`.
    pub app_runtime: String,
    /// The identity of the publisher whose key signed the package: the author whose host runs it.
    pub publisher: [u8; 32],
    /// Its module, from the verified payload.
    pub module: runtime_portal::process_link::Module,
}

impl VerifiedInstance {
    /// Whether both runtimes it names are installed here, so a host on them can start: an app installed
    /// for runtimes since replaced runs nowhere until it is built again.
    #[must_use]
    pub fn runs_here(&self) -> bool {
        super::runtime_installed(&self.runtime) && super::runtime_installed(&self.app_runtime)
    }
}

/// The tilolang app `installed`, installed at `manifest_path` with its payload at `payload_root`, whose
/// signed root hashes to `expected`: verified (its files, its signature, its publisher not revoked) and
/// read for a tilolang host.
pub fn verified_instance(
    installed: &package_store::InstalledPackage,
    manifest_path: &Path,
    payload_root: &Path,
    expected: &str,
) -> Result<VerifiedInstance, LaunchError> {
    let (manifest, publisher) =
        super::verified_launch_publisher(installed, manifest_path, payload_root, expected)?;
    let not_tilolang =
        || LaunchError::InvalidServiceLaunch(format!("{} is no tilolang app", manifest.package_id));
    let runtime = manifest
        .entrypoint
        .runtime
        .clone()
        .filter(|runtime| is_tilolang(runtime))
        .ok_or_else(not_tilolang)?;
    let app_runtime = manifest.runtime.clone().ok_or_else(not_tilolang)?;
    let module = runtime_portal::process_link::Module::read(payload_root)?;
    Ok(VerifiedInstance {
        runtime,
        app_runtime,
        publisher,
        module,
    })
}

/// Whether `runtime` (`<name>/<version>`) is a tilolang runtime.
#[must_use]
pub fn is_tilolang(runtime: &str) -> bool {
    runtime
        .split_once('/')
        .is_some_and(|(name, version)| name == "tilolang" && !version.is_empty())
}

/// Start the tilolang host `launch` on `socket` and wait for it to end; or, handed a `page`
/// window's patch connection and page link, that window, while this process keeps `socket`.
pub fn launch_tilolang_process(
    launch: &ProcessLaunch,
    socket: OwnedFd,
    page: Option<&(OwnedFd, OwnedFd)>,
    opts: &LaunchOptions,
) -> Result<std::process::ExitStatus, LaunchError> {
    let (found, searched) = find_runtime_bundle(&launch.runtime, opts)?;
    let Some((runtime_root, version)) = found else {
        return Err(LaunchError::RuntimeBundleMissing {
            runtime: launch.runtime.clone(),
            paths: searched,
        });
    };
    let (found, app_searched) = find_runtime_bundle(&launch.app_runtime, opts)?;
    let Some((app_root, app_version)) = found else {
        return Err(LaunchError::RuntimeBundleMissing {
            runtime: launch.app_runtime.clone(),
            paths: app_searched,
        });
    };
    if app_version == version {
        return Err(LaunchError::RuntimeReferenceMalformed(
            launch.app_runtime.clone(),
        ));
    }
    let bin = if page.is_some() { WINDOW_BIN } else { BIN };
    if !runtime_root.join(bin).is_file() {
        return Err(LaunchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("the runtime {} has no {bin}", launch.runtime),
        )));
    }
    let storage = std::fs::canonicalize(&launch.storage)?;
    if !storage.is_dir() {
        return Err(LaunchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "the host's storage is no directory",
        )));
    }
    // A grant that does not read, or would be seen anywhere but beneath its mount, is none: the
    // host starts without it, and what an earlier version kept waits for a start that reads it.
    let earlier = launch.earlier.as_ref().and_then(|earlier| {
        let dir = std::fs::canonicalize(&earlier.dir)
            .ok()
            .filter(|dir| dir.is_dir())?;
        Some((dir, earlier.mount()?))
    });
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
        earlier
            .as_ref()
            .map(|(dir, mount)| (dir.as_path(), mount.as_path())),
        &shim,
        page.is_some(),
    ));
    let mut command = Command::new(opts.bwrap_bin.clone().unwrap_or_else(|| "bwrap".into()));
    command
        .args(&argv)
        .env_clear()
        .stdin(std::process::Stdio::null());
    // A page window holds its two connections; the link it was started on stays with this
    // process, which ends with the window.
    let (link, page) = match page {
        Some((patch, page)) => (patch.as_raw_fd(), Some(page.as_raw_fd())),
        None => (socket.as_raw_fd(), None),
    };
    let placed = [
        (link, runtime_portal::process_link::CHILD_FD),
        (clone3.as_raw_fd(), CLONE3_COMPAT_FD),
        (baseline.as_raw_fd(), BASELINE_FD),
        (
            page.unwrap_or(link),
            page.map_or(runtime_portal::process_link::CHILD_FD, |_| {
                runtime_portal::process_link::PAGE_FD
            }),
        ),
    ];
    place(&mut command, placed);
    let status = command.status()?;
    drop((socket, clone3, baseline));
    Ok(status)
}

/// Hand `command` each descriptor of `placed` at its slot.
fn place(command: &mut Command, placed: [(RawFd, RawFd); 4]) {
    // SAFETY: only async-signal-safe calls between fork and exec: each descriptor is moved above every
    // target slot first, so no dup2 overwrites one not yet placed, then placed at its slot without
    // close-on-exec.
    unsafe {
        command.pre_exec(move || {
            let mut moved = [(0, 0); 4];
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

    tilo_test_env::scenario! {
        fn a_tilolang_process_runs_installed_runtimes_and_holds_only_its_own;

        phase an_instance_runs_here_only_on_runtimes_that_are_installed;
        // An instance runs here only while both runtimes it names are installed: once a newer build
        // replaced one, the app is no candidate for a host.
        {
            let root = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(runtime_bundle::RUNTIME_DIR_ENV, root.path());
            let baked = "2026.07+native.b16b0fa4317d8b7d24aa5b3569d44fa9";
            for version in [baked, "0.1.0+tilolang"] {
                let dir = root.path().join(version);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME), "").unwrap();
            }
            let instance = |app_runtime: &str| VerifiedInstance {
                runtime: "tilolang/0.1.0+tilolang".to_owned(),
                app_runtime: app_runtime.to_owned(),
                publisher: [0; 32],
                module: runtime_portal::process_link::Module::default(),
            };
            assert!(instance(&format!("tilo-runtime/{baked}")).runs_here());
            assert!(
                !instance("tilo-runtime/2026.07+native.4c4a00000000000000000000000000ff").runs_here(),
                "the runtime an earlier build named, since replaced"
            );
            let mut on_gone_tilolang = instance(&format!("tilo-runtime/{baked}"));
            on_gone_tilolang.runtime = "tilolang/0.0.9+tilolang".to_owned();
            assert!(!on_gone_tilolang.runs_here());
        }

        phase a_tilolang_process_holds_its_runtime_its_storage_and_its_socket_only;
        // The host holds the runtime read-only, its storage, its socket, and nothing an app would: no
        // display, no bus, no service, no network, no home, no app storage.
        {
            let mut zone = tilo_test_env::ScopedEnv::new();
            zone.set("TZ", "America/Los_Angeles");
            // What the native Notes kept of the workspace, which the session grants it to read.
            const NOTEBOOKS: &str = "/home/ada/.local/share/tilo-workspaces/workspace/so.tilo.notes/.local/share/so.tilo.notes/workspaces/workspace";
            let notebooks_mount = Earlier {
                dir: NOTEBOOKS.into(),
                under: "so.tilo.notes/workspaces/workspace".into(),
            }
            .mount()
            .unwrap();
            for outside in ["../so.tilo.mail", "/home/ada", ""] {
                assert!(
                    Earlier {
                        dir: NOTEBOOKS.into(),
                        under: outside.into(),
                    }
                    .mount()
                    .is_none(),
                    "a grant is seen only beneath its mount: {outside:?}"
                );
            }
            let argv = bwrap_argv(
                (
                    Path::new("/usr/lib/tilo/runtimes/tilolang/0.1.0"),
                    "0.1.0+tilolang",
                ),
                (
                    Path::new("/usr/lib/tilo/runtimes/tilo/2026.10.1"),
                    "2026.10.1",
                ),
                Path::new("/home/ada/.local/share/tilo/tilolang-processes/workspace/author"),
                Some((Path::new(NOTEBOOKS), &notebooks_mount)),
                Path::new("/usr/libexec/tilo-launcher"),
                false,
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
                    "/home/ada/.local/share/tilo/tilolang-processes/workspace/author".to_owned(),
                    STORAGE_MOUNT.to_owned()
                )],
                "its storage is the one thing it writes"
            );
            let mounted: Vec<String> = pair(&argv, "--ro-bind")
                .into_iter()
                .map(|(_, to)| to)
                .collect();
            assert!(
                pair(&argv, "--ro-bind").contains(&(
                    NOTEBOOKS.to_owned(),
                    format!("{EARLIER_MOUNT}/so.tilo.notes/workspaces/workspace")
                )),
                "what the native app kept of this workspace, read-only, and nothing else of it"
            );
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
            let read_only = pair(&argv, "--ro-bind-try");
            for zone in ZONE_PATHS {
                assert!(
                    read_only.contains(&(zone.to_owned(), zone.to_owned())),
                    "the person's zone is read where an app reads it: {read_only:?}"
                );
            }
            let env = pair(&argv, "--setenv");
            assert!(
                env.contains(&("TZ".to_owned(), "America/Los_Angeles".to_owned())),
                "the zone the person chose for the session: {env:?}"
            );
            assert!(env.contains(&(
                runtime_portal::process_link::EARLIER_ENV.to_owned(),
                EARLIER_MOUNT.to_owned()
            )));
            assert!(env.contains(&(
                runtime_portal::process_link::FD_ENV.to_owned(),
                runtime_portal::process_link::CHILD_FD.to_string()
            )));
            assert!(
                !env.iter().any(|(name, _)| name.starts_with("TILO_SVC_")
                    && name != runtime_portal::process_link::FD_ENV
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
            assert!(
                rest.windows(2).any(|w| w[0] == "--ro-noexec"
                    && w[1] == notebooks_mount.display().to_string()),
                "it reads what the native app kept, and runs nothing from it"
            );
            assert_eq!(
                rest.windows(2)
                    .filter(|w| w[0] == "--keep-fd")
                    .map(|w| w[1].clone())
                    .collect::<Vec<_>>(),
                [runtime_portal::process_link::CHILD_FD.to_string()],
                "only the socket is kept"
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
                Some("/run/tilo-runtime/0.1.0+tilolang/bin/tilolang-process")
            );

            // A page window: the same sandbox, holding its patch connection and its page link in
            // place of a process link, and running the window binary.
            let window = bwrap_argv(
                (
                    Path::new("/usr/lib/tilo/runtimes/tilolang/0.1.0"),
                    "0.1.0+tilolang",
                ),
                (
                    Path::new("/usr/lib/tilo/runtimes/tilo/2026.10.1"),
                    "2026.10.1",
                ),
                Path::new("/home/ada/.local/share/tilo/tilolang-processes/workspace/.pages"),
                None,
                Path::new("/usr/libexec/tilo-launcher"),
                true,
            );
            let env = pair(&window, "--setenv");
            assert!(env.contains(&(
                runtime_portal::process_link::PAGE_ENV.to_owned(),
                "36,37".to_owned()
            )));
            assert!(
                !env.iter()
                    .any(|(name, _)| name == runtime_portal::process_link::FD_ENV),
                "a page window has no process link: {env:?}"
            );
            assert_eq!(
                window
                    .windows(2)
                    .filter(|w| w[0] == "--keep-fd")
                    .map(|w| w[1].clone())
                    .collect::<Vec<_>>(),
                ["36", "37"]
            );
            assert_eq!(
                pair(&window, "--bind"),
                [(
                    "/home/ada/.local/share/tilo/tilolang-processes/workspace/.pages".to_owned(),
                    STORAGE_MOUNT.to_owned()
                )]
            );
            assert_eq!(
                window.last().map(String::as_str),
                Some("/run/tilo-runtime/0.1.0+tilolang/bin/tilolang-app")
            );
        }
    }
}
