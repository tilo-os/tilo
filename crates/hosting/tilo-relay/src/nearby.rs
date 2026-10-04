//! The nearby rendezvous: how Tilo devices on one local network find each
//! other with no internet and no prior address.
//!
//! Every surface that looks for a nearby device listens on the well-known UDP
//! port [`NEARBY_PORT`] and broadcasts to it on every IPv4 interface. Two
//! conversations share it, told apart by their magic:
//!
//! - **Pairing** (`tilo-enroll`): a new device solicits an enrollment offer
//!   from a device of the identity it is joining.
//! - **Linked devices** ([`linked`]): a session asks the person's devices it
//!   is already linked with where their endpoints are, without saying whose
//!   devices it is looking for.
//!
//! A datagram for one conversation does not parse, or is refused, as the
//! other. Every listener binds with `SO_REUSEADDR`, so a session's
//! always-on listener and a privileged pairing listener (another user)
//! receive the same broadcasts. Answers go to the asker's own socket, never
//! to the shared port.

pub mod linked;

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use socket2::{Domain, Protocol, Socket, Type};

/// Well-known UDP port every nearby listener binds and every asking device
/// broadcasts to. Broadcast discovery has no channel to advertise a port
/// before the first datagram, so the rendezvous must be fixed. It stays below
/// Linux's ephemeral range (32768-60999) so an outgoing socket can never be
/// auto-assigned it and block a listener, and inside the 1025-65535 range the
/// desktop firewall zone accepts. IANA leaves it unassigned and no common
/// service uses it.
pub const NEARBY_PORT: u16 = 23_831;

/// Largest datagram any nearby conversation sends.
pub const MAX_PACKET: usize = 60 * 1024;

/// Bind a nearby listener on `port` without making an already-running
/// listener (a discovery view, an approval sheet, a session's linked-device
/// listener, possibly of another user) prevent another from joining. UDP
/// broadcast is delivered to every listener; each still has to authenticate
/// what it receives before acting on it.
///
/// # Errors
///
/// When the socket cannot be created or bound.
pub fn bind_listener(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())?;
    Ok(socket.into())
}

/// Where a nearby datagram goes: the limited broadcast address, which leaves
/// only through the interface of the default route, and the directed
/// broadcast address of every other up IPv4 interface. A laptop whose default
/// route is a VPN or a second uplink, or a board with no default route at
/// all, still reaches the devices on its LAN.
#[must_use]
pub fn broadcast_destinations(port: u16) -> Vec<SocketAddrV4> {
    let mut destinations = vec![SocketAddrV4::new(Ipv4Addr::BROADCAST, port)];
    #[cfg(target_os = "linux")]
    {
        let mut addresses: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs writes a list head we free below with freeifaddrs.
        if unsafe { libc::getifaddrs(&raw mut addresses) } == 0 {
            let mut cursor = addresses;
            while !cursor.is_null() {
                // SAFETY: `cursor` is a non-null node of the list getifaddrs returned.
                let entry = unsafe { &*cursor };
                cursor = entry.ifa_next;
                let flags = entry.ifa_flags;
                let usable = flags & libc::IFF_UP as libc::c_uint != 0
                    && flags & libc::IFF_BROADCAST as libc::c_uint != 0
                    && flags & libc::IFF_LOOPBACK as libc::c_uint == 0;
                let broadcast = entry.ifa_ifu;
                if !usable || entry.ifa_addr.is_null() || broadcast.is_null() {
                    continue;
                }
                // SAFETY: both pointers are non-null sockaddrs owned by the list.
                let (family, target) = unsafe { ((*entry.ifa_addr).sa_family, &*broadcast) };
                if i32::from(family) != libc::AF_INET
                    || i32::from(target.sa_family) != libc::AF_INET
                {
                    continue;
                }
                // SAFETY: an AF_INET sockaddr is a sockaddr_in.
                let target = unsafe { &*broadcast.cast::<libc::sockaddr_in>() };
                let address = Ipv4Addr::from(u32::from_be(target.sin_addr.s_addr));
                let destination = SocketAddrV4::new(address, port);
                if !address.is_unspecified() && !destinations.contains(&destination) {
                    destinations.push(destination);
                }
            }
            // SAFETY: frees the list getifaddrs allocated, exactly once.
            unsafe { libc::freeifaddrs(addresses) };
        }
    }
    destinations
}

/// Send `bytes` from `socket` to every [`broadcast_destinations`]. An
/// interface that refuses the datagram does not stop the others.
///
/// # Errors
///
/// When no destination accepted the datagram.
pub fn broadcast(socket: &UdpSocket, bytes: &[u8], port: u16) -> io::Result<()> {
    let mut sent = false;
    let mut last_error = None;
    for destination in broadcast_destinations(port) {
        match socket.send_to(bytes, destination) {
            Ok(_) => sent = true,
            Err(error) => last_error = Some(error),
        }
    }
    match last_error {
        Some(error) if !sent => Err(error),
        _ => Ok(()),
    }
}
