//! Pure data model for the firewall manager.
//!
//! The shapes here mirror firewalld's mental model: a set of **zones**,
//! each with a default **target** (the action taken when no rule matches),
//! a list of bound network **interfaces**, a set of enabled named
//! **services**, and a list of **rules** (port openings + rich,
//! source-scoped accept/reject entries).
//!
//! Everything is plain data + small pure helpers — no IO, no rendering.
//! [`Firewall`] is the stored record (`#[derive(Model)]`): zones keyed by
//! name, each with its membership sets and an ordered collection of rules.
//! [`FirewallConfig`] is the configuration as the window and firewalld read
//! it, made from the record. The live `firewall-cmd`/`nft` apply path lives
//! in `backend.rs`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Collection, Id, Json, Model, Refusal, Set};
use uuid::Uuid;

/// Stable identifier for a single rule. Backs selection + edit/delete.
pub type RuleId = Uuid;

/// The nine stock firewalld zones. `Public` is the conventional default.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZoneKind {
    Drop,
    Block,
    #[default]
    Public,
    External,
    Dmz,
    Work,
    Home,
    Internal,
    Trusted,
}

impl ZoneKind {
    /// Display order in the rail — least-trusted at the top, most-trusted
    /// at the bottom, matching how firewalld documents the spectrum.
    pub const ALL: [ZoneKind; 9] = [
        ZoneKind::Drop,
        ZoneKind::Block,
        ZoneKind::Public,
        ZoneKind::External,
        ZoneKind::Dmz,
        ZoneKind::Work,
        ZoneKind::Home,
        ZoneKind::Internal,
        ZoneKind::Trusted,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ZoneKind::Drop => "drop",
            ZoneKind::Block => "block",
            ZoneKind::Public => "public",
            ZoneKind::External => "external",
            ZoneKind::Dmz => "dmz",
            ZoneKind::Work => "work",
            ZoneKind::Home => "home",
            ZoneKind::Internal => "internal",
            ZoneKind::Trusted => "trusted",
        }
    }

    /// Resolve a firewalld zone name back to a [`ZoneKind`]. Returns `None`
    /// for custom zones the model doesn't represent (only the nine stock
    /// zones are modelled), so an import skips anything non-standard rather
    /// than mislabelling it.
    pub fn from_name(name: &str) -> Option<ZoneKind> {
        ZoneKind::ALL.iter().copied().find(|z| z.name() == name)
    }

    /// The zone as people read it: sentence case, with the acronym kept.
    pub fn title(self) -> &'static str {
        match self {
            ZoneKind::Drop => "Drop",
            ZoneKind::Block => "Block",
            ZoneKind::Public => "Public",
            ZoneKind::External => "External",
            ZoneKind::Dmz => "DMZ",
            ZoneKind::Work => "Work",
            ZoneKind::Home => "Home",
            ZoneKind::Internal => "Internal",
            ZoneKind::Trusted => "Trusted",
        }
    }

    /// One-line description shown under the zone title in the summary.
    pub fn description(self) -> &'static str {
        match self {
            ZoneKind::Drop => {
                "Incoming traffic is dropped without a reply. Outgoing traffic is allowed."
            }
            ZoneKind::Block => "Incoming connections are refused. Outgoing traffic is allowed.",
            ZoneKind::Public => {
                "For untrusted public networks. Only the services you allow are reachable."
            }
            ZoneKind::External => "For the outside of a router that masquerades your network.",
            ZoneKind::Dmz => "For public-facing hosts with limited access to the internal network.",
            ZoneKind::Work => "For work networks where you mostly trust the other computers.",
            ZoneKind::Home => "For home networks where you mostly trust the other computers.",
            ZoneKind::Internal => {
                "For internal networks where you mostly trust the other computers."
            }
            ZoneKind::Trusted => "All network connections are accepted.",
        }
    }

    /// A coarse trust score 0..=8 used to tint the rail dot.
    pub fn trust(self) -> u8 {
        match self {
            ZoneKind::Drop => 0,
            ZoneKind::Block => 1,
            ZoneKind::Public => 2,
            ZoneKind::External => 3,
            ZoneKind::Dmz => 4,
            ZoneKind::Work => 5,
            ZoneKind::Home => 6,
            ZoneKind::Internal => 7,
            ZoneKind::Trusted => 8,
        }
    }
}

