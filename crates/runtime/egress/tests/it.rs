//! Egress against a real kernel: a namespace of its own, the filter
//! programmed over netlink, `pasta` attached, and a probe process inside
//! that resolves through the stub and connects.
//!
//! The outside world is faked on loopback — a resolver that answers every
//! name with one address, and a TCP server — and `pasta` maps that address
//! to the host's loopback, so the probe's connections really leave the
//! namespace through the filter and `pasta` without needing the internet.
//!
//! Skipped, with the reason, where the machine cannot make a user namespace
//! or has no `pasta`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::process::Command;
use std::time::Duration;

use tilo_egress::{EgressError, EgressOptions, EgressPolicy, Netns, dns, spawn_egress};

/// The address the fake world lives at, as the sandbox sees it.
const WORLD: &str = "192.0.2.10";
const PROBE_ENV: &str = "TILO_EGRESS_PROBE";

/// A resolver on host loopback answering every A query with [`WORLD`].
fn fake_resolver() -> SocketAddr {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 1500];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            let q = &buf[..n];
            let Some(question) = dns::parse_query(q) else {
                continue;
            };
            let name_len = if question.name.is_empty() {
                1
            } else {
                question.name.len() + 2
            };
            let end = 12 + name_len + 4;
            let mut out = q[..end].to_vec();
            out[2] |= 0x80;
            out[3] |= 0x80;
            if question.qtype == dns::TYPE_A {
                out[7] = 1;
                out.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 1, 44, 0, 4]);
                out.extend_from_slice(&WORLD.parse::<std::net::Ipv4Addr>().unwrap().octets());
            }
            let _ = sock.send_to(&out, from);
        }
    });
    addr
}

/// A TCP server on host loopback that greets every connection.
fn fake_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let _ = stream.write_all(b"hello from outside\n");
        }
    });
    port
}

fn options(resolver: SocketAddr) -> EgressOptions {
    EgressOptions {
        upstream: vec![resolver],
        pasta: tilo_egress::pasta::find(),
        map_host_loopback: Some(WORLD.parse().unwrap()),
        refuse_netfilter: false,
    }
}

/// Why this machine can't run these tests, if it can't.
fn unavailable() -> Option<String> {
    if let Err(e) = Netns::create() {
        return Some(format!("no user/network namespace: {e}"));
    }
    if !tilo_egress::pasta::find().is_file() {
        return Some("pasta (the passt package) is not installed".into());
    }
    if !std::path::Path::new("/dev/net/tun").exists() {
        return Some("there is no /dev/net/tun for pasta's tap device".into());
    }
    None
}

/// Run the probe inside `netns`: each step is `resolve:NAME` or
/// `connect:IP:PORT`; one line of result per step.
fn probe(netns: &Netns, steps: &[String]) -> Vec<String> {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", "probe_inside_the_namespace", "--nocapture"])
        .env(PROBE_ENV, steps.join(";"));
    netns.enter(&mut cmd);
    let out = cmd.output().expect("the probe runs");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|l| l.strip_prefix("PROBE "))
        .map(str::to_owned)
        .collect()
}

/// The probe itself: does nothing unless started by [`probe`].
#[test]
fn probe_inside_the_namespace() {
    let Ok(steps) = std::env::var(PROBE_ENV) else {
        return;
    };
    if steps == "supervise" {
        // Be a supervisor: start egress, say where pasta is, and wait to be
        // killed. Not dumpable, as a supervisor that loaded key material
        // is: its forks must still be able to hand pasta the namespaces.
        // SAFETY: PR_SET_DUMPABLE takes scalar arguments only.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, 0);
        let netns = Netns::create().unwrap();
        let egress = spawn_egress(&netns, EgressPolicy::Any, &options(fake_resolver())).unwrap();
        println!("PASTA {}", egress.pasta_pid().unwrap());
        std::thread::sleep(Duration::from_secs(60));
        return;
    }
    for step in steps.split(';') {
        let line = if let Some(name) = step.strip_prefix("resolve:") {
            resolve(name)
        } else if let Some(target) = step.strip_prefix("connect:") {
            connect(target.parse().unwrap())
        } else {
            format!("bad step {step}")
        };
        println!("PROBE {line}");
    }
}

fn resolve(name: &str) -> String {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    sock.send_to(&dns::query(77, name, dns::TYPE_A), "127.0.0.1:53")
        .unwrap();
    let mut buf = [0_u8; 1500];
    match sock.recv(&mut buf) {
        Ok(n) => {
            let reply = &buf[..n];
            let addrs: Vec<String> = dns::answered_addresses(reply)
                .iter()
                .map(|a| a.addr.to_string())
                .collect();
            format!(
                "rcode={} addrs={}",
                dns::rcode(reply).unwrap(),
                addrs.join(",")
            )
        }
        Err(e) => format!("no answer: {e}"),
    }
}

fn connect(target: SocketAddr) -> String {
    match TcpStream::connect_timeout(&target, Duration::from_secs(5)) {
        Ok(mut s) => {
            let mut text = String::new();
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = s.read_to_string(&mut text);
            format!("connected {}", text.trim())
        }
        Err(e) => format!("failed {:?}", e.kind()),
    }
}

