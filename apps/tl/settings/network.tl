/// The Network pane: Wi-Fi, the wired link, mobile data, the hotspot and the VPN profiles of
/// this device, each read from the device's own resources and changed by their own actions, so
/// a press here, a press in quick settings and an agent's call change the same thing.
view network_pane(s: Settings) {
    state page: Text = ""
    state chosen: Text = ""
    if page == "all" {
        all_networks(page, chosen)
    } else if page == "known" {
        known_networks(page, chosen)
    } else if page == "hidden" {
        hidden_network_page(page)
    } else if page == "wired" {
        wired_page(page)
    } else if page == "hotspot" {
        read_pane(of: device.tethering, what: "Hotspot") {
            hotspot_page()
        }
    } else if page == "advanced" {
        advanced_page()
    } else if page == "mobile" {
        read_pane(of: device.modem, what: "Mobile data") {
            mobile_page()
        }
    } else if page == "vpn" {
        read_pane(of: device.vpn, what: "VPN") {
            vpn_pane(page, chosen, s)
        }
    } else if page == "vpn-add" {
        read_pane(of: device.vpn, what: "VPN") {
            vpn_add_page(page, chosen, s)
        }
    } else if page == "vpn-devices" {
        read_pane(of: device.vpn, what: "VPN") {
            vpn_devices_page(page)
        }
    } else if page == "vpn-paste" {
        read_pane(of: device.vpn, what: "VPN") {
            vpn_paste_page(chosen, page)
        }
    } else if page == "vpn-profile" {
        read_pane(of: device.vpn, what: "VPN") {
            vpn_profile_page(chosen, page)
        }
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
                    ui.Button(
                        if wifi.on { "On" } else { "Off" },
                        toggled: wifi.on,
                        enabled: not wifi.hardware_blocked,
                        description: "A switch on the device holds Wi-Fi off.",
                    ) {
                        on press: device.set_wifi(device.network, not wifi.on)
                    }
                }
            }
            if not wifi.on {
                ui.Text(
                    if net.airplane_mode {
                        "Airplane mode holds the radios off"
                    } else { "Wi-Fi is off" },
                    role: .caption,
                    secondary: true,
                )
            } else if net.connectivity == .portal {
                // The network answers with a page of its own: it opens in the app that opens web
                // addresses, where the person signs in once.
                if let uri = net.portal_uri {
                    ui.ListRow(
                        "Sign in to this network",
                        secondary: if let problem = net.portal_problem {
                            problem
                        } else {
                            "The network asks for a sign-in page before it lets traffic out"
                        },
                        icon: .external_link,
                    ) {
                        trailing: {
                            ui.Button("Open sign-in page", role: .primary) {
                                on press: device.open_link(device.invite, link: uri)
                            }
                        }
                    }
                }
            } else if wifi.networks.is_empty() {
                ui.EmptyState("No networks in reach", icon: .wifi, inline: true)
            } else {
                let short = wifi.networks.take(6)
                network_rows(net, short, page, chosen, password_for, password)
                if wifi.networks.len() > short.len() {
                    ui.ListRow(
                        "Show all networks",
                        secondary: "{wifi.networks.len()} in reach",
                        icon: .list,
                    ) {
                        on activate: set(page, "all")
                    }
                }
            }
            if let problem = net.problem {
                ui.StatusLine(problem, status: .error)
            }
            if wifi.available {
                ui.ListRow(
                    "Open networks",
                    secondary: if net.ask_before_open_networks {
                        "Asks before joining one this device keeps"
                    } else { "Joins the ones it keeps by itself" },
                    icon: if net.ask_before_open_networks { .eye } else { .wifi },
                ) {
                    trailing: {
                        ui.Button(
                            if net.ask_before_open_networks {
                                "Ask first"
                            } else { "Join by itself" },
                            toggled: net.ask_before_open_networks,
                        ) {
                            on press: device.set_ask_before_open_networks(
                                device.network,
                                not net.ask_before_open_networks,
                            )
                        }
                    }
                }
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
                    ui.Button(
                        if wired.connected { "On" } else { "Off" },
                        toggled: wired.connected,
                    ) {
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
                    ui.Button(
                        if hotspot.hotspot { "On" } else { "Off" },
                        toggled: hotspot.hotspot,
                    ) {
                        on press: device.set_hotspot(device.tethering, not hotspot.hotspot)
                    }
                }
            }
        }
        if wifi.available or net.mobile.available {
            ui.ListRow(
                "Airplane mode",
                secondary: if net.airplane_mode { "On" } else { "Off" },
                icon: .plane,
            ) {
                trailing: {
                    ui.Button(
                        if net.airplane_mode { "On" } else { "Off" },
                        toggled: net.airplane_mode,
                    ) {
                        on press: device.set_airplane_mode(device.network, not net.airplane_mode)
                    }
                }
            }
        }
        ui.ListRow("Known networks", secondary: "{net.saved.len()} kept", icon: .bookmark) {
            on activate: set(page, "known")
        }
        ui.ListRow(
            "Join a hidden network",
            secondary: "One that does not say its name",
            icon: .eye_off,
        ) {
            on activate: set(page, "hidden")
        }
        if net.wired != .none or net.mobile.available {
            ui.ListRow("Advanced", secondary: "Enterprise networks and checks", icon: .settings) {
                on activate: set(page, "advanced")
            }
        }
    }
}

