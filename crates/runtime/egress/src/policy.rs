//! Which names a sandbox may resolve, and so which addresses it may reach.
//!
//! The grammar and the decision are `collab_types::caps::net`'s — the same
//! [`EgressPolicy`] the launcher's gate enforces for apps — so a grant means
//! one thing to an app and to an environment: `*` is every host, a leading
//! `.` (or `*.`) is a domain and everything under it, anything else is one
//! exact host, and an IP literal is not a host. What is this crate's own is
//! the backend: which *addresses* the DNS stub may admit for an allowed
//! name ([`admissible`]).

use std::net::IpAddr;

pub use collab_types::caps::{Allowed, EgressPolicy, HostPattern, NetHostError};

/// Whether an address a resolver answered for an allowed name may be
/// admitted. Never the unspecified, multicast, broadcast, link-local
/// (cloud metadata lives at `169.254.169.254`) or loopback addresses — the
/// last is the sandbox's own anyway. Private and shared ranges only for an
/// exact host grant: a name under a granted domain may belong to someone
/// else, who could point it at the local network (DNS rebinding).
pub fn admissible(addr: IpAddr, how: Allowed) -> bool {
    match addr {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            if v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || a == 0
                || a >= 240
            {
                return false;
            }
            let local = v4.is_private() || (a == 100 && (64..128).contains(&b));
            !local || how != Allowed::Domain
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return admissible(IpAddr::V4(mapped), how);
            }
            let first = v6.segments().first().copied().unwrap_or_default();
            if v6.is_unspecified() || v6.is_loopback() || v6.is_multicast() {
                return false;
            }
            // fe80::/10, link-local.
            if first & 0xffc0 == 0xfe80 {
                return false;
            }
            // fc00::/7, unique local.
            let local = first & 0xfe00 == 0xfc00;
            !local || how != Allowed::Domain
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(grants: &[&str]) -> EgressPolicy {
        EgressPolicy::from_grants(grants.iter().copied())
    }

    #[test]
    fn exact_hosts_match_only_themselves_and_domains_match_below() {
        let p = policy(&["github.com", "*.crates.io", ".fedoraproject.org"]);
        assert_eq!(p.allows("github.com"), Some(Allowed::Exact));
        assert_eq!(p.allows("GitHub.COM."), Some(Allowed::Exact));
        assert_eq!(p.allows("api.github.com"), None, "an exact grant is exact");
        assert_eq!(p.allows("evilgithub.com"), None);
        assert_eq!(p.allows("static.crates.io"), Some(Allowed::Domain));
        assert_eq!(p.allows("a.b.crates.io"), Some(Allowed::Domain));
        assert_eq!(
            p.allows("crates.io"),
            Some(Allowed::Exact),
            "a domain grant covers the domain's own name"
        );
        assert_eq!(
            p.allows("notcrates.io"),
            None,
            "a suffix is not a subdomain"
        );
        assert_eq!(p.allows("mirrors.fedoraproject.org"), Some(Allowed::Domain));
        assert_eq!(p.allows("io"), None);
        assert_eq!(p.allows(""), None);
    }

    #[test]
    fn a_star_is_everything_and_nothing_is_nothing() {
        assert_eq!(policy(&["github.com", "*"]), EgressPolicy::Any);
        assert_eq!(
            policy(&["*"]).allows("anything.example"),
            Some(Allowed::Any)
        );
        let none = policy(&[]);
        assert_eq!(none, EgressPolicy::none());
        assert!(!none.reaches_anything());
        assert_eq!(none.allows("example.com"), None);
    }

    #[test]
    fn addresses_and_junk_are_not_hosts() {
        for bad in [
            "",
            "*",
            "*.",
            "1.1.1.1",
            "::1",
            "[::1]",
            "a b.com",
            "-x.com",
            "x..com",
            "exa$mple.com",
        ] {
            assert!(HostPattern::parse(bad).is_err(), "{bad:?} parsed");
        }
        // A bad grant narrows, it never widens.
        assert_eq!(policy(&["1.1.1.1"]), EgressPolicy::none());
        assert_eq!(
            HostPattern::parse("*.Example.com.").unwrap().to_string(),
            ".example.com"
        );
    }

    #[test]
    fn narrowing_is_detected() {
        let wide = policy(&["*.example.com", "github.com"]);
        let narrow = policy(&["api.example.com"]);
        assert!(narrow.within(&wide));
        assert!(!wide.within(&narrow));
        assert!(wide.within(&EgressPolicy::Any));
        assert!(!EgressPolicy::Any.within(&wide));
        assert!(EgressPolicy::none().within(&narrow));
        assert!(policy(&["example.com"]).within(&policy(&[".example.com"])));
        assert!(!policy(&[".example.com"]).within(&policy(&["example.com"])));
    }

    #[test]
    fn local_and_special_addresses_are_not_admitted_from_a_domain() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        for never in [
            "0.0.0.0",
            "127.0.0.1",
            "169.254.169.254",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fe80::1",
            "ff02::1",
            "::ffff:169.254.169.254",
        ] {
            assert!(!admissible(ip(never), Allowed::Exact), "{never}");
            assert!(!admissible(ip(never), Allowed::Any), "{never}");
        }
        for local in [
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "100.64.0.1",
            "fd00::1",
        ] {
            assert!(admissible(ip(local), Allowed::Exact), "{local}");
            assert!(!admissible(ip(local), Allowed::Domain), "{local}");
        }
        for global in ["140.82.112.3", "2606:4700::1111"] {
            assert!(admissible(ip(global), Allowed::Domain), "{global}");
        }
    }

    /// Apps and environments answer every row of the shared vector file
    /// identically: this is the environment side, through the policy the DNS
    /// stub consults; `launcher`'s `app_egress_answers_every_shared_vector`
    /// is the app side.
    #[test]
    fn environment_egress_answers_every_shared_vector() {
        let vectors = include_str!("../../../collab/collab-types/tests/net_host_vectors.txt");
        let rows: Vec<(&str, &str, bool)> = vectors
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(
                |line| match line.split_whitespace().collect::<Vec<_>>()[..] {
                    [grant, host, "yes"] => (grant, host, true),
                    [grant, host, "no"] => (grant, host, false),
                    _ => panic!("malformed vector line {line:?}"),
                },
            )
            .collect();
        assert!(rows.len() > 20, "the vector file was read: {rows:?}");
        for (grant, host, covered) in rows {
            assert_eq!(
                policy(&[grant]).allows(host).is_some(),
                covered,
                "an environment granted {grant:?} asking for {host:?}"
            );
        }
    }
}
