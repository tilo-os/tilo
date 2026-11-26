//! This computer's daemon (`tilo-runtime`) across real process
//! boundaries: no global test environment mutation and no GUI. The host is
//! Cargo's build of the binary, which Cargo builds, current, for every
//! integration test run.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tilo_test_env::SpawnUnderEnvLock as _;

/// The runtime host every test launches.
fn runtime_binary() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-runtime")
}

/// What a test's program kept in `log`, and what the test was waiting for, for a wait that ran
/// out: a test's bound is the suite's hang bound, and its panic still says what the program did.
struct ItsLog {
    log: PathBuf,
    what: &'static str,
}

impl std::fmt::Display for ItsLog {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            out,
            "{} (what it kept:\n{})",
            self.what,
            std::fs::read_to_string(&self.log).unwrap_or_default()
        )
    }
}

fn host(profile: &Path) -> Command {
    let mut command = Command::new(runtime_bin(profile));
    command
        .env("TILO_RUNTIME_PROFILE", profile)
        // The device's key and certificates live in the profile, as the service names them to
        // this computer: the daemon starts as the person `set_up` certified only when the
        // certificate it reads is the one made here.
        .env("TILO_IDENTITY_ROOT", profile.join("identity"))
        // The device's package cache, where a test installs the SDK app of its own, and the
        // display a launch is handed (a file: nothing draws).
        .env("TILO_APPS_DATA_DIR", profile.join("apps"))
        .env("WAYLAND_DISPLAY", profile.join("wayland-0"))
        // The person's shell the daemon starts when a desktop asks it to: a plain shell, which
        // runs what the test asks of it, as no session is built beside the daemon here.
        .env(tilo_runtime::daemon::TEST_SHELL_ENV, "/bin/sh")
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD");
    command
}

/// The runtime a test starts: the profile's own copy when it made one, beside the launcher the
/// launch path looks for ([`DaemonProfile::start_with_launcher`]).
fn runtime_bin(profile: &Path) -> PathBuf {
    let copy = profile.join("host/tilo-runtime");
    if copy.is_file() {
        copy
    } else {
        runtime_binary()
    }
}

/// The directory the daemon serving `profile` keeps its sockets in: a profile named apart keeps
/// them beside this account's own, named by its path, so no profile's path ever becomes a
/// socket's path. Every client derives it the same way (`--connect` included).
fn sockets_dir(profile: &Path) -> Result<PathBuf> {
    tilo_api::daemon::profile_sockets(profile).map_err(anyhow::Error::msg)
}

/// The native endpoint of the daemon serving `profile`, in its [`sockets_dir`].
fn native_socket(profile: &Path) -> Result<PathBuf> {
    Ok(sockets_dir(profile)?.join(tilo_machine::DAEMON_NATIVE))
}

