//! Launching an application Tilo did not package.
//!
//! Every other module in this crate exists to build a sandbox. This one
//! exists because there are applications for which that is the wrong
//! thing to do, and doing it anyway produces an app that does not start
//! rather than an app that is safer.
//!
//! ## Why a host application is not sandboxed
//!
//! A `.desktop` entry is a text file naming a command line, not a
//! package manifest: nothing in it says which paths, devices, services
//! or network hosts the program needs. Tilo's sandbox is *derived* from
//! those declarations — [`crate::sandbox`] turns a manifest into bind
//! mounts, [`crate::seccomp`] into a syscall filter, [`crate::landlock`]
//! into an FS ruleset. With no manifest there's nothing to derive, and a
//! sandbox invented here would be a guess presented as policy.
//!
//! For a Flatpak specifically it's not even a guess that could work:
//! `flatpak run` builds its own sandbox, needing `unshare(2)`,
//! `mount(2)` and `pivot_root(2)`, all of which [`crate::seccomp`]'s
//! baseline denies, plus `/usr/bin/flatpak`, its installation tree and
//! system D-Bus (none of which Tilo's sandbox binds), plus a network
//! namespace, which `--unshare-all` removes. Nesting the two doesn't
//! double the confinement — it produces `bwrap: execvp flatpak: No such
//! file or directory`, or a seccomp kill.
//!
//! ## What the user is therefore told
//!
//! A host application runs with the user's own identity, files and
//! network, exactly as under any other desktop: no `TILO_WORKSPACE_ID`
//! (no collaboration room, no per-room partition), no capability file
//! (workspace grants describe what a *sandboxed* app may reach, and
//! passing them here would assert a limit that doesn't exist), and no
//! `TILO_APP_DIR` (it writes to the user's real `$HOME`).
//!
//! Those absences are why the desktop labels host applications
//! separately and why this module prints a one-line notice naming the
//! app it's about to run unconfined — the risk managed is not escape
//! (there was never a sandbox to escape), it's a user who can't tell
//! the two kinds of application apart.
//!
//! ## What *is* controlled
//!
//! The environment. See [`desktop_entry::launch`]: the child is spawned
//! from an explicit allowlist plus the Wayland handoff and, when available,
//! one authenticated rootless-Xwayland fallback, so the
//! compositor's own `TILO_*` variables don't leak into a process not
//! subject to them.
//!
//! And the lifetime. Each launch runs in its own transient
//! `app-tilo-<id>-<pid>.scope` of the systemd user manager, part of the
//! graphical session target, so a logout or a compositor crash stops the
//! application and everything it forked instead of leaving it running with
//! no display, and resource accounting and OOM handling see one app as one
//! unit. A manager that is unreachable or refuses costs the scope, never
//! the launch.

use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use desktop_entry::launch::{HostLaunch, HostLaunchError, LaunchEnv, plan_by_id, plan_file};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Plan(#[from] HostLaunchError),
    #[error("spawning {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
}

impl HostError {
    pub fn to_exit_code(&self) -> i32 {
        2
    }
}

/// Resolve `desktop_id` on this system and run it, unconfined. Blocks
/// until the application exits, relaying its exit status — the same
/// shape as [`crate::run::run_with`], so the compositor's launch worker
/// treats both kinds of launch identically.
///
/// # Errors
///
/// Returns [`HostError::Plan`] if `desktop_id` cannot be resolved, or
/// [`HostError::Spawn`] if the resolved program cannot be spawned or waited
/// on.
pub fn run_host(desktop_id: &str, env: &LaunchEnv) -> Result<ExitStatus, HostError> {
    let launch = plan_host(desktop_id, env)?;
    spawn(&launch)
}

/// Resolve `desktop_id` into the exact command line and environment a
/// launch would use, without starting anything — so "what will this
/// actually run, and with what?" is answerable before it runs, the
/// question that matters most for a launch with no sandbox around it.
///
/// # Errors
///
/// Returns [`HostError::Plan`] if `desktop_id` does not resolve to an
/// installed `.desktop` entry.
pub fn plan_host(desktop_id: &str, env: &LaunchEnv) -> Result<HostLaunch, HostError> {
    Ok(plan_by_id(desktop_id, env)?)
}

/// [`plan_host`] for a desktop file named by path, such as an XDG autostart entry.
///
/// # Errors
///
/// Returns [`HostError::Plan`] if the file is unreadable or not a runnable entry.
pub fn plan_host_file(path: &std::path::Path, env: &LaunchEnv) -> Result<HostLaunch, HostError> {
    Ok(plan_file(path, env)?)
}

