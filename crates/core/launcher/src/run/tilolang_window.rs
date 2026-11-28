//! The launch kind "spare tilolang window": a window process of no app yet, which the session
//! starts in a workspace before any tilolang app's window there is asked for, and binds to the
//! app whose window is asked for then (`tilolang_process::window::spare`). Opening an app's window
//! starts no process.
//!
//! It holds no app: no package, no app data, no app identity. What it holds is what every
//! tilolang window draws with: the tilolang runtime and the Tilo runtime it runs on (their fonts
//! among them), its workspace's render service, the filtered accessibility bus, and the link the
//! session hands it with its release, over which it is sent its Wayland connection and bound.

use super::{
    LaunchError, LaunchOptions, compile_app_baseline, find_runtime_bundle, stage_seccomp_blob,
};
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::path::Path;
use std::process::Command;

/// The subcommand the session starts a spare window with.
pub const SUBCOMMAND: &str = "tilolang-window";

/// The seccomp filters' descriptors in the sandbox's setup.
const CLONE3_COMPAT_FD: RawFd = crate::services::CLONE3_COMPAT_SECCOMP_CHILD_FD;
const BASELINE_FD: RawFd = crate::services::BASELINE_SECCOMP_CHILD_FD;

/// Where the window's home and runtime directory are: empty, and gone with the window.
const HOME: &str = "/run/tilo/window";

/// What a spare window is started with.
#[derive(Clone, Debug)]
pub struct WindowLaunch {
    /// The tilolang runtime, `tilolang/<version>`.
    pub runtime: String,
    /// The Tilo runtime its binary runs on, `tilo-runtime/<version>`.
    pub app_runtime: String,
    /// The workspace whose apps it is bound to one of.
    pub workspace: String,
}

/// The sandbox's parts that are not bubblewrap's own: the render service's connection, its
/// descriptor in the sandbox. What assistive technology reads of the window goes to the session
/// over the window's shell-ui connection, never a bus of the sandbox's.
#[derive(Debug, Default)]
pub struct WindowServices {
    pub render: Option<RawFd>,
}

/// The bubblewrap arguments of a spare window on the verified tilolang runtime bundle
/// `runtime_root` of `version`, running on the verified Tilo runtime `app_runtime` (its root and
/// version), in `workspace`, reaching `services`, with the launcher's own binary at `shim` for
/// Landlock.
#[must_use]
pub fn bwrap_argv(
    (runtime_root, version): (&Path, &str),
    app_runtime: (&Path, &str),
    workspace: &str,
    services: &WindowServices,
    shim: &Path,
) -> Vec<String> {
    use runtime_portal::process_link::{CHILD_FD, FD_ENV, SPARE_ENV};
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
        "--tmpfs",
        HOME,
    ]
    .iter()
    .map(|arg| (*arg).to_owned())
    .collect();
    crate::sandbox::push_clean_environment(&mut argv);
    for (name, value) in [
        (FD_ENV.to_owned(), CHILD_FD.to_string()),
        (SPARE_ENV.to_owned(), "1".to_owned()),
        ("HOME".to_owned(), HOME.to_owned()),
        ("XDG_RUNTIME_DIR".to_owned(), HOME.to_owned()),
        (
            crate::sandbox::WORKSPACE_ID_ENV.to_owned(),
            workspace.to_owned(),
        ),
    ] {
        argv.extend(["--setenv".to_owned(), name, value]);
    }
    if let Some(render) = services.render {
        argv.extend([
            "--setenv".to_owned(),
            crate::sandbox::svc_env_var(tilo_render_protocol::broker::SERVICE),
            render.to_string(),
        ]);
    }
    // A window of no app yet may be bound to any tilolang app, and an app may draw with a
    // runtime of its own (the Browser's Chromium engine): the person's store of bundles stands
    // where a sandbox looks for one, read-only, and is what the window may read and run.
    let store = runtime_bundle::bundle_root()
        .ok()
        .filter(|root| root.is_dir());
    if let Some(store) = &store {
        crate::sandbox::push_runtime_store(&mut argv, store);
    }
    crate::sandbox::push_runtime_bundle(&mut argv, Some(app_runtime));
    let _ = crate::sandbox::push_required_runtime(&mut argv, runtime_root, version);
    crate::profile::push_system_profile(&mut argv, false);
    // Landlock inside the namespace, through the launcher's own binary: read and execute the
    // runtimes, read the system profile, write only its empty home, no TCP; every descriptor but
    // its link and its render connection closed.
    argv.extend([
        "--ro-bind".to_owned(),
        shim.display().to_string(),
        super::SHIM_MOUNT_PATH.to_owned(),
        super::SHIM_MOUNT_PATH.to_owned(),
        crate::landlock_shim::SANDBOX_EXEC_SUBCOMMAND.to_owned(),
        "--ro".to_owned(),
        runtime_mount.display().to_string(),
    ]);
    if store.is_some() {
        argv.extend([
            "--ro".to_owned(),
            crate::sandbox::RUNTIME_STORE_MOUNT.to_owned(),
        ]);
    }
    super::push_runtime_landlock_rules(&mut argv, Some(app_runtime));
    for path in crate::profile::landlock_ro_paths(false) {
        argv.extend(["--ro".to_owned(), path.display().to_string()]);
    }
    argv.extend([
        "--rw-noexec".to_owned(),
        HOME.to_owned(),
        "--net-restricted".to_owned(),
        "--keep-fd".to_owned(),
        CHILD_FD.to_string(),
    ]);
    if let Some(render) = services.render {
        argv.extend(["--keep-fd".to_owned(), render.to_string()]);
    }
    argv.extend([
        "--".to_owned(),
        runtime_mount
            .join(crate::manifest::TILOLANG_BIN)
            .display()
            .to_string(),
    ]);
    argv
}

