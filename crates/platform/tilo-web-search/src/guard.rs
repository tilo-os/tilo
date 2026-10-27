//! Which addresses a fetch may reach: the public web, never this device or
//! the network it sits on.
//!
//! A fetch's address comes from whoever invoked it, an agent following
//! instructions in a page it read included. Where the service runs with the
//! host's network, `http://localhost:631/` or a router's admin page would
//! otherwise be one invocation away, so names and addresses that mean "here"
//! are refused before anything is sent, every redirect hop included.
//!
//! [`check_url`] judges what the address says. [`check_address`] judges what
//! a name resolved to, for callers that can resolve before connecting. Where
//! the launcher's egress gate stands in front of the service, resolution
//! happens at the gate, which refuses device-local destinations itself.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use url::{Host, Url};

/// Why an address is not fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Not `http` or `https`.
    Scheme(String),
    /// The address carries a user name or password.
    Credentials,
    /// The address names this device or its local network.
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

/// Whether `url` may be fetched, judged by what it says.
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
/// documentation, multicast and unspecified addresses, IPv4 ones embedded in
/// IPv6 included.
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
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    let segments = ip.segments();
    let [first, second, ..] = segments;
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
}
