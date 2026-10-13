//! A guest's network, end to end: a record that allows one host, a shell
//! in a real guest, and what that shell can and cannot reach.
//!
//! The outside world is faked on loopback — a resolver that answers every
//! name with one address, and a TCP server — and `pasta` maps that address
//! to the host's loopback (`TILO_USH_EGRESS_MAP_HOST_LOOPBACK`), so the
//! guest's connections really leave its namespace through the filter and
//! `pasta`, without the internet.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::Write;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use tilo_environment::{DEFAULT_RECORD, Environment, PackageId};
use ush::records::Records;
use ush::store::Store;

use crate::serve_handoff::{RUNTIME, fixture_images, guest_unavailable};

/// Where the fake world is, as the guest sees it.
const WORLD: &str = "192.0.2.10";

/// A resolver on host loopback answering every A query with [`WORLD`].
fn fake_resolver() -> SocketAddr {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = sock.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut buf = [0_u8; 1500];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            let q = &buf[..n];
            let Some(question) = tilo_egress::dns::parse_query(q) else {
                continue;
            };
            let name_len = if question.name.is_empty() {
                1
            } else {
                question.name.len() + 2
            };
            let mut out = q[..12 + name_len + 4].to_vec();
            out[2] |= 0x80;
            out[3] |= 0x80;
            if question.qtype == tilo_egress::dns::TYPE_A {
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
        for mut stream in listener.incoming().flatten() {
            let _ = stream.write_all(b"hello-from-outside\n");
        }
    });
    port
}

/// A store whose default environment allows `hosts`.
fn store_allowing(dir: &Path, hosts: &[&str]) -> Store {
    let store = Store::at(dir.join("store"));
    store.ensure().unwrap();
    let records = Records::at(store.records_root());
    records
        .create(
            DEFAULT_RECORD,
            &Environment::new(
                "Network",
                "2026.10",
                tilo_environment::host_arch(),
                &PackageId::from_bytes(RUNTIME),
            ),
        )
        .unwrap();
    for host in hosts {
        records
            .apply(
                DEFAULT_RECORD,
                Environment::ALLOW_HOST.with((*host).to_owned()),
            )
            .unwrap();
    }
    store
}