/// Joining a network that does not say its name: the name and its password.
view hidden_network_page(page: State<Text>) {
    state name: Text = ""
    state password: Text = ""
    ui.Section("A hidden network") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "")
        }
        ui.TextInput(bind: name, placeholder: "The network's name")
        ui.TextInput(bind: password, placeholder: "Password, where it has one")
        ui.Button("Join it", icon: .wifi, role: .primary) {
            // Named arguments cannot tell the network's name from the root they act on here:
            // both are called `network`, so the call gives them in order.
            on press: device.connect_hidden_wifi(device.network, name, .some(password))
        }
        ui.Text(
            "A network whose name is not broadcast is joined by typing it. Where it has no password, leave that empty.",
            role: .caption,
            secondary: true,
        )
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
            } else if n.secure and not n.known { set(password_for, n.name) } else { n.join() }
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
                    ui.Button(
                        "This network",
                        icon: .chevron_right,
                        icon_only: true,
                        role: .plain,
                        size: .small,
                    ) {
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
/// each forgettable, and each opening what this device keeps of it.
view known_networks(page: State<Text>, chosen: State<Text>) {
    let net = device.network
    let saved = net.saved
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
                secondary: if s.open and net.ask_before_open_networks {
                    "Asks first"
                } else if s.autoconnect { "Joins by itself" } else { "Saved" },
                value: if s.proxy != .none { "Proxy" },
                icon: if s.kind == .wired {
                    .ethernet_port
                } else if s.kind == .mobile { .signal } else { .wifi },
                mark: if s.hidden { .eye_off },
            ) {
                on activate: set(chosen, s.name).then(set(page, "net"))
                trailing: {
                    ui.MenuButton(
                        "This network",
                        icon: .more_horizontal,
                        icon_only: true,
                        role: .plain,
                    ) {
                        if not (s.open and net.ask_before_open_networks) {
                            ui.MenuItem(
                                if s.autoconnect {
                                    "Stop joining by itself"
                                } else { "Join by itself" },
                                icon: .refresh,
                            ) {
                                on choose: s.set_autoconnect(not s.autoconnect)
                            }
                        }
                        ui.MenuItem("Forget this network", icon: .trash) {
                            on choose: s.forget()
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
    state ipv6_method: Text = ""
    state ipv6_addresses: Text = ""
    state ipv6_gateway: Text = ""
    state ipv6_dns: Text = ""
    state priority: Text = ""
    state proxy_address: Text = ""
    state password: Text = ""
    state shared: Bool = false
    let net = device.network
    let n = net.wifi.networks.find(x -> x.name == name)
    ui.Section(name) {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "")
        }
        ui.PropertyList {
            ui.Property(
                "State",
                if n?.connected ?? false {
                    connectivity_words(net.connectivity)
                } else if n?.known ?? false { "Known" } else { "In reach" },
            )
            ui.Property("Security", if n?.secure ?? false { "Password" } else { "Open" })
            if let signal = n?.signal {
                ui.Property("Signal", "{signal} of 4")
            }
        }
        if let w = n {
            if w.connected {
                ui.Button("Leave this network", icon: .wifi_off) {
                    on press: w.leave()
                }
            } else {
                ui.Button("Join", icon: .wifi, role: .primary) {
                    on press: w.join()
                }
            }
        }
    }
    let kept = net.saved.find(s -> s.name == name)
    if let saved = kept {
        ui.Section("What this device keeps") {
            if saved.open and net.ask_before_open_networks {
                ui.ListRow(
                    "Joins by itself",
                    secondary: "Never: this device asks before joining an open network",
                    icon: .eye,
                )
            } else {
                ui.ListRow(
                    "Joins by itself",
                    secondary: if saved.autoconnect { "On" } else { "Off" },
                    icon: .refresh,
                ) {
                    trailing: {
                        ui.Button("Change", role: .plain, size: .small) {
                            on press: saved.set_autoconnect(not saved.autoconnect)
                        }
                    }
                }
            }
            if let proxy = saved.proxy {
                ui.ListRow("Proxy", secondary: proxy, icon: .globe)
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.TextInput(bind: proxy_address, placeholder: "A proxy, as host:port", fill)
                ui.Button("Use it", size: .small) {
                    on press: device.set_network_policy(
                        device.network,
                        profile: saved.profile,
                        proxy_address: proxy_address.trim(),
                    )
                }
                ui.Button("No proxy", size: .small) {
                    on press: device.set_network_policy(
                        device.network,
                        profile: saved.profile,
                        proxy_address: "",
                    )
                }
            }
            ui.ListRow(
                "Its priority",
                secondary: "{saved.priority}; the higher one wins when several are in reach",
                icon: .bar_chart,
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        ui.TextInput(bind: priority, placeholder: "A number")
                        ui.Button("Save", size: .small) {
                            on press: device.set_network_policy(
                                device.network,
                                profile: saved.profile,
                                priority: priority.to_int() ?? saved.priority,
                            )
                        }
                    }
                }
            }
            ui.ListRow(
                "Counts as metered",
                secondary: metered_words(saved.metered),
                icon: .bar_chart,
            ) {
                trailing: {
                    row(gap: .tight, align: .center, wrap: true) {
                        ui.Button("Let it say", size: .small) {
                            on press: device.set_network_policy(
                                device.network,
                                profile: saved.profile,
                                metered: "automatic",
                            )
                        }
                        ui.Button("Metered", size: .small) {
                            on press: device.set_network_policy(
                                device.network,
                                profile: saved.profile,
                                metered: "metered",
                            )
                        }
                        ui.Button("Unmetered", size: .small) {
                            on press: device.set_network_policy(
                                device.network,
                                profile: saved.profile,
                                metered: "unmetered",
                            )
                        }
                    }
                }
            }
            if saved.kind == .wifi {
                ui.ListRow("The address it shows", secondary: mac_words(saved.mac), icon: .wifi) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            for choice in [
                                ["Permanent", "permanent"],
                                ["Random", "random"],
                                ["Stable", "stable"],
                            ] {
                                ui.Button(choice.at(0) ?? "", size: .small) {
                                    on press: device.set_network_policy(
                                        device.network,
                                        profile: saved.profile,
                                        mac_address: choice.at(1) ?? "stable",
                                    )
                                }
                            }
                        }
                    }
                }
                ui.Text(
                    "A random address keeps networks from following this device.",
                    role: .caption,
                    secondary: true,
                )
            }
            if let wake = saved.wake {
                ui.ListRow("Wake over this network", secondary: wake_words(wake), icon: .power) {
                    trailing: {
                        ui.Button(
                            if wake.on { "On" } else { "Off" },
                            toggled: wake.on,
                            enabled: wake_supported(wake),
                            description: "This network cannot wake this computer.",
                        ) {
                            on press: saved.set_wake(not wake.on)
                        }
                    }
                }
            }
            if saved.kind == .wifi {
                ui.ListRow(
                    "Says its name",
                    secondary: if saved.hidden {
                        "Hidden: joined by its name"
                    } else { "Broadcast" },
                    icon: if saved.hidden { .eye_off } else { .eye },
                ) {
                    trailing: {
                        ui.Button("Change", role: .plain, size: .small) {
                            on press: device.set_network_policy(
                                device.network,
                                profile: saved.profile,
                                hidden: not saved.hidden,
                            )
                        }
                    }
                }
                row(gap: .related, align: .center, wrap: true) {
                    ui.TextInput(
                        bind: password,
                        placeholder: "A new password, where it has one",
                        fill,
                    )
                    ui.Button("Change the password", size: .small) {
                        on press: device.set_wifi_password(
                            device.network,
                            profile: saved.profile,
                            password: password,
                        )
                    }
                }
                if not saved.open {
                    ui.ListRow(
                        "Share with my devices",
                        secondary: if shared {
                            "Shared with your other devices"
                        } else {
                            "Share a saved Wi-Fi network securely with your other devices"
                        },
                        icon: .share,
                    ) {
                        trailing: {
                            ui.Button("Share", role: .plain, size: .small) {
                                on press: saved.share_with_devices().then(set(shared, true))
                            }
                        }
                    }
                }
            }
            if let enterprise = saved.enterprise {
                work_identity(enterprise, saved.profile)
            }
            if let ipv4 = saved.ipv4 {
                ui.ListRow(
                    "Addresses",
                    secondary: if ipv4.automatic {
                        "The network hands one out"
                    } else { ipv4.addresses.join(", ") },
                    icon: .globe,
                )
                ui.TextInput(
                    bind: addresses,
                    placeholder: "Addresses with their prefixes, comma-separated",
                )
                ui.TextInput(bind: gateway, placeholder: "Router")
                ui.TextInput(bind: dns, placeholder: "Name servers, comma-separated")
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button("Take one from the network", size: .small) {
                        on press: device.set_network_ipv4(
                            device.network,
                            profile: saved.profile,
                            automatic: true,
                            addresses: comma_list(addresses),
                            gateway: gateway.trim(),
                            dns: comma_list(dns),
                        )
                    }
                    ui.Button("Use these addresses", size: .small) {
                        on press: device.set_network_ipv4(
                            device.network,
                            profile: saved.profile,
                            automatic: false,
                            addresses: comma_list(addresses),
                            gateway: gateway.trim(),
                            dns: comma_list(dns),
                        )
                    }
                }
            }
            if let ipv6 = saved.ipv6 {
                ui.ListRow(
                    "IPv6",
                    secondary: if ipv6.method == "manual" {
                        ipv6.addresses.join(", ")
                    } else { ipv6_method_words(ipv6.method) },
                    icon: .globe,
                )
                ui.ToggleGroup(
                    selected: ipv6_method_index(chosen_ipv6_method(ipv6_method, ipv6.method)),
                    label: "IPv6",
                ) {
                    ui.Segment("Automatic") {
                        on press: set(ipv6_method, "automatic")
                    }
                    ui.Segment("Manual") {
                        on press: set(ipv6_method, "manual")
                    }
                    ui.Segment("Link-local") {
                        on press: set(ipv6_method, "link-local")
                    }
                    ui.Segment("Off") {
                        on press: set(ipv6_method, "disabled")
                    }
                }
                ui.TextInput(
                    bind: ipv6_addresses,
                    placeholder: "IPv6 addresses with their prefixes, comma-separated",
                )
                ui.TextInput(bind: ipv6_gateway, placeholder: "IPv6 router")
                ui.TextInput(bind: ipv6_dns, placeholder: "IPv6 name servers, comma-separated")
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button("Apply IPv6 settings", size: .small) {
                        on press: device.set_network_ipv6(
                            device.network,
                            profile: saved.profile,
                            method: ipv6_method_key(chosen_ipv6_method(ipv6_method, ipv6.method)),
                            addresses: if comma_list(ipv6_addresses).is_empty() {
                                ipv6.addresses
                            } else { comma_list(ipv6_addresses) },
                            gateway: if ipv6_gateway.trim() == "" {
                                ipv6.gateway ?? ""
                            } else { ipv6_gateway.trim() },
                            dns: if comma_list(ipv6_dns).is_empty() {
                                ipv6.dns
                            } else { comma_list(ipv6_dns) },
                        )
                    }
                }
            }
            ui.Button("Forget this network", icon: .trash, role: .destructive) {
                on press: saved.forget()
            }
        }
    }
}

