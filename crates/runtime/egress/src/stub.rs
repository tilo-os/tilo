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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use crate::allowed::{AllowedAddrs, Change};
use crate::dns::{self, Rcode};
use crate::nft::Netfilter;
use crate::policy::{Allowed, EgressPolicy, admissible};

/// How long the upstream resolver may take.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(4);
/// How often the serving threads look at the stop flag.
const POLL: Duration = Duration::from_millis(200);
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

/// The threads serving the stub's sockets.
#[derive(Debug)]
pub struct Stub {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Stub {
    /// Serve `udp` and `tcp` (already bound inside the namespace).
    ///
    /// # Errors
    /// Returns the error configuring the sockets or starting a thread.
    pub fn start(udp: UdpSocket, tcp: TcpListener, shared: &Arc<Shared>) -> std::io::Result<Self> {
        udp.set_read_timeout(Some(POLL))?;
        tcp.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
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
        self.stop.store(true, Ordering::Release);
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

fn serve_udp(
    udp: &UdpSocket,
    shared: &Arc<Shared>,
    stop: &AtomicBool,
    in_flight: &Arc<AtomicUsize>,
) {
    let mut buf = [0_u8; 4096];
    while !stop.load(Ordering::Acquire) {
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

fn serve_tcp(
    tcp: &TcpListener,
    shared: &Arc<Shared>,
    stop: &AtomicBool,
    in_flight: &Arc<AtomicUsize>,
) {
    while !stop.load(Ordering::Acquire) {
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
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
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

/// The resolvers named in a `resolv.conf`, on port 53.
pub fn nameservers(resolv_conf: &str) -> Vec<SocketAddr> {
    resolv_conf
        .lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|rest| {
            let addr = rest.trim();
            // A scoped link-local resolver (`fe80::1%eth0`) can't be named
            // by a SocketAddr; it is skipped.
            addr.parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, 53))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::tests::response;

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

    #[test]
    fn nameservers_come_from_resolv_conf() {
        let conf = "# generated\nnameserver 10.0.0.1\nnameserver  2001:db8::53\nnameserver fe80::1%eth0\nsearch lan\n";
        assert_eq!(
            nameservers(conf),
            vec![
                "10.0.0.1:53".parse::<SocketAddr>().unwrap(),
                "[2001:db8::53]:53".parse().unwrap()
            ]
        );
    }
}
