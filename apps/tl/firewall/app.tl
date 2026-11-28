app so.tilo.firewall "Firewall" (
    icon: .shield,
    category: .system,
    tagline: "The firewalld zones and rules this computer enforces, shared with the workspace",
    keywords: ["firewall", "firewalld", "network", "rules", "security", "ports", "zones", "nftables", "ufw", "gufw", "protection", "filter", "block", "sicherheit", "netzwerk", "brandmauer", "schutz"],
    splash_budget: 250,
)

use so.tilo.device as device

/// One rule of a zone: a service or a port, what it does with matching traffic, and where
/// that traffic comes from.
type Rule (title: title) {
    /// What the rule reads as: a service's name, or its port.
    title: Text = ""
    /// The service it matches (`https`), or empty for a port rule.
    service: Text = ""
    /// The port or port range it matches (`8080`, `8000-8100`), or empty for a service rule.
    port: Text = ""
    /// `tcp` or `udp`.
    proto: Text = "tcp"
    /// `allow` or `deny`.
    verdict: Text = "allow"
    /// The source address or network; empty for anywhere.
    source: Text = ""
    /// Whether firewalld applies it on each device.
    enabled: Bool = true
}

/// One firewalld zone: what happens to unmatched traffic, the interfaces and services it
/// allows, and its rules.
type Zone (title: kind) {
    /// `drop`, `block`, `public`, `external`, `dmz`, `work`, `home`, `internal` or `trusted`.
    kind: Text = "public"
    /// What happens to traffic no rule matched: `default`, `accept`, `reject` or `drop`.
    target: Text = "default"
    /// The interfaces it claims.
    interfaces: List<Text>
    /// The services it lets through.
    services: Set<Text>
    /// Its rules, in the order they apply.
    rules: Collection<Rule>
    /// Whether it masquerades outbound traffic.
    masquerade: Bool = false
}

/// The workspace's firewall configuration: one set of zones and rules every device applies
/// to its own firewalld.
type Firewall {
    /// Whether firewalld runs.
    enabled: Bool = false
    /// Panic mode: all traffic in and out is dropped.
    panic_mode: Bool = false
    /// The active zone.
    default_zone: Text = "public"
    /// The configured zones.
    zones: Collection<Zone>
}

root firewall: Firewall

default view window for Firewall

/// A configuration as the device's `apply` verb reads it: the same fields the reading carries,
/// so one shape goes out and comes back.
type ApplyConfig {
    enabled: Bool
    panic_mode: Bool
    default_zone: Text
    zones: List<ApplyZone>
}

type ApplyZone {
    kind: Text
    target: Text
    interfaces: List<Text>
    services: List<Text>
    rules: List<ApplyRule>
    masquerade: Bool
}

type ApplyRule {
    service: Text
    port: Text
    proto: Text
    verdict: Text
    source: Text
    enabled: Bool
}

/// A well-known service firewalld knows, offered in the Services tab and the rule form.
type ServiceDef {
    key: Text
    name: Text
    ports: Text
    proto: Text
    description: Text
}

/// The services the app offers, as firewalld names them.
fn service_catalog() -> List<ServiceDef> {
    [
        ServiceDef(
            key: "ssh",
            name: "SSH",
            ports: "22",
            proto: "TCP",
            description: "Remote login over Secure Shell",
        ),
        ServiceDef(
            key: "http",
            name: "HTTP",
            ports: "80",
            proto: "TCP",
            description: "Unencrypted web server",
        ),
        ServiceDef(
            key: "https",
            name: "HTTPS",
            ports: "443",
            proto: "TCP",
            description: "Encrypted web server",
        ),
        ServiceDef(
            key: "dns",
            name: "DNS",
            ports: "53",
            proto: "TCP and UDP",
            description: "Domain name resolver",
        ),
        ServiceDef(
            key: "mdns",
            name: "mDNS",
            ports: "5353",
            proto: "UDP",
            description: "Service discovery on the local network",
        ),
        ServiceDef(
            key: "dhcpv6-client",
            name: "DHCPv6 client",
            ports: "546",
            proto: "UDP",
            description: "IPv6 address autoconfiguration",
        ),
        ServiceDef(
            key: "samba",
            name: "Samba",
            ports: "137-139, 445",
            proto: "TCP and UDP",
            description: "Windows file and printer sharing",
        ),
        ServiceDef(
            key: "samba-client",
            name: "Samba client",
            ports: "137-138",
            proto: "UDP",
            description: "Browse Windows shares",
        ),
        ServiceDef(
            key: "cockpit",
            name: "Cockpit",
            ports: "9090",
            proto: "TCP",
            description: "Web management console",
        ),
        ServiceDef(
            key: "ipp",
            name: "IPP",
            ports: "631",
            proto: "TCP",
            description: "Network printing",
        ),
        ServiceDef(
            key: "nfs",
            name: "NFS",
            ports: "2049",
            proto: "TCP",
            description: "Network file system",
        ),
        ServiceDef(
            key: "wireguard",
            name: "WireGuard",
            ports: "51820",
            proto: "UDP",
            description: "VPN tunnel",
        ),
        ServiceDef(
            key: "ntp",
            name: "NTP",
            ports: "123",
            proto: "UDP",
            description: "Network time",
        ),
        ServiceDef(
            key: "ftp",
            name: "FTP",
            ports: "21",
            proto: "TCP",
            description: "Unencrypted file transfer",
        ),
    ]
}

/// The display name of the service `id`, or the id itself when the catalog does not know it.
fn service_name(id: Text) -> Text {
    if let found = service_catalog().find(service -> service.key == id) { found.name } else { id }
}

