//! The `firewall` resource: what [`tilo_system_resources::firewall`] says it is.
//!
//! [`firewall-cmd`](read) answers what this computer's firewalld holds, [`apply`] hands it a
//! configuration, and [`blocked`] reads the kernel's drops from this device's journal. A
//! machine with no firewalld says so; it never answers with an empty ruleset.

use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tilo_actions::ResourceSpec;
use tilo_service::offer;
use tilo_system_resources::firewall::{
    BlockedGroup, FIREWALL, Firewall, FirewallAction, FirewallRule, FirewallZone,
};

use crate::i18n::name;

const COMMAND_OUTPUT_LIMIT: usize = 1024 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
const APPLY_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const BLOCKED_TIMEOUT: Duration = Duration::from_secs(8);
const BLOCKED_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
/// How many buckets a blocked report's trend is divided into.
pub const BLOCKED_BUCKETS: usize = 12;

/// The `firewall` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.firewall", label = "Firewall")]
pub struct FirewallReading;

/// The `firewall` resource reading `firewall`.
#[must_use]
pub fn resource(_firewall: &Firewall) -> ResourceSpec {
    offer::<FirewallAction>(tilo_resource::typed_resource::<FirewallReading>(
        FIREWALL,
        name("so.tilo.system.firewall", "Firewall"),
    ))
}

/// The `firewall` resource's surface, as a shell reads it over `so.tilo.device`.
#[must_use]
pub fn surface() -> tilo_resource::surface::Surface {
    tilo_resource::surface::reading_surface::<Firewall, FirewallAction>(FIREWALL, "Firewall")
}

/// What this computer's firewalld holds now.
#[must_use]
pub fn reading() -> Firewall {
    match import() {
        Ok(firewall) => firewall,
        Err(problem) => Firewall {
            serving: false,
            read_error: Some(problem),
            default_zone: "public".to_owned(),
            ..Firewall::default()
        },
    }
}

/// One argv-preserving system invocation. Never rebuild argv by splitting a display string.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Invocation {
    program: &'static str,
    args: Vec<String>,
}

