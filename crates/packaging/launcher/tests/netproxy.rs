//! A Windows program's filtered network as its launch starts it: the proxy in the launch's own
//! process tree, its policy, and the session's control over it.
//!
//! The proxy is the real `tilo-launcher windows-network` / `windows-network-serve`, and every
//! connection here is to a loopback listener in the test: nothing reaches the network.

#![cfg(target_os = "linux")]

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use launcher::egress_proxy::{EgressPolicy, reply};
use launcher::netproxy;
use tilo_test_env::SpawnUnderEnvLock as _;

const LAUNCHER: &str = env!("CARGO_BIN_EXE_tilo-launcher");

/// A private directory, as a keeper's is: the proxy refuses a control socket anywhere another
/// user could reach it.
fn private(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// A loopback server that answers one request and ends the connection, and its port.
fn answer_once() -> (TcpListener, u16) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    (listener, port)
}

/// Serve `body` on `listener` until the test ends.
fn serve(listener: TcpListener, body: &'static [u8]) {
    std::thread::spawn(move || {
        for mut client in listener.incoming().flatten() {
            let _ = client.write_all(body);
        }
    });
}

/// Start the proxy in `dir`, answering on `control`.
fn start_proxy(dir: &Path, control: &Path) -> Child {
    let child = Command::new(LAUNCHER)
        .arg("windows-network-serve")
        .arg("--dir")
        .arg(dir)
        .arg("--control")
        .arg(control)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("the proxy starts");
    wait_until("the proxy listens", || {
        dir.join("proxy.sock").exists() && control.exists()
    });
    child
}

/// Wait for `condition`, or fail the test naming `what`.
fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("a deadline");
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether `pid` is alive.
fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The proxy's pid: the child `parent` started.
fn proxy_pid(parent: &Child) -> u32 {
    let path = format!("/proc/{}/task/{}/children", parent.id(), parent.id());
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("a deadline");
    loop {
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Some(pid) = text.split_whitespace().next()
        {
            return pid.parse().expect("a pid");
        }
        assert!(Instant::now() < deadline, "the proxy did not start");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Ask the proxy at `socket` for `name`, and whether it let the connection through.
fn reach(socket: &Path, name: &str, port: u16) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    if stream.write_all(&[0x05, 0x01, 0x00]).is_err() {
        return false;
    }
    let mut ack = [0u8; 2];
    if stream.read_exact(&mut ack).is_err() {
        return false;
    }
    let Ok(len) = u8::try_from(name.len()) else {
        return false;
    };
    let mut request = vec![0x05, 0x01, 0x00, 0x03, len];
    request.extend_from_slice(name.as_bytes());
    request.extend_from_slice(&port.to_be_bytes());
    if stream.write_all(&request).is_err() {
        return false;
    }
    let mut head = [0u8; 4];
    if stream.read_exact(&mut head).is_err() {
        return false;
    }
    head[1] == reply::SUCCESS
}

#[test]
fn a_proxy_allows_what_its_policy_names_and_refuses_the_rest() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("netproxy");
    let control_dir = scratch.path().join("keeper");
    private(&control_dir);
    let control = control_dir.join("control");
    netproxy::prepare(&dir, &EgressPolicy::from_grants(["localhost"])).unwrap();

    let (listener, port) = answer_once();
    serve(listener, b"HTTP/1.0 200 OK\r\n\r\nallowed");
    let mut proxy = start_proxy(&dir, &control);
    let socket = dir.join("proxy.sock");
    assert!(
        reach(&socket, "localhost", port),
        "a name the policy names is reachable"
    );
    assert!(
        !reach(&socket, "blocked.invalid", port),
        "a name it does not name is refused"
    );
    proxy.kill().unwrap();
    proxy.wait().unwrap();
}

