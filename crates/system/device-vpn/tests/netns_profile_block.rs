//! The kill switch of the person's VPN profiles in a network namespace: the
//! helper's own Linux backend puts the nftables block up and takes it down
//! as the profile's state changes, refuses a caller that is not the seat's
//! shell, and a helper started after one died lifts what it left.
//!
//! Each test runs itself again under `unshare --user --map-root-user --net`,
//! where it may change the namespace's firewall, and checks from inside what
//! a program there can still send: a send the block refuses fails with
//! "operation not permitted" at once.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use device_vpn::profile::{Activity, Intent, Switch, block_for};
use device_vpn::protocol::{Envelope, PROTOCOL_VERSION, ProfileBlock, Request, Response};
use device_vpn::service::linux::{Config, LinuxBackend};
use device_vpn::service::{Caller, Service, peer, server};

const ROLE: &str = "TILO_DEVICE_VPN_PROFILE_ROLE";
const STATE: &str = "TILO_DEVICE_VPN_PROFILE_STATE";
const TABLE: &str = "tilo_device_vpn_profile";
const PROFILE: &str = "work";
/// The profile's server, out through the uplink.
const SERVER: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 7);
/// Somewhere on the internet, out through the uplink.
const ELSEWHERE: Ipv4Addr = Ipv4Addr::new(198, 18, 0, 1);
/// A printer on the local network, out through the uplink.
const PRINTER: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 10);
/// An address the tunnel interface routes.
const THROUGH_TUNNEL: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 5);

fn run(program: &Path, args: &str) -> bool {
    Command::new(program)
        .args(args.split_whitespace())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("{} {args}: {error}", program.display()))
        .success()
}

fn ip(args: &str) {
    assert!(run(&Config::default().ip, args), "ip {args}");
}

/// Whether the block's table is in the namespace's ruleset.
fn table_present() -> bool {
    run(&Config::default().nft, &format!("list table inet {TABLE}"))
}

fn config(state: &Path) -> Config {
    Config {
        record: state.join("record.json"),
        dns_marker: state.join("dns"),
        seat: seat_peer::Seat {
            trusted_ui: PathBuf::from("/usr/bin/shell"),
            seat: state.join("seat0"),
            users: state.join("users"),
        },
        ..Config::default()
    }
}

/// The helper as it starts: on the Linux backend, with the record an earlier
/// one left.
fn helper(state: &Path) -> Service<LinuxBackend> {
    let backend = LinuxBackend::new(config(state));
    let record = backend.load();
    Service::new(backend, record)
}

/// What a program in the namespace gets from sending one UDP datagram from
/// `from_port` (any with 0) to `to`.
fn send(from_port: u16, to: SocketAddr) -> std::io::Result<()> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, from_port))?;
    socket.send_to(b"x", to).map(|_| ())
}

fn sends(to: Ipv4Addr, port: u16) -> bool {
    match send(0, SocketAddr::from((to, port))) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => false,
        Err(error) => panic!("sending to {to}: {error}"),
    }
}

/// An uplink to "the internet" and a tunnel interface, as a computer with a
/// VPN profile has: one end of a veth pair with the default route (an
/// Ethernet-like link, not a tunnel), and a TUN whose descriptor the caller
/// keeps.
fn machine(state: &Path) -> std::os::fd::OwnedFd {
    ip("link set lo up");
    ip("link add uplink0 type veth peer name uplink1");
    ip("address add 192.0.2.2/24 dev uplink0");
    ip("link set uplink1 up");
    ip("link set uplink0 up");
    ip("route add default via 192.0.2.1 dev uplink0");
    let tun = device_vpn::service::linux::create_tun("vpntest0").unwrap();
    ip("address add 10.99.0.2/24 dev vpntest0");
    ip("link set vpntest0 up");
    ip(&format!("route add {THROUGH_TUNNEL}/32 dev vpntest0"));
    std::fs::create_dir_all(state.join("users")).unwrap();
    std::fs::write(state.join("users/0"), "STATE=active\n").unwrap();
    std::fs::write(state.join("seat0"), "ACTIVE_UID=0\n").unwrap();
    tun
}

fn apply(service: &mut Service<LinuxBackend>, block: Option<ProfileBlock>) {
    let request = match block {
        Some(block) => Request::HoldProfileBlock { block },
        None => Request::ReleaseProfileBlock,
    };
    assert_eq!(service.handle(Caller { uid: 0 }, request).0, Response::Done);
}

fn assert_blocking(allow_local: bool) {
    assert!(table_present(), "the block's table is up");
    assert!(!sends(ELSEWHERE, 443), "the internet is blocked");
    assert!(sends(SERVER, 1194), "the server is reachable");
    assert!(sends(THROUGH_TUNNEL, 443), "the tunnel interface is open");
    assert_eq!(sends(PRINTER, 631), allow_local, "the local network");
    assert!(
        send(68, SocketAddr::from((ELSEWHERE, 67))).is_ok(),
        "DHCP still works"
    );
}

