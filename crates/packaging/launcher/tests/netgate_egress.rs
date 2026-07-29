//! Packet-level proof of the egress mechanism, run against a real
//! `bwrap` sandbox.
//!
//! Per-host egress filtering rests on two kernel properties. Neither is
//! obvious, both are load-bearing, and asserting them here means a
//! kernel or a `bwrap` that stops providing them fails the build
//! instead of quietly turning the filter into a no-op:
//!
//! 1. **An unshared network namespace is genuinely empty.** With
//!    `--unshare-all` and no `--share-net`, a socket inside the sandbox
//!    has no route to anywhere. `connect(2)` to a public address fails
//!    with `ENETUNREACH` before a packet is built, and the loopback
//!    inside the sandbox is *its own* loopback — a server the host is
//!    running on `127.0.0.1` is not reachable at that address from
//!    inside. This is what makes the filter unbypassable: an app that
//!    ignores the egress path we give it does not get a fallback, it
//!    gets nothing.
//!
//! 2. **A bind-mounted `AF_UNIX` socket crosses the boundary anyway.**
//!    Pathname sockets are addressed through the filesystem, not
//!    through the network namespace, so one bind-mount punches exactly
//!    one hole through an otherwise-empty namespace — and the process
//!    on the far side of it is ours, in the host namespace, holding the
//!    policy.
//!
//! Together those two say: the sandbox's *only* egress is a channel
//! whose far end we control. That is the whole architecture. Everything
//! else — SOCKS5, hostname matching, resolution — is policy layered on
//! a channel these two properties make exclusive.
//!
//! ## How the in-sandbox side runs
//!
//! There is no separate probe binary to package. The test binary
//! re-executes *itself* inside the sandbox, selecting the single
//! `netns_probe` test by name and handing it the sandbox-side socket
//! path through the environment. The probe writes `PROBE <key>=<value>`
//! lines to stdout; the outer test asserts on those lines, so a probe
//! that silently did nothing fails the outer test rather than passing
//! it.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

/// Set by the outer test on the re-executed child; carries the
/// sandbox-side path of the bind-mounted gate socket.
const PROBE_SOCKET_ENV: &str = "QOS_NETGATE_PROBE_SOCKET";
/// Carries the host-side TCP port the outer test is listening on, so
/// the probe can demonstrate that the host's loopback is *not* the
/// sandbox's loopback.
const PROBE_PORT_ENV: &str = "QOS_NETGATE_PROBE_PORT";

/// Where the gate socket appears inside the sandbox. Short on purpose:
/// `sun_path` is 108 bytes and a socket bound at a long host path
/// cannot be connected to at all. The host side lives under
/// `$XDG_RUNTIME_DIR`, which is short; a `TempDir` under `/tmp` in a
/// harness-provided scratch directory is not, which is a real failure
/// mode worth pinning here rather than discovering in the field.
const SANDBOX_SOCKET_PATH: &str = "/run/qos-gate.sock";

/// The in-sandbox half. A no-op unless the outer test selected it and
/// handed it a socket path — running the suite normally executes this
/// as an empty test, and it asserts nothing in that case *by design*:
/// every assertion about what the probe found lives in the outer test,
/// which reads the probe's stdout. An empty run here therefore cannot
/// hide a regression, it can only fail the outer test.
#[test]
fn netns_probe() {
    let Ok(socket) = std::env::var(PROBE_SOCKET_ENV) else {
        return;
    };
    let host_port: u16 = std::env::var(PROBE_PORT_ENV)
        .expect("probe needs the host's port")
        .parse()
        .expect("port parses");

    // (1a) A public address, from inside an unshared namespace. There
    // is no default route, so this fails in the kernel's routing
    // lookup — no packet is emitted, nothing to filter, nothing to
    // leak. 198.51.100.0/24 is TEST-NET-2 (RFC 5737): it is not
    // routable anywhere, so a machine that *does* have a route would
    // still not reach it, and this assertion is about the local
    // routing table rather than about the test host's connectivity.
    let public = std::net::TcpStream::connect_timeout(
        &"198.51.100.9:80".parse().unwrap(),
        Duration::from_secs(2),
    );
    println!(
        "PROBE tcp_public={}",
        match &public {
            Ok(_) => "REACHED".to_string(),
            Err(e) => format!("{:?}", e.kind()),
        }
    );

    // (1b) The host's own loopback server, by address. Inside the
    // sandbox `127.0.0.1` names the sandbox's private loopback, where
    // nothing is listening — so a host service bound to loopback is
    // invisible even though the address is spellable.
    let loopback = std::net::TcpStream::connect_timeout(
        &format!("127.0.0.1:{host_port}").parse().unwrap(),
        Duration::from_secs(2),
    );
    println!(
        "PROBE tcp_loopback={}",
        match &loopback {
            Ok(_) => "REACHED".to_string(),
            Err(e) => format!("{:?}", e.kind()),
        }
    );

    // (2) The bind-mounted unix socket. Same empty namespace, and this
    // one reaches a process in the host's namespace.
    let unix = UnixStream::connect(&socket).and_then(|mut s| {
        s.write_all(b"PING")?;
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf)?;
        Ok(buf)
    });
    println!(
        "PROBE unix={}",
        match &unix {
            Ok(b) => String::from_utf8_lossy(b).to_string(),
            Err(e) => format!("ERR:{:?}", e.kind()),
        }
    );
}

