//! The one grammar for [`Scope::NetHost`](super::Scope::NetHost) grants, and
//! the one egress policy built from them.
//!
//! A grant is stored as the string it was written as — `Scope::NetHost` is
//! replicated in workspace documents and environment records, so its wire
//! shape never changes — and parsed here whenever it is read:
//!
//! - `*` is every host.
//! - `example.com` is that one host.
//! - `.example.com` is the domain and every name under it. `*.example.com`
//!   is accepted as a spelling of `.example.com`; it is normalised on the
//!   way in, never matched as a glob.
//!
//! A package manifest's `[sandbox.net].hosts` is a declaration, not a grant:
//! there `*.example.com` is every name under `example.com` and **not**
//! `example.com` itself, which needs its own entry. [`EgressPolicy::from_declared`]
//! reads that spelling; the launcher narrows it by the person's grants.
//!
//! Names compare lower-case and without a trailing dot, so `Example.COM.`
//! is `example.com`. An **IP literal is not a host**: egress is decided by
//! name, and an address literal would bypass the resolver that decides. A
//! grant that does not parse covers nothing — a bad grant narrows, it never
//! widens.
//!
//! Every enforcer reads this grammar through [`EgressPolicy`]: the
//! launcher's check (apps, Windows capsules, the Windows guest relay) and
//! `tilo-egress`'s DNS stub (environments, builds). They differ in backend
//! only.

use std::fmt;
use std::net::IpAddr;

/// Why a grant's host is not a host.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetHostError {
    /// Empty, or a bare wildcard where a name was expected.
    #[error(
        "{0:?} is not a host: name one (example.com), a domain with everything under it \
         (*.example.com), or * for any host"
    )]
    NotAHost(String),
    /// An IP address literal.
    #[error(
        "{0:?} is an address; hosts are granted by name, so the resolver that decides what \
         is reachable cannot be bypassed"
    )]
    Address(String),
    /// Characters or lengths DNS names do not carry.
    #[error("{0:?} is not a valid host name")]
    Invalid(String),
}

/// One named grant: a host, or a domain with everything under it. Never
/// `*` — that is [`NetHostPattern::Any`] / [`EgressPolicy::Any`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostPattern {
    /// Lower-case, no trailing dot, no `*.`/`.` prefix.
    name: String,
    /// The names strictly under `name` are covered.
    subdomains: bool,
    /// `name` itself is covered. A grant's `.name`/`*.name` covers the apex;
    /// a package manifest's `*.name` covers only what is under it.
    apex: bool,
}

impl HostPattern {
    /// Parse a grant's host. `example.com` is that host; `.example.com` and
    /// `*.example.com` are the domain and every name under it.
    ///
    /// # Errors
    /// Returns the reason when `raw` is not a host name: empty, `*` (that is
    /// [`NetHostPattern::Any`], not a name), an IP address, or a name with
    /// characters DNS names do not carry.
    pub fn parse(raw: &str) -> Result<Self, NetHostError> {
        let trimmed = raw.trim().trim_end_matches('.');
        let (subdomains, name) = if let Some(rest) = trimmed.strip_prefix("*.") {
            (true, rest)
        } else if let Some(rest) = trimmed.strip_prefix('.') {
            (true, rest)
        } else {
            (false, trimmed)
        };
        let name = validated_name(raw, name)?;
        Ok(Self {
            name,
            subdomains,
            apex: true,
        })
    }

    /// A package manifest's `*.name`: every name under `name`, never `name`
    /// itself. A grant's `*.name` is a spelling of `.name` and covers the
    /// apex ([`Self::parse`]); this is the declared meaning, so no grant
    /// spelling produces it.
    ///
    /// # Errors
    /// As [`Self::parse`], for the domain part.
    pub fn subdomains_only(raw: &str) -> Result<Self, NetHostError> {
        let trimmed = raw.trim().trim_end_matches('.');
        let name = validated_name(raw, trimmed)?;
        Ok(Self {
            name,
            subdomains: true,
            apex: false,
        })
    }