#[test]
fn setting_the_policy_cuts_a_connection_open_under_the_old_one() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("netproxy");
    let control_dir = scratch.path().join("keeper");
    private(&control_dir);
    let control = control_dir.join("control");
    netproxy::prepare(&dir, &EgressPolicy::from_grants(["localhost"])).unwrap();
    let (listener, port) = answer_once();
    // A tunnel that stays open, as a stream does.
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            std::thread::sleep(Duration::from_secs(60));
            drop(client);
        }
    });
    let mut proxy = start_proxy(&dir, &control);

    let mut stream = UnixStream::connect(dir.join("proxy.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(&[0x05, 0x01, 0x00]).unwrap();
    let mut ack = [0u8; 2];
    stream.read_exact(&mut ack).unwrap();
    let name = b"localhost";
    let mut request = vec![
        0x05,
        0x01,
        0x00,
        0x03,
        u8::try_from(name.len()).expect("a short name"),
    ];
    request.extend_from_slice(name);
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).unwrap();
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).unwrap();
    assert_eq!(head[1], reply::SUCCESS, "the tunnel opens");
    let rest = if head[3] == 0x01 { 6 } else { 18 };
    let mut drain = vec![0u8; rest];
    stream.read_exact(&mut drain).unwrap();

    // The person takes the grant back: the open tunnel ends rather than carrying the traffic it
    // allowed.
    netproxy::set_policy(&control, &EgressPolicy::none()).unwrap();
    let mut one = [0u8; 1];
    assert_eq!(
        stream.read(&mut one).unwrap_or(0),
        0,
        "the connection was cut"
    );
    // And a new connection is refused.
    assert!(!reach(&dir.join("proxy.sock"), "localhost", port));
    proxy.kill().unwrap();
    proxy.wait().unwrap();
}

#[test]
fn a_proxy_with_no_policy_file_refuses_to_start() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("netproxy");
    let control_dir = scratch.path().join("keeper");
    private(&control_dir);
    let status = Command::new(LAUNCHER)
        .arg("windows-network-serve")
        .arg("--dir")
        .arg(&dir)
        .arg("--control")
        .arg(control_dir.join("control"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status_locked()
        .unwrap();
    assert!(
        !status.success(),
        "a proxy that cannot read its policy starts nothing"
    );
}

#[test]
fn a_proxy_ends_with_the_program_its_launch_starts_and_leaves_no_way_out() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("netproxy");
    let control_dir = scratch.path().join("keeper");
    private(&control_dir);
    let control = control_dir.join("control");
    netproxy::prepare(&dir, &EgressPolicy::from_grants(["localhost"])).unwrap();
    let (listener, port) = answer_once();
    serve(listener, b"HTTP/1.0 200 OK\r\n\r\nallowed");

    // The program the launch becomes: the proxy is its child.
    let mut program = Command::new(LAUNCHER)
        .arg("windows-network")
        .arg("--dir")
        .arg(&dir)
        .arg("--control")
        .arg(&control)
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg("exec sleep 60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .unwrap();
    wait_until("the proxy listens", || {
        dir.join("proxy.sock").exists() && control.exists()
    });
    let pid = proxy_pid(&program);
    assert!(alive(pid), "the proxy lives beside the program");
    assert!(reach(&dir.join("proxy.sock"), "localhost", port));

    // The program ends: its proxy ends with it.
    program.kill().unwrap();
    program.wait().unwrap();
    wait_until("the proxy ends with the program", || !alive(pid));
    assert!(
        !reach(&dir.join("proxy.sock"), "localhost", port),
        "no proxy, no route out: the program is offline, not unfiltered"
    );
}

#[test]
fn a_killed_proxy_leaves_the_program_without_a_network() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("netproxy");
    let control_dir = scratch.path().join("keeper");
    private(&control_dir);
    let control = control_dir.join("control");
    netproxy::prepare(&dir, &EgressPolicy::from_grants(["localhost"])).unwrap();
    let (listener, port) = answer_once();
    serve(listener, b"HTTP/1.0 200 OK\r\n\r\nallowed");
    let mut proxy = start_proxy(&dir, &control);
    assert!(reach(&dir.join("proxy.sock"), "localhost", port));

    // A dead listener behind the stale socket refuses, rather than letting anything through.
    proxy.kill().unwrap();
    proxy.wait().unwrap();
    wait_until("the listener is gone", || {
        UnixStream::connect(dir.join("proxy.sock")).is_err()
    });
    assert!(!reach(&dir.join("proxy.sock"), "localhost", port));
}