#[test]
fn an_unshared_netns_has_no_route_but_a_bound_unix_socket_still_reaches_the_host() {
    let bwrap = which("bwrap").expect(
        "bwrap is a hard runtime dependency of the launcher — a host without it cannot run \
         a sandboxed app at all, so this is a failure and not a reason to skip",
    );

    // Host side of the one hole in the namespace. Under
    // $XDG_RUNTIME_DIR because `sun_path` is 108 bytes; see
    // SANDBOX_SOCKET_PATH.
    let dir = runtime_scratch_dir();
    let host_socket = dir.join("proof.sock");
    let _ = std::fs::remove_file(&host_socket);
    let listener = UnixListener::bind(&host_socket).expect("bind unix socket");
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4];
            if stream.read_exact(&mut buf).is_ok() && &buf == b"PING" {
                let _ = stream.write_all(b"PONG");
            }
        }
    });

    // A host TCP service on loopback, so the probe can try to reach it
    // by address and fail. Port 0 → the kernel picks a free one.
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").expect("bind tcp");
    let host_port = tcp.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = tcp.accept() {
            let _ = stream.write_all(b"HOST");
        }
    });

    let exe = std::env::current_exe().expect("current_exe");
    let output = std::process::Command::new(&bwrap)
        .args(["--unshare-all", "--die-with-parent"])
        .args(["--ro-bind", "/usr", "/usr"])
        .args(["--ro-bind-try", "/lib64", "/lib64"])
        .args(["--ro-bind-try", "/lib", "/lib"])
        .args(["--proc", "/proc"])
        .args(["--dev", "/dev"])
        .args(["--tmpfs", "/tmp"])
        .args(["--ro-bind", &exe.display().to_string(), &exe.display().to_string()])
        .args([
            "--bind",
            &host_socket.display().to_string(),
            SANDBOX_SOCKET_PATH,
        ])
        .arg(&exe)
        .args(["--exact", "netns_probe", "--nocapture", "--test-threads", "1"])
        .env(PROBE_SOCKET_ENV, SANDBOX_SOCKET_PATH)
        .env(PROBE_PORT_ENV, host_port.to_string())
        .output()
        .expect("spawn bwrap");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    // libtest writes its own `test <name> ... ` prefix onto the same
    // line as the first `println!`, so match the marker anywhere in
    // the line rather than only at its start.
    let probe = |key: &str| -> String {
        let marker = format!("PROBE {key}=");
        stdout
            .lines()
            .find_map(|l| l.split_once(&marker).map(|(_, v)| v.trim().to_string()))
            .unwrap_or_else(|| {
                panic!("probe did not report `{key}`.\nstdout:\n{stdout}\nstderr:\n{stderr}")
            })
    };

    // Property 1: the namespace is empty.
    assert_eq!(
        probe("tcp_public"),
        "NetworkUnreachable",
        "an unshared netns must have no route to a public address — if this reaches, every \
         filter built on namespace isolation is decorative.\nstdout:\n{stdout}"
    );
    assert_ne!(
        probe("tcp_loopback"),
        "REACHED",
        "the host's loopback must not be the sandbox's loopback — a host service bound to \
         127.0.0.1 is otherwise reachable from inside and the isolation is illusory.\
         \nstdout:\n{stdout}"
    );

    // Property 2: one bind-mounted unix socket still crosses it.
    assert_eq!(
        probe("unix"),
        "PONG",
        "a bind-mounted AF_UNIX socket must remain connectable from inside an unshared \
         network namespace — this is the single channel the egress policy sits on.\
         \nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// `$XDG_RUNTIME_DIR/qos-test`, falling back to `/run/user/<uid>`.
/// Deliberately not a `TempDir`: the harness' scratch root is long
/// enough to overflow `sun_path`.
fn runtime_scratch_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        // SAFETY: getuid always succeeds.
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() })));
    let dir = base.join("qos-netgate-test");
    std::fs::create_dir_all(&dir).expect("create runtime scratch dir");
    dir
}

fn which(bin: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}
