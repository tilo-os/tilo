//! What a converged, idle two-desktop session costs on the wire: two devices
//! of one person share a workspace room (and the person's own continuity
//! topic), one of them with its pointer resting over a shared tab. Both run
//! their network tick and pointer presence the way the compositor's frame
//! loop does, ten times a second, as a loop woken by something unrelated (a
//! clock, a blinking caret) would. Nobody does anything for a minute, so
//! nothing needs saying: every application message either device receives
//! in that minute, and every repaint a peer's pointer causes, is waste.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use ui::presence::PresenceCtx;

use super::{HostIdentity, NetCmd, Reach, ShellNetwork};
use crate::shell::peer_presence::{Desk, PeerPresence};
use crate::shell::state::{RoomId, WorkspaceTree, WorkspaceTreeHandle};

/// How often the simulated frame loop runs.
const FRAME: Duration = Duration::from_millis(100);
/// How long the session runs after converging before it is measured, so the
/// join's own catch-up and the pointer's last move are behind it.
const SETTLE: Duration = Duration::from_secs(10);
/// The idle stretch measured.
const IDLE: Duration = Duration::from_secs(60);
/// Most times per second the two desktops' own threads (their network
/// actors, endpoints and drains; not the thread driving their frame loops)
/// may block while idle, together.
///
/// Counted as voluntary switches, one per wakeup, as the lone gate is:
/// preemptions measure the runner. Counting both, this gate failed
/// the shell shard at 15.45 to 17.31/s against 15/s while its threads
/// blocked about 12.8 times a second (forge #4035), and nightly run 91200
/// measured 15.68/s, 3.63 of it preemptions.
///
/// Measured blocking on CI (focused, #4138): main `f744af88ad` 6.12 to
/// 6.98/s (runs 92709-92713); with the upkeep tick and the gossip sweep
/// armed only while there are peers, which this pair has, 5.84 to 7.17/s
/// (runs 92724, 92787-92796). What remains is the two network runtimes'
/// workers over both desktops: the transport's 5 s keep-alives, the 5 s
/// upkeep tick and the 10 s sweep both desktops keep while linked.
///
/// Where the gate runs, loaded, its threads block more than when focused:
/// nightly's 12.05/s against about 10/s focused on the same code before
/// #4074 (#3983, `docs/benchmarks/idle-wakeups-2026-10-09.md`). The
/// focused p95, 7.2/s, a third more for that, plus a quarter: 12/s.
const IDLE_WAKEUP_BUDGET: f64 = 12.0;
/// Most CPU the two desktops' own threads may use while idle, in percent of
/// one core: 600 ms over [`IDLE`]. The wakeup count leaves out preemptions,
/// so this is what still catches a thread that spins without blocking.
/// Measured 0.2 to 0.3 % focused and 0.5 % in nightly run 91200.
const IDLE_CPU_BUDGET: f64 = 1.0;

