//! Packet-level tests for per-host egress filtering in a `bwrap` sandbox.
//!
//! These tests assert on **connection outcomes from inside the
//! sandbox** — bytes that arrived, or a refusal code — never the argv
//! that built it: argv only describes the requested command, while these
//! outcomes show whether a packet could leave the sandbox.
//!
//! ## The two kernel properties underneath
//!
//! Pinned by
//! [`without_a_gate_the_sandbox_has_no_network_and_fails_closed`] (phase
//! `an_unshared_netns_has_no_route_but_a_bound_unix_socket_still_reaches_the_host`)
//! so a kernel or `bwrap` regression fails the build instead of quietly
//! turning the filter into a no-op: (1) an unshared network namespace
//! is genuinely empty — `connect(2)` to a public address fails
//! `ENETUNREACH` before a packet is built, and the sandbox's loopback
//! is *its own*; (2) a bind-mounted `AF_UNIX` socket crosses the
//! boundary anyway, addressed through the filesystem — one bind-mount
//! is exactly one hole, and the process on its far side is ours.
//!
//! ## Where the sandbox shape comes from
//!
//! The tests build a sandbox around a probe rather than launching a
//! packaged app, but every security-relevant decision is made by the
//! **production** functions: `EgressDecision::resolve`, `push_net_gate`,
//! `NetGate`, `run_relay`.
//!
//! ## How the in-sandbox side runs
//!
//! There is no separate probe binary. The test binary re-executes
//! *itself* inside the sandbox, selecting one probe test by name, and
//! writes `PROBE <key>=<value>` lines to stdout that the outer tests
//! assert on, so a probe that silently did nothing fails the outer
//! assertions rather than passing them.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use launcher::netgate::{EgressDecision, EgressPolicy, NetGate, reply};
use launcher::sandbox::NetGateBind;

/// Sandbox-side path of the bind-mounted gate socket. Absent when the
/// launch has no gate.
const ENV_SOCKET: &str = "TILO_NETGATE_PROBE_SOCKET";
/// Port of the server the probe should be able to reach.
const ENV_ALLOWED_PORT: &str = "TILO_NETGATE_PROBE_ALLOWED_PORT";
/// Port of a second server, for the raw-TCP reachability checks.
const ENV_OTHER_PORT: &str = "TILO_NETGATE_PROBE_OTHER_PORT";
/// Set when the probe should also exercise the loopback relay.
const ENV_RELAY: &str = "TILO_NETGATE_PROBE_RELAY";

/// Where the gate socket appears inside the sandbox. Short on purpose:
/// `sun_path` is 108 bytes.
const SANDBOX_SOCKET_PATH: &str = "/run/tilo-gate.sock";

/// Loopback port for the relay inside the sandbox.
const PROBE_RELAY_PORT: u16 = 19050;

/// The name the allowlist is written against in these tests. `localhost`
/// resolves on every host, so the gate's resolution step runs.
const ALLOWED_NAME: &str = "localhost";

/// RFC 2606 reserves `.invalid` as never-resolvable, so a refusal for
/// this name can only be the policy.
const BLOCKED_NAME: &str = "blocked.invalid";

/// The in-sandbox half. A no-op unless the outer test selected it —
/// assertions live in the outer tests, which read its stdout.
#[test]
fn netns_probe() {
    let Ok(allowed_port) = std::env::var(ENV_ALLOWED_PORT) else {
        return;
    };
    let allowed_port: u16 = allowed_port.parse().expect("port parses");
    let other_port: u16 = std::env::var(ENV_OTHER_PORT)
        .expect("probe needs the second port")
        .parse()
        .expect("port parses");

    // Raw TCP, no gate involved: under an unshared namespace these hit the
    // sandbox's own empty loopback; under `--share-net` they reach the servers.
    println!("PROBE raw_allowed={}", describe(raw_get(allowed_port)));
    println!("PROBE raw_other={}", describe(raw_get(other_port)));
    println!(
        "PROBE raw_public={}",
        match TcpStream::connect_timeout(
            &"198.51.100.9:80".parse().unwrap(),
            Duration::from_secs(2)
        ) {
            Ok(_) => "REACHED".to_string(),
            Err(e) => format!("{:?}", e.kind()),
        }
    );

    let Ok(socket) = std::env::var(ENV_SOCKET) else {
        return;
    };

    // Through the gate, by name: the allowlisted host.
    println!(
        "PROBE gate_by_name={}",
        describe(socks_get(&socket, ALLOWED_NAME, allowed_port))
    );

    // Through the gate, by address literal: the SAME server. Identical
    // reachability, so a different outcome can only be the policy.
    println!(
        "PROBE gate_by_literal={}",
        describe(socks_get(&socket, "127.0.0.1", allowed_port))
    );

    // Through the gate, a name that is not on the list.
    println!(
        "PROBE gate_by_other_name={}",
        describe(socks_get(&socket, BLOCKED_NAME, 80))
    );

    // The same three, over HTTP CONNECT rather than SOCKS5 — the path a
    // client with no SOCKS support takes, and the one most apps use.
    println!(
        "PROBE http_by_name={}",
        describe(http_connect(&socket, ALLOWED_NAME, allowed_port))
    );
    println!(
        "PROBE http_by_literal={}",
        describe(http_connect(&socket, "127.0.0.1", allowed_port))
    );
    println!(
        "PROBE http_by_other_name={}",
        describe(http_connect(&socket, BLOCKED_NAME, 80))
    );

    // The loopback relay — the path an unmodified HTTP client takes via
    // ALL_PROXY. Uses the production relay; the listener is bound before
    // the relay starts, so the first connect is already queued for it.
    if std::env::var(ENV_RELAY).is_ok() {
        let gate = PathBuf::from(&socket);
        let listener =
            TcpListener::bind(("127.0.0.1", PROBE_RELAY_PORT)).expect("bind the relay port");
        std::thread::spawn(move || {
            let _ = launcher::netgate::serve_relay(listener, &gate);
        });
        println!(
            "PROBE relay_by_name={}",
            describe(socks_get_tcp(PROBE_RELAY_PORT, ALLOWED_NAME, allowed_port))
        );
        println!(
            "PROBE relay_by_literal={}",
            describe(socks_get_tcp(PROBE_RELAY_PORT, "127.0.0.1", allowed_port))
        );
    }
}