#[test]
fn a_granted_name_is_reachable_and_nothing_else_is() {
    if let Some(reason) = unavailable() {
        eprintln!("egress: skipping — {reason}");
        return;
    }
    let resolver = fake_resolver();
    let port = fake_server();
    let netns = Netns::create().unwrap();
    let egress = spawn_egress(
        &netns,
        EgressPolicy::from_grants(["granted.test", ".wild.test"]),
        &options(resolver),
    )
    .expect("egress starts");
    let world = format!("{WORLD}:{port}");
    let got = probe(
        &netns,
        &[
            // An address literal, before any name pointed at it: refused at
            // once, not left to time out.
            format!("connect:{world}"),
            "resolve:other.test".into(),
            format!("connect:{world}"),
            "resolve:granted.test".into(),
            format!("connect:{world}"),
            "resolve:a.wild.test".into(),
            // DNS straight to another server is refused even for an
            // address that is now allowed.
            format!("connect:{WORLD}:53"),
        ],
    );
    assert_eq!(got.len(), 7, "{got:?}");
    assert_eq!(
        got[0], "failed ConnectionRefused",
        "a raw IP got through: {got:?}"
    );
    assert_eq!(
        got[1], "rcode=3 addrs=",
        "a name that is not granted resolved: {got:?}"
    );
    assert_eq!(got[2], "failed ConnectionRefused", "{got:?}");
    assert_eq!(got[3], format!("rcode=0 addrs={WORLD}"), "{got:?}");
    assert_eq!(
        got[4], "connected hello from outside",
        "the granted host: {got:?}"
    );
    assert_eq!(
        got[5],
        format!("rcode=0 addrs={WORLD}"),
        "under a domain grant: {got:?}"
    );
    assert_eq!(
        got[6], "failed ConnectionRefused",
        "DNS around the stub: {got:?}"
    );
    assert!(matches!(
        egress.enforcement(),
        tilo_egress::Enforcement::Filtered(_)
    ));

    // Taking the grant away empties the sets: the address is refused again.
    egress.update_policy(EgressPolicy::none()).unwrap();
    let got = probe(
        &netns,
        &[format!("connect:{world}"), "resolve:granted.test".into()],
    );
    assert_eq!(got, ["failed ConnectionRefused", "rcode=3 addrs="]);
}

#[test]
fn a_star_grant_reaches_everything_through_its_own_namespace() {
    if let Some(reason) = unavailable() {
        eprintln!("egress: skipping — {reason}");
        return;
    }
    let resolver = fake_resolver();
    let port = fake_server();
    let netns = Netns::create().unwrap();
    let egress = spawn_egress(&netns, EgressPolicy::Any, &options(resolver)).unwrap();
    assert_eq!(egress.enforcement(), tilo_egress::Enforcement::Unrestricted);
    let got = probe(
        &netns,
        &[
            format!("connect:{WORLD}:{port}"),
            "resolve:anything.test".into(),
        ],
    );
    assert_eq!(got[0], "connected hello from outside", "{got:?}");
    assert_eq!(got[1], format!("rcode=0 addrs={WORLD}"), "{got:?}");
}

#[test]
fn without_the_filter_a_sandbox_with_grants_does_not_start() {
    if let Some(reason) = unavailable() {
        eprintln!("egress: skipping — {reason}");
        return;
    }
    let resolver = fake_resolver();
    let port = fake_server();
    let mut opts = options(resolver);
    opts.refuse_netfilter = true;

    let netns = Netns::create().unwrap();
    let err = spawn_egress(&netns, EgressPolicy::from_grants(["granted.test"]), &opts)
        .expect_err("egress started with grants it could not enforce");
    assert!(matches!(err, EgressError::Netfilter(_)), "{err}");
    assert!(err.to_string().contains("does not start"), "{err}");

    // With nothing granted there is nothing to enforce: the sandbox starts,
    // offline — no route out at all, so not even an address literal leaves.
    let netns = Netns::create().unwrap();
    let egress = spawn_egress(&netns, EgressPolicy::none(), &opts).unwrap();
    assert_eq!(egress.enforcement(), tilo_egress::Enforcement::Offline);
    let got = probe(
        &netns,
        &[
            format!("connect:{WORLD}:{port}"),
            "resolve:granted.test".into(),
        ],
    );
    assert_eq!(got[0], "failed NetworkUnreachable", "{got:?}");
    assert_eq!(got[1], "rcode=3 addrs=", "{got:?}");
    // And it cannot be widened later: there is no filter to widen behind.
    assert!(
        egress
            .update_policy(EgressPolicy::from_grants(["granted.test"]))
            .is_err()
    );
    let _: IpAddr = WORLD.parse().unwrap();
}

/// `pasta` goes when its supervisor does, even by `SIGKILL` — which is how a
/// closed Terminal ends a served session.
#[test]
fn pasta_does_not_outlive_a_killed_supervisor() {
    use std::io::BufRead;
    if let Some(reason) = unavailable() {
        eprintln!("egress: skipping — {reason}");
        return;
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe_inside_the_namespace", "--nocapture"])
        .env(PROBE_ENV, "supervise")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = std::io::BufReader::new(child.stdout.take().unwrap()).lines();
    let pid: u32 = lines
        .by_ref()
        .map_while(Result::ok)
        .find_map(|l| l.strip_prefix("PASTA ").map(|p| p.trim().parse().unwrap()))
        .expect("the supervisor reported pasta");
    let proc_dir = std::path::PathBuf::from(format!("/proc/{pid}"));
    assert!(proc_dir.exists(), "pasta is running");
    child.kill().unwrap();
    child.wait().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while proc_dir.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!proc_dir.exists(), "pasta {pid} outlived its supervisor");
}
