/// The VPN profiles this device has, each switched on or off from the list and opened on a page
/// of its own to change; and the ways to add one: a configuration pasted in, or a profile of an
/// installed kind.
view vpn_pane(page: State<Text>, chosen: State<Text>, s: Settings) {
    state listing: Text = "profiles"
    let vpn = device.vpn
    ui.ToggleGroup(selected: if listing == "activity" { 1 } else { 0 }, label: "VPN") {
        ui.Segment("Profiles") {
            on press: set(listing, "profiles")
        }
        ui.Segment("Activity") {
            on press: set(listing, "activity")
        }
    }
    if listing == "activity" {
        ui.Section("Activity") {
            ui.ListRow("Back", icon: .chevron_left) {
                on activate: set(page, "")
            }
            if vpn.profiles.is_empty() {
                ui.EmptyState("No VPN profile", icon: .shield, inline: true)
            }
            for p in vpn.profiles {
                ui.ListRow(
                    p.name,
                    secondary: vpn_row_words(p),
                    icon: if p.state == .connected { .shield_check } else { .shield },
                ) {
                    trailing: {
                        if p.state == .connected {
                            ui.Chip("Connected", tone: .success)
                        } else {
                            ui.Chip(vpn_state_words(p), tone: .neutral)
                        }
                    }
                }
                if p.state == .connected {
                    ui.PropertyList {
                        ui.Property(
                            "Received",
                            "{rate_words(p.received_rate)} · {bytes_words(p.received)}",
                        )
                        ui.Property("Sent", "{rate_words(p.sent_rate)} · {bytes_words(p.sent)}")
                        if let seconds = p.handshake {
                            ui.Property("Last handshake", "{ago_words(seconds)}")
                        }
                        if p.tor {
                            ui.Property("Tor", "Routed over Tor")
                        }
                    }
                }
            }
        }
        if let problem = vpn.problem {
            ui.StatusLine(problem, status: .error)
        }
    } else {
        ui.Section("VPN") {
            ui.ListRow("Back", icon: .chevron_left) {
                on activate: set(page, "")
            }
            if vpn.profiles.is_empty() {
                ui.EmptyState("No VPN profile", icon: .shield, inline: true)
            }
            for p in vpn.profiles {
                ui.ListRow(
                    p.name,
                    secondary: vpn_row_words(p),
                    icon: if p.state == .connected { .shield_check } else { .shield },
                    selected: p.state == .connected,
                ) {
                    on activate: set(chosen, p.id).then(set(page, "vpn-profile"))
                    trailing: {
                        row(gap: .tight, align: .center) {
                            if p.state == .connecting or p.state == .disconnecting {
                                ui.Chip(vpn_state_words(p), tone: .neutral)
                            } else {
                                ui.Button(
                                    if p.state == .connected { "On" } else { "Off" },
                                    toggled: p.state == .connected,
                                ) {
                                    on press: if p.state == .connected {
                                        p.disconnect()
                                    } else { p.connect() }
                                }
                            }
                            ui.Button(
                                "This VPN",
                                icon: .chevron_right,
                                icon_only: true,
                                role: .plain,
                                size: .small,
                            ) {
                                on press: set(chosen, p.id).then(set(page, "vpn-profile"))
                            }
                        }
                    }
                }
                if let notice = p.notice {
                    row(gap: .related, align: .center, wrap: true) {
                        ui.Chip("Could not connect", tone: .warning)
                        ui.Text(notice, role: .caption)
                        ui.Button("Dismiss", icon: .x, size: .small, role: .plain) {
                            on press: p.dismiss()
                        }
                    }
                }
            }
            ui.Button("Add a VPN", icon: .plus) {
                on press: set(page, "vpn-add")
            }
            ui.ListRow(
                "Your devices",
                secondary: "Route through them, or let them route through this one",
                icon: .monitor_smartphone,
            ) {
                on activate: set(page, "vpn-devices")
            }
            if vpn.tor_available {
                ui.ListRow(
                    "Every connection through Tor",
                    secondary: if vpn.tor {
                        "On; every connection on this device goes through Tor"
                    } else { "Off" },
                    icon: .globe,
                ) {
                    trailing: {
                        ui.Toggle(shown: vpn.tor, label: "Over Tor") {
                            on check(on): device.set_tor(device.vpn, on: on)
                        }
                    }
                }
            }
            if let problem = vpn.problem {
                ui.StatusLine(problem, status: .error)
            }
        }
    }
}