/// How long a lone desktop runs after it starts before it is measured, so
/// its endpoint and its room's topic are up, and how long it is measured.
pub(super) const LONE_SETTLE: Duration = Duration::from_secs(15);
pub(super) const LONE_IDLE: Duration = Duration::from_secs(20);
/// Most times per second a lone desktop's own threads may wake while idle:
/// one room of its own, no peer, and no frame loop driving it, as on a
/// desktop nobody touches, whose compositor never ticks the network.
///
/// Measured on CI (focused, loopback): 2.79/s while gossip swept the room's
/// topic every second (run 91402), 0.95/s and 1.05/s once it sweeps every
/// 10 s (runs 91459, 91528), 0.90 to 1.05/s on main `f744af88ad` (runs
/// 92709-92713). Most of that was the 5 s upkeep tick and the sweeps of the
/// desktop's three topics, neither of which runs now while there is no peer:
/// 0.30 to 0.45/s (runs 92724, 92787-92796, 92833), 0.25 to
/// 0.40/s of it `tilo-net-rt`. A tracing probe (run 92833) names what is
/// left: two iroh timers that ran whatever Tilo did, netwatch's 15 s
/// wall-clock check and the socket's 20-26 s re-STUN tick, which with no
/// relays does nothing. In that run the two cost a worker eight switches;
/// tokio's timer wheel, which wakes at each level a far timer cascades
/// through, would account for most of them (not verified). The vendored
/// iroh and netwatch no longer run either (`vendor/iroh/TILO-PATCH.md`,
/// `vendor/netwatch/TILO-PATCH.md`): 0.35 to 0.40/s on main `7180919b15`
/// (runs 96742-96744), 0.20/s in all six runs after (96812, 96954-96958),
/// 0.15/s of it `tilo-net-rt` and 0.05/s the test thread's own `/proc`
/// read. What `tilo-net-rt` still does in this window, 15 to 35 s after
/// the desktop starts, is not periodic: the desktop whose collaborator left,
/// measured two minutes after it started, shows none (not attributed
/// further). The p95, 0.20/s, plus a quarter.
///
/// Counted as voluntary switches, one per wakeup, because preemptions
/// measure the runner: a loaded pull-request shard (run 92351, its cgroup at
/// `memory.max` 2832 times) preempted the same two `tilo-net-rt` workers
/// 1.35 times a second on top of the 0.95 times they blocked, and failed the
/// gate at 2.30/s with nothing in the session changed.
#[cfg(target_os = "linux")]
pub(super) const LONE_WAKEUP_BUDGET: f64 = 0.25;
/// Most CPU a lone idle desktop's threads may use, in percent of one core:
/// 100 ms over [`LONE_IDLE`]. The wakeup count leaves out preemptions, so
/// this is what still catches a thread that spins without blocking. Measured
/// 0.0 % (under one 10 ms tick in the window).
pub(super) const LONE_CPU_BUDGET: f64 = 0.5;

/// Set in the process a wakeup gate reruns itself in: `1` inside a network
/// namespace of its own, `host` on the host's network where the machine
/// allows no user namespaces.
#[cfg(target_os = "linux")]
const OWN_PROCESS: &str = "TILO_IDLE_OWN_PROCESS";

/// `program` run by `unshare` in a user and network namespace of its own.
#[cfg(target_os = "linux")]
fn unshare(program: &str, args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new("unshare");
    command
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg(program)
        .args(args);
    command
}

/// Run `test` (its path, `module_path!()` and name) again, alone in a
/// process of its own with only a loopback interface, and report that it
/// did.
///
/// A wakeup gate counts every thread of its process, and `cargo test` runs
/// a binary's tests as threads of one process: in a parallel run the gate
/// counted the threads of whichever tests ran beside it (a converged pair
/// measured 19.6 wakeups a second, most of them test threads), so it ran
/// alone by luck only. In a process of its own the gate's threads are the
/// desktops'.
///
/// A wakeup budget also measures what Tilo does while idle, and a host's
/// network is not Tilo. The kernel tells every netlink subscriber, which is
/// how iroh's network monitor learns of changes, about each address refresh
/// on the host: a router that advertises its prefix every 3 s on a wifi
/// network wakes the monitor 0.3 times a second by itself (0.70 to 0.75/s
/// measured on the Yoga against 0.40 to 0.45/s with nothing else on the
/// link). So the gate runs in a user and network namespace whose only link
/// is loopback, which is also all the test uses; where the machine allows
/// no user namespaces, alone on the host's network as it is.
///
/// Returns `true` when the child ran and passed (the caller is done) and
/// `false` when the caller is the child.
#[cfg(target_os = "linux")]
pub(super) fn in_a_process_of_its_own(test: &str) -> bool {
    if let Some(network) = std::env::var_os(OWN_PROCESS) {
        if network == "1" {
            bring_loopback_up();
        }
        return false;
    }
    let exe = std::env::current_exe().expect("this test binary's path");
    let exe = exe.to_str().expect("a UTF-8 path");
    let name = test_name(test);
    let args = ["--exact", name.as_str(), "--nocapture", "--test-threads=1"];
    let mut command = if unshare("true", &[]).status().is_ok_and(|s| s.success()) {
        let mut command = unshare(exe, &args);
        command.env(OWN_PROCESS, "1");
        command
    } else {
        eprintln!("no user namespaces here: measuring on the host's network");
        let mut command = std::process::Command::new(exe);
        command.args(args).env(OWN_PROCESS, "host");
        command
    };
    let output = command
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("start the test in a process of its own");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{name} failed in its own process: {}\n{stdout}",
        output.status
    );
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "{name} ran no test in its own process:\n{stdout}"
    );
    true
}