fn assert_open() {
    assert!(!table_present(), "no block's table");
    assert!(sends(ELSEWHERE, 443));
    assert!(sends(PRINTER, 631));
}

/// The profile's life, as `NetworkManager` reports it, through the same
/// decisions the session makes and the helper's Linux backend.
fn follows_the_profile_state(state: &Path) {
    let _tun = machine(state);
    let mut service = helper(state);
    let mut intent = Intent::default();
    let servers = BTreeMap::from([(PROFILE.to_owned(), vec![IpAddr::V4(SERVER)])]);
    let mut step = |switch: Option<Switch>, activity: Option<Activity>, released: &[String]| {
        let switches: BTreeMap<String, Switch> = switch
            .map(|switch| BTreeMap::from([(PROFILE.to_owned(), switch)]))
            .unwrap_or_default();
        let active: BTreeMap<String, Activity> = activity
            .map(|activity| BTreeMap::from([(PROFILE.to_owned(), activity)]))
            .unwrap_or_default();
        let blocking = intent.decide(&switches, &active, released);
        apply(
            &mut service,
            blocking.map(|blocking| block_for(&blocking, &servers)),
        );
    };
    let on = Some(Switch { allow_local: false });
    assert_open();
    // Set to connect: NetworkManager brings it up.
    step(on, Some(Activity::Activating), &[]);
    assert_blocking(false);
    // Connected: the tunnel carries the traffic, nothing blocks.
    step(on, Some(Activity::Activated), &[]);
    assert_open();
    // The tunnel drops, not on purpose.
    step(on, None, &[]);
    assert_blocking(false);
    // The person allows the local network meanwhile.
    step(Some(Switch { allow_local: true }), None, &[]);
    assert_blocking(true);
    // Turned off on purpose.
    step(on, None, &[PROFILE.to_owned()]);
    assert_open();
    // Up again, then the person turns the kill switch off while it drops.
    step(on, Some(Activity::Activated), &[]);
    step(on, Some(Activity::Deactivating), &[]);
    assert_blocking(false);
    step(None, None, &[]);
    assert_open();
    assert!(service.is_idle());
}

/// The helper's own server loop and caller check on `listener`, in a
/// thread, until the flag is set; the thread hands the helper back.
fn serve(
    service: Service<LinuxBackend>,
    listener: UnixListener,
    state: &Path,
) -> (
    std::thread::JoinHandle<Service<LinuxBackend>>,
    Arc<AtomicBool>,
) {
    let stop = Arc::new(AtomicBool::new(false));
    let seat = config(state).seat;
    let serving = std::thread::spawn({
        let stop = Arc::clone(&stop);
        move || {
            let mut service = service;
            server::serve(
                &listener,
                &mut service,
                |stream| {
                    seat_peer::facts(stream)
                        .map_err(|error| format!("unidentified caller: {error}"))
                        .and_then(|facts| peer::decide(&seat, &facts))
                },
                server::Pacing {
                    housekeeping: Duration::from_millis(50),
                    idle_exit: None,
                },
                &stop,
            );
            service
        }
    });
    (serving, stop)
}

/// Ask over the socket at `socket`, as any local program can.
fn ask(socket: &Path, request: Request) -> Response {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut line = serde_json::to_vec(&Envelope {
        protocol: PROTOCOL_VERSION,
        request,
    })
    .unwrap();
    line.push(b'\n');
    // Refused before its request is read: the write may find the connection
    // already closed.
    let _ = stream.write_all(&line);
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer).unwrap();
    serde_json::from_str(&answer).unwrap()
}

/// This test program, a program of the person's that is not the seat's
/// shell, asks for and against the block over the helper's socket.
fn a_foreign_caller_is_refused(state: &Path) {
    let _tun = machine(state);
    let socket = state.join("control.sock");
    let shell_block = ProfileBlock {
        servers: vec![IpAddr::V4(SERVER)],
        allow_local: false,
    };

    let (serving, stop) = serve(helper(state), UnixListener::bind(&socket).unwrap(), state);
    assert!(matches!(
        ask(
            &socket,
            Request::HoldProfileBlock {
                block: shell_block.clone()
            }
        ),
        Response::Refused { .. }
    ));
    stop.store(true, Ordering::Release);
    let mut service = serving.join().unwrap();
    assert!(service.is_idle());
    assert!(!table_present(), "nothing was put up");
    assert!(sends(ELSEWHERE, 443));

    // A block the shell holds is not the foreign caller's to lift or change.
    apply(&mut service, Some(shell_block.clone()));
    std::fs::remove_file(&socket).unwrap();
    let (serving, stop) = serve(service, UnixListener::bind(&socket).unwrap(), state);
    assert!(matches!(
        ask(&socket, Request::ReleaseProfileBlock),
        Response::Refused { .. }
    ));
    assert!(matches!(
        ask(
            &socket,
            Request::HoldProfileBlock {
                block: ProfileBlock {
                    servers: vec![],
                    allow_local: true,
                }
            }
        ),
        Response::Refused { .. }
    ));
    stop.store(true, Ordering::Release);
    let mut service = serving.join().unwrap();
    assert_blocking(false);
    assert_eq!(
        service
            .record()
            .profile_block
            .as_ref()
            .map(|held| &held.block),
        Some(&shell_block)
    );
    apply(&mut service, None);
    assert_open();
}

