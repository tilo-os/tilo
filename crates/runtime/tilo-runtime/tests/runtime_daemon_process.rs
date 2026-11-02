//! The runtime host a Tilo login runs (`tilo-runtime-mcp`) across real process
//! boundaries: no global test environment mutation and no GUI. The host is
//! Cargo's build of the binary, which Cargo builds, current, for every
//! integration test run.
use anyhow::{Context as _, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tilo_test_env::SpawnUnderEnvLock as _;

// These services decode peer bytes in worker processes started from this
// executable, which is the test binary here.
tilo_automerge_isolate::test_worker!();

/// The runtime host every test launches.
fn runtime_binary() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-runtime-mcp")
}
/// The launcher's absolute bound on a daemon start (`STARTUP.cap` in
/// `tilo_runtime::daemon`). A launcher that fails sooner exits with the reason.
const LAUNCHER_START_CAP: Duration = Duration::from_secs(120);

fn host(profile: &Path) -> Command {
    let mut command = Command::new(runtime_binary());
    command
        .env("TILO_RUNTIME_PROFILE", profile)
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_RUNTIME_STANDALONE");
    command
}
struct DaemonProfile {
    root: tempfile::TempDir,
}
impl DaemonProfile {
    fn new() -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .prefix("tilo-d-")
            .tempdir_in(std::fs::canonicalize("/tmp").context("resolving /tmp")?)
            .context("creating the daemon profile")?;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
            .context("making the daemon profile private")?;
        Ok(Self { root })
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
    }
}
/// The runtime's answer to a `status` request on the profile's socket.
fn status(profile: &Path) -> Result<Value> {
    let mut stream = UnixStream::connect(profile.join("native.sock"))
        .context("connecting to the runtime's profile socket")?;
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .context("bounding the status reply")?;
    stream
        .write_all(b"{\"native\":\"status\"}\n")
        .context("asking the runtime for its status")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).with_context(|| format!("the status reply: {line:?}"))
}
/// The daemon serving `profile`'s socket, as its status names it.
fn daemon(profile: &Path) -> Result<Value> {
    let status = status(profile)?;
    status.get("pid").cloned().context("a status without a pid")
}
fn run_true(profile: &Path, app: &str) -> Result<std::process::Output> {
    Ok(host(profile)
        .args(["--run-app", app, "--", "/usr/bin/true"])
        .output_locked()?)
}
/// [`run_true`]'s app, which must have started; the launcher's stderr is why not.
fn app_starts(profile: &Path, app: &str) -> Result<()> {
    let output = run_true(profile, app)?;
    ensure!(
        output.status.success(),
        "starting {app} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
/// A helper app's collaboration manager once it has joined the broker.
/// Construction returns while the connection is still being made; the
/// helper polls until it settles.
fn connected_manager() -> Result<ui::collaboration::CollaborationManager> {
    let mut manager = ui::collaboration::CollaborationManager::new();
    let started = Instant::now();
    while manager.state() == ui::collaboration::CollaborationState::Connecting
        && started.elapsed() < tilo_test_env::hang_bound()
    {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    ensure!(
        manager.is_connected(),
        "the helper app never connected: {:?}",
        manager.state()
    );
    Ok(manager)
}
/// Ask for the apps `tilo_list_apps` reports, once.
fn apps_once(stream: &mut UnixStream) -> Result<Vec<Value>> {
    let mut reader = BufReader::new(stream.try_clone()?);
    writeln!(
        stream,
        "{}",
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"tilo_list_apps","arguments":{}}})
    )?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let reply: Value =
        serde_json::from_str(&line).with_context(|| format!("the apps reply: {line:?}"))?;
    reply
        .get("result")
        .and_then(|result| result.get("structuredContent"))
        .and_then(|content| content.get("apps"))
        .and_then(Value::as_array)
        .cloned()
        .with_context(|| format!("the reply reports no apps: {reply}"))
}
/// The apps a `tilo_list_apps` request reports, once `count` of them run.
fn listed_apps(stream: &mut UnixStream, count: usize) -> Result<Vec<Value>> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let apps = apps_once(stream)?;
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