/// The launcher a test's profile starts apps through: the build the run made for the runtime's
/// tests, else one built beside the runtime binary, else the system's.
fn launcher_binary() -> Option<PathBuf> {
    if let Some(named) = std::env::var_os("TILO_TEST_LAUNCHER") {
        return Some(PathBuf::from(named));
    }
    let beside = runtime_binary()
        .parent()
        .map(|dir| dir.join("tilo-launcher"))
        .filter(|path| path.is_file());
    tilo_test_env::started_variant("tilo-launcher", "fast", "dev-overrides")
        .filter(|path| path.is_file())
        .or(beside)
        .or_else(|| {
            [
                "/usr/libexec/tilo/tilo-launcher",
                "/usr/libexec/tilo-launcher",
            ]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
        })
}
/// A profile named apart, and the daemon serving it: started by the test that owns the profile,
/// as only the service starts this computer's own. A launcher never starts one. Its person's
/// workspace is the first call of theirs in it to found, not the start.
struct DaemonProfile {
    root: tempfile::TempDir,
    daemon: Option<Child>,
}
impl DaemonProfile {
    fn new() -> Result<Self> {
        let mut profile = Self::unstarted()?;
        profile.start()?;
        profile.found()?;
        Ok(profile)
    }
    /// Start the profile's daemon, wait until it answers its native endpoint and status, and set
    /// this computer up the way a person does: the founding call `tilo login --new-identity`
    /// makes, asking the daemon, since it is the one process that signs as the device.
    ///
    /// Its person's workspace is not founded yet: nothing founds one at the daemon's start,
    /// since their desktop may still hand over the one it kept.
    fn start(&mut self) -> Result<()> {
        let log = std::fs::File::create(self.path().join("daemon.log"))?;
        self.daemon = Some(
            host(self.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn_locked()?,
        );
        let path = self.path().to_owned();
        let answered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.wait_until_answering(&path);
        }));
        if let Err(panic) = answered {
            self.end();
            std::panic::resume_unwind(panic);
        }
        self.found_person()
    }
    /// Wait for the daemon this profile started to answer `path`'s status, failing the test with
    /// its log at once when it ended first.
    fn wait_until_answering(&mut self, path: &Path) {
        let mut unanswered = String::new();
        // The condition is the daemon's own answer on its endpoint; a machine busy with the rest
        // of the suite must not turn a start that is progressing into a failure, so the wait is
        // bounded only by the suite's hang bound. A daemon that ended is a failure at once, with
        // its log, and is not left serving behind the test.
        tilo_test_env::wait_for(
            ItsLog {
                log: path.join("daemon.log"),
                what: "the profile's daemon to answer its status",
            },
            || {
                match status(path) {
                    Ok(status)
                        if status
                            .get("pid")
                            .and_then(Value::as_u64)
                            .is_some_and(|pid| pid != 0) =>
                    {
                        return Some(());
                    }
                    Ok(status) => unanswered = format!("its status answers no pid: {status}"),
                    Err(error) => unanswered = format!("its status: {error:#}"),
                }
                let ended = self.daemon.as_mut().map(Child::try_wait);
                match ended {
                    Some(Ok(Some(status))) => panic!(
                        "the profile's daemon ended ({status}) before it answered: {unanswered}\n{}",
                        self.log()
                    ),
                    Some(Err(error)) => panic!("whether the daemon ended: {error:#}"),
                    Some(Ok(None)) | None => {}
                }
                None
            },
        );
    }
    /// Set this computer up for the account that runs it, as `tilo login --new-identity` does: a
    /// device no person's certificate names yet answers its own actions, and its founding call
    /// makes this account's person and certifies this device's key. The daemon takes the key and
    /// signs as the device, so no process beside it makes the identity. A daemon that serves a
    /// person already says so, which is no failure here.
    fn found_person(&self) -> Result<()> {
        let place = tilo_api::daemon::DaemonPlace::at(&sockets_dir(self.path())?, self.path());
        place
            .found(&tilo_api::daemon::Founding::default())
            // None: the daemon serves this account's person already, which is set up as it is.
            .map(|_| ())
            .map_err(|why| anyhow::anyhow!("setting this computer up for its person: {why}"))
    }
    /// End the daemon this profile started: a start that never answered is not asked first.
    fn end(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }
    /// Start the profile's daemon with a launcher beside its runtime binary: apps this profile
    /// installs are then started the way a window's launch starts them.
    fn start_with_launcher(&mut self) -> Result<()> {
        let launcher = launcher_binary().context(
            "no tilo-launcher to start an app through: build it (cargo build -p launcher --bin \
             tilo-launcher) or run this test through `t test`, which builds one for it",
        )?;
        let host = self.path().join("host");
        std::fs::create_dir_all(&host)?;
        std::fs::copy(runtime_binary(), host.join("tilo-runtime"))?;
        std::fs::copy(launcher, host.join("tilo-launcher"))?;
        std::fs::write(self.path().join("wayland-0"), b"")?;
        self.start()
    }

    /// What a call in the workspace is told while the daemon is not serving it yet: its person's
    /// services start after it answers its device's actions.
    const NOT_SERVING_YET: &'static str = "run `tilo status` to see how it stands";

    /// Found the person's workspace here, as the first call of theirs in it does: this device's
    /// own client of the daemon asking in the workspace their key names. A key's run and a
    /// desktop's first call found it the same way.
    ///
    /// The daemon answers its device's actions from boot and takes connections as its person
    /// once their services start. A machine busy with the rest of the suite can outlast the bound
    /// the daemon gives its own wait, so a call refused for it is asked again, bounded only by the
    /// suite's hang bound.
    fn found(&self) -> Result<()> {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let Some(refused) = self.first_call()? else {
                return Ok(());
            };
            if !refused.contains(Self::NOT_SERVING_YET) {
                bail!("the person's first call failed: {refused}\n{}", self.log());
            }
            if Instant::now() >= deadline {
                bail!(
                    "the daemon never served its person's first call: {refused}\n{}",
                    self.log()
                );
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// The person's first call in their workspace: one client of the daemon, ending once the
    /// workspace is founded, or the words it failed with when it did not.
    fn first_call(&self) -> Result<Option<String>> {
        let mut client = host(self.path())
            .arg("--connect")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_locked()?;
        writeln!(
            client
                .stdin
                .take()
                .context("the client has no standard input")?,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"ping"})
        )?;
        let mut said = client.stdout.take().context("the client's answer")?;
        let mut why = client.stderr.take().context("the client's reason")?;
        // The condition is the client ending, which the daemon admits only once the workspace is
        // founded. That can take as long as its own wait for the person's services, so the wait
        // is bounded only by the suite's hang bound: a busy machine must not turn a founding in
        // progress into a failure. One still there at the bound is ended, so nothing keeps
        // serving behind the test.
        let pid = client.id();
        let path = self.path().to_owned();
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tilo_test_env::exited(
                ItsLog {
                    log: path.join("daemon.log"),
                    what: "the person's first call to end and found their workspace",
                },
                pid,
            );
            client.wait()
        }));
        let ended = match ended {
            Ok(ended) => ended?,
            Err(panic) => {
                let _ = client.kill();
                let _ = client.wait();
                std::panic::resume_unwind(panic);
            }
        };
        let mut answer = String::new();
        std::io::Read::read_to_string(&mut said, &mut answer)?;
        let mut reason = String::new();
        std::io::Read::read_to_string(&mut why, &mut reason)?;
        if !ended.success() {
            return Ok(Some(reason));
        }
        let reply: Value = serde_json::from_str(&answer)
            .with_context(|| format!("the first call's answer: {answer:?}\n{}", self.log()))?;
        ensure!(
            reply.get("id") == Some(&json!(1)),
            "the first call's answer: {reply}"
        );
        ensure!(
            status(self.path())?
                .get("workspace")
                .and_then(Value::as_str)
                .is_some_and(|workspace| !workspace.is_empty()),
            "the person's first call founded no workspace\n{}",
            self.log()
        );
        Ok(None)
    }
    /// The profile's daemon log, as it stands.
    fn log(&self) -> String {
        std::fs::read_to_string(self.path().join("daemon.log")).unwrap_or_default()
    }
    fn unstarted() -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .prefix("tilo-d-")
            .tempdir_in(std::fs::canonicalize("/tmp").context("resolving /tmp")?)
            .context("creating the daemon profile")?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .context("making the daemon profile private")?;
        Ok(Self { root, daemon: None })
    }
    fn path(&self) -> &Path {
        self.root.path()
    }
}
impl Drop for DaemonProfile {
    fn drop(&mut self) {
        // The daemon's log goes with the profile; keep it for a failed test.
        if std::thread::panicking()
            && let Ok(log) = std::fs::read_to_string(self.path().join("daemon.log"))
        {
            eprintln!("daemon.log:\n{log}");
        }
        // The daemon this profile started is told to stop by the pid this test kept of it:
        // asking its socket for that pid waits a second on a machine busy with the rest of the
        // suite, and a daemon left untold waits the whole hang bound before it is killed.
        let own = self.daemon.as_mut().and_then(|daemon| {
            let ended = daemon.try_wait();
            matches!(ended, Ok(None))
                .then(|| i32::try_from(daemon.id()).ok())
                .flatten()
        });
        if let Some(pid) = own {
            // SAFETY: `kill(2)` with SIGTERM on the daemon this test started, the signal a stop
            // sends.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        } else if let Ok(socket) = native_socket(self.path())
            && let Ok(stream) = UnixStream::connect(socket)
        {
            let mut stream = stream;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = stream.write_all(b"{\"native\":\"status\"}\n");
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok()
                && let Ok(value) = serde_json::from_str::<Value>(&line)
                && let Some(pid) = value.get("pid").and_then(Value::as_i64)
                && let Some(pid) = i32::try_from(pid).ok()
            {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
        if let Some(mut daemon) = self.daemon.take() {
            // It was asked to end above; one that does not, or never started, is named and
            // killed rather than waited on: a failed test must not become the run's timeout.
            // The bound is the suite's hang bound, so a slow machine's shutdown is not a hang.
            let pid = daemon.id();
            let deadline = tilo_test_env::hang_deadline();
            while matches!(daemon.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(25));
            }
            if matches!(daemon.try_wait(), Ok(None)) {
                eprintln!(
                    "the daemon (pid {pid}) serving {} did not end within {:?}: killing it\n{}",
                    self.path().display(),
                    tilo_test_env::hang_bound(),
                    self.log()
                );
            }
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
        // The sockets live beside this account's own, not in the profile: they go with it.
        if let Ok(sockets) = sockets_dir(self.path()) {
            let _ = std::fs::remove_dir_all(sockets);
        }
    }
}
/// The runtime's answer to `request` on the profile's socket.
fn ask(profile: &Path, request: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(native_socket(profile)?)
        .context("connecting to the runtime's profile socket")?;
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .context("bounding the reply")?;
    stream
        .write_all(format!("{request}\n").as_bytes())
        .context("asking the runtime")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).with_context(|| format!("the answer to {request}: {line:?}"))
}
/// The runtime's answer to a `status` request on the profile's socket.
fn status(profile: &Path) -> Result<Value> {
    ask(profile, &json!({"native": "status"}))
}
/// An MCP `ping` answered on the profile's socket, through the session the daemon admits the
/// person to: every MCP request of a connection is answered on the one app runtime.
fn ping(profile: &Path) -> Result<()> {
    let reply = ask(profile, &json!({"jsonrpc":"2.0","id":9,"method":"ping"}))?;
    ensure!(
        reply.get("id") == Some(&json!(9)),
        "the ping's answer: {reply}"
    );
    ensure!(
        reply.get("result") == Some(&json!({})),
        "the ping's answer: {reply}"
    );
    Ok(())
}
/// The daemon serving `profile`'s socket, as its status names it.
fn daemon(profile: &Path) -> Result<Value> {
    let status = status(profile)?;
    status.get("pid").cloned().context("a status without a pid")
}
/// A person's program asking this computer to start `app`: what `--run-app` sends, as the
/// launcher run a person makes. The app this computer resolves decides what runs.
fn run_app(profile: &Path, app: &str) -> Result<std::process::Output> {
    Ok(host(profile).args(["--run-app", app]).output_locked()?)
}
/// The daemon's answer to a launch request for `app` in the workspace it serves.
fn launch(profile: &Path, app: &str) -> Result<Value> {
    let workspace = status(profile)?
        .get("workspace")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    ask(
        profile,
        &json!({"native": "launch", "app_id": app, "workspace": workspace, "presentation": false}),
    )
}
#[test]
fn launches_reuse_the_one_daemon_which_recovers_a_stale_socket() -> Result<()> {
    let mut profile = DaemonProfile::unstarted()?;
    let stale = std::os::unix::net::UnixListener::bind(native_socket(profile.path())?)?;
    drop(stale);
    profile.start()?;
    profile.found()?;
    let before = daemon(profile.path())?;
    // One daemon answers each program that names an app, and names the app it cannot start:
    // starting an app never starts a second daemon.
    for app in ["dev.tilo.first", "dev.tilo.second"] {
        let refused = run_app(profile.path(), app)?;
        ensure!(
            !refused.status.success(),
            "an app this computer has not installed started"
        );
        ensure!(
            String::from_utf8_lossy(&refused.stderr).contains("is not installed on this computer"),
            "a launch must say the app is not installed: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    ensure!(
        daemon(profile.path())? == before,
        "a launch started another daemon"
    );
    let published = status(profile.path())?;
    ensure!(
        published
            .get("workspace")
            .and_then(Value::as_str)
            .is_some_and(|workspace| !workspace.is_empty()),
        "the status names no workspace: {published}"
    );
    Ok(())
}
#[test]
fn simultaneous_launches_share_one_daemon_and_mcp_eof_keeps_it_alive() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let mut children = (0..5)
        .map(|_| {
            host(profile.path())
                .args(["--run-app", "dev.tilo.race"])
                .spawn_locked()
        })
        .collect::<std::io::Result<Vec<Child>>>()?;
    for child in &mut children {
        // Each racing launch is answered by the one daemon: the app is not installed here, and
        // the daemon that says so is the one that keeps serving.
        ensure!(
            !child.wait()?.success(),
            "an app this computer has not installed started"
        );
    }
    let before = daemon(profile.path())?;
    let mut proxy = host(profile.path())
        .arg("--connect")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn_locked()?;
    writeln!(
        proxy
            .stdin
            .take()
            .context("the proxy has no standard input")?,
        "{}",
        json!({"jsonrpc":"2.0","id":7,"method":"ping"})
    )?;
    let output = proxy.wait_with_output()?;
    ensure!(
        output.status.success(),
        "the proxy failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reply: Value = serde_json::from_slice(&output.stdout).context("the ping reply")?;
    ensure!(reply.get("id") == Some(&json!(7)), "{reply}");
    ensure!(reply.get("result") == Some(&json!({})), "{reply}");
    ensure!(
        daemon(profile.path())? == before,
        "the proxy's end restarted the daemon"
    );
    Ok(())
}
#[test]
fn live_profile_lock_fails_without_unlinking_owners_socket() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let before = daemon(profile.path())?;
    let competing = host(profile.path()).output_locked()?;
    ensure!(
        !competing.status.success(),
        "a second daemon took a profile that is in use"
    );
    // It fails on the held device lock itself, naming who holds it.
    let stderr = String::from_utf8_lossy(&competing.stderr);
    ensure!(
        stderr.contains("is in use"),
        "a competing daemon must give up on the held device lock: {stderr}"
    );
    ensure!(
        daemon(profile.path())? == before,
        "the competing daemon replaced the owner"
    );
    Ok(())
}
#[test]
fn unsafe_profile_and_relative_override_are_rejected() -> Result<()> {
    let root = tempfile::tempdir()?;
    std::os::unix::fs::symlink(root.path(), root.path().join("link"))?;
    ensure!(
        !run_app(&root.path().join("link"), "dev.tilo.test")?
            .status
            .success(),
        "a symlinked profile was accepted"
    );
    ensure!(
        !run_app(Path::new("relative-profile"), "dev.tilo.test")?
            .status
            .success(),
        "a relative profile was accepted"
    );
    Ok(())
}

#[test]
fn public_profile_and_symlinked_device_lock_are_refused() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // A launcher and a daemon both refuse a profile anyone can write to, and change nothing.
    let profile = DaemonProfile::unstarted()?;
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o777))?;
    let output = run_app(profile.path(), "dev.tilo.test")?;
    ensure!(
        !output.status.success(),
        "a launcher accepted a public profile"
    );
    ensure!(
        !host(profile.path()).output_locked()?.status.success(),
        "a daemon accepted a public profile"
    );
    ensure!(
        std::fs::metadata(profile.path())?.permissions().mode() & 0o777 == 0o777,
        "the rejected start changed the profile"
    );
    // Only the daemon takes the device's lock, so it is the one that must not follow a
    // symlink planted where the lock goes.
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))?;
    let target = profile.path().join("other-file");
    std::fs::write(&target, "untouched")?;
    let device = profile.path().join("state/tilo/device");
    std::fs::create_dir_all(&device)?;
    std::os::unix::fs::symlink(&target, device.join("holder.lock"))?;
    ensure!(
        !host(profile.path()).output_locked()?.status.success(),
        "a daemon took a symlinked device lock"
    );
    ensure!(
        std::fs::read_to_string(target)? == "untouched",
        "the rejected daemon wrote through the symlinked lock"
    );
    Ok(())
}