/// A zone's default target — what happens to traffic no rule matched.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// firewalld `default` — reject with an ICMP error.
    #[default]
    Default,
    /// Accept everything not otherwise denied (trusted zone).
    Accept,
    /// Reject with an ICMP error.
    Reject,
    /// Silently drop.
    Drop,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Default => "default (reject)",
            Target::Accept => "ACCEPT",
            Target::Reject => "REJECT",
            Target::Drop => "DROP",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Target::Default => "default",
            Target::Accept => "ACCEPT",
            Target::Reject => "REJECT",
            Target::Drop => "DROP",
        }
    }

    /// What happens to traffic no rule matched, as a sentence-case value.
    pub fn outcome(self) -> &'static str {
        match self {
            Target::Default | Target::Reject => "Rejected",
            Target::Accept => "Accepted",
            Target::Drop => "Dropped",
        }
    }

    pub const ALL: [Target; 4] = [
        Target::Default,
        Target::Accept,
        Target::Reject,
        Target::Drop,
    ];
}

/// Transport protocol for a port rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    pub fn label(self) -> &'static str {
        match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        }
    }
    /// The protocol as it is written in prose: an acronym.
    pub fn title(self) -> &'static str {
        match self {
            Proto::Tcp => "TCP",
            Proto::Udp => "UDP",
        }
    }
    pub const ALL: [Proto; 2] = [Proto::Tcp, Proto::Udp];
    #[must_use]
    pub fn next(self) -> Proto {
        match self {
            Proto::Tcp => Proto::Udp,
            Proto::Udp => Proto::Tcp,
        }
    }
}

/// What a rule does with matching traffic.
#[derive(Model, Default, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    #[default]
    Allow,
    Deny,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Allow => "allow",
            Action::Deny => "deny",
        }
    }
    /// The action as a sentence-case word.
    pub fn title(self) -> &'static str {
        match self {
            Action::Allow => "Allow",
            Action::Deny => "Deny",
        }
    }
    pub const ALL: [Action; 2] = [Action::Allow, Action::Deny];
    #[must_use]
    pub fn next(self) -> Action {
        match self {
            Action::Allow => Action::Deny,
            Action::Deny => Action::Allow,
        }
    }
}

/// What a rule matches: a named service, or an explicit port + protocol.
/// Stored whole: a rule's match changes as one value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Matcher {
    /// A named service from the catalog (e.g. `https`).
    Service(String),
    /// An explicit port (or port range like `8000-8080`) + protocol.
    Port { port: String, proto: Proto },
}

impl Default for Matcher {
    fn default() -> Self {
        Self::Service(String::new())
    }
}

impl Matcher {
    /// Short, human label for the first table column.
    pub fn label(&self) -> String {
        match self {
            Matcher::Service(s) => s.clone(),
            Matcher::Port { port, proto } => format!("{}/{}", port, proto.label()),
        }
    }

    pub fn proto_label(&self) -> String {
        match self {
            Matcher::Service(s) => ServiceCatalog::proto_of(s).to_string(),
            Matcher::Port { proto, .. } => proto.label().to_string(),
        }
    }

    /// What the rule matches as people read it: the service's name, or
    /// the port (range); the protocol has a column of its own.
    pub fn title(&self) -> String {
        match self {
            Matcher::Service(s) => ServiceCatalog::name_of(s).to_string(),
            Matcher::Port { port, .. } => format!("Port {port}"),
        }
    }

    /// The protocol column: acronyms, both named when a service uses both.
    pub fn proto_title(&self) -> &'static str {
        match self {
            Matcher::Service(s) => ServiceCatalog::proto_title(ServiceCatalog::proto_of(s)),
            Matcher::Port { proto, .. } => proto.title(),
        }
    }

    pub fn is_service(&self) -> bool {
        matches!(self, Matcher::Service(_))
    }
}

/// A single firewall rule inside a zone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub matcher: Matcher,
    pub action: Action,
    /// Source CIDR / address. Empty == "anywhere" (`0.0.0.0/0`).
    pub source: String,
    /// Disabled rules stay in the list but are not applied.
    pub enabled: bool,
}

impl Rule {
    #[must_use]
    pub fn new(matcher: Matcher, action: Action, source: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            matcher,
            action,
            source: source.into(),
            enabled: true,
        }
    }

    /// The displayed source — "anywhere" for an empty match.
    pub fn source_label(&self) -> &str {
        if self.source.trim().is_empty() {
            "anywhere"
        } else {
            self.source.as_str()
        }
    }
}

