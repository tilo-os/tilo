//! The vendored iroh and netwatch patches (see each crate's
//! `vendor/*/TILO-PATCH.md`): an idle endpoint wakes for nothing, and still
//! hears at once of a network change.
//!
//! Every Tilo process with an endpoint (the compositor, `fs serve`,
//! `fs replica`) used to wake on three iroh timers that had
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

#[cfg(target_os = "linux")]
const IDLE_WINDOW: Duration = Duration::from_secs(30);

#[cfg(target_os = "linux")]
const SETTLED: Duration = Duration::from_secs(3);

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

#[cfg(target_os = "linux")]
#[test]
fn an_endpoint_whose_relays_were_removed_sleeps() {
    use iroh::Watcher as _;

    let relay_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("relay-server")
        .enable_all()
        .build()
        .expect("a runtime for the relay");
    let relay = relay_runtime.block_on(tilo_test_env::relay::relay());
    let url = relay.url().clone();

    let runtime = endpoint_runtime();
    let endpoint = runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(relay.relays()))
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
    let server = relay.server();
    tilo_test_env::wait_until("the relay to count the endpoint's connection", || {
        server.metrics().server.accepts.get() >= 1
    });
    let accepts = server.metrics().server.accepts.get();

    runtime.block_on(endpoint.remove_relay(&url));
    tilo_test_env::wait_until("the endpoint to give up its home relay", || {
        endpoint.home_relay_status().get().is_empty()
    });
    tilo_test_env::wait_until("the relay connection to close", || {
        server.metrics().server.disconnects.get() >= accepts
    });

    let threads = || {
        let mut threads = tilo_test_env::idle::sample_process(std::process::id())
            .expect("this process's threads");
        threads.retain(|thread| thread.comm.starts_with(ENDPOINT_THREADS));
        threads
    };
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
    drop(relay);
}

#[cfg(target_os = "linux")]
#[test]
fn an_unchanged_net_report_rests_the_next() {
    const RESTING: Duration = Duration::from_secs(75);

    let relay_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("relay-server")
        .enable_all()
        .build()
        .expect("a runtime for the relay");
    let relay = relay_runtime.block_on(tilo_test_env::relay::relay());

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("net-report-rest")
        .enable_all()
        .build()
        .expect("a runtime for the endpoint");
    let endpoint = runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(relay.relays()))
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
    drop(relay);
}

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
    use tilo_test_env::SpawnUnderEnvLock as _;

    const CHILD: &str = "TILO_IROH_NETNS_CHILD";

    pub(super) fn in_child() -> bool {
        std::env::var_os(CHILD).is_some()
    }

    pub(super) fn rerun_in_private_network(name: &str) {
        let output = std::process::Command::new("unshare")
            .args(["--user", "--map-root-user", "--net", "--"])
            .arg(std::env::current_exe().expect("this test binary"))
            .args([name, "--nocapture", "--test-threads=1"])
            .env(CHILD, "1")
            .output_locked()
            .expect("util-linux unshare runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("{stdout}\n{stderr}");
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{name} did not pass in its own network namespace"
        );
    }

    const REFRESHES: u32 = 50;
    const REFRESH_EVERY: std::time::Duration = std::time::Duration::from_millis(200);

    pub(super) fn refreshes_do_not_wake_the_endpoint() {
        set_up("lo", true);
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

    struct Refresher {
        socket: libc::c_int,
        sent: u32,
    }

    impl Refresher {
        fn new() -> Self {
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
            unsafe { libc::close(self.socket) };
        }
    }

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

    fn noticed(what: &str, seen: impl FnMut() -> bool) {
        let changed = Instant::now();
        tilo_test_env::wait_until(format_args!("the endpoint to notice {what}"), seen);
        eprintln!("noticed {what} after {} ms", changed.elapsed().as_millis());
    }

    fn request(name: &str) -> libc::ifreq {
        let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
        assert!(name.len() < request.ifr_name.len(), "interface name fits");
        for (slot, byte) in request.ifr_name.iter_mut().zip(name.bytes()) {
            *slot = libc::c_char::from_ne_bytes([byte]);
        }
        request
    }

    fn ioctl(request: &mut libc::ifreq, op: libc::Ioctl) {
        let sock = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        assert!(sock >= 0, "a socket: {}", std::io::Error::last_os_error());
        let res = unsafe { libc::ioctl(sock, op, std::ptr::from_mut(request)) };
        let error = std::io::Error::last_os_error();
        unsafe { libc::close(sock) };
        assert!(res >= 0, "interface ioctl {op}: {error}");
    }

    fn set_up(name: &str, up: bool) {
        let mut request = request(name);
        ioctl(&mut request, libc::SIOCGIFFLAGS);
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
        let mut sin: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        sin.sin_family = libc::sa_family_t::try_from(libc::AF_INET).expect("AF_INET fits");
        sin.sin_addr.s_addr = u32::from_ne_bytes(addr.octets());
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