/// A desktop started beside the daemon of a profile named apart, in state directories of its own
/// (a development device's session, as the server plan cycle's owner device starts one), finds
/// that daemon running and asks it to start the person's shell. The daemon starts the shell with
/// a one-use secret, which the shell redeems for its link to the person's workspaces. A desktop
/// that found no daemon there would run as no shell of the daemon's and reach no workspace.
#[test]
fn a_desktop_beside_a_profiles_daemon_is_started_by_it_and_reaches_its_workspaces() -> Result<()> {
    let profile = DaemonProfile::new()?;
    // Where the desktop finds the daemon: from the profile it names alone, since none of the
    // desktop's own directories is the profile's.
    let place = tilo_api::daemon::serving_profile(profile.path()).map_err(anyhow::Error::msg)?;
    ensure!(
        !place.system() && place.alive(),
        "the desktop finds no daemon running by its lock at {}\n{}",
        place.lock().display(),
        profile.log()
    );
    let handed = profile.path().join("handed-secret");
    let started = ask_native(
        &place.native(),
        &json!({
            "native": "shells#start",
            "args": ["-c", "printf %s \"$TILO_SHELL_TOKEN\" > \"$TILO_TEST_HANDED\""],
            "env": {"TILO_TEST_HANDED": handed},
        }),
    )?;
    ensure!(
        started.get("pid").and_then(Value::as_u64).is_some(),
        "the daemon started no shell: {started}\n{}",
        profile.log()
    );
    let token = tilo_test_env::wait_for(
        ItsLog {
            log: profile.path().join("daemon.log"),
            what: "the shell the daemon started to read the secret it was handed",
        },
        || {
            std::fs::read_to_string(&handed)
                .ok()
                .filter(|token| token.len() == 64)
        },
    );
    let link = UnixStream::connect(place.native())?;
    link.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
    writeln!(
        &link,
        "{}",
        json!({"native": "shells#link", "token": token})
    )?;
    let (answer, links) = runtime_portal::fd_transport::recv_with_fds(&link)?;
    let answer: Value = serde_json::from_slice(&answer)?;
    ensure!(
        answer.get("error").is_none() && links.len() == 1,
        "the shell's secret was redeemed for no link to the person's workspaces: {answer}"
    );
    Ok(())
}