/// Companion probe for the kernel-property test. Same dispatcher shape
/// as [`netns_probe`].
#[test]
fn proof_probe() {
    let Ok(socket) = std::env::var("TILO_NETGATE_PROOF_SOCKET") else {
        return;
    };
    let port: u16 = std::env::var("TILO_NETGATE_PROOF_PORT")
        .expect("port")
        .parse()
        .expect("parses");

    let public =
        TcpStream::connect_timeout(&"198.51.100.9:80".parse().unwrap(), Duration::from_secs(2));
    println!(
        "PROBE tcp_public={}",
        match &public {
            Ok(_) => "REACHED".to_string(),
            Err(e) => format!("{:?}", e.kind()),
        }
    );
    let loopback = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(2),
    );
    println!(
        "PROBE tcp_loopback={}",
        match &loopback {
            Ok(_) => "REACHED".to_string(),
            Err(e) => format!("{:?}", e.kind()),
        }
    );
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

/// Outcome of one attempt, in the form the outer tests match on.
enum Outcome {
    /// The HTTP body that came back.
    Body(String),
    /// The gate refused with this SOCKS5 reply code.
    Refused(u8),
    /// The gate refused with this HTTP status code.
    HttpStatus(u16),
    /// Something else went wrong.
    Failed(String),
}

fn describe(o: Outcome) -> String {
    match o {
        Outcome::Body(b) => format!("BODY:{b}"),
        Outcome::Refused(code) => format!("REFUSED:{code:#04x}"),
        Outcome::HttpStatus(code) => format!("STATUS:{code}"),
        Outcome::Failed(e) => format!("ERR:{e}"),
    }
}

/// HTTP `CONNECT` through the gate, then an HTTP request down the
/// tunnel — what an ordinary client does with `HTTPS_PROXY` set.
fn http_connect(socket: &str, host: &str, port: u16) -> Outcome {
    let mut s = match UnixStream::connect(socket) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(format!("connect gate: {:?}", e.kind())),
    };
    if s.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .is_err()
    {
        return Outcome::Failed("connect write".into());
    }
    // Read the proxy's response head.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match s.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
                if head.len() > 4096 {
                    return Outcome::Failed("proxy head too large".into());
                }
            }
            Err(e) => return Outcome::Failed(format!("proxy head read: {:?}", e.kind())),
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let status: u16 = match text.split_whitespace().nth(1).and_then(|c| c.parse().ok()) {
        Some(c) => c,
        None => return Outcome::Failed(format!("no status in {text:?}")),
    };
    if status != 200 {
        return Outcome::HttpStatus(status);
    }
    http_exchange(&mut s)
}

fn raw_get(port: u16) -> Outcome {
    match TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().unwrap(),
        Duration::from_secs(2),
    ) {
        Ok(mut s) => http_exchange(&mut s),
        Err(e) => Outcome::Failed(format!("{:?}", e.kind())),
    }
}

/// SOCKS5 CONNECT over the gate's unix socket, then an HTTP request.
fn socks_get(socket: &str, host: &str, port: u16) -> Outcome {
    match UnixStream::connect(socket) {
        Ok(mut s) => socks_exchange(&mut s, host, port),
        Err(e) => Outcome::Failed(format!("connect gate: {:?}", e.kind())),
    }
}

/// Same, but over the loopback relay.
fn socks_get_tcp(relay_port: u16, host: &str, port: u16) -> Outcome {
    match TcpStream::connect(("127.0.0.1", relay_port)) {
        Ok(mut s) => socks_exchange(&mut s, host, port),
        Err(e) => Outcome::Failed(format!("connect relay: {:?}", e.kind())),
    }
}

