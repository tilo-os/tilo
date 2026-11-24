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
//!
//! The two tests of one endpoint move the endpoint's Tokio clock instead of
//! waiting the 20-26 s, 80-104 s and five-minute windows out; the network
//! namespace tests below still watch a real process.

#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::Duration;

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

/// The endpoint's Tokio clock, moved by the test instead of waited out.
///
/// `tokio::time::pause` stops the clock and [`Clock::advance`] moves it, so a
/// wait of 20 s or five minutes passes at once. A paused runtime also advances
/// the clock by itself whenever it has nothing to do, which would fire the very
/// timer a test is about to look for; a blocking task holds that off for the
/// clock's life. A net report keeps real time ([`Clock::resume`]) because its
/// own probes measure, and time out, on the clock.
#[cfg(target_os = "linux")]
mod clock {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    /// The longest wait iroh's periodic net report picks (20-26 s).
    pub(super) const MAX_PERIOD: Duration = Duration::from_secs(26);
    /// How far the test moves the clock at a time.
    pub(super) const STEP: Duration = Duration::from_secs(5);
    /// The real time the endpoint gets to answer what the clock just started.
    pub(super) const SETTLE: Duration = Duration::from_millis(5);
    /// The real time a net report gets to finish.
    pub(super) const REPORT: Duration = Duration::from_millis(250);

    /// The holder that keeps a stopped clock still: `(started, stop)`.
    type Holder = Arc<(Mutex<(bool, bool)>, Condvar)>;

    /// Waits on the holder's state until `ready` reports it, holding the guard
    /// across the wait as a condition variable asks.
    fn wait_for(state: &Holder, mut ready: impl FnMut(&(bool, bool)) -> bool) {
        let (lock, woke) = &**state;
        let mut flags = lock.lock().expect("the holder's lock");
        while !ready(&flags) {
            flags = woke.wait(flags).expect("the holder's lock");
        }
        drop(flags);
    }

    pub(super) struct Clock {
        runtime: tokio::runtime::Runtime,
        holder: Holder,
    }

    impl Clock {
        /// A runtime for the endpoint whose clock the test can stop, and the
        /// holder that keeps a stopped clock from moving on its own.
        pub(super) fn new() -> Self {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .thread_name("iroh-clock")
                .enable_all()
                .build()
                .expect("a runtime for the endpoint");
            let holder: Holder = Arc::new((Mutex::new((false, false)), Condvar::new()));
            {
                let holder = Arc::clone(&holder);
                runtime.spawn_blocking(move || {
                    let (lock, woke) = &*holder;
                    {
                        let mut flags = lock.lock().expect("the holder's lock");
                        flags.0 = true;
                        woke.notify_all();
                        drop(flags);
                    }
                    wait_for(&holder, |&(_, stop)| stop);
                });
            }
            // Blocking here is deliberate: an await would let the runtime
            // advance a stopped clock by itself before the holder starts.
            wait_for(&holder, |&(started, _)| started);
            Self { runtime, holder }
        }

        pub(super) fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
            self.runtime.block_on(future)
        }

        /// Stops the clock at its current reading, until [`Self::resume`].
        pub(super) fn pause(&self) {
            self.runtime.block_on(async { tokio::time::pause() });
        }

        /// Lets the clock keep real time again, so a report that is due
        /// measures its probes, and times them out, as it does in production.
        pub(super) fn resume(&self) {
            self.runtime.block_on(async { tokio::time::resume() });
        }

        /// Moves the stopped clock forward, firing what is due then.
        pub(super) fn advance(&self, by: Duration) {
            self.runtime.block_on(tokio::time::advance(by));
        }

        pub(super) fn now(&self) -> tokio::time::Instant {
            self.runtime.block_on(async { tokio::time::Instant::now() })
        }

        /// Gives the endpoint `duration` of real time to answer what the clock
        /// just started, without moving the clock.
        pub(super) fn settle(&self, duration: Duration) {
            self.runtime.block_on(async {
                let _ = tokio::task::spawn_blocking(move || std::thread::sleep(duration)).await;
            });
        }

        /// Polls `ready` for up to the test hang bound of real time.
        pub(super) fn until(&self, what: impl std::fmt::Display, mut ready: impl FnMut() -> bool) {
            let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
            while !ready() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "gave up waiting for {what} after {:?}",
                    tilo_test_env::hang_bound()
                );
                self.settle(SETTLE);
            }
        }
    }

    impl Drop for Clock {
        /// Ends the clock's holder before the runtime is shut down, which waits
        /// for its blocking tasks; a test that panics may never resume it.
        fn drop(&mut self) {
            let (lock, woke) = &*self.holder;
            let mut flags = lock.lock().expect("the holder's lock");
            flags.1 = true;
            woke.notify_all();
            drop(flags);
        }
    }
}