/// A program that opens the collaboration service for itself, naming an app this computer never
/// launched, is refused as no app — and told how to run so this computer launches it.
/// A launch that names a program is refused before anything starts: what runs is the app this
/// computer has installed, never a program its caller names.
#[test]
fn a_launch_that_names_a_program_is_refused() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let workspace = status(profile.path())?
        .get("workspace")
        .and_then(Value::as_str)
        .context("the daemon names no workspace")?
        .to_owned();
    let answer = ask(
        profile.path(),
        &json!({
            "native": "launch",
            "app_id": "so.tilo.passwords",
            "workspace": workspace,
            "presentation": false,
            "executable": "/bin/sh",
        }),
    )?;
    ensure!(
        answer
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("unknown field")),
        "a launch that names a program must be refused: {answer}"
    );
    // Nothing of it ran: the daemon is the one that served the request.
    ensure!(
        status(profile.path())?
            .get("pid")
            .and_then(Value::as_u64)
            .is_some_and(|pid| pid != 0),
        "the daemon stopped"
    );
    Ok(())
}

/// A launch of an app this computer has not installed is refused, saying how to install it, and
/// the person's MCP connection keeps answering beside the refusal.
#[test]
fn a_launch_of_an_uninstalled_app_is_refused_and_mcp_still_answers() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let answer = launch(profile.path(), "so.tilo.passwords")?;
    let refused = answer
        .get("error")
        .and_then(Value::as_str)
        .context("an uninstalled app was started")?;
    ensure!(
        refused.contains("so.tilo.passwords is not installed on this computer")
            && refused.contains("tilo app install so.tilo.passwords"),
        "the refusal must say how to install the app: {refused}"
    );
    ping(profile.path())?;
    let running = apps_once(profile.path())?;
    ensure!(
        running.is_empty(),
        "nothing runs, so no app is listed: {running:?}"
    );
    Ok(())
}

#[test]
fn a_program_that_attaches_without_a_launch_is_refused_and_told_how_to_run() -> Result<()> {
    const ATTACHED: &str = "TILO_SELF_ATTACHED_PROGRAM";
    const WORKSPACE: &str = "TILO_SELF_ATTACHED_WORKSPACE";
    const APP: &str = "dev.tilo.self-attached";
    if let Ok(file) = std::env::var(ATTACHED) {
        let profile = PathBuf::from(
            std::env::var_os("TILO_RUNTIME_PROFILE").context("the profile is named")?,
        );
        let workspace = std::env::var(WORKSPACE).context("the workspace is named")?;
        // This program makes the collaboration connection itself, as a program started by hand
        // does, and names an app this computer never launched.
        let mut stream =
            UnixStream::connect(sockets_dir(&profile)?.join(tilo_runtime::COLLAB_SOCKET))
                .context("connecting to the collaboration socket")?;
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .context("bounding the refusal")?;
        runtime_portal::launch_handoff::write(&mut stream, APP, &workspace)
            .context("naming the app this program claims to be")?;
        // The first call any app makes: where the workspace its record names stands. A
        // connection no launch attests is answered why, on every call.
        let call = runtime_collab::wire::Call {
            id: 1,
            request: runtime_collab::wire::Operation::Workspace {
                record: String::new(),
                seen: None,
            },
        };
        runtime_collab::wire::FRAMING
            .write_value(&mut stream, &call)
            .context("asking the collaboration service")?;
        let answer: runtime_collab::wire::HostFrame = runtime_collab::wire::FRAMING
            .read_value(&mut stream)
            .context("reading the collaboration service's answer")?
            .context("the collaboration service answered nothing")?;
        std::fs::write(&file, format!("{answer:?}"))?;
        return Ok(());
    }
    let profile = DaemonProfile::new()?;
    let workspace = status(profile.path())?
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|workspace| !workspace.is_empty())
        .context("the daemon names no workspace")?
        .to_owned();
    let file = profile.path().join("attached.refusal");
    let log_path = profile.path().join("attached.log");
    let log = std::fs::File::create(&log_path)?;
    // As libtest names it: a module of `tests/it.rs`, without the test binary's crate root.
    let module = module_path!();
    let name = format!(
        "{}::a_program_that_attaches_without_a_launch_is_refused_and_told_how_to_run",
        module.split_once("::").map_or(module, |(_, within)| within)
    );
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            name.as_str(),
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(ATTACHED, &file)
        .env(WORKSPACE, &workspace)
        .env("TILO_RUNTIME_PROFILE", profile.path())
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let mut program = command.spawn_locked()?;
    // The condition is the refusal the program writes: the file is its answer, so a busy machine
    // delays it without failing the test. One that ended without writing is a failure at once,
    // and the wait is bounded only by the suite's hang bound.
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        attached_refusal(&mut program, &file, &log_path)
    }));
    let refused = match refused {
        Ok(refused) => refused,
        Err(panic) => {
            let _ = program.kill();
            let _ = program.wait();
            std::panic::resume_unwind(panic);
        }
    };
    ensure!(
        refused.contains("is not one this computer made"),
        "the refusal must say this computer made no launch: {refused}"
    );
    ensure!(
        refused.contains("`tilo-runtime --run-app"),
        "the refusal must say how to run the program so this computer launches it: {refused}"
    );
    Ok(())
}