impl Invocation {
    fn new(program: &'static str, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program,
            args: args.into_iter().collect(),
        }
    }

    fn display(&self) -> String {
        let args = self
            .args
            .iter()
            .map(|arg| {
                if arg.contains(char::is_whitespace) {
                    format!("'{}'", arg.replace('\'', "'\\\"'\\\"'"))
                } else {
                    arg.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        format!("{} {args}", self.program)
    }
}

/// Run a read-only `firewall-cmd` query and return its trimmed stdout.
///
/// Every argument set here is a query (`--get-*`, `--list-all`, `--query-*`, `--state`),
/// never a mutation.
fn query(args: &[&str]) -> Result<String, String> {
    let output = bounded_command::run(
        "firewall-cmd",
        args,
        None,
        COMMAND_OUTPUT_LIMIT,
        QUERY_TIMEOUT,
    )
    .map_err(|error| match error {
        bounded_command::Error::Process(ref io) if io.kind() == std::io::ErrorKind::NotFound => {
            "This computer has no firewalld.".to_owned()
        }
        _ => "firewalld could not be read.".to_owned(),
    })?;
    if !output.status.success() {
        return Err("firewalld is not running. Start it, then try again.".to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Read this computer's firewalld.
///
/// # Errors
/// Why it could not be read.
fn import() -> Result<Firewall, String> {
    if query(&["--state"]).ok().as_deref() != Some("running") {
        return Err("firewalld is not running. Start it, then try again.".to_owned());
    }
    let default_name = query(&["--get-default-zone"]).unwrap_or_else(|_| "public".to_owned());
    let default_zone = zone_name(&default_name).map_or_else(|| "public".to_owned(), str::to_owned);
    let panic_mode = query(&["--query-panic"]).is_ok_and(|answer| answer == "yes");
    let mut zones = Vec::new();
    for raw in query(&["--get-zones"])
        .unwrap_or_default()
        .split_whitespace()
    {
        let Some(name) = zone_name(raw) else {
            continue;
        };
        if let Ok(text) = query(&["--zone", raw, "--list-all"]) {
            zones.push(parse_list_all(name, &text));
        }
    }
    Ok(Firewall {
        serving: true,
        read_error: None,
        enabled: true,
        panic_mode,
        default_zone,
        zones,
        ..Firewall::default()
    })
}

/// The zone name a firewalld name resolves to, when it is one of the nine stock zones.
fn zone_name(name: &str) -> Option<&'static str> {
    const ALL: [&str; 9] = [
        "drop", "block", "public", "external", "dmz", "work", "home", "internal", "trusted",
    ];
    ALL.into_iter().find(|kind| *kind == name)
}

/// Parse the block emitted by `firewall-cmd --zone=<zone> --list-all`.
fn parse_list_all(kind: &str, text: &str) -> FirewallZone {
    let mut zone = FirewallZone {
        kind: kind.to_owned(),
        target: "default".to_owned(),
        ..FirewallZone::default()
    };
    let mut in_rich = false;
    for raw in text.lines() {
        let trimmed = raw.trim();
        if in_rich {
            if let Some(rule) = parse_rich_rule(trimmed) {
                zone.rules.push(rule);
                continue;
            }
            if !trimmed.is_empty() && !trimmed.starts_with("rule ") {
                in_rich = false;
            }
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        match key.trim() {
            "target" => target_label(value.trim()).clone_into(&mut zone.target),
            "interfaces" => {
                zone.interfaces = value.split_whitespace().map(str::to_owned).collect();
            }
            "services" => zone.services = value.split_whitespace().map(str::to_owned).collect(),
            "ports" => {
                for spec in value.split_whitespace() {
                    if let Some((port, proto)) = spec.split_once('/') {
                        zone.rules.push(FirewallRule {
                            port: port.to_owned(),
                            proto: proto_label(proto).to_owned(),
                            verdict: "allow".to_owned(),
                            enabled: true,
                            ..FirewallRule::default()
                        });
                    }
                }
            }
            "masquerade" => zone.masquerade = value.trim() == "yes",
            "rich rules" => in_rich = true,
            _ => {}
        }
    }
    zone
}

/// Map a firewalld target string to the reading's name.
fn target_label(target: &str) -> &'static str {
    match target {
        "ACCEPT" => "accept",
        "DROP" => "drop",
        "%%REJECT%%" | "REJECT" => "reject",
        _ => "default",
    }
}

/// The writing form of a target the reading names.
fn target_wire(target: &str) -> &'static str {
    match target {
        "accept" => "ACCEPT",
        "drop" => "DROP",
        "reject" => "%%REJECT%%",
        _ => "default",
    }
}

fn proto_label(proto: &str) -> &'static str {
    if proto == "udp" { "udp" } else { "tcp" }
}

/// Parse a single firewalld rich-rule line.
fn parse_rich_rule(line: &str) -> Option<FirewallRule> {
    if !line.starts_with("rule ") {
        return None;
    }
    let verdict = if line.contains(" accept") {
        "allow"
    } else if line.contains(" reject") || line.contains(" drop") {
        "deny"
    } else {
        return None;
    };
    let source = quoted_after(line, "source address=").unwrap_or_default();
    let (service, port, proto) = if let Some(service) = quoted_after(line, "service name=") {
        (service, String::new(), "tcp".to_owned())
    } else {
        let port = quoted_after(line, "port port=")?;
        let proto = quoted_after(line, "protocol=").unwrap_or_else(|| "tcp".to_owned());
        (String::new(), port, proto)
    };
    Some(FirewallRule {
        service,
        port,
        proto: proto_label(&proto).to_owned(),
        verdict: verdict.to_owned(),
        source,
        enabled: true,
    })
}

/// Read the double-quoted value immediately following `needle` in `hay`.
fn quoted_after(hay: &str, needle: &str) -> Option<String> {
    let at = hay.find(needle)?.checked_add(needle.len())?;
    let rest = hay.get(at..)?;
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest.get(..end)?.to_owned())
}

/// Apply `config` to this computer's firewalld, answering how many commands ran.
///
/// # Errors
/// Why the configuration was refused, or which command failed.
pub fn apply(config: &Firewall) -> Result<usize, String> {
    let plan = build_invocations(config)?;
    for invocation in &plan {
        let mut command = Command::new(invocation.program);
        command.args(&invocation.args);
        let output =
            bounded_command::run_prepared(command, None, COMMAND_OUTPUT_LIMIT, APPLY_TIMEOUT)
                .map_err(|error| format!("could not run {}: {error}", invocation.program))?;
        if !output.status.success() {
            return Err(command_failure(invocation, &output));
        }
    }
    Ok(plan.len())
}

/// Why a command the plan ran failed, in words a person reads.
fn command_failure(invocation: &Invocation, output: &bounded_command::Output) -> String {
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let guidance = if detail.contains("Access denied")
        || detail.contains("not authorized")
        || detail.contains("Authorization")
    {
        " Check your administrator authorization, then retry."
    } else if detail.contains("not running") || detail.contains("Failed to connect") {
        " firewalld is not running; start it and refresh before retrying."
    } else {
        " The live firewall was left at its last confirmed state; inspect the service and retry."
    };
    format!(
        "{} failed ({}): {}.{}",
        invocation.display(),
        output.status,
        if detail.is_empty() {
            "no diagnostic returned"
        } else {
            &detail
        },
        guidance
    )
}

fn build_invocations(config: &Firewall) -> Result<Vec<Invocation>, String> {
    validate(config)?;
    if !config.enabled {
        return Ok(vec![Invocation::new(
            "systemctl",
            ["stop".to_owned(), "firewalld".to_owned()],
        )]);
    }
    let mut out = vec![Invocation::new(
        "systemctl",
        ["start".to_owned(), "firewalld".to_owned()],
    )];
    let default_zone = zone_name(&config.default_zone).unwrap_or("public");
    out.push(firewall_cmd([format!("--set-default-zone={default_zone}")]));
    out.push(firewall_cmd([if config.panic_mode {
        "--panic-on".to_owned()
    } else {
        "--panic-off".to_owned()
    }]));
    for zone in &config.zones {
        plan_zone_args(zone, &mut out);
    }
    out.push(firewall_cmd(["--reload".to_owned()]));
    Ok(out)
}

fn firewall_cmd(args: impl IntoIterator<Item = String>) -> Invocation {
    Invocation::new("firewall-cmd", args)
}

fn plan_zone_args(zone: &FirewallZone, out: &mut Vec<Invocation>) {
    let kind = if zone_name(&zone.kind).is_some() {
        zone.kind.as_str()
    } else {
        "public"
    };
    out.push(firewall_cmd([
        "--permanent".to_owned(),
        format!("--zone={kind}"),
        format!("--set-target={}", target_wire(&zone.target)),
    ]));
    for interface in &zone.interfaces {
        out.push(firewall_cmd([
            "--permanent".to_owned(),
            format!("--zone={kind}"),
            format!("--change-interface={interface}"),
        ]));
    }
    if zone.masquerade {
        out.push(firewall_cmd([
            "--permanent".to_owned(),
            format!("--zone={kind}"),
            "--add-masquerade".to_owned(),
        ]));
    }
    for service in &zone.services {
        out.push(firewall_cmd([
            "--permanent".to_owned(),
            format!("--zone={kind}"),
            format!("--add-service={service}"),
        ]));
    }
    for rule in &zone.rules {
        if rule.enabled {
            out.push(rule_invocation(kind, rule));
        }
    }
}

fn rule_invocation(zone: &str, rule: &FirewallRule) -> Invocation {
    let allow = rule.verdict != "deny";
    let has_source = !rule.source.trim().is_empty();
    if allow && !has_source && !rule.port.is_empty() {
        return firewall_cmd([
            "--permanent".to_owned(),
            format!("--zone={zone}"),
            format!("--add-port={}/{}", rule.port, proto_label(&rule.proto)),
        ]);
    }
    firewall_cmd([
        "--permanent".to_owned(),
        format!("--zone={zone}"),
        format!("--add-rich-rule={}", rich_rule_body(rule)),
    ])
}

fn rich_rule_body(rule: &FirewallRule) -> String {
    use std::fmt::Write as _;

    let has_source = !rule.source.trim().is_empty();
    if rule.verdict != "deny" && !has_source && !rule.port.is_empty() {
        return format!(
            "port port=\"{}\" protocol=\"{}\" accept",
            rule.port,
            proto_label(&rule.proto)
        );
    }
    let mut body = String::from("rule family=\"ipv4\"");
    if has_source {
        let _ = write!(body, " source address=\"{}\"", rule.source.trim());
    }
    if rule.service.is_empty() {
        let _ = write!(
            body,
            " port port=\"{}\" protocol=\"{}\"",
            rule.port,
            proto_label(&rule.proto)
        );
    } else {
        let _ = write!(body, " service name=\"{}\"", rule.service);
    }
    let verb = if rule.verdict == "deny" {
        "reject"
    } else {
        "accept"
    };
    let _ = write!(body, " {verb}");
    body
}

/// Verify a configuration this app can hand to firewalld safely.
///
/// # Errors
/// The first malformed zone, service, port or source.
fn validate(config: &Firewall) -> Result<(), String> {
    if config.zones.is_empty() {
        return Err("the configuration has no zones".to_owned());
    }
    if zone_name(&config.default_zone).is_none() {
        return Err("the active zone is not one of the stock zones".to_owned());
    }
    for zone in &config.zones {
        if zone_name(&zone.kind).is_none() {
            return Err(format!("`{}` is not a stock zone", zone.kind));
        }
        if config
            .zones
            .iter()
            .filter(|other| other.kind == zone.kind)
            .count()
            != 1
        {
            return Err(format!("zone {} appears more than once", zone.kind));
        }
        for service in &zone.services {
            validate_service(service)?;
        }
        for rule in &zone.rules {
            if rule.service.is_empty() {
                validate_port(&rule.port)?;
            } else {
                validate_service(&rule.service)?;
            }
            validate_source(&rule.source)?;
        }
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
        return Err("service names may contain only letters, numbers, '.', '_' or '-'".to_owned());
    }
    Ok(())
}

fn validate_port(port: &str) -> Result<(), String> {
    let value = port.trim();
    if value.is_empty() {
        return Err("Enter a port or a port range.".to_owned());
    }
    let (first, last) = match value.split_once('-') {
        Some(range) => range,
        None => (value, value),
    };
    let parse = |part: &str| -> Result<u16, String> {
        part.parse::<u16>()
            .ok()
            .filter(|number| *number > 0)
            .ok_or_else(|| "Use a port from 1 to 65535.".to_owned())
    };
    let first = parse(first)?;
    let last = parse(last)?;
    if first > last {
        return Err("Start a port range with the lower port.".to_owned());
    }
    Ok(())
}

fn validate_source(source: &str) -> Result<(), String> {
    let value = source.trim();
    if value.is_empty() {
        return Ok(());
    }
    let (address, prefix) = value
        .split_once('/')
        .map_or((value, None), |(ip, prefix)| (ip, Some(prefix)));
    if address.parse::<std::net::Ipv4Addr>().is_err() {
        return Err("Enter an IPv4 address or network, such as 192.168.1.0/24.".to_owned());
    }
    if let Some(prefix) = prefix {
        prefix
            .parse::<u8>()
            .ok()
            .filter(|prefix| *prefix <= 32)
            .ok_or_else(|| "Use a network prefix from /0 to /32.".to_owned())?;
    }
    Ok(())
}

/// What the kernel dropped over the last `window_minutes`, busiest rule first.
///
/// # Errors
/// Why the journal could not be read.
pub fn blocked(window_minutes: u32) -> Result<(Vec<BlockedGroup>, u64, Option<String>), String> {
    let minutes = window_minutes.max(1);
    let since = format!("-{minutes} min");
    let output = bounded_command::run(
        "journalctl",
        &["--kernel", "--no-pager", "-o", "json", "--since", &since],
        None,
        BLOCKED_OUTPUT_LIMIT,
        BLOCKED_TIMEOUT,
    )
    .map_err(|error| match error {
        bounded_command::Error::Process(ref io) if io.kind() == std::io::ErrorKind::NotFound => {
            "The system journal is not available on this device.".to_owned()
        }
        _ => "The system journal could not be read.".to_owned(),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        return Err(
            if stderr.contains("permission") || stderr.contains("access denied") {
                "This session is not allowed to read the system journal.".to_owned()
            } else {
                "The system journal could not be read.".to_owned()
            },
        );
    }
    Ok(parse_blocked(
        &String::from_utf8_lossy(&output.stdout),
        minutes,
    ))
}

/// Parse `journalctl -o json` lines into the report.
fn parse_blocked(text: &str, window_minutes: u32) -> (Vec<BlockedGroup>, u64, Option<String>) {
    let read_at = now_ms();
    let window_ms = u64::from(window_minutes).saturating_mul(60_000);
    let start = read_at.saturating_sub(window_ms);
    let bucket = window_ms
        .checked_div(BLOCKED_BUCKETS as u64)
        .unwrap_or(1)
        .max(1);
    let last_bucket = BLOCKED_BUCKETS.saturating_sub(1);
    let mut groups: std::collections::BTreeMap<
        (String, String, Option<String>, Option<String>),
        Group,
    > = std::collections::BTreeMap::new();
    let mut skipped = false;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(message) = value.get("MESSAGE").and_then(Value::as_str) else {
            continue;
        };
        let Some(drop) = parse_drop(message) else {
            continue;
        };
        let at_ms = value
            .get("__REALTIME_TIMESTAMP")
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<u64>().ok())
            .map_or(read_at, |micros| micros.checked_div(1000).unwrap_or(0));
        let offset = at_ms.saturating_sub(start);
        let index = usize::try_from(offset.checked_div(bucket).unwrap_or(0))
            .unwrap_or(BLOCKED_BUCKETS)
            .min(last_bucket);
        let entry = groups
            .entry((
                drop.verdict.clone(),
                drop.protocol.clone(),
                drop.port.clone(),
                drop.interface.clone(),
            ))
            .or_default();
        entry.packets = entry.packets.saturating_add(1);
        entry.last_seen_ms = entry.last_seen_ms.max(at_ms);
        if let Some(slot) = entry.buckets.get_mut(index) {
            *slot = slot.saturating_add(1);
        }
        if let Some(source) = drop.source {
            entry.sources.insert(source);
        } else {
            skipped = true;
        }
    }
    let mut groups: Vec<BlockedGroup> = groups
        .into_iter()
        .map(
            |((verdict, protocol, port, interface), group)| BlockedGroup {
                verdict,
                protocol,
                port,
                interface,
                packets: group.packets,
                sources: u64::try_from(group.sources.len()).unwrap_or(u64::MAX),
                last_seen_ms: group.last_seen_ms,
                buckets: group.buckets,
                window_minutes,
            },
        )
        .collect();
    groups.sort_by(|a, b| {
        b.packets
            .cmp(&a.packets)
            .then_with(|| blocked_title(a).cmp(&blocked_title(b)))
    });
    let total = groups.iter().map(|group| group.packets).sum();
    let note = skipped.then(|| {
        "Some journal records did not name a source address; they are counted without one."
            .to_owned()
    });
    (groups, total, note)
}

