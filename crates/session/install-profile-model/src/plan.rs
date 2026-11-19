//! An install plan: what a machine at another place should become, as the
//! person's devices keep it and as the installer reads it back.
//!
//! Every choice is made now or left open. The plan is secret-free: it names
//! the single-use enrollment by its nonce and expiry, never the token the
//! medium carries, so the same document reaches the machine as a USB medium,
//! a command in a rescue system, or an installation alongside the system it
//! already has.
//!
//! The machine's real hardware decides the open choices: a disk rule that
//! matches no drive or several is never guessed ([`choose`]). The answer comes
//! from one of the person's devices.

use serde::{Deserialize, Serialize};

/// The OS an installation becomes.
pub const DESTINATIONS: [&str; 2] = ["tilo-os", "tilo-server"];
/// The destination that installs the Tilo Server image: the only one a plan
/// installs with nobody at its screen.
pub const SERVER: &str = "tilo-server";

/// How a plan names the disk to install to. A rule, not a device: the
/// destination's real drives are only known when the installer starts.
pub const DISK_RULES: [&str; 5] = ["empty", "serial", "wwid", "largest", "only"];

/// The rules that need a drive identity; the others name no particular drive.
pub const IDENTIFIED_RULES: [&str; 2] = ["serial", "wwid"];

/// How an interface is addressed.
pub const ADDRESSING: [&str; 2] = ["dhcp", "static"];

/// How a machine unlocks after a boot.
pub const UNLOCK: [&str; 3] = ["owner-device", "tpm", "passphrase"];

/// The updates a machine follows.
pub const UPDATES: [&str; 2] = ["stable", "dev"];

/// How a server holds one workspace: encrypted copies only, or the keys and
/// the work.
pub const HOLDING_MODES: [&str; 2] = ["store", "key-holder"];

/// The choices a plan may leave open, by the name it lists them under.
pub const OPEN_CHOICES: [&str; 7] = [
    "destination",
    "name",
    "network",
    "disk",
    "unlock",
    "updates",
    "hosting",
];

/// The plan's schema version.
pub const SCHEMA: u32 = 1;

/// What a machine at another place should become.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, try_from = "RawPlan")]
pub struct Plan {
    /// [`SCHEMA`].
    pub schema: u32,
    /// The plan's name wherever the person's devices and the medium keep it.
    pub plan: String,
    /// How many times the plan changed after a medium was made from it.
    pub revision: u64,
    /// The OS it installs, one of [`DESTINATIONS`]; empty when open.
    pub destination: String,
    /// The machine's name; empty when open.
    pub name: String,
    /// How its network interfaces are set up.
    pub network: Network,
    /// Which drive it installs to, by rule.
    pub disk: DiskRule,
    /// How it unlocks after a boot, one of [`UNLOCK`]; empty when open.
    pub unlock: String,
    /// The updates it follows, one of [`UPDATES`]; empty when open.
    pub updates: String,
    /// The workspaces it holds, for a server.
    pub hosting: Vec<Holding>,
    /// The choices this plan leaves open, by [`OPEN_CHOICES`]. The machine
    /// asks one of the person's devices for the rest.
    pub open: Vec<String>,
    /// The single-use enrollment it goes with, by nonce and expiry only.
    pub token: Token,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlan {
    schema: u32,
    plan: String,
    revision: u64,
    #[serde(default)]
    destination: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    network: Network,
    #[serde(default)]
    disk: DiskRule,
    #[serde(default)]
    unlock: String,
    #[serde(default)]
    updates: String,
    #[serde(default)]
    hosting: Vec<Holding>,
    #[serde(default)]
    open: Vec<String>,
    #[serde(default)]
    token: Token,
}

impl TryFrom<RawPlan> for Plan {
    type Error = &'static str;

    fn try_from(raw: RawPlan) -> Result<Self, Self::Error> {
        let value = Self {
            schema: raw.schema,
            plan: raw.plan,
            revision: raw.revision,
            destination: raw.destination,
            name: raw.name,
            network: raw.network,
            disk: raw.disk,
            unlock: raw.unlock,
            updates: raw.updates,
            hosting: raw.hosting,
            open: raw.open,
            token: raw.token,
        };
        value.valid().then_some(value).ok_or("invalid install plan")
    }
}

/// The single-use enrollment a plan goes with: enough to find the plan, never
/// enough to redeem it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Token {
    /// The minted token's 16-byte nonce, lowercase hex: unique per token and
    /// what the machine is recognised by until it enrolls.
    pub nonce: String,
    /// Nanoseconds since the epoch after which the token is invalid.
    pub expires_at_ns: u64,
}

