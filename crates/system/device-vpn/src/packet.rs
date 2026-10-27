//! The little of IPv4 the tunnel reads and writes: header checks, the ICMP
//! "fragmentation needed" that tells a sender the tunnel's size, and the UDP
//! packet the exit answers a DNS query with.

use std::net::{Ipv4Addr, SocketAddrV4};

const ICMP: u8 = 1;
const UDP: u8 = 17;
const HEADER_LEN: usize = 20;
const UDP_HEADER_LEN: usize = 8;
const TTL: u8 = 64;
/// The "don't fragment" flag in the flags-and-offset field.
const DONT_FRAGMENT: u16 = 0x4000;
/// The fragment offset bits.
const OFFSET_MASK: u16 = 0x1fff;

/// An IPv4 packet whose header and length agree.
#[derive(Clone, Copy, Debug)]
pub struct Ipv4Packet<'a> {
    bytes: &'a [u8],
    header_len: usize,
}

impl<'a> Ipv4Packet<'a> {
    /// `bytes` as an IPv4 packet, if it is one: version 4, a header of at
    /// least 20 bytes, and a total length that is exactly `bytes`' length.
    #[must_use]
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let first = *bytes.first()?;
        if first >> 4 != 4 {
            return None;
        }
        let header_len = usize::from(first & 0x0f).checked_mul(4)?;
        let total = usize::from(u16::from_be_bytes([*bytes.get(2)?, *bytes.get(3)?]));
        (header_len >= HEADER_LEN && header_len <= total && total == bytes.len())
            .then_some(Self { bytes, header_len })
    }

    fn octets(&self, at: usize) -> Option<Ipv4Addr> {
        let end = at.checked_add(4)?;
        let octets: [u8; 4] = self.bytes.get(at..end)?.try_into().ok()?;
        Some(Ipv4Addr::from(octets))
    }

    #[must_use]
    pub fn source(&self) -> Ipv4Addr {
        self.octets(12).unwrap_or(Ipv4Addr::UNSPECIFIED)
    }

    #[must_use]
    pub fn destination(&self) -> Ipv4Addr {
        self.octets(16).unwrap_or(Ipv4Addr::UNSPECIFIED)
    }

    #[must_use]
    pub fn protocol(&self) -> u8 {
        self.bytes.get(9).copied().unwrap_or(0)
    }

    fn flags_and_offset(&self) -> u16 {
        u16::from_be_bytes([
            self.bytes.get(6).copied().unwrap_or(0),
            self.bytes.get(7).copied().unwrap_or(0),
        ])
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    #[must_use]
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    fn payload(&self) -> &'a [u8] {
        self.bytes.get(self.header_len..).unwrap_or_default()
    }

    /// The ports and payload of a whole (unfragmented) UDP datagram.
    #[must_use]
    pub fn udp(&self) -> Option<(u16, u16, &'a [u8])> {
        if self.protocol() != UDP || self.flags_and_offset() & (OFFSET_MASK | 0x2000) != 0 {
            return None;
        }
        let payload = self.payload();
        let source = u16::from_be_bytes([*payload.first()?, *payload.get(1)?]);
        let destination = u16::from_be_bytes([*payload.get(2)?, *payload.get(3)?]);
        let length = usize::from(u16::from_be_bytes([*payload.get(4)?, *payload.get(5)?]));
        if length < UDP_HEADER_LEN || length > payload.len() {
            return None;
        }
        Some((source, destination, payload.get(UDP_HEADER_LEN..length)?))
    }

    /// Whether an ICMP error may be sent about this packet: never about a
    /// later fragment, nor about an ICMP error.
    fn may_be_answered_with_an_error(&self) -> bool {
        if self.flags_and_offset() & OFFSET_MASK != 0 {
            return false;
        }
        if self.protocol() == ICMP {
            let kind = self.payload().first().copied().unwrap_or(0);
            return matches!(kind, 0 | 8 | 13 | 14 | 15 | 16 | 17 | 18);
        }
        true
    }
}

/// The internet checksum of `chunks`, taken as one byte string.
fn checksum(chunks: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;
    let mut pending: Option<u8> = None;
    for byte in chunks.iter().flat_map(|chunk| chunk.iter().copied()) {
        match pending.take() {
            Some(high) => sum = sum.wrapping_add(u32::from(u16::from_be_bytes([high, byte]))),
            None => pending = Some(byte),
        }
    }
    if let Some(high) = pending {
        sum = sum.wrapping_add(u32::from(u16::from_be_bytes([high, 0])));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff).wrapping_add(sum >> 16);
    }
    !u16::try_from(sum).unwrap_or(u16::MAX)
}

/// A 20-byte IPv4 header for a packet of `total` bytes, its checksum set.
fn header(source: Ipv4Addr, destination: Ipv4Addr, protocol: u8, total: u16) -> [u8; HEADER_LEN] {
    let mut header = [0_u8; HEADER_LEN];
    header[0] = 0x45;
    header[2..4].copy_from_slice(&total.to_be_bytes());
    header[6..8].copy_from_slice(&DONT_FRAGMENT.to_be_bytes());
    header[8] = TTL;
    header[9] = protocol;
    header[12..16].copy_from_slice(&source.octets());
    header[16..20].copy_from_slice(&destination.octets());
    let sum = checksum(&[&header]);
    header[10..12].copy_from_slice(&sum.to_be_bytes());
    header
}

