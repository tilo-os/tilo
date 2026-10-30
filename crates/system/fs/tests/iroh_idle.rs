//! The vendored iroh and netwatch patches (see each crate's
//! `vendor/*/TILO-PATCH.md`): an idle endpoint wakes for nothing, and still
//! hears at once of a network change.
//!
//! Every Tilo process with an endpoint (the compositor, `fs serve`,
//! `fs replica`, `fs mount-share`) used to wake on three iroh timers that had
//! nothing to do: netwatch's 15 s wall-clock poll, the socket's 20-26 s
//! re-STUN tick with no relays, and the 15 s ping of a home relay kept after
//! the relays were removed. With relays, the
//! net report re-ran every 20-26 s however often it found the same thing.

#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

/// Long enough to span each removed period at least once: the 15 s
/// wall-clock poll twice and the 20-26 s re-STUN tick once.
#[cfg(target_os = "linux")]
const IDLE_WINDOW: Duration = Duration::from_secs(30);

/// How long the endpoint's runtime must have been still before an idle
/// window opens: well under the shortest removed period (15 s).
#[cfg(target_os = "linux")]
const SETTLED: Duration = Duration::from_secs(3);

/// The endpoint's own runtime threads, so the test's and the relay's are
/// not counted.
#[cfg(target_os = "linux")]
const ENDPOINT_THREADS: &str = "iroh-idle";

#[cfg(target_os = "linux")]
fn endpoint_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name(ENDPOINT_THREADS)
        .enable_all()
        .build()
        .expect("a runtime for the endpoint")
}

/// An endpoint that had a home relay, once its relays are removed, closes
/// that connection and then wakes for nothing at all: no re-STUN tick, no
/// relay ping, no clock poll.
#[cfg(target_os = "linux")]
#[test]
fn an_endpoint_whose_relays_were_removed_sleeps() {
    use iroh::Watcher as _;
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};

    let relay_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("relay-server")
        .enable_all()
        .build()
        .expect("a runtime for the relay");
    let server = relay_runtime.block_on(async {
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new((std::net::Ipv4Addr::LOCALHOST, 0)));
        Server::spawn(config)
            .await
            .expect("a loopback relay starts")
    });
    let url: iroh::RelayUrl = format!("http://{}", server.http_addr().expect("relay HTTP"))
        .parse()
        .expect("a loopback relay URL");
    let mut relay = iroh::RelayConfig::from(url.clone());
    // No QUIC address discovery: the net report needs only the HTTP probe.
    relay.quic = None;

    let runtime = endpoint_runtime();
    let endpoint = runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(std::iter::once(relay).collect()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback bind address")
            .bind()
            .await
            .expect("the endpoint binds");
        tilo_test_env::within(
            "the endpoint to connect to its home relay",
            endpoint.online(),
        )
        .await;
        endpoint
    });
    // The relay counts the connection once it has registered the client,
    // which may be just after the client saw itself connected.
    tilo_test_env::wait_until("the relay to count the endpoint's connection", || {
        server.metrics().server.accepts.get() >= 1
    });
    let accepts = server.metrics().server.accepts.get();

    runtime.block_on(endpoint.remove_relay(&url));
    tilo_test_env::wait_until("the endpoint to give up its home relay", || {
        endpoint.home_relay_status().get().is_empty()
    });
    // The released relay closes on iroh's inactivity rule, a minute after
    // it last carried data at the latest.
    tilo_test_env::wait_until("the relay connection to close", || {
        server.metrics().server.disconnects.get() >= accepts
    });

    let threads = || {
        let mut threads = tilo_test_env::idle::sample_process(std::process::id())
            .expect("this process's threads");
        threads.retain(|thread| thread.comm.starts_with(ENDPOINT_THREADS));
        threads
    };
    // The relay counts the disconnect as soon as the endpoint's relay task
    // drops the connection, and the endpoint reaps that task a moment
    // later on its own runtime; on a loaded machine that last wake can fall
    // inside the window. Start the window once the runtime has been still
    // for a few seconds: shorter than any of the periods the window must
    // catch, so a periodic wake still shows within it.
    tilo_test_env::wait_until("the endpoint's runtime to finish closing the relay", || {
        let before = threads();
        let started = std::time::Instant::now();
        std::thread::sleep(SETTLED);
        tilo_test_env::idle::Wakeups::between(before, threads(), started.elapsed()).blocking() == 0
    });
    let before = threads();
    let started = std::time::Instant::now();
    std::thread::sleep(IDLE_WINDOW);
    let wakeups = tilo_test_env::idle::Wakeups::between(before, threads(), started.elapsed());
    eprintln!("an endpoint whose relays were removed: {wakeups}");
    assert_eq!(
        wakeups.blocking(),
        0,
        "an idle endpoint with no relays woke its runtime:\n{wakeups}"
    );
    assert!(
        endpoint.addr().relay_urls().next().is_none(),
        "the endpoint still names the relay it gave up"
    );
    runtime.block_on(endpoint.close());
    drop(server);
}