fn socks_exchange<S: Read + Write>(s: &mut S, host: &str, port: u16) -> Outcome {
    // Greeting: SOCKS5, one method, "no authentication".
    if s.write_all(&[0x05, 0x01, 0x00]).is_err() {
        return Outcome::Failed("greeting write".into());
    }
    let mut ack = [0u8; 2];
    if s.read_exact(&mut ack).is_err() {
        return Outcome::Failed("greeting read".into());
    }

    // CONNECT, by domain name — including for an address literal, so the
    // gate sees exactly what the app asked for.
    let Ok(host_len) = u8::try_from(host.len()) else {
        return Outcome::Failed("host name too long for SOCKS5".into());
    };
    let mut req = vec![0x05, 0x01, 0x00, 0x03, host_len];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    if s.write_all(&req).is_err() {
        return Outcome::Failed("request write".into());
    }

    let mut head = [0u8; 4];
    if s.read_exact(&mut head).is_err() {
        return Outcome::Failed("reply read".into());
    }
    if head[1] != reply::SUCCESS {
        return Outcome::Refused(head[1]);
    }
    // Drain the bound-address field of a successful reply.
    let rest = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        _ => return Outcome::Failed("unexpected reply address type".into()),
    };
    let mut drain = vec![0u8; rest];
    if s.read_exact(&mut drain).is_err() {
        return Outcome::Failed("reply drain".into());
    }
    http_exchange(s)
}

fn http_exchange<S: Read + Write>(s: &mut S) -> Outcome {
    if s.write_all(b"GET / HTTP/1.0\r\n\r\n").is_err() {
        return Outcome::Failed("http write".into());
    }
    let mut buf = Vec::new();
    if s.read_to_end(&mut buf).is_err() {
        return Outcome::Failed("http read".into());
    }
    let text = String::from_utf8_lossy(&buf);
    match text.split("\r\n\r\n").nth(1) {
        Some(body) if !body.is_empty() => Outcome::Body(body.trim().to_string()),
        _ => Outcome::Failed(format!("no body in {text:?}")),
    }
}

/// A tiny HTTP server on loopback answering every request with `body`.
/// Returns the port it bound.
fn serve(body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind http");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            std::thread::spawn(move || {
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            });
        }
    });
    port
}