/// Whether this process is the rerun in a network namespace of its own.
#[cfg(target_os = "linux")]
fn in_private_network() -> bool {
    std::env::var_os(OWN_PROCESS).is_some_and(|network| network == "1")
}

/// libtest's name of `test`, a path from `module_path!()`: the path without
/// the crate's name.
#[cfg(target_os = "linux")]
fn test_name(test: &str) -> String {
    let (_, in_crate) = test.split_once("::").unwrap_or(("", test));
    in_crate.to_owned()
}

/// Bring the namespace's `lo` up (a new network namespace starts with it
/// down), which the endpoint's loopback address needs.
#[cfg(target_os = "linux")]
fn bring_loopback_up() {
    // SAFETY: a plain datagram socket; its descriptor is closed below.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
    assert!(
        fd >= 0,
        "open a socket: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: an all-zero ifreq is a valid value.
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (slot, byte) in request.ifr_name.iter_mut().zip(b"lo\0") {
        *slot = libc::c_char::try_from(*byte).unwrap_or(0);
    }
    // SAFETY: SIOCGIFFLAGS fills the flags of the one ifreq it is given.
    let got = unsafe { libc::ioctl(fd, libc::SIOCGIFFLAGS as _, &raw mut request) };
    assert_eq!(
        got,
        0,
        "read lo's flags: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: SIOCGIFFLAGS filled the flags member of the union.
    unsafe {
        request.ifr_ifru.ifru_flags |=
            libc::c_short::try_from(libc::IFF_UP).expect("IFF_UP fits the flags member");
    }
    // SAFETY: SIOCSIFFLAGS reads the one initialised ifreq it is given.
    let set = unsafe { libc::ioctl(fd, libc::SIOCSIFFLAGS as _, &raw const request) };
    assert_eq!(set, 0, "bring lo up: {}", std::io::Error::last_os_error());
    // SAFETY: `fd` is ours and unused after this.
    unsafe { libc::close(fd) };
}

/// What a router advertising its prefix every few seconds does to a host:
/// refreshes the lifetimes of an address it already has, which the kernel
/// announces (`RTM_NEWADDR`) to every netlink subscriber. Replaces `fd00::1`
/// on loopback with new lifetimes at each [`Self::refresh`]. Needs the
/// private network namespace's root, and speaks Linux's rtnetlink.
#[cfg(target_os = "linux")]
struct AddressRefresher {
    socket: libc::c_int,
    sent: u32,
}

#[cfg(target_os = "linux")]
impl AddressRefresher {
    fn new() -> Self {
        // SAFETY: a plain netlink socket, closed on drop.
        let socket = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW, libc::NETLINK_ROUTE) };
        assert!(
            socket >= 0,
            "open a netlink socket: {}",
            std::io::Error::last_os_error()
        );
        Self { socket, sent: 0 }
    }

    fn refresh(&mut self) {
        const RTM_NEWADDR: u16 = 20;
        const FLAGS: u16 = 0x1 | 0x4 | 0x100 | 0x400; // REQUEST | ACK | REPLACE | CREATE
        self.sent = self.sent.saturating_add(1);
        let lifetime = 100_000_u32.saturating_sub(self.sent);
        let address: [u8; 16] = std::net::Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1).octets();
        let mut message = Vec::with_capacity(84);
        message.extend_from_slice(&84_u32.to_ne_bytes()); // length
        message.extend_from_slice(&RTM_NEWADDR.to_ne_bytes());
        message.extend_from_slice(&FLAGS.to_ne_bytes());
        message.extend_from_slice(&self.sent.to_ne_bytes()); // sequence
        message.extend_from_slice(&0_u32.to_ne_bytes()); // port
        message.extend_from_slice(&[
            u8::try_from(libc::AF_INET6).expect("AF_INET6 fits the family byte"),
            128,
            0,
            0,
        ]); // ifaddrmsg
        message.extend_from_slice(&1_u32.to_ne_bytes()); // lo
        for kind in [2_u16, 1] {
            message.extend_from_slice(&20_u16.to_ne_bytes());
            message.extend_from_slice(&kind.to_ne_bytes());
            message.extend_from_slice(&address);
        }
        message.extend_from_slice(&20_u16.to_ne_bytes()); // IFA_CACHEINFO
        message.extend_from_slice(&6_u16.to_ne_bytes());
        for value in [lifetime, lifetime, 0, 0] {
            message.extend_from_slice(&value.to_ne_bytes());
        }
        let mut reply = [0_u8; 256];
        // SAFETY: sends `message` to the kernel and reads its acknowledgement
        // into `reply`, both alive for the calls.
        let answered = unsafe {
            libc::send(self.socket, message.as_ptr().cast(), message.len(), 0);
            libc::recv(self.socket, reply.as_mut_ptr().cast(), reply.len(), 0)
        };
        assert!(
            answered > 0,
            "the kernel answered: {}",
            std::io::Error::last_os_error()
        );
    }
}