/// After a net report finds what the one before did, the next waits four
/// times the 20-26 s period, so an idle endpoint with relays stops probing
/// them every 20-26 s for as long as it lives.
#[cfg(target_os = "linux")]
#[test]
fn an_unchanged_net_report_rests_the_next() {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};

    /// Three of the 20-26 s periods; a resting report waits 80-104 s.
    const RESTING: Duration = Duration::from_secs(75);

    let relay_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("relay-server")
        .enable_all()
        .build()
        .expect("a runtime for the relay");
    let server = relay_runtime.block_on(async {
        let mut config = ServerConfig::default();
        config.relay = Some(RelayConfig::new((std::net::Ipv4Addr::LOCALHOST, 0)));
        Server::spawn(config)
            .await
            .expect("a loopback relay starts")
    });
    let url: iroh::RelayUrl = format!("http://{}", server.http_addr().expect("relay HTTP"))
        .parse()
        .expect("a loopback relay URL");
    let mut relay = iroh::RelayConfig::from(url);
    relay.quic = None;

    // Its own thread name: the wakeup counts of the other tests here are
    // taken by name, and the test harness may run this beside them.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("net-report-rest")
        .enable_all()
        .build()
        .expect("a runtime for the endpoint");
    let endpoint = runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(std::iter::once(relay).collect()))
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback bind address")
            .bind()
            .await
            .expect("the endpoint binds");
        tilo_test_env::within(
            "the endpoint to connect to its home relay",
            endpoint.online(),
        )
        .await;
        endpoint
    });
    let reports = || endpoint.metrics().net_report.reports.get();
    // The report at bind runs before there is a home relay, the first
    // periodic one finds it, and the second finds the same as the first.
    tilo_test_env::wait_until("two periodic net reports", || reports() >= 3);
    let before = reports();
    std::thread::sleep(RESTING);
    let during = reports() - before;
    eprintln!(
        "net reports in the {} s after an unchanged one: {during}",
        RESTING.as_secs()
    );
    assert_eq!(
        during, 0,
        "an unchanged net report must rest the next for 80-104 s"
    );
    runtime.block_on(endpoint.close());
    drop(server);
}

/// A network change (an interface coming up, an address added and removed,
/// as when Wi-Fi joins or a VPN goes up and down) reaches the endpoint's
/// addresses through netlink, with no clock poll to fall back on. Runs itself again inside a user and network namespace of
/// its own, where it may change `lo`.
#[cfg(target_os = "linux")]
#[test]
fn an_endpoint_hears_of_network_changes_through_netlink() {
    const NAME: &str = "an_endpoint_hears_of_network_changes_through_netlink";
    if netns::in_child() {
        netns::changes_reach_the_endpoint();
        return;
    }
    netns::rerun_in_private_network(NAME);
}

/// A router that advertises its prefix every few seconds makes the kernel
/// announce the same address again with new lifetimes to every netlink
/// subscriber, and an endpoint's network monitor is one. Nothing changed, so
/// the endpoint must not wake for it: the kernel drops these messages before
/// they reach the monitor's socket. Here the lifetimes are refreshed five times
/// a second, in a network namespace of its own, and the endpoint's threads
/// still do not wake. Runs itself again inside a user and network namespace.
#[cfg(target_os = "linux")]
#[test]
fn an_endpoint_sleeps_through_address_lifetime_refreshes() {
    const NAME: &str = "an_endpoint_sleeps_through_address_lifetime_refreshes";
    if netns::in_child() {
        netns::refreshes_do_not_wake_the_endpoint();
        return;
    }
    netns::rerun_in_private_network(NAME);
}

