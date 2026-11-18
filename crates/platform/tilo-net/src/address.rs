//! One judge of whether an address is on the public internet, for every
//! caller that keeps a request off this machine and its network: the fetch
//! proxy ([`crate::proxy`]) and the guard of a page's fetch both ask it, and
//! neither keeps a list of its own.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `address` is on the public internet: not loopback, private
/// (RFC 1918, unique-local), link-local, shared (carrier-grade NAT),
/// multicast, unspecified, documentation or otherwise reserved, and not the
/// address a cloud answers its own machines on. An IPv6 address that carries
/// an IPv4 one is judged by it where it is mapped or translated under the
/// well-known prefix; the blocks that carry or stand for one in another way
/// are not public at all.
#[must_use]
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => public_v6(address),
    }
}

fn public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    let reserved = a == 0
        || a == 10
        || a == 127
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // Link-local, 169.254.0.0/16, a cloud's metadata among it.
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 168)
        // IETF protocol assignments, 192.0.0.0/24, and documentation.
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b == 18 || b == 19))
        // Multicast, reserved and broadcast, 224.0.0.0/3.
        || a >= 224
        // The one address Azure answers its own machines on (their metadata
        // and their agent's own service): public by its range, reached only
        // from inside a machine there.
        || address == Ipv4Addr::new(168, 63, 129, 16);
    !reserved
}

fn public_v6(address: Ipv6Addr) -> bool {
    if let Some(v4) = address.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let [s0, s1, s2, s3, s4, s5, s6, s7] = address.segments();
    // NAT64 under the well-known prefix (64:ff9b::/96) reaches the IPv4
    // address it ends in.
    if s0 == 0x0064 && s1 == 0xff9b && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0 {
        let [a, b] = s6.to_be_bytes();
        let [c, d] = s7.to_be_bytes();
        return public_v4(Ipv4Addr::new(a, b, c, d));
    }
    let reserved =
        // ::, ::1 and the deprecated IPv4-compatible addresses, ::/96.
        (s0 == 0 && s1 == 0 && s2 == 0 && s3 == 0 && s4 == 0 && s5 == 0)
        // Blocks that carry or stand for an IPv4 address in a way this does
        // not judge, none of them a place of the public web: the rest of
        // 64:ff9b::/32 with the local-use translation prefix
        // (64:ff9b:1::/48), 6to4 (2002::/16) and the protocol assignments
        // with Teredo among them (2001::/23).
        || (s0 == 0x0064 && s1 == 0xff9b)
        || s0 == 0x2002
        || (s0 == 0x2001 && (s1 & 0xfe00) == 0)
        // Discard-only, 100::/64.
        || (s0 == 0x0100 && s1 == 0 && s2 == 0 && s3 == 0)
        // Documentation, 2001:db8::/32.
        || (s0 == 0x2001 && s1 == 0x0db8)
        // Unique-local, link-local, site-local and multicast.
        || (s0 & 0xfe00) == 0xfc00
        || (s0 & 0xffc0) == 0xfe80
        || (s0 & 0xffc0) == 0xfec0
        || (s0 & 0xff00) == 0xff00;
    // Only global unicast (2000::/3) is routed on the internet.
    !reserved && (s0 & 0xe000) == 0x2000
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every form of an address of this machine or its network is not
    /// public, in IPv6 as in IPv4, and the public web is: the one table the
    /// fetch proxy and a page's fetch are both judged by.
    #[test]
    fn every_form_of_an_own_address_is_not_public() {
        let own = [
            // Unspecified, loopback, private, shared, link-local, reserved.
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.7",
            "127.0.0.1",
            "100.64.0.1",
            "100.100.100.200",
            "169.254.169.254",
            "169.254.170.2",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "192.0.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            // The one address Azure answers its own machines on.
            "168.63.129.16",
            "::",
            "::1",
            "fe80::1",
            "febf::1",
            "fe80::a9fe:a9fe",
            "fc00::1",
            "fd00:ec2::254",
            "fdff:ffff::1",
            "fec0::1",
            "ff02::1",
            // An IPv4 address inside an IPv6 one: mapped, compatible,
            // translated under both prefixes, 6to4 and Teredo.
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.7",
            "::127.0.0.1",
            "::10.0.0.7",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::7f00:1",
            "64:ff9b:1::101:101",
            "2002:7f00:1::",
            "2002:a00:7::1",
            "2002:101:101::",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "2001::1",
            "2001:1::1",
            // Where no page is: discard-only, documentation, and what lies
            // outside global unicast.
            "100::1",
            "2001:db8::1",
            "4000::1",
        ];
        let let_through: Vec<&str> = own
            .into_iter()
            .filter(|address| is_public(address.parse().unwrap()))
            .collect();
        assert!(
            let_through.is_empty(),
            "addresses of this machine or its network judged public: {let_through:#?}"
        );
        for public in [
            "1.1.1.1",
            "93.184.216.34",
            "172.32.0.1",
            "100.128.0.1",
            "168.63.129.17",
            "2606:4700:4700::1111",
            "2a00:1450:4001:81b::200e",
            "2001:4860:4860::8888",
            "::ffff:1.1.1.1",
            "64:ff9b::5db8:d822",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public} is public");
        }
    }
}
