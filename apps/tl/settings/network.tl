/// The Network pane: Wi-Fi, the wired link, mobile data, the hotspot and the VPN profiles of
/// this device, each read from the device's own resources and changed by their own actions, so
/// a press here, a press in quick settings and an agent's call change the same thing.
view network_pane() {
    state page: Text = ""
    state chosen: Text = ""
    if page == "all" {
        all_networks(page, chosen)
    } else if page == "known" {
        known_networks(page)
    } else if page == "wired" {
        wired_page(page)
    } else if page == "hotspot" {
        hotspot_page()
    } else if page == "advanced" {
        advanced_page()
    } else if page == "mobile" {
        mobile_page()
    } else if page == "vpn" {
        vpn_pane(page, chosen)
    } else if page == "vpn-add" {
        vpn_add_page(page, chosen)
    } else if page == "vpn-wireguard" {
        vpn_wireguard_page(page)
    } else if page == "vpn-plugin" {
        vpn_plugin_page(chosen, page)
    } else if page == "vpn-profile" {
        vpn_profile_page(chosen, page)
    } else if page == "net" {
        network_page(chosen, page)
    } else {
        network_first(page, chosen)
    }
}

/// The pane's first page: the radio, the networks in reach, mobile data, the wired link and the
/// ways on to the rest.
view network_first(page: State<Text>, chosen: State<Text>) {
    state password_for: Text = ""
    state password: Text = ""
    let net = device.network
    let wifi = net.wifi
    ui.Section("Wi-Fi") {
        if not wifi.available {
            ui.Text("This device has no Wi-Fi", role: .caption, secondary: true)
        } else {
            ui.ListRow(
                "Wi-Fi",
                secondary: if wifi.hardware_blocked {
                    "A switch on the device holds it off"
                } else if wifi.on { "On" } else { "Off" },
                icon: .wifi,
            ) {
                trailing: {
                    ui.Button(if wifi.on { "On" } else { "Off" }, toggled: wifi.on, enabled: not wifi.hardware_blocked) {
                        on press: device.set_wifi(device.network, not wifi.on)
                    }
                }
            }
            if not wifi.on {
                ui.Text(
                    if net.airplane_mode { "Airplane mode holds the radios off" } else { "Wi-Fi is off" },
                    role: .caption,
                    secondary: true,
                )
            } else if wifi.networks.is_empty() {
                ui.EmptyState("No networks in reach", icon: .wifi, inline: true)
            } else {
                let short = wifi.networks.take(6)
                network_rows(net, short, page, chosen, password_for, password)
                if wifi.networks.len() > short.len() {
                    ui.ListRow("Show all networks", secondary: "{wifi.networks.len()} in reach", icon: .list) {
                        on activate: set(page, "all")
                    }
                }
            }
            if let problem = net.problem {
                ui.StatusLine(problem, status: .error)
            }
        }
    }
    ui.Section("Mobile data") {
        let mobile = net.mobile
        if mobile.available {
            let modem = device.modem.modems.first()
            ui.ListRow(
                if let m = modem { m.name } else { "Mobile data" },
                secondary: mobile_words(mobile, modem),
                icon: .signal,
            ) {
                on activate: set(page, "mobile")
                trailing: {
                    ui.Button(if mobile.on { "On" } else { "Off" }, toggled: mobile.on) {
                        on press: device.set_mobile_data(device.network, not mobile.on)
                    }
                }
            }
        } else {
            ui.Text("This device has no mobile modem", role: .caption, secondary: true)
        }
    }
    if let wired = net.wired {
        ui.Section("Wired") {
            ui.ListRow(
                "Wired network",
                secondary: if wired.connected { "Connected" } else { "Cable unplugged" },
                icon: .ethernet_port,
                selected: wired.connected,
            ) {
                on activate: set(page, "wired")
                trailing: {
                    ui.Button(if wired.connected { "On" } else { "Off" }, toggled: wired.connected) {
                        on press: device.set_wired(device.network, not wired.connected)
                    }
                }
            }
        }
    }
    ui.Section("Connections") {
        ui.ListRow("VPN", secondary: vpn_words(), icon: .shield) {
            on activate: set(page, "vpn")
        }
        let hotspot = device.tethering
        if hotspot.available {
            ui.ListRow("Hotspot", secondary: hotspot_words(hotspot.hotspot), icon: .radio) {
                on activate: set(page, "hotspot")
                trailing: {
                    ui.Button(if hotspot.hotspot { "On" } else { "Off" }, toggled: hotspot.hotspot) {
                        on press: device.set_hotspot(device.tethering, not hotspot.hotspot)
                    }
                }
            }
        }
        if wifi.available or net.mobile.available {
            ui.ListRow("Airplane mode", secondary: if net.airplane_mode { "On" } else { "Off" }, icon: .plane) {
                trailing: {
                    ui.Button(if net.airplane_mode { "On" } else { "Off" }, toggled: net.airplane_mode) {
                        on press: device.set_airplane_mode(device.network, not net.airplane_mode)
                    }
                }
            }
        }
        ui.ListRow("Known networks", secondary: "{net.saved.len()} kept", icon: .bookmark) {
            on activate: set(page, "known")
        }
        if net.wired != .none or net.mobile.available {
            ui.ListRow("Advanced", secondary: "Enterprise networks and checks", icon: .settings) {
                on activate: set(page, "advanced")
            }
        }
    }
}

