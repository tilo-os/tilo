/// Mobile data: the modem's connection, what its SIM asks for, and the SIMs it holds. The
/// connection's own settings are the network resource's; a modem and its SIMs are the modem
/// resource's, and each press is that resource's action.
view mobile_page() {
    state pin: Text = ""
    state puk: Text = ""
    state old_pin: Text = ""
    state new_pin: Text = ""
    state apn_open: Bool = false
    state apn: Text = ""
    state network_type: Text = ""
    let net = device.network
    let mobile = net.mobile
    let modem = device.modem.modems.first()
    ui.Section("Mobile data") {
        if not mobile.available {
            ui.EmptyState("This device has no mobile modem", icon: .signal, inline: true)
        } else {
            ui.ListRow(
                if let m = modem { m.name } else { "Mobile data" },
                secondary: mobile_state(net, modem),
                icon: .signal,
                value: if let m = modem { m.operator ?? "" } else { "" },
            ) {
                trailing: {
                    ui.Button(if mobile.on { "On" } else { "Off" }, toggled: mobile.on) {
                        on press: device.set_mobile_data(device.network, not mobile.on)
                    }
                }
            }
            if let m = modem {
                if let signal = m.signal {
                    ui.PropertyList {
                        ui.Property("Signal", "{signal}%")
                        if let technology = m.technology {
                            ui.Property("Network", technology_words(technology))
                        }
                        ui.Property("Stands on", registration_words(m.registration))
                    }
                }
            }
            ui.ListRow("Refresh", secondary: "Read the modems again", icon: .refresh_ccw) {
                on activate: device.refresh_modems(device.modem)
            }
            if let problem = device.modem.problem {
                ui.StatusLine(problem, status: .error)
            }
        }
    }
    if mobile.available {
        if let m = modem {
            if m.lock != .none or m.failure != .none {
                ui.Section("The SIM") {
                    if m.lock == .pin {
                        ui.Text(pin_retries(m.retries.pin), role: .caption, secondary: true)
                        row(gap: .related, align: .center, wrap: true) {
                            ui.TextInput(bind: pin, placeholder: "PIN", fill)
                            ui.Button("Unlock", role: .primary) {
                                on press: device
                                    .unlock_sim(
                                        device.modem,
                                        sim: active_sim_path(m),
                                        pin: pin,
                                    )
                                    .then(set(pin, ""))
                            }
                        }
                    } else if m.lock == .puk {
                        ui.Text(
                            "The SIM is blocked: its PUK and a new PIN lift it.",
                            role: .caption,
                            secondary: true,
                        )
                        row(gap: .related, align: .center, wrap: true) {
                            ui.TextInput(bind: puk, placeholder: "PUK", fill)
                            ui.TextInput(bind: new_pin, placeholder: "New PIN", fill)
                            ui.Button("Unblock", role: .primary) {
                                on press: device
                                    .unblock_sim(
                                        device.modem,
                                        sim: active_sim_path(m),
                                        puk: puk,
                                        pin: new_pin,
                                    )
                                    .then(set(puk, ""))
                                    .then(set(new_pin, ""))
                            }
                        }
                    } else if m.lock == .other {
                        ui.Text(
                            "The operator locked this SIM; nothing here lifts it.",
                            role: .caption,
                            secondary: true,
                        )
                    }
                }
            } else {
                ui.Section("The SIM") {
                    if let sim = m.sims.find(s -> s.active) {
                        sim_properties(sim)
                    } else if let sim = m.sims.first() {
                        sim_properties(sim)
                    }
                    if not m.numbers.is_empty() {
                        ui.PropertyList {
                            ui.Property("Number", m.numbers.join(", "))
                        }
                    }
                    if m.sims.len() > 1 {
                        for sim in m.sims {
                            ui.ListRow(
                                sim.label,
                                secondary: sim_kind_words(sim.kind),
                                selected: sim.active,
                                icon: .sd_card,
                            ) {
                                on activate: device.use_sim(device.modem, sim: sim.path)
                                trailing: {
                                    if not sim.active {
                                        ui.Button("Use it", size: .small) {
                                            on press: device.use_sim(device.modem, sim: sim.path)
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let enabled = m.pin_enabled {
                        ui.ListRow(
                            "Ask for the PIN at startup",
                            secondary: if enabled { "On" } else { "Off" },
                            icon: .lock,
                        ) {
                            trailing: {
                                row(gap: .tight, align: .center) {
                                    ui.Button("Change", size: .small) {
                                        on press: device.set_sim_pin_lock(
                                            device.modem,
                                            sim: active_sim_path(m),
                                            on: not enabled,
                                            pin: pin,
                                        )
                                    }
                                }
                            }
                        }
                    }
                    ui.TextInput(bind: pin, placeholder: "The SIM's PIN")
                    row(gap: .related, align: .center, wrap: true) {
                        ui.TextInput(bind: old_pin, placeholder: "Current PIN", fill)
                        ui.TextInput(bind: new_pin, placeholder: "New PIN", fill)
                        ui.Button("Change the PIN", size: .small) {
                            on press: device
                                .change_sim_pin(
                                    device.modem,
                                    sim: active_sim_path(m),
                                    old_pin: old_pin,
                                    new_pin: new_pin,
                                )
                                .then(set(old_pin, ""))
                                .then(set(new_pin, ""))
                        }
                    }
                }
            }
        }
        ui.Section("The connection") {
            if let profile = mobile.profile {
                ui.ListRow(
                    "Use other operators",
                    secondary: if profile.roaming { "On" } else { "Only this operator" },
                    icon: .globe,
                ) {
                    trailing: {
                        ui.Button(
                            if profile.roaming { "On" } else { "Off" },
                            toggled: profile.roaming,
                        ) {
                            on press: device.set_mobile_roaming(
                                device.network,
                                on: not profile.roaming,
                            )
                        }
                    }
                }
                ui.Text("Counts as metered", role: .caption, secondary: true)
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button("Let the network say", size: .small) {
                        on press: device.set_mobile_metered(device.network, metered: "automatic")
                    }
                    ui.Button("Metered", size: .small) {
                        on press: device.set_mobile_metered(device.network, metered: "metered")
                    }
                    ui.Button("Unmetered", size: .small) {
                        on press: device.set_mobile_metered(device.network, metered: "unmetered")
                    }
                }
                ui.ListRow(
                    "Access point",
                    secondary: if let apn = profile.apn { apn } else { "From the operator" },
                    icon: .globe,
                ) {
                    on activate: set(apn_open, not apn_open)
                }
                if apn_open {
                    row(gap: .related, align: .center, wrap: true) {
                        ui.TextInput(
                            bind: apn,
                            placeholder: "The access point name, such as internet.telekom",
                            fill,
                        )
                        ui.Button("Save", role: .primary, size: .small) {
                            on press: device
                                .set_mobile_apn(
                                    device.network,
                                    apn: apn,
                                )
                                .then(set(apn_open, false))
                        }
                        ui.Button("From the operator", size: .small) {
                            on press: device.set_mobile_apn(device.network)
                        }
                    }
                }
            } else {
                ui.Button("Make the mobile profile", icon: .plus) {
                    on press: device.set_mobile_apn(device.network)
                }
            }
            if let m = modem {
                if m.network_types.len() > 1 {
                    ui.Text("Network types", role: .caption, secondary: true)
                    ui.Dropdown(
                        bind: network_type,
                        options: m.network_types,
                        placeholder: m.network_type ?? "Choose",
                    )
                    ui.Button("Use these", size: .small) {
                        on press: device.set_preferred_network(device.modem, network: network_type)
                    }
                }
            }
        }
    }
}

/// What the modem and the connection say in one line.
fn mobile_state(net: device.Network, modem: Option<device.ModemUnit>) -> Text {
    if not net.mobile.on {
        "Off"
    } else if let m = modem {
        if m.state == .connected {
            "Connected"
        } else if m.state == .registered {
            "Registered"
        } else if m.lock != .none { "The SIM is locked" } else { "On" }
    } else { "On" }
}

/// What one SIM is, in a card of its own.
view sim_properties(sim: device.ModemSim) {
    ui.PropertyList {
        ui.Property("SIM", sim.label)
        ui.Property("Kind", sim_kind_words(sim.kind))
        if let slot = sim.slot {
            ui.Property("Slot", "{slot}")
        }
    }
}

/// The path of the SIM mobile data uses: the active one, else the first.
fn active_sim_path(m: device.ModemUnit) -> Text {
    if let sim = m.sims.find(s -> s.active) {
        sim.path
    } else if let first = m.sims.first() { first.path } else { "" }
}

/// What a locked SIM says: how many tries it has left.
fn pin_retries(tries: Option<Int>) -> Text {
    if let left = tries {
        "The SIM asks for its PIN: {left} tries left"
    } else { "The SIM asks for its PIN" }
}

/// What a SIM is.
fn sim_kind_words(kind: device.SimKind) -> Text {
    if kind == .esim {
        "eSIM"
    } else if kind == .esim-without-profile {
        "eSIM with no profile"
    } else if kind == .physical { "SIM card" } else { "SIM" }
}

/// Which network the modem stands on.
fn registration_words(r: device.Registration) -> Text {
    if r == .home {
        "This operator's network"
    } else if r == .roaming {
        "Another operator's network"
    } else if r == .searching {
        "Looking for a network"
    } else if r == .denied {
        "Refused by the network"
    } else if r == .emergency-only {
        "Emergency calls only"
    } else if r == .idle { "Not looking" } else { "Unknown" }
}
