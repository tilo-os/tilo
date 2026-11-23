/// The VPN profiles this device has, each switched on or off from the list and opened on a page
/// of its own to change; and the ways to add one: a WireGuard tunnel made here, an installed
/// plugin's type, or a configuration's text pasted in.
view vpn_pane(page: State<Text>, chosen: State<Text>) {
    let vpn = device.vpn
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
                secondary: vpn_state_words(p),
                icon: if p.connected { .shield_check } else { .shield },
                selected: p.connected,
            ) {
                on activate: set(chosen, p.id).then(set(page, "vpn-profile"))
                trailing: {
                    row(gap: .tight, align: .center) {
                        ui.Button(if p.connected { "On" } else { "Off" }, toggled: p.connected) {
                            on press: device.set_vpn(device.vpn, profile: p.id, on: not p.connected)
                        }
                        ui.Button("This VPN", icon: .chevron_right, icon_only: true, role: .plain, size: .small) {
                            on press: set(chosen, p.id).then(set(page, "vpn-profile"))
                        }
                    }
                }
            }
        }
        ui.Button("Add a VPN", icon: .plus) {
            on press: set(page, "vpn-add")
        }
        if let problem = vpn.problem {
            ui.StatusLine(problem, status: .error)
        }
    }
}

/// The ways to add a VPN.
view vpn_add_page(page: State<Text>, chosen: State<Text>) {
    state name: Text = ""
    state kind: Text = "WireGuard"
    state text: Text = ""
    let vpn = device.vpn
    ui.Section("Add a VPN") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "vpn")
        }
        ui.ListRow("WireGuard", secondary: "A tunnel this device sets up with its own key", icon: .shield) {
            on activate: set(page, "vpn-wireguard")
        }
        if not vpn.plugins.is_empty() {
            ui.Text("Installed kinds", role: .caption, secondary: true)
            for plugin in vpn.plugins {
                ui.ListRow(plugin.name, secondary: "Set up a profile of this kind", icon: .puzzle) {
                    on activate: set(chosen, plugin.service).then(set(page, "vpn-plugin"))
                }
            }
        }
    }
    ui.Section("Paste a configuration") {
        ui.Text(
            "A WireGuard .conf or an OpenVPN configuration, pasted as text.",
            role: .caption,
            secondary: true,
        )
        ui.TextInput(bind: name, placeholder: "Name")
        ui.Dropdown(bind: kind, options: ["WireGuard", "OpenVPN"], placeholder: "Kind")
        ui.TextInput(bind: text, placeholder: "The configuration", lines: 6)
        ui.Button("Set it up", role: .primary) {
            on press: device.import_vpn(
                device.vpn,
                kind: if kind == "OpenVPN" { "openvpn" } else { "wireguard" },
                name: name,
                text: text,
            )
        }
    }
}

/// A WireGuard tunnel made here: a key pair for this device, the server and what goes through
/// it. The public key is what the server is told; the private one never leaves the device.
view vpn_wireguard_page(page: State<Text>) {
    state name: Text = ""
    state address: Text = ""
    state server_key: Text = ""
    state endpoint: Text = ""
    state allowed: Text = ""
    state dns: Text = ""
    state keepalive: Text = ""
    let vpn = device.vpn
    ui.Section("WireGuard") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "vpn-add")
        }
        if let public = vpn.generated_key {
            ui.PropertyList {
                ui.Property("This device's public key", public)
            }
            ui.Text(
                "Give this key to the server. The private one stays on this device.",
                role: .caption,
                secondary: true,
            )
            ui.Button("Make another key pair", icon: .key) {
                on press: device.generate_vpn_keys(device.vpn)
            }
        } else {
            ui.Button("Make a key pair", icon: .key) {
                on press: device.generate_vpn_keys(device.vpn)
            }
        }
    }
    ui.Section("The tunnel") {
        ui.TextInput(bind: name, placeholder: "Name")
        ui.TextInput(bind: address, placeholder: "This device's address in the tunnel, such as 10.7.0.2/32")
        ui.TextInput(bind: server_key, placeholder: "The server's public key")
        ui.TextInput(bind: endpoint, placeholder: "The server, as host:port")
        ui.TextInput(bind: allowed, placeholder: "What goes through it, comma-separated; everything when empty")
        ui.TextInput(bind: dns, placeholder: "Name servers, comma-separated")
        ui.TextInput(bind: keepalive, placeholder: "Seconds between keepalives; 0 for none")
        ui.Button("Set it up", role: .primary, enabled: vpn.generated_key != .none) {
            on press: device.create_wireguard(
                device.vpn,
                name: name,
                address: address,
                server_public_key: server_key,
                endpoint: endpoint,
                allowed_ips: allowed,
                dns: dns,
                keepalive: keepalive.to_int() ?? 0,
            )
        }
    }
}