/// The catalog's id for the display name `name`, or the name itself when the catalog does not
/// know it.
fn service_id_of(name: Text) -> Text {
    if let found = service_catalog().find(service -> service.name == name) {
        found.key
    } else { name }
}

/// A rule's name from what it matches.
fn rule_title_of(service: Text, port: Text) -> Text {
    if service != "" { service_name(service) } else { std.say("Port {port}") }
}

/// A rule's row title.
fn matcher(rule: Rule) -> Text {
    if rule.title != "" { rule.title } else { rule_title_of(rule.service, rule.port) }
}

/// A zone's name as places read it, with the acronym kept.
fn zone_title(kind: Text) -> Text {
    if kind == "drop" {
        std.say("Drop")
    } else if kind == "block" {
        std.say("Block")
    } else if kind == "public" {
        std.say("Public")
    } else if kind == "external" {
        std.say("External")
    } else if kind == "dmz" {
        std.say("DMZ")
    } else if kind == "work" {
        std.say("Work")
    } else if kind == "home" {
        std.say("Home")
    } else if kind == "internal" {
        std.say("Internal")
    } else if kind == "trusted" { std.say("Trusted") } else { kind }
}

/// What a zone is for, as its page says under the title.
fn zone_description(kind: Text) -> Text {
    if kind == "drop" {
        std.say("Incoming traffic is dropped without a reply. Outgoing traffic is allowed.")
    } else if kind == "block" {
        std.say("Incoming connections are refused. Outgoing traffic is allowed.")
    } else if kind == "public" {
        std.say("For untrusted public networks. Only the services you allow are reachable.")
    } else if kind == "external" {
        std.say("For the outside of a router that masquerades your network.")
    } else if kind == "dmz" {
        std.say("For public-facing hosts with limited access to the internal network.")
    } else if kind == "work" {
        std.say("For work networks where you mostly trust the other computers.")
    } else if kind == "home" {
        std.say("For home networks where you mostly trust the other computers.")
    } else if kind == "internal" {
        std.say("For internal networks where you mostly trust the other computers.")
    } else {
        std.say("All network connections are accepted.")
    }
}

/// What a zone may do with traffic no rule matched.
fn targets() -> List<Text> {
    ["default", "accept", "reject", "drop"]
}

/// What happens to traffic no rule matched, in the person's words.
fn target_words(target: Text) -> Text {
    if target == "accept" {
        std.say("Accepted")
    } else if target == "reject" {
        std.say("Rejected")
    } else if target == "drop" {
        std.say("Dropped")
    } else {
        std.say("Rejected by firewalld’s default policy")
    }
}

/// A zone's services as a list, so an action payload can carry them.
fn services_of(zone: Zone) -> List<Text> {
    let out: List<Text> = []
    for item in zone.services {
        out = out + [item]
    }
    out
}

/// The rules of `zone` that allow, or that deny.
fn rule_count(zone: Zone, allow: Bool) -> Int {
    let count = 0
    for rule in zone.rules {
        if (rule.verdict == "allow") == allow {
            count = count + 1
        }
    }
    count
}

/// The rule `id` of a zone, when it holds one.
fn rule_at(zone: Zone, id: Text) -> Option<Rule> {
    zone.rules.find(rule -> rule.id() == id)
}

/// Whether `port` is a port or an inclusive port range from 1 to 65535.
fn port_ok(port: Text) -> Bool {
    port_error(port) == ""
}

/// Why `port` is not a port or a port range; empty when it is.
fn port_error(port: Text) -> Text {
    let value = port.trim()
    if value == "" {
        std.say("Enter a port or a port range.")
    } else {
        let parts = value.split("-")
        if parts.len() > 2 {
            std.say("Use a port or a range such as 8000-8100.")
        } else if not whole_port(parts.at(0) ?? "") {
            std.say("Use a port from 1 to 65535.")
        } else if parts.len() == 2 and not whole_port(parts.at(1) ?? "") {
            std.say("Use a port from 1 to 65535.")
        } else if parts.len() == 2 and number(parts.at(0) ?? "") > number(parts.at(1) ?? "") {
            std.say("Start a port range with the lower port.")
        } else { "" }
    }
}

/// `text` as a whole number, or -1 when it is not one.
fn number(text: Text) -> Int {
    text.to_int() ?? -1
}

/// Whether `text` is a whole number from 1 to 65535.
fn whole_port(text: Text) -> Bool {
    let value = text.trim()
    let parsed = number(value)
    value != "" and parsed > 0 and parsed <= 65535 and "{parsed}" == value
}

/// Whether `source` is an IPv4 address or a network with a /0 to /32 prefix; empty is anywhere.
fn source_ok(source: Text) -> Bool {
    source_error(source) == ""
}

/// Why `source` is not an IPv4 address or network; empty when it is, or when it is anywhere.
fn source_error(source: Text) -> Text {
    let value = source.trim()
    if value == "" {
        ""
    } else {
        let parts = value.split("/")
        let address = parts.at(0) ?? value
        let prefix = parts.at(1)
        if parts.len() > 2 {
            std.say("Use a network prefix from /0 to /32.")
        } else if not ipv4(address) {
            std.say("Enter an IPv4 address or network, such as 192.168.1.0/24.")
        } else if let p = prefix {
            let parsed = number(p)
            if p == "" or parsed < 0 or parsed > 32 or "{parsed}" != p {
                std.say("Use a network prefix from /0 to /32.")
            } else { "" }
        } else { "" }
    }
}