/// The networks in reach as rows: a press joins one, the password field appears for a protected
/// network this device does not know, and the chevron opens its page.
view network_rows(
    net: device.Network,
    networks: List<device.WifiNetwork>,
    page: State<Text>,
    chosen: State<Text>,
    password_for: State<Text>,
    password: State<Text>,
) {
    for n in networks {
        ui.ListRow(
            n.name,
            secondary: if n.connected {
                connectivity_words(net.connectivity)
            } else if n.known { "Known" } else if n.secure { "Secured" } else { "Open" },
            selected: n.connected,
            icon: if n.secure { .lock } else { .wifi },
            mark: if n.known { .check },
        ) {
            on activate: if n.connected {
                set(chosen, n.name).then(set(page, "net"))
            } else if n.secure and not n.known {
                set(password_for, n.name)
            } else {
                device.join_wifi(device.network, n.name)
            }
            trailing: {
                row(gap: .tight, align: .center) {
                    ui.SignalMeter(
                        level: if n.signal >= 4 {
                            .strong
                        } else if n.signal == 3 {
                            .good
                        } else if n.signal == 2 {
                            .fair
                        } else if n.signal == 1 { .weak } else { .unknown },
                    )
                    ui.Button("This network", icon: .chevron_right, icon_only: true, role: .plain, size: .small) {
                        on press: set(chosen, n.name).then(set(page, "net"))
                    }
                }
            }
        }
        if password_for == n.name {
            row(gap: .related, align: .center, wrap: true) {
                ui.TextInput(bind: password, placeholder: "Password", fill)
                ui.Button("Join", role: .primary, size: .small) {
                    on press: device
                        .connect_wifi(device.network, n.name, password)
                        .then(set(password_for, ""))
                        .then(set(password, ""))
                }
                ui.Button("Cancel", role: .plain, size: .small) {
                    on press: set(password_for, "")
                }
            }
        }
    }
}

/// Every network in reach, the joined one first.
view all_networks(page: State<Text>, chosen: State<Text>) {
    state password_for: Text = ""
    state password: Text = ""
    let net = device.network
    ui.Section("All networks", collapsible: false) {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "")
        }
        network_rows(net, net.wifi.networks, page, chosen, password_for, password)
        ui.Button("Look again", icon: .refresh_ccw, role: .plain) {
            on press: device.scan_wifi(device.network)
        }
    }
}

/// The networks this device keeps: what it joins by itself, whether it is metered and its proxy,
/// each forgettable.
view known_networks(page: State<Text>) {
    let saved = device.network.saved
    ui.Section("Known networks") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "")
        }
        if saved.is_empty() {
            ui.EmptyState("No network is kept", icon: .wifi, inline: true)
        }
        for s in saved {
            ui.ListRow(
                s.name,
                secondary: if s.autoconnect { "Joins by itself" } else { "Saved" },
                value: if s.proxy != .none { "Proxy" },
                icon: if s.kind == .wired {
                    .ethernet_port
                } else if s.kind == .mobile { .signal } else { .wifi },
                mark: if s.hidden { .eye_off },
            ) {
                trailing: {
                    ui.MenuButton("This network", icon: .more_horizontal, icon_only: true, role: .plain) {
                        ui.MenuItem(
                            if s.autoconnect { "Stop joining by itself" } else { "Join by itself" },
                            icon: .refresh,
                        ) {
                            on choose: device.set_network_autoconnect(
                                device.network,
                                s.id,
                                on: not s.autoconnect,
                            )
                        }
                        ui.MenuItem("Forget this network", icon: .trash) {
                            on choose: device.forget_network(device.network, s.id)
                        }
                    }
                }
            }
        }
    }
}

