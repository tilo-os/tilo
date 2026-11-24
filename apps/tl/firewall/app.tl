app so.tilo.firewall "Firewall" (
    icon: .shield,
    category: .system,
    tagline: "The firewalld zones and rules this computer enforces, shared with the workspace",
    keywords: ["firewall", "firewalld", "network", "rules", "security", "ports", "zones", "sicherheit", "netzwerk"],
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

/// `items` joined with `, `, one at a time.
fn joined(items: List<Text>, at: Int) -> Text {
    if at >= items.len() {
        ""
    } else if at == 0 {
        (items.at(at) ?? "") + joined(items, 1)
    } else {
        ", " + (items.at(at) ?? "") + joined(items, at + 1)
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
    json.encode(
        ApplyConfig(
            enabled: f.enabled,
            panic_mode: f.panic_mode,
            default_zone: f.default_zone,
            zones: f.zones.map(
                zone -> ApplyZone(
                    kind: zone.kind,
                    target: zone.target,
                    interfaces: zone.interfaces,
                    services: services_of(zone),
                    rules: zone.rules.map(
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
            ),
        ),
    )
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
action import_device(f: Firewall) (icon: .rotate_cw, label: "Read this device's firewalld") {
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
}

/// Starts firewalld.
action start(f: Firewall) (icon: .power, label: "Start firewall") {
    require not f.zones.is_empty() "There is no firewall configuration yet"
    f.enabled = true
    device.apply(device.firewall, config: payload(f))
}

/// Stops firewalld.
///
/// Undo starts it again but cannot take back what reached the machine while it was open, so
/// this is asked first.
destructive action stop(f: Firewall) (icon: .power, label: "Stop firewall") {
    f.enabled = false
    device.apply(device.firewall, config: payload(f))
}

/// Enters panic mode: all traffic in and out is dropped.
///
/// Undo lifts it but cannot restore the connections it cut, so this is asked first.
destructive action enter_panic(f: Firewall) (icon: .shield_alert, label: "Enter panic mode") {
    require not f.zones.is_empty() "There is no firewall configuration yet"
    f.panic_mode = true
    device.apply(device.firewall, config: payload(f))
}

/// Leaves panic mode.
action exit_panic(f: Firewall) (icon: .shield, label: "Exit panic mode") {
    f.panic_mode = false
    device.apply(device.firewall, config: payload(f))
}

/// Makes a zone the active one.
action make_active(f: Firewall, zone: Text) (icon: .circle_check, label: "Make active zone") {
    f.default_zone = zone
    device.apply(device.firewall, config: payload(f))
}

/// Turns a rule on or off in a zone.
action toggle_rule(f: Firewall, zone: Text, rule: Text, on: Bool) (
    icon: .power,
    label: "Turn a rule on or off",
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
) {
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
) (icon: .plus, label: "Add rule") {
    require not as_service or service != "" "Choose a service"
    require as_service or port_ok(port) "That is not a port or a port range this firewall accepts"
    require source_ok(source) "That is not an IPv4 address or network"
    if let held = f.zones.find(x -> x.kind == zone) {
        let matched_service = if as_service { service_id_of(service) } else { "" }
        let matched_port = if as_service { "" } else { port.trim() }
        if let found = rule_at(held, editing) {
            found.title = rule_title_of(matched_service, matched_port)
            found.service = matched_service
            found.port = matched_port
            found.proto = proto
            found.verdict = verdict
            found.source = source.trim()
        } else {
            held.rules.insert(
                Rule(
                    title: rule_title_of(matched_service, matched_port),
                    service: matched_service,
                    port: matched_port,
                    proto: proto,
                    verdict: verdict,
                    source: source.trim(),
                ),
            )
        }
        device.apply(device.firewall, config: payload(f))
    }
}

/// Deletes a rule from a zone.
destructive action delete_rule(f: Firewall, zone: Text, rule: Text) (
    icon: .trash,
    label: "Delete rule",
) {
    if let held = f.zones.find(x -> x.kind == zone) {
        if let found = rule_at(held, rule) {
            held.rules.remove(found)
            device.apply(device.firewall, config: payload(f))
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
    state pending_delete: Text = ""
    state pending_switch: Text = ""
    let now = device.firewall
    let shown_kind = if let zone = zone_shown(f, chosen) { zone.kind } else { "" }
    let title = if shown_kind == "" { "Firewall" } else { zone_title(shown_kind) }
    ui.NavShell(content_kind: .page, title: title, sidebar_title: "Firewall") {
        sidebar: {
            stack {
                ui.NavList(selected: shown_kind) {
                    ui.NavSection("Zones") {
                        for zone in f.zones {
                            ui.NavItem(
                                zone.kind,
                                zone_title(zone.kind),
                                icon: .shield,
                                count: zone.rules.len(),
                                item: zone,
                            ) {
                                on select: set(chosen, zone.kind)
                            }
                        }
                    }
                }
                if now.panic_mode {
                    ui.StatusLine("Panic mode is on", status: .error)
                }
                if not f.enabled and not f.zones.is_empty() {
                    ui.StatusLine("The firewall is stopped", status: .warning)
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
                pending_delete,
            )
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
    delete_dialog(f, shown_kind, pending_delete)
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
    pending_delete: State<Text>,
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
            ui.PropertyList {
                ui.Property(
                    "Interfaces",
                    if zone.interfaces.is_empty() { "None" } else { joined(zone.interfaces, 0) },
                )
                ui.Property("Unmatched traffic", target_words(zone.target))
                ui.Property("Masquerading", if zone.masquerade { "On" } else { "Off" })
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
                pending_delete,
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

/// The zone's rules, each with its switch and its verbs.
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
    pending_delete: State<Text>,
) {
    if zone.rules.is_empty() {
        ui.Text("This zone has no rules yet.", role: .body, secondary: true)
    } else {
        stack(gap: .related) {
            for rule in zone.rules {
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
                    ui.Toggle(shown: rule.enabled, label: if rule.enabled { "On" } else { "Off" }) {
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
                    ui.Button("Delete {name}", icon: .trash, icon_only: true) {
                        on press: set(pending_delete, rule.id())
                    }
                }
            }
        }
    }
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

/// Asks before a rule is deleted.
view delete_dialog(f: Firewall, zone: Text, pending: State<Text>) {
    if let held = f.zones.find(x -> x.kind == zone) {
        if let rule = rule_at(held, pending) {
            let source = if rule.source.trim() == "" { "anywhere" } else { rule.source }
            let word = if rule.verdict == "allow" { "allowed" } else { "denied" }
            let name = matcher(rule)
            let protocol = rule.proto.upper()
            ui.ModalDialog(
                title: "Delete this rule?",
                open: true,
                primary: "Delete rule",
                secondary: "Cancel",
                destructive: true,
            ) {
                body: {
                    ui.Text(
                        "{name} over {protocol} from {source} will no longer be {word}.",
                        role: .body,
                    )
                }
                on act_primary: delete_rule(f, zone, pending).then(set(pending, ""))
                on dismiss: set(pending, "")
            }
        }
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
