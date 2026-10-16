use std::{
    collections::HashMap,
    net::IpAddr,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

use libc::{
    RTNLGRP_IPV4_IFADDR, RTNLGRP_IPV4_ROUTE, RTNLGRP_IPV4_RULE, RTNLGRP_IPV6_IFADDR,
    RTNLGRP_IPV6_ROUTE, RTNLGRP_IPV6_RULE,
};
use n0_error::stack_error;
use n0_future::{
    Stream, StreamExt,
    task::AbortOnDropHandle,
    time::{self, Duration},
};
use netlink_packet_core::{NLM_F_DUMP, NLM_F_REQUEST, NetlinkMessage, NetlinkPayload};
use netlink_packet_route::{RouteNetlinkMessage, address, route};
use netlink_sys::{AsyncSocket, SocketAddr};
use tokio::sync::mpsc;
use tracing::{trace, warn};

use super::actor::NetworkMessage;
use crate::ip::is_link_local;

#[derive(Debug)]
pub(super) struct RouteMonitor {
    _handle: AbortOnDropHandle<()>,
}

#[stack_error(derive, add_meta, from_sources, std_sources)]
#[non_exhaustive]
pub enum Error {
    #[error("IO")]
    Io { source: std::io::Error },
}

const fn nl_mgrp(group: u32) -> u32 {
    if group > 31 {
        panic!("use netlink_sys::Socket::add_membership() for this group");
    }
    if group == 0 { 0 } else { 1 << (group - 1) }
}
macro_rules! get_nla {
    ($msg:expr, $nla:path) => {
        $msg.attributes.iter().find_map(|nla| match nla {
            $nla(n) => Some(n),
            _ => None,
        })
    };
}

#[allow(clippy::type_complexity)]
fn setup_netlink() -> std::io::Result<(
    AbortOnDropHandle<()>,
    netlink_proto::ConnectionHandle<RouteNetlinkMessage>,
    AddressFilter,
    impl Stream<Item = (NetlinkMessage<RouteNetlinkMessage>, SocketAddr)>,
)> {
    use netlink_sys::protocols::NETLINK_ROUTE;

    let (mut conn, handle, messages) =
        netlink_proto::new_connection::<RouteNetlinkMessage>(NETLINK_ROUTE)?;

    let groups = nl_mgrp(RTNLGRP_IPV4_IFADDR)
        | nl_mgrp(RTNLGRP_IPV6_IFADDR)
        | nl_mgrp(RTNLGRP_IPV4_ROUTE)
        | nl_mgrp(RTNLGRP_IPV6_ROUTE)
        | nl_mgrp(RTNLGRP_IPV4_RULE)
        | nl_mgrp(RTNLGRP_IPV6_RULE);

    let addr = SocketAddr::new(0, groups);
    conn.socket_mut().socket_mut().bind(&addr)?;
    let filter = AddressFilter::new(conn.socket_mut().socket_mut().as_raw_fd())?;

    let conn_handle = AbortOnDropHandle::new(tokio::task::spawn(conn));

    Ok((conn_handle, handle, filter, messages))
}

/// TILO PATCH: the addresses the host has now, which the kernel filter needs
/// from the start: a refresh of an address first seen only as a refresh
/// would otherwise wake the process once before the filter knew it.
async fn learn_addresses(
    handle: &netlink_proto::ConnectionHandle<RouteNetlinkMessage>,
    known: &mut HashMap<(u32, IpAddr), KnownAddress>,
) {
    let mut request = NetlinkMessage::from(RouteNetlinkMessage::GetAddress(
        address::AddressMessage::default(),
    ));
    request.header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    let mut replies = match handle.request(request, SocketAddr::new(0, 0)) {
        Ok(replies) => replies,
        Err(err) => {
            warn!("could not list the host's addresses: {err:?}");
            return;
        }
    };
    while let Some(reply) = replies.next().await {
        if let NetlinkPayload::InnerMessage(RouteNetlinkMessage::NewAddress(msg)) = reply.payload {
            if let Some(addr) = get_nla!(msg, address::AddressAttribute::Address) {
                known.insert((msg.header.index, *addr), KnownAddress::of(&msg, *addr));
            }
        }
    }
}

/// Returns `true` if the connection was lost (should reconnect),
/// `false` if the sender is gone (should shut down).
async fn process_messages(
    sender: &mpsc::Sender<NetworkMessage>,
    filter: &mut AddressFilter,
    addr_cache: &mut HashMap<(u32, IpAddr), KnownAddress>,
    messages: &mut (impl Stream<Item = (NetlinkMessage<RouteNetlinkMessage>, SocketAddr)> + Unpin),
) -> bool {

    while let Some((message, _)) = messages.next().await {
        match message.payload {
            NetlinkPayload::Error(err) => {
                warn!("error reading netlink payload: {:?}", err);
            }
            NetlinkPayload::Done(_) => {
                trace!("done received, reconnecting");
                return true;
            }
            NetlinkPayload::InnerMessage(msg) => match msg {
                RouteNetlinkMessage::NewAddress(msg) => {
                    trace!("NEWADDR: {:?}", msg);
                    if let Some(addr) = get_nla!(msg, address::AddressAttribute::Address) {
                        let known = KnownAddress::of(&msg, *addr);
                        match addr_cache.insert((msg.header.index, *addr), known) {
                            // A refresh of lifetimes: not a change.
                            Some(previous) if previous == known => continue,
                            // The kernel now drops refreshes of this form.
                            Some(_) => filter.update(addr_cache),
                            None => {
                                filter.update(addr_cache);
                                if sender.send(NetworkMessage::Change).await.is_err() {
                                    return false;
                                }
                            }
                        }
                    }
                }
                RouteNetlinkMessage::DelAddress(msg) => {
                    trace!("DELADDR: {:?}", msg);
                    if let Some(addr) = get_nla!(msg, address::AddressAttribute::Address) {
                        addr_cache.remove(&(msg.header.index, *addr));
                        filter.update(addr_cache);
                    }
                    if sender.send(NetworkMessage::Change).await.is_err() {
                        return false;
                    }
                }
                RouteNetlinkMessage::NewRoute(msg) | RouteNetlinkMessage::DelRoute(msg) => {
                    trace!("ROUTE:: {:?}", msg);

                    let table = get_nla!(msg, route::RouteAttribute::Table)
                        .copied()
                        .unwrap_or_default();
                    if let Some(dst) = get_nla!(msg, route::RouteAttribute::Destination) {
                        match dst {
                            route::RouteAddress::Inet(addr)
                                if (table == 255 || table == 254)
                                    && (addr.is_multicast()
                                        || is_link_local(IpAddr::V4(*addr))) =>
                            {
                                continue;
                            }
                            route::RouteAddress::Inet6(addr)
                                if (table == 255 || table == 254)
                                    && (addr.is_multicast()
                                        || is_link_local(IpAddr::V6(*addr))) =>
                            {
                                continue;
                            }
                            _ => {}
                        }
                    }
                    if sender.send(NetworkMessage::Change).await.is_err() {
                        return false;
                    }
                }
                RouteNetlinkMessage::NewRule(msg) => {
                    trace!("NEWRULE: {:?}", msg);
                    if sender.send(NetworkMessage::Change).await.is_err() {
                        return false;
                    }
                }
                RouteNetlinkMessage::DelRule(msg) => {
                    trace!("DELRULE: {:?}", msg);
                    if sender.send(NetworkMessage::Change).await.is_err() {
                        return false;
                    }
                }
                RouteNetlinkMessage::NewLink(msg) => {
                    trace!("NEWLINK: {:?}", msg);
                }
                RouteNetlinkMessage::DelLink(msg) => {
                    trace!("DELLINK: {:?}", msg);
                }
                msg => {
                    trace!("unhandled: {:?}", msg);
                }
            },
            _ => {}
        }
    }

    // Stream ended — connection lost
    true
}

/// An address as the kernel announces it, less its lifetimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KnownAddress {
    family: u8,
    prefix_len: u8,
    flags: u8,
    scope: u8,
    index: u32,
    addr: IpAddr,
    /// Whether the message carries only `IFA_ADDRESS` before its lifetimes,
    /// which is what the kernel filter looks for.
    plain: bool,
}