/// Start the spare window `launch` on `link` and wait for it to end.
pub fn launch_tilolang_window(
    launch: &WindowLaunch,
    link: OwnedFd,
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
    let bin = crate::manifest::TILOLANG_BIN;
    if !runtime_root.join(bin).is_file() {
        return Err(LaunchError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("the runtime {} has no {bin}", launch.runtime),
        )));
    }
    // The workspace's render service, asked for by the workspace: the window draws for whichever
    // of its apps it is bound to.
    let render = crate::services::resolve_render_bind(
        SUBCOMMAND,
        Some(&launch.workspace),
        opts.no_services,
    )?;
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
        &launch.workspace,
        &WindowServices {
            render: render.as_ref().map(|render| render.child_fd),
        },
        &shim,
    ));
    let mut command = Command::new(opts.bwrap_bin.clone().unwrap_or_else(|| "bwrap".into()));
    command
        .args(&argv)
        .env_clear()
        .stdin(std::process::Stdio::null());
    let mut placed = vec![
        (link.as_raw_fd(), runtime_portal::process_link::CHILD_FD),
        (clone3.as_raw_fd(), CLONE3_COMPAT_FD),
        (baseline.as_raw_fd(), BASELINE_FD),
    ];
    if let Some(render) = &render {
        placed.push((render.host.as_raw_fd(), render.child_fd));
    }
    crate::inherit::hand_down(&mut command, placed, false);
    let status = command.status()?;
    drop((link, clone3, baseline, render));
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(argv: &[String], flag: &str) -> Vec<String> {
        argv.windows(2)
            .filter(|window| window[0] == flag)
            .map(|window| window[1].clone())
            .collect()
    }

    fn set(argv: &[String], name: &str) -> Option<String> {
        argv.windows(3)
            .find(|window| window[0] == "--setenv" && window[1] == name)
            .map(|window| window[2].clone())
    }

    #[test]
    fn a_spare_window_holds_its_runtimes_its_link_and_no_app() {
        let argv = bwrap_argv(
            (Path::new("/system/tilolang"), "0.1.0+tilolang"),
            (Path::new("/system/tilo-runtime"), "0.1.0"),
            "c0ffee00-0000-4000-8000-000000000001",
            &WindowServices { render: Some(9) },
            Path::new("/usr/libexec/tilo-launcher"),
        );
        assert_eq!(
            set(&argv, runtime_portal::process_link::SPARE_ENV).as_deref(),
            Some("1")
        );
        assert_eq!(
            set(&argv, crate::sandbox::WORKSPACE_ID_ENV).as_deref(),
            Some("c0ffee00-0000-4000-8000-000000000001")
        );
        assert_eq!(set(&argv, "TILO_SVC_RENDER_FD").as_deref(), Some("9"));
        assert_eq!(
            values(&argv, "--keep-fd"),
            [
                runtime_portal::process_link::CHILD_FD.to_string(),
                "9".to_owned()
            ]
        );
        // Nothing of an app: no package, no app data, no app identity, no display socket.
        for name in [
            "TILO_PACKAGE_ID",
            "TILO_APP_MODULE",
            "TILO_SVC_IDENTITY_FD",
            "TILO_SVC_COLLAB_FD",
            "WAYLAND_DISPLAY",
        ] {
            assert_eq!(set(&argv, name), None, "{name} is no spare window's");
        }
        assert_eq!(values(&argv, "--rw-noexec"), [HOME]);
        assert!(argv.iter().any(|arg| arg == "--net-restricted"));
        assert_eq!(
            argv.last().map(String::as_str),
            Some("/run/tilo-runtime/0.1.0+tilolang/bin/tilolang-process")
        );
    }
}
