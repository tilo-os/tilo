//! The DNS stub: the sandbox's only resolver.
//!
//! It listens on `127.0.0.1:53` inside the namespace (the sockets were
//! opened there; the threads serving them run in the supervisor) and
//! forwards a query to the host's resolver only when the policy allows the
//! name. Every address in the answer is put in the filter's allowed set —
//! with the record's TTL — *before* the answer goes back, so the connection
//! that follows finds it admitted. Any other name is answered `NXDOMAIN`
//! without leaving the machine.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bounded_command::WakeFd;

use crate::allowed::{AllowedAddrs, Change};
use crate::dns::{self, Rcode};
use crate::nft::Netfilter;
use crate::policy::{Allowed, EgressPolicy, admissible};

/// How long the upstream resolver may take.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(4);
/// The longest the UDP thread may block in `recv_from` once `poll(2)` said
/// a datagram is ready, should that readiness turn out to be spurious. It
/// never runs while the stub is idle: the thread blocks in `poll(2)`.
const RECV_GUARD: Duration = Duration::from_millis(200);
/// Queries in flight at once; more are dropped (the client retries).
const MAX_IN_FLIGHT: usize = 64;
/// A TCP client that sends nothing for this long is dropped.
const TCP_IDLE: Duration = Duration::from_secs(10);

/// State the stub shares with [`crate::Egress`].
#[derive(Debug)]
pub struct Shared {
    pub policy: RwLock<EgressPolicy>,
    pub filter: Mutex<Filter>,
    pub upstream: Vec<SocketAddr>,
}

/// The kernel's sets and their mirror, changed together.
#[derive(Debug)]
pub struct Filter {
    pub netfilter: Option<Netfilter>,
    pub allowed: AllowedAddrs,
}

impl Filter {
    /// Put the admissible addresses of an answer in the allowed sets.
    ///
    /// # Errors
    /// The kernel's refusal; the mirror is rolled back so the next answer
    /// tries again.
    pub fn admit(&mut self, addrs: &[(IpAddr, u32)], now: Instant) -> std::io::Result<()> {
        let Some(netfilter) = self.netfilter.as_mut().filter(|n| n.installed()) else {
            return Ok(());
        };
        self.allowed.prune(now);
        let mut add = Vec::new();
        let mut refresh = Vec::new();
        for (addr, ttl) in addrs {
            match self.allowed.admit(*addr, *ttl, now) {
                Some(Change::Add(t)) => add.push((*addr, t)),
                Some(Change::Refresh(t)) => refresh.push((*addr, t)),
                None => {}
            }
        }
        let result = netfilter.admit(&add, &refresh).or_else(|e| {
            if refresh.is_empty() {
                return Err(e);
            }
            // A refreshed element may have aged out of the kernel's set in
            // the meantime, failing the delete; add everything instead.
            let all: Vec<(IpAddr, Duration)> = add.iter().chain(&refresh).copied().collect();
            netfilter.admit(&all, &[])
        });
        if result.is_err() {
            self.allowed
                .forget(add.iter().chain(&refresh).map(|(a, _)| *a));
        }
        result
    }
}