/// Run an already-planned launch. Split out so a caller that built the
/// plan itself doesn't have to resolve the entry twice.
///
/// # Errors
///
/// Returns [`HostError::Spawn`] if `launch`'s program cannot be spawned or
/// waited on.
pub fn spawn(launch: &HostLaunch) -> Result<ExitStatus, HostError> {
    let busctl = Path::new(BUSCTL);
    spawn_with(
        launch,
        busctl.is_file().then_some(busctl),
        SCOPE_JOIN_TIMEOUT,
    )
}

fn spawn_with(
    launch: &HostLaunch,
    busctl: Option<&Path>,
    scope_join_timeout: Duration,
) -> Result<ExitStatus, HostError> {
    // Deliberately on stderr and unconditional: a launch that skips the
    // sandbox says so where the compositor's log, journal and a
    // developer's terminal will all see it.
    eprintln!(
        "tilo-launcher: running host application {} ({}) UNCONFINED — \
         no Tilo sandbox, no workspace room, no per-room state. {} {}",
        launch.desktop_id,
        launch.source.label(),
        launch.program.display(),
        launch.args.join(" "),
    );
    debug_assert!(
        !launch.is_confined(),
        "a host launch must never claim confinement"
    );

    if let Some(busctl) = busctl
        && let Err(error) = join_app_scope(busctl, launch, scope_join_timeout)
    {
        eprintln!(
            "tilo-launcher: {} runs outside a session scope: {error}",
            launch.desktop_id
        );
    }
    let mut cmd = launch.to_command();
    cmd.stdin(Stdio::null());
    configure_host_launch_session(&mut cmd);
    let mut child = cmd.spawn().map_err(|source| HostError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })?;
    child.wait().map_err(|source| HostError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })
}

/// The systemd user manager's D-Bus front end.
const BUSCTL: &str = "/usr/bin/busctl";

/// Stopping this target at logout stops every host application's scope.
const SESSION_TARGET: &str = "tilo-graphical-session.target";

/// The longest unit name systemd accepts.
const UNIT_NAME_MAX: usize = 255;

/// How long the manager gets to move this process once it accepted the
/// scope. It runs the start job right after answering, so a healthy manager
/// needs milliseconds.
const SCOPE_JOIN_TIMEOUT: Duration = Duration::from_secs(1);