/// Whether `text` is an IPv4 octet from 0 to 255.
fn octet(text: Text) -> Bool {
    let value = text.trim()
    let parsed = number(value)
    value != "" and parsed >= 0 and parsed <= 255 and "{parsed}" == value
}

/// Whether `text` is four IPv4 octets from 0 to 255.
fn ipv4(text: Text) -> Bool {
    let parts = text.split(".")
    parts.len() == 4 and parts.all(part -> octet(part))
}

/// How much a zone kind is trusted: firewalld's own order, least trusted first. The list reads
/// in it whatever order firewalld hands the zones over in.
fn zone_rank(kind: Text) -> Int {
    if kind == "drop" {
        0
    } else if kind == "block" {
        1
    } else if kind == "public" {
        2
    } else if kind == "external" {
        3
    } else if kind == "dmz" {
        4
    } else if kind == "work" {
        5
    } else if kind == "home" { 6 } else if kind == "internal" { 7 } else { 8 }
}

/// The zones in the order a person reads them: least trusted first, the active one marked where
/// it stands.
fn zones_ordered(f: Firewall) -> List<Zone> {
    f.zones.sort_by(zone -> zone_rank(zone.kind))
}

/// What a rule's sort key is, for the column `sort_by` names: what the row shows in it.
fn rule_sort_key(rule: Rule, sort_by: Text) -> Text {
    if sort_by == "protocol" {
        rule.proto.upper()
    } else if sort_by == "source" {
        if rule.source.trim() == "" { "Anywhere" } else { rule.source.trim() }
    } else if sort_by == "rule" {
        matcher(rule)
    } else if rule.verdict == "allow" { "0" } else { "1" }
}

/// The zone's rules as the person sorted them: by the rule they match, its protocol, where it
/// applies from, or what it does (allowing before denying), ascending unless they said otherwise.
fn rules_ordered(zone: Zone, sort_by: Text, descending: Bool) -> List<Rule> {
    if descending {
        zone.rules.map(rule -> rule).sort_by(rule -> rule_sort_key(rule, sort_by), desc)
    } else {
        zone.rules.map(rule -> rule).sort_by(rule -> rule_sort_key(rule, sort_by))
    }
}

/// The zone the window shows: the chosen one, else the active one, else the first.
fn zone_shown(f: Firewall, chosen: Text) -> Option<Zone> {
    if let found = f.zones.find(zone -> zone.kind == chosen) {
        .some(found)
    } else if let active = f.zones.find(zone -> zone.kind == f.default_zone) {
        .some(active)
    } else { f.zones.first() }
}

/// The configuration as the device applies it.
fn payload(f: Firewall) -> Text {
    json.encode(stored_config(f))
}

/// The workspace's configuration in the device's shape, so what this device holds and what the
/// workspace asks of it compare as one value.
fn stored_config(f: Firewall) -> ApplyConfig {
    ApplyConfig(
        enabled: f.enabled,
        panic_mode: f.panic_mode,
        default_zone: f.default_zone,
        zones: f.zones
            .map(
                zone -> ApplyZone(
                    kind: zone.kind,
                    target: zone.target,
                    interfaces: sorted(zone.interfaces),
                    services: sorted(services_of(zone)),
                    rules: zone.rules
                        .filter(rule -> rule.enabled)
                        .map(
                            rule -> ApplyRule(
                                service: rule.service,
                                port: rule.port,
                                proto: rule.proto,
                                verdict: rule.verdict,
                                source: rule.source,
                                enabled: rule.enabled,
                            ),
                        ),
                    masquerade: zone.masquerade,
                ),
            )
            .sort_by(zone -> zone.kind),
    )
}

/// What this device's firewalld holds, in the shape the workspace's configuration takes.
fn device_config(now: device.Firewall) -> ApplyConfig {
    ApplyConfig(
        enabled: now.enabled,
        panic_mode: now.panic_mode,
        default_zone: now.default_zone,
        zones: now.zones
            .map(
                zone -> ApplyZone(
                    kind: zone.kind,
                    target: zone.target,
                    interfaces: sorted(zone.interfaces),
                    services: sorted(zone.services),
                    rules: zone.rules
                        .filter(rule -> rule.enabled)
                        .map(
                            rule -> ApplyRule(
                                service: rule.service,
                                port: rule.port,
                                proto: rule.proto,
                                verdict: rule.verdict,
                                source: rule.source,
                                enabled: rule.enabled,
                            ),
                        ),
                    masquerade: zone.masquerade,
                ),
            )
            .sort_by(zone -> zone.kind),
    )
}

/// Whether this device's firewalld enforces exactly the workspace's configuration.
fn device_holds(f: Firewall, now: device.Firewall) -> Bool {
    json.encode(stored_config(f)) == json.encode(device_config(now))
}

fn sorted(items: List<Text>) -> List<Text> {
    items.sort_by(item -> item)
}

/// Applies the workspace's configuration to this device's firewalld when it does not hold it:
/// a change another device made, or one taken back, reaches this device's firewall too.
action apply_stored(f: Firewall) {
    device.apply(device.firewall, config: payload(f))
}

/// The highest count in `buckets`.
fn peak(buckets: List<Int>) -> Int {
    let high = 0
    for count in buckets {
        if count > high {
            high = count
        }
    }
    high
}

/// The trend of a blocked group as bars, oldest first.
fn trend(buckets: List<Int>) -> Text {
    let top = peak(buckets)
    if top == 0 {
        ""
    } else {
        let bars: List<Text> = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"]
        let out: Text = ""
        for count in buckets {
            let level = count * 7 / top
            out = out + (bars.at(level) ?? "█")
        }
        out
    }
}