/// The ways to add a VPN.
view vpn_add_page(page: State<Text>, chosen: State<Text>, s: Settings) {
    let vpn = device.vpn
    ui.Section("Add a VPN") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "vpn")
        }
        ui.ListRow(
            "Paste a configuration",
            secondary: "A WireGuard or OpenVPN configuration, as text",
            icon: .file_text,
        ) {
            on activate: set(page, "vpn-paste")
        }
        ui.ListRow(
            "Import a configuration from a file",
            secondary: "A configuration file on this device, such as .conf or .ovpn",
            icon: .upload,
        ) {
            on activate: file.choose(
                import_vpn_file(s),
                accept: ["text/plain", "application/octet-stream"],
            )
        }
        if not vpn.plugins.is_empty() {
            ui.Text("Installed kinds", role: .caption, secondary: true)
            for plugin in vpn.plugins {
                ui.ListRow(
                    plugin.name,
                    secondary: "Paste a configuration of this kind",
                    icon: .puzzle,
                ) {
                    on activate: set(chosen, plugin.service).then(set(page, "vpn-paste"))
                }
            }
        }
    }
}

/// A configuration pasted in: its name, its kind, and its text, as the device reads it.
view vpn_paste_page(service: Text, page: State<Text>) {
    state name: Text = ""
    state kind: Text = "WireGuard"
    state text: Text = ""
    let vpn = device.vpn
    ui.Section("Paste a configuration") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "vpn-add")
        }
        ui.TextInput(bind: name, placeholder: "Name")
        if service == "" {
            ui.Dropdown(bind: kind, options: ["WireGuard", "OpenVPN"], placeholder: "Kind")
        }
        ui.TextInput(bind: text, placeholder: "The configuration", lines: 8)
        ui.Button("Set it up", role: .primary) {
            on press: device.import_vpn(
                device.vpn,
                kind: vpn_kind_key(service, kind),
                name: name,
                text: text,
            )
        }
        ui.Text(
            "The configuration's secrets are kept by the device's VPN service; this page does not keep a copy.",
            role: .caption,
            secondary: true,
        )
    }
    if let problem = vpn.problem {
        ui.StatusLine(problem, status: .error)
    }
}

/// The kind an import names: an installed plugin's own service, or the chosen kind's key.
fn vpn_kind_key(service: Text, kind: Text) -> Text {
    if service != "" { service } else if kind == "OpenVPN" { "openvpn" } else { "wireguard" }
}

/// Adds the VPN a file on this device holds, its kind read from what the file says.
action import_vpn_file(s: Settings, file: File) (icon: .upload, label: "Import a VPN from a file") {
    let text = match file.text() {
        .ok(said) => said,
        .err(why) => "",
    }
    if text == "" {
        return
    }
    device.import_vpn(device.vpn, kind: vpn_kind_of_text(text), name: file.name, text: text)
}

/// The kind a configuration's own words name: WireGuard's interface section, or OpenVPN's.
fn vpn_kind_of_text(text: Text) -> Text {
    if text.contains("[Interface]") or text.contains("PrivateKey") {
        "wireguard"
    } else { "openvpn" }
}

/// Saves the VPN as a configuration file a person chooses: what setting it up elsewhere reads.
action vpn_file(p: device.VpnProfile) -> File (icon: .download, label: "Save the VPN as a file") {
    if let empty = empty_ask() {
        let answer = actions.call(
            address: "so.tilo.device/vpn-profile:{p.id}#export",
            arguments: empty,
        )
        if answer.status == .ended {
            if let out = answer.output {
                return File.of_text("{p.name}.conf", text_of(out))
            }
        }
    }
    File.of_text("{p.name}.conf", "")
}

/// The words a JSON value holds, or nothing where it is not one.
fn text_of(value: json.Value) -> Text {
    match value {
        .string(text) => text,
        .object(fields) => "",
        .array(items) => "",
        .number(number) => "",
        .bool(flag) => "",
        .null => "",
    }
}