#[cfg(target_os = "linux")]
impl Drop for AddressRefresher {
    fn drop(&mut self) {
        // SAFETY: the socket is ours and unused after this.
        unsafe { libc::close(self.socket) };
    }
}

/// Every thread of this process except the one driving the desktops'
/// frame loops, whose 10 Hz is the simulation's and not the session's.
pub(super) fn session_threads(driver: Option<u32>) -> Vec<tilo_test_env::idle::ThreadSample> {
    let mut threads = tilo_test_env::idle::sample_process(std::process::id())
        .expect("sample this process's threads");
    threads.retain(|thread| Some(thread.tid) != driver);
    threads
}

fn device(
    tree: WorkspaceTreeHandle,
    dir: &std::path::Path,
    root: &SigningKey,
    capabilities: DeviceCapabilities,
) -> Arc<ShellNetwork> {
    device_reaching(tree, dir, root, capabilities, Reach::Loopback)
}

/// [`device`] reaching peers as `reach` says.
pub(super) fn device_reaching(
    tree: WorkspaceTreeHandle,
    dir: &std::path::Path,
    root: &SigningKey,
    capabilities: DeviceCapabilities,
    reach: Reach,
) -> Arc<ShellNetwork> {
    let root = root.clone();
    Arc::new(
        ShellNetwork::spawn_with(
            tree,
            &dir.join("device-private/keypair.bin"),
            &dir.join("peer-sync"),
            reach,
            move |secret, _| {
                let key = SigningKey::from_bytes(&secret.to_bytes());
                let mut cert = DeviceCert {
                    user_public_key: root.verifying_key().to_bytes(),
                    device_public_key: key.verifying_key().to_bytes(),
                    device_label: "Device".into(),
                    capabilities,
                    issued_at_ns: 1,
                    expires_at_ns: None,
                    signature: [0; 64],
                };
                sign_device_cert(&mut cert, &root).unwrap();
                Some(HostIdentity {
                    device_cert: cert,
                    device_signing_key: key,
                    workspace_device_certs: std::collections::HashMap::new(),
                    workspace_revocations: std::collections::HashMap::new(),
                    identity_paths: None,
                })
            },
        )
        .unwrap(),
    )
}

/// One desktop as its frame loop drives it.
struct Desktop {
    net: Arc<ShellNetwork>,
    tree: WorkspaceTreeHandle,
    presence: PeerPresence,
    ctx: PresenceCtx,
    desk: Desk,
    repaints: u64,
}

impl Desktop {
    fn new(net: Arc<ShellNetwork>, tree: WorkspaceTreeHandle) -> Self {
        let presence = PeerPresence::for_network(&net, Vec::new());
        Self {
            net,
            tree,
            presence,
            ctx: PresenceCtx::default(),
            desk: Desk {
                tiles: vec![(SHARED_TAB, vello::kurbo::Rect::new(0.0, 0.0, 800.0, 600.0))],
                pointer: None,
                strip: None,
            },
            repaints: 0,
        }
    }