/// A configured zone.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Zone {
    pub kind: ZoneKind,
    pub target: Target,
    pub interfaces: Vec<String>,
    /// Enabled named services (the services panel toggles membership).
    pub services: Vec<String>,
    pub rules: Vec<Rule>,
    /// firewalld "masquerade" (source NAT) flag — meaningful on external.
    pub masquerade: bool,
}

impl Zone {
    #[must_use]
    pub fn new(kind: ZoneKind, target: Target) -> Self {
        Self {
            kind,
            target,
            interfaces: Vec::new(),
            services: Vec::new(),
            rules: Vec::new(),
            masquerade: false,
        }
    }

    pub fn has_service(&self, id: &str) -> bool {
        self.services.iter().any(|s| s == id)
    }

    /// Toggle a named service on/off. Returns the new state.
    pub fn toggle_service(&mut self, id: &str) -> bool {
        if let Some(idx) = self.services.iter().position(|s| s == id) {
            self.services.remove(idx);
            false
        } else {
            self.services.push(id.to_string());
            true
        }
    }

    /// Count of rules that allow vs deny — used by the posture summary.
    pub fn rule_counts(&self) -> (usize, usize) {
        let allow = self
            .rules
            .iter()
            .filter(|r| r.action == Action::Allow)
            .count();
        (allow, self.rules.len() - allow)
    }
}

/// Top-level firewall configuration — the document root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallConfig {
    /// Master switch — whether the firewall is running at all.
    pub enabled: bool,
    /// firewalld panic mode: drop ALL traffic in + out.
    pub panic_mode: bool,
    /// The active (default) zone — new interfaces land here.
    pub default_zone: ZoneKind,
    pub zones: Vec<Zone>,
}

impl FirewallConfig {
    /// An empty configuration — no zones, firewall reported as stopped.
    /// Used as the honest fallback when firewalld cannot be read, so the
    /// live app never displays a fabricated ruleset.
    pub fn empty() -> Self {
        FirewallConfig {
            enabled: false,
            panic_mode: false,
            default_zone: ZoneKind::Public,
            zones: Vec::new(),
        }
    }

    pub fn zone(&self, kind: ZoneKind) -> Option<&Zone> {
        self.zones.iter().find(|z| z.kind == kind)
    }

    pub fn zone_mut(&mut self, kind: ZoneKind) -> Option<&mut Zone> {
        self.zones.iter_mut().find(|z| z.kind == kind)
    }

    /// This configuration as the document reads it back once written whole:
    /// zones in their stock order, membership sets sorted without repeats,
    /// rules in their order. The form two devices compare a configuration in.
    #[must_use]
    pub fn canonical(&self) -> Self {
        let mut zones = Vec::new();
        for kind in ZoneKind::ALL {
            if let Some(zone) = self.zone(kind) {
                let mut zone = zone.clone();
                zone.interfaces.sort();
                zone.interfaces.dedup();
                zone.services.sort();
                zone.services.dedup();
                zones.push(zone);
            }
        }
        Self {
            zones,
            ..self.clone()
        }
    }

    /// JSON encode for the CRDT text blob.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Verify that this is a configuration the app can safely represent and
    /// hand to firewalld. This is deliberately independent of the UI: remote
    /// collaborators and action replay are untrusted input too.
    ///
    /// # Errors
    ///
    /// Returns the first malformed zone, service, port, or source constraint.
    pub fn validate(&self) -> Result<(), String> {
        if self.zones.is_empty() {
            return Err("configuration has no zones".into());
        }
        if self.zone(self.default_zone).is_none() {
            return Err("default zone is missing from configuration".into());
        }
        for zone in &self.zones {
            if self
                .zones
                .iter()
                .filter(|other| other.kind == zone.kind)
                .count()
                != 1
            {
                return Err(format!("zone {} appears more than once", zone.kind.name()));
            }
            for service in &zone.services {
                validate_service(service)?;
            }
            for rule in &zone.rules {
                validate_rule(rule)?;
            }
        }
        Ok(())
    }
}

/// One rule as the document stores it; its item id is the rule's uuid.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct StoredRule {
    /// What it matches
    pub matcher: Json<Matcher>,
    /// What it does
    pub action: Action,
    /// Source address
    pub source: String,
    /// Enabled
    pub enabled: bool,
}

