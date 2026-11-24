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
/// How long a daemon this test started has to answer on its native endpoint.
const LAUNCHER_START_CAP: Duration = Duration::from_secs(120);

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
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_RUNTIME_STANDALONE");
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
    /// Start the profile's daemon, and wait until it answers: its native endpoint, its status,
    /// and the device it is. Its person's workspace is not founded yet: nothing founds one at
    /// the daemon's start, since their desktop may still hand over the one it kept.
    fn start(&mut self) -> Result<()> {
        let log = std::fs::File::create(self.path().join("daemon.log"))?;
        self.daemon = Some(
            host(self.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn_locked()?,
        );
        let deadline = Instant::now()
            .checked_add(LAUNCHER_START_CAP)
            .context("the start's deadline")?;
        loop {
            if status(self.path()).is_ok_and(|status| {
                status
                    .get("pid")
                    .and_then(Value::as_u64)
                    .is_some_and(|pid| pid != 0)
            }) {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "the profile's daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(25));
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

    /// Found the person's workspace here, as the first call of theirs in it does: this device's
    /// own client of the daemon asking in the workspace their key names. A key's run and a
    /// desktop's first call found it the same way.
    fn found(&self) -> Result<()> {
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
        // The daemon admits the call only once the workspace is founded, which can take as long
        // as its own wait for the person's services: a client that is still there after that
        // waited on a founding that never finished, and says so with the daemon's own log.
        let deadline = Instant::now()
            .checked_add(LAUNCHER_START_CAP.saturating_add(Duration::from_secs(60)))
            .context("the first call's deadline")?;
        let ended = loop {
            if let Some(status) = client.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = client.kill();
                let _ = client.wait();
                bail!(
                    "the person's first call founded no workspace and did not end (who: {})\n{}",
                    who_briefly(self.path()),
                    self.log()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let mut answer = String::new();
        std::io::Read::read_to_string(&mut said, &mut answer)?;
        let mut reason = String::new();
        std::io::Read::read_to_string(&mut why, &mut reason)?;
        ensure!(
            ended.success(),
            "the person's first call failed: {reason}\n{}",
            self.log()
        );
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
        Ok(())
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
        set_up(root.path())?;
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
        if let Ok(stream) = UnixStream::connect(self.path().join("native.sock")) {
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
            let _ = daemon.wait();
        }
    }
}
/// Names the profile whose computer [`a_computer_is_set_up`] sets Tilo up on.
const SET_UP: &str = "TILO_TEST_SET_UP_PROFILE";

/// Set Tilo up on the computer the daemon of `profile` runs as: the person it acts as, kept in
/// the profile's identity, as Tilo set up on a computer keeps them. A daemon with no identity
/// and no login says to set Tilo up first, and serves nothing.
fn set_up(profile: &Path) -> Result<()> {
    let module = module_path!();
    let name = format!(
        "{}::a_computer_is_set_up",
        module.split_once("::").map_or(module, |(_, within)| within)
    );
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", &name, "--nocapture", "--test-threads", "1"])
        .env(SET_UP, profile)
        .env("XDG_CONFIG_HOME", profile.join("config"))
        .env("XDG_DATA_HOME", profile.join("data"))
        .env("XDG_CACHE_HOME", profile.join("cache"))
        .env("XDG_STATE_HOME", profile.join("state"))
        .env("TILO_IDENTITY_ROOT", profile.join("identity"))
        .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .stdout(Stdio::null())
        .status_locked()?;
    ensure!(
        status.success(),
        "setting Tilo up for the daemon failed: {status}"
    );
    Ok(())
}

/// The body of [`set_up`]'s child, and a no-op in every other run.
#[test]
fn a_computer_is_set_up() {
    if std::env::var_os(SET_UP).is_none() {
        return;
    }
    let registry = tilo_identity::store::IdentityRegistryStore::from_env().unwrap();
    tilo_identity::provisioning::ensure_collaboration_identity(
        registry.identity_dir(),
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
}

/// Who the daemon says it is, asked with a short bound: a daemon stuck admitting the person's
/// call answers nothing at all.
fn who_briefly(profile: &Path) -> String {
    (|| -> Result<Value> {
        let mut stream = UnixStream::connect(profile.join("native.sock"))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"{\"native\":\"account#who\"}\n")?;
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line)?;
        Ok(serde_json::from_str(&line)?)
    })()
    .map_or_else(
        |error| format!("unanswered: {error}"),
        |who| who.to_string(),
    )
}
/// The runtime's answer to `request` on the profile's socket.
fn ask(profile: &Path, request: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(profile.join("native.sock"))
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
    let stale = std::os::unix::net::UnixListener::bind(profile.path().join("native.sock"))?;
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
    let listed = ask(
        profile.path(),
        &json!({"jsonrpc":"2.0","id":1,"method":"resources/list","params":{}}),
    )?;
    ensure!(
        listed
            .get("result")
            .and_then(|result| result.get("resources"))
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "nothing runs, so resources/list lists no app: {listed}"
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
        let mut stream = UnixStream::connect(profile.join(tilo_runtime::COLLAB_SOCKET))
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
    let deadline = Instant::now()
        .checked_add(LAUNCHER_START_CAP)
        .context("the program's deadline")?;
    let refused = loop {
        let said = std::fs::read_to_string(&file).unwrap_or_default();
        if !said.is_empty() {
            break said;
        }
        ensure!(
            program.try_wait()?.is_none() && Instant::now() < deadline,
            "the attaching program said nothing:\n{}",
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(25));
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

/// A built development app: its manifest and payload in one directory, the SDK app with
/// `lifetime_ms` and, with `spawn_child`, one that starts a second process of itself. `app` is
/// the app's own id; the device it is installed on gives it the dev id it runs as.
fn dev_app_build(
    profile: &Path,
    app: &str,
    lifetime_ms: u64,
    spawn_child: bool,
) -> Result<PathBuf> {
    let dir = profile.join(format!("dev-app-{}", app.replace('.', "-")));
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(
        tilo_test_env::package_binary!("tilo-sdk-app"),
        dir.join("app"),
    )?;
    std::fs::write(dir.join("tilo-app-lifetime-ms"), format!("{lifetime_ms}\n"))?;
    if spawn_child {
        std::fs::write(dir.join("tilo-app-spawn-child"), b"")?;
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
collab = "optional@48"

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
    let payload = ["app", "tilo-app-lifetime-ms", "tilo-app-spawn-child"]
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
        &profile.join("native.sock"),
        &package_actions::Change::InstallDev {
            manifest: package_actions::Uploaded {
                source: dir.join("tilo-app.toml").display().to_string(),
            },
            payload,
        },
    )
    .map_err(anyhow::Error::msg)?;
    ensure!(!report.lines.is_empty(), "the device installed nothing");
    Ok(tilo_app_package::dev_app_id(
        app,
        &device.verifying_key().to_bytes(),
    ))
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

/// The app instances the daemon's MCP `resources/list` reports, once: each as its `app_id` and
/// the `uri` of its own connection.
fn apps_once(profile: &Path) -> Result<Vec<Value>> {
    let reply = ask(
        profile,
        &json!({"jsonrpc":"2.0","id":1,"method":"resources/list","params":{}}),
    )?;
    let resources = reply
        .get("result")
        .and_then(|result| result.get("resources"))
        .and_then(Value::as_array)
        .with_context(|| format!("the reply reports no apps: {reply}"))?;
    // Each is named `<app id> (<instance>)`.
    Ok(resources
        .iter()
        .map(|resource| {
            let name = resource["name"].as_str().unwrap_or_default();
            let app_id = name.split_once(" (").map_or(name, |(app, _)| app);
            json!({"app_id": app_id, "peer_id": resource["uri"]})
        })
        .collect())
}

/// The app instances `resources/list` reports, once `count` of them run.
fn listed_apps(profile: &Path, count: usize) -> Result<Vec<Value>> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let apps = apps_once(profile)?;
        if apps.len() == count {
            return Ok(apps);
        }
        ensure!(
            Instant::now() < deadline,
            "expected {count} apps, got {apps:?}"
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

/// An app whose process starts another of itself gets two connections, and both are its
/// instances: ordinary process spawning after the app adopted the services it was handed.
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
