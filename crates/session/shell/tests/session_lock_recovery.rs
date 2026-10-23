use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PASSWORD: &str = "synthetic-lock-test";

struct Session(Child, PathBuf);

impl Drop for Session {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "native fixture diagnostics: {}",
                fs::read_to_string(self.1.join("child.log")).unwrap_or_default()
            );
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Session {
    fn start(root: &Path, unavailable_verifier: bool) -> (Self, UnixStream) {
        let socket = root.join("ctl");
        let _ = fs::remove_file(&socket);
        let log = fs::File::create(root.join("child.log")).unwrap();
        let mut command = Command::new(tilo_test_env::package_binary!("shell"));
        command
            .args(["--mode", "native", "--backend", "headless"])
            .env("TILO_TEST_CONTROL", &socket)
            .env("TILO_SESSION_LOGOUT_REQUEST", root.join("logout-request"))
            .env("TILO_LOCK_TEST_PASSWORD", PASSWORD)
            .env("TILO_IDLE_LOCK_SECS", "0")
            .env("TILO_IDLE_BLANK_SECS", "0")
            .env("TILO_IDLE_SUSPEND_SECS", "0")
            .env("TILO_HEADLESS_SIZE", "640x480")
            .env("XDG_RUNTIME_DIR", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log));
        if unavailable_verifier {
            command
                .env_remove("TILO_LOCK_TEST_PASSWORD")
                .env("TILO_UNLOCK_HELPER", root.join("missing-unlock-helper"));
        }
        let child = command.spawn().unwrap();
        let mut session = Self(child, root.to_owned());
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Ok(stream) = UnixStream::connect(&socket) {
                stream
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                return (session, stream);
            }
            assert!(
                session.0.try_wait().unwrap().is_none(),
                "compositor exited: {}",
                fs::read_to_string(root.join("child.log")).unwrap()
            );
            assert!(Instant::now() < deadline, "control socket never appeared");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn stop(&mut self, signal: i32) {
        // SAFETY: signal only this fixture's live child, never a process pattern.
        assert_eq!(
            unsafe { libc::kill(i32::try_from(self.0.id()).unwrap(), signal) },
            0
        );
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert_eq!(
                    status.success(),
                    signal != libc::SIGKILL,
                    "ordered TERM must run teardown"
                );
                return;
            }
            assert!(Instant::now() < deadline, "child did not stop");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn rpc(stream: &mut UnixStream, command: &serde_json::Value) -> serde_json::Value {
    let request = format!("{command}\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut line = String::new();
    assert!(
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap_or_else(|error| panic!("control response to {command}: {error}"))
            > 0
    );
    serde_json::from_str(&line).unwrap()
}

fn locked(stream: &mut UnixStream) -> bool {
    rpc(stream, &serde_json::json!({"op":"state_dump"}))["state"]["chrome"]["session_locked"]
        .as_bool()
        .unwrap()
}

fn authenticate(stream: &mut UnixStream, password: &str) {
    rpc(stream, &serde_json::json!({"op":"type", "text":password}));
    rpc(stream, &serde_json::json!({"op":"key", "key":"Enter"}));
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let state = rpc(stream, &serde_json::json!({"op":"state_dump"}));
        let chrome = &state["state"]["chrome"];
        let verifying = chrome["lock_auth"]["verifying"]
            .as_bool()
            .unwrap_or_else(|| {
                panic!("state dump must expose lock authentication status: {state}")
            });
        if !verifying
            && (chrome["session_locked"] == false || chrome["lock_auth"]["error"].is_string())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "password verification did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn locked_restart_restores_before_the_first_control_response() {
    tilo_test_env::require_isolated_desktop();
    let runtime = format!("/run/user/{}", rustix::process::geteuid().as_raw());
    fs::create_dir_all(&runtime).unwrap();
    let directory = tempfile::Builder::new()
        .prefix("lr-")
        .tempdir_in(runtime)
        .unwrap();
    let root = directory.path();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    tilo_files::native::private::write_atomic(&root.join("lock-state"), b"unlocked\n").unwrap();
    let (mut session, mut stream) = Session::start(root, false);
    rpc(
        &mut stream,
        &serde_json::json!({"op":"actions_invoke", "provider":"session", "id":"lock"}),
    );
    let deadline = tilo_test_env::hang_deadline();
    while !locked(&mut stream) {
        assert!(Instant::now() < deadline, "lock action did not engage");
    }
    // Accept both an old compositor's default TERM exit and ordered teardown:
    // this case isolates the lock-state regression, including at its base.
    assert_eq!(
        unsafe { libc::kill(i32::try_from(session.0.id()).unwrap(), libc::SIGTERM) },
        0
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(status) = session.0.try_wait().unwrap() {
            assert!(status.success() || status.signal() == Some(libc::SIGTERM));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "TERM did not stop the old compositor"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let (_restarted, mut stream) = Session::start(root, false);
    assert!(
        locked(&mut stream),
        "locked login must stay locked across compositor restart"
    );
}

#[test]
fn locked_restart_and_crash_refuse_contents_until_verification() {
    tilo_test_env::require_isolated_desktop();
    let runtime = format!("/run/user/{}", rustix::process::geteuid().as_raw());
    fs::create_dir_all(&runtime).unwrap();
    let directory = tempfile::Builder::new()
        .prefix("lr-")
        .tempdir_in(runtime)
        .unwrap();
    let root = directory.path();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    tilo_files::native::private::write_atomic(&root.join("lock-state"), b"unlocked\n").unwrap();
    let (mut first, mut stream) = Session::start(root, false);
    assert!(
        !locked(&mut stream),
        "fresh authenticated login starts unlocked"
    );
    first.stop(libc::SIGTERM);
    let (mut second, mut stream) = Session::start(root, false);
    assert!(
        !locked(&mut stream),
        "ordered unlocked restart preserves the login"
    );
    rpc(
        &mut stream,
        &serde_json::json!({"op":"actions_invoke", "provider":"session", "id":"lock"}),
    );
    let deadline = tilo_test_env::hang_deadline();
    while !locked(&mut stream) {
        assert!(
            Instant::now() < deadline,
            "session lock action did not lock"
        );
    }
    second.stop(libc::SIGTERM);
    let (mut third, mut stream) = Session::start(root, false);
    assert!(
        locked(&mut stream),
        "locked state restored before the first control response"
    );
    let response = rpc(
        &mut stream,
        &serde_json::json!({"op":"snapshot", "path":root.join("forbidden.png")}),
    );
    assert_eq!(response["err"]["code"], "session_locked");
    assert!(
        !root.join("forbidden.png").exists(),
        "no contents were exported"
    );
    authenticate(&mut stream, "wrong");
    assert!(
        locked(&mut stream),
        "rejected verification cannot unlock recovery"
    );
    rpc(
        &mut stream,
        &serde_json::json!({"op":"key", "key":"Escape"}),
    );
    third.stop(libc::SIGKILL);
    let (mut unavailable, mut stream) = Session::start(root, true);
    assert!(
        locked(&mut stream),
        "crash and control reconnect remain locked"
    );
    authenticate(&mut stream, PASSWORD);
    assert!(locked(&mut stream), "failed verifier cannot admit access");
    unavailable.stop(libc::SIGTERM);
    let (mut recovered, mut stream) = Session::start(root, false);
    assert!(locked(&mut stream));
    authenticate(&mut stream, PASSWORD);
    assert!(
        !locked(&mut stream),
        "only the existing verifier admits access"
    );
    recovered.stop(libc::SIGKILL);
    let (mut crashed_unlocked, mut stream) = Session::start(root, false);
    assert!(
        locked(&mut stream),
        "an unlocked compositor crash must require verification on reconnect"
    );
    authenticate(&mut stream, PASSWORD);
    assert!(!locked(&mut stream));
    crashed_unlocked.stop(libc::SIGTERM);
}