/// Who a work or school network knows this device as, and the certificate that verifies it. The
/// two are applied together, so changing one keeps the other as the device has it.
view work_identity(kept: device.SavedEnterprise, profile: Text) {
    state identity: Text = kept.identity
    state ca: Text = kept.ca_certificate
    ui.ListRow("Work or school identity", secondary: kept.identity, icon: .landmark)
    row(gap: .related, align: .center, wrap: true) {
        ui.TextInput(bind: identity, placeholder: "The identity the network knows", fill)
        ui.TextInput(bind: ca, placeholder: "The certificate that verifies it", fill)
        ui.Button("Apply identity", size: .small) {
            on press: device.set_enterprise_identity(
                device.network,
                profile: profile,
                identity: identity,
                ca_certificate: ca,
            )
        }
    }
    ui.Text(
        "The certificate is the full path of the organization's file; leave it empty for a network that is not checked.",
        role: .caption,
        secondary: true,
    )
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
                if let device_name = wired.device {
                    ui.Property("Interface", device_name)
                }
                if let speed = wired.speed {
                    ui.Property("Speed", speed)
                }
                if let address = wired.ipv4 {
                    ui.Property("IPv4", address)
                }
            }
            ui.Button(
                if wired.connected { "Disconnect" } else { "Connect" },
                icon: .ethernet_port,
            ) {
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
            if let named = t.name {
                ui.Property("Name", named)
            }
        }
        ui.ListRow(
            "Share this connection",
            secondary: if t.hotspot { "On" } else { "Off" },
            icon: .radio,
        ) {
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
                    on press: device.configure_hotspot(
                        device.tethering,
                        name,
                        password,
                        band_key(band),
                    )
                }
                ui.Button(
                    if show_code { "Hide the code" } else { "Share the code" },
                    icon: .qr_code,
                ) {
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
                    secondary: if let signal = c.signal {
                        "{signal} dBm"
                    } else { "Signal unknown" },
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
            ui.Dropdown(
                bind: inner,
                options: ["MSCHAPv2", "PAP", "CHAP"],
                placeholder: "Inner authentication",
            )
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

/// Whether a saved network counts its traffic against a plan.
fn metered_words(metered: device.Metered) -> Text {
    if metered == .metered {
        "Metered"
    } else if metered == .unmetered { "Unmetered" } else { "The network says" }
}

/// What the radios say of a connected network.
fn connectivity_words(c: device.Connectivity) -> Text {
    if c == .portal {
        "Sign in to the network"
    } else if c == .limited {
        "Connected, no internet"
    } else if c == .offline { "Offline" } else { "Connected" }
}

/// What mobile data says: the modem's network and how strong it is.
fn mobile_words(mobile: device.Mobile, modem: Option<device.ModemUnit>) -> Text {
    if not mobile.available {
        "No modem"
    } else if not mobile.on {
        "Off"
    } else if let m = modem {
        if let technology = m.technology { technology_words(technology) } else { "On" }
    } else { "On" }
}

/// What a modem's network is called.
fn technology_words(t: device.Technology) -> Text {
    if t == .nr5g {
        "5G"
    } else if t == .lte {
        "LTE"
    } else if t == .hspa-plus {
        "HSPA+"
    } else if t == .hspa {
        "HSPA"
    } else if t == .umts {
        "3G"
    } else if t == .ev-do {
        "EV-DO"
    } else if t == .cdma {
        "CDMA"
    } else if t == .edge {
        "EDGE"
    } else if t == .gprs { "GPRS" } else if t == .gsm { "2G" } else { "On" }
}

/// What a band is called in the dropdown.
fn band_label(band: device.Band) -> Text {
    if band == .two-point-four { "2.4 GHz" } else if band == .five { "5 GHz" } else { "Automatic" }
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

/// The entries `text` lists between commas, each trimmed, none empty.
fn comma_list(text: Text) -> List<Text> {
    text.split(",").map(x -> x.trim()).filter(x -> x != "")
}

/// Which hardware address a saved network shows, in words.
fn mac_words(mac: Option<Text>) -> Text {
    if let shown = mac {
        if shown == "permanent" {
            "Permanent hardware address"
        } else if shown == "random" { "Random each connection" } else { "Stable per network" }
    } else { "The device's own address" }
}

/// Whether a network wakes this device, and what the device says about it.
fn wake_words(wake: device.NetworkWake) -> Text {
    if not wake_supported(wake) {
        "This network cannot wake this computer"
    } else if let problem = wake.problem {
        problem
    } else if wake.on and applied_now(wake) {
        "Another device on this network can wake this computer from sleep"
    } else if wake.on {
        "Saved, and applied when the network connects"
    } else {
        "Another device on this network cannot wake this computer"
    }
}

/// Whether the interface can wake this device: what the driver says, or that it can where the
/// driver says nothing.
fn wake_supported(wake: device.NetworkWake) -> Bool {
    if let supported = wake.supported { supported } else { true }
}

/// Whether the setting is in force on the interface now.
fn applied_now(wake: device.NetworkWake) -> Bool {
    if let applied = wake.applied { applied } else { false }
}

/// The IPv6 method a person chose, or the one the network keeps where they chose none.
fn chosen_ipv6_method(chosen: Text, kept: Text) -> Text {
    if chosen == "" { kept } else { chosen }
}

/// The segment the IPv6 method picker shows.
fn ipv6_method_index(method: Text) -> Int {
    if method == "manual" {
        1
    } else if method == "link-local" { 2 } else if method == "disabled" { 3 } else { 0 }
}

/// An IPv6 method in words.
fn ipv6_method_words(method: Text) -> Text {
    if method == "manual" {
        "Manual"
    } else if method == "link-local" {
        "Link-local only"
    } else if method == "disabled" { "Off" } else { "Automatic" }
}

/// The IPv6 method as the action takes it.
fn ipv6_method_key(method: Text) -> Text {
    if method == "manual" {
        "manual"
    } else if method == "link-local" {
        "link-local"
    } else if method == "disabled" { "disabled" } else { "automatic" }
}