/// A helper holds the block and dies; the next one lifts it, once nobody
/// is left to want it, or when reset at boot.
fn a_crashed_helpers_block_is_lifted(state: &Path) {
    let _tun = machine(state);
    let crash = |state: &Path| {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "netns_profile_crasher",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ROLE, "crasher")
            .env(STATE, state)
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(!status.success(), "the helper died");
        assert!(table_present(), "what it put up outlived it");
        assert!(!sends(ELSEWHERE, 443));
    };

    // Reset, as the reset unit does.
    crash(state);
    let mut next = helper(state);
    assert!(!next.is_idle(), "the next helper knows what was left");
    next.reset();
    assert!(next.is_idle());
    assert_open();

    // While the person is still logged in, a restarted helper keeps the
    // block; once their session ends, its housekeeping lifts it.
    crash(state);
    let mut next = helper(state);
    next.housekeeping();
    assert!(table_present(), "kept while its owner is logged in");
    std::fs::remove_file(state.join("users/0")).unwrap();
    next.housekeeping();
    assert!(next.is_idle());
    assert_open();
}

/// A helper that holds the block and dies without lifting it.
#[test]
#[ignore = "run by a_crashed_helpers_block_is_lifted inside its namespace"]
fn netns_profile_crasher() {
    if std::env::var(ROLE).as_deref() != Ok("crasher") {
        return;
    }
    let state = PathBuf::from(std::env::var(STATE).unwrap());
    let mut service = helper(&state);
    apply(
        &mut service,
        Some(ProfileBlock {
            servers: vec![IpAddr::V4(SERVER)],
            allow_local: false,
        }),
    );
    std::process::abort();
}

/// One test's body, inside its own user and network namespace.
#[test]
#[ignore = "run by the tests below inside a namespace"]
fn netns_profile_role() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let state = PathBuf::from(std::env::var(STATE).unwrap());
    match role.as_str() {
        "follows" => follows_the_profile_state(&state),
        "foreign" => a_foreign_caller_is_refused(&state),
        "crash" => a_crashed_helpers_block_is_lifted(&state),
        "crasher" => {}
        other => panic!("no role {other}"),
    }
}

/// What the tests need from the machine, or what is missing.
fn machine_ready() -> Result<(), String> {
    let config = Config::default();
    for (name, path) in [("ip", &config.ip), ("nft", &config.nft)] {
        if !path.exists() {
            return Err(format!("no {name} at {}", path.display()));
        }
    }
    if !Path::new("/dev/net/tun").exists() {
        return Err("no /dev/net/tun".into());
    }
    let status = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg(&config.nft)
        .args(["list", "ruleset"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("unshare: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "nft in a namespace of unshare --user --map-root-user --net: {status}"
        ))
    }
}

/// Run `role` in a fresh user and network namespace and require it passes.
fn in_namespace(role: &str) {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "netns_profile_role",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(ROLE, role)
        .env(STATE, dir.path())
        .stdin(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("unshare: {error}"));
    assert!(status.success(), "the {role} namespace failed: {status}");
}

#[test]
fn the_block_comes_and_goes_with_the_profile_state() {
    tilo_test_env::require_capability!(
        NetworkTunnel,
        machine_ready(),
        "the namespace needs TUN devices, ip and nft"
    );
    in_namespace("follows");
}

#[test]
fn a_caller_other_than_the_seats_shell_is_refused() {
    tilo_test_env::require_capability!(
        NetworkTunnel,
        machine_ready(),
        "the namespace needs TUN devices, ip and nft"
    );
    in_namespace("foreign");
}

#[test]
fn a_block_left_by_a_crashed_helper_is_lifted() {
    tilo_test_env::require_capability!(
        NetworkTunnel,
        machine_ready(),
        "the namespace needs TUN devices, ip and nft"
    );
    in_namespace("crash");
}