/// One profile's page: how it stands, and the settings its own page changes. What is left out of
/// a change stays as it is.
view vpn_profile_page(chosen: State<Text>, page: State<Text>) {
    state name: Text = ""
    state endpoint: Text = ""
    state mtu: Text = ""
    state keepalive: Text = ""
    state dns: Text = ""
    state route: Text = ""
    state confirm_delete: Bool = false
    let vpn = device.vpn
    if let p = vpn.profiles.find(x -> x.id == chosen) {
        ui.Section(p.name) {
            ui.ListRow("Back", icon: .chevron_left) {
                on activate: set(page, "vpn")
            }
            ui.ListRow(
                "Connected",
                secondary: vpn_state_words(p),
                icon: if p.state == .connected { .shield_check } else { .shield },
            ) {
                trailing: {
                    if p.state == .connecting or p.state == .disconnecting {
                        ui.Chip(vpn_state_words(p), tone: .neutral)
                    } else {
                        ui.Button(
                            if p.state == .connected { "On" } else { "Off" },
                            toggled: p.state == .connected,
                        ) {
                            on press: if p.state == .connected {
                                p.disconnect()
                            } else { p.connect() }
                        }
                    }
                }
            }
            if let notice = p.notice {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Chip("Could not connect", tone: .warning)
                    ui.Text(notice, role: .caption)
                    ui.Button("Dismiss", icon: .x, size: .small, role: .plain) {
                        on press: p.dismiss()
                    }
                }
            }
            if not p.addresses.is_empty() {
                ui.PropertyList {
                    ui.Property("This device's address", p.addresses.join(", "))
                }
            }
        }
        ui.Section("On this device") {
            ui.PropertyList {
                ui.Property("This device's profile", p.id)
            }
            ui.Text(
                "Its keys are kept by this device's network service, which holds the profile this page changes; the page keeps no copy of them.",
                role: .caption,
                secondary: true,
            )
        }
        ui.Section("Settings") {
            ui.TextInput(bind: name, placeholder: p.name)
            ui.Button("Rename", size: .small) {
                on press: p.edit(name: name)
            }
            ui.ListRow(
                "Joins by itself",
                secondary: if p.autoconnect { "On" } else { "Off" },
                icon: .refresh,
            ) {
                trailing: {
                    ui.Button(if p.autoconnect { "On" } else { "Off" }, toggled: p.autoconnect) {
                        on press: p.edit(autoconnect: not p.autoconnect)
                    }
                }
            }
            ui.ListRow(
                "Every connection through it",
                secondary: if p.all_traffic { "On" } else { "Only its own networks" },
                icon: .globe,
            ) {
                trailing: {
                    ui.Button(if p.all_traffic { "On" } else { "Off" }, toggled: p.all_traffic) {
                        on press: p.edit(all_traffic: not p.all_traffic)
                    }
                }
            }
            ui.ListRow(
                "Route it over Tor",
                secondary: if p.tor {
                    "On; its traffic goes through this device's Tor service"
                } else { "Off" },
                icon: .globe,
            ) {
                trailing: {
                    if vpn.tor_available {
                        ui.Toggle(shown: p.tor, label: "Route it over Tor") {
                            on check(on): p.edit(tor: on)
                        }
                    } else {
                        ui.Chip("No Tor service on this device", tone: .neutral)
                    }
                }
            }
            if let server = p.endpoint {
                ui.PropertyList {
                    ui.Property("Server", server)
                }
            }
            ui.TextInput(bind: endpoint, placeholder: "The server, as host:port")
            ui.Button("Change the server", size: .small) {
                on press: p.edit(endpoint: endpoint)
            }
            ui.PropertyList {
                if let key = p.peer_public_key {
                    ui.Property("Server's public key", key)
                }
                if let m = p.mtu {
                    ui.Property("Largest packet", "{m} bytes")
                }
                if let k = p.keepalive {
                    ui.Property("Keepalive", "{k} seconds")
                }
                if p.allowed_ips.is_empty() {
                    ui.Property("Routes", "Everything")
                } else {
                    ui.Property("Routes", p.allowed_ips.join(", "))
                }
            }
            ui.TextInput(bind: keepalive, placeholder: "Seconds between keepalives; 0 for none")
            ui.Button("Change the keepalive", size: .small) {
                on press: p.edit(keepalive: keepalive.to_int() ?? 0)
            }
            ui.TextInput(
                bind: route,
                placeholder: "An address to route through it, such as 10.20.0.0/16",
            )
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Route it", size: .small) {
                    on press: p.edit(allowed_ips: with_entry(p.allowed_ips, route))
                }
                ui.Button("Stop routing it", size: .small) {
                    on press: p.edit(allowed_ips: without_entry(p.allowed_ips, route))
                }
            }
            ui.TextInput(
                bind: mtu,
                placeholder: "Largest packet in bytes; empty to choose automatically",
            )
            ui.Button("Change the packet size", size: .small) {
                on press: p.edit(mtu: mtu.to_int() ?? 0)
            }
        }
        ui.Section("Name servers") {
            if p.dns.is_empty() {
                ui.Text("It asks the server's own", role: .caption, secondary: true)
            }
            for server in p.dns {
                ui.ListRow(server, icon: .server) {
                    trailing: {
                        ui.Button(
                            "Ask it no more",
                            icon: .x,
                            icon_only: true,
                            role: .plain,
                            size: .small,
                        ) {
                            on press: p.edit(dns: without_entry(p.dns, server))
                        }
                    }
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.TextInput(bind: dns, placeholder: "A name server's address", fill)
                ui.Button("Ask it too", size: .small) {
                    on press: p.edit(dns: with_entry(p.dns, dns))
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Save it as a file", icon: .download) {
                on press: file.save(vpn_file(p), name: "{p.name}.conf")
            }
            ui.Button("Delete this VPN", icon: .trash, role: .destructive) {
                on press: set(confirm_delete, true)
            }
        }
        ui.Text(
            "The file holds the VPN's keys: keep it only as long as setting it up elsewhere takes.",
            role: .caption,
            secondary: true,
        )
        ui.ModalDialog(
            title: "Delete {p.name}?",
            secondary: "Cancel",
            primary: "Delete",
            destructive: true,
            open: confirm_delete,
        ) {
            on act_primary: p.forget().then(set(confirm_delete, false)).then(set(page, "vpn"))
            on act_secondary: set(confirm_delete, false)
            on dismiss: set(confirm_delete, false)
            ui.Text("Its keys go with it.", role: .caption)
        }
    } else {
        ui.EmptyState("This VPN is gone", icon: .shield, inline: true)
    }
}