#[test]
fn a_program_starts_only_after_its_proxy_listens() {
    let scratch = tempfile::tempdir().unwrap();
    let dir = scratch.path().join("netproxy");
    let control_dir = scratch.path().join("keeper");
    private(&control_dir);
    let control = control_dir.join("control");
    // No policy was prepared: the launch must not start the program unfiltered.
    let status = Command::new(LAUNCHER)
        .arg("windows-network")
        .arg("--dir")
        .arg(&dir)
        .arg("--control")
        .arg(&control)
        .arg("--")
        .arg("/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status_locked()
        .unwrap();
    assert!(
        !status.success(),
        "a launch whose proxy cannot start is refused, never started unfiltered"
    );
    assert!(!dir.join("proxy.sock").exists());
}

#[test]
fn set_policy_refuses_an_oversized_answer_and_reads_an_exact_bound_answer() {
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::net::UnixListener;

    const BOUND: usize = netproxy::MAX_CONTROL_LINE_BYTES;
    let scratch = tempfile::tempdir().unwrap();
    let control_dir = scratch.path().join("keeper");
    private(&control_dir);
    let control = control_dir.join("control");
    let listener = UnixListener::bind(&control).unwrap();

    // 1) Oversized valid JSON answer (bound + 1) with no newline, then close:
    //    succeeds on main, refused with InvalidData after fix.
    let ok_json = r#"{"allowed":1,"error":null}"#;
    let mut over_wire = ok_json.as_bytes().to_vec();
    over_wire.resize(BOUND + 1, b' ');

    // 2) Exact-bound valid JSON Ok answer (bound bytes + '\n'): still succeeds.
    let mut exact_ok_wire = ok_json.as_bytes().to_vec();
    exact_ok_wire.resize(BOUND, b' ');
    exact_ok_wire.push(b'\n');

    // 3) Exact-bound valid JSON Refused answer (bound bytes + '\n'): error text decoded intact.
    let prefix = "refused:";
    let shell = format!(r#"{{"allowed":null,"error":"{prefix}"}}"#);
    let pad = BOUND - shell.len();
    let expected_error = format!("{prefix}{}", "e".repeat(pad));
    let mut exact_err_wire =
        format!(r#"{{"allowed":null,"error":"{expected_error}"}}"#).into_bytes();
    assert_eq!(exact_err_wire.len(), BOUND);
    exact_err_wire.push(b'\n');

    let server = std::thread::spawn(move || {
        for reply in [over_wire, exact_ok_wire, exact_err_wire] {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            let _ = (&stream).write_all(&reply);
            drop(stream);
        }
    });

    let err = netproxy::set_policy(&control, &EgressPolicy::none())
        .expect_err("an oversized control answer must be refused");
    match err {
        netproxy::NetworkProxyError::Control { source, .. } => {
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData, "{source}");
        }
        other => panic!("expected Control(InvalidData), got {other:?}"),
    }

    netproxy::set_policy(&control, &EgressPolicy::none())
        .expect("an exact-bound ok answer must succeed");

    let err = netproxy::set_policy(&control, &EgressPolicy::none())
        .expect_err("an exact-bound refusal must be decoded");
    match err {
        netproxy::NetworkProxyError::Refused(got) => assert_eq!(got, expected_error),
        other => panic!("expected Refused, got {other:?}"),
    }

    server.join().unwrap();
}