#[cfg(target_os = "linux")]
mod netns {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Instant,
    };

    const CHILD: &str = "TILO_IROH_NETNS_CHILD";

    pub(super) fn in_child() -> bool {
        std::env::var_os(CHILD).is_some()
    }

    /// Run test `name` again in a user and network namespace of its own, where
    /// it may change `lo`, and require that it passed.
    pub(super) fn rerun_in_private_network(name: &str) {
        let output = std::process::Command::new("unshare")
            .args(["--user", "--map-root-user", "--net", "--"])
            .arg(std::env::current_exe().expect("this test binary"))
            .args(["--exact", name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output()
            .expect("util-linux unshare runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{stdout}\n{stderr}");
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{name} did not pass in its own network namespace"
        );
    }

    /// The refreshes the endpoint is watched through, and the gap between
    /// them: ten seconds of a router's advertisements, roughly.
    const REFRESHES: u32 = 50;
    const REFRESH_EVERY: std::time::Duration = std::time::Duration::from_millis(200);

    pub(super) fn refreshes_do_not_wake_the_endpoint() {
        set_up("lo", true);
        // The address exists before the endpoint does, as a router's does.
        let mut refresher = Refresher::new();
        refresher.refresh();
        let runtime = super::endpoint_runtime();
        let endpoint = runtime.block_on(async {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .bind()
                .await
                .expect("the endpoint binds")
        });
        std::thread::sleep(std::time::Duration::from_secs(1));

        let threads = || {
            let mut threads = tilo_test_env::idle::sample_process(std::process::id())
                .expect("this process's threads");
            threads.retain(|thread| thread.comm.starts_with(super::ENDPOINT_THREADS));
            threads
        };
        // The window lasts as long as the router takes for its refreshes,
        // however slowly they go out.
        let sent_before = refresher.sent;
        let before = threads();
        let started = Instant::now();
        for _ in 0..REFRESHES {
            refresher.refresh();
            std::thread::sleep(REFRESH_EVERY);
        }
        let wakeups = tilo_test_env::idle::Wakeups::between(before, threads(), started.elapsed());
        let sent = refresher.sent - sent_before;
        eprintln!("{sent} lifetime refreshes; the endpoint: {wakeups}");
        assert_eq!(sent, REFRESHES, "the router simulation ran");
        assert_eq!(
            wakeups.blocking(),
            0,
            "address lifetime refreshes woke the endpoint:\n{wakeups}"
        );
        runtime.block_on(endpoint.close());
    }

    /// Refreshes the lifetimes of `fd00::1` on `lo`, as a router's
    /// advertisements do to an address derived from its prefix: an
    /// `RTM_NEWADDR` that replaces the address with the same one.
    struct Refresher {
        socket: libc::c_int,
        sent: u32,
    }

    impl Refresher {
        fn new() -> Self {
            // SAFETY: a plain netlink socket, closed on drop.
            let socket = unsafe {
                libc::socket(
                    libc::AF_NETLINK,
                    libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                    libc::NETLINK_ROUTE,
                )
            };
            assert!(
                socket >= 0,
                "a netlink socket: {}",
                std::io::Error::last_os_error()
            );
            Self { socket, sent: 0 }
        }

        fn refresh(&mut self) {
            const RTM_NEWADDR: u16 = 20;
            // REQUEST | ACK | REPLACE | CREATE
            const FLAGS: u16 = 0x1 | 0x4 | 0x100 | 0x400;
            self.sent += 1;
            let lifetime = 100_000_u32.saturating_sub(self.sent);
            let address = std::net::Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1).octets();
            let mut message = Vec::with_capacity(84);
            message.extend_from_slice(&84_u32.to_ne_bytes());
            message.extend_from_slice(&RTM_NEWADDR.to_ne_bytes());
            message.extend_from_slice(&FLAGS.to_ne_bytes());
            message.extend_from_slice(&self.sent.to_ne_bytes());
            message.extend_from_slice(&0_u32.to_ne_bytes());
            let family = u8::try_from(libc::AF_INET6).expect("AF_INET6 fits");
            message.extend_from_slice(&[family, 128, 0, 0]);
            message.extend_from_slice(&1_u32.to_ne_bytes());
            for kind in [2_u16, 1] {
                message.extend_from_slice(&20_u16.to_ne_bytes());
                message.extend_from_slice(&kind.to_ne_bytes());
                message.extend_from_slice(&address);
            }
            message.extend_from_slice(&20_u16.to_ne_bytes());
            message.extend_from_slice(&6_u16.to_ne_bytes()); // IFA_CACHEINFO
            for value in [lifetime, lifetime, 0, 0] {
                message.extend_from_slice(&value.to_ne_bytes());
            }
            let mut reply = [0_u8; 256];
            // SAFETY: sends `message` to the kernel and reads its answer into
            // `reply`, both alive for the calls.
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

    impl Drop for Refresher {
        fn drop(&mut self) {
            // SAFETY: the socket is ours and unused after this.
            unsafe { libc::close(self.socket) };
        }
    }

    /// An address of its own, added to `lo` under a label.
    const ALIAS: &str = "lo:7";
    const ALIAS_ADDR: Ipv4Addr = Ipv4Addr::new(10, 77, 0, 7);

    pub(super) fn changes_reach_the_endpoint() {
        let runtime = super::endpoint_runtime();
        let endpoint = runtime.block_on(async {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .bind()
                .await
                .expect("the endpoint binds")
        });
        let has = |ip: IpAddr| endpoint.addr().ip_addrs().any(|addr| addr.ip() == ip);
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(
            !has(loopback),
            "a fresh network namespace starts with lo down"
        );

        set_up("lo", true);
        noticed("lo coming up", || has(loopback));

        add_address(ALIAS, ALIAS_ADDR);
        set_up(ALIAS, true);
        noticed("an address added", || has(IpAddr::V4(ALIAS_ADDR)));

        set_up(ALIAS, false);
        noticed("the address removed", || !has(IpAddr::V4(ALIAS_ADDR)));

        runtime.block_on(endpoint.close());
    }

    /// Waits for the endpoint to see a change. That netlink carries it, and
    /// not the clock poll netwatch no longer runs, is what this file's idle
    /// tests show: with no poll, nothing else could.
    fn noticed(what: &str, seen: impl FnMut() -> bool) {
        let changed = Instant::now();
        tilo_test_env::wait_until(format_args!("the endpoint to notice {what}"), seen);
        eprintln!("noticed {what} after {} ms", changed.elapsed().as_millis());
    }

    fn request(name: &str) -> libc::ifreq {
        // SAFETY: `ifreq` is plain data; all zeroes is a valid value.
        let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
        assert!(name.len() < request.ifr_name.len(), "interface name fits");
        for (slot, byte) in request.ifr_name.iter_mut().zip(name.bytes()) {
            *slot = libc::c_char::from_ne_bytes([byte]);
        }
        request
    }

    fn ioctl(request: &mut libc::ifreq, op: libc::Ioctl) {
        // SAFETY: a plain socket for interface ioctls, closed below.
        let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        assert!(sock >= 0, "a socket: {}", std::io::Error::last_os_error());
        // SAFETY: `request` is a valid `ifreq` for these interface ioctls.
        let res = unsafe { libc::ioctl(sock, op, std::ptr::from_mut(request)) };
        let error = std::io::Error::last_os_error();
        // SAFETY: `sock` is open and owned here.
        unsafe { libc::close(sock) };
        assert!(res >= 0, "interface ioctl {op}: {error}");
    }

    fn set_up(name: &str, up: bool) {
        let mut request = request(name);
        ioctl(&mut request, libc::SIOCGIFFLAGS);
        // SAFETY: SIOCGIFFLAGS filled the flags member.
        let flags = unsafe { request.ifr_ifru.ifru_flags };
        let up_flag = libc::c_short::try_from(libc::IFF_UP).expect("IFF_UP is a short flag");
        request.ifr_ifru.ifru_flags = if up {
            flags | up_flag
        } else {
            flags & !up_flag
        };
        ioctl(&mut request, libc::SIOCSIFFLAGS);
    }

    fn add_address(name: &str, addr: Ipv4Addr) {
        let mut request = request(name);
        // SAFETY: `sockaddr_in` is plain data; all zeroes is a valid value.
        let mut sin: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        sin.sin_family = libc::sa_family_t::try_from(libc::AF_INET).expect("AF_INET fits");
        sin.sin_addr.s_addr = u32::from_ne_bytes(addr.octets());
        // SAFETY: `sockaddr_in` fits in the `sockaddr` member and both are
        // plain data.
        unsafe {
            std::ptr::copy_nonoverlapping(
                std::ptr::from_ref(&sin).cast::<u8>(),
                std::ptr::from_mut(&mut request.ifr_ifru.ifru_addr).cast::<u8>(),
                std::mem::size_of::<libc::sockaddr_in>(),
            );
        }
        ioctl(&mut request, libc::SIOCSIFADDR);
    }
}