/// What a full bar of a blocked group means.
fn trend_scale(group: device.BlockedGroup) -> Text {
    let top = peak(group.buckets)
    let minutes = (group.window_minutes / 12).max(1)
    std.say("{top} per {minutes} min")
}

/// What a blocked group does, in the person's words.
fn blocked_title(group: device.BlockedGroup) -> Text {
    let said = if group.verdict == "REJECT" { std.say("Reject") } else { std.say("Drop") }
    let proto = group.protocol.upper()
    if let port = group.port {
        std.say("{said} {proto} port {port}")
    } else if proto == "" { said } else { std.say("{said} {proto} traffic") }
}

/// The source spread of a blocked group.
fn sources_label(sources: Int) -> Text {
    if sources == 1 { std.say("from 1 address") } else { std.say("from {sources} addresses") }
}

/// Brings in what this device's firewalld holds: the zones, their services and rules, and the
/// switches. A device whose firewalld does not answer keeps the configuration it has.
action import_device(f: Firewall) (
    icon: .rotate_cw,
    label: "Read this device's firewalld",
    did: "took in this device's firewall configuration for",
) {
    let now = device.firewall
    if not now.serving or now.zones.is_empty() {
        return
    }
    f.enabled = now.enabled
    f.panic_mode = now.panic_mode
    f.default_zone = now.default_zone
    f.zones.remove_where(zone -> true)
    for held in now.zones {
        let zone = f.zones.insert(
            Zone(kind: held.kind, target: held.target, masquerade: held.masquerade),
        )
        zone.interfaces = held.interfaces
        for service in held.services {
            zone.services.add(service)
        }
        for rule in held.rules {
            zone.rules.insert(
                Rule(
                    title: rule_title_of(rule.service, rule.port),
                    service: rule.service,
                    port: rule.port,
                    proto: rule.proto,
                    verdict: rule.verdict,
                    source: rule.source,
                    enabled: rule.enabled,
                ),
            )
        }
    }
    notice("Connected to firewalld", kind: .success)
}

/// Starts firewalld.
action start(f: Firewall) (icon: .power, label: "Start firewall", did: "started") {
    require not f.zones.is_empty() "There is no firewall configuration yet"
    f.enabled = true
    device.apply(device.firewall, config: payload(f))
}

/// Stops firewalld.
///
/// Undo starts it again but cannot take back what reached the machine while it was open, so
/// this is asked first.
destructive action stop(f: Firewall) (icon: .power, label: "Stop firewall", did: "stopped") {
    f.enabled = false
    device.apply(device.firewall, config: payload(f))
}

/// Enters panic mode: all traffic in and out is dropped.
///
/// Undo lifts it but cannot restore the connections it cut, so this is asked first.
destructive action enter_panic(f: Firewall) (
    icon: .shield_alert,
    label: "Enter panic mode",
    did: "dropped all traffic with panic mode in",
) {
    require not f.zones.is_empty() "There is no firewall configuration yet"
    f.panic_mode = true
    device.apply(device.firewall, config: payload(f))
}

/// Leaves panic mode.
action exit_panic(f: Firewall) (
    icon: .shield,
    label: "Exit panic mode",
    did: "left panic mode in",
) {
    f.panic_mode = false
    device.apply(device.firewall, config: payload(f))
}

/// Makes a zone the active one.
action make_active(f: Firewall, zone: Text) (
    icon: .circle_check,
    label: "Make active zone",
    did: "made {zone} the active zone of",
) {
    f.default_zone = zone
    device.apply(device.firewall, config: payload(f))
    notice("Made {zone_title(zone)} the active zone", kind: .success)
}

/// What a zone does with traffic no rule matched: firewalld's default policy, or accept, reject
/// or drop it.
action set_target(f: Firewall, zone: Text, target: Text) (
    icon: .shield,
    label: "Set what happens to unmatched traffic",
    did: if target == "accept" {
    "set unmatched traffic to be accepted in"
} else if target == "reject" {
    "set unmatched traffic to be rejected in"
} else if target == "drop" {
    "set unmatched traffic to be dropped in"
} else {
    "left unmatched traffic to firewalld’s default policy in"
},
) {
    if target == "default" or target == "accept" or target == "reject" or target == "drop" {
        if let held = f.zones.find(x -> x.kind == zone) {
            held.target = target
            device.apply(device.firewall, config: payload(f))
        }
    }
}

/// Turns masquerading of a zone's outbound traffic on or off.
action set_masquerade(f: Firewall, zone: Text, on: Bool) (
    icon: .power,
    label: "Turn masquerading on or off",
    did: if on { "turned masquerading on in" } else { "turned masquerading off in" },
) {
    if let held = f.zones.find(x -> x.kind == zone) {
        held.masquerade = on
        device.apply(device.firewall, config: payload(f))
    }
}

/// Gives a zone a network interface; a zone that claimed it before lets it go.
action add_interface(f: Firewall, zone: Text, interface: Text) (
    icon: .plus,
    label: "Add an interface",
    did: "added {interface} to",
) {
    let name = interface.trim()
    if name != "" {
        for other in f.zones {
            if other.kind != zone and other.interfaces.contains(name) {
                other.interfaces = other.interfaces.filter(x -> x != name)
            }
        }
        if let held = f.zones.find(x -> x.kind == zone) {
            if not held.interfaces.contains(name) {
                held.interfaces = held.interfaces + [name]
                device.apply(device.firewall, config: payload(f))
            }
        }
    }
}