    /// The name, without any `*.`/`.` prefix.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Does this pattern cover `host` (lower-case, no trailing dot — see
    /// [`NetHostPattern::covers`] for one that normalises)?
    pub fn covers(&self, host: &str) -> bool {
        if host == self.name {
            return self.apex;
        }
        self.subdomains && is_strictly_under(host, &self.name)
    }

    /// Does this pattern name `host` **exactly** (lower-case, no trailing
    /// dot)? A domain grant covers names without pointing at one machine.
    pub fn names_exactly(&self, host: &str) -> bool {
        !self.subdomains && host == self.name
    }

    /// Does this pattern allow every name `other` allows? A domain pattern
    /// covering the other's name covers everything under it too.
    fn includes(&self, other: &Self) -> bool {
        if self == other {
            return true;
        }
        if other.subdomains {
            // Everything strictly under `other.name` is under `self.name`
            // too, and the apex needs covering by itself.
            self.subdomains
                && (self.name == other.name || is_strictly_under(&other.name, &self.name))
                && (!other.apex || self.covers(&other.name))
        } else {
            self.covers(&other.name)
        }
    }
}

impl fmt::Display for HostPattern {
    /// The record's spelling: `example.com`, `.example.com` or a manifest's
    /// `*.example.com`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.subdomains {
            if self.apex {
                write!(f, ".{}", self.name)
            } else {
                write!(f, "*.{}", self.name)
            }
        } else {
            f.write_str(&self.name)
        }
    }
}

/// Is `host` a name strictly under `domain`, with a label of its own?
fn is_strictly_under(host: &str, domain: &str) -> bool {
    host.strip_suffix(domain)
        .is_some_and(|head| head.ends_with('.') && head.len() > 1)
}

/// The normalised name a pattern holds, or the reason `raw` is not one.
fn validated_name(raw: &str, name: &str) -> Result<String, NetHostError> {
    let name = name.to_ascii_lowercase();
    if name.is_empty() || name == "*" {
        return Err(NetHostError::NotAHost(raw.to_owned()));
    }
    if name.parse::<IpAddr>().is_ok() || name.starts_with('[') {
        return Err(NetHostError::Address(raw.to_owned()));
    }
    if name.len() > 253 || !name.split('.').all(is_label) {
        return Err(NetHostError::Invalid(raw.to_owned()));
    }
    Ok(name)
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

/// A host as a lookup asks for it, in the form patterns compare against.
fn normalise_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

/// A parsed `Scope::NetHost` grant: `*`, or one [`HostPattern`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NetHostPattern {
    /// `*`: every host.
    Any,
    Named(HostPattern),
}

impl NetHostPattern {
    /// The one grammar: `*`, `example.com`, `.example.com`, `*.example.com`
    /// (= `.example.com`).
    ///
    /// # Errors
    /// As [`HostPattern::parse`], except that `*` is [`Self::Any`].
    pub fn parse(raw: &str) -> Result<Self, NetHostError> {
        if raw.trim() == "*" {
            return Ok(Self::Any);
        }
        HostPattern::parse(raw).map(Self::Named)
    }

    /// Does this grant cover `host`? `host` is compared case-insensitively
    /// and without a trailing dot. An address literal is covered only by
    /// [`Self::Any`].
    pub fn covers(&self, host: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Named(pattern) => pattern.covers(&normalise_host(host)),
        }
    }

    /// Does this grant name `host` exactly? Never true for `*` or a domain.
    pub fn names_exactly(&self, host: &str) -> bool {
        match self {
            Self::Any => false,
            Self::Named(pattern) => pattern.names_exactly(&normalise_host(host)),
        }
    }
}

impl fmt::Display for NetHostPattern {
    /// The record's spelling: `*`, `example.com` or `.example.com`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Any => f.write_str("*"),
            Self::Named(pattern) => pattern.fmt(f),
        }
    }
}