/// The refusal the attaching program wrote, waited for: the file is its answer, so a busy
/// machine delays it without failing the test, and one that ended without writing fails it at
/// once, with its log.
fn attached_refusal(program: &mut Child, file: &Path, log: &Path) -> String {
    tilo_test_env::wait_for(
        ItsLog {
            log: log.to_owned(),
            what: "the attaching program to write its refusal",
        },
        || {
            let said = std::fs::read_to_string(file).unwrap_or_default();
            if !said.is_empty() {
                return Some(said);
            }
            match program.try_wait() {
                Ok(Some(status)) => panic!(
                    "the attaching program ended ({status}) and wrote nothing:\n{}",
                    std::fs::read_to_string(log).unwrap_or_default()
                ),
                Ok(None) => {}
                Err(error) => panic!("whether the program ended: {error:#}"),
            }
            None
        },
    )
}

/// A built development app: its manifest and payload in one directory, the SDK app with
/// `lifetime_ms` and, with `second_instance`, a package that asks the daemon for another instance
/// of itself. `app` is the app's own id; the device it is installed on gives it the dev id it runs
/// as.
fn dev_app_build(
    profile: &Path,
    app: &str,
    lifetime_ms: u64,
    second_instance: bool,
) -> Result<PathBuf> {
    let dir = profile.join(format!("dev-app-{}", app.replace('.', "-")));
    std::fs::create_dir_all(&dir)?;
    let program = dir.join("app");
    std::fs::copy(tilo_test_env::package_binary!("tilo-sdk-app"), &program)?;
    // The build is a debug one, far over the local-build ceiling a package installs under: its
    // debug sections go, and the program is the same.
    let _ = std::process::Command::new("strip")
        .args(["--strip-debug", "--strip-unneeded"])
        .arg(&program)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status_locked();
    std::fs::write(dir.join("tilo-app-lifetime-ms"), format!("{lifetime_ms}\n"))?;
    if second_instance {
        std::fs::write(dir.join("tilo-app-second-instance"), b"")?;
    }
    let isa = match std::env::consts::ARCH {
        "x86_64" => "x86_64 = \"v3\"",
        other if other.starts_with("aarch64") => "aarch64 = \"aarch64\"",
        other => bail!("no package for the architecture {other}"),
    };
    let manifest = format!(
        r#"[package]
id = "{app}"
name = "SDK app"
version = "0.1.0"
kind = "app"

[package.isa]
{isa}

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[sandbox.services]
collab = "optional@54"
# The daemon the app asks for another instance of itself.
native = "optional@54"

[entrypoint]
bin = "app"
type = "gui"
"#
    );
    std::fs::write(dir.join("tilo-app.toml"), manifest)?;
    Ok(dir)
}

/// Hand the built app in `dir` to the profile's daemon: the device marks it, packs and signs it
/// with its own key, and installs it. Answers the dev id it runs as.
fn install_dev(profile: &Path, dir: &Path, app: &str) -> Result<String> {
    let keys = tilo_device_identity::persist::IdentityPaths::under(profile.join("identity"));
    let device = tilo_device_identity::persist::load_device_key(&keys)
        .context("the profile's device key")?;
    let payload = ["app", "tilo-app-lifetime-ms", "tilo-app-second-instance"]
        .into_iter()
        .filter(|name| dir.join(name).is_file())
        .map(|name| package_actions::HostFile {
            path: name.to_owned(),
            file: package_actions::Uploaded {
                source: dir.join(name).display().to_string(),
            },
        })
        .collect();
    let report = package_actions::device::change_at(
        &native_socket(profile)?,
        &package_actions::Change::InstallDev {
            manifest: package_actions::Uploaded {
                source: dir.join("tilo-app.toml").display().to_string(),
            },
            payload,
        },
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "{error}\n{}",
            std::fs::read_to_string(profile.join("daemon.log")).unwrap_or_default()
        )
    })?;
    let id = tilo_app_package::dev_app_id(app, &device.verifying_key().to_bytes());
    // The cache says what it installed: an answer that only says why it did not is no install.
    ensure!(
        report
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("installed development build: {id}"))),
        "the device did not install {id}: {}",
        report.lines.join("\n")
    );
    Ok(id)
}