/// Takes a network interface from a zone.
action remove_interface(f: Firewall, zone: Text, interface: Text) (
    icon: .trash,
    label: "Remove an interface",
    did: "removed an interface from",
) {
    // A change keeps only the arguments its zone still holds: the interface is gone from it, so
    // the sentence cannot name it.
    if let held = f.zones.find(x -> x.kind == zone) {
        held.interfaces = held.interfaces.filter(x -> x != interface)
        device.apply(device.firewall, config: payload(f))
    }
}

/// Turns a rule on or off in a zone.
action toggle_rule(f: Firewall, zone: Text, rule: Text, on: Bool) (
    icon: .power,
    label: "Turn a rule on or off",
    did: if on { "turned on the rule" } else { "turned off the rule" },
) {
    if let held = f.zones.find(x -> x.kind == zone) {
        if let found = rule_at(held, rule) {
            found.enabled = on
            device.apply(device.firewall, config: payload(f))
        }
    }
}

/// Allows a service in a zone, or stops allowing it.
action allow_service(f: Firewall, zone: Text, service: Text, on: Bool) (
    icon: .power,
    label: "Allow a service",
    did: if on { "allowed {service} in" } else { "stopped allowing a service in" },
) {
    // A change keeps only the arguments its zone still holds: a service no longer allowed is
    // gone from it, so that sentence cannot name it.
    if let held = f.zones.find(x -> x.kind == zone) {
        if on { held.services.add(service) } else { held.services.remove(service) }
        device.apply(device.firewall, config: payload(f))
    }
}

/// Adds a rule to a zone, or saves the one being edited.
///
/// A rule that names no valid port or source is refused, and the form says why.
action add_rule(
    f: Firewall,
    zone: Text,
    editing: Text,
    as_service: Bool,
    service: Text,
    port: Text,
    proto: Text,
    verdict: Text,
    source: Text,
) (
    icon: .plus,
    label: "Add rule",
    did: if verdict == "deny" {
    "saved a rule that denies its traffic in"
} else {
    "saved a rule that allows its traffic in"
},
) {
    require not as_service or service != "" "Choose a service"
    require as_service or port_ok(port) "That is not a port or a port range this firewall accepts"
    require source_ok(source) "That is not an IPv4 address or network"
    if let held = f.zones.find(x -> x.kind == zone) {
        let matched_service = if as_service { service_id_of(service) } else { "" }
        let matched_port = if as_service { "" } else { port.trim() }
        let title = rule_title_of(matched_service, matched_port)
        if let found = rule_at(held, editing) {
            found.title = title
            found.service = matched_service
            found.port = matched_port
            found.proto = proto
            found.verdict = verdict
            found.source = source.trim()
            notice("Updated the rule for {title}", kind: .success)
        } else {
            held.rules.insert(
                Rule(
                    title: title,
                    service: matched_service,
                    port: matched_port,
                    proto: proto,
                    verdict: verdict,
                    source: source.trim(),
                ),
            )
            notice("Added a rule for {title}", kind: .success)
        }
        device.apply(device.firewall, config: payload(f))
    }
}

/// Deletes a rule from a zone.
destructive action delete_rule(f: Firewall, zone: Text, rule: Text) (
    icon: .trash,
    label: "Delete rule",
    did: "deleted a rule from",
) {
    if let held = f.zones.find(x -> x.kind == zone) {
        if let found = rule_at(held, rule) {
            held.rules.remove(found)
            device.apply(device.firewall, config: payload(f))
            notice("Deleted a rule from the {zone_title(zone)} zone", kind: .info)
        }
    }
}

/// Reads what the kernel dropped in the last hour.
action load_blocked() (icon: .shield_alert, label: "Show dropped traffic") {
    device.read_blocked(device.firewall, window_minutes: 60)
}

/// Opening the app reads this device's firewalld into the workspace's configuration when it
/// holds none yet.
opened action arrive(f: Firewall) {
    if f.zones.is_empty() { import_device(f) }
}