/// Does the host grant `pattern` cover `host`? [`NetHostPattern::parse`]
/// then [`NetHostPattern::covers`]; a pattern that does not parse covers
/// nothing.
///
/// `host` may itself be a grant spelling — the launcher and `ush` ask
/// whether a grant stands by asking about its own scope — and is covered
/// when everything it names is: `.crates.io` by `.crates.io`, `.io` or `*`;
/// `*` only by `*`.
pub fn host_covers(pattern: &str, host: &str) -> bool {
    let Ok(pattern) = NetHostPattern::parse(pattern) else {
        return false;
    };
    match NetHostPattern::parse(host) {
        Ok(NetHostPattern::Any) => pattern == NetHostPattern::Any,
        Ok(NetHostPattern::Named(target)) if target.subdomains => match &pattern {
            NetHostPattern::Any => true,
            NetHostPattern::Named(p) => p.subdomains && p.covers(&target.name),
        },
        _ => pattern.covers(host),
    }
}

/// What a sandbox may reach: the decision every egress enforcer reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressPolicy {
    /// Everything: a `*` grant. How much of *this device* that includes is
    /// the backend's — see [`Self::permits_device_local`].
    Any,
    /// Only these names. Empty means no network at all.
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
    /// spellings that are not hosts — IP literals included — are skipped
    /// with a warning, so a bad grant narrows rather than widens.
    pub fn from_grants<'a>(grants: impl IntoIterator<Item = &'a str>) -> Self {
        let mut hosts = Vec::new();
        for raw in grants {
            match NetHostPattern::parse(raw) {
                Ok(NetHostPattern::Any) => return Self::Any,
                Ok(NetHostPattern::Named(pattern)) => hosts.push(pattern),
                Err(reason) => tracing::warn!(%reason, "ignoring a network grant"),
            }
        }
        Self::hosts(hosts)
    }

    /// A policy from a package manifest's declared `[sandbox.net].hosts`:
    /// `example.com` is that host, and `*.example.com` is every name under
    /// it — never `example.com` itself. `*` is [`Self::Any`], which an older
    /// manifest may carry. A spelling that is not a host is skipped with a
    /// warning, so a bad declaration narrows rather than widens.
    pub fn from_declared<'a>(hosts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut patterns = Vec::new();
        for raw in hosts {
            let trimmed = raw.trim();
            if trimmed == "*" {
                return Self::Any;
            }
            let parsed = match trimmed
                .strip_prefix("*.")
                .or_else(|| trimmed.strip_prefix('.'))
            {
                // The manifest spells the wildcard `*.`; a leading dot is
                // still the names under the domain, so it only narrows.
                Some(domain) => HostPattern::subdomains_only(domain),
                None => HostPattern::parse(trimmed),
            };
            match parsed {
                Ok(pattern) => patterns.push(pattern),
                Err(reason) => tracing::warn!(%reason, "ignoring a declared network host"),
            }
        }
        Self::hosts(patterns)
    }

    /// A policy allowing exactly `hosts`, deduplicated and sorted.
    pub fn hosts(mut hosts: Vec<HostPattern>) -> Self {
        hosts.sort();
        hosts.dedup();
        Self::Hosts(hosts)
    }

    /// Whether `name` may be reached, and how it was allowed.
    pub fn allows(&self, name: &str) -> Option<Allowed> {
        let hosts = match self {
            Self::Any => return Some(Allowed::Any),
            Self::Hosts(hosts) => hosts,
        };
        let name = normalise_host(name);
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

    pub fn permits(&self, host: &str) -> bool {
        self.allows(host).is_some()
    }

    /// Does a grant name `host` **exactly**, not merely cover it? `*` and
    /// `.example.com` cover a host without pointing at one machine; only a
    /// literal entry does.
    pub fn names_exactly(&self, host: &str) -> bool {
        match self {
            Self::Any => false,
            Self::Hosts(hosts) => {
                let host = normalise_host(host);
                hosts.iter().any(|p| p.names_exactly(&host))
            }
        }
    }

    /// May a connection to `host` land on an address of *this device*?
    /// Only when a grant names that host exactly, or the policy is `*`.
    ///
    /// This is the launcher check's question. For an app, `*` is
    /// `--share-net`, which reaches the device's loopback like any other
    /// process, and a check in place of it must not be stricter. The DNS
    /// stub of `tilo-egress` answers its own question
    /// (`tilo_egress::policy::admissible`): an environment's `*` keeps a
    /// namespace of its own, whose loopback is not the device's.
    pub fn permits_device_local(&self, host: &str) -> bool {
        matches!(self, Self::Any) || self.names_exactly(host)
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
            (Self::Hosts(mine), Self::Hosts(theirs)) => {
                mine.iter().all(|p| theirs.iter().any(|t| t.includes(p)))
            }
        }
    }

    /// The names both `self` and `other` allow: of each pair of patterns,
    /// the narrower one when one includes the other.
    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        match (self, other) {
            (Self::Any, only) | (only, Self::Any) => only.clone(),
            (Self::Hosts(mine), Self::Hosts(theirs)) => Self::hosts(
                mine.iter()
                    .flat_map(|p| {
                        theirs.iter().filter_map(move |t| {
                            if t.includes(p) {
                                Some(p.clone())
                            } else if p.includes(t) {
                                Some(t.clone())
                            } else {
                                None
                            }
                        })
                    })
                    .collect(),
            ),
        }
    }

    /// The grant spellings, for reports and logs.
    pub fn describe(&self) -> Vec<String> {
        match self {
            Self::Any => vec!["*".to_owned()],
            Self::Hosts(hosts) => hosts.iter().map(ToString::to_string).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test vectors every enforcer is checked against.
    const VECTORS: &str = include_str!("../../tests/net_host_vectors.txt");

    #[test]
    fn net_host_pattern_has_one_grammar() {
        parse_accepts_every_spelling_of_a_host();
        parse_refuses_addresses_and_junk();
        the_table_agrees_with_the_policy();
        a_grant_spelling_covers_everything_it_names();
        loopback_names_are_exact_unless_the_grant_widens();
    }

    fn parse_accepts_every_spelling_of_a_host() {
        let named = |name: &str, subdomains| {
            Ok(NetHostPattern::Named(HostPattern {
                name: name.to_owned(),
                subdomains,
                apex: true,
            }))
        };
        assert_eq!(NetHostPattern::parse("*"), Ok(NetHostPattern::Any));
        assert_eq!(NetHostPattern::parse(" * "), Ok(NetHostPattern::Any));
        assert_eq!(
            NetHostPattern::parse("example.com"),
            named("example.com", false)
        );
        assert_eq!(
            NetHostPattern::parse("Example.COM."),
            named("example.com", false)
        );
        assert_eq!(
            NetHostPattern::parse(".example.com"),
            named("example.com", true)
        );
        assert_eq!(
            NetHostPattern::parse("*.example.com"),
            named("example.com", true),
            "`*.` is a spelling of the domain form, not a glob"
        );
        assert_eq!(
            NetHostPattern::parse("*.Example.com.").map(|p| p.to_string()),
            Ok(".example.com".to_owned())
        );
    }

    fn parse_refuses_addresses_and_junk() {
        for address in ["10.0.0.5", "::1", "[::1]", "1.1.1.1"] {
            assert_eq!(
                NetHostPattern::parse(address),
                Err(NetHostError::Address(address.to_owned())),
                "{address}"
            );
        }
        for junk in [
            "",
            "*.",
            "-x.com",
            "x..com",
            "a b.com",
            "exa$mple.com",
            "**.x.com",
        ] {
            assert!(NetHostPattern::parse(junk).is_err(), "{junk:?} parsed");
        }
    }

    /// The shared table: `(grant, host, covered)`, through `host_covers`
    /// (what `decide_capability` evaluates) and the policy every enforcer
    /// runs.
    fn the_table_agrees_with_the_policy() {
        let rows: Vec<_> = vectors().collect();
        assert!(rows.len() > 20, "the vector file was read: {rows:?}");
        for (grant, host, covered) in rows {
            assert_eq!(
                host_covers(grant, host),
                covered,
                "host_covers({grant:?}, {host:?})"
            );
            assert_eq!(
                EgressPolicy::from_grants([grant]).permits(host),
                covered,
                "EgressPolicy::from_grants([{grant:?}]).permits({host:?})"
            );
        }
    }

    /// `decide_capability` is asked about a grant's own scope (the
    /// launcher's capability plan, ush's policy), so a domain grant must
    /// cover itself in every spelling, and nothing narrower may.
    fn a_grant_spelling_covers_everything_it_names() {
        for (pattern, target, covered) in [
            (".crates.io", ".crates.io", true),
            (".crates.io", "*.crates.io", true),
            ("*.crates.io", ".crates.io", true),
            (".io", ".crates.io", true),
            ("*", ".crates.io", true),
            ("*", "*", true),
            ("crates.io", ".crates.io", false),
            ("static.crates.io", ".crates.io", false),
            (".crates.io", ".io", false),
            (".crates.io", "*", false),
            ("example.com", "*", false),
            ("10.0.0.5", "10.0.0.5", false),
        ] {
            assert_eq!(
                host_covers(pattern, target),
                covered,
                "host_covers({pattern:?}, {target:?})"
            );
        }
    }

    fn loopback_names_are_exact_unless_the_grant_widens() {
        let exact = EgressPolicy::from_grants(["localhost"]);
        assert!(exact.names_exactly("LOCALHOST."));
        assert!(exact.permits_device_local("localhost"));
        let domain = EgressPolicy::from_grants([".localhost"]);
        assert!(domain.permits("localhost"));
        assert!(!domain.names_exactly("localhost"));
        assert!(!domain.permits_device_local("localhost"));
        assert!(EgressPolicy::Any.permits_device_local("localhost"));
        assert!(!EgressPolicy::Any.names_exactly("localhost"));
    }

    /// A package's declaration: `*.domain` is every name under the domain,
    /// never the domain itself. Grants keep the `.domain` meaning.
    #[test]
    fn a_declared_wildcard_covers_names_under_the_domain_and_not_the_domain() {
        let policy = EgressPolicy::from_declared(["*.discord.media"]);
        for host in [
            "rotterdam1234.discord.media",
            "a.b.discord.media",
            "EU.Discord.Media.",
        ] {
            assert!(policy.permits(host), "{host} is under the domain");
        }
        for host in ["discord.media", "discord.media.evil", "xdiscord.media"] {
            assert!(!policy.permits(host), "{host} is not under the domain");
        }
        assert_eq!(policy.describe(), ["*.discord.media"]);
        // The apex needs its own entry.
        let with_apex = EgressPolicy::from_declared(["*.discord.media", "discord.media"]);
        assert!(with_apex.permits("discord.media"));
        assert!(with_apex.permits("rotterdam1234.discord.media"));
        // Exact declarations stay exact.
        let exact = EgressPolicy::from_declared(["discord.com"]);
        assert!(exact.permits("discord.com"));
        assert!(!exact.permits("a.discord.com"));
        // A declaration narrows a wider grant: the manifest's `*.` wins.
        let narrowed = EgressPolicy::from_declared(["*.discord.media"])
            .intersect(&EgressPolicy::from_grants([".discord.media"]));
        assert!(narrowed.permits("rotterdam1234.discord.media"));
        assert!(!narrowed.permits("discord.media"));
        // A wildcard anywhere but the leftmost label is not a host here.
        for junk in ["a.*.discord.media", "discord.*", "*.*.discord.media"] {
            assert_eq!(
                EgressPolicy::from_declared([junk]),
                EgressPolicy::none(),
                "{junk:?} declares nothing"
            );
        }
        // A manifest read refuses a bare `*`; an older one that carries it
        // still means every host, as it did.
        assert_eq!(EgressPolicy::from_declared(["*"]), EgressPolicy::Any);
    }

    /// `(grant, host, covered)` rows of the shared vector file.
    fn vectors() -> impl Iterator<Item = (&'static str, &'static str, bool)> {
        VECTORS
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| {
                let fields: Vec<&str> = line.split_whitespace().collect();
                match fields.as_slice() {
                    [grant, host, "yes"] => (*grant, *host, true),
                    [grant, host, "no"] => (*grant, *host, false),
                    _ => panic!("malformed vector line {line:?}"),
                }
            })
    }
}
