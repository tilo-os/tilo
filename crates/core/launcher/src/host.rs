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
    spawn(&launch, None)
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

/// An app started behind an app keeper (`tilo-app-keeper`): the keeper is the app's Wayland
/// display and keeps its windows open while the desktop restarts in place.
///
/// The launcher starts the keeper once it is in the app's scope, so the keeper is inside the
/// scope (the group OOM kill and a logout's stop end both) and the app descends from it (a
/// window's process is traced back to this launch). The launcher stays as their parent until the
/// keeper ends, relaying what both write: the session reading those streams goes away in a
/// restart, and a write into a pipe nobody reads would end the app.
#[derive(Clone, Debug)]
pub struct Keep {
    /// The keeper program.
    pub keeper: std::path::PathBuf,
    /// The keeper's private directory.
    pub dir: std::path::PathBuf,
    /// What the session names this launch, handed to the keeper as given.
    pub launch: String,
}

/// Run an already-planned launch, behind a keeper when `keep` names one.
pub fn spawn(launch: &HostLaunch, keep: Option<&Keep>) -> Result<ExitStatus, HostError> {
    crate::chromium_frame::prepare(launch);
    // A launcher already inside an app's cgroup (an app running a program of its own) stays in it and
    // asks the manager for nothing, and neither does one whose session owns no machine: the manager
    // it would ask is the desktop's around it, and its scopes are that desktop's to end.
    let busctl = Path::new(scope::BUSCTL);
    let ask_manager = busctl.is_file()
        && !crate::app_cgroup::current_is_app_cgroup()
        && tilo_machine::owns_the_machine();
    spawn_with(
        launch,
        ask_manager.then_some(busctl),
        scope::JOIN_TIMEOUT,
        keep,
    )
}

/// End this process by `signal`, as the app it launched ended: its default action, whatever this
/// process set for it.
pub fn end_by(signal: i32) {
    // SAFETY: resetting a signal to its default action and raising it take plain integers.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
        libc::raise(signal);
    }
}

fn spawn_with(
    launch: &HostLaunch,
    busctl: Option<&Path>,
    scope_join_timeout: Duration,
    keep: Option<&Keep>,
) -> Result<ExitStatus, HostError> {
    // A kept launch's streams carry its keeper's output alone: the desktop that follows reads a
    // kept app's file by the count of bytes its launcher passed on, and a line of this process's
    // own would be counted as the app's and hide the first bytes of it after a restart.
    let kept = keep.is_some();
    // Deliberately on stderr and otherwise unconditional: a launch that skips the sandbox says so
    // where the compositor's log, journal and a developer's terminal will all see it.
    if !kept {
        eprintln!(
            "tilo-launcher: running machine application {} ({}) UNCONFINED — \
             no Tilo sandbox, no workspace, no per-workspace state. {} {}",
            launch.desktop_id,
            launch.source.label(),
            launch.program.display(),
            launch.args.join(" "),
        );
    }
    debug_assert!(
        !launch.is_confined(),
        "a host launch must never claim confinement"
    );

    if let Some(busctl) = busctl
        && !kept
        && let Err(error) = scope::join_with(
            busctl,
            &launch.desktop_id,
            &launch.display_name,
            scope::Oom::OneProcess,
            scope_join_timeout,
        )
    {
        eprintln!(
            "tilo-launcher: {} runs outside a session scope: {error}",
            launch.desktop_id
        );
    }
    if let Some(keep) = keep {
        let cmd = launch.to_command();
        // An app the session serves X11 gets a display of its keeper's instead of the desktop's,
        // whose server would end with the desktop and take the app's X11 windows with it.
        let x11 = cmd
            .get_envs()
            .any(|(key, value)| key == "DISPLAY" && value.is_some());
        return spawn_kept(&cmd, x11, keep);
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

/// Start a program the compatibility service planned (a Windows program's sandbox, `command`)
/// behind the keeper `keep` names, in a scope of its own named for `app_id` and `name` as a
/// machine app's is, with an X11 display of the keeper's when `x11`.
///
/// The descriptors the launcher inherited reach the program unchanged, as does the launcher's
/// own environment: the sandbox's environment is in its arguments.
pub fn start_kept(
    app_id: &str,
    name: &str,
    command: &[std::ffi::OsString],
    x11: bool,
    keep: &Keep,
) -> Result<ExitStatus, HostError> {
    let Some((program, args)) = command.split_first() else {
        return Err(HostError::Spawn {
            program: String::new(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "no command"),
        });
    };
    let busctl = Path::new(scope::BUSCTL);
    if busctl.is_file()
        && !crate::app_cgroup::current_is_app_cgroup()
        && let Err(error) = scope::join_with(
            busctl,
            app_id,
            name,
            scope::Oom::OneProcess,
            scope::JOIN_TIMEOUT,
        )
    {
        eprintln!("tilo-launcher: {app_id} is outside a session scope: {error}");
    }
    let mut cmd = Command::new(program);
    cmd.args(args).envs(std::env::vars_os());
    spawn_kept(&cmd, x11, keep)
}

/// The arguments a keeper is handed for `command`: its directory and launch name, `--x11` when it
/// serves the app an X11 display, then the command behind `--`. `windows-program --print-keeper`
/// prints exactly these, so a test can hold the session's command line against the real launcher
/// without starting anything.
#[must_use]
pub fn keeper_args(
    keep: &Keep,
    x11: bool,
    command: &[std::ffi::OsString],
) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "--dir".into(),
        keep.dir.clone().into_os_string(),
        "--launch".into(),
        keep.launch.clone().into(),
    ];
    if x11 {
        args.push("--x11".into());
    }
    args.push("--".into());
    args.extend(command.iter().cloned());
    args
}