/// A group's rule in words, as the app's table shows it.
fn blocked_title(group: &BlockedGroup) -> String {
    let action = if group.verdict == "REJECT" {
        "Reject"
    } else {
        "Drop"
    };
    let proto = group.protocol.to_ascii_uppercase();
    match (&group.port, proto.is_empty()) {
        (Some(port), _) => format!("{action} {proto} port {port}"),
        (None, true) => action.to_owned(),
        (None, false) => format!("{action} {proto}"),
    }
}

struct Group {
    packets: u64,
    sources: std::collections::BTreeSet<String>,
    buckets: Vec<u32>,
    last_seen_ms: u64,
}

impl Default for Group {
    fn default() -> Self {
        Self {
            packets: 0,
            sources: std::collections::BTreeSet::new(),
            buckets: vec![0; BLOCKED_BUCKETS],
            last_seen_ms: 0,
        }
    }
}

struct Drop {
    verdict: String,
    protocol: String,
    port: Option<String>,
    interface: Option<String>,
    source: Option<String>,
}

/// Read netfilter's `key=value` resource, when `message` is one.
fn parse_drop(message: &str) -> Option<Drop> {
    let looks_like_packet = (message.contains("PROTO=") || message.contains("DPT="))
        && (message.contains("SRC=")
            || message.contains("DROP=")
            || message.contains("REJECT=")
            || message.contains("IN=")
            || message.contains("OUT="));
    if !looks_like_packet {
        return None;
    }
    let field = |name: &str| {
        message
            .split_whitespace()
            .find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let action = if field("REJECT").is_some() {
        "REJECT"
    } else {
        "DROP"
    };
    Some(Drop {
        verdict: action.to_owned(),
        protocol: field("PROTO").unwrap_or_default().to_ascii_lowercase(),
        port: field("DPT"),
        interface: field("IN"),
        source: field("SRC"),
    })
}

/// The moment now, in milliseconds since the Unix epoch.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
