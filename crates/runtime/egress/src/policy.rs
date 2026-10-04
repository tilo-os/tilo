//! Which names a sandbox may resolve, and so which addresses it may reach.
//!
//! The vocabulary is `collab_types::caps::host_covers`'s: `*` is every
//! host, a leading `.` is a domain and everything under it, anything else is
//! one exact host. `*.example.com` is accepted as a spelling of
//! `.example.com`; it is normalised on the way in, never matched as a glob.

use std::net::IpAddr;

/// One allowed name: a host, or a domain with everything under it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostPattern {
    /// Lower-case, no trailing dot.
    name: String,
    subdomains: bool,
}

impl HostPattern {
    /// Parse a grant's host. `example.com` is that host; `.example.com` and
    /// `*.example.com` are the domain and every name under it.
    ///
    /// # Errors
    /// Returns the reason when `raw` is not a host name: empty, `*` (that is
    /// [`EgressPolicy::Any`], not a pattern), an IP address (egress is decided
    /// by name; an address literal would bypass the resolver that decides),
    /// or a name with characters DNS names do not carry.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim().trim_end_matches('.');
        let (subdomains, name) = if let Some(rest) = trimmed.strip_prefix("*.") {
            (true, rest)
        } else if let Some(rest) = trimmed.strip_prefix('.') {
            (true, rest)
        } else {
            (false, trimmed)
        };
        let name = name.to_ascii_lowercase();
        if name.is_empty() || name == "*" {
            return Err(format!(
                "{raw:?} is not a host: name one (example.com), or a domain with \
                 everything under it (*.example.com)"
            ));
        }
        if name.parse::<IpAddr>().is_ok() || name.starts_with('[') {
            return Err(format!(
                "{raw:?} is an address; hosts are granted by name, so the resolver \
                 that decides what is reachable cannot be bypassed"
            ));
        }
        if name.len() > 253 || !name.split('.').all(is_label) {
            return Err(format!("{raw:?} is not a valid host name"));
        }
        Ok(Self { name, subdomains })
    }

    /// The name, without any `*.`/`.` prefix.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether names under [`Self::name`] are covered too.
    pub fn covers_subdomains(&self) -> bool {
        self.subdomains
    }

    /// Does this pattern cover `host` (lower-case, no trailing dot)?
    pub fn covers(&self, host: &str) -> bool {
        if host == self.name {
            return true;
        }
        self.subdomains
            && host
                .strip_suffix(self.name.as_str())
                .is_some_and(|head| head.ends_with('.') && head.len() > 1)
    }
}

impl std::fmt::Display for HostPattern {
    /// The record's spelling: `example.com` or `.example.com`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.subdomains {
            write!(f, ".{}", self.name)
        } else {
            f.write_str(&self.name)
        }
    }
}

fn is_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// What a sandbox may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressPolicy {
    /// Everything: a `*` grant. The sandbox still has its own namespace.
    Any,
    /// Only the addresses the DNS stub answered for these names. Empty means
    /// no network at all: the stub refuses every name.
    Hosts(Vec<HostPattern>),
}

/// How a name was allowed: by an exact host grant, or under a domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    /// The policy is [`EgressPolicy::Any`].
    Any,
    /// An exact host grant names it.
    Exact,
    /// A domain grant covers it.
    Domain,
}

impl EgressPolicy {
    /// No network at all.
    pub fn none() -> Self {
        Self::Hosts(Vec::new())
    }

    /// A policy from grant spellings (`*`, `example.com`, `.example.com`,
    /// `*.example.com`). A `*` anywhere makes it [`EgressPolicy::Any`];
    /// spellings that are not hosts are skipped with a warning, so a bad
    /// grant narrows rather than widens.
    pub fn from_grants<'a>(grants: impl IntoIterator<Item = &'a str>) -> Self {
        let mut hosts = Vec::new();
        for raw in grants {
            if raw.trim() == "*" {
                return Self::Any;
            }
            match HostPattern::parse(raw) {
                Ok(pattern) => hosts.push(pattern),
                Err(reason) => tracing::warn!(%reason, "ignoring a host grant"),
            }
        }
        Self::hosts(hosts)
    }

    /// A policy allowing exactly `hosts`, deduplicated and sorted.
    pub fn hosts(mut hosts: Vec<HostPattern>) -> Self {
        hosts.sort();
        hosts.dedup();
        Self::Hosts(hosts)
    }

    /// Whether `name` may be resolved, and how it was allowed.
    pub fn allows(&self, name: &str) -> Option<Allowed> {
        let hosts = match self {
            Self::Any => return Some(Allowed::Any),
            Self::Hosts(hosts) => hosts,
        };
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        let mut found = None;
        for pattern in hosts {
            if !pattern.covers(&name) {
                continue;
            }
            if !pattern.subdomains || pattern.name == name {
                return Some(Allowed::Exact);
            }
            found = Some(Allowed::Domain);
        }
        found
    }

    /// Whether the sandbox needs a route out at all.
    pub fn reaches_anything(&self) -> bool {
        match self {
            Self::Any => true,
            Self::Hosts(hosts) => !hosts.is_empty(),
        }
    }

    /// Whether every name `self` allows, `other` allows too.
    pub fn within(&self, other: &Self) -> bool {
        match (self, other) {
            (_, Self::Any) => true,
            (Self::Any, Self::Hosts(_)) => false,
            // A domain grant covering a pattern's name covers everything
            // under it too.
            (Self::Hosts(mine), Self::Hosts(theirs)) => mine.iter().all(|p| {
                theirs
                    .iter()
                    .any(|t| t == p || (t.subdomains && t.covers(&p.name)))
            }),
        }
    }

    /// The grant spellings, for reports.
    pub fn describe(&self) -> Vec<String> {
        match self {
            Self::Any => vec!["*".to_owned()],
            Self::Hosts(hosts) => hosts.iter().map(ToString::to_string).collect(),
        }
    }
}

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
}