#[test]
fn first_launch_reuse_and_stale_socket_recovery() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let stale = std::os::unix::net::UnixListener::bind(profile.path().join("native.sock"))?;
    drop(stale);
    app_starts(profile.path(), "dev.tilo.first")?;
    let before = daemon(profile.path())?;
    app_starts(profile.path(), "dev.tilo.second")?;
    ensure!(
        daemon(profile.path())? == before,
        "the second launch started another daemon"
    );
    let published = status(profile.path())?;
    ensure!(
        published
            .get("room")
            .and_then(Value::as_str)
            .is_some_and(|room| !room.is_empty()),
        "the status names no workspace: {published}"
    );
    Ok(())
}
#[test]
fn simultaneous_first_launch_has_one_owner_and_mcp_eof_keeps_it_alive() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let mut children = (0..5)
        .map(|_| {
            host(profile.path())
                .args(["--run-app", "dev.tilo.race", "--", "/usr/bin/true"])
                .spawn_locked()
        })
        .collect::<std::io::Result<Vec<Child>>>()?;
    for child in &mut children {
        ensure!(child.wait()?.success(), "a racing launcher failed");
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
fn run_app_preserves_arguments_cwd_and_connected_descriptors() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let output = host(profile.path())
        .env_remove("TILO_WORKSPACE_ID")
        .env_remove("TILO_PARTICIPATION")
        .current_dir("/tmp")
        .args(["--run-app", "dev.tilo.argv", "--", "/bin/sh", "-c", "printf '%s\\n' \"$PWD\" \"$1\" \"$2\" \"$3\" \"$TILO_PACKAGE_ID\" \"$TILO_PARTICIPATION\" \"$TILO_SVC_COLLAB_FD\" \"$TILO_SVC_IDENTITY_FD\"", "sentinel", "two words", "--strange=✓", "--apps"])
        .output_locked()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).context("the app's output")?;
    let working_directory = std::fs::canonicalize("/tmp")?;
    ensure!(
        text.lines().next() == working_directory.to_str(),
        "the app's working directory is not the launcher's: {text}"
    );
    ensure!(
        text.contains("\ntwo words\n--strange=✓\n--apps\ndev.tilo.argv\nauthor\n"),
        "{text}"
    );
    ensure!(text.lines().count() == 8, "{text}");
    Ok(())
}
#[test]
fn live_profile_lock_fails_without_unlinking_owners_socket() -> Result<()> {
    let profile = DaemonProfile::new()?;
    app_starts(profile.path(), "dev.tilo.first")?;
    let before = daemon(profile.path())?;
    let competing = host(profile.path()).arg("--daemon").output_locked()?;
    ensure!(
        !competing.status.success(),
        "a second daemon took a profile that is in use"
    );
    // It fails on the held profile lock itself, not after waiting for it.
    let stderr = String::from_utf8_lossy(&competing.stderr);
    ensure!(
        stderr.contains("is in use"),
        "a competing daemon must give up on the held profile lock at once: {stderr}"
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
        !run_true(&root.path().join("link"), "dev.tilo.test")?
            .status
            .success(),
        "a symlinked profile was accepted"
    );
    ensure!(
        !run_true(Path::new("relative-profile"), "dev.tilo.test")?
            .status
            .success(),
        "a relative profile was accepted"
    );
    Ok(())
}

#[test]
fn independent_sdk_apps_late_join_and_survive_another_app_exit() -> Result<()> {
    struct Children(Vec<Child>);
    impl Drop for Children {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    const TEST: &str = "independent_sdk_apps_late_join_and_survive_another_app_exit";
    if let Ok(app_id) = std::env::var("TILO_NATIVE_SDK_HELPER") {
        runtime_portal::native::ensure_attached(&app_id)?;
        let mut manager = connected_manager()?;
        manager.publish_action_directory(ui::collaboration::local_app_id());
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let profile = DaemonProfile::new()?;
    let mut apps = Children(Vec::new());
    let start_app = |id: &str| -> Result<Child> {
        let log = std::fs::File::create(profile.path().join(format!("{id}.log")))?;
        Ok(Command::new(std::env::current_exe()?)
            .args(["--exact", TEST, "--nocapture"])
            .env("TILO_NATIVE_SDK_HELPER", id)
            .env("TILO_RUNTIME_PROFILE", profile.path())
            .env("TILO_RUNTIME_BINARY", runtime_binary())
            .env_remove("TILO_SVC_COLLAB_FD")
            .env_remove("TILO_SVC_IDENTITY_FD")
            .env_remove("TILO_RUNTIME_STANDALONE")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn_locked()?)
    };
    apps.0.push(start_app("dev.tilo.independent-first")?);
    // The app's own launcher bounds the daemon start and exits with the
    // reason when it fails, so wait as long as that launcher is alive.
    let deadline = Instant::now()
        .checked_add(LAUNCHER_START_CAP)
        .context("the launcher's bound is past the clock's range")?;
    while UnixStream::connect(profile.path().join("native.sock")).is_err() {
        let first = apps.0.first_mut().context("the first app is gone")?;
        ensure!(
            first.try_wait()?.is_none() && Instant::now() < deadline,
            "first SDK process did not start daemon; child log: {}",
            std::fs::read_to_string(profile.path().join("dev.tilo.independent-first.log"))
                .unwrap_or_else(|error| format!("child log unavailable: {error}"))
                .chars()
                .take(8192)
                .collect::<String>()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let mut stream = UnixStream::connect(profile.path().join("native.sock"))?;
    stream.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
    let only = listed_apps(&mut stream, 1)?;
    ensure!(
        app_ids(&only) == ["dev.tilo.independent-first"],
        "the first app is not the only one listed: {only:?}"
    );
    let before = daemon(profile.path())?;
    apps.0.push(start_app("dev.tilo.independent-second")?);
    let both = listed_apps(&mut stream, 2)?;
    let [one, two] = both.as_slice() else {
        bail!("expected two apps, got {both:?}");
    };
    ensure!(
        one.get("peer_id") != two.get("peer_id"),
        "both apps hold the same connection: {both:?}"
    );
    ensure!(
        daemon(profile.path())? == before,
        "the second app restarted the daemon"
    );
    let first = apps.0.first_mut().context("the first app is gone")?;
    first.kill()?;
    first.wait()?;
    let survivor = listed_apps(&mut stream, 1)?;
    ensure!(
        app_ids(&survivor) == ["dev.tilo.independent-second"],
        "the app that stayed is not the one listed: {survivor:?}"
    );
    ensure!(
        apps.0
            .get_mut(1)
            .context("the second app is gone")?
            .try_wait()?
            .is_none(),
        "the second app died with the first"
    );
    Ok(())
}

#[test]
fn public_profile_and_symlinked_session_lock_are_rejected() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let profile = DaemonProfile::new()?;
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o777))?;
    let output = run_true(profile.path(), "dev.tilo.test")?;
    ensure!(!output.status.success(), "a public profile was accepted");
    ensure!(
        std::fs::metadata(profile.path())?.permissions().mode() & 0o777 == 0o777,
        "the rejected launcher changed the profile"
    );
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))?;
    let target = profile.path().join("other-file");
    std::fs::write(&target, "untouched")?;
    std::os::unix::fs::symlink(&target, profile.path().join("runtime.lock"))?;
    ensure!(
        !run_true(profile.path(), "dev.tilo.test")?.status.success(),
        "a symlinked session lock was accepted"
    );
    ensure!(
        std::fs::read_to_string(target)? == "untouched",
        "the rejected launcher wrote through the symlinked lock"
    );
    Ok(())
}