    /// One frame: the network tick, then pointer presence, as the native
    /// compositor runs them.
    fn frame(&mut self) {
        self.net.tick();
        if self.presence.frame(&mut self.ctx, &self.desk, &self.tree) {
            self.repaints = self.repaints.saturating_add(1);
        }
    }
}

const SHARED_TAB: u64 = 7;

fn workspace_name(tree: &WorkspaceTreeHandle, room: RoomId) -> Option<String> {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.name.clone())
}

/// Run both desktops' frame loops for `span`, or until `done` holds.
fn run(
    desktops: &mut [&mut Desktop],
    span: Duration,
    mut done: impl FnMut(&[&mut Desktop]) -> bool,
) -> bool {
    let started = Instant::now();
    let deadline = started.checked_add(span).unwrap_or(started);
    loop {
        for desktop in desktops.iter_mut() {
            desktop.frame();
        }
        if done(&*desktops) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(FRAME);
    }
}

fn received(net: &ShellNetwork) -> BTreeMap<&'static str, u64> {
    net.wire_tally()
        .expect("the actor answers for its tally")
        .snapshot()
}

/// What arrived between two tallies, by kind.
fn since(
    before: &BTreeMap<&'static str, u64>,
    after: &BTreeMap<&'static str, u64>,
) -> BTreeMap<&'static str, u64> {
    after
        .iter()
        .filter_map(|(kind, count)| {
            let delta = count.saturating_sub(before.get(kind).copied().unwrap_or(0));
            (delta > 0).then_some((*kind, delta))
        })
        .collect()
}

fn room_change_count(tree: &WorkspaceTreeHandle, room: RoomId) -> usize {
    tree.write()
        .room_docs
        .get_mut(&room)
        .map_or(0, |doc| doc.get_changes(&[]).len())
}

/// What `room`'s document gained on `tree` since it held `before` changes:
/// the newest few, so a report names what an idle desktop keeps writing.
fn newest_room_changes(tree: &WorkspaceTreeHandle, room: RoomId, before: usize) -> String {
    let Some(changes) = tree
        .write()
        .room_docs
        .get_mut(&room)
        .map(|doc| doc.get_changes(&[]))
    else {
        return "none".into();
    };
    let mut out = format!(
        "{} changes while idle",
        changes.len().saturating_sub(before)
    );
    for change in changes.iter().skip(before).rev().take(3) {
        let change = change.decode();
        let ops: String = serde_json::to_string(&change.operations)
            .unwrap_or_default()
            .chars()
            .take(800)
            .collect();
        let _ = write!(
            out,
            "\n    by {} at {}: {ops}",
            change.actor_id, change.time
        );
    }
    out
}