/// `entries` with `entry` added, where it is not there yet and is not empty.
fn with_entry(entries: List<Text>, entry: Text) -> List<Text> {
    let trimmed = entry.trim()
    if trimmed == "" or entries.contains(trimmed) {
        entries
    } else { [entries, [trimmed]].flat_map(x -> x) }
}

/// `entries` without `entry`.
fn without_entry(entries: List<Text>, entry: Text) -> List<Text> {
    let trimmed = entry.trim()
    entries.filter(x -> x != trimmed)
}

/// Where a VPN profile's tunnel stands, in the words a person reads: a tunnel coming up or going
/// down says so and is not pressed again until it rests.
fn vpn_state_words(p: device.VpnProfile) -> Text {
    if p.state == .connecting {
        "Connecting…"
    } else if p.state == .disconnecting {
        "Disconnecting…"
    } else if p.state == .connected {
        "Connected"
    } else if p.kind == "wireguard" { "Down · WireGuard" } else { "Down" }
}

/// A profile's row under its name: where it stands, and the server it reaches without the port
/// or transport the tunnel dials.
fn vpn_row_words(p: device.VpnProfile) -> Text {
    if let server = p.endpoint {
        let host = server_host(server)
        if host == "" { vpn_state_words(p) } else { "{vpn_state_words(p)} · {host}" }
    } else { vpn_state_words(p) }
}

/// The host of a server as a profile writes it, without its port or transport: `home.example`,
/// `10.0.0.1`, `2001:db8::1`.
fn server_host(server: Text) -> Text {
    let first = server.split(" ").at(0) ?? server
    if first.starts_with("[") {
        (first.split("]").at(0) ?? first).replace("[", "")
    } else if first.contains(":") and first.contains("::") {
        first
    } else { first.split(":").at(0) ?? first }
}

/// A rate of bytes per second, split into its value and its unit: `6.7 MB/s`.
fn rate_words(bytes_per_second: Float) -> Text {
    if bytes_per_second < 1000.0 {
        "{bytes_per_second.to_int()} B/s"
    } else if bytes_per_second < 1000000.0 {
        "{tenths_of(bytes_per_second, 1000.0)} kB/s"
    } else if bytes_per_second < 1000000000.0 {
        "{tenths_of(bytes_per_second, 1000000.0)} MB/s"
    } else {
        "{tenths_of(bytes_per_second, 1000000000.0)} GB/s"
    }
}