#[test]
fn ordinary_sdk_parent_child_get_distinct_connections_in_one_runtime() -> Result<()> {
    use std::os::unix::process::CommandExt;
    struct Family(Child);
    impl Drop for Family {
        fn drop(&mut self) {
            if let Some(group) = i32::try_from(self.0.id()).ok().and_then(i32::checked_neg) {
                unsafe {
                    libc::kill(group, libc::SIGKILL);
                }
            }
            let _ = self.0.wait();
        }
    }
    const TEST: &str = "ordinary_sdk_parent_child_get_distinct_connections_in_one_runtime";
    const APP: &str = "dev.tilo.parent-child";
    if let Ok(stage) = std::env::var("TILO_SDK_FAMILY_STAGE") {
        runtime_portal::native::ensure_attached(APP)?;
        let mut manager = connected_manager()?;
        manager.publish_action_directory(ui::collaboration::local_app_id());
        let _child = if stage == "parent" {
            // No service-environment clearing: this is ordinary Command spawn
            // after the SDK has adopted both parent's service descriptors.
            Some(
                Command::new(std::env::current_exe()?)
                    .args(["--exact", TEST, "--nocapture"])
                    .env("TILO_SDK_FAMILY_STAGE", "child")
                    .spawn_locked()?,
            )
        } else {
            None
        };
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let profile = DaemonProfile::new()?;
    let log_path = profile.path().join("family.log");
    let log = std::fs::File::create(&log_path)?;
    let mut command = host(profile.path());
    command
        .args(["--run-app", APP, "--"])
        .arg(std::env::current_exe()?)
        .args(["--exact", TEST, "--nocapture"])
        .env("TILO_SDK_FAMILY_STAGE", "parent")
        .env("TILO_RUNTIME_BINARY", runtime_binary())
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let mut family = Family(command.spawn_locked()?);
    // The launcher bounds the daemon start itself and exits with the reason
    // when it fails, which the loop reports at once.
    let deadline = Instant::now()
        .checked_add(LAUNCHER_START_CAP)
        .context("the launcher's bound is past the clock's range")?;
    loop {
        ensure!(
            family.0.try_wait()?.is_none(),
            "{}",
            std::fs::read_to_string(&log_path)?
        );
        if let Ok(mut stream) = UnixStream::connect(profile.path().join("native.sock")) {
            stream.set_read_timeout(Some(tilo_test_env::hang_bound()))?;
            let apps = apps_once(&mut stream)?;
            if let [one, two] = apps.as_slice() {
                ensure!(
                    app_ids(&apps).iter().all(|app| *app == APP),
                    "another app was reported: {apps:?}"
                );
                ensure!(
                    one.get("peer_id") != two.get("peer_id"),
                    "the parent and child share a connection: {apps:?}"
                );
                break;
            }
        }
        ensure!(
            Instant::now() < deadline,
            "parent/child did not both register: {}",
            std::fs::read_to_string(&log_path)?
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[test]
fn explicit_profile_overrides_are_propagated_to_app_descendants() -> Result<()> {
    let profile = DaemonProfile::new()?;
    let output = host(profile.path())
        .env("TILO_RUNTIME_PROFILE", "/must-not-be-used")
        .arg("--profile")
        .arg(profile.path())
        .args([
            "--run-app",
            "dev.tilo.profile",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s' \"$TILO_RUNTIME_PROFILE\"",
        ])
        .output_locked()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed = String::from_utf8(output.stdout).context("the app's output")?;
    ensure!(
        Some(printed.as_str()) == profile.path().to_str(),
        "the app did not inherit the explicit profile: {printed:?}"
    );
    Ok(())
}