#[test]
#[serial_test::serial]
fn a_converged_idle_pair_exchanges_no_messages_and_repaints_nothing_for_a_minute() {
    #[cfg(target_os = "linux")]
    if in_a_process_of_its_own(concat!(
        module_path!(),
        "::a_converged_idle_pair_exchanges_no_messages_and_repaints_nothing_for_a_minute"
    )) {
        return;
    }
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[43; 32]);
    let person = match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    };
    home.env().set("TILO_USER_ID", &person);

    let mut first_tree = WorkspaceTree::default();
    let home_workspace = first_tree.create_hosted_workspace("Home").0;
    first_tree.own_workspaces_for_test(|_| person.clone());
    // The pointer is said in the room on screen.
    first_tree.desktop.active = home_workspace;
    let room = first_tree.room_of(home_workspace).unwrap();
    let first_tree = first_tree.shared();
    let first_net = device(
        first_tree.clone(),
        &dir.path().join("first"),
        &root,
        DeviceCapabilities::primary(),
    );
    assert!(first_net.join_default_workspace());
    let mut second_tree = WorkspaceTree::default();
    assert!(second_tree.join_enrolled_room(room));
    let second_tree = second_tree.shared();
    let second_net = device(
        second_tree.clone(),
        &dir.path().join("second"),
        &root,
        DeviceCapabilities::member(),
    );
    assert!(second_net.join_default_workspace());
    // What each person chose to show there, as the desktop says on start:
    // until it does, nothing of them leaves.
    for net in [&first_net, &second_net] {
        net.set_presence_visibility(&room.to_string(), ui::PresenceVisibility::WhereIAm);
    }

    let mut first = Desktop::new(Arc::clone(&first_net), first_tree);
    let mut second = Desktop::new(Arc::clone(&second_net), second_tree.clone());
    let deadline = Instant::now() + tilo_test_env::hang_bound();
    let first_addr = loop {
        if let Some(addr) = first_net.dial_addr() {
            break addr;
        }
        assert!(Instant::now() < deadline, "the first device never listened");
        std::thread::sleep(Duration::from_millis(20));
    };
    second_net
        .cmd_tx
        .try_send(NetCmd::Connect { addr: first_addr })
        .unwrap();
    assert!(
        run(
            &mut [&mut first, &mut second],
            tilo_test_env::hang_bound(),
            |_| { workspace_name(&second_tree, room).as_deref() == Some("Home") }
        ),
        "the two desktops never converged on the shared room"
    );

    // The first desktop's person points at the shared tab, the second sees
    // it arrive, and then the hand comes off the mouse.
    first.desk.pointer = Some((120.0, 80.0));
    let before = second.repaints;
    assert!(
        run(
            &mut [&mut first, &mut second],
            tilo_test_env::hang_bound(),
            |desktops| desktops[1].repaints > before,
        ),
        "the peer never drew the pointer"
    );
    let _ = run(&mut [&mut first, &mut second], SETTLE, |_| false);

    let first_before = received(&first_net);
    let second_before = received(&second_net);
    let repaints_before = (first.repaints, second.repaints);
    let room_changes_before = (
        room_change_count(&first.tree, room),
        room_change_count(&second.tree, room),
    );
    // Wakeups are read from /proc, so only where there is one.
    let linux = cfg!(target_os = "linux");
    let driver = linux.then(|| tilo_test_env::idle::current_thread_id().expect("this thread's id"));
    let threads_before = linux.then(|| session_threads(driver));
    let started = Instant::now();
    let _ = run(&mut [&mut first, &mut second], IDLE, |_| false);
    let measured = started.elapsed();
    let wakeups = threads_before.map(|before| {
        tilo_test_env::idle::Wakeups::between(before, session_threads(driver), measured)
    });
    let first_idle = since(&first_before, &received(&first_net));
    let second_idle = since(&second_before, &received(&second_net));
    let repaints = (
        first.repaints - repaints_before.0,
        second.repaints - repaints_before.1,
    );

    let per_minute = |count: u64| {
        f64::from(u32::try_from(count).unwrap_or(u32::MAX)) * 60.0 / measured.as_secs_f64()
    };
    let mut report = format!("idle two-desktop session over {measured:.1?}:\n");
    let mut total = 0;
    for (name, idle) in [("first", &first_idle), ("second", &second_idle)] {
        for (kind, count) in idle {
            total += count;
            let _ = writeln!(
                report,
                "  {name} received {count} {kind} ({:.1}/min)",
                per_minute(*count)
            );
        }
    }
    let _ = writeln!(
        report,
        "  total {total} application messages ({:.1}/min); repaints first {} second {}",
        per_minute(total),
        repaints.0,
        repaints.1,
    );
    for (name, tree, before) in [
        ("first", &first.tree, room_changes_before.0),
        ("second", &second.tree, room_changes_before.1),
    ] {
        let _ = writeln!(
            report,
            "  {name}'s room document: {}",
            newest_room_changes(tree, room, before)
        );
    }
    if let Some(wakeups) = &wakeups {
        let _ = writeln!(
            report,
            "both desktops' threads, {wakeups}  blocked {:.2} times a second",
            wakeups.blocking_per_second()
        );
    }
    eprintln!("{report}");
    assert_eq!(total, 0, "{report}");
    if let Some(wakeups) = &wakeups {
        assert!(
            wakeups.blocking_per_second() <= IDLE_WAKEUP_BUDGET,
            "the idle pair's threads woke {:.2} times a second, over {IDLE_WAKEUP_BUDGET}/s:\n{report}",
            wakeups.blocking_per_second(),
        );
        assert!(
            wakeups.total_cpu_percent() <= IDLE_CPU_BUDGET,
            "the idle pair's threads used {:.2} % of a core, over {IDLE_CPU_BUDGET} %:\n{report}",
            wakeups.total_cpu_percent(),
        );
    }
    assert_eq!(repaints, (0, 0), "{report}");
    drop((first, second, first_net, second_net));
}