/// One network's page: how it stands and what this device keeps of it.
view network_page(name: Text, page: State<Text>) {
    state addresses: Text = ""
    state gateway: Text = ""
    state dns: Text = ""
    let net = device.network
    let n = net.wifi.networks.find(x -> x.name == name)
    ui.Section(name) {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "")
        }
        ui.PropertyList {
            ui.Property("State", if n?.connected ?? false { connectivity_words(net.connectivity) } else if n?.known ?? false { "Known" } else { "In reach" })
            ui.Property("Security", if n?.secure ?? false { "Password" } else { "Open" })
            if let signal = n?.signal { ui.Property("Signal", "{signal} of 4") }
        }
        if n?.connected ?? false {
            ui.Button("Leave this network", icon: .wifi_off) {
                on press: device.disconnect_wifi(device.network, name)
            }
        } else {
            ui.Button("Join", icon: .wifi, role: .primary) {
                on press: device.join_wifi(device.network, name)
            }
        }
    }
    let kept = net.saved.find(s -> s.name == name)
    if let saved = kept {
        ui.Section("What this device keeps") {
                ui.ListRow("Joins by itself", secondary: if saved.autoconnect { "On" } else { "Off" }, icon: .refresh) {
                    trailing: {
                        ui.Button("Change", role: .plain, size: .small) {
                            on press: device.set_network_autoconnect(
                                device.network,
                                saved.id,
                                on: not saved.autoconnect,
                            )
                        }
                    }
                }
                if let proxy = saved.proxy {
                    ui.ListRow("Proxy", secondary: proxy, icon: .globe)
                }
                if let ipv4 = saved.ipv4 {
                    ui.ListRow(
                        "Addresses",
                        secondary: if ipv4.automatic {
                            "The network hands one out"
                        } else {
                            ipv4.addresses.join(", ")
                        },
                        icon: .globe,
                    )
                    ui.TextInput(bind: addresses, placeholder: "Addresses with their prefixes, comma-separated")
                    ui.TextInput(bind: gateway, placeholder: "Router")
                    ui.TextInput(bind: dns, placeholder: "Name servers, comma-separated")
                    row(gap: .related, align: .center, wrap: true) {
                        ui.Button("Take one from the network", size: .small) {
                            on press: device.set_network_addresses(
                                device.network,
                                profile: saved.id,
                                automatic: true,
                            )
                        }
                        ui.Button("Use these addresses", size: .small) {
                            on press: device.set_network_addresses(
                                device.network,
                                profile: saved.id,
                                automatic: false,
                                addresses: addresses,
                                gateway: gateway,
                                dns: dns,
                            )
                        }
                    }
                }
            ui.Button("Forget this network", icon: .trash, role: .destructive) {
                on press: device.forget_network(device.network, saved.id)
            }
        }
    }
}

/// The wired connection's page: how it stands, its addresses and whether it wakes the machine.
view wired_page(page: State<Text>) {
    let net = device.network
    ui.Section("Wired") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "")
        }
        if let wired = net.wired {
            ui.PropertyList {
                ui.Property("State", if wired.connected { "Connected" } else { "Cable unplugged" })
                if let device_name = wired.device { ui.Property("Interface", device_name) }
                if let speed = wired.speed { ui.Property("Speed", speed) }
                if let address = wired.ipv4 { ui.Property("IPv4", address) }
            }
            ui.Button(if wired.connected { "Disconnect" } else { "Connect" }, icon: .ethernet_port) {
                on press: device.set_wired(device.network, not wired.connected)
            }
        } else {
            ui.EmptyState("This device has no wired network", icon: .ethernet_port, inline: true)
        }
    }
}

/// The hotspot's page: its name, password and band, starting it, its code to share and who is
/// on it.
view hotspot_page() {
    state name: Text = device.tethering.name ?? ""
    state password: Text = ""
    state band: Text = band_label(device.tethering.band)
    state show_code: Bool = false
    let t = device.tethering
    ui.Section("Hotspot") {
        ui.PropertyList {
            ui.Property("State", if t.hotspot { "On" } else { "Off" })
            if let named = t.name { ui.Property("Name", named) }
        }
        ui.ListRow("Share this connection", secondary: if t.hotspot { "On" } else { "Off" }, icon: .radio) {
            trailing: {
                ui.Button(if t.hotspot { "On" } else { "Off" }, toggled: t.hotspot) {
                    on press: device.set_hotspot(device.tethering, not t.hotspot)
                }
            }
        }
        if let problem = t.problem {
            ui.StatusLine(problem, status: .error)
        }
    }
    if t.available {
        ui.Section("Set it up") {
            ui.TextInput(bind: name, placeholder: "Network name")
            ui.TextInput(bind: password, placeholder: "Password, eight characters or more")
            ui.Dropdown(bind: band, options: ["Automatic", "2.4 GHz", "5 GHz"], placeholder: "Band")
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Start the hotspot", role: .primary) {
                    on press: device.configure_hotspot(device.tethering, name, password, band_key(band))
                }
                ui.Button(if show_code { "Hide the code" } else { "Share the code" }, icon: .qr_code) {
                    on press: set(show_code, not show_code)
                }
            }
            if show_code and name != "" and password != "" {
                ui.QrCode(
                    "WIFI:T:WPA;S:{name};P:{password};;",
                    alt: "Scan to join {name}",
                    secret: true,
                )
            }
        }
    }
    if t.hotspot {
        ui.Section("Devices here") {
            if t.clients.is_empty() {
                ui.Text("Nothing is sharing it yet", role: .caption, secondary: true)
            }
            for c in t.clients {
                ui.ListRow(
                    c.address,
                    secondary: if let signal = c.signal { "{signal} dBm" } else { "Signal unknown" },
                    icon: .smartphone,
                )
            }
        }
    }
}