/// Move this launcher into a new `app-tilo-<id>-<pid>.scope` before it
/// forks the app, so the app and everything it forks start inside the scope
/// rather than being chased into it afterwards. `StartTransientUnit`
/// answers once the start job is queued, before the move, hence the wait
/// on this process's own cgroup.
fn join_app_scope(busctl: &Path, launch: &HostLaunch, timeout: Duration) -> Result<(), String> {
    let pid = std::process::id();
    let unit = scope_unit_name(&launch.desktop_id, pid)
        .ok_or_else(|| "its desktop id cannot name a unit".to_string())?;
    let output = Command::new(busctl)
        .args(scope_request_args(&unit, pid, &launch.display_name))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("running {}: {error}", busctl.display()))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let deadline = Instant::now().checked_add(timeout);
    loop {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        if cgroup_is_unit(&cgroup, &unit) {
            return Ok(());
        }
        if deadline.is_none_or(|deadline| Instant::now() >= deadline) {
            return Err(format!(
                "{unit} was accepted but not joined within {timeout:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// `busctl` arguments for `StartTransientUnit` of a scope holding `pid`.
fn scope_request_args(unit: &str, pid: u32, display_name: &str) -> Vec<String> {
    let mut properties: Vec<Vec<String>> = vec![
        vec!["PIDs".into(), "au".into(), "1".into(), pid.to_string()],
        vec!["Slice".into(), "s".into(), "app.slice".into()],
        vec![
            "CollectMode".into(),
            "s".into(),
            "inactive-or-failed".into(),
        ],
        vec![
            "PartOf".into(),
            "as".into(),
            "1".into(),
            SESSION_TARGET.into(),
        ],
    ];
    let description: String = display_name.chars().filter(|c| !c.is_control()).collect();
    if !description.trim().is_empty() {
        properties.push(vec!["Description".into(), "s".into(), description]);
    }
    let mut args: Vec<String> = [
        "--user",
        "--timeout=2",
        "call",
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
        "StartTransientUnit",
        "ssa(sv)a(sa(sv))",
    ]
    .map(String::from)
    .to_vec();
    args.push(unit.to_string());
    args.push("fail".into());
    args.push(properties.len().to_string());
    args.extend(properties.into_iter().flatten());
    args.push("0".into());
    args
}

/// Whether `/proc/self/cgroup` places this process in `unit`.
fn cgroup_is_unit(cgroup: &str, unit: &str) -> bool {
    cgroup
        .lines()
        .any(|line| line.rsplit('/').next() == Some(unit))
}

/// `app-tilo-<desktop id>-<pid>.scope`, the name the desktop-environment
/// convention (<https://systemd.io/DESKTOP_ENVIRONMENTS>) gives a launched
/// app, with the id escaped as a unit name must be: `-` separates the name's
/// fields, so it and anything else outside `[A-Za-z0-9:_.]` becomes `\xNN`.
/// `None` when the result is too long to be a unit name.
fn scope_unit_name(desktop_id: &str, pid: u32) -> Option<String> {
    use std::fmt::Write as _;
    let mut escaped = String::with_capacity(desktop_id.len());
    for byte in desktop_id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'.') {
            escaped.push(char::from(byte));
        } else {
            let _ = write!(escaped, "\\x{byte:02x}");
        }
    }
    let unit = format!("app-tilo-{escaped}-{pid}.scope");
    (!escaped.is_empty() && unit.len() <= UNIT_NAME_MAX).then_some(unit)
}

/// Put each host launch in its own session before its desktop entry is
/// executed.  This gives Xwayland bridges a bounded, launch-scoped ancestry
/// domain when applications such as Electron create renderer processes and
/// process groups whose immediate environment no longer contains the startup
/// token.
///
/// This is deliberately established by the production launcher, rather than
/// inferred from the compositor's session: a host app must never share
/// its correlation domain with an unrelated desktop process.
fn configure_host_launch_session(cmd: &mut Command) {
    // SAFETY: `setsid()` is async-signal-safe and this closure captures
    // no Rust state.  It runs in the child after fork and before exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{LaunchMode, RunError, RunOptions};

    /// A perfectly valid package manifest — the refusal being tested has
    /// to come from the launch *mode*, not from anything wrong with the
    /// package.
    fn example_manifest() -> crate::manifest::MinimalManifest {
        toml::from_str(
            "[package]\nid = \"so.tilo.example\"\nversion = \"1.0.0\"\n\
             [entrypoint]\nbin = \"bin/example\"\n",
        )
        .expect("the fixture manifest parses")
    }

    #[test]
    fn host_launch_is_refused_before_anything_is_built() {
        // Phase `run_with_refuses_a_host_launch_before_it_builds_anything`.
        // The guard that keeps a host application out of the sandbox
        // builder. Drives the real `run_with`, not a copy of its check.
        {
            let manifest = example_manifest();
            let opts = RunOptions {
                mode: LaunchMode::Host {
                    desktop_id: "com.spotify.Client".into(),
                },
                ..Default::default()
            };
            let err = crate::run::run_with(&manifest, std::path::Path::new("/nonexistent"), &opts)
                .expect_err("a host application is not a package");
            match err {
                RunError::HostAppNotAPackage { desktop_id } => {
                    assert_eq!(desktop_id, "com.spotify.Client");
                }
                other => panic!("expected HostAppNotAPackage, got {other:?}"),
            }
        }
        // Phase `run_with_captured_refuses_it_too`.
        {
            let manifest = example_manifest();
            let opts = RunOptions {
                mode: LaunchMode::Host {
                    desktop_id: "org.gnome.TextEditor".into(),
                },
                ..Default::default()
            };
            let err = crate::run::run_with_captured(
                &manifest,
                std::path::Path::new("/nonexistent"),
                &opts,
            )
            .expect_err("the second door into the sandbox builder is closed too");
            assert!(matches!(err, RunError::HostAppNotAPackage { .. }));
        }
        // Phase `an_unknown_desktop_id_is_reported`.
        // A launch of something that is not installed fails with a name,
        // not a silent no-op.
        {
            let err = run_host(
                "so.tilo.definitely-not-installed-anywhere",
                &LaunchEnv::default(),
            )
            .expect_err("nothing has that id");
            assert!(
                err.to_string()
                    .contains("so.tilo.definitely-not-installed-anywhere"),
                "the error must name what could not be found: {err}"
            );
        }
    }

    #[test]
    fn a_refused_or_unjoined_scope_never_costs_the_launch() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("scratch directory");
        let entry = dir.path().join("org.example.Probe-x.desktop");
        std::fs::write(
            &entry,
            "[Desktop Entry]\nType=Application\nName=Probe\nExec=sh -c \"exit 7\"\n",
        )
        .expect("write the probe entry");
        let launch = plan_host_file(&entry, &LaunchEnv::default()).expect("the probe plans");
        let stand_in = |name: &str, status: u8| {
            let path = dir.path().join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.argv\"\n\
                     echo 'Access denied' >&2\nexit {status}\n"
                ),
            )
            .expect("write the stand-in busctl");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("make the stand-in executable");
            path
        };
        // Phase `the_manager_is_asked_for_this_launchers_scope`.
        // The stand-in accepts but never moves the process, like a manager
        // whose start job fails after answering.
        {
            let busctl = stand_in("accepting-busctl", 0);
            let status = spawn_with(&launch, Some(&busctl), Duration::from_millis(20))
                .expect("the launch runs");
            assert_eq!(
                status.code(),
                Some(7),
                "the app's own exit status reaches the caller"
            );
            let argv = std::fs::read_to_string(dir.path().join("accepting-busctl.argv"))
                .expect("the manager was asked");
            let pid = std::process::id();
            assert_eq!(
                argv.lines().map(str::to_string).collect::<Vec<_>>(),
                scope_request_args(
                    &format!("app-tilo-org.example.Probe\\x2dx-{pid}.scope"),
                    pid,
                    "Probe"
                )
            );
        }
        // Phase `a_refusal_still_starts_the_app`.
        {
            let busctl = stand_in("refusing-busctl", 1);
            let status = spawn_with(&launch, Some(&busctl), Duration::from_millis(20))
                .expect("the launch runs");
            assert_eq!(status.code(), Some(7));
            assert!(
                join_app_scope(&busctl, &launch, Duration::from_millis(20))
                    .expect_err("a refusal is reported")
                    .contains("Access denied"),
                "the manager's own reason is kept for the log"
            );
        }
        // Phase `without_busctl_the_app_starts_directly`.
        {
            let status =
                spawn_with(&launch, None, Duration::from_millis(20)).expect("the launch runs");
            assert_eq!(status.code(), Some(7));
        }
    }

    #[test]
    fn a_scope_request_names_its_unit_slice_session_and_process() {
        assert_eq!(
            scope_request_args("app-tilo-com.spotify.Client-42.scope", 42, "Spo\ntify"),
            [
                "--user",
                "--timeout=2",
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "StartTransientUnit",
                "ssa(sv)a(sa(sv))",
                "app-tilo-com.spotify.Client-42.scope",
                "fail",
                "5",
                "PIDs",
                "au",
                "1",
                "42",
                "Slice",
                "s",
                "app.slice",
                "CollectMode",
                "s",
                "inactive-or-failed",
                "PartOf",
                "as",
                "1",
                "tilo-graphical-session.target",
                "Description",
                "s",
                "Spotify",
                "0",
            ]
        );
        assert_eq!(
            scope_unit_name("com.spotify.Client", 42).as_deref(),
            Some("app-tilo-com.spotify.Client-42.scope")
        );
        assert_eq!(
            scope_unit_name("org.gnome.Text-Editor", 1).as_deref(),
            Some("app-tilo-org.gnome.Text\\x2dEditor-1.scope")
        );
        assert_eq!(
            scope_unit_name("a b/c", 1).as_deref(),
            Some("app-tilo-a\\x20b\\x2fc-1.scope")
        );
        assert_eq!(scope_unit_name("", 1), None);
        assert_eq!(scope_unit_name(&"x".repeat(250), 1), None);
        assert!(cgroup_is_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-tilo-a\\x2db-7.scope\n",
            "app-tilo-a\\x2db-7.scope"
        ));
        assert!(!cgroup_is_unit(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-cursor-1.scope\n",
            "app-tilo-a\\x2db-7.scope"
        ));
    }

    #[test]
    fn host_child_becomes_its_own_session_leader() {
        struct ReapChild(std::process::Child);

        impl Drop for ReapChild {
            fn drop(&mut self) {
                let pid = i32::try_from(self.0.id()).unwrap_or(i32::MAX);
                // SAFETY: this is the bounded child created by this test.
                let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
                let _ = self.0.wait();
            }
        }

        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        configure_host_launch_session(&mut command);
        let child = ReapChild(command.spawn().expect("spawn grouped host child"));
        let pid = i32::try_from(child.0.id()).expect("child pid fits i32");
        // SAFETY: querying the session of our own still-running bounded child
        // is read-only. `sleep 30` keeps the PID valid through the assertion.
        assert_eq!(
            unsafe { libc::getsid(pid) },
            pid,
            "the production host launch configuration must isolate a fresh session"
        );
    }
}