/// A desktop alone with a room of its own, as most are, wakes nothing while
/// nobody uses it: no peer to keep alive, nothing to sync, and its
/// compositor parked, so not even a frame tick reaches the network. Its
/// relays wait the way an installed desktop's do: nobody could
/// dial it, so it neither probes them nor holds a home relay open.
///
/// Linux only: its router simulation speaks rtnetlink and names its thread
/// by `gettid`.
#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial]
fn a_lone_idle_desktop_keeps_its_network_runtime_asleep() {
    if in_a_process_of_its_own(concat!(
        module_path!(),
        "::a_lone_idle_desktop_keeps_its_network_runtime_asleep"
    )) {
        return;
    }
    // The address exists before the desktop does, as a router's does.
    let refresher = in_private_network().then(|| {
        let mut refresher = AddressRefresher::new();
        refresher.refresh();
        refresher
    });
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = SigningKey::from_bytes(&[44; 32]);
    let person = match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("a root key names a person: {other:?}"),
    };
    home.env().set("TILO_USER_ID", &person);
    let mut tree = WorkspaceTree::default();
    let workspace = tree.create_hosted_workspace("Home").0;
    tree.own_workspaces_for_test(|_| person.clone());
    tree.desktop.active = workspace;
    let net = device_reaching(
        tree.shared(),
        &dir.path().join("lone"),
        &root,
        DeviceCapabilities::primary(),
        Reach::LoopbackHoldingRelays,
    );
    assert!(net.join_default_workspace());
    let deadline = Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .unwrap();
    while net.dial_addr().is_none() {
        assert!(Instant::now() < deadline, "the desktop never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(LONE_SETTLE);

    // In the private network, a router refreshes an address's lifetimes
    // five times a second, far oftener than a real one: the desktop must
    // sleep through it. That thread is the simulation's, not the session's.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let refresher = refresher.map(|mut refresher| {
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            // SAFETY: gettid has no arguments and cannot fail.
            let tid = unsafe { libc::syscall(libc::SYS_gettid) };
            tid_tx.send(u32::try_from(tid).unwrap_or(0)).unwrap();
            while !stop.load(std::sync::atomic::Ordering::Acquire) {
                refresher.refresh();
                std::thread::sleep(Duration::from_millis(200));
            }
            refresher.sent
        });
        (tid_rx.recv().unwrap(), thread)
    });
    let driver = refresher.as_ref().map(|(tid, _)| *tid);
    std::thread::sleep(Duration::from_millis(500));

    // Nothing drives this desktop, so every thread of the process counts.
    let before = session_threads(driver);
    let started = Instant::now();
    std::thread::sleep(LONE_IDLE);
    let measured = started.elapsed();
    let wakeups = tilo_test_env::idle::Wakeups::between(before, session_threads(driver), measured);
    stop.store(true, std::sync::atomic::Ordering::Release);
    if let Some((_, thread)) = refresher {
        let refreshes = thread.join().unwrap();
        eprintln!("{refreshes} address lifetime refreshes during the run");
        assert!(refreshes >= 50, "the router simulation ran: {refreshes}");
    }
    eprintln!("a lone idle desktop's threads, {wakeups}");
    eprintln!(
        "a lone idle desktop's threads blocked {:.2} times a second",
        wakeups.blocking_per_second()
    );
    assert!(
        wakeups.blocking_per_second() <= LONE_WAKEUP_BUDGET,
        "a lone idle desktop woke {:.2} times a second, over {LONE_WAKEUP_BUDGET}/s:\n{wakeups}",
        wakeups.blocking_per_second(),
    );
    assert!(
        wakeups.total_cpu_percent() <= LONE_CPU_BUDGET,
        "a lone idle desktop used {:.2} % of a core, over {LONE_CPU_BUDGET} %:\n{wakeups}",
        wakeups.total_cpu_percent(),
    );
    assert!(
        net.reachability.is_dormant(),
        "a desktop nobody could dial started its relays"
    );
    drop(net);
}
