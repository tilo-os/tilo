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
    let mut command = Command::new(runtime_binary());
    command
        .env("TILO_RUNTIME_PROFILE", profile)
        // The device's key and certificates live in the profile, as the service names them to
        // this computer: the daemon starts as the person `set_up` certified only when the
        // certificate it reads is the one made here.
        .env("TILO_IDENTITY_ROOT", profile.join("identity"))
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_RUNTIME_STANDALONE");
    command
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
            String::from_utf8_lossy(&refused.stderr)
                .contains("is not installed on this computer"),
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
        runtime_portal::launch_handoff::write(&mut stream, APP, &workspace)
            .context("naming the app this program claims to be")?;
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(
            "TILO_SVC_COLLAB_FD",
            std::os::fd::AsRawFd::as_raw_fd(&stream).to_string(),
        );
        environment.set("TILO_PACKAGE_ID", APP);
        environment.set("TILO_WORKSPACE_ID", &workspace);
        let mut manager = ui::collaboration::CollaborationManager::new();
        let deadline = Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .context("the refusal's deadline")?;
        loop {
            let state = manager.state();
            if let ui::collaboration::CollaborationState::Offline(_) = &state {
                std::fs::write(&file, format!("{state:?}"))?;
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "the unattested program was never refused: {state:?}"
            );
            // Publishing opens one of the app's records, which the workspace service decides for an app:
            // what a program that attached for itself asks first.
            let _ = manager.publish_action_directory(ui::collaboration::local_app_id());
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(25));
        }
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
    let name = format!(
        "{}::a_program_that_attaches_without_a_launch_is_refused_and_told_how_to_run",
        module_path!()
    );
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", name.as_str(), "--nocapture", "--test-threads", "1"])
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