/// One zone as the document stores it, keyed by its zone name.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct StoredZone {
    /// Default policy
    pub target: Target,
    /// Interfaces
    pub interfaces: Set<String>,
    /// Allowed services
    pub services: Set<String>,
    /// Rules, in the order they apply.
    pub rules: Collection<StoredRule>,
    /// Masquerade
    pub masquerade: bool,
    /// Rules deleted from this zone: a deletion wins over a concurrent edit
    /// and an import never brings the rule back.
    #[model(readonly)]
    pub deleted_rules: Set<String>,
}

/// The stored firewall configuration: what every device running the app
/// projects onto its own firewalld.
#[derive(Model, Default, Clone, Debug, PartialEq)]
#[model(check = Firewall::valid)]
pub struct Firewall {
    /// Running: started and stopped by verbs.
    #[model(readonly)]
    pub enabled: bool,
    /// Panic mode: entered and left by verbs.
    #[model(readonly)]
    pub panic_mode: bool,
    /// Active zone
    pub default_zone: ZoneKind,
    /// The configured zones, by zone name.
    pub zones: BTreeMap<String, StoredZone>,
}

impl Firewall {
    /// A configured firewall is one firewalld can be handed safely.
    fn valid(&self) -> Result<(), Refusal> {
        if self.zones.is_empty() {
            return Ok(());
        }
        self.config().validate().map_err(Refusal::new)
    }

    /// The item id of rule `id`.
    pub fn rule(id: RuleId) -> Id {
        Id::from_raw(id.to_string())
    }

    /// The configuration the record holds: stock zones in order, each
    /// membership sorted, rules in order without the deleted ones.
    pub fn config(&self) -> FirewallConfig {
        let zones = ZoneKind::ALL
            .into_iter()
            .filter_map(|kind| {
                let stored = self.zones.get(kind.name())?;
                let rules = stored
                    .rules
                    .iter()
                    .filter(|(id, _)| !stored.deleted_rules.contains(&id.as_str().to_owned()))
                    .filter_map(|(id, rule)| {
                        Some(Rule {
                            id: id.as_str().parse().ok()?,
                            matcher: rule.matcher.0.clone(),
                            action: rule.action,
                            source: rule.source.clone(),
                            enabled: rule.enabled,
                        })
                    })
                    .collect();
                Some(Zone {
                    kind,
                    target: stored.target,
                    interfaces: stored.interfaces.iter().cloned().collect(),
                    services: stored.services.iter().cloned().collect(),
                    rules,
                    masquerade: stored.masquerade,
                })
            })
            .collect();
        FirewallConfig {
            enabled: self.enabled,
            panic_mode: self.panic_mode,
            default_zone: self.default_zone,
            zones,
        }
    }

    /// Take in what firewalld reported: members it lacks turn off, rules it
    /// lacks are deleted, a deleted rule stays deleted, and the rest is
    /// written where it differs.
    pub fn take_in(&mut self, config: &FirewallConfig) {
        self.enabled = config.enabled;
        self.panic_mode = config.panic_mode;
        self.default_zone = config.default_zone;
        for incoming in &config.zones {
            let zone = self
                .zones
                .entry(incoming.kind.name().to_owned())
                .or_default();
            zone.target = incoming.target;
            zone.masquerade = incoming.masquerade;
            zone.interfaces = incoming.interfaces.iter().cloned().collect();
            zone.services = incoming.services.iter().cloned().collect();
            let wanted: Vec<Id> = incoming
                .rules
                .iter()
                .map(|rule| Self::rule(rule.id))
                .collect();
            let gone: Vec<Id> = zone
                .rules
                .ids()
                .filter(|id| !wanted.contains(id))
                .cloned()
                .collect();
            for id in gone {
                zone.rules.remove(&id);
                zone.deleted_rules.insert(id.as_str().to_owned());
            }
            for rule in &incoming.rules {
                let id = Self::rule(rule.id);
                if zone.deleted_rules.contains(&id.as_str().to_owned()) {
                    continue;
                }
                let stored = StoredRule {
                    matcher: Json(rule.matcher.clone()),
                    action: rule.action,
                    source: rule.source.clone(),
                    enabled: rule.enabled,
                };
                match zone.rules.get_mut(&id) {
                    Some(existing) => {
                        if *existing != stored {
                            *existing = stored;
                        }
                    }
                    None => {
                        zone.rules.push(id, stored);
                    }
                }
            }
        }
    }
}