/// The threads serving the stub's sockets. Each blocks in `poll(2)` on its
/// socket and on `stop`, so it runs only for DNS traffic or to end.
#[derive(Debug)]
pub struct Stub {
    /// Rung once, by [`Drop`], and never drained: readable from then on, so
    /// both threads see it however late they look.
    stop: Arc<WakeFd>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Stub {
    /// Serve `udp` and `tcp` (already bound inside the namespace).
    ///
    /// # Errors
    /// Returns the error configuring the sockets or starting a thread.
    pub fn start(udp: UdpSocket, tcp: TcpListener, shared: &Arc<Shared>) -> std::io::Result<Self> {
        // The UDP socket stays blocking: its clones send the replies, and a
        // non-blocking send could drop one when the buffer is full.
        udp.set_read_timeout(Some(RECV_GUARD))?;
        tcp.set_nonblocking(true)?;
        let stop = Arc::new(WakeFd::new()?);
        let in_flight = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        {
            let (stop, shared, in_flight) = (
                Arc::clone(&stop),
                Arc::clone(shared),
                Arc::clone(&in_flight),
            );
            threads.push(
                std::thread::Builder::new()
                    .name("egress-dns-udp".into())
                    .spawn(move || serve_udp(&udp, &shared, &stop, &in_flight))?,
            );
        }
        {
            let (stop, shared) = (Arc::clone(&stop), Arc::clone(shared));
            threads.push(
                std::thread::Builder::new()
                    .name("egress-dns-tcp".into())
                    .spawn(move || serve_tcp(&tcp, &shared, &stop, &in_flight))?,
            );
        }
        Ok(Self { stop, threads })
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.ring();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// A slot in the in-flight budget, released on drop.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(counter: &Arc<AtomicUsize>) -> Option<Self> {
        let prev = counter.fetch_add(1, Ordering::AcqRel);
        if prev >= MAX_IN_FLIGHT {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(Self(Arc::clone(counter)))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What woke a serving thread.
#[derive(Debug, PartialEq, Eq)]
enum Ready {
    /// The socket has a datagram, a connection or an error to collect.
    Socket,
    /// The stub is being dropped.
    Stop,
}

/// Block, with no timeout, until `socket` is ready or `stop` is rung. Stop
/// wins when both are.
fn wait(socket: BorrowedFd<'_>, stop: &WakeFd) -> std::io::Result<Ready> {
    let pollin = |fd: BorrowedFd<'_>| libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let mut fds = [pollin(socket), pollin(stop.fd())];
    loop {
        // SAFETY: `fds` is a valid, exclusively borrowed two-element array
        // of pollfd for the duration of the call, and both descriptors are
        // borrowed from owners that outlive it.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready >= 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    let [_, stop] = fds;
    // Unless stop was rung, poll(2) without a timeout returned for the
    // socket: readable, or an error or hang-up the caller's `recv_from` or
    // `accept` then reports.
    Ok(if stop.revents == 0 {
        Ready::Socket
    } else {
        Ready::Stop
    })
}

fn serve_udp(udp: &UdpSocket, shared: &Arc<Shared>, stop: &WakeFd, in_flight: &Arc<AtomicUsize>) {
    let mut buf = [0_u8; 4096];
    loop {
        match wait(udp.as_fd(), stop) {
            Ok(Ready::Socket) => {}
            Ok(Ready::Stop) => return,
            Err(e) => {
                tracing::warn!(error = %e, "egress: the DNS stub's UDP socket failed");
                return;
            }
        }
        let (n, from) = match udp.recv_from(&mut buf) {
            Ok(got) => got,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(e) => {
                tracing::warn!(error = %e, "egress: the DNS stub's UDP socket failed");
                return;
            }
        };
        let Some(query) = buf.get(..n).map(<[u8]>::to_vec) else {
            continue;
        };
        let Some(slot) = Slot::take(in_flight) else {
            continue;
        };
        let (Ok(reply_sock), shared) = (udp.try_clone(), Arc::clone(shared)) else {
            continue;
        };
        std::thread::spawn(move || {
            let _slot = slot;
            if let Some(reply) = answer(&shared, &query, Transport::Udp) {
                let _ = reply_sock.send_to(&reply, from);
            }
        });
    }
}

fn serve_tcp(tcp: &TcpListener, shared: &Arc<Shared>, stop: &WakeFd, in_flight: &Arc<AtomicUsize>) {
    loop {
        match wait(tcp.as_fd(), stop) {
            Ok(Ready::Socket) => {}
            Ok(Ready::Stop) => return,
            Err(e) => {
                tracing::warn!(error = %e, "egress: the DNS stub's TCP socket failed");
                return;
            }
        }
        match tcp.accept() {
            Ok((stream, _)) => {
                let Some(slot) = Slot::take(in_flight) else {
                    continue;
                };
                let shared = Arc::clone(shared);
                std::thread::spawn(move || {
                    let _slot = slot;
                    serve_tcp_client(stream, &shared);
                });
            }
            // The connection went away between poll(2) and accept: wait again.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => {
                tracing::warn!(error = %e, "egress: the DNS stub's TCP socket failed");
                return;
            }
        }
    }
}

fn serve_tcp_client(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(TCP_IDLE));
    while let Some(query) = read_framed(&mut stream) {
        let Some(reply) = answer(shared, &query, Transport::Tcp) else {
            return;
        };
        if write_framed(&mut stream, &reply).is_err() {
            return;
        }
    }
}

fn read_framed(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut len = [0_u8; 2];
    stream.read_exact(&mut len).ok()?;
    let mut message = vec![0_u8; usize::from(u16::from_be_bytes(len))];
    stream.read_exact(&mut message).ok()?;
    Some(message)
}

fn write_framed(stream: &mut TcpStream, message: &[u8]) -> std::io::Result<()> {
    let len =
        u16::try_from(message.len()).map_err(|_| std::io::Error::other("DNS message too long"))?;
    let mut framed = Vec::with_capacity(message.len().saturating_add(2));
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(message);
    stream.write_all(&framed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Udp,
    Tcp,
}

/// The stub's answer to one query, or `None` to stay silent (a message that
/// is not even a DNS header).
pub fn answer(shared: &Shared, query: &[u8], transport: Transport) -> Option<Vec<u8>> {
    dns::query_id(query)?;
    let Some(question) = dns::parse_query(query) else {
        return Some(dns::refusal(query, None, Rcode::Refused));
    };
    let allowed = shared
        .policy
        .read()
        .ok()
        .and_then(|p| p.allows(&question.name));
    let Some(how) = allowed else {
        tracing::debug!(name = %question.name, "egress: refused a name that is not granted");
        return Some(dns::refusal(query, Some(&question), Rcode::NxDomain));
    };
    let Some(response) = forward(&shared.upstream, query, &question, transport) else {
        return Some(dns::refusal(query, Some(&question), Rcode::ServFail));
    };
    if how == Allowed::Any {
        return Some(response);
    }
    let addrs: Vec<(IpAddr, u32)> = dns::answered_addresses(&response)
        .into_iter()
        .filter(|a| admissible(a.addr, how))
        .map(|a| (a.addr, a.ttl))
        .collect();
    if !addrs.is_empty() {
        let Ok(mut filter) = shared.filter.lock() else {
            return Some(dns::refusal(query, Some(&question), Rcode::ServFail));
        };
        // The policy may have narrowed while the resolver was asked; under
        // the filter lock it cannot change, so ask it again here.
        let still = shared
            .policy
            .read()
            .ok()
            .and_then(|p| p.allows(&question.name));
        if still.is_none() {
            return Some(dns::refusal(query, Some(&question), Rcode::NxDomain));
        }
        let admitted = filter.admit(&addrs, Instant::now());
        drop(filter);
        if let Err(e) = admitted {
            // The address would not be reachable: say so as a failed lookup
            // rather than hand out an answer the filter will refuse.
            tracing::warn!(error = %e, name = %question.name, "egress: could not admit an answer");
            return Some(dns::refusal(query, Some(&question), Rcode::ServFail));
        }
    }
    Some(response)
}

/// Ask the upstream resolvers, in order. Over UDP first; a truncated answer
/// is fetched again over TCP, except for a UDP client, which gets the
/// truncated answer and asks the stub over TCP itself.
fn forward(
    upstream: &[SocketAddr],
    query: &[u8],
    question: &dns::Question,
    transport: Transport,
) -> Option<Vec<u8>> {
    for server in upstream {
        let udp = forward_udp(*server, query, question);
        match (udp, transport) {
            (Some(r), Transport::Udp) => return Some(r),
            (Some(r), Transport::Tcp) if !dns::truncated(&r) => return Some(r),
            _ => {}
        }
        if let Some(r) = forward_tcp(*server, query, question) {
            return Some(r);
        }
    }
    None
}

fn forward_udp(server: SocketAddr, query: &[u8], question: &dns::Question) -> Option<Vec<u8>> {
    let bind: SocketAddr = if server.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0_u16; 8], 0))
    };
    let sock = UdpSocket::bind(bind).ok()?;
    sock.connect(server).ok()?;
    sock.set_read_timeout(Some(UPSTREAM_TIMEOUT)).ok()?;
    sock.send(query).ok()?;
    let deadline = Instant::now().checked_add(UPSTREAM_TIMEOUT)?;
    let mut buf = vec![0_u8; 65_535];
    while Instant::now() < deadline {
        let n = sock.recv(&mut buf).ok()?;
        let reply = buf.get(..n)?;
        // Anything that is not the answer to this question (a stray or
        // spoofed datagram) is ignored.
        if dns::answers(reply, question) {
            return Some(reply.to_vec());
        }
    }
    None
}