/// The window: the zones beside the open zone's rules, services and dropped traffic.
view window(f: Firewall) {
    state chosen: Text = ""
    state adding: Bool = false
    state editing: Text = ""
    state as_service: Bool = true
    state draft_service: Text = "SSH"
    state draft_port: Text = ""
    state draft_proto: Text = "tcp"
    state draft_verdict: Text = "allow"
    state draft_source: Text = ""
    state pending_switch: Text = ""
    state draft_interface: Text = ""
    // Which of a rule's own columns the list is sorted by, and whether the order is descending.
    state sort_by: Text = "action"
    state descending: Bool = false
    let now = device.firewall
    let shown_kind = if let zone = zone_shown(f, chosen) { zone.kind } else { "" }
    let title = if shown_kind == "" { "Firewall" } else { zone_title(shown_kind) }
    ui.NavShell(content_kind: .page, title: title, sidebar_title: "Firewall") {
        sidebar: {
            stack {
                ui.NavList(selected: shown_kind) {
                    ui.NavSection("Zones") {
                        for zone in zones_ordered(f) {
                            ui.NavItem(
                                zone.kind,
                                zone_title(zone.kind),
                                icon: .shield,
                                count: zone.rules.len(),
                                secondary: if zone.kind == f.default_zone {
                                    .some("Active")
                                } else { .none },
                                item: zone,
                            ) {
                                on select: set(chosen, zone.kind)
                            }
                        }
                    }
                }
            }
        }
        actions: {
            if not f.zones.is_empty() {
                if f.enabled {
                    ui.Button("Stop", icon: .power) {
                        on press: set(pending_switch, "stop")
                    }
                } else {
                    ui.Button("Start", icon: .power) {
                        on press: start(f)
                    }
                }
                if f.panic_mode {
                    ui.Button("Leave panic mode", icon: .shield) {
                        on press: exit_panic(f)
                    }
                } else {
                    ui.Button("Panic mode", icon: .shield_alert) {
                        on press: set(pending_switch, "panic")
                    }
                }
                ui.Button("Apply", icon: .save, accessible_name: "Apply the rules to the system") {
                    on press: device.apply(device.firewall, config: payload(f))
                }
                ui.Button("Add rule", icon: .plus, role: .primary) {
                    on press: set(editing, "")
                        .then(set(as_service, true))
                        .then(set(draft_service, "SSH"))
                        .then(set(draft_port, ""))
                        .then(set(draft_proto, "tcp"))
                        .then(set(draft_verdict, "allow"))
                        .then(set(draft_source, ""))
                        .then(set(adding, true))
                }
            }
            ui.Button(
                "Read firewalld",
                icon: .rotate_cw,
                icon_only: true,
                accessible_name: "Read this device's firewalld again",
            ) {
                on press: import_device(f)
            }
        }
        // What firewalld said when it refused what the configuration asked, so a rule the person
        // changed never looks in force when it is not.
        stack {
            // The workspace's configuration is this device's: when it changed somewhere else,
            // or an undo took it back, the window at this device hands it to firewalld. The
            // device's own answer arrives first, so a change this window made is not applied
            // twice.
            if now.serving
                and not f.zones.is_empty()
                and now.pending.is_empty()
                and not device_holds(f, now) {
                every 2s: apply_stored(f)
            }
            if f.panic_mode {
                ui.Banner(
                    "Panic mode is on",
                    message: "All incoming and outgoing network traffic is dropped.",
                    kind: .error,
                    icon: .shield_alert,
                )
            }
            if not f.enabled and not f.zones.is_empty() {
                ui.Banner(
                    "The firewall is stopped",
                    message: "No rules are enforced until you turn it on.",
                    kind: .warning,
                    icon: .power,
                )
            }
            if let problem = now.problem {
                ui.Banner(problem, kind: .error, icon: .triangle_alert)
            }
            if f.zones.is_empty() {
                unavailable(f, now)
            } else if let zone = zone_shown(f, chosen) {
                zone_page(
                    f,
                    now,
                    zone,
                    adding,
                    editing,
                    as_service,
                    draft_service,
                    draft_port,
                    draft_proto,
                    draft_verdict,
                    draft_source,
                    draft_interface,
                    sort_by,
                    descending,
                )
            }
        }
    }
    add_rule_dialog(
        f,
        shown_kind,
        adding,
        editing,
        as_service,
        draft_service,
        draft_port,
        draft_proto,
        draft_verdict,
        draft_source,
    )
    switch_dialog(f, pending_switch)
}

/// What a computer whose firewalld does not answer says: the reason it gave, and a way to ask
/// again.
view unavailable(f: Firewall, now: device.Firewall) {
    ui.EmptyState(
        "Firewalld isn't available",
        message: if let problem = now.read_error {
            problem
        } else {
            "This computer's firewall service is not running. Start it, then try again."
        },
        icon: .shield,
        action: "Try again",
    ) {
        on act: import_device(f)
    }
}

/// The open zone: how it stands, its figures and details, and its rules, services or dropped
/// traffic.
view zone_page(
    f: Firewall,
    now: device.Firewall,
    zone: Zone,
    adding: State<Bool>,
    editing: State<Text>,
    as_service: State<Bool>,
    draft_service: State<Text>,
    draft_port: State<Text>,
    draft_proto: State<Text>,
    draft_verdict: State<Text>,
    draft_source: State<Text>,
    draft_interface: State<Text>,
    sort_by: State<Text>,
    descending: State<Bool>,
) {
    let allow = rule_count(zone, true)
    let deny = rule_count(zone, false)
    let zone_label = zone_title(zone.kind)
    stack(gap: .group) {
        if zone.kind != f.default_zone {
            row(gap: .tight) {
                ui.Button("Make active zone", icon: .circle_check) {
                    on press: make_active(f, zone.kind)
                }
            }
        }
        ui.Text(zone_description(zone.kind), role: .body, secondary: true)
        ui.StatGroup {
            ui.StatTile("Allowed services", "{zone.services.len()}")
            ui.StatTile("Allow rules", "{allow}")
            ui.StatTile("Deny rules", "{deny}")
        }
        ui.Section("Details") {
            form {
                field("Unmatched traffic") {
                    ui.Dropdown(
                        value: targets().find(x -> x == zone.target) ?? "default",
                        options: targets().map(x -> target_words(x)),
                        values: targets(),
                        caption: "Unmatched traffic",
                    ) {
                        on change(target: Text): set_target(f, zone.kind, target)
                    }
                }
                field("Masquerading") {
                    ui.Toggle(
                        shown: zone.masquerade,
                        label: if zone.masquerade { "On" } else { "Off" },
                    ) {
                        on check(on): set_masquerade(f, zone.kind, on)
                    }
                }
                field("Interfaces") {
                    stack(gap: .tight) {
                        if zone.interfaces.is_empty() {
                            ui.Text("None", role: .body, secondary: true)
                        }
                        for name in zone.interfaces {
                            row(gap: .related, align: .center) {
                                ui.Text(name, role: .body)
                                spacer
                                ui.Button("Remove {name}", icon: .trash, icon_only: true) {
                                    on press: remove_interface(f, zone.kind, name)
                                }
                            }
                        }
                        row(gap: .related, align: .center) {
                            ui.TextInput(bind: draft_interface, placeholder: "eth0") {
                            }
                            ui.Button("Add", icon: .plus) {
                                on press: add_interface(f, zone.kind, draft_interface)
                                    .then(
                                        set(draft_interface, ""),
                                    )
                            }
                        }
                    }
                }
            }
        }
        ui.Section("Rules") {
            rules_page(
                f,
                zone,
                editing,
                as_service,
                draft_service,
                draft_port,
                draft_proto,
                draft_verdict,
                draft_source,
                adding,
                sort_by,
                descending,
            )
        }
        ui.Section("Services") {
            services_page(f, zone)
        }
        ui.Section("Dropped traffic") {
            blocked_page(now)
        }
        if now.pending.contains("apply") {
            ui.StatusLine("Applying the rules to this system", status: .active)
        }
        if adding {
            ui.Text("Adding a rule to {zone_label}", role: .caption, secondary: true)
        }
    }
}