/// How a machine's network interfaces are set up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// [`ADDRESSING`]; empty when the plan leaves the network open.
    pub addressing: String,
    /// `address/prefix` entries, comma separated, for static addressing.
    pub addresses: String,
    /// The default gateways, comma separated: one for each address family the
    /// addresses have, so a dual-stack machine routes both families. Empty for
    /// none.
    pub gateway: String,
    /// DNS servers, comma separated; empty for none.
    pub dns: String,
}

/// The rule a plan picks its disk by.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskRule {
    /// One of [`DISK_RULES`]; empty when open.
    pub kind: String,
    /// The serial or WWN [`IDENTIFIED_RULES`] name; empty otherwise.
    pub identity: String,
}

impl DiskRule {
    /// The rule as one line (`empty`, `serial:<serial>`), as a document's field
    /// keeps it.
    #[must_use]
    pub fn encode(&self) -> String {
        if self.identity.is_empty() {
            self.kind.clone()
        } else {
            format!("{}:{}", self.kind, self.identity)
        }
    }

    /// The rule [`Self::encode`] wrote, or one that matches nothing.
    ///
    /// # Errors
    ///
    /// `text` is not a rule [`DISK_RULES`] names, or an identified rule
    /// carries no identity.
    pub fn decode(text: &str) -> Result<Self, &'static str> {
        let (kind, identity) = text.split_once(':').unwrap_or((text, ""));
        let rule = Self {
            kind: kind.to_owned(),
            identity: identity.to_owned(),
        };
        if !DISK_RULES.contains(&kind) {
            return Err("not a disk rule");
        }
        if IDENTIFIED_RULES.contains(&kind) == identity.is_empty() {
            return Err("a serial or WWN rule names its drive, and no other rule does");
        }
        Ok(rule)
    }
}

/// One workspace a server holds, and how.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Holding {
    /// The workspace's id.
    pub workspace: String,
    /// [`HOLDING_MODES`].
    pub mode: String,
}

/// A drive the destination really has.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Drive {
    /// The drive's model, for the person to recognise it by.
    pub model: String,
    /// Its serial or WWN, whichever identifies it across reboots.
    pub identity: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Whether it already holds files.
    pub holds_data: bool,
    /// Whether it holds the system the machine booted from.
    pub boots: bool,
}

/// A planned machine as its person's devices and the installer read it: the
/// plan document itself, the drives its machine reported since it enrolled,
/// and the questions it asked that no device settled yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaitingPlan {
    /// The same document the machine installs from.
    pub plan: Plan,
    /// The drives the machine reported, by their identity.
    pub drives: Vec<Drive>,
    /// The choices it asked about, each with the reason it could not settle
    /// it.
    pub asks: Vec<Question>,
}

/// A choice a machine asked about, and why it could not settle it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// The choice, one of [`OPEN_CHOICES`].
    pub choice: String,
    /// Why the machine could not settle it, for the person to read.
    pub reason: String,
}

/// What a rule picked on this machine's real hardware.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice<'a> {
    /// The one drive the rule names.
    Drive(&'a Drive),
    /// The rule matched no drive or several, so the choice opens on one of
    /// the person's devices, with the reason.
    Open {
        /// What the person is asked to choose.
        choice: &'static str,
        /// Why the rule did not settle it, for the person to read.
        reason: String,
    },
}