/// `value` in units of `unit`, to a tenth when it is not whole: `1.5`, `1`.
fn tenths_of(value: Float, unit: Float) -> Text {
    let parts = (value / unit * 10.0 + 0.5).to_int()
    let rest = parts - parts / 10 * 10
    if rest == 0 { "{parts / 10}" } else { "{parts / 10}.{rest}" }
}

/// A count of bytes as a person reads it: `999 B`, `1.5 kB`, `84.2 MB`.
fn bytes_words(bytes: Int) -> Text {
    if bytes < 1000 {
        "{bytes} B"
    } else if bytes < 1000000 {
        "{tenths_of(bytes.to_float(), 1000.0)} kB"
    } else if bytes < 1000000000 {
        "{tenths_of(bytes.to_float(), 1000000.0)} MB"
    } else {
        "{tenths_of(bytes.to_float(), 1000000000.0)} GB"
    }
}

/// How long ago a count of seconds names: `34 s ago`, `3 min ago`, `2 hr ago`.
fn ago_words(seconds: Int) -> Text {
    if seconds < 60 {
        "{seconds} s ago"
    } else if seconds < 3600 {
        "{seconds / 60} min ago"
    } else if seconds < 86400 { "{seconds / 3600} hr ago" } else { "{seconds / 86400} days ago" }
}

/// The VPNs in one word for the network page's row.
fn vpn_words() -> Text {
    let vpn = device.vpn
    if vpn.profiles.is_empty() {
        "None set up"
    } else {
        let joined = vpn.profiles.find(p -> p.state == .connected)
        if let p = joined { "Through {p.name}" } else { "Off" }
    }
}

/// This device as one of the person's own VPNs: whether their other devices route through it,
/// which of their devices it routes through, and whether traffic is held while that is down.
view vpn_devices_page(page: State<Text>) {
    state other: Text = ""
    state profile: Text = ""
    let dv = settings.device_vpn
    let vpn = device.vpn
    ui.Section("Your devices") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "vpn")
        }
        setting(
            "Route their traffic through this device",
            "The person's other devices come out at this one",
        ) {
            ui.Toggle(bind: dv.exit)
        }
        if vpn.tor_available {
            setting(
                "Route everything through Tor",
                "Every connection on this device goes through its Tor service",
            ) {
                ui.Toggle(shown: vpn.tor, label: "Over Tor") {
                    on check(on): device.set_tor(device.vpn, on: on)
                }
            }
        } else {
            ui.Text(
                "This device has no Tor service, so no connection can be routed over Tor here.",
                role: .caption,
                secondary: true,
            )
        }
        ui.Text("Route through another of your devices", role: .caption, secondary: true)
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(bind: other, placeholder: "The device's id, as Devices lists it", fill)
            ui.Button("Through it", role: .primary, size: .small) {
                on press: settings.route_through(settings.device_vpn, device: other, on: true)
            }
            ui.Button("Straight out", size: .small) {
                on press: settings.route_through(settings.device_vpn, device: other, on: false)
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Hold traffic while it is down", size: .small) {
                on press: settings.set_kill_switch(settings.device_vpn, on: true)
            }
            ui.Button("Let it out directly", size: .small) {
                on press: settings.set_kill_switch(settings.device_vpn, on: false)
            }
        }
    }
    ui.Section("While a profile is down") {
        ui.Text(
            "A profile set to connect holds the traffic it should carry until it connects.",
            role: .caption,
            secondary: true,
        )
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(bind: profile, placeholder: "The profile's id, as the list shows", fill)
            ui.Button("Hold its traffic", size: .small) {
                on press: settings.set_profile_kill_switch(
                    settings.device_vpn,
                    profile: profile,
                    on: true,
                )
            }
            ui.Button("Let it out", size: .small) {
                on press: settings.set_profile_kill_switch(
                    settings.device_vpn,
                    profile: profile,
                    on: false,
                )
            }
            ui.Button("Lift the block", size: .small) {
                on press: settings.release_profile(settings.device_vpn, profile: profile)
            }
        }
    }
}
