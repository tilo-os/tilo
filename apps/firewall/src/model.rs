//! Pure data model for the firewall manager.
//!
//! The shapes here mirror firewalld's mental model: a set of **zones**,
//! each with a default **target** (the action taken when no rule matches),
//! a list of bound network **interfaces**, a set of enabled named
//! **services**, and a list of **rules** (port openings + rich,
//! source-scoped accept/reject entries).
//!
//! Everything is plain data + small pure helpers — no IO, no rendering,
//! no automerge. The CRDT bridge lives in `doc.rs`; the live
//! `firewall-cmd`/`nft` apply path lives in `backend.rs`. Keeping this
//! layer pure means the whole model round-trips through serde and is
//! unit-testable without a document or a GPU.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a single rule. Backs selection + edit/delete.
pub type RuleId = Uuid;

/// The nine stock firewalld zones. `Public` is the conventional default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZoneKind {
    Drop,
    Block,
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

    /// One-line description shown under the zone title in the summary.
    pub fn description(self) -> &'static str {
        match self {
            ZoneKind::Drop => "Incoming packets are dropped with no reply. Outgoing allowed.",
            ZoneKind::Block => "Incoming rejected with icmp-host-prohibited. Outgoing allowed.",
            ZoneKind::Public => "For untrusted public networks. Only selected services accepted.",
            ZoneKind::External => "For routers with masquerading enabled. Untrusted networks.",
            ZoneKind::Dmz => "For a demilitarized zone — limited internal access, public-facing.",
            ZoneKind::Work => "For mostly-trusted work networks.",
            ZoneKind::Home => "For trusted home networks. Peers mostly allowed.",
            ZoneKind::Internal => "For internal networks. Peers mostly trusted.",
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// firewalld `default` — reject with an ICMP error.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Matcher {
    /// A named service from the catalog (e.g. `https`).
    Service(String),
    /// An explicit port (or port range like `8000-8080`) + protocol.
    Port { port: String, proto: Proto },
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

    /// JSON encode for the CRDT text blob.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Decode from the CRDT text blob; `None` on parse failure.
    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
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
            description: "Secure Shell remote login",
        },
        ServiceDef {
            id: "http",
            name: "HTTP",
            ports: "80",
            proto: "tcp",
            description: "Web server (cleartext)",
        },
        ServiceDef {
            id: "https",
            name: "HTTPS",
            ports: "443",
            proto: "tcp",
            description: "Web server (TLS)",
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
            description: "Multicast DNS / service discovery",
        },
        ServiceDef {
            id: "dhcpv6-client",
            name: "DHCPv6 Client",
            ports: "546",
            proto: "udp",
            description: "IPv6 address autoconfiguration",
        },
        ServiceDef {
            id: "samba",
            name: "Samba",
            ports: "137-139,445",
            proto: "tcp/udp",
            description: "Windows file & printer sharing",
        },
        ServiceDef {
            id: "samba-client",
            name: "Samba Client",
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
            description: "Internet printing protocol",
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
            description: "Network time sync",
        },
        ServiceDef {
            id: "ftp",
            name: "FTP",
            ports: "21",
            proto: "tcp",
            description: "File transfer (legacy)",
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

    /// The list of catalog ids — used to seed the service dropdown in the
    /// add-rule form.
    pub fn ids() -> Vec<String> {
        Self::ALL.iter().map(|s| s.id.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_json_round_trips() {
        let cfg = crate::sample::sample_config();
        let json = cfg.to_json();
        let back = FirewallConfig::from_json(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn toggle_service_is_idempotent_pair() {
        let mut z = Zone::new(ZoneKind::Public, Target::Default);
        assert!(!z.has_service("ssh"));
        assert!(z.toggle_service("ssh"));
        assert!(z.has_service("ssh"));
        assert!(!z.toggle_service("ssh"));
        assert!(!z.has_service("ssh"));
    }

    #[test]
    fn rule_counts_split_allow_deny() {
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

    #[test]
    fn matcher_labels_are_stable() {
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

    #[test]
    fn empty_source_reads_as_anywhere() {
        let r = Rule::new(Matcher::Service("ssh".into()), Action::Allow, "");
        assert_eq!(r.source_label(), "anywhere");
        let r2 = Rule::new(Matcher::Service("ssh".into()), Action::Allow, "10.0.0.0/8");
        assert_eq!(r2.source_label(), "10.0.0.0/8");
    }

    #[test]
    fn all_zones_present_in_order() {
        assert_eq!(ZoneKind::ALL.len(), 9);
        assert_eq!(ZoneKind::ALL[0], ZoneKind::Drop);
        assert_eq!(ZoneKind::ALL[8], ZoneKind::Trusted);
    }
}
