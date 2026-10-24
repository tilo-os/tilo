//! The tunnel's fixed addressing, and IPv4 networks as requests carry them.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The client's tunnel interface.
pub const CLIENT_INTERFACE: &str = "tilovpn0";
/// The exit's tunnel interface.
pub const EXIT_INTERFACE: &str = "tiloexit0";

/// The tunnel network every exit serves: the exit is [`GATEWAY`], its
/// clients get the other host addresses.
pub const TUNNEL_NETWORK: Ipv4Net = Ipv4Net {
    address: Ipv4Addr::new(100, 100, 64, 0),
    prefix: 24,
};
/// The exit's own address in [`TUNNEL_NETWORK`], and the DNS server clients
/// that resolve through the exit ask.
pub const GATEWAY: Ipv4Addr = Ipv4Addr::new(100, 100, 64, 1);

/// The smallest MTU an IPv4 link may have.
pub const MIN_MTU: u16 = 576;
/// The largest MTU either side asks for: a QUIC datagram never carries more.
pub const MAX_MTU: u16 = 1500;

/// Whether `address` is one a client may be given: a host of
/// [`TUNNEL_NETWORK`] other than the gateway.
#[must_use]
pub fn is_client_address(address: Ipv4Addr) -> bool {
    let host = address.octets()[3];
    TUNNEL_NETWORK.contains(address) && (2..=254).contains(&host)
}

/// An IPv4 network in canonical form: no host bits beyond the prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ipv4Net {
    address: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Net {
    /// The network `address/prefix`, or `None` when the prefix is out of
    /// range or `address` has host bits set.
    #[must_use]
    pub fn new(address: Ipv4Addr, prefix: u8) -> Option<Self> {
        let net = Self { address, prefix };
        (prefix <= 32 && u32::from(address) & !net.mask() == 0).then_some(net)
    }

    /// The network `address` lies in with `prefix`.
    #[must_use]
    pub fn containing(address: Ipv4Addr, prefix: u8) -> Option<Self> {
        let mask = Self {
            address: Ipv4Addr::UNSPECIFIED,
            prefix: prefix.min(32),
        }
        .mask();
        Self::new(Ipv4Addr::from(u32::from(address) & mask), prefix)
    }

    #[must_use]
    pub const fn address(self) -> Ipv4Addr {
        self.address
    }

    #[must_use]
    pub const fn prefix(self) -> u8 {
        self.prefix
    }

    fn mask(self) -> u32 {
        32_u32
            .checked_sub(u32::from(self.prefix))
            .and_then(|shift| u32::MAX.checked_shl(shift))
            .unwrap_or(0)
    }

    #[must_use]
    pub fn contains(self, address: Ipv4Addr) -> bool {
        u32::from(address) & self.mask() == u32::from(self.address)
    }

    /// Whether the two networks share any address.
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.contains(other.address) || other.contains(self.address)
    }

    /// Whether the network is one an exit's home network may be: unicast,
    /// not loopback, link-local, multicast or the tunnel's own, and not the
    /// whole internet (that is all traffic, said otherwise).
    #[must_use]
    pub fn is_routable_home(self) -> bool {
        let first = self.address.octets()[0];
        self.prefix >= 8
            && !self.address.is_loopback()
            && !self.address.is_link_local()
            && !self.address.is_multicast()
            && !self.address.is_broadcast()
            && first != 0
            && first < 224
            && !self.overlaps(TUNNEL_NETWORK)
    }
}

impl fmt::Display for Ipv4Net {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.address, self.prefix)
    }
}

impl FromStr for Ipv4Net {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (address, prefix) = text
            .split_once('/')
            .ok_or_else(|| format!("{text} is not address/prefix"))?;
        let address: Ipv4Addr = address
            .parse()
            .map_err(|_| format!("{text} has no IPv4 address"))?;
        let prefix: u8 = prefix
            .parse()
            .map_err(|_| format!("{text} has no prefix length"))?;
        Self::new(address, prefix).ok_or_else(|| format!("{text} is not a canonical network"))
    }
}

impl Serialize for Ipv4Net {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Ipv4Net {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// Whether `address` may be exempted from the tunnel as the address of a
/// relay the carrier uses: a unicast address that is neither loopback,
/// unspecified nor inside the tunnel.
#[must_use]
pub fn is_relay_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => {
            !v4.is_unspecified()
                && !v4.is_loopback()
                && !v4.is_multicast()
                && !v4.is_broadcast()
                && !TUNNEL_NETWORK.contains(v4)
        }
        IpAddr::V6(v6) => !v6.is_unspecified() && !v6.is_loopback() && !v6.is_multicast(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn networks_are_canonical_and_round_trip_as_text() {
        let net: Ipv4Net = "192.168.1.0/24".parse().unwrap();
        assert_eq!(net.to_string(), "192.168.1.0/24");
        assert!(
            "192.168.1.7/24".parse::<Ipv4Net>().is_err(),
            "host bits set"
        );
        assert!("10.0.0.0/33".parse::<Ipv4Net>().is_err());
        assert_eq!(
            Ipv4Net::containing(Ipv4Addr::new(192, 168, 1, 7), 24),
            Some(net)
        );
        let json = serde_json::to_string(&net).unwrap();
        assert_eq!(json, r#""192.168.1.0/24""#);
        assert_eq!(serde_json::from_str::<Ipv4Net>(&json).unwrap(), net);
        assert!(serde_json::from_str::<Ipv4Net>(r#""1.2.3.4/8""#).is_err());
        assert_eq!(
            Ipv4Net::new(Ipv4Addr::UNSPECIFIED, 0).map(Ipv4Net::mask),
            Some(0)
        );
    }

    #[test]
    fn home_networks_exclude_what_must_never_be_routed_to_an_exit() {
        for good in [
            "192.168.1.0/24",
            "10.0.0.0/8",
            "172.16.4.0/22",
            "203.0.113.0/24",
        ] {
            assert!(
                good.parse::<Ipv4Net>().unwrap().is_routable_home(),
                "{good}"
            );
        }
        for bad in [
            "0.0.0.0/0",
            "127.0.0.0/8",
            "169.254.0.0/16",
            "224.0.0.0/4",
            "100.100.64.0/24",
            "100.100.0.0/16",
            "100.100.64.128/25",
            "0.0.0.0/8",
        ] {
            assert!(!bad.parse::<Ipv4Net>().unwrap().is_routable_home(), "{bad}");
        }
    }

    #[test]
    fn clients_get_tunnel_hosts_other_than_the_gateway() {
        assert!(is_client_address(Ipv4Addr::new(100, 100, 64, 2)));
        assert!(is_client_address(Ipv4Addr::new(100, 100, 64, 254)));
        assert!(!is_client_address(GATEWAY));
        assert!(!is_client_address(Ipv4Addr::new(100, 100, 64, 255)));
        assert!(!is_client_address(Ipv4Addr::new(100, 100, 64, 0)));
        assert!(!is_client_address(Ipv4Addr::new(100, 100, 65, 2)));
    }
}
