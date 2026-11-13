//! Launching an application Tilo did not package.
//!
//! Every other module in this crate exists to build a sandbox.

use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use crate::scope;
use desktop_entry::launch::{HostLaunch, HostLaunchError, LaunchEnv, plan_by_id, plan_file};

/// The `Name[xx]` candidates of the person's interface language: the entry a host app starts from is
/// named in it, so a German machine with English chosen says `Files` rather than `Dateien` in what it
/// reports.
#[must_use]
pub fn locales() -> Vec<String> {
    desktop_entry::locale::language_candidates(
        settings_store::appearance::preferred_language().as_deref(),
    )
}

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

/// Resolve `desktop_id` on this system and run it, unconfined.
pub fn run_host(desktop_id: &str, env: &LaunchEnv) -> Result<ExitStatus, HostError> {
    let launch = plan_host(desktop_id, env, &locales())?;
    spawn(&launch)
}

/// Resolve `desktop_id` into the exact command line and environment a launch would use, without
/// starting anything — so "what will this actually run, and with what?" is answerable before it runs,
/// the question that matters most for a launch with no sandbox around it.
pub fn plan_host(
    desktop_id: &str,
    env: &LaunchEnv,
    locales: &[String],
) -> Result<HostLaunch, HostError> {
    Ok(plan_by_id(desktop_id, env, locales)?)
}

/// [`plan_host`] for a desktop file named by path, such as an XDG autostart entry.
pub fn plan_host_file(
    path: &std::path::Path,
    env: &LaunchEnv,
    locales: &[String],
) -> Result<HostLaunch, HostError> {
    Ok(plan_file(path, env, locales)?)
}

/// [`plan_host`] for a program file run by itself, such as a downloaded `AppImage` opened in Files.
pub fn plan_host_program(path: &Path, env: &LaunchEnv) -> Result<HostLaunch, HostError> {
    Ok(desktop_entry::launch::plan_program(path, env)?)
}

/// Make the program file at `path` runnable, as opening it does.
pub fn make_runnable(path: &Path) -> Result<(), HostError> {
    Ok(desktop_entry::launch::make_runnable(path)?)
}

/// Run an already-planned launch.
pub fn spawn(launch: &HostLaunch) -> Result<ExitStatus, HostError> {
    crate::chromium_frame::prepare(launch);
    // A launcher already inside an app's cgroup (an app running a program of its own) stays in it and
    // asks the manager for nothing.
    let busctl = Path::new(scope::BUSCTL);
    let ask_manager = busctl.is_file() && !crate::app_cgroup::current_is_app_cgroup();
    spawn_with(launch, ask_manager.then_some(busctl), scope::JOIN_TIMEOUT)
}

fn spawn_with(
    launch: &HostLaunch,
    busctl: Option<&Path>,
    scope_join_timeout: Duration,
) -> Result<ExitStatus, HostError> {
    // Deliberately on stderr and unconditional: a launch that skips the sandbox says so where the
    // compositor's log, journal and a developer's terminal will all see it.
    eprintln!(
        "tilo-launcher: running host application {} ({}) UNCONFINED — \
         no Tilo sandbox, no workspace, no per-workspace state. {} {}",
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
        && let Err(error) = scope::join_with(
            busctl,
            &launch.desktop_id,
            &launch.display_name,
            scope_join_timeout,
        )
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

/// Put each host launch in its own session before its desktop entry is executed.
fn configure_host_launch_session(cmd: &mut Command) {
    // SAFETY: `setsid()` is async-signal-safe and this closure captures no Rust state.
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
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A perfectly valid package manifest — the refusal being tested has to come from the launch
    /// *mode*, not from anything wrong with the package.
    fn example_manifest() -> crate::manifest::ManifestView {
        crate::manifest::test_manifest(
            "id = \"so.tilo.example\"\nversion = \"1.0.0\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/example\"\n",
        )
    }

    #[test]
    fn host_launch_is_refused_before_anything_is_built() {
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
        let launch = plan_host_file(&entry, &LaunchEnv::default(), &[]).expect("the probe plans");
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
                scope::request_args(
                    &format!("app-tilo-org.example.Probe\\x2dx-{pid}.scope"),
                    pid,
                    "Probe"
                )
            );
        }
        {
            let busctl = stand_in("refusing-busctl", 1);
            let status = spawn_with(&launch, Some(&busctl), Duration::from_millis(20))
                .expect("the launch runs");
            assert_eq!(status.code(), Some(7));
            assert!(
                scope::join_with(
                    &busctl,
                    &launch.desktop_id,
                    &launch.display_name,
                    Duration::from_millis(20)
                )
                .expect_err("a refusal is reported")
                .contains("Access denied"),
                "the manager's own reason is kept for the log"
            );
        }
        {
            let status =
                spawn_with(&launch, None, Duration::from_millis(20)).expect("the launch runs");
            assert_eq!(status.code(), Some(7));
        }
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
        let child = ReapChild(command.spawn_locked().expect("spawn grouped host child"));
        let pid = i32::try_from(child.0.id()).expect("child pid fits i32");
        // SAFETY: querying the session of our own still-running bounded child is read-only.
        assert_eq!(
            unsafe { libc::getsid(pid) },
            pid,
            "the production host launch configuration must isolate a fresh session"
        );
    }
}