/// Start `cmd` behind the keeper `keep` names, with exactly the environment `cmd` sets, and answer
/// how the app ended as the keeper recorded it (the keeper's own status when it recorded nothing:
/// the app never started).
fn spawn_kept(cmd: &Command, x11: bool, keep: &Keep) -> Result<ExitStatus, HostError> {
    use std::os::fd::AsFd as _;
    use std::os::unix::process::ExitStatusExt as _;
    let failed = |source| HostError::Spawn {
        program: keep.keeper.display().to_string(),
        source,
    };
    let program = cmd.get_program().to_owned();
    let command: Vec<std::ffi::OsString> = std::iter::once(program)
        .chain(cmd.get_args().map(ToOwned::to_owned))
        .collect();
    let mut keeper = Command::new(&keep.keeper);
    keeper
        .args(keeper_args(keep, x11, &command))
        .env_clear()
        .envs(
            cmd.get_envs()
                .filter_map(|(key, value)| Some((key, value?))),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_host_launch_session(&mut keeper);
    // The keeper and its app end with this launcher: what ends the launch (a logout's signal on a
    // host that gave it no scope) ends what it started.
    // SAFETY: prctl is async-signal-safe and the closure captures nothing.
    unsafe {
        keeper.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = keeper.spawn().map_err(failed)?;
    let own = |stream: std::os::fd::BorrowedFd<'_>| stream.try_clone_to_owned().ok();
    let relays = [
        child
            .stdout
            .take()
            .zip(own(std::io::stdout().as_fd()))
            .map(|(out, to)| relay(out, to)),
        child
            .stderr
            .take()
            .zip(own(std::io::stderr().as_fd()))
            .map(|(err, to)| relay(err, to)),
    ];
    let status = child.wait().map_err(failed)?;
    for relay in relays.into_iter().flatten() {
        let _ = relay.join();
    }
    let ended = std::fs::read(keep.dir.join("ended"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let raw = ended.as_ref().and_then(|end| {
        if let Some(code) = end
            .pointer("/exited/code")
            .and_then(serde_json::Value::as_i64)
        {
            return i32::try_from(code).ok().map(|code| (code & 0xff) << 8);
        }
        end.pointer("/signalled/signal")
            .and_then(serde_json::Value::as_i64)
            .and_then(|signal| i32::try_from(signal).ok())
    });
    Ok(raw.map_or(status, ExitStatus::from_raw))
}

/// Copy what `from` says to `to` (this process's own stream) until it ends.
///
/// `from` is always read on, so the keeper and the app writing into it never wait and never get
/// a broken pipe. `to` is written only while it has room: the session reading it may have
/// stopped (it is going away for a restart) or be gone, and what does not fit then is dropped.
fn relay<R>(mut from: R, to: std::os::fd::OwnedFd) -> std::thread::JoinHandle<()>
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        // At most `PIPE_BUF`: a pipe with room takes a write this size whole, without waiting.
        let mut buffer = [0u8; 4096];
        let mut open = true;
        loop {
            match from.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => {
                    if open {
                        open = write_if_room(&to, buffer.get(..read).unwrap_or_default());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    })
}

/// Write `bytes` to `to` if it has room now, else drop them: whether `to` still has a reader.
fn write_if_room(to: &std::os::fd::OwnedFd, bytes: &[u8]) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut ready = libc::pollfd {
        fd: to.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: poll reads and writes the one pollfd it is given, and waits not at all.
    if unsafe { libc::poll(&raw mut ready, 1, 0) } <= 0 {
        return true;
    }
    if ready.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
        return false;
    }
    // SAFETY: write reads at most `bytes.len()` bytes from `bytes`; the launcher ignores
    // SIGPIPE (Rust's runtime does), so a reader gone since the poll is an error.
    let written = unsafe { libc::write(to.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    written >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::BrokenPipe
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
    use crate::run::{LaunchError, LaunchMode, LaunchOptions};
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A perfectly valid package manifest — the refusal being tested has to come from the launch
    /// *mode*, not from anything wrong with the package.
    fn example_manifest() -> crate::manifest::ManifestView {
        crate::manifest::test_manifest(
            "id = \"so.tilo.example\"\nversion = \"1.0.0\"\nkind = \"app\"",
            "[entrypoint]\nbin = \"bin/example\"\n",
        )
    }

    tilo_test_env::scenario! {
        fn a_host_launch_refuses_early_tolerates_its_scope_and_leads_its_session;

        phase host_launch_is_refused_before_anything_is_built;
        {
            {
                let manifest = example_manifest();
                let opts = LaunchOptions {
                    mode: LaunchMode::Host {
                        desktop_id: "com.spotify.Client".into(),
                    },
                    ..Default::default()
                };
                let err = crate::run::launch_with(&manifest, std::path::Path::new("/nonexistent"), &opts)
                    .expect_err("a host application is not a package");
                match err {
                    LaunchError::HostAppNotAPackage { desktop_id } => {
                        assert_eq!(desktop_id, "com.spotify.Client");
                    }
                    other => panic!("expected HostAppNotAPackage, got {other:?}"),
                }
            }
            {
                let manifest = example_manifest();
                let opts = LaunchOptions {
                    mode: LaunchMode::Host {
                        desktop_id: "org.gnome.TextEditor".into(),
                    },
                    ..Default::default()
                };
                let err = crate::run::launch_with_captured(
                    &manifest,
                    std::path::Path::new("/nonexistent"),
                    &opts,
                )
                .expect_err("the second way into the sandbox builder is closed too");
                assert!(matches!(err, LaunchError::HostAppNotAPackage { .. }));
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

        phase a_kept_launch_runs_the_app_behind_its_keeper_and_ends_as_the_app_did;
        {
            use std::os::unix::fs::PermissionsExt as _;

            let dir = tempfile::tempdir().expect("scratch directory");
            let entry = dir.path().join("org.example.Kept.desktop");
            std::fs::write(
                &entry,
                "[Desktop Entry]\nType=Application\nName=Kept\nExec=sh -c \"exit 7\"\n",
            )
            .expect("write the entry");
            let launch = plan_host_file(&entry, &LaunchEnv::default(), &[]).expect("the entry plans");
            // A keeper that records what it was handed, runs the app, and records how it ended as
            // the real one does.
            let keeper = dir.path().join("keeper");
            std::fs::write(
                &keeper,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.argv\"\nwhile [ \"$1\" != -- ]; do shift; done\nshift\n\"$@\"\n\
                 printf '{\"exited\":{\"code\":%s}}' $? > \"$KEPT/ended\"\nexit 0\n",
            )
            .expect("write the keeper");
            std::fs::set_permissions(&keeper, std::fs::Permissions::from_mode(0o755))
                .expect("runnable");
            let kept = dir.path().join("kept");
            std::fs::create_dir(&kept).expect("the keeper's directory");
            let mut launch = launch;
            launch.env.insert("KEPT".into(), kept.display().to_string());
            let keep = Keep {
                keeper,
                dir: kept.clone(),
                launch: "{\"app\":\"org.example.Kept\"}".into(),
            };
            let status = spawn_with(&launch, None, Duration::from_millis(20), Some(&keep))
                .expect("the launch runs");
            assert_eq!(status.code(), Some(7), "the app's end, not the keeper's");
            let argv = std::fs::read_to_string(dir.path().join("keeper.argv")).expect("handed");
            let argv: Vec<&str> = argv.lines().collect();
            assert_eq!(
                argv.get(..5),
                Some(&["--dir", kept.to_str().unwrap_or_default(), "--launch", "{\"app\":\"org.example.Kept\"}", "--"][..]),
                "the keeper is told its directory and the launch, then the app's command"
            );
            // An app the session serves X11 gets the keeper's own display.
            launch.env.insert("DISPLAY".into(), ":5".into());
            let status = spawn_with(&launch, None, Duration::from_millis(20), Some(&keep))
                .expect("the launch runs");
            assert_eq!(status.code(), Some(7));
            let argv = std::fs::read_to_string(dir.path().join("keeper.argv")).expect("handed");
            assert_eq!(
                argv.lines().nth(4),
                Some("--x11"),
                "an app with a display is served one by its keeper: {argv}"
            );
        }

        phase a_kept_launchs_output_never_holds_it_when_the_session_stops_reading_or_goes;
        {
            use std::io::Write as _;
            // Gone: the session's end closed. Stalled: nobody reads, the pipe fills.
            let (gone_reader, gone) = std::io::pipe().expect("a pipe");
            drop(gone_reader);
            let (_stalled_reader, stalled) = std::io::pipe().expect("a pipe");
            for to in [gone, stalled] {
                let (from, mut app) = std::io::pipe().expect("a pipe");
                let relayed = relay(from, std::os::fd::OwnedFd::from(to));
                let chunk = [b'x'; 64 * 1024];
                for _ in 0..64 {
                    app.write_all(&chunk).expect("the app writes on");
                }
                drop(app);
                relayed.join().expect("the relay reads to the end");
            }
        }

        phase a_refused_or_unjoined_scope_never_costs_the_launch;
        {
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
                let status = spawn_with(&launch, Some(&busctl), Duration::from_millis(20), None)
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
                        "Probe",
                        scope::Oom::OneProcess,
                    )
                );
            }
            {
                let busctl = stand_in("refusing-busctl", 1);
                let status = spawn_with(&launch, Some(&busctl), Duration::from_millis(20), None)
                    .expect("the launch runs");
                assert_eq!(status.code(), Some(7));
                assert!(
                    scope::join_with(
                        &busctl,
                        &launch.desktop_id,
                        &launch.display_name,
                        scope::Oom::OneProcess,
                        Duration::from_millis(20)
                    )
                    .expect_err("a refusal is reported")
                    .contains("Access denied"),
                    "the manager's own reason is kept for the log"
                );
            }
            {
                let status =
                    spawn_with(&launch, None, Duration::from_millis(20), None).expect("the launch runs");
                assert_eq!(status.code(), Some(7));
            }
        }

        phase host_child_becomes_its_own_session_leader;
        {
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
}