/// A profile of an installed plugin's kind.
view vpn_plugin_page(service: Text, page: State<Text>) {
    state name: Text = ""
    state server: Text = ""
    let plugin = device.vpn.plugins.find(p -> p.service == service)
    ui.Section(if let p = plugin { p.name } else { "VPN" }) {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "vpn-add")
        }
        ui.TextInput(bind: name, placeholder: "Name")
        ui.TextInput(bind: server, placeholder: "The server, as a host name or address")
        ui.Button("Set it up", role: .primary) {
            on press: device.create_plugin_vpn(
                device.vpn,
                service: service,
                name: name,
                server: server,
            )
        }
    }
}

/// One profile's page: how it stands, and the settings its own page changes.
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
                icon: if p.connected { .shield_check } else { .shield },
            ) {
                trailing: {
                    ui.Button(if p.connected { "On" } else { "Off" }, toggled: p.connected) {
                        on press: device.set_vpn(device.vpn, profile: p.id, on: not p.connected)
                    }
                }
            }
        }
        ui.Section("Settings") {
            ui.TextInput(bind: name, placeholder: if p.name != "" { p.name })
            ui.Button("Rename", size: .small) {
                on press: device.set_vpn_name(device.vpn, profile: p.id, name: name)
            }
            ui.ListRow(
                "Joins by itself",
                secondary: if p.autoconnect { "On" } else { "Off" },
                icon: .refresh,
            ) {
                trailing: {
                    ui.Button(if p.autoconnect { "On" } else { "Off" }, toggled: p.autoconnect) {
                        on press: device.set_vpn_autoconnect(
                            device.vpn,
                            profile: p.id,
                            on: not p.autoconnect,
                        )
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
                        on press: device.set_vpn_all_traffic(
                            device.vpn,
                            profile: p.id,
                            on: not p.all_traffic,
                        )
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
                on press: device.set_vpn_endpoint(device.vpn, profile: p.id, endpoint: endpoint)
            }
            if p.kind == "wireguard" {
                ui.PropertyList {
                    if let key = p.peer_public_key {
                        ui.Property("Server's public key", key)
                    }
                    if let m = p.mtu { ui.Property("Largest packet", "{m} bytes") }
                    if let k = p.keepalive { ui.Property("Keepalive", "{k} seconds") }
                    if p.allowed_ips.is_empty() {
                        ui.Property("Routes", "Everything")
                    } else {
                        ui.Property("Routes", p.allowed_ips.join(", "))
                    }
                }
                ui.TextInput(bind: keepalive, placeholder: "Seconds between keepalives; 0 for none")
                ui.Button("Change the keepalive", size: .small) {
                    on press: device.set_vpn_keepalive(
                        device.vpn,
                        profile: p.id,
                        seconds: keepalive.to_int() ?? 0,
                    )
                }
                ui.TextInput(bind: route, placeholder: "An address to route through it, such as 10.20.0.0/16")
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button("Route it", size: .small) {
                        on press: device.add_vpn_route(device.vpn, profile: p.id, address: route)
                    }
                    ui.Button("Stop routing it", size: .small) {
                        on press: device.remove_vpn_route(device.vpn, profile: p.id, address: route)
                    }
                }
            }
            ui.TextInput(bind: mtu, placeholder: "Largest packet in bytes; empty to choose automatically")
            ui.Button("Change the packet size", size: .small) {
                on press: device.set_vpn_mtu(device.vpn, profile: p.id, mtu: mtu.to_int())
            }
        }
        ui.Section("Name servers") {
            if p.dns.is_empty() {
                ui.Text("It asks the server's own", role: .caption, secondary: true)
            }
            for server in p.dns {
                ui.ListRow(server, icon: .server) {
                    trailing: {
                        ui.Button("Ask it no more", icon: .x, icon_only: true, role: .plain, size: .small) {
                            on press: device.remove_vpn_dns(
                                device.vpn,
                                profile: p.id,
                                server: server,
                            )
                        }
                    }
                }
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.TextInput(bind: dns, placeholder: "A name server's address", fill)
                ui.Button("Ask it too", size: .small) {
                    on press: device.add_vpn_dns(device.vpn, profile: p.id, server: dns)
                }
            }
        }
        ui.Button("Delete this VPN", icon: .trash, role: .destructive) {
            on press: set(confirm_delete, true)
        }
        ui.ModalDialog(
            title: "Delete {p.name}?",
            secondary: "Cancel",
            primary: "Delete",
            destructive: true,
            open: confirm_delete,
        ) {
            on act_primary: device
                .forget_vpn(device.vpn, profile: p.id)
                .then(set(confirm_delete, false))
                .then(set(page, "vpn"))
            on act_secondary: set(confirm_delete, false)
            on dismiss: set(confirm_delete, false)
            ui.Text("Its keys go with it.", role: .caption)
        }
    } else {
        ui.EmptyState("This VPN is gone", icon: .shield, inline: true)
    }
}

/// What a VPN profile says of itself.
fn vpn_state_words(p: device.VpnProfile) -> Text {
    if p.connected {
        "Connected"
    } else if p.kind == "wireguard" {
        "WireGuard"
    } else {
        "Off"
    }
}