/// The zone's rules, each with its switch and its verbs, in the order the person chose by any of
/// the columns it shows.
view rules_page(
    f: Firewall,
    zone: Zone,
    editing: State<Text>,
    as_service: State<Bool>,
    draft_service: State<Text>,
    draft_port: State<Text>,
    draft_proto: State<Text>,
    draft_verdict: State<Text>,
    draft_source: State<Text>,
    adding: State<Bool>,
    sort_by: State<Text>,
    descending: State<Bool>,
) {
    if zone.rules.is_empty() {
        ui.Text("This zone has no rules yet.", role: .body, secondary: true)
    } else {
        stack(gap: .related) {
            row(gap: .related, align: .center, wrap: true) {
                ui.ToggleGroup(selected: sort_index(sort_by), caption: "Sort by") {
                    ui.Segment("Action")
                    ui.Segment("Rule")
                    ui.Segment("Protocol")
                    ui.Segment("Source")
                    on change(index): set(sort_by, sort_name(index))
                }
                ui.Toggle(shown: descending, label: "Descending") {
                    on check(on): set(descending, on)
                }
            }
            for rule in rules_ordered(zone, sort_by, descending) {
                let source = if rule.source.trim() == "" { "Anywhere" } else { rule.source.trim() }
                let name = matcher(rule)
                let protocol = rule.proto.upper()
                row(gap: .related, align: .center) {
                    ui.Badge(
                        if rule.verdict == "allow" { "Allow" } else { "Deny" },
                        tone: if rule.verdict == "allow" { .success } else { .danger },
                    )
                    stack(gap: .none) {
                        ui.Text(name, role: .body)
                        ui.Text("{protocol} · {source}", role: .caption, secondary: true)
                    }
                    spacer
                    ui.Toggle(
                        shown: rule.enabled,
                        label: if rule.enabled { "On" } else { "Off" },
                        accessible_name: "Enable the {name} rule",
                    ) {
                        on check(on): toggle_rule(f, zone.kind, rule.id(), on)
                    }
                    ui.Button("Edit {name}", icon: .pencil, icon_only: true) {
                        on press: set(editing, rule.id())
                            .then(set(as_service, rule.service != ""))
                            .then(
                                set(
                                    draft_service,
                                    if rule.service != "" {
                                        service_name(rule.service)
                                    } else { "SSH" },
                                ),
                            )
                            .then(set(draft_port, rule.port))
                            .then(set(draft_proto, rule.proto))
                            .then(set(draft_verdict, rule.verdict))
                            .then(set(draft_source, rule.source))
                            .then(set(adding, true))
                    }
                    ui.Button("Delete {name}", icon: .trash, icon_only: true, key: "Delete") {
                        on press: delete_rule(f, zone.kind, rule.id())
                    }
                }
            }
        }
    }
}

/// Where the sort control stands for `sort_by`: the index of its segment.
fn sort_index(sort_by: Text) -> Int {
    if sort_by == "rule" {
        1
    } else if sort_by == "protocol" { 2 } else if sort_by == "source" { 3 } else { 0 }
}

/// The column the segment at `index` sorts by.
fn sort_name(index: Int) -> Text {
    if index == 1 {
        "rule"
    } else if index == 2 { "protocol" } else if index == 3 { "source" } else { "action" }
}

/// The catalog's services, each with a switch that lets it through the zone.
view services_page(f: Firewall, zone: Zone) {
    stack(gap: .related) {
        for service in service_catalog() {
            row(gap: .related, align: .center) {
                stack(gap: .none) {
                    ui.Text(service.name, role: .body)
                    ui.Text(
                        "{service.description} · {service.proto} {service.ports}",
                        role: .caption,
                        secondary: true,
                    )
                }
                spacer
                ui.Toggle(
                    shown: zone.services.contains(service.key),
                    label: if zone.services.contains(service.key) {
                        "Allowed"
                    } else { "Blocked" },
                    accessible_name: "Allow {service.name}",
                ) {
                    on check(on): allow_service(f, zone.kind, service.key, on)
                }
            }
        }
    }
}

