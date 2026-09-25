//! Real process boundaries: no global test environment mutation and no GUI.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn host(profile: &Path) -> Command {
    let mut command = Command::new(
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("qos-runtime-mcp"),
    );
    command
        .env("QOS_RUNTIME_PROFILE", profile)
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("QOS_RUNTIME_STANDALONE");
    command
}
struct DaemonProfile {
    root: tempfile::TempDir,
}
impl DaemonProfile {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .prefix("qos-d-")
            .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root }
    }
    fn path(&self) -> &Path {
        self.root.path()
    }
}
impl Drop for DaemonProfile {
    fn drop(&mut self) {
        if let Ok(stream) = UnixStream::connect(self.path().join("native.sock")) {
            let mut stream = stream;
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = stream.write_all(b"{\"native\":\"status\"}\n");
            let mut line = String::new();
            if BufReader::new(stream).read_line(&mut line).is_ok() {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if let Some(pid) = value["pid"].as_i64() {
                        unsafe {
                            libc::kill(i32::try_from(pid).unwrap(), libc::SIGTERM);
                        }
                    }
                }
            }
        }
    }
}
fn status(profile: &Path) -> Value {
    let mut stream = UnixStream::connect(profile.join("native.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"{\"native\":\"status\"}\n").unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn run_true(profile: &Path, app: &str) -> std::process::Output {
    host(profile)
        .args(["--run-app", app, "--", "/usr/bin/true"])
        .output()
        .unwrap()
}
#[test]
fn first_launch_reuse_and_stale_socket_recovery() {
    let profile = DaemonProfile::new();
    let stale = std::os::unix::net::UnixListener::bind(profile.path().join("native.sock")).unwrap();
    drop(stale);
    let first = run_true(profile.path(), "dev.qos.first");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let before = status(profile.path());
    let second = run_true(profile.path(), "dev.qos.second");
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(status(profile.path())["pid"], before["pid"]);
    assert!(before["room"].as_str().is_some_and(|room| !room.is_empty()));
}
#[test]
fn simultaneous_first_launch_has_one_owner_and_mcp_eof_keeps_it_alive() {
    let profile = DaemonProfile::new();
    let mut children: Vec<Child> = (0..5)
        .map(|_| {
            host(profile.path())
                .args(["--run-app", "dev.qos.race", "--", "/usr/bin/true"])
                .spawn()
                .unwrap()
        })
        .collect();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    let before = status(profile.path());
    let mut proxy = host(profile.path())
        .arg("--connect")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        proxy.stdin.take().unwrap(),
        "{}",
        json!({"jsonrpc":"2.0","id":7,"method":"ping"})
    )
    .unwrap();
    let output = proxy.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"], json!({}));
    assert_eq!(status(profile.path())["pid"], before["pid"]);
}
#[test]
fn run_app_preserves_arguments_cwd_and_connected_descriptors() {
    let profile = DaemonProfile::new();
    let output = host(profile.path()).current_dir("/tmp").args(["--run-app", "dev.qos.argv", "--", "/bin/sh", "-c", "printf '%s\\n' \"$PWD\" \"$1\" \"$2\" \"$3\" \"$TILO_PACKAGE_ID\" \"$TILO_SVC_COLLAB_FD\" \"$TILO_SVC_IDENTITY_FD\"", "sentinel", "two words", "--strange=✓", "--apps"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text.lines().next().unwrap(),
        std::fs::canonicalize("/tmp").unwrap().to_str().unwrap()
    );
    assert!(
        text.contains("\ntwo words\n--strange=✓\n--apps\ndev.qos.argv\n"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 7);
}
#[test]
fn live_profile_lock_fails_without_unlinking_owners_socket() {
    let profile = DaemonProfile::new();
    let first = run_true(profile.path(), "dev.qos.first");
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let before = status(profile.path());
    let start = Instant::now();
    let competing = host(profile.path()).arg("--daemon").output().unwrap();
    assert!(!competing.status.success());
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(status(profile.path())["pid"], before["pid"]);
}
#[test]
fn unsafe_profile_and_relative_override_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(root.path(), root.path().join("link")).unwrap();
    assert!(
        !run_true(&root.path().join("link"), "dev.qos.test")
            .status
            .success()
    );
    assert!(
        !run_true(Path::new("relative-profile"), "dev.qos.test")
            .status
            .success()
    );
}

#[test]
fn independent_sdk_apps_late_join_and_survive_another_app_exit() {
    const TEST: &str = "runtime_host::daemon_process_tests::independent_sdk_apps_late_join_and_survive_another_app_exit";
    if let Ok(app_id) = std::env::var("QOS_NATIVE_SDK_HELPER") {
        runtime_portal::native::ensure_attached(&app_id).unwrap();
        let mut manager = ui::collaboration::CollaborationManager::new();
        assert!(manager.is_connected());
        manager.publish_action_directory(ui::collaboration::local_app_id());
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    struct Children(Vec<Child>);
    impl Drop for Children {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let profile = DaemonProfile::new();
    let mut apps = Children(Vec::new());
    let start_app = |id: &str| {
        let log = std::fs::File::create(profile.path().join(format!("{id}.log"))).unwrap();
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env("QOS_NATIVE_SDK_HELPER", id)
            .env("QOS_RUNTIME_PROFILE", profile.path())
            .env(
                "QOS_RUNTIME_BINARY",
                std::env::current_exe()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("qos-runtime-mcp"),
            )
            .env_remove("TILO_SVC_COLLAB_FD")
            .env_remove("TILO_SVC_IDENTITY_FD")
            .env_remove("QOS_RUNTIME_STANDALONE")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap()
    };
    apps.0.push(start_app("dev.qos.independent-first"));
    let deadline = Instant::now().checked_add(Duration::from_secs(15)).unwrap();
    while UnixStream::connect(profile.path().join("native.sock")).is_err() {
        assert!(
            Instant::now() < deadline,
            "first SDK process did not start daemon; child log: {}",
            std::fs::read_to_string(profile.path().join("dev.qos.independent-first.log"))
                .unwrap_or_else(|error| format!("child log unavailable: {error}"))
                .chars()
                .take(8192)
                .collect::<String>()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let mut stream = UnixStream::connect(profile.path().join("native.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut roots = |count| {
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        loop {
            writeln!(stream, "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"qos_list_apps","arguments":{}}})).unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let reply: Value = serde_json::from_str(&line).unwrap();
            let apps = reply["result"]["structuredContent"]["apps"]
                .as_array()
                .unwrap()
                .clone();
            if apps.len() == count {
                return apps;
            }
            assert!(
                Instant::now() < deadline,
                "expected {count} apps, got {reply}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    assert_eq!(roots(1)[0]["app_id"], "dev.qos.independent-first");
    let before = status(profile.path());
    apps.0.push(start_app("dev.qos.independent-second"));
    let both = roots(2);
    assert_ne!(both[0]["peer_id"], both[1]["peer_id"]);
    assert_eq!(status(profile.path())["pid"], before["pid"]);
    apps.0[0].kill().unwrap();
    apps.0[0].wait().unwrap();
    assert_eq!(roots(1)[0]["app_id"], "dev.qos.independent-second");
    assert!(apps.0[1].try_wait().unwrap().is_none());
}

#[test]
fn public_profile_and_symlinked_session_lock_are_rejected() {
    use std::os::unix::fs::PermissionsExt;
    let profile = DaemonProfile::new();
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let output = run_true(profile.path(), "dev.qos.test");
    assert!(!output.status.success(), "a public profile was accepted");
    assert_eq!(
        std::fs::metadata(profile.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o777
    );
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let target = profile.path().join("other-file");
    std::fs::write(&target, "untouched").unwrap();
    std::os::unix::fs::symlink(&target, profile.path().join("runtime.lock")).unwrap();
    assert!(!run_true(profile.path(), "dev.qos.test").status.success());
    assert_eq!(std::fs::read_to_string(target).unwrap(), "untouched");
}

#[test]
fn ordinary_sdk_parent_child_get_distinct_connections_in_one_runtime() {
    const TEST: &str = "runtime_host::daemon_process_tests::ordinary_sdk_parent_child_get_distinct_connections_in_one_runtime";
    const APP: &str = "dev.qos.parent-child";
    if let Ok(stage) = std::env::var("QOS_SDK_FAMILY_STAGE") {
        runtime_portal::native::ensure_attached(APP).unwrap();
        let mut manager = ui::collaboration::CollaborationManager::new();
        assert!(manager.is_connected());
        manager.publish_action_directory(ui::collaboration::local_app_id());
        let _child = if stage == "parent" {
            // No service-environment clearing: this is ordinary Command spawn
            // after the SDK has adopted both parent's service descriptors.
            Some(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", TEST, "--nocapture"])
                    .env("QOS_SDK_FAMILY_STAGE", "child")
                    .spawn()
                    .unwrap(),
            )
        } else {
            None
        };
        loop {
            let _ = manager.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    use std::os::unix::process::CommandExt;
    struct Family(Child);
    impl Drop for Family {
        fn drop(&mut self) {
            let group = i32::try_from(self.0.id()).unwrap().checked_neg().unwrap();
            unsafe {
                libc::kill(group, libc::SIGKILL);
            }
            let _ = self.0.wait();
        }
    }
    let profile = DaemonProfile::new();
    let log_path = profile.path().join("family.log");
    let log = std::fs::File::create(&log_path).unwrap();
    let mut command = host(profile.path());
    command
        .args(["--run-app", APP, "--"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env("QOS_SDK_FAMILY_STAGE", "parent")
        .env(
            "QOS_RUNTIME_BINARY",
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("qos-runtime-mcp"),
        )
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log));
    let mut family = Family(command.spawn().unwrap());
    let deadline = Instant::now().checked_add(Duration::from_secs(15)).unwrap();
    loop {
        assert!(
            family.0.try_wait().unwrap().is_none(),
            "{}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        if let Ok(mut stream) = UnixStream::connect(profile.path().join("native.sock")) {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            writeln!(stream, "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"qos_list_apps","arguments":{}}})).unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            let reply: Value = serde_json::from_str(&line).unwrap();
            let apps = reply["result"]["structuredContent"]["apps"]
                .as_array()
                .unwrap();
            if apps.len() == 2 {
                assert!(apps.iter().all(|app| app["app_id"] == APP));
                assert_ne!(apps[0]["peer_id"], apps[1]["peer_id"]);
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "parent/child did not both register: {}",
            std::fs::read_to_string(&log_path).unwrap()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn explicit_profile_overrides_are_propagated_to_app_descendants() {
    let profile = DaemonProfile::new();
    let output = host(profile.path())
        .env("QOS_RUNTIME_PROFILE", "/must-not-be-used")
        .arg("--profile")
        .arg(profile.path())
        .args([
            "--run-app",
            "dev.qos.profile",
            "--",
            "/bin/sh",
            "-c",
            "printf '%s' \"$QOS_RUNTIME_PROFILE\"",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        profile.path().to_str().unwrap()
    );
}

#[test]
fn managed_connection_preparation_clears_ancestor_automatic_metadata() {
    let profile = DaemonProfile::new();
    let collab_path = profile.path().join("collab.sock");
    let identity_path = profile.path().join("identity.sock");
    let _collab = std::os::unix::net::UnixListener::bind(&collab_path).unwrap();
    let _identity = std::os::unix::net::UnixListener::bind(&identity_path).unwrap();
    let mut command = Command::new("/usr/bin/true");
    command
        .env("QOS_RUNTIME_CONNECTION_OWNER_PID", "1")
        .env("QOS_RUNTIME_RESUME_LIVE", "dev.qos.parent");
    let _connections = super::unix::Connections::prepare(
        &mut command,
        "dev.qos.child",
        "workspace",
        &collab_path,
        &identity_path,
    )
    .unwrap();
    for key in [
        "QOS_RUNTIME_CONNECTION_OWNER_PID",
        "QOS_RUNTIME_RESUME_LIVE",
    ] {
        assert_eq!(
            command.get_envs().find(|(name, _)| *name == key).unwrap().1,
            None
        );
    }
}