/// The advanced page: joining an enterprise network with its identity and certificates.
view advanced_page() {
    state ssid: Text = ""
    state identity: Text = ""
    state anonymous: Text = ""
    state ca: Text = ""
    state method: Text = "PEAP"
    state inner: Text = "MSCHAPv2"
    state password: Text = ""
    state profile: Text = ""
    ui.Section("Enterprise network") {
        ui.Text(
            "A network that names this device with an identity and an organization's certificate.",
            role: .caption,
            secondary: true,
        )
        ui.TextInput(bind: ssid, placeholder: "Network name")
        ui.TextInput(bind: identity, placeholder: "Identity")
        ui.TextInput(bind: anonymous, placeholder: "Anonymous identity")
        ui.TextInput(bind: ca, placeholder: "CA certificate path")
        row(gap: .related, align: .center, wrap: true) {
            ui.Dropdown(bind: method, options: ["PEAP", "TTLS", "TLS"], placeholder: "Method")
            ui.Dropdown(bind: inner, options: ["MSCHAPv2", "PAP", "CHAP"], placeholder: "Inner authentication")
        }
        ui.TextInput(bind: profile, placeholder: "Profile name")
        ui.TextInput(bind: password, placeholder: "Password")
        ui.Button("Save and connect", role: .primary, icon: .landmark) {
            on press: device.connect_enterprise_wifi(
                device.network,
                ssid: ssid,
                identity: identity,
                ca_certificate: ca,
                method: method,
                inner_auth: inner,
                password: password,
                anonymous_identity: if anonymous != "" { anonymous },
                profile: if profile != "" { profile },
            )
        }
    }
}

/// What the radios say of a connected network.
fn connectivity_words(c: device.Connectivity) -> Text {
    if c == .portal {
        "Sign in to the network"
    } else if c == .limited {
        "Connected, no internet"
    } else if c == .offline {
        "Offline"
    } else {
        "Connected"
    }
}

/// What mobile data says: the modem's network and how strong it is.
fn mobile_words(mobile: device.Mobile, modem: Option<device.ModemUnit>) -> Text {
    if not mobile.available {
        "No modem"
    } else if not mobile.on {
        "Off"
    } else if let m = modem {
        if let technology = m.technology { technology_words(technology) } else { "On" }
    } else {
        "On"
    }
}

/// What a modem's network is called.
fn technology_words(t: device.Technology) -> Text {
    if t == .nr5g {
        "5G"
    } else if t == .lte {
        "LTE"
    } else if t == .hspa_plus {
        "HSPA+"
    } else if t == .hspa {
        "HSPA"
    } else if t == .umts {
        "3G"
    } else if t == .ev_do {
        "EV-DO"
    } else if t == .cdma {
        "CDMA"
    } else if t == .edge {
        "EDGE"
    } else if t == .gprs {
        "GPRS"
    } else if t == .gsm { "2G" } else { "On" }
}

/// What a band is called in the dropdown.
fn band_label(band: device.Band) -> Text {
    if band == .two_point_four {
        "2.4 GHz"
    } else if band == .five { "5 GHz" } else { "Automatic" }
}

/// The dropdown's band name as the action takes it.
fn band_key(label: Text) -> Text {
    if label == "2.4 GHz" {
        "two-point-four"
    } else if label == "5 GHz" { "five" } else { "automatic" }
}

/// What the hotspot says in one line.
fn hotspot_words(on: Bool) -> Text {
    if on { "Sharing this connection" } else { "Off" }
}

/// What the VPN row says: how many profiles there are and whether one is up.
fn vpn_words() -> Text {
    let vpn = device.vpn
    if vpn.profiles.is_empty() {
        "No profile"
    } else if let connected = vpn.profiles.find(p -> p.connected) {
        connected.name
    } else {
        "Off"
    }
}
