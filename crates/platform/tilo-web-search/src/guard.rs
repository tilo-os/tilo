//! Which addresses a fetch may reach: the public web, never this device or the network
//! it sits on.
//!
//! A fetch's address comes from whoever invoked it, an agent following instructions in a
//! page it read included, so names and addresses that mean "here" are refused before
//! anything is sent, every redirect hop included. [`check_url`] judges what the address
//! says, [`check_address`] what a name resolved to.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// Why an address is not fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Scheme(String),
    Credentials,
    Local(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scheme(scheme) => write!(
                f,
                "only http and https addresses are fetched, not {scheme}:"
            ),
            Self::Credentials => {
                f.write_str("addresses with a user name or password are not fetched")
            }
            Self::Local(host) => write!(
                f,
                "{host} is on this device or its local network; only public web addresses are fetched"
            ),
        }
    }
}

/// Top-level names that only ever mean a private network.
const LOCAL_SUFFIXES: &[&str] = &[
    "localhost",
    "local",
    "lan",
    "internal",
    "intranet",
    "home.arpa",
    "home",
    "corp",
    "localdomain",
];

pub fn check_url(url: &Url) -> Result<(), Refusal> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Refusal::Scheme(url.scheme().to_owned()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Refusal::Credentials);
    }
    match url.host() {
        None => Err(Refusal::Local(String::new())),
        Some(Host::Ipv4(ip)) => check_address(IpAddr::V4(ip)),
        Some(Host::Ipv6(ip)) => check_address(IpAddr::V6(ip)),
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            let local = !name.contains('.')
                || LOCAL_SUFFIXES
                    .iter()
                    .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")));
            if local {
                Err(Refusal::Local(name))
            } else {
                Ok(())
            }
        }
    }
}

/// Whether a resolved address is on the public internet.
///
/// # Errors
///
/// [`Refusal::Local`] for loopback, private, link-local, shared, reserved,
/// documentation, multicast and unspecified addresses, the address a cloud
/// answers its own machines on, and every IPv6 address that carries or stands
/// for an IPv4 one that is not judged as itself.
pub fn check_address(ip: IpAddr) -> Result<(), Refusal> {
    let public = match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    };
    if public {
        Ok(())
    } else {
        Err(Refusal::Local(ip.to_string()))
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        // Shared address space (carrier-grade NAT), 100.64.0.0/10.
        || (a == 100 && (64..128).contains(&b))
        // IETF protocol assignments, 192.0.0.0/24.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking, 198.18.0.0/15.
        || (a == 198 && (b == 18 || b == 19))
        // Reserved, 240.0.0.0/4.
        || a >= 240
        // The one address Azure answers its own machines on (their metadata and
        // their agent's own service): public by its range, reached only from
        // inside a machine there.
        || ip == Ipv4Addr::new(168, 63, 129, 16))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let segments = ip.segments();
    let [first, second, third, fourth, ..] = segments;
    // NAT64 (64:ff9b::/96) carries an IPv4 address in its last 32 bits.
    if first == 0x0064
        && second == 0xff9b
        && segments
            .get(2..6)
            .is_some_and(|s| s.iter().all(|&x| x == 0))
    {
        let [.., a, b, c, d] = ip.octets();
        return public_v4(Ipv4Addr::new(a, b, c, d));
    }
    // Blocks that carry or stand for an IPv4 address this does not judge as
    // itself, none of them a place a page of the public web is: the local-use
    // translation prefix (64:ff9b:1::/48), 6to4 (2002::/16) and the protocol
    // assignments with Teredo among them (2001::/23). So is the block that
    // discards what it is sent (100::/64).
    if (first == 0x0064 && second == 0xff9b && third == 0x0001)
        || first == 0x2002
        || (first == 0x2001 && (second & 0xfe00) == 0)
        || (first == 0x0100 && second == 0 && third == 0 && fourth == 0)
    {
        return false;
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // Unique local, fc00::/7.
        || (first & 0xfe00) == 0xfc00
        // Link-local, fe80::/10, and the old site-local, fec0::/10.
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        // Documentation, 2001:db8::/32.
        || (first == 0x2001 && second == 0x0db8)
        // The deprecated IPv4-compatible ::/96.
        || segments.get(..6).is_some_and(|s| s.iter().all(|&x| x == 0)))
}

/// The system's resolver, as the lookup a client dials by.
#[cfg(not(target_family = "wasm"))]
#[must_use]
pub fn system_lookup() -> tilo_net::Lookup {
    std::sync::Arc::new(|name: String| {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((name.as_str(), 0))
                .await?
                .map(|address| address.ip())
                .collect())
        })
    })
}