/// Ask the profile's daemon to start `app` through the same launch a window takes.
fn start_app(profile: &Path, app: &str) -> Result<()> {
    let output = run_app(profile, app)?;
    ensure!(
        output.status.success(),
        "starting {app} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// The app instances the daemon's action registry lists, once: each as its `app_id` and the
/// provider of its own connection.
fn apps_once(profile: &Path) -> Result<Vec<Value>> {
    use tilo_action_services::LineClient;
    use tilo_actions::remote::{ActionProvider as _, Caller, CatalogueQuery};
    let line = LineClient::connect(&native_socket(profile)?)?;
    let page = line
        .query(
            &Caller::local_person(""),
            CatalogueQuery {
                max_results: 0,
                ..CatalogueQuery::default()
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .context("the daemon answers what its registry lists")?
        .map_err(|error| anyhow::anyhow!("the registry's listing was refused: {error:?}"))?;
    Ok(page
        .providers
        .into_iter()
        .filter(|provider| provider.browsable && provider.provider.parse::<u64>().is_ok())
        .map(|provider| json!({"app_id": provider.app_id, "peer_id": provider.provider}))
        .collect())
}

/// The app instances the registry lists, once `count` of them run.
fn listed_apps(profile: &Path, count: usize) -> Result<Vec<Value>> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let apps = apps_once(profile)?;
        if apps.len() == count {
            return Ok(apps);
        }
        ensure!(
            Instant::now() < deadline,
            "expected {count} apps, got {apps:?}\n{}",
            std::fs::read_to_string(profile.join("daemon.log")).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The app ids of `apps`, in the order they were listed.
fn app_ids(apps: &[Value]) -> Vec<&str> {
    apps.iter()
        .filter_map(|app| app.get("app_id").and_then(Value::as_str))
        .collect()
}

/// A native SDK program installed as this device's development build runs as it, joins the
/// person's session as an app of its own, and a second one joins beside it; the first ending
/// leaves the other running.
#[test]
fn independent_sdk_apps_late_join_and_one_ends_while_the_other_runs() -> Result<()> {
    let mut profile = DaemonProfile::unstarted()?;
    profile.start_with_launcher()?;
    profile.found()?;
    let first = dev_app_build(profile.path(), "dev.tilo.independent-first", 8_000, false)?;
    let second = dev_app_build(
        profile.path(),
        "dev.tilo.independent-second",
        120_000,
        false,
    )?;
    let first_id = install_dev(profile.path(), &first, "dev.tilo.independent-first")?;
    let second_id = install_dev(profile.path(), &second, "dev.tilo.independent-second")?;
    start_app(profile.path(), &first_id)?;
    let only = listed_apps(profile.path(), 1)?;
    ensure!(
        app_ids(&only) == [first_id.as_str()],
        "the first app is not the only one listed: {only:?}"
    );
    let before = daemon(profile.path())?;
    start_app(profile.path(), &second_id)?;
    let both = listed_apps(profile.path(), 2)?;
    let [one, two] = both.as_slice() else {
        bail!("expected two apps, got {both:?}");
    };
    ensure!(
        one.get("peer_id") != two.get("peer_id"),
        "both apps name the same connection: {both:?}"
    );
    ensure!(
        app_ids(&both).iter().any(|app| *app == first_id)
            && app_ids(&both).iter().any(|app| *app == second_id),
        "both apps are listed: {both:?}"
    );
    ensure!(
        daemon(profile.path())? == before,
        "the second app restarted the daemon"
    );
    ping(profile.path())?;
    // The first app's package names a short life: it ends while the second keeps running.
    let survivor = listed_apps(profile.path(), 1)?;
    ensure!(
        app_ids(&survivor) == [second_id.as_str()],
        "the app that stayed is not the one listed: {survivor:?}"
    );
    ping(profile.path())?;
    Ok(())
}

/// An app that asks this computer's daemon for a second instance of itself gets two connections,
/// and both are its instances: the daemon starts the second as it started the first, attested
/// like any instance.
#[test]
fn ordinary_sdk_parent_child_get_distinct_connections_in_one_runtime() -> Result<()> {
    let mut profile = DaemonProfile::unstarted()?;
    profile.start_with_launcher()?;
    profile.found()?;
    let dir = dev_app_build(profile.path(), "dev.tilo.parent-child", 30_000, true)?;
    let id = install_dev(profile.path(), &dir, "dev.tilo.parent-child")?;
    start_app(profile.path(), &id)?;
    let both = listed_apps(profile.path(), 2)?;
    let [one, two] = both.as_slice() else {
        bail!("expected two instances, got {both:?}");
    };
    ensure!(
        app_ids(&both).iter().all(|app| *app == id),
        "another app was reported: {both:?}"
    );
    ensure!(
        one.get("peer_id") != two.get("peer_id"),
        "the parent and child share a connection: {both:?}"
    );
    ping(profile.path())?;
    Ok(())
}

/// A file the person puts for one of this device's own actions is kept on the device, where that
/// action reads it, and never in the workspace's store every member's device carries.
#[test]
fn a_put_for_a_device_action_is_kept_on_the_device() -> Result<()> {
    use tilo_action_services::LineClient;
    use tilo_actions::remote::{Caller, ContentGets as _, ContentPut as _};
    let profile = DaemonProfile::new()?;
    let line = LineClient::connect(&native_socket(profile.path())?)?;
    let caller = Caller::local_person("");
    let keys = tilo_device_identity::persist::IdentityPaths::under(profile.path().join("identity"));
    let device = tilo_device_identity::persist::load_device_key(&keys)
        .context("the profile's device key")?;
    // The action the put is for: this device's development build, whose arguments name
    // `tilo.content:` references its own store answers.
    let target = tilo_actions::remote::RemoteActionTarget {
        provider: package_actions::PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(
            format!(
                "devices/{}/packages",
                hex::encode(device.verifying_key().to_bytes())
            ),
            "install-dev",
        ),
    };
    let bytes = b"a development build's own file";
    let mut put = line.begin_put_for(u64::try_from(bytes.len())?, Some(target))?;
    put.append(0, bytes)?;
    let content = Box::new(put).finish()?;
    let hash = content
        .strip_prefix("tilo.content:")
        .context("a content reference")?;
    ensure!(
        profile.path().join("apps/content").join(hash).is_file(),
        "the device keeps the bytes its own action reads: {content}\n{}",
        profile.log()
    );
    ensure!(
        line.get(&caller, &content, 0, 0).is_err(),
        "the workspace's store, which every member's device carries, keeps none of it: {content}"
    );
    Ok(())
}

/// `daemon`'s exit status once it ends: the daemon ends as it says it did. The wait is the
/// process's own exit, bounded only by the suite's hang bound, so a machine busy with the rest of
/// the suite does not turn a stop being taken into a failure; one still running at the bound is
/// killed, and its log says what kept it.
fn ended(daemon: &mut Child, profile: &DaemonProfile) -> Result<std::process::ExitStatus> {
    let pid = daemon.id();
    let path = profile.path().to_owned();
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        tilo_test_env::exited(
            ItsLog {
                log: path.join("daemon.log"),
                what: "a daemon told to stop, to end",
            },
            pid,
        );
    }));
    if let Err(panic) = ended {
        let _ = daemon.kill();
        let _ = daemon.wait();
        std::panic::resume_unwind(panic);
    }
    daemon.wait().context("the daemon's exit status")
}

/// A daemon started before there is a person hears the stop a stack sends it: SIGTERM ends it
/// with its own exit status, even where the caller had the signal ignored, which a child
/// inherits across exec. Its wait for a person is outside any part a signal used to stop, so a
/// daemon started this way ran on.
#[test]
fn sigterm_ends_a_daemon_started_before_its_person() -> Result<()> {
    use std::os::unix::process::CommandExt as _;
    let profile = DaemonProfile::unstarted()?;
    let path = profile.path().to_owned();
    // No identity: the daemon serves this computer before there is a person, as a stack starts
    // it while the person it will serve does not exist yet.
    let mut command = host(&path);
    command
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(path.join("daemon.log"))?);
    // SAFETY: the closure runs between fork and exec and calls `signal(2)` alone, which is
    // async-signal-safe. A caller that had SIGTERM ignored leaves it ignored across exec, as a
    // supervisor that stopped its own waited for no signal may.
    unsafe {
        command.pre_exec(|| {
            let _ = libc::signal(libc::SIGTERM, libc::SIG_IGN);
            Ok(())
        });
    }
    let mut daemon = command.spawn_locked()?;
    let pid = i32::try_from(daemon.id()).context("the daemon's pid")?;
    let deadline = tilo_test_env::hang_deadline();
    // Its log says it answers, which it says after it built the stop listener: a stop sent once
    // it said so finds the daemon hearing it, not the process it started as. The wait is bounded
    // only by the suite's hang bound, so a busy machine's start is not a failure.
    loop {
        if profile
            .log()
            .contains("answering this device's actions on its native endpoint")
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = daemon.kill();
            let _ = daemon.wait();
            bail!("the daemon did not answer\n{}", profile.log());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // SAFETY: `kill(2)` with SIGTERM on the daemon this test started, the signal a stop sends.
    ensure!(
        unsafe { libc::kill(pid, libc::SIGTERM) } == 0,
        "SIGTERM did not reach the daemon: {}",
        std::io::Error::last_os_error()
    );
    let ended = ended(&mut daemon, &profile)?;
    ensure!(
        ended.success(),
        "the daemon ended as {ended}, not of its own stop\n{}",
        profile.log()
    );
    Ok(())
}

/// The native endpoint of the daemon serving `profile`: a profile named apart keeps its sockets
/// in this account's runtime directory, under its own path's name.
fn served_socket(profile: &Path) -> Result<PathBuf> {
    let login = tilo_device_identity::login::Place::here(
        tilo_identity::provisioning::collaboration_identity_paths(),
    )?;
    let sockets = tilo_device_identity::login::daemon::profile_sockets(login.dir(), profile)
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(sockets.join("native.sock"))
}

/// A daemon between its person's parts hears a stop: held in a step of the part that ended,
/// where its own thread cannot take the stop up, SIGTERM still ends it, with its own status,
/// and its endpoint goes with it.
#[test]
fn sigterm_ends_a_daemon_held_between_its_person_parts() -> Result<()> {
    // This computer set up by its daemon, which then ends: what starts next starts over a
    // profile whose person is already there, and holds before its person's part begins.
    let mut profile = DaemonProfile::unstarted()?;
    profile.start()?;
    profile.end();
    let path = profile.path().to_owned();
    let socket = served_socket(&path)?;
    let mut command = host(&path);
    command
        .env("RUST_LOG", "info")
        // A step of the person's part ending that does not return, as a folder that will not
        // unmount: the daemon sits between its parts, and only a stop heard outside its own
        // thread can end it.
        .env("TILO_TEST_HOLD_BETWEEN_PARTS_MS", "60000")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(path.join("daemon.log"))?);
    let mut daemon = command.spawn_locked()?;
    let pid = i32::try_from(daemon.id()).context("the daemon's pid")?;
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if profile
            .log()
            .contains("a test holds the daemon between its person's parts")
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = daemon.kill();
            let _ = daemon.wait();
            bail!(
                "the daemon did not reach the wait between parts\n{}",
                profile.log()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // SAFETY: `kill(2)` with SIGTERM on the daemon this test started, the signal a stop sends.
    ensure!(
        unsafe { libc::kill(pid, libc::SIGTERM) } == 0,
        "SIGTERM did not reach the daemon: {}",
        std::io::Error::last_os_error()
    );
    let ended = ended(&mut daemon, &profile)?;
    ensure!(
        ended.success(),
        "the daemon ended as {ended}, not of its own stop\n{}",
        profile.log()
    );
    ensure!(
        !socket.exists(),
        "the daemon's endpoint outlived it: {}",
        socket.display()
    );
    Ok(())
}

/// A computer whose daemon is the account's own, as a person's runs: no profile is named apart,
/// so its sockets are where every program of the person's looks for them
/// (`$XDG_RUNTIME_DIR/tilo/native.sock`), and a program the test starts reaches it as a build in
/// a dev session reaches the session's daemon.
struct OwnComputer {
    root: tempfile::TempDir,
    daemon: Option<Child>,
}

impl OwnComputer {
    /// The computer — its home, runtime directory, identity and package store the test's own —
    /// with its daemon started, answering, and its person founded.
    fn start() -> Result<Self> {
        let computer = Self::launch(None)?;
        computer.found_person()?;
        Ok(computer)
    }

    /// The computer with its daemon started and answering, and held between its person's parts
    /// for `hold_between_parts_ms` when one is named (a debug build's test hold): the test founds
    /// its person itself.
    fn start_held(hold_between_parts_ms: u64) -> Result<Self> {
        Self::launch(Some(hold_between_parts_ms))
    }

    /// The computer with its daemon started and answering, its person not yet founded.
    fn launch(hold_between_parts_ms: Option<u64>) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::Builder::new()
            .prefix("tilo-o-")
            .tempdir_in(std::fs::canonicalize("/tmp").context("resolving /tmp")?)
            .context("creating the computer's directories")?;
        for dir in [
            root.path().to_owned(),
            root.path().join("home"),
            root.path().join("home/.local/state/tilo/runtime"),
            root.path().join("run"),
            root.path().join("identity"),
            root.path().join("apps"),
        ] {
            std::fs::create_dir_all(&dir)?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut computer = Self { root, daemon: None };
        let (relay, loopback) = test_server::loopback_relay();
        let mut command = Command::new(runtime_binary());
        tilo_test_env::launch_env(&mut command);
        command
            .env(relay, loopback)
            .env("HOME", computer.home())
            .env("XDG_RUNTIME_DIR", computer.runtime())
            .env("XDG_CONFIG_HOME", computer.path().join("home/config"))
            .env("XDG_DATA_HOME", computer.path().join("home/data"))
            .env("XDG_CACHE_HOME", computer.path().join("home/cache"))
            .env("XDG_STATE_HOME", computer.path().join("home/state"))
            .env("TILO_IDENTITY_ROOT", computer.identity())
            .env("TILO_APPS_DATA_DIR", computer.path().join("apps"))
            .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("TILO_SVC_COLLAB_FD")
            .env_remove("TILO_SVC_IDENTITY_FD")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(computer.path().join("daemon.log"))?);
        if let Some(ms) = hold_between_parts_ms {
            command.env("TILO_TEST_HOLD_BETWEEN_PARTS_MS", ms.to_string());
        }
        computer.daemon = Some(command.spawn_locked()?);
        let native = computer.native();
        let log = computer.path().join("daemon.log");
        let answered: Result<()> = tilo_test_env::wait_for(
            ItsLog {
                log,
                what: "this computer's own daemon to answer its status",
            },
            || match ask_native(&native, &json!({ "native": "status" })) {
                Ok(status)
                    if status
                        .get("pid")
                        .and_then(Value::as_u64)
                        .is_some_and(|pid| pid != 0) =>
                {
                    Some(Ok(()))
                }
                Ok(_) | Err(_) => match computer.daemon.as_mut().map(Child::try_wait) {
                    Some(Ok(Some(ended))) => Some(Err(anyhow::anyhow!(
                        "the daemon ended ({ended}) before it answered\n{}",
                        computer.log()
                    ))),
                    Some(Err(error)) => {
                        Some(Err(anyhow::anyhow!("whether the daemon ended: {error:#}")))
                    }
                    _ => None,
                },
            },
        );
        answered?;
        Ok(computer)
    }

    /// Set this computer up for the account that runs it, as the first command of a computer
    /// with no person does: the founding call followed to where the person's part starts.
    fn found_person(&self) -> Result<()> {
        let place = tilo_api::daemon::DaemonPlace::at(&self.sockets(), &self.identity());
        place
            .found(&tilo_api::daemon::Founding::default())
            .map_err(|why| anyhow::anyhow!("setting this computer up: {why}"))?;
        Ok(())
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn home(&self) -> PathBuf {
        self.path().join("home")
    }

    fn runtime(&self) -> PathBuf {
        self.path().join("run")
    }

    fn identity(&self) -> PathBuf {
        self.path().join("identity")
    }

    fn sockets(&self) -> PathBuf {
        self.runtime().join("tilo")
    }

    fn native(&self) -> PathBuf {
        self.sockets().join(tilo_machine::DAEMON_NATIVE)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.path().join("daemon.log")).unwrap_or_default()
    }
}

impl Drop for OwnComputer {
    fn drop(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = daemon.kill();
            let _ = daemon.wait();
        }
    }
}