fn run(dir: &Path, store: &Store, script: &str, env: &[(&str, String)]) -> Output {
    let images = dir.join("images");
    if !images.exists() {
        fixture_images(&images);
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tilo-ush"));
    cmd.args(["run", "-c", script])
        .env(ush::store::STORE_ENV, store.root())
        .env("TILO_USH_IMAGES", &images)
        .env("SHELL", "/usr/bin/bash")
        .stdin(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("tilo-ush runs")
}

fn network_unavailable() -> Option<String> {
    if let Some(reason) = guest_unavailable() {
        return Some(reason);
    }
    if let Err(e) = tilo_egress::Netns::create() {
        return Some(format!("no network namespace: {e}"));
    }
    if !tilo_egress::pasta::find().is_file() {
        return Some("pasta (the passt package) is not installed".into());
    }
    if !Path::new("/dev/net/tun").exists() {
        return Some("there is no /dev/net/tun for pasta's tap device".into());
    }
    None
}

/// The guest reaches the host its record allows — by name, through the
/// session's resolver — and nothing else: not an address literal, not a
/// name nobody granted. `perm allow-host` takes effect in the running
/// session, and `guard status` reports the enforcement it tested.
#[test]
fn a_guest_reaches_its_granted_host_and_nothing_else() {
    if let Some(reason) = network_unavailable() {
        eprintln!("network: not exercising a guest's network here — {reason}");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = store_allowing(dir.path(), &["granted.test"]);
    let resolver = fake_resolver();
    let port = fake_server();
    // Markers are printed by the guest; a subshell keeps a failed `exec`
    // redirection from ending the script.
    let script = format!(
        "( exec 3<>/dev/tcp/{WORLD}/{port} ) 2>/dev/null && echo RAW_REACHED || echo RAW_REFUSED; \
         getent hosts denied.test >/dev/null && echo DENIED_RESOLVED || echo DENIED_NXDOMAIN; \
         ( exec 3<>/dev/tcp/granted.test/{port}; read -r l <&3; echo \"GRANTED_$l\" ) 2>/dev/null || echo GRANTED_FAILED; \
         perm allow-host denied.test; \
         ( exec 3<>/dev/tcp/denied.test/{port}; read -r l <&3; echo \"LIVE_$l\" ) 2>/dev/null || echo LIVE_FAILED; \
         cat /etc/resolv.conf; guard status"
    );
    let out = run(
        dir.path(),
        &store,
        &script,
        &[
            ("TILO_USH_RESOLVER", resolver.to_string()),
            ("TILO_USH_EGRESS_MAP_HOST_LOOPBACK", WORLD.to_owned()),
        ],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let both = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(out.status.success(), "the guest failed:\n{both}");
    assert!(
        stdout.contains("RAW_REFUSED"),
        "an address literal got out:\n{both}"
    );
    assert!(
        stdout.contains("DENIED_NXDOMAIN"),
        "a name nobody granted resolved:\n{both}"
    );
    assert!(
        stdout.contains("GRANTED_hello-from-outside"),
        "the granted host was not reachable:\n{both}"
    );
    assert!(
        stdout.contains("denied.test is allowed") && stdout.contains("in this session now"),
        "perm allow-host did not apply live:\n{both}"
    );
    assert!(
        stdout.contains("LIVE_hello-from-outside"),
        "a host allowed in the session was not reachable:\n{both}"
    );
    assert!(stdout.contains("nameserver 127.0.0.1"), "{both}");
    assert!(
        stdout.contains("network     : only denied.test, granted.test"),
        "guard status must name the hosts in force:\n{both}"
    );
    assert!(
        stdout.contains("1.1.1.1:443 (not granted) refused"),
        "guard status must test an address nothing granted:\n{both}"
    );
    assert!(
        stdout.contains(&format!("denied.test (granted) resolved to {WORLD}")),
        "guard status must test a granted host:\n{both}"
    );
    // The grant is the record's, for the next session too.
    let env = Records::at(store.records_root())
        .load(DEFAULT_RECORD)
        .unwrap()
        .unwrap();
    assert!(
        env.grants
            .0
            .iter()
            .any(|c| c.scope == collab_types::caps::Scope::NetHost("denied.test".into())),
        "{:?}",
        env.grants.0
    );
}

/// When the kernel will not take the packet filter, a guest whose record
/// grants hosts does not start — it never runs with an unfiltered network.
/// One with nothing granted starts offline.
#[test]
fn a_guest_whose_hosts_cannot_be_enforced_does_not_start() {
    if let Some(reason) = guest_unavailable() {
        eprintln!("network: no guest on this machine — {reason}");
        return;
    }
    if let Err(e) = tilo_egress::Netns::create() {
        eprintln!("network: no network namespace on this machine — {e}");
        return;
    }
    let refuse = [("TILO_USH_EGRESS_REFUSE_NETFILTER", "1".to_owned())];

    let dir = tempfile::tempdir().unwrap();
    let store = store_allowing(dir.path(), &["granted.test"]);
    let out = run(dir.path(), &store, "echo GUEST_RAN", &refuse);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the guest started:\n{stdout}\n{stderr}"
    );
    assert!(!stdout.contains("GUEST_RAN"), "{stdout}");
    assert!(
        stderr.contains("could not be set up") && stderr.contains("network filter"),
        "the refusal must say why: {stderr}"
    );

    let dir = tempfile::tempdir().unwrap();
    let store = store_allowing(dir.path(), &[]);
    let out = run(
        dir.path(),
        &store,
        "( exec 3<>/dev/tcp/192.0.2.10/80 ) 2>/dev/null && echo OUT || echo OFFLINE",
        &refuse,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("OFFLINE"), "{stdout}\n{stderr}");
}
