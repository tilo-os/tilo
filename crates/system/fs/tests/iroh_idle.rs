//! The vendored iroh, netwatch and iroh-gossip patches (see each crate's
//! `vendor/*/TILO-PATCH.md`): an idle endpoint wakes for nothing, and still
//! hears at once of a network change.
//!
//! Every Tilo process with an endpoint (the compositor, `fs serve`,
//! `fs replica`, `fs mount-share`) used to wake on three iroh timers that had
//! nothing to do: netwatch's 15 s wall-clock poll, the socket's 20-26 s
//! re-STUN tick with no relays, and the 15 s ping of a home relay kept after
//! the relays were removed. And iroh-gossip swept each topic's message cache
//! every 10 s for as long as the topic lived, empty or not.

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

/// How soon a change on an interface must reach the endpoint's addresses.
/// netwatch debounces netlink events for 250 ms; the clock poll it no longer
/// runs came every 15 s.
#[cfg(target_os = "linux")]
const NOTICED_WITHIN: Duration = Duration::from_secs(5);

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

/// A network change (an interface coming up, an address added and removed,
/// as when Wi-Fi joins or a VPN goes up and down) reaches the endpoint's
/// addresses through netlink within [`NOTICED_WITHIN`], with no clock poll
/// to fall back on. Runs itself again inside a user and network namespace of
/// its own, where it may change `lo`.
#[cfg(target_os = "linux")]
#[test]
fn an_endpoint_hears_of_network_changes_through_netlink() {
    const CHILD: &str = "TILO_IROH_NETNS_CHILD";
    const NAME: &str = "an_endpoint_hears_of_network_changes_through_netlink";
    if std::env::var_os(CHILD).is_some() {
        netns::changes_reach_the_endpoint();
        return;
    }
    let output = std::process::Command::new("unshare")
        .args(["--user", "--map-root-user", "--net", "--"])
        .arg(std::env::current_exe().expect("this test binary"))
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .expect("util-linux unshare runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("{stdout}\n{stderr}");
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "the endpoint in its own network namespace did not see the changes"
    );
}

#[cfg(target_os = "linux")]
mod netns {
    use std::{
        net::{IpAddr, Ipv4Addr},
        time::Instant,
    };

    use super::NOTICED_WITHIN;

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

        // Taking a labelled address down removes it.
        set_up(ALIAS, false);
        noticed("the address removed", || !has(IpAddr::V4(ALIAS_ADDR)));

        runtime.block_on(endpoint.close());
    }

    fn noticed(what: &str, mut seen: impl FnMut() -> bool) {
        let changed = Instant::now();
        while !seen() {
            assert!(
                changed.elapsed() <= NOTICED_WITHIN,
                "the endpoint did not notice {what} within {NOTICED_WITHIN:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
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

/// iroh-gossip sweeps a topic's message cache only while something is
/// cached: the first message arms the sweep, it stops once the caches have
/// expired, and the next message arms it once more. Driven through the
/// public sans-IO topic state, with Tilo's gossip timings.
#[test]
fn gossip_sweeps_a_topic_cache_only_while_it_holds_messages() {
    use iroh_gossip::proto::{Command, Config, PlumtreeConfig, Scope, topic};
    use tokio::time::Instant;

    // The compositor's timings (`broadcast_config` in the shell's actor).
    let plumtree = PlumtreeConfig {
        cache_evict_interval: Duration::from_secs(10),
        message_cache_retention: Duration::from_secs(20),
        ..PlumtreeConfig::default()
    };
    let config = Config {
        broadcast: plumtree.clone(),
        ..Config::default()
    };
    let mut state: topic::State<u64, _> = topic::State::new(1, None, config);
    let sweeps = |events: Vec<topic::OutEvent<u64>>| -> Vec<topic::Timer<u64>> {
        events
            .into_iter()
            .filter_map(|event| match event {
                topic::OutEvent::ScheduleTimer(after, timer @ topic::Timer::Gossip(_))
                    if after == plumtree.cache_evict_interval =>
                {
                    Some(timer)
                }
                _ => None,
            })
            .collect()
    };
    let broadcast = |state: &mut topic::State<u64, _>, body: &'static [u8], now| {
        sweeps(
            state
                .handle(
                    topic::InEvent::Command(Command::Broadcast(body.into(), Scope::Swarm)),
                    now,
                )
                .collect(),
        )
    };

    let start = Instant::now();
    let mut now = start;
    let mut armed = broadcast(&mut state, b"first", now);
    assert_eq!(armed.len(), 1, "the first message arms one sweep");
    assert!(
        broadcast(&mut state, b"second", now).is_empty(),
        "a second message while the sweep is pending arms no other"
    );

    let retention = plumtree
        .message_id_retention
        .max(plumtree.message_cache_retention);
    while let Some(timer) = armed.pop() {
        now += plumtree.cache_evict_interval;
        assert!(
            now - start <= retention + plumtree.cache_evict_interval,
            "the sweep outlived everything it could expire"
        );
        armed = sweeps(
            state
                .handle(topic::InEvent::TimerExpired(timer), now)
                .collect(),
        );
        assert!(armed.len() <= 1, "one sweep at a time");
    }

    assert_eq!(
        broadcast(&mut state, b"third", now).len(),
        1,
        "the next message arms the sweep again"
    );
}