impl Plan {
    /// Whether every choice is made, so the machine installs unattended.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.open.is_empty()
    }

    /// The choices the plan's fields leave open, in [`OPEN_CHOICES`] order.
    /// A plan's `open` list is this list, so the document and the machine
    /// cannot disagree about what is still asked. Holding no workspace is a
    /// server's complete answer, so `hosting` is open only when the plan lists
    /// it.
    #[must_use]
    pub fn open_choices(&self) -> Vec<&'static str> {
        let mut open = Vec::new();
        let listed = |name: &str| self.open.iter().any(|open| open == name);
        if self.destination.is_empty() {
            open.push("destination");
        }
        if self.name.is_empty() {
            open.push("name");
        }
        if self.network.addressing.is_empty() {
            open.push("network");
        }
        if self.disk.kind.is_empty() {
            open.push("disk");
        }
        if self.unlock.is_empty() {
            open.push("unlock");
        }
        if self.updates.is_empty() {
            open.push("updates");
        }
        if listed("hosting") {
            open.push("hosting");
        }
        open
    }

    #[must_use]
    fn valid(&self) -> bool {
        let named = |value: &str, allowed: &[&str]| value.is_empty() || allowed.contains(&value);
        let mut declared = self.open.clone();
        declared.sort_unstable();
        declared.dedup();
        let mut computed: Vec<&str> = self.open_choices();
        computed.sort_unstable();
        self.schema == SCHEMA
            && is_hex32(&self.plan)
            && self.token.valid()
            && named(&self.destination, &DESTINATIONS)
            && self.name.len() <= 64
            && !self.name.chars().any(char::is_control)
            && self.name.trim().len() == self.name.len()
            && self.network.valid()
            && self.disk.valid()
            && named(&self.unlock, &UNLOCK)
            && named(&self.updates, &UPDATES)
            && self.hosting.iter().all(|holding| {
                valid_id(&holding.workspace) && HOLDING_MODES.contains(&holding.mode.as_str())
            })
            && declared
                .iter()
                .all(|name| OPEN_CHOICES.contains(&name.as_str()))
            && declared.len() == self.open.len()
            && declared.iter().map(String::as_str).eq(computed)
    }
}

impl Network {
    fn valid(&self) -> bool {
        if self.addressing.is_empty() {
            return self.addresses.is_empty() && self.gateway.is_empty() && self.dns.is_empty();
        }
        if !ADDRESSING.contains(&self.addressing.as_str()) {
            return false;
        }
        let clean = |text: &str| {
            !text.chars().any(char::is_control) && !text.chars().any(char::is_whitespace)
        };
        clean(&self.addresses)
            && gateways(&self.gateway)
            && clean(&self.dns)
            && (self.addressing == "dhcp" || !self.addresses.is_empty())
    }
}

/// Whether `text` names default gateways a machine can use: an IP address
/// each, at most one for each family, so a dual-stack machine names an IPv4
/// and an IPv6 gateway at once.
fn gateways(text: &str) -> bool {
    let mut families: Vec<bool> = Vec::new();
    for gateway in text.split(',').map(str::trim).filter(|one| !one.is_empty()) {
        let Ok(address) = gateway.parse::<std::net::IpAddr>() else {
            return false;
        };
        if families.contains(&address.is_ipv4()) {
            return false;
        }
        families.push(address.is_ipv4());
    }
    true
}

impl DiskRule {
    fn valid(&self) -> bool {
        self.kind.is_empty() || {
            DISK_RULES.contains(&self.kind.as_str())
                && IDENTIFIED_RULES.contains(&self.kind.as_str()) != self.identity.is_empty()
                && self
                    .identity
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._:".contains(&byte))
        }
    }
}

fn is_hex32(text: &str) -> bool {
    text.len() == 32
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// A workspace or server id: letters, digits and hyphens, never empty.
fn valid_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

impl Token {
    fn valid(&self) -> bool {
        is_hex32(&self.nonce) && self.expires_at_ns > 0
    }
}

/// The drive `rule` picks among the destination's real drives, or why the
/// choice opens instead.
#[must_use]
pub fn choose<'a>(rule: &DiskRule, drives: &'a [Drive]) -> Choice<'a> {
    let (candidates, described): (Vec<&Drive>, String) = match rule.kind.as_str() {
        "empty" => (
            drives.iter().filter(|drive| !drive.holds_data).collect(),
            "a drive without data".to_owned(),
        ),
        "serial" | "wwid" => (
            drives
                .iter()
                .filter(|drive| drive.identity == rule.identity)
                .collect(),
            format!("a drive identified {}", rule.identity),
        ),
        "largest" => {
            let usable: Vec<&Drive> = drives.iter().filter(|drive| !drive.boots).collect();
            let largest = usable.iter().map(|drive| drive.bytes).max();
            (
                usable
                    .into_iter()
                    .filter(|drive| Some(drive.bytes) == largest)
                    .collect(),
                "the largest drive that is not the boot drive".to_owned(),
            )
        }
        "only" => (
            drives.iter().filter(|drive| !drive.boots).collect(),
            "the only drive that is not the boot drive".to_owned(),
        ),
        _ => return open(rule, 0, "the plan names no disk rule"),
    };
    match candidates.as_slice() {
        [drive] => Choice::Drive(drive),
        many => open(rule, many.len(), &described),
    }
}