/// What the kernel dropped, grouped by rule, with the window's shape.
view blocked_page(now: device.Firewall) {
    if let error = now.blocked_error {
        ui.EmptyState(
            "Dropped traffic couldn't be read",
            message: error,
            icon: .shield,
            action: "Try again",
        ) {
            on act: load_blocked()
        }
    } else if now.blocked_read_at_ms == 0 {
        stack(gap: .group) {
            ui.Text(
                "Read what the kernel dropped in the last hour from this device's journal.",
                role: .body,
                secondary: true,
            )
            ui.Button("Read dropped traffic", icon: .shield_alert) {
                on press: load_blocked()
            }
        }
    } else {
        stack(gap: .group) {
            if now.pending.contains("read-blocked") {
                ui.Text(
                    "Reading dropped traffic from the system journal…",
                    role: .body,
                    secondary: true,
                )
            }
            row(gap: .tight, align: .end) {
                ui.Text("{now.blocked_total}", role: .heading, numeric: true)
                ui.Text(
                    "packets in the last {now.blocked_window_minutes} min",
                    role: .caption,
                    secondary: true,
                )
                spacer
                ui.Button("Refresh", icon: .refresh) {
                    on press: load_blocked()
                }
            }
            if let note = now.blocked_note {
                ui.Text(note, role: .caption, secondary: true)
            }
            if now.blocked.is_empty() {
                ui.EmptyState(
                    "Nothing was dropped",
                    message: "The kernel logged no dropped or rejected packets in the last {now.blocked_window_minutes} minutes.",
                    icon: .shield,
                )
            } else {
                stack(gap: .related) {
                    for group in now.blocked {
                        let title = blocked_title(group)
                        let spread = sources_label(group.sources)
                        let shape = trend(group.buckets)
                        row(gap: .related, align: .center) {
                            ui.Badge(
                                group.verdict,
                                tone: if group.verdict == "REJECT" { .danger } else { .warning },
                            )
                            stack(gap: .none) {
                                ui.Text(title, role: .body)
                                ui.Text("{spread} · {shape}", role: .caption, secondary: true)
                            }
                            spacer
                            ui.Text("{group.packets}", role: .body, numeric: true)
                        }
                    }
                }
                if let first_group = now.blocked.first() {
                    let scale = trend_scale(first_group)
                    ui.Text("A full bar is {scale}", role: .caption, secondary: true)
                }
            }
        }
    }
}

/// Asks for a new rule in the open zone, or for the one being edited.
view add_rule_dialog(
    f: Firewall,
    zone: Text,
    open: State<Bool>,
    editing: State<Text>,
    as_service: State<Bool>,
    service: State<Text>,
    port: State<Text>,
    proto: State<Text>,
    verdict: State<Text>,
    source: State<Text>,
) {
    let zone_label = zone_title(zone)
    let title = if editing == "" {
        std.say("Add a rule to {zone_label}")
    } else { std.say("Edit the rule in {zone_label}") }
    ui.ModalDialog(
        title: title,
        open: open,
        primary: if editing == "" { "Add rule" } else { "Save rule" },
        secondary: "Cancel",
    ) {
        body: {
            stack(gap: .related) {
                ui.ToggleGroup(selected: if as_service { 0 } else { 1 }, label: "Match") {
                    ui.Segment("Service")
                    ui.Segment("Port")
                    on change(index): set(as_service, index == 0)
                }
                if as_service {
                    ui.Dropdown(
                        bind: service,
                        options: service_catalog().map(entry -> entry.name),
                        caption: "Service",
                    )
                } else {
                    text("Port", role: .caption, secondary: true)
                    ui.TextInput(bind: port, placeholder: "8080 or 8000-8100") {
                    }
                    if port_error(port) != "" {
                        ui.StatusLine(port_error(port), status: .error)
                    }
                    ui.ToggleGroup(
                        selected: if proto == "udp" { 1 } else { 0 },
                        label: "Protocol",
                    ) {
                        ui.Segment("TCP")
                        ui.Segment("UDP")
                        on change(index): set(proto, if index == 1 { "udp" } else { "tcp" })
                    }
                }
                ui.ToggleGroup(selected: if verdict == "deny" { 1 } else { 0 }, label: "Action") {
                    ui.Segment("Allow")
                    ui.Segment("Deny")
                    on change(index): set(verdict, if index == 1 { "deny" } else { "allow" })
                }
                text("Source", role: .caption, secondary: true)
                ui.TextInput(bind: source, placeholder: "Anywhere") {
                }
                if source_error(source) != "" {
                    ui.StatusLine(source_error(source), status: .error)
                } else {
                    ui.Text(
                        "An IPv4 address or network, such as 192.168.1.0/24",
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        }
        on act_primary: add_rule(
                f,
                zone,
                editing,
                as_service,
                service,
                port,
                proto,
                verdict,
                source,
            )
            .then(set(open, false))
        on dismiss: set(open, false)
    }
}

/// Asks before a switch that can cut the session it is used from.
view switch_dialog(f: Firewall, pending: State<Text>) {
    if pending != "" {
        let stopping = pending == "stop"
        ui.ModalDialog(
            title: if stopping { "Turn off the firewall?" } else { "Enter panic mode?" },
            open: true,
            primary: if stopping { "Turn off the firewall" } else { "Drop all traffic" },
            secondary: "Cancel",
            destructive: true,
        ) {
            body: {
                ui.Text(
                    if stopping {
                        "No rules will be enforced until you turn it on again. If this machine is reachable from a network you do not control, that connection can reach it without the rules you set."
                    } else {
                        "All incoming and outgoing traffic is dropped, including the remote session you may be using to fix this machine. Anything already connected is cut."
                    },
                    role: .body,
                )
            }
            on act_primary: if stopping {
                stop(f).then(set(pending, ""))
            } else { enter_panic(f).then(set(pending, "")) }
            on dismiss: set(pending, "")
        }
    }
}