/// The daemon at `socket` answering `request` on its native endpoint, one line.
fn ask_native(socket: &Path, request: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .context("bounding the reply")?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).with_context(|| format!("the answer to {request}: {line:?}"))
}

/// A development build's id is read on the person's own daemon connection, as installing the
/// build is: `tilolang build --dev` runs `tilo-install dev-id`, which asks the daemon the way
/// `tilo-install install --dev` does. An MCP client's connection is the person's agent, and the
/// device answers it no id: only their own connection is admitted.
#[test]
fn a_development_builds_id_is_read_on_the_persons_own_connection_alone() -> Result<()> {
    let computer = OwnComputer::start()?;
    let keys = tilo_device_identity::persist::IdentityPaths::under(computer.identity());
    let device = tilo_device_identity::persist::load_device_key(&keys)
        .context("this computer's device key")?;
    let app = "org.tilo.tube.playback";
    let expected = tilo_app_package::dev_app_id(app, &device.verifying_key().to_bytes());

    // The read as `tilolang build --dev` asks it.
    let output = dev_id(&computer, app)?;
    ensure!(
        output.status.success(),
        "tilo-install dev-id failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        computer.log()
    );
    ensure!(
        String::from_utf8_lossy(&output.stdout).trim() == expected,
        "the device named {:?} for {app}, not {expected}",
        String::from_utf8_lossy(&output.stdout).trim()
    );

    // The connection the read was refused on while it was asked over the daemon's MCP endpoint.
    let reply = ask_native(
        &computer.native(),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"tilo_do",
                "arguments":{"path":"devices#dev-id","args":{"app": app}}}}),
    )?;
    let refused = reply.get("error").is_some()
        || reply.pointer("/result/isError") == Some(&Value::Bool(true));
    ensure!(
        refused,
        "an MCP client was answered a development build's id: {reply}\n{}",
        computer.log()
    );
    ensure!(
        reply.to_string().contains("not_permitted"),
        "the refusal must be the device's, not another's: {reply}\n{}",
        computer.log()
    );
    Ok(())
}