fn open(rule: &DiskRule, matched: usize, described: &str) -> Choice<'static> {
    Choice::Open {
        choice: "disk",
        reason: format!(
            "{rule} matched {matched} drives on this machine, not {described}",
            rule = rule.encode(),
        ),
    }
}

/// The artifact name of the installer for `destination`.
#[must_use]
pub fn installer_image(destination: &str) -> &'static str {
    match destination {
        "tilo-server" => "tilo-server-installer",
        _ => "tilo-installer",
    }
}

/// What a person does once the plan exists: the medium to make, and what a
/// rescue system or a Linux system already on the machine is given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commands {
    /// The installer image to write to a removable drive.
    pub medium: String,
    /// What a rescue or netboot system is given.
    pub netboot: String,
    /// What a Linux system already on the machine is given to install
    /// alongside it.
    pub alongside: String,
}

/// What carries `plan` to its machine. `installer_url` is where the release's
/// installer is served; `token_file` is the file the minted token was written
/// to, never the token itself.
#[must_use]
pub fn commands(
    plan: &Plan,
    installer_url: &str,
    token_file: &str,
    architecture: &str,
) -> Commands {
    let image = installer_image(&plan.destination);
    Commands {
        medium: format!(
            "{image}-{architecture}: write it to a removable drive with plan {plan} and the token in {token_file}",
            plan = plan.plan,
        ),
        netboot: format!(
            "in a rescue or netboot system, {installer_url} installs with plan {plan} and the token in {token_file}",
            plan = plan.plan,
        ),
        alongside: format!(
            "on the machine's own Linux, {image} installs alongside it with plan {plan} and the token in {token_file}",
            plan = plan.plan,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan {
            schema: SCHEMA,
            plan: "0123456789abcdef0123456789abcdef".to_owned(),
            revision: 1,
            destination: "tilo-server".to_owned(),
            name: "cupboard".to_owned(),
            network: Network {
                addressing: "dhcp".to_owned(),
                ..Network::default()
            },
            disk: DiskRule {
                kind: "empty".to_owned(),
                identity: String::new(),
            },
            unlock: "tpm".to_owned(),
            updates: "stable".to_owned(),
            hosting: vec![Holding {
                workspace: "fedcba9876543210fedcba9876543210".to_owned(),
                mode: "key-holder".to_owned(),
            }],
            open: Vec::new(),
            token: Token {
                nonce: "00112233445566778899aabbccddeeff".to_owned(),
                expires_at_ns: 1_700_000_000_000_000_000,
            },
        }
    }

    #[test]
    fn a_complete_plan_round_trips_through_json() {
        let plan = plan();
        assert!(plan.complete());
        let text = serde_json::to_string(&plan).expect("a plan encodes");
        assert_eq!(
            serde_json::from_str::<Plan>(&text).expect("a plan decodes"),
            plan
        );
    }

    #[test]
    fn an_open_plan_names_exactly_what_is_open() {
        let mut plan = plan();
        plan.name.clear();
        plan.disk = DiskRule {
            kind: String::new(),
            identity: String::new(),
        };
        plan.open = vec!["disk".to_owned(), "name".to_owned()];
        let text = serde_json::to_string(&plan).expect("a plan encodes");
        assert!(serde_json::from_str::<Plan>(&text).is_ok());

        plan.open = vec!["name".to_owned()];
        let text = serde_json::to_string(&plan).expect("a plan encodes");
        assert!(serde_json::from_str::<Plan>(&text).is_err());
    }

    #[test]
    fn a_plan_refuses_what_its_machine_could_not_use() {
        let mut invalid = plan();
        invalid.plan = "not a plan id".to_owned();
        assert!(!invalid.valid());

        let mut invalid = plan();
        invalid.disk = DiskRule {
            kind: "serial".to_owned(),
            identity: String::new(),
        };
        assert!(!invalid.valid());

        let mut invalid = plan();
        invalid.network = Network {
            addressing: "static".to_owned(),
            ..Network::default()
        };
        assert!(!invalid.valid());

        let mut invalid = plan();
        invalid.hosting[0].mode = "mirror".to_owned();
        assert!(!invalid.valid());

        let mut invalid = plan();
        invalid.open = vec!["purpose".to_owned()];
        assert!(!invalid.valid());
    }

    #[test]
    fn a_dual_stack_plan_names_a_gateway_for_each_family() {
        let mut stack = plan();
        stack.network = Network {
            addressing: "static".to_owned(),
            addresses: "65.108.204.148/26,2a01:4f9:1a:a083::2/64".to_owned(),
            gateway: "65.108.204.129,fe80::1".to_owned(),
            dns: "185.12.64.1,185.12.64.2".to_owned(),
        };
        assert!(stack.valid(), "an IPv4 and an IPv6 gateway at once");
        let text = serde_json::to_string(&stack).expect("a plan encodes");
        assert_eq!(
            serde_json::from_str::<Plan>(&text).expect("a plan decodes"),
            stack
        );

        for gateway in ["65.108.204.129,65.108.204.130", "fe80::1,fe80::2", "gw"] {
            stack.network.gateway = gateway.to_owned();
            assert!(!stack.valid(), "two gateways of one family or none");
        }
    }

    #[test]
    fn a_disk_rule_encodes_and_decodes_one_line() {
        let rule = DiskRule {
            kind: "serial".to_owned(),
            identity: "WD-ABC123".to_owned(),
        };
        assert_eq!(rule.encode(), "serial:WD-ABC123");
        assert_eq!(
            DiskRule::decode(&rule.encode()).expect("a rule decodes"),
            rule
        );
        assert_eq!(
            DiskRule::decode("empty").expect("a rule decodes"),
            DiskRule {
                kind: "empty".to_owned(),
                identity: String::new()
            }
        );
        assert!(DiskRule::decode("empty:xyz").is_err());
        assert!(DiskRule::decode("biggest").is_err());
    }

    fn drive(name: &str, bytes: u64, holds_data: bool, boots: bool) -> Drive {
        Drive {
            model: name.to_owned(),
            identity: format!("serial-{name}"),
            bytes,
            holds_data,
            boots,
        }
    }

    #[test]
    fn a_rule_picks_exactly_one_drive_or_opens() {
        let drives = vec![
            drive("boot", 512, true, true),
            drive("data", 2048, true, false),
            drive("fresh", 4096, false, false),
        ];
        let empty = DiskRule {
            kind: "empty".to_owned(),
            identity: String::new(),
        };
        assert_eq!(choose(&empty, &drives), Choice::Drive(&drives[2]));
        let serial = DiskRule {
            kind: "serial".to_owned(),
            identity: "serial-data".to_owned(),
        };
        assert_eq!(choose(&serial, &drives), Choice::Drive(&drives[1]));
        let largest = DiskRule {
            kind: "largest".to_owned(),
            identity: String::new(),
        };
        assert_eq!(choose(&largest, &drives), Choice::Drive(&drives[2]));
        let only = DiskRule {
            kind: "only".to_owned(),
            identity: String::new(),
        };
        assert!(matches!(choose(&only, &drives), Choice::Open { .. }));
    }

    #[test]
    fn a_rule_that_matches_none_or_several_opens_with_the_reason() {
        let drives = vec![drive("a", 512, false, false), drive("b", 512, false, false)];
        let empty = DiskRule {
            kind: "empty".to_owned(),
            identity: String::new(),
        };
        match choose(&empty, &drives) {
            Choice::Open { choice, reason } => {
                assert_eq!(choice, "disk");
                assert!(reason.contains('2'), "the reason counts: {reason}");
            }
            Choice::Drive(_) => panic!("two empty drives are not one"),
        }
        let missing = DiskRule {
            kind: "serial".to_owned(),
            identity: "serial-z".to_owned(),
        };
        assert!(matches!(choose(&missing, &drives), Choice::Open { .. }));
        let largest = DiskRule {
            kind: "largest".to_owned(),
            identity: String::new(),
        };
        match choose(&largest, &drives) {
            Choice::Open { reason, .. } => assert!(reason.contains("largest")),
            Choice::Drive(_) => panic!("a tie is not a largest drive"),
        }
    }

    #[test]
    fn the_outputs_name_the_installer_for_the_destination() {
        let plan = plan();
        let commands = commands(&plan, "https://tilo.sh/install", "/tmp/token.txt", "x86_64");
        assert!(commands.medium.starts_with("tilo-server-installer-x86_64"));
        assert!(commands.netboot.contains(&plan.plan));
        assert!(commands.alongside.contains("/tmp/token.txt"));
        assert!(!commands.netboot.contains(&plan.token.nonce));
    }
}