/// A short, process-private test directory. This deliberately ignores
/// `XDG_RUNTIME_DIR`: other launcher tests temporarily override that global
/// environment variable, and libtest runs modules in this binary in parallel.
/// Reading it here made those tests delete the netgate's live socket directory.
/// The PID keeps concurrent Cargo invocations apart while remaining well below
/// `sun_path`'s 108-byte limit.
fn runtime_scratch_dir() -> PathBuf {
    let dir = PathBuf::from(format!(
        "/tmp/tilo-netgate-{}-{}",
        unsafe { libc::getuid() },
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create runtime scratch dir");
    dir
}

fn which(bin: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

fn bwrap() -> PathBuf {
    which("bwrap").expect(
        "bwrap is a hard runtime dependency of the launcher — a host without it cannot run a \
         sandboxed app at all, so this is a failure and not a reason to skip",
    )
}

/// What one probe run reported.
struct ProbeReport {
    stdout: String,
    stderr: String,
}

impl ProbeReport {
    /// Echo everything the probe observed, tagged with the scenario — the
    /// packet-level record under `--nocapture`.
    fn dump(&self, scenario: &str) -> &Self {
        for line in self.stdout.lines() {
            if let Some(idx) = line.find("PROBE ") {
                println!("[{scenario}] {}", &line[idx..]);
            }
        }
        self
    }

    fn get(&self, key: &str) -> String {
        // libtest writes its own `test <name> ... ` prefix on the same line as
        // the first `println!`, so match the marker anywhere in the line.
        let marker = format!("PROBE {key}=");
        self.stdout
            .lines()
            .find_map(|l| l.split_once(&marker).map(|(_, v)| v.trim().to_string()))
            .unwrap_or_else(|| {
                panic!(
                    "probe did not report `{key}`.\nstdout:\n{}\nstderr:\n{}",
                    self.stdout, self.stderr
                )
            })
    }
}

/// The name `--exact` needs to select one of this file's probe tests
/// when the binary re-executes itself, since this file is a module of
/// `tests/it.rs` not its own binary and `module_path!` is one segment
/// too long for libtest. Derived rather than written out so a stale
/// name can't silently leave the probe running nothing.
fn probe_test_name(probe: &str) -> String {
    let module = module_path!();
    let relative = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{relative}::{probe}")
}

/// Run the probe inside a sandbox whose network shape is decided by the
/// production [`EgressDecision`].
fn run_probe(
    decision: &EgressDecision,
    gate_socket: Option<&Path>,
    allowed_port: u16,
    other_port: u16,
    with_relay: bool,
) -> ProbeReport {
    let exe = std::env::current_exe().expect("current_exe");
    let exe_s = exe.display().to_string();

    let mut argv: Vec<String> = vec!["--unshare-all".into(), "--die-with-parent".into()];
    // THE decision under test, taken from production code.
    if decision.shares_host_network() {
        argv.push("--share-net".into());
    }
    for (flag, a, b) in [
        ("--ro-bind", "/usr", "/usr"),
        ("--ro-bind-try", "/lib64", "/lib64"),
        ("--ro-bind-try", "/lib", "/lib"),
        ("--ro-bind", exe_s.as_str(), exe_s.as_str()),
    ] {
        argv.extend([flag.to_string(), a.to_string(), b.to_string()]);
    }
    argv.extend(["--proc".into(), "/proc".into()]);
    argv.extend(["--dev".into(), "/dev".into()]);
    argv.extend(["--tmpfs".into(), "/tmp".into()]);

    // The gate bind + proxy environment, written by production code.
    if let Some(host_socket) = gate_socket {
        let bind = NetGateBind {
            host_socket: host_socket.to_path_buf(),
            sandbox_socket: PathBuf::from(SANDBOX_SOCKET_PATH),
            port: PROBE_RELAY_PORT,
        };
        launcher::sandbox::push_net_gate(&mut argv, &bind);
    }

    argv.push(exe_s);
    argv.extend([
        "--exact".into(),
        probe_test_name("netns_probe"),
        "--nocapture".into(),
        "--test-threads".into(),
        "1".into(),
    ]);

    let mut cmd = std::process::Command::new(bwrap());
    cmd.args(&argv)
        .env(ENV_ALLOWED_PORT, allowed_port.to_string())
        .env(ENV_OTHER_PORT, other_port.to_string());
    if gate_socket.is_some() {
        cmd.env(ENV_SOCKET, SANDBOX_SOCKET_PATH);
    } else {
        cmd.env_remove(ENV_SOCKET);
    }
    if with_relay {
        cmd.env(ENV_RELAY, "1");
    } else {
        cmd.env_remove(ENV_RELAY);
    }

    let output = cmd.output().expect("spawn bwrap");
    report_from(&output)
}

/// Read the child's output, refusing one that selected no test —
/// libtest exits 0 when its filter matches nothing, so a stale probe
/// name looks like a probe that ran and found nothing to say.
fn report_from(output: &std::process::Output) -> ProbeReport {
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        stdout.contains("PROBE "),
        "the child reported nothing. If it says `running 0 tests`, the probe's \
         name no longer selects it — see `probe_test_name`.\nstdout:\n{stdout}\n\
         stderr:\n{stderr}"
    );
    ProbeReport { stdout, stderr }
}

/// Start a gate on a unique per-test socket path.
fn start_gate(name: &str, policy: EgressPolicy) -> (NetGate, PathBuf) {
    let path = runtime_scratch_dir().join(format!("{name}.sock"));
    let gate = NetGate::start(&path, policy).expect("gate starts");
    (gate, path)
}

#[test]
fn without_a_gate_the_sandbox_has_no_network_and_fails_closed() {
    // Phase `an_unshared_netns_has_no_route_but_a_bound_unix_socket_still_reaches_the_host`.
    {
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

        let exe = std::env::current_exe().expect("current_exe");
        let exe_s = exe.display().to_string();
        let tcp = TcpListener::bind("127.0.0.1:0").expect("bind tcp");
        let host_port = tcp.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = tcp.accept() {
                let _ = stream.write_all(b"HOST");
            }
        });

        let output = std::process::Command::new(bwrap())
            .args(["--unshare-all", "--die-with-parent"])
            .args(["--ro-bind", "/usr", "/usr"])
            .args(["--ro-bind-try", "/lib64", "/lib64"])
            .args(["--ro-bind-try", "/lib", "/lib"])
            .args(["--proc", "/proc"])
            .args(["--dev", "/dev"])
            .args(["--tmpfs", "/tmp"])
            .args(["--ro-bind", &exe_s, &exe_s])
            .args([
                "--bind",
                &host_socket.display().to_string(),
                SANDBOX_SOCKET_PATH,
            ])
            .arg(&exe)
            .args([
                "--exact",
                &probe_test_name("proof_probe"),
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env("TILO_NETGATE_PROOF_SOCKET", SANDBOX_SOCKET_PATH)
            .env("TILO_NETGATE_PROOF_PORT", host_port.to_string())
            .output()
            .expect("spawn bwrap");

        let report = ProbeReport {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        };

        assert_eq!(
            report.get("tcp_public"),
            "NetworkUnreachable",
            "an unshared netns must have no route to a public address — if this reaches, every \
             filter built on namespace isolation is decorative"
        );
        assert_ne!(
            report.get("tcp_loopback"),
            "REACHED",
            "the host's loopback must not be the sandbox's loopback — a host service bound to \
             127.0.0.1 would otherwise be reachable from inside and the isolation illusory"
        );
        assert_eq!(
            report.get("unix"),
            "PONG",
            "a bind-mounted AF_UNIX socket must remain connectable from inside an unshared \
             network namespace — this is the single channel the egress policy sits on"
        );
    }
    // Phase `a_gate_that_cannot_start_refuses_rather_than_sharing_the_network`.
    // (5) The degradation path: the gate needs no privilege or kernel
    // feature `bwrap` doesn't already require, so what's simulated here
    // is a failed socket bind. The launcher refuses rather than falling
    // back to `--share-net`, which would apply a grant naming one host as
    // "every host".
    {
        let policy = EgressPolicy::from_grants([ALLOWED_NAME]);
        // `/proc` is not writable, so creating the parent directory fails
        // before any bind is attempted.
        let impossible = PathBuf::from("/proc/tilo-netgate-nope/gate.sock");
        let err = NetGate::start(&impossible, policy).expect_err("binding here must fail");

        let rendered = err.to_string();
        assert!(
            rendered.contains("gate socket"),
            "the error must name the gate so an operator can act on it: {rendered}"
        );

        // And the launch-level consequence: a refusal that says what was
        // granted, and how to deliberately choose the unfiltered path.
        let run_err = launcher::run::RunError::EgressGate {
            hosts: vec![ALLOWED_NAME.to_string()],
            source: err,
        };
        let msg = run_err.to_string();
        assert!(
            msg.contains("refusing to launch"),
            "the launch must be refused, not downgraded: {msg}"
        );
        assert!(
            msg.contains(launcher::run::NET_GATE_ENV),
            "the refusal must name the explicit opt-out so the fallback is a decision on the \
             record rather than a default: {msg}"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn grants_decide_which_hosts_the_sandbox_reaches() {
    // Phase `a_grant_naming_one_host_reaches_that_host_and_no_other`.
    // (1) An app granted one host reaches it, and reaches nothing else.
    {
        let allowed_port = serve("ALLOWED-SERVER");
        let other_port = serve("OTHER-SERVER");

        let decision = EgressDecision::resolve("off", &[], &[ALLOWED_NAME.to_string()]);
        let policy = decision.policy().expect("a named host is filtered").clone();
        let (gate, socket) = start_gate("one-host", policy);

        let report = run_probe(&decision, Some(&socket), allowed_port, other_port, true);
        report.dump("granted: localhost");

        // Reaches the allowlisted host, end to end, by name.
        assert_eq!(
            report.get("gate_by_name"),
            "BODY:ALLOWED-SERVER",
            "a granted host must actually be reachable — a filter that denies everything is not \
             enforcement, it is breakage"
        );
        // And via the relay, the path an unmodified HTTP client takes
        // through ALL_PROXY.
        assert_eq!(
            report.get("relay_by_name"),
            "BODY:ALLOWED-SERVER",
            "the loopback relay must carry the same traffic the direct socket does"
        );

        // Does NOT reach the same server addressed as a literal — identical
        // reachability, so a different outcome can only be the policy.
        assert_eq!(
            report.get("gate_by_literal"),
            format!("REFUSED:{:#04x}", reply::HOST_UNREACHABLE),
            "an address literal must not satisfy a hostname grant, even when it is the very \
             server the grant names — otherwise anyone who controls a DNS answer turns a \
             name-scoped grant into an any-host grant"
        );
        assert_eq!(
            report.get("relay_by_literal"),
            format!("REFUSED:{:#04x}", reply::HOST_UNREACHABLE),
            "the relay must not be a way around the policy the direct socket enforces"
        );

        // Does not reach an unlisted name; the refusal is `HOST_UNREACHABLE`,
        // the same as an unresolvable host, a state the app already handles.
        assert_eq!(
            report.get("gate_by_other_name"),
            format!("REFUSED:{:#04x}", reply::HOST_UNREACHABLE),
            "an unlisted host must be refused as unreachable, not as forbidden"
        );

        // The same three answers over HTTP CONNECT — the path a client with no
        // SOCKS support takes, so the policy must hold there too.
        assert_eq!(
            report.get("http_by_name"),
            "BODY:ALLOWED-SERVER",
            "HTTP CONNECT must reach a granted host — it is the only proxy protocol an \
             unmodified HTTP client speaks without an opt-in feature"
        );
        assert_eq!(
            report.get("http_by_literal"),
            "STATUS:502",
            "the literal must be refused over HTTP too, or the SOCKS-side rule is bypassable by \
             switching protocol"
        );
        assert_eq!(
            report.get("http_by_other_name"),
            "STATUS:502",
            "an unlisted host must be refused over HTTP too, and as a gateway that could not \
             reach it rather than as a forbidden request"
        );

        // No route beside the gate.
        assert_eq!(
            report.get("raw_public"),
            "NetworkUnreachable",
            "the namespace must stay empty — the gate is the only way out"
        );
        assert!(
            report.get("raw_allowed").starts_with("ERR:"),
            "even the allowed server must be unreachable by raw TCP: {}",
            report.get("raw_allowed")
        );

        // The gate itself agrees about what it did.
        assert!(
            gate.stats().allowed() >= 3,
            "gate should have allowed the by-name connections on both protocols, saw {}",
            gate.stats().allowed()
        );
        assert!(
            gate.stats().denied() >= 5,
            "gate should have denied both literals and the unlisted name over both protocols, saw {}",
            gate.stats().denied()
        );
    }
    // Phase `a_grant_of_nothing_reaches_nothing`.
    // (2) An app granted nothing reaches neither host.
    {
        let allowed_port = serve("ALLOWED-SERVER");
        let other_port = serve("OTHER-SERVER");

        let decision = EgressDecision::resolve("off", &[], &[]);
        assert_eq!(
            decision,
            EgressDecision::Blocked,
            "no manifest network and no grant must resolve to blocked"
        );

        // Blocked means no gate is started at all, so there is no socket to
        // bind into the sandbox.
        let report = run_probe(&decision, None, allowed_port, other_port, false);
        report.dump("granted: nothing");

        for key in ["raw_allowed", "raw_other"] {
            assert!(
                report.get(key).starts_with("ERR:"),
                "an app granted nothing must reach no server: {key} = {}",
                report.get(key)
            );
        }
        assert_eq!(report.get("raw_public"), "NetworkUnreachable");
    }
    // Phase `a_grant_of_star_reaches_both`.
    // (3) An app granted `*` reaches both.
    {
        let allowed_port = serve("ALLOWED-SERVER");
        let other_port = serve("OTHER-SERVER");

        let decision = EgressDecision::resolve("off", &[], &["*".to_string()]);
        assert_eq!(
            decision,
            EgressDecision::Unrestricted,
            "a grant for `*` is unrestricted and needs no gate"
        );

        let report = run_probe(&decision, None, allowed_port, other_port, false);
        report.dump("granted: *");

        assert_eq!(
            report.get("raw_allowed"),
            "BODY:ALLOWED-SERVER",
            "`*` shares the host network, so both servers are reachable directly"
        );
        assert_eq!(report.get("raw_other"), "BODY:OTHER-SERVER");
    }
    // Phase `widening_the_allowlist_lets_the_refused_connection_through`.
    // (4) The negative case is not vacuous: same sandbox, same servers,
    // one variable changed (the allowlist). The connection refused in
    // the `a_grant_naming_one_host_reaches_that_host_and_no_other` phase now
    // carries traffic.
    {
        let allowed_port = serve("ALLOWED-SERVER");
        let other_port = serve("OTHER-SERVER");

        // Control: the exact request the strict policy refused.
        let strict = EgressDecision::resolve("off", &[], &[ALLOWED_NAME.to_string()]);
        let (strict_gate, strict_socket) =
            start_gate("breakage-strict", strict.policy().unwrap().clone());
        let refused = run_probe(
            &strict,
            Some(&strict_socket),
            allowed_port,
            other_port,
            false,
        );
        refused.dump("control: filter on");
        assert_eq!(
            refused.get("gate_by_literal"),
            format!("REFUSED:{:#04x}", reply::HOST_UNREACHABLE)
        );
        // Exactly the two by-name requests plus the four naming the
        // literal/unlisted host — the control run (relay disabled) accounts
        // for every connection.
        assert_eq!(
            strict_gate.stats().allowed(),
            2,
            "only the by-name requests (SOCKS5 and HTTP CONNECT) should have been allowed"
        );
        assert_eq!(
            strict_gate.stats().denied(),
            4,
            "the literal and the unlisted name should have been denied on both protocols"
        );

        // Filter disabled: allowlist permits everything, nothing else changed.
        // Spelled `Filtered(["*"])`, not `Unrestricted`, which would take the
        // `--share-net` path too.
        let (open_gate, open_socket) = start_gate("breakage-open", EgressPolicy::Any);
        let open_decision = EgressDecision::Filtered(EgressPolicy::Any);
        let passed = run_probe(
            &open_decision,
            Some(&open_socket),
            allowed_port,
            other_port,
            false,
        );

        passed.dump("breakage: filter off");
        assert_eq!(
            passed.get("gate_by_literal"),
            "BODY:ALLOWED-SERVER",
            "with the filter open, the previously-refused connection must carry traffic — if it \
             still failed, the negative assertion above was proving nothing"
        );
        // A refused host and an unreachable one answer identically — the app
        // must not tell a policy decision from an absence of network.
        assert_eq!(
            passed.get("gate_by_other_name"),
            format!("REFUSED:{:#04x}", reply::HOST_UNREACHABLE),
            "an unresolvable name is unreachable, the same answer a refused one gets"
        );
        // Same experiment on the HTTP path: the refusal (502) becomes 200 for
        // the reachable host and stays 502 for the unresolvable one.
        assert_eq!(
            passed.get("http_by_literal"),
            "BODY:ALLOWED-SERVER",
            "the HTTP refusal must also have been the policy: {}",
            passed.get("http_by_literal")
        );
        assert_eq!(
            passed.get("http_by_other_name"),
            "STATUS:502",
            "and an unresolvable host stays a bad gateway whether or not the policy would have \
             allowed it"
        );
        assert!(
            open_gate.stats().allowed() > strict_gate.stats().allowed(),
            "the open gate must have allowed strictly more than the strict one: {} vs {}",
            open_gate.stats().allowed(),
            strict_gate.stats().allowed()
        );
    }
}

// Device-local is network: Tilo's own services (e.g. the collaboration
// broker) are reachable without leaving the box, so "no network" must
// also mean no device-local access. Two routes closed by different
// mechanisms: raw TCP is closed by the empty namespace itself;
// through-the-gate-by-name is closed by `is_device_local`, since the
// gate resolves in the host namespace and a name can point anywhere.

/// `IP:PORT` of a service on the host's loopback.
const ENV_LOCAL_LOOPBACK: &str = "TILO_NETGATE_LOCAL_LOOPBACK";
/// `IP:PORT` of the same service on the host's own LAN address, or unset
/// when the machine has no non-loopback address.
const ENV_LOCAL_LAN: &str = "TILO_NETGATE_LOCAL_LAN";
/// Name to ask the gate for, when a gate is present.
const ENV_LOCAL_NAME: &str = "TILO_NETGATE_LOCAL_NAME";

/// The in-sandbox half of the device-local tests. Same dispatcher shape as
/// [`netns_probe`]: a no-op unless the outer test selected it.
#[test]
fn device_local_probe() {
    let Ok(loopback) = std::env::var(ENV_LOCAL_LOOPBACK) else {
        return;
    };
    println!("PROBE raw_loopback={}", describe(raw_get_addr(&loopback)));
    match std::env::var(ENV_LOCAL_LAN) {
        Ok(lan) => println!("PROBE raw_lan={}", describe(raw_get_addr(&lan))),
        Err(_) => println!("PROBE raw_lan=NO-LAN-ADDRESS"),
    }
    if let (Ok(socket), Ok(name)) = (std::env::var(ENV_SOCKET), std::env::var(ENV_LOCAL_NAME)) {
        let port: u16 = loopback.rsplit(':').next().unwrap().parse().unwrap();
        println!(
            "PROBE gate_by_name={}",
            describe(socks_get(&socket, &name, port))
        );
        println!(
            "PROBE http_by_name={}",
            describe(http_connect(&socket, &name, port))
        );
    }
}

/// [`raw_get`] against a full `IP:PORT`.
fn raw_get_addr(addr: &str) -> Outcome {
    let parsed = match addr.parse() {
        Ok(a) => a,
        Err(e) => return Outcome::Failed(format!("bad addr {addr}: {e}")),
    };
    match TcpStream::connect_timeout(&parsed, Duration::from_secs(2)) {
        Ok(mut s) => http_exchange(&mut s),
        Err(e) => Outcome::Failed(format!("{:?}", e.kind())),
    }
}

/// This host's own non-loopback address, if any — found via a
/// UDP-socket connect that sends no packet, just picks a routing-table
/// source address. `None` skips that half of the assertion.
fn host_lan_addr() -> Option<std::net::IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("198.51.100.9:9").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// Serve `body` on `ip:0`, returning the bound port. Stands in for a
/// device-local service — the collaboration broker's shape.
fn serve_on(ip: std::net::IpAddr, body: &'static str) -> u16 {
    let listener = TcpListener::bind((ip, 0)).expect("bind local service");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            std::thread::spawn(move || {
                let mut scratch = [0u8; 1024];
                let _ = stream.read(&mut scratch);
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.0 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            });
        }
    });
    port
}

/// Run [`device_local_probe`] in a sandbox shaped by `decision`.
fn run_device_local_probe(
    decision: &EgressDecision,
    gate_socket: Option<&Path>,
    loopback: &str,
    lan: Option<&str>,
    name: Option<&str>,
) -> ProbeReport {
    let exe = std::env::current_exe().expect("current_exe");
    let exe_s = exe.display().to_string();

    let mut argv: Vec<String> = vec!["--unshare-all".into(), "--die-with-parent".into()];
    if decision.shares_host_network() {
        argv.push("--share-net".into());
    }
    for (flag, a, b) in [
        ("--ro-bind", "/usr", "/usr"),
        ("--ro-bind-try", "/lib64", "/lib64"),
        ("--ro-bind-try", "/lib", "/lib"),
        ("--ro-bind", exe_s.as_str(), exe_s.as_str()),
    ] {
        argv.extend([flag.to_string(), a.to_string(), b.to_string()]);
    }
    argv.extend(["--proc".into(), "/proc".into()]);
    argv.extend(["--dev".into(), "/dev".into()]);
    argv.extend(["--tmpfs".into(), "/tmp".into()]);
    if let Some(host_socket) = gate_socket {
        let bind = NetGateBind {
            host_socket: host_socket.to_path_buf(),
            sandbox_socket: PathBuf::from(SANDBOX_SOCKET_PATH),
            port: PROBE_RELAY_PORT,
        };
        launcher::sandbox::push_net_gate(&mut argv, &bind);
    }
    argv.push(exe_s);
    argv.extend([
        "--exact".into(),
        probe_test_name("device_local_probe"),
        "--nocapture".into(),
        "--test-threads".into(),
        "1".into(),
    ]);

    let mut cmd = std::process::Command::new(bwrap());
    cmd.args(&argv).env(ENV_LOCAL_LOOPBACK, loopback);
    match lan {
        Some(l) => cmd.env(ENV_LOCAL_LAN, l),
        None => cmd.env_remove(ENV_LOCAL_LAN),
    };
    match name {
        Some(n) => cmd.env(ENV_LOCAL_NAME, n),
        None => cmd.env_remove(ENV_LOCAL_NAME),
    };
    match gate_socket {
        Some(_) => cmd.env(ENV_SOCKET, SANDBOX_SOCKET_PATH),
        None => cmd.env_remove(ENV_SOCKET),
    };

    let output = cmd.output().expect("spawn bwrap");
    report_from(&output)
}

#[test]
fn device_local_services_stay_out_of_reach_without_an_exact_grant() {
    // Phase `an_app_denied_the_network_cannot_reach_a_device_local_service`.
    // An app denied the network cannot reach a service on this device — not
    // on loopback, and not on the machine's own LAN address either.
    {
        let loopback_port = serve_on("127.0.0.1".parse().unwrap(), "BROKER-STANDIN");
        let loopback = format!("127.0.0.1:{loopback_port}");
        let lan = host_lan_addr().map(|ip| {
            let port = serve_on(ip, "BROKER-STANDIN");
            format!("{ip}:{port}")
        });

        let decision = EgressDecision::resolve("off", &[], &[]);
        assert_eq!(decision, EgressDecision::Blocked);

        let report = run_device_local_probe(&decision, None, &loopback, lan.as_deref(), None);
        report.dump("denied: device-local");

        assert!(
            report.get("raw_loopback").starts_with("ERR:"),
            "a sandbox with no network capability must not reach a service on the host's \
             loopback — that is where the collaboration broker lives: {}",
            report.get("raw_loopback")
        );
        if lan.is_some() {
            assert!(
                report.get("raw_lan").starts_with("ERR:"),
                "nor on the host's own LAN address, which is the same machine by another name: {}",
                report.get("raw_lan")
            );
        }

        // Not vacuous: the same probe and listeners, with the network shared,
        // reach both — so the failures above are the namespace, not a dead
        // service.
        let shared = run_device_local_probe(
            &EgressDecision::Unrestricted,
            None,
            &loopback,
            lan.as_deref(),
            None,
        );
        shared.dump("shared: device-local");
        assert_eq!(
            shared.get("raw_loopback"),
            "BODY:BROKER-STANDIN",
            "with the host network shared the loopback service is reachable, so the listener \
             works and the denial above was real"
        );
        if lan.is_some() {
            assert_eq!(shared.get("raw_lan"), "BODY:BROKER-STANDIN");
        }
    }
    // Phase `a_grant_that_merely_covers_a_local_name_does_not_reach_this_device`.
    // An app with a *host allowlist* has a gate resolving names in the
    // host namespace, so a name pointing at this device is the bypass —
    // closed unless the grant names that host exactly. One variable
    // changes: the spelling of the grant.
    {
        let port = serve_on("127.0.0.1".parse().unwrap(), "BROKER-STANDIN");
        let loopback = format!("127.0.0.1:{port}");

        // `.localhost` covers `localhost` for the access decision — but it is
        // not the user pointing at this machine.
        let covered = EgressPolicy::from_grants([".localhost"]);
        assert!(
            covered.permits("localhost"),
            "the grant must cover the name, or the refusal below proves nothing about \
             device-local filtering"
        );
        let (covered_gate, covered_socket) = start_gate("device-local-covered", covered.clone());
        let refused = run_device_local_probe(
            &EgressDecision::Filtered(covered),
            Some(&covered_socket),
            &loopback,
            None,
            Some("localhost"),
        );
        refused.dump("covered grant");
        assert_eq!(
            refused.get("gate_by_name"),
            format!("REFUSED:{:#04x}", reply::HOST_UNREACHABLE),
            "a name that resolves to this device must be unreachable through the gate when the \
             grant only covers it"
        );
        assert_eq!(
            refused.get("http_by_name"),
            "STATUS:502",
            "and over HTTP CONNECT too, or the rule is bypassable by switching protocol"
        );
        assert!(
            refused.get("raw_loopback").starts_with("ERR:"),
            "the namespace is still empty, so there is no route beside the gate: {}",
            refused.get("raw_loopback")
        );
        let _ = covered_gate;

        // Same everything, an exact grant: the service is reached. So the
        // refusal above was the device-local rule and not a broken fixture.
        let named = EgressPolicy::from_grants(["localhost"]);
        let (named_gate, named_socket) = start_gate("device-local-named", named.clone());
        let allowed = run_device_local_probe(
            &EgressDecision::Filtered(named),
            Some(&named_socket),
            &loopback,
            None,
            Some("localhost"),
        );
        allowed.dump("exact grant");
        assert_eq!(
            allowed.get("gate_by_name"),
            "BODY:BROKER-STANDIN",
            "an exact grant for a device-local host must reach it — the rule narrows what a \
             pattern covers, it does not ban local services outright"
        );
        let _ = named_gate;
    }
}