fn forward_tcp(server: SocketAddr, query: &[u8], question: &dns::Question) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect_timeout(&server, UPSTREAM_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(UPSTREAM_TIMEOUT)).ok()?;
    write_framed(&mut stream, query).ok()?;
    let reply = read_framed(&mut stream)?;
    dns::answers(&reply, question).then_some(reply)
}

pub use crate::config::nameservers;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::tests::response;
    use tilo_test_env::idle;

    fn shared(policy: EgressPolicy, upstream: Vec<SocketAddr>) -> Shared {
        Shared {
            policy: RwLock::new(policy),
            filter: Mutex::new(Filter {
                netfilter: None,
                allowed: AllowedAddrs::default(),
            }),
            upstream,
        }
    }

    /// A resolver on loopback answering every A query with 192.0.2.1 —
    /// and counting how many questions reached it.
    fn fake_resolver() -> (SocketAddr, Arc<AtomicUsize>) {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        let asked = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&asked);
        std::thread::spawn(move || {
            let mut buf = [0_u8; 1500];
            while let Ok((n, from)) = sock.recv_from(&mut buf) {
                counter.fetch_add(1, Ordering::SeqCst);
                let reply = response(&buf[..n], &[(dns::TYPE_A, 300, vec![192, 0, 2, 1])]);
                let _ = sock.send_to(&reply, from);
            }
        });
        (addr, asked)
    }

    #[test]
    fn a_name_that_is_not_granted_never_leaves_the_machine() {
        let (resolver, asked) = fake_resolver();
        let s = shared(EgressPolicy::from_grants(["github.com"]), vec![resolver]);
        let q = dns::query(1, "example.com", dns::TYPE_A);
        let reply = answer(&s, &q, Transport::Udp).unwrap();
        assert_eq!(dns::rcode(&reply), Some(3), "NXDOMAIN");
        assert_eq!(asked.load(Ordering::SeqCst), 0, "the resolver was asked");

        let q = dns::query(2, "github.com", dns::TYPE_A);
        let reply = answer(&s, &q, Transport::Udp).unwrap();
        assert_eq!(dns::rcode(&reply), Some(0));
        assert_eq!(
            dns::answered_addresses(&reply)[0].addr,
            "192.0.2.1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        // No filter is installed here, so nothing was mirrored either.
        assert!(s.filter.lock().unwrap().allowed.is_empty());
    }

    #[test]
    fn no_grants_means_every_name_is_refused_and_any_forwards_everything() {
        let (resolver, asked) = fake_resolver();
        let none = shared(EgressPolicy::none(), vec![resolver]);
        for name in ["github.com", "localhost", "1.1.1.1.in-addr.arpa"] {
            let reply = answer(&none, &dns::query(3, name, dns::TYPE_A), Transport::Tcp).unwrap();
            assert_eq!(dns::rcode(&reply), Some(3), "{name}");
        }
        assert_eq!(asked.load(Ordering::SeqCst), 0);
        let any = shared(EgressPolicy::Any, vec![resolver]);
        let reply = answer(
            &any,
            &dns::query(4, "anything.example", dns::TYPE_A),
            Transport::Udp,
        )
        .unwrap();
        assert_eq!(dns::rcode(&reply), Some(0));
    }

    /// Tests that start a stub hold this, so a thread name is one stub's.
    static STUBS: Mutex<()> = Mutex::new(());

    /// A stub serving loopback sockets, as `spawn_egress` starts it on the
    /// namespace's, with its two threads' ids.
    struct Running {
        stub: Stub,
        udp: SocketAddr,
        tcp: SocketAddr,
        udp_thread: String,
        tcp_thread: String,
    }

    fn start(shared: &Arc<Shared>) -> Running {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        let (udp_addr, tcp_addr) = (udp.local_addr().unwrap(), tcp.local_addr().unwrap());
        let ((stub, udp_thread), tcp_thread) = idle::spawned_thread("egress-dns-tcp", || {
            idle::spawned_thread("egress-dns-udp", || Stub::start(udp, tcp, shared).unwrap())
        });
        Running {
            stub,
            udp: udp_addr,
            tcp: tcp_addr,
            udp_thread,
            tcp_thread,
        }
    }

    fn ask_udp(stub: SocketAddr, query: &[u8]) -> Vec<u8> {
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        client.send_to(query, stub).unwrap();
        let mut buf = [0_u8; 4096];
        let (n, _) = client.recv_from(&mut buf).unwrap();
        buf[..n].to_vec()
    }

    fn ask_tcp(client: &mut TcpStream, query: &[u8]) -> Vec<u8> {
        write_framed(client, query).unwrap();
        read_framed(client).unwrap()
    }

    #[test]
    fn an_idle_stub_does_not_wake() {
        let _one = STUBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shared = Arc::new(shared(
            EgressPolicy::from_grants(["github.com"]),
            Vec::new(),
        ));
        let running = start(&shared);
        idle::assert_stays_asleep(&running.udp_thread, "egress-dns-udp");
        idle::assert_stays_asleep(&running.tcp_thread, "egress-dns-tcp");
        drop(running.stub);
    }

    #[test]
    fn dropping_the_stub_ends_both_threads_promptly() {
        let _one = STUBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shared = Arc::new(shared(EgressPolicy::none(), Vec::new()));
        let running = start(&shared);
        // Both threads parked in poll(2), as on an idle desktop.
        std::thread::sleep(Duration::from_millis(50));
        let stopping = Instant::now();
        drop(running.stub);
        let took = stopping.elapsed();
        // Drop joins both threads, so returning at all means they ended. The
        // stop-flag loop this replaced looked every 200 ms.
        assert!(took < Duration::from_millis(100), "stopping took {took:?}");
        for tid in [running.udp_thread, running.tcp_thread] {
            let task = format!("/proc/self/task/{tid}");
            let gone = Instant::now();
            while std::path::Path::new(&task).exists() {
                assert!(
                    gone.elapsed() < Duration::from_secs(5),
                    "{task} outlived the stub"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    #[test]
    fn queries_over_both_sockets_resolve_or_are_refused_as_before() {
        let _one = STUBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (resolver, asked) = fake_resolver();
        let shared = Arc::new(shared(
            EgressPolicy::from_grants(["github.com"]),
            vec![resolver],
        ));
        let running = start(&shared);
        let granted = "192.0.2.1".parse::<IpAddr>().unwrap();

        // Over UDP: the same bytes `answer` gives, which the decision tests
        // above pin down.
        let denied = dns::query(11, "example.com", dns::TYPE_A);
        let reply = ask_udp(running.udp, &denied);
        assert_eq!(dns::rcode(&reply), Some(3), "NXDOMAIN");
        assert_eq!(asked.load(Ordering::SeqCst), 0, "the resolver was asked");
        assert_eq!(Some(reply), answer(&shared, &denied, Transport::Udp));
        let allowed = dns::query(12, "github.com", dns::TYPE_A);
        let reply = ask_udp(running.udp, &allowed);
        assert_eq!(dns::rcode(&reply), Some(0));
        assert_eq!(dns::answered_addresses(&reply)[0].addr, granted);
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert_eq!(Some(reply), answer(&shared, &allowed, Transport::Udp));
        assert_eq!(asked.load(Ordering::SeqCst), 2);

        // Over TCP, two queries on one connection.
        let mut client = TcpStream::connect(running.tcp).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let denied = dns::query(13, "example.com", dns::TYPE_A);
        let reply = ask_tcp(&mut client, &denied);
        assert_eq!(dns::rcode(&reply), Some(3), "NXDOMAIN");
        assert_eq!(asked.load(Ordering::SeqCst), 2, "the resolver was asked");
        assert_eq!(Some(reply), answer(&shared, &denied, Transport::Tcp));
        let allowed = dns::query(14, "github.com", dns::TYPE_A);
        let reply = ask_tcp(&mut client, &allowed);
        assert_eq!(dns::rcode(&reply), Some(0));
        assert_eq!(dns::answered_addresses(&reply)[0].addr, granted);
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        assert_eq!(Some(reply), answer(&shared, &allowed, Transport::Tcp));
        drop(client);
        drop(running.stub);
    }

    #[test]
    fn a_silent_resolver_is_a_servfail_and_junk_is_refused() {
        let s = shared(
            EgressPolicy::from_grants(["github.com"]),
            vec!["127.0.0.1:9".parse().unwrap()],
        );
        let reply = answer(
            &s,
            &dns::query(5, "github.com", dns::TYPE_A),
            Transport::Udp,
        )
        .unwrap();
        assert_eq!(dns::rcode(&reply), Some(2));
        let mut junk = dns::query(6, "github.com", dns::TYPE_A);
        junk[5] = 2;
        assert_eq!(
            dns::rcode(&answer(&s, &junk, Transport::Udp).unwrap()),
            Some(5)
        );
        assert!(answer(&s, &[1, 2], Transport::Udp).is_none());
    }
}
