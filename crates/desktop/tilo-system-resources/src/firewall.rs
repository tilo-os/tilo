//! The `firewall` resource: this computer's firewalld, the zones and rules it holds, the
//! traffic it dropped in the last hour, and the actions that read it and hand it a new
//! configuration.
//!
//! The window over a workspace's shared configuration applies it here; this reading is what
//! that device's firewalld held when it was last read, so a device that cannot reach its
//! firewall says so rather than showing an empty ruleset.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const FIREWALL: &str = "firewall";

/// One rule of a zone, as firewalld and the app both name it: a service or a port, what it
/// does with matching traffic, and where that traffic comes from.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct FirewallRule {
    /// The service it matches (`https`), or empty for a port rule.
    #[serde(default)]
    pub service: String,
    /// The port or port range it matches (`8080`, `8000-8100`), or empty for a service rule.
    #[serde(default)]
    pub port: String,
    /// `tcp` or `udp`.
    #[serde(default)]
    pub proto: String,
    /// `allow` or `deny`.
    #[serde(default)]
    pub verdict: String,
    /// The source address or network; empty for anywhere.
    #[serde(default)]
    pub source: String,
    /// Whether firewalld applies it.
    #[serde(default)]
    pub enabled: bool,
}

/// One zone of firewalld.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct FirewallZone {
    /// The zone's name (`drop`, `block`, `public`, `external`, `dmz`, `work`, `home`,
    /// `internal`, `trusted`).
    #[serde(default)]
    pub kind: String,
    /// What happens to traffic no rule matched: `default`, `accept`, `reject` or `drop`.
    #[serde(default)]
    pub target: String,
    /// The interfaces it claims.
    #[serde(default)]
    pub interfaces: Vec<String>,
    /// The services it lets through.
    #[serde(default)]
    pub services: Vec<String>,
    /// Its rules, in the order firewalld applies them.
    #[serde(default)]
    pub rules: Vec<FirewallRule>,
    /// Whether it masquerades outbound traffic.
    #[serde(default)]
    pub masquerade: bool,
}

/// One group of dropped packets: the rule that dropped them, how many, and how they spread
/// over the window.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct BlockedGroup {
    /// `DROP` or `REJECT`.
    #[serde(default)]
    pub verdict: String,
    /// The transport protocol, lower case.
    #[serde(default)]
    pub protocol: String,
    /// The destination port, when the packet named one.
    #[serde(default)]
    pub port: Option<String>,
    /// The interface the packet arrived on, when it named one.
    #[serde(default)]
    pub interface: Option<String>,
    /// How many packets the rule dropped.
    #[serde(default)]
    pub packets: u64,
    /// How many distinct source addresses hit the rule.
    #[serde(default)]
    pub sources: u64,
    /// When the last packet was seen, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub last_seen_ms: u64,
    /// The counts over the window, oldest first.
    #[serde(default)]
    pub buckets: Vec<u32>,
    /// How long the window covers, in minutes.
    #[serde(default)]
    pub window_minutes: u32,
}

/// What this computer's firewall reads as now.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, tilo_resource::JsonSchema,
)]
pub struct Firewall {
    /// Whether firewalld answers on this machine.
    #[serde(default)]
    pub serving: bool,
    /// Why the configuration could not be read, when it could not.
    #[serde(default)]
    pub read_error: Option<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
    /// Whether firewalld runs.
    #[serde(default)]
    pub enabled: bool,
    /// Whether panic mode drops all traffic in and out.
    #[serde(default)]
    pub panic_mode: bool,
    /// The active zone: `public` when firewalld named none.
    #[serde(default)]
    pub default_zone: String,
    /// The zones firewalld holds.
    #[serde(default)]
    pub zones: Vec<FirewallZone>,
    /// What the kernel dropped in the last window, busiest rule first.
    #[serde(default)]
    pub blocked: Vec<BlockedGroup>,
    /// The packets every group dropped together.
    #[serde(default)]
    pub blocked_total: u64,
    /// The window the blocked report covers, in minutes.
    #[serde(default)]
    pub blocked_window_minutes: u32,
    /// When the blocked report was read, in milliseconds since the Unix epoch.
    #[serde(default)]
    pub blocked_read_at_ms: u64,
    /// Why the blocked report could not be read, when it could not.
    #[serde(default)]
    pub blocked_error: Option<String>,
    /// What the blocked report does not account for, when it leaves something out.
    #[serde(default)]
    pub blocked_note: Option<String>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
}

/// The `firewall` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.firewall")]
pub enum FirewallAction {
    /// Read this computer's firewalld again.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "rotate-cw",
        keywords = "refresh firewall, firewall neu lesen"
    )]
    Refresh,
    /// Apply a configuration to this computer's firewalld, as canonical JSON.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "shield",
        keywords = "apply firewall, firewall anwenden"
    )]
    Apply {
        #[param("The configuration, as canonical JSON")]
        config: String,
    },
    /// Read what the kernel dropped over the last `window_minutes`.
    #[effect(kind = "read")]
    #[effect(
        own_device,
        icon = "shield-off",
        keywords = "dropped traffic, verworfene verbindungen"
    )]
    ReadBlocked {
        #[param("How many minutes back to read")]
        window_minutes: u32,
    },
}