/// `lookup`, answering a name only when every address it resolves to is on
/// the public internet ([`check_address`]): the lookup of a client that
/// reaches the public web and nothing else.
///
/// A name that resolves to both public and local addresses is refused,
/// since the connection might take either. The refusal is made where the
/// connection gets its addresses, so a name that answers differently from
/// one lookup to the next is dialled only where this lookup saw it, and a
/// look at the name made earlier decides nothing.
#[cfg(not(target_family = "wasm"))]
#[must_use]
pub fn public_only(lookup: tilo_net::Lookup) -> tilo_net::Lookup {
    std::sync::Arc::new(move |name: String| {
        let lookup = std::sync::Arc::clone(&lookup);
        Box::pin(async move {
            let addresses = lookup(name.clone()).await?;
            if addresses
                .iter()
                .any(|address| check_address(*address).is_err())
            {
                return Err(tilo_net::refused_lookup(Refusal::Local(name).to_string()));
            }
            Ok(addresses)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(url: &str) -> Result<(), Refusal> {
        check_url(&Url::parse(url).unwrap())
    }

    #[test]
    fn public_web_addresses_pass() {
        for ok in [
            "https://example.org/a?b",
            "http://en.wikipedia.org:8080/",
            "https://93.184.216.34/",
            "https://[2606:4700::1111]/",
        ] {
            assert_eq!(check(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn this_device_and_its_network_are_refused() {
        for local in [
            "http://localhost:631/",
            "http://app.localhost/",
            "http://router/",
            "http://printer.local/",
            "http://nas.home.arpa/",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://172.16.0.1/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[64:ff9b::a00:1]/",
            "http://2130706433/",
        ] {
            assert!(matches!(check(local), Err(Refusal::Local(_))), "{local}");
        }
        assert_eq!(
            check("file:///etc/passwd"),
            Err(Refusal::Scheme("file".into()))
        );
        assert_eq!(
            check("https://user:pw@example.org/"),
            Err(Refusal::Credentials)
        );
    }

    /// Every form of an address of this machine or its network is refused, in IPv6 as in
    /// IPv4: loopback, link-local, unique local, the unspecified address, an IPv4 address
    /// written inside an IPv6 one in each way one can be, and the addresses a cloud answers
    /// its own machines on. What is let through is named, all of it at once.
    #[test]
    fn every_form_of_an_own_address_is_refused_in_ipv6_too() {
        let own = [
            // The unspecified address and loopback.
            "http://[::]/",
            "http://[::1]/",
            "http://0.0.0.0/",
            // Link-local, unique local and the old site-local.
            "http://[fe80::1]/",
            "http://[febf::1]/",
            "http://[fc00::1]/",
            "http://[fdff:ffff::1]/",
            "http://[fec0::1]/",
            // What a cloud answers its own machines on.
            "http://169.254.169.254/latest/meta-data/",
            "http://169.254.170.2/",
            "http://[fd00:ec2::254]/",
            "http://[fe80::a9fe:a9fe]/",
            "http://100.100.100.200/",
            "http://168.63.129.16/",
            // An IPv4 address written inside an IPv6 one: mapped, compatible, translated
            // (both prefixes), 6to4 and Teredo.
            "http://[::ffff:127.0.0.1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[::ffff:10.0.0.7]/",
            "http://[::127.0.0.1]/",
            "http://[::10.0.0.7]/",
            "http://[64:ff9b::7f00:1]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[64:ff9b:1::7f00:1]/",
            "http://[64:ff9b:1::a9fe:a9fe]/",
            "http://[2002:7f00:1::]/",
            "http://[2002:a9fe:a9fe::1]/",
            "http://[2002:a00:7::1]/",
            "http://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/",
            "http://[2001::1]/",
            // Where no page is: discard-only, documentation, multicast.
            "http://[100::1]/",
            "http://[2001:db8::1]/",
            "http://[ff02::1]/",
        ];
        let let_through: Vec<&str> = own
            .into_iter()
            .filter(|address| check(address).is_ok())
            .collect();
        assert!(
            let_through.is_empty(),
            "addresses of this machine or its network that are let through: {let_through:#?}"
        );
        // The public web still passes in both families, also where an IPv6 address
        // translates a public IPv4 one.
        for public in [
            "https://[2606:4700::1111]/",
            "https://[2a00:1450:4001:81b::200e]/",
            "https://[2001:4860:4860::8888]/",
            "https://[64:ff9b::5db8:d822]/",
            "https://93.184.216.34/",
            "https://168.63.129.17/",
        ] {
            assert_eq!(check(public), Ok(()), "{public}");
        }
    }
}