impl KnownAddress {
    fn of(msg: &address::AddressMessage, addr: IpAddr) -> Self {
        Self {
            family: u8::from(msg.header.family),
            prefix_len: msg.header.prefix_len,
            flags: msg.header.flags.bits(),
            scope: u8::from(msg.header.scope),
            index: msg.header.index,
            addr,
            plain: !msg
                .attributes
                .iter()
                .any(|nla| matches!(nla, address::AddressAttribute::Local(_))),
        }
    }
}

/// TILO PATCH: a classic BPF program on the netlink socket that makes the
/// kernel drop `RTM_NEWADDR` messages repeating an IPv6 address the monitor
/// already knows (same interface, prefix length, flags and scope), which only
/// refresh its lifetimes. A router that advertises its prefix every few seconds
/// otherwise wakes every process holding a monitor each time, for nothing: the
/// message is ignored once read, but reading it costs the wakeup.
#[derive(Debug)]
struct AddressFilter {
    /// A duplicate of the monitor's socket descriptor: the filter belongs to
    /// the socket, and this keeps it reachable for the monitor's lifetime.
    socket: OwnedFd,
    attached: bool,
}

/// Most addresses the filter lists; with more the monitor reads every message.
const MAX_FILTERED: usize = 32;

impl AddressFilter {
    fn new(socket: std::os::fd::RawFd) -> std::io::Result<Self> {
        // SAFETY: dup of a descriptor the caller holds open.
        let duplicate = unsafe { libc::dup(socket) };
        if duplicate < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            // SAFETY: `duplicate` is a new descriptor nothing else owns.
            socket: unsafe { OwnedFd::from_raw_fd(duplicate) },
            attached: false,
        })
    }

    /// Make the kernel drop refreshes of exactly the addresses in `known`.
    fn update(&mut self, known: &HashMap<(u32, IpAddr), KnownAddress>) {
        let mut entries: Vec<&KnownAddress> = known
            .values()
            .filter(|known| known.plain && known.addr.is_ipv6())
            .collect();
        entries.sort_by_key(|known| (known.index, known.addr));
        let result = if entries.is_empty() || entries.len() > MAX_FILTERED {
            self.detach()
        } else {
            self.attach(&program(&entries))
        };
        if let Err(err) = result {
            warn!("could not filter address refreshes in the kernel: {err}");
        }
    }

    fn attach(&mut self, program: &[libc::sock_filter]) -> std::io::Result<()> {
        let len = u16::try_from(program.len()).expect("at most MAX_FILTERED short blocks");
        let fprog = libc::sock_fprog {
            len,
            filter: program.as_ptr().cast_mut(),
        };
        // SAFETY: SO_ATTACH_FILTER reads `fprog` and the `len` instructions it
        // points at, both alive for the call; the kernel copies them.
        let result = unsafe {
            libc::setsockopt(
                self.socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ATTACH_FILTER,
                (&raw const fprog).cast(),
                size_of::<libc::sock_fprog>() as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        self.attached = true;
        Ok(())
    }

    fn detach(&mut self) -> std::io::Result<()> {
        if !std::mem::take(&mut self.attached) {
            return Ok(());
        }
        let unused = 0_i32;
        // SAFETY: SO_DETACH_FILTER ignores its value.
        let result = unsafe {
            libc::setsockopt(
                self.socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_DETACH_FILTER,
                (&raw const unused).cast(),
                size_of::<i32>() as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

const BPF_LD_W_ABS: u16 = 0x20;
const BPF_LD_H_ABS: u16 = 0x28;
const BPF_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;
const BPF_JA: u16 = 0x05;

fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter { code, jt: 0, jf: 0, k }
}

/// A word of the message, as the program reads it (big-endian) from `bytes`
/// laid out as the kernel wrote them.
fn word(bytes: [u8; 4]) -> u32 {
    u32::from_be_bytes(bytes)
}

fn half(bytes: [u8; 2]) -> u32 {
    u32::from(u16::from_be_bytes(bytes))
}

/// Instructions per listed address: three header checks, four words of
/// address, and the verdict.
const BLOCK: usize = 2 * 7 + 1;

/// The program: pass everything but a `RTM_NEWADDR` that repeats one of
/// `entries`, whose `nlmsghdr` is followed by `ifaddrmsg` and `IFA_ADDRESS`:
/// type at 4, family/prefix/flags/scope at 16, interface at 20, attribute
/// type at 26 and the address at 28.
fn program(entries: &[&KnownAddress]) -> Vec<libc::sock_filter> {
    // A conditional jump reaches 255 instructions; the way out of a long
    // program is an unconditional one.
    let total = 3 + entries.len() * BLOCK + 1;
    let accept = total - 1;
    let mut out = Vec::with_capacity(total);
    out.push(stmt(BPF_LD_H_ABS, 4));
    out.push(libc::sock_filter {
        code: BPF_JEQ_K,
        jt: 1,
        jf: 0,
        k: half(libc::RTM_NEWADDR.to_ne_bytes()),
    });
    out.push(stmt(
        BPF_JA,
        u32::try_from(accept - 3).expect("a short program"),
    ));
    for known in entries {
        let start = out.len();
        let IpAddr::V6(v6) = known.addr else {
            continue;
        };
        let octets = v6.octets();
        let mut checks = vec![
            (BPF_LD_W_ABS, 16, word([known.family, known.prefix_len, known.flags, known.scope])),
            (BPF_LD_W_ABS, 20, word(known.index.to_ne_bytes())),
            (BPF_LD_H_ABS, 26, half(libc::IFA_ADDRESS.to_ne_bytes())),
        ];
        for (n, chunk) in octets.chunks_exact(4).enumerate() {
            checks.push((BPF_LD_W_ABS, 28 + 4 * n as u32, word([chunk[0], chunk[1], chunk[2], chunk[3]])));
        }
        for (load, offset, expected) in checks {
            out.push(stmt(load, offset));
            let next_entry = start + BLOCK;
            let here = out.len();
            out.push(libc::sock_filter {
                code: BPF_JEQ_K,
                jt: 0,
                jf: u8::try_from(next_entry - (here + 1)).expect("a short block"),
                k: expected,
            });
        }
        out.push(stmt(BPF_RET_K, 0));
    }
    out.push(stmt(BPF_RET_K, u32::MAX));
    debug_assert_eq!(out.len(), total);
    out
}

impl RouteMonitor {
    pub(super) fn new(sender: mpsc::Sender<NetworkMessage>) -> Result<Self, Error> {
        let handle = tokio::task::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            const MAX_BACKOFF: Duration = Duration::from_secs(30);

            loop {
                match setup_netlink() {
                    Ok((_conn_handle, handle, mut filter, mut messages)) => {
                        backoff = Duration::from_secs(1);
                        let mut known = HashMap::new();
                        learn_addresses(&handle, &mut known).await;
                        filter.update(&known);
                        // TILO PATCH: the actor took its interface snapshot
                        // before this socket subscribed (and, on a reconnect,
                        // before the old one was lost), and a change in that
                        // gap reached no socket; one whose address the dump
                        // above already lists is read below as a repeat. Have
                        // the actor look again now that every later change
                        // arrives here: an interface that came up meanwhile
                        // would otherwise go unnoticed until the next event.
                        if sender.send(NetworkMessage::Change).await.is_err() {
                            break;
                        }
                        let should_reconnect =
                            process_messages(&sender, &mut filter, &mut known, &mut messages)
                                .await;
                        // _conn_handle dropped here, aborting the connection task
                        if !should_reconnect {
                            break;
                        }
                        warn!("netlink connection lost, reconnecting");
                    }
                    Err(err) => {
                        warn!("failed to setup netlink: {:?}", err);
                    }
                }
                time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        });

        Ok(RouteMonitor {
            _handle: AbortOnDropHandle::new(handle),
        })
    }
}

pub(crate) fn is_interesting_interface(_name: &str) -> bool {
    true
}

#[cfg(test)]
mod filter_tests {
    use std::{net::Ipv6Addr, os::fd::AsRawFd, os::unix::net::UnixDatagram};

    use super::*;

    const RTM_DELADDR: u16 = 21;

    /// One address message as the kernel writes it: `nlmsghdr`, `ifaddrmsg`,
    /// `IFA_ADDRESS` and `IFA_CACHEINFO`.
    fn message(kind: u16, known: &KnownAddress, lifetime: u32) -> Vec<u8> {
        let IpAddr::V6(v6) = known.addr else {
            panic!("an IPv6 address");
        };
        let mut out = Vec::new();
        out.extend_from_slice(&64_u32.to_ne_bytes());
        out.extend_from_slice(&kind.to_ne_bytes());
        out.extend_from_slice(&[0; 10]);
        out.extend_from_slice(&[known.family, known.prefix_len, known.flags, known.scope]);
        out.extend_from_slice(&known.index.to_ne_bytes());
        out.extend_from_slice(&20_u16.to_ne_bytes());
        out.extend_from_slice(&libc::IFA_ADDRESS.to_ne_bytes());
        out.extend_from_slice(&v6.octets());
        out.extend_from_slice(&20_u16.to_ne_bytes());
        out.extend_from_slice(&libc::IFA_CACHEINFO.to_ne_bytes());
        for value in [lifetime, lifetime, 0, 0] {
            out.extend_from_slice(&value.to_ne_bytes());
        }
        out
    }

    fn address(last: u16) -> KnownAddress {
        KnownAddress {
            family: libc::AF_INET6 as u8,
            prefix_len: 64,
            flags: 0,
            scope: 0,
            index: 2,
            addr: IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, last)),
            plain: true,
        }
    }

    /// The kernel runs the program on a netlink socket's messages exactly as
    /// on a datagram socket's: it sees the message from its `nlmsghdr`.
    #[test]
    fn the_kernel_drops_only_lifetime_refreshes_of_known_addresses() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let known = address(1);
        let mut filter = AddressFilter::new(receiver.as_raw_fd()).unwrap();
        let mut cache = HashMap::new();
        cache.insert((known.index, known.addr), known);
        cache.insert((3, address(2).addr), KnownAddress { index: 3, ..address(2) });
        filter.update(&cache);
        assert!(filter.attached);

        let delivered = |bytes: Vec<u8>| {
            sender.send(&bytes).unwrap();
            let mut buffer = [0_u8; 128];
            receiver.recv(&mut buffer).is_ok()
        };
        assert!(!delivered(message(20, &known, 900)), "a refresh is dropped");
        assert!(!delivered(message(20, &known, 899)), "and the next one");
        assert!(
            !delivered(message(20, &KnownAddress { index: 3, ..address(2) }, 5)),
            "the second known address too"
        );
        assert!(delivered(message(RTM_DELADDR, &known, 900)), "a removal is not");
        assert!(delivered(message(20, &address(9), 900)), "a new address is not");
        assert!(
            delivered(message(20, &KnownAddress { index: 7, ..known }, 900)),
            "nor the same address on another interface"
        );
        assert!(
            delivered(message(20, &KnownAddress { flags: 0x20, ..known }, 900)),
            "nor a change of flags (deprecated)"
        );
        assert!(
            delivered(message(20, &KnownAddress { prefix_len: 48, ..known }, 900)),
            "nor of prefix length"
        );

        cache.clear();
        filter.update(&cache);
        assert!(!filter.attached);
        assert!(delivered(message(20, &known, 900)), "no filter, no drops");
    }

    #[test]
    fn a_long_list_still_jumps_to_the_end() {
        let many: Vec<KnownAddress> = (1..=MAX_FILTERED as u16).map(address).collect();
        let refs: Vec<&KnownAddress> = many.iter().collect();
        let program = program(&refs);
        assert!(program.len() > 255, "{}", program.len());
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let mut filter = AddressFilter::new(receiver.as_raw_fd()).unwrap();
        filter.attach(&program).unwrap();
        sender.send(&message(RTM_DELADDR, &many[0], 1)).unwrap();
        assert!(receiver.recv(&mut [0_u8; 128]).is_ok());
        sender.send(&message(20, many.last().unwrap(), 1)).unwrap();
        assert!(receiver.recv(&mut [0_u8; 128]).is_err(), "the last entry matches");
    }
}