/// `tilo-install dev-id app`, the read a development build's compilation makes first.
fn dev_id(computer: &OwnComputer, app: &str) -> Result<std::process::Output> {
    let mut command = Command::new(tilo_test_env::sibling_binary("install", "tilo-install"));
    tilo_test_env::launch_env(&mut command);
    command
        .args(["dev-id", app])
        .env("HOME", computer.home())
        .env("XDG_RUNTIME_DIR", computer.runtime())
        .stdin(Stdio::null());
    Ok(command.output_locked()?)
}

/// The founding call a computer's first command makes, sent and taken up, its answer left to the
/// connection it came on: the person is made and their part starts behind it.
fn found_under_way(socket: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(socket)
        .with_context(|| format!("connecting to {}", socket.display()))?;
    stream
        .write_all(
            format!(
                "{}\n",
                json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                    "params":{"name":"tilo_do",
                        "arguments":{"path":"devices#found","args":{}}}})
            )
            .as_bytes(),
        )
        .context("sending the founding call")?;
    // The call's answer comes to this connection alone, held while the call is under way: read
    // and dropped on a thread of its own, so the test reaches the device as a caller does.
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stream).read_line(&mut line);
    });
    Ok(())
}

/// A person just founded at this computer reads a development build's id at once: the read is
/// the device's own package cache's, answered while their part has not started, and never waits
/// out the services that start behind the founding answer.
///
/// The daemon is held between its person's parts for five seconds, as a debug build's test hold
/// lets a test do: a read that waits for the person's part cannot come back inside the bound.
/// Measured on a fresh profile before the device's own cache answered it, the first read took
/// 8.7 s to 9.7 s, the person's part and the workspace the read's connection founded among the
/// wait; answered as the device's own it takes the program's start and one round trip.
#[test]
fn a_development_builds_id_is_read_at_once_after_founding() -> Result<()> {
    const HELD_MS: u64 = 5_000;
    let computer = OwnComputer::start_held(HELD_MS)?;
    let app = "org.tilo.tube.playback";
    // The founding call a computer's first command makes, answered under way: the person is
    // made and their part starts behind the answer. Its own answer is not what this test waits
    // on, so it is left to the connection it came on.
    found_under_way(&computer.native())?;
    // The person is claimed and their part has not begun: the hold the daemon says it is in.
    tilo_test_env::wait_for(
        ItsLog {
            log: computer.path().join("daemon.log"),
            what: "the daemon to hold before its person's part starts",
        },
        || {
            computer
                .log()
                .contains("a test holds the daemon between its person's parts")
                .then_some(())
        },
    );
    let keys = tilo_device_identity::persist::IdentityPaths::under(computer.identity());
    let device = tilo_device_identity::persist::load_device_key(&keys)
        .context("this computer's device key")?;
    let expected = tilo_app_package::dev_app_id(app, &device.verifying_key().to_bytes());
    let began = Instant::now();
    let read = dev_id(&computer, app)?;
    let took = began.elapsed();
    ensure!(
        read.status.success(),
        "tilo-install dev-id failed: {}\n{}",
        String::from_utf8_lossy(&read.stderr),
        computer.log()
    );
    ensure!(
        String::from_utf8_lossy(&read.stdout).trim() == expected,
        "the device named {:?} for {app}, not {expected}",
        String::from_utf8_lossy(&read.stdout).trim()
    );
    ensure!(
        took < Duration::from_secs(3),
        "the read after founding took {took:?}, waiting out the person's part the daemon held \
         for {HELD_MS} ms: {}",
        computer.log()
    );
    ensure!(
        !computer
            .log()
            .contains("this daemon's person's part started"),
        "the read waited until the person's part started: {}",
        computer.log()
    );
    Ok(())
}