/// Validate a user-authored port or inclusive port range. Firewalld itself is
/// still authoritative, but rejecting malformed values here keeps invalid
/// shared state and misleading dry-run plans out of the system.
///
/// # Errors
///
/// Returns a human-readable reason when `port` is empty, out of range, or has
/// a descending/malformed range.
pub fn validate_port(port: &str) -> Result<(), String> {
    let value = port.trim();
    if value.is_empty() {
        return Err("Enter a port or a port range.".into());
    }
    let (first, last) = match value.split_once('-') {
        Some(range) => range,
        None => (value, value),
    };
    let parse = |part: &str| -> Result<u16, String> {
        part.parse::<u16>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| "Use a port from 1 to 65535.".into())
    };
    let first = parse(first)?;
    let last = parse(last)?;
    if first > last {
        return Err("Start a port range with the lower port.".into());
    }
    Ok(())
}

/// Validate an optional IPv4 source address or CIDR. The backend emits IPv4
/// rich rules, so accepting IPv6 here would silently create a rule with the
/// wrong family.
///
/// # Errors
///
/// Returns a human-readable reason when `source` is not an IPv4 address or a
/// CIDR with a `/0` through `/32` prefix.
pub fn validate_source(source: &str) -> Result<(), String> {
    let value = source.trim();
    if value.is_empty() {
        return Ok(());
    }
    let (address, prefix) = value
        .split_once('/')
        .map_or((value, None), |(ip, p)| (ip, Some(p)));
    if address.parse::<std::net::Ipv4Addr>().is_err() {
        return Err("Enter an IPv4 address or network, such as 192.168.1.0/24.".into());
    }
    if let Some(prefix) = prefix {
        prefix
            .parse::<u8>()
            .ok()
            .filter(|prefix| *prefix <= 32)
            .ok_or_else(|| "Use a network prefix from /0 to /32.".to_string())?;
    }
    Ok(())
}

fn validate_service(service: &str) -> Result<(), String> {
    if service.is_empty()
        || service.len() > 64
        || !service
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("service names may contain only letters, numbers, '.', '_' or '-'".into());
    }
    Ok(())
}

fn validate_rule(rule: &Rule) -> Result<(), String> {
    match &rule.matcher {
        Matcher::Service(service) => validate_service(service)?,
        Matcher::Port { port, .. } => validate_port(port)?,
    }
    validate_source(&rule.source)
}

/// A catalog entry describing a well-known service: stable id, display
/// name, the ports it opens, and a one-line description. The services
/// panel renders a toggle per entry.
#[derive(Clone, Copy, Debug)]
pub struct ServiceDef {
    pub id: &'static str,
    pub name: &'static str,
    pub ports: &'static str,
    pub proto: &'static str,
    pub description: &'static str,
}

/// Static catalog of common services, in panel display order.
pub struct ServiceCatalog;