/// The ICMP "destination unreachable, fragmentation needed" answer from
/// `router` to the sender of `original`, saying packets must be at most
/// `mtu` bytes; `None` when no error may be sent about `original`.
#[must_use]
pub fn fragmentation_needed(
    original: &Ipv4Packet<'_>,
    router: Ipv4Addr,
    mtu: u16,
) -> Option<Vec<u8>> {
    if !original.may_be_answered_with_an_error() {
        return None;
    }
    // The original's header and the first eight bytes of what it carried.
    let quoted_len = original.header_len.checked_add(8)?.min(original.len());
    let quoted = original.bytes.get(..quoted_len)?;
    let mut icmp = Vec::with_capacity(quoted_len.checked_add(8)?);
    icmp.extend_from_slice(&[3, 4, 0, 0, 0, 0]);
    icmp.extend_from_slice(&mtu.to_be_bytes());
    icmp.extend_from_slice(quoted);
    let sum = checksum(&[&icmp]);
    icmp.get_mut(2..4)?.copy_from_slice(&sum.to_be_bytes());
    let total = u16::try_from(HEADER_LEN.checked_add(icmp.len())?).ok()?;
    let mut packet = header(router, original.source(), ICMP, total).to_vec();
    packet.extend_from_slice(&icmp);
    Some(packet)
}

/// A UDP datagram from `source` to `destination` carrying `payload`, with
/// both checksums set.
#[must_use]
pub fn udp_packet(
    source: SocketAddrV4,
    destination: SocketAddrV4,
    payload: &[u8],
) -> Option<Vec<u8>> {
    let udp_len = u16::try_from(UDP_HEADER_LEN.checked_add(payload.len())?).ok()?;
    let total = udp_len.checked_add(u16::try_from(HEADER_LEN).ok()?)?;
    let mut udp = Vec::with_capacity(usize::from(udp_len));
    udp.extend_from_slice(&source.port().to_be_bytes());
    udp.extend_from_slice(&destination.port().to_be_bytes());
    udp.extend_from_slice(&udp_len.to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(payload);
    let mut pseudo = [0_u8; 12];
    pseudo[0..4].copy_from_slice(&source.ip().octets());
    pseudo[4..8].copy_from_slice(&destination.ip().octets());
    pseudo[9] = UDP;
    pseudo[10..12].copy_from_slice(&udp_len.to_be_bytes());
    let sum = match checksum(&[&pseudo, &udp]) {
        // Zero means "no checksum" in UDP; all ones says the same sum.
        0 => 0xffff,
        sum => sum,
    };
    udp.get_mut(6..8)?.copy_from_slice(&sum.to_be_bytes());
    let mut packet = header(*source.ip(), *destination.ip(), UDP, total).to_vec();
    packet.extend_from_slice(&udp);
    Some(packet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(text: &str) -> SocketAddrV4 {
        text.parse().unwrap()
    }

    #[test]
    fn built_datagrams_parse_back_with_valid_checksums() {
        let packet = udp_packet(
            addr("100.100.64.1:53"),
            addr("100.100.64.2:40000"),
            b"answer",
        )
        .unwrap();
        let parsed = Ipv4Packet::parse(&packet).expect("a whole packet");
        assert_eq!(parsed.source(), Ipv4Addr::new(100, 100, 64, 1));
        assert_eq!(parsed.destination(), Ipv4Addr::new(100, 100, 64, 2));
        assert_eq!(parsed.udp(), Some((53, 40000, &b"answer"[..])));
        assert_eq!(checksum(&[&packet[..20]]), 0, "header checksum");
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&packet[12..20]);
        pseudo.extend_from_slice(&[0, 17]);
        pseudo.extend_from_slice(&packet[24..26]);
        assert_eq!(checksum(&[&pseudo, &packet[20..]]), 0, "UDP checksum");
    }

    #[test]
    fn packets_whose_header_and_length_disagree_are_not_packets() {
        let packet = udp_packet(addr("10.0.0.1:1"), addr("10.0.0.2:2"), b"x").unwrap();
        assert!(Ipv4Packet::parse(&packet[..packet.len() - 1]).is_none());
        let mut longer = packet.clone();
        longer.push(0);
        assert!(Ipv4Packet::parse(&longer).is_none());
        let mut six = packet;
        six[0] = 0x65;
        assert!(Ipv4Packet::parse(&six).is_none());
        assert!(Ipv4Packet::parse(&[]).is_none());
    }

    #[test]
    fn a_packet_too_big_for_the_tunnel_is_answered_with_its_size() {
        let original = udp_packet(
            addr("100.100.64.2:5000"),
            addr("198.51.100.9:443"),
            &[7; 1400],
        )
        .unwrap();
        let parsed = Ipv4Packet::parse(&original).unwrap();
        let answer = fragmentation_needed(&parsed, Ipv4Addr::new(100, 100, 64, 1), 1180).unwrap();
        let icmp = Ipv4Packet::parse(&answer).unwrap();
        assert_eq!(icmp.protocol(), ICMP);
        assert_eq!(icmp.source(), Ipv4Addr::new(100, 100, 64, 1));
        assert_eq!(icmp.destination(), Ipv4Addr::new(100, 100, 64, 2));
        let body = &answer[20..];
        assert_eq!(&body[..2], &[3, 4]);
        assert_eq!(u16::from_be_bytes([body[6], body[7]]), 1180);
        assert_eq!(
            &body[8..],
            &original[..28],
            "quotes the header and eight bytes"
        );
        assert_eq!(checksum(&[body]), 0);
        assert!(
            fragmentation_needed(&icmp, Ipv4Addr::new(100, 100, 64, 1), 1180).is_none(),
            "no error about an error"
        );
    }
}