/// Measures how far the test moved the stopped clock before the endpoint's
/// next net report started, failing if none had after `most`. The report is
/// then left to finish on real time, so the clock's next move cannot catch it
/// probing.
#[cfg(target_os = "linux")]
fn wait_for_next_report(
    clock: &clock::Clock,
    reports: impl Fn() -> u64,
    most: Duration,
) -> Duration {
    let started = clock.now();
    let before = reports();
    let elapsed = || clock.now().duration_since(started);
    let wait = loop {
        if reports() > before {
            break elapsed();
        }
        assert!(
            elapsed() < most,
            "no net report {:?} after the one before",
            elapsed()
        );
        clock.advance(clock::STEP);
        clock.settle(clock::SETTLE);
    };
    clock.resume();
    clock.settle(clock::REPORT);
    clock.pause();
    wait
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
    let relay = relay_runtime.block_on(tilo_test_relay::relay());
    let url = relay.url().clone();

    let clock = clock::Clock::new();
    let endpoint = clock.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(relay.relays()))
            // No router answers here, and the mapper's probes time out on the
            // clock this test moves; the report they ride in is what is timed.
            .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
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
    clock.until("the server to count the endpoint's connection", || {
        server.metrics().server.accepts.get() >= 1
    });
    let accepts = server.metrics().server.accepts.get();

    clock.pause();
    // With a server to ask, the endpoint's periodic tick is due; the test
    // moves the clock to it rather than waiting 20-26 s for it.
    let ticks = || endpoint.metrics().socket.actor_tick_re_stun.get();
    let ticked = ticks();
    clock.advance(clock::MAX_PERIOD);
    clock.until("the periodic re-STUN tick", || ticks() > ticked);
    clock.resume();
    clock.settle(clock::REPORT);
    clock.pause();

    clock.block_on(endpoint.remove_relay(&url));
    clock.until("the endpoint to give up its home relay", || {
        endpoint.home_relay_status().get().is_empty()
    });
    assert!(
        endpoint.addr().relay_urls().next().is_none(),
        "the endpoint still names the relay it gave up"
    );
    // The released connection closes when its idle wait expires, a minute on,
    // which the test spends on the clock rather than in real time.
    let mut waited = Duration::ZERO;
    while server.metrics().server.disconnects.get() < accepts {
        assert!(
            waited < Duration::from_secs(180),
            "the released connection never closed"
        );
        clock.advance(Duration::from_secs(20));
        clock.settle(clock::REPORT);
        waited = waited.saturating_add(Duration::from_secs(20));
    }

    // Five minutes of the endpoint's clock: the span that used to hold a dozen
    // 20-26 s ticks, each with a net report behind it.
    let settled = ticks();
    let reports = endpoint.metrics().net_report.reports.get();
    for _ in 0..10 {
        clock.advance(Duration::from_secs(30));
        clock.settle(clock::SETTLE);
    }
    eprintln!(
        "a tick with no server to ask: {} (was {settled}); net reports: {} (was {reports})",
        ticks(),
        endpoint.metrics().net_report.reports.get()
    );
    assert_eq!(ticks(), settled, "an endpoint with no server ticked");
    assert_eq!(
        endpoint.metrics().net_report.reports.get(),
        reports,
        "an endpoint with no server measured its addresses"
    );

    clock.resume();
    clock.block_on(endpoint.close());
    drop(relay);
}

#[cfg(target_os = "linux")]
#[test]
fn an_unchanged_net_report_rests_the_next() {
    let relay_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("relay-server")
        .enable_all()
        .build()
        .expect("a runtime for the relay");
    let relay = relay_runtime.block_on(tilo_test_relay::relay());

    let clock = clock::Clock::new();
    let endpoint = clock.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Custom(relay.relays()))
            // No router answers here, and the mapper's probes time out on the
            // clock this test moves; the report they ride in is what is timed.
            .portmapper_config(iroh::endpoint::PortmapperConfig::Disabled)
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
    // The endpoint's first report is what made it online above, so none is in
    // flight when the clock stops.
    clock.until("the endpoint's first net report", || reports() >= 1);
    clock.pause();

    // The wait between unchanged periodic reports grows: the 20-26 s period
    // while a report still finds something new, then four periods (80-104 s),
    // then the five-minute cap. How many reports the start takes to settle
    // depends on the network it starts on, so the test reads the waits until
    // the first long one instead of counting reports.
    let most = Duration::from_secs(109);
    let mut short = 0;
    let rest = loop {
        let wait = wait_for_next_report(&clock, reports, most);
        if wait <= Duration::from_secs(31) {
            short += 1;
            assert!(
                short < 5,
                "unchanged net reports never rested: {short} waits of {wait:?} at the 20-26 s period"
            );
            continue;
        }
        break wait;
    };
    let capped = wait_for_next_report(&clock, reports, Duration::from_secs(305));
    eprintln!(
        "unchanged net reports came {short} waits of the period, then {:.0} s and {:.0} s apart",
        rest.as_secs_f32(),
        capped.as_secs_f32()
    );
    assert!(
        rest >= Duration::from_secs(80) && rest <= Duration::from_secs(109),
        "the rested wait was {rest:?}, not the four periods of 80-104 s"
    );
    assert!(
        capped >= Duration::from_secs(295) && capped <= Duration::from_secs(305),
        "the capped wait was {capped:?}, not five minutes"
    );

    clock.resume();
    clock.block_on(endpoint.close());
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