impl ServiceCatalog {
    pub const ALL: &'static [ServiceDef] = &[
        ServiceDef {
            id: "ssh",
            name: "SSH",
            ports: "22",
            proto: "tcp",
            description: "Remote login over Secure Shell",
        },
        ServiceDef {
            id: "http",
            name: "HTTP",
            ports: "80",
            proto: "tcp",
            description: "Unencrypted web server",
        },
        ServiceDef {
            id: "https",
            name: "HTTPS",
            ports: "443",
            proto: "tcp",
            description: "Encrypted web server",
        },
        ServiceDef {
            id: "dns",
            name: "DNS",
            ports: "53",
            proto: "tcp/udp",
            description: "Domain name resolver",
        },
        ServiceDef {
            id: "mdns",
            name: "mDNS",
            ports: "5353",
            proto: "udp",
            description: "Service discovery on the local network",
        },
        ServiceDef {
            id: "dhcpv6-client",
            name: "DHCPv6 client",
            ports: "546",
            proto: "udp",
            description: "IPv6 address autoconfiguration",
        },
        ServiceDef {
            id: "samba",
            name: "Samba",
            ports: "137-139,445",
            proto: "tcp/udp",
            description: "Windows file and printer sharing",
        },
        ServiceDef {
            id: "samba-client",
            name: "Samba client",
            ports: "137-138",
            proto: "udp",
            description: "Browse Windows shares",
        },
        ServiceDef {
            id: "cockpit",
            name: "Cockpit",
            ports: "9090",
            proto: "tcp",
            description: "Web management console",
        },
        ServiceDef {
            id: "ipp",
            name: "IPP",
            ports: "631",
            proto: "tcp",
            description: "Network printing",
        },
        ServiceDef {
            id: "nfs",
            name: "NFS",
            ports: "2049",
            proto: "tcp",
            description: "Network file system",
        },
        ServiceDef {
            id: "wireguard",
            name: "WireGuard",
            ports: "51820",
            proto: "udp",
            description: "Modern VPN tunnel",
        },
        ServiceDef {
            id: "ntp",
            name: "NTP",
            ports: "123",
            proto: "udp",
            description: "Network time",
        },
        ServiceDef {
            id: "ftp",
            name: "FTP",
            ports: "21",
            proto: "tcp",
            description: "Unencrypted file transfer",
        },
    ];

    pub fn get(id: &str) -> Option<&'static ServiceDef> {
        Self::ALL.iter().find(|s| s.id == id)
    }

    pub fn name_of(id: &str) -> &str {
        Self::get(id).map_or(id, |s| s.name)
    }

    pub fn proto_of(id: &str) -> &'static str {
        Self::get(id).map_or("tcp", |s| s.proto)
    }

    /// A catalog protocol (`tcp`, `udp`, `tcp/udp`) as it reads in prose.
    pub fn proto_title(proto: &str) -> &'static str {
        match proto {
            "tcp/udp" => "TCP and UDP",
            "udp" => "UDP",
            _ => "TCP",
        }
    }

    /// The list of catalog ids — used to seed the service dropdown in the
    /// add-rule form.
    pub fn ids() -> Vec<String> {
        Self::ALL.iter().map(|s| s.id.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config toggles services idempotently, splits rule counts and lists every zone in order.
    #[test]
    fn config_model_toggles_and_counts() {
        // --- phase: toggle_service_is_idempotent_pair ---
        {
            let mut z = Zone::new(ZoneKind::Public, Target::Default);
            assert!(!z.has_service("ssh"));
            assert!(z.toggle_service("ssh"));
            assert!(z.has_service("ssh"));
            assert!(!z.toggle_service("ssh"));
            assert!(!z.has_service("ssh"));
        }

        // --- phase: rule_counts_split_allow_deny ---
        {
            let mut z = Zone::new(ZoneKind::Public, Target::Default);
            z.rules.push(Rule::new(
                Matcher::Service("https".into()),
                Action::Allow,
                "",
            ));
            z.rules.push(Rule::new(
                Matcher::Port {
                    port: "23".into(),
                    proto: Proto::Tcp,
                },
                Action::Deny,
                "",
            ));
            assert_eq!(z.rule_counts(), (1, 1));
        }

        // --- phase: all_zones_present_in_order ---
        {
            assert_eq!(ZoneKind::ALL.len(), 9);
            assert_eq!(ZoneKind::ALL[0], ZoneKind::Drop);
            assert_eq!(ZoneKind::ALL[8], ZoneKind::Trusted);
        }
    }

    /// Matcher labels are stable, an empty source reads as anywhere, and unsafe input is rejected.
    #[test]
    fn rule_matchers_label_stably_and_inputs_validate() {
        // --- phase: matcher_labels_are_stable ---
        {
            assert_eq!(Matcher::Service("https".into()).label(), "https");
            assert_eq!(
                Matcher::Port {
                    port: "8080".into(),
                    proto: Proto::Tcp
                }
                .label(),
                "8080/tcp"
            );
        }

        // --- phase: empty_source_reads_as_anywhere ---
        {
            let r = Rule::new(Matcher::Service("ssh".into()), Action::Allow, "");
            assert_eq!(r.source_label(), "anywhere");
            let r2 = Rule::new(Matcher::Service("ssh".into()), Action::Allow, "10.0.0.0/8");
            assert_eq!(r2.source_label(), "10.0.0.0/8");
        }

        // --- phase: firewall_input_validation_rejects_unsafe_values ---
        {
            assert!(validate_port("1").is_ok());
            assert!(validate_port("8000-8080").is_ok());
            assert!(validate_port("0").is_err());
            assert!(validate_port("8080-8000").is_err());
            assert!(validate_port("22; reboot").is_err());
            assert!(validate_source("192.168.1.0/24").is_ok());
            assert!(validate_source("2001:db8::1").is_err());
            assert!(validate_source("10.0.0.1/33").is_err());
        }
    }
}
