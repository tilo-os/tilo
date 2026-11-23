use so.tilo.device as device
use so.tilo.settings as settings

/// The quick settings: the switches and sliders a person reaches for most, then the sound, the
/// networks in reach, the Bluetooth devices and the ways to leave the session. Each reads the
/// resource the session publishes (the sound, the network, Bluetooth, the VPN, the night light, the
/// power, the person's appearance settings) and each press is that resource's own published action,
/// so the settings app, the keys and the agent change the same thing this panel shows, and nothing
/// here holds a copy of it.
///
/// It is a floating panel at the shell's trailing edge, over what it covers, beside the
/// notification centre and on the same material; the bar's status opens it.
view quick_settings() {
    row(fill, distribute: .end, padding: .related) {
        ui.FloatingPanel(
            width: if not space.compact { 360.0 },
            fill: if space.compact { 1.0 } else { 0.0 },
        ) {
            header: {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text("Quick settings", role: .heading)
                    spacer
                    battery_line()
                }
            }
            body: {
                scroll {
                    stack(gap: .related) {
                        grid(min: 10rem, gap: .related) {
                            wifi_tile()
                            bluetooth_tile()
                            vpn_tile()
                            dark_tile()
                            quiet_tile()
                            night_light_tile()
                            airplane_tile()
                            mobile_tile()
                            power_mode_tile()
                        }
                        levels()
                        // What runs now — the song playing, the call, the timer — stands here: its
                        // resource's own `live` view, which the shell composes from the index.
                        sound_section()
                        network_section()
                        bluetooth_section()
                        power_row()
                    }
                }
            }
        }
    }
}

/// Wi-Fi, on while it is on, naming the network it is joined to; it cannot be pressed on a
/// device without it.
view wifi_tile() {
    let wifi = device.network.wifi
    let joined = wifi.networks.find(n -> n.connected)
    let label = if let n = joined { n.name } else { "Wi-Fi" }
    ui.Button(
        label,
        icon: .wifi,
        toggled: wifi.on,
        enabled: wifi.available,
        description: "This device has no Wi-Fi",
    ) {
        on press: device.set_wifi(device.network, not wifi.on)
    }
}

/// Bluetooth, on while it is on; it cannot be pressed on a device without it.
view bluetooth_tile() {
    let b = device.bluetooth
    ui.Button(
        "Bluetooth",
        icon: .bluetooth,
        toggled: b.on,
        enabled: b.available,
        description: "This device has no Bluetooth",
    ) {
        on press: device.set_bluetooth(b, not b.on)
    }
}

/// The VPN, on while one of the person's profiles is connected and named for it; the device's
/// first profile goes on when none is, and the connected one goes off. Nothing on a device with no
/// profile.
view vpn_tile() {
    let vpn = device.vpn
    if not vpn.profiles.is_empty() {
        let on = vpn.profiles.any(p -> p.connected)
        let joined = vpn.profiles.find(p -> p.connected)
        ui.Button(if let p = joined { p.name } else { "VPN" }, icon: .shield, toggled: on) {
            on press: toggle_vpn(device.vpn)
        }
    }
}

/// Turn the VPN on through its first profile, or off through the one connected: the tile's one
/// press, whichever way it flips.
action toggle_vpn(v: device.Vpn) {
    if let connected = v.profiles.find(p -> p.connected) {
        device.set_vpn(v, profile: connected.id, on: false)
    } else if let first = v.profiles.first() {
        device.set_vpn(v, profile: first.id, on: true)
    }
}

/// Dark mode: on while every app draws dark, each with the theme the person chose for it.
view dark_tile() {
    let a = settings.appearance
    ui.Button("Dark mode", icon: .moon, toggled: a.mode == "dark") {
        on press: settings.set_dark_mode(a, a.mode != "dark")
    }
}

/// Do not disturb: on while banners and sounds wait.
view quiet_tile() {
    let n = settings.notifications
    ui.Button("Do not disturb", icon: .bell_off, toggled: n.do_not_disturb) {
        on press: settings.set_do_not_disturb(n, not n.do_not_disturb)
    }
}

/// Night light: on while the screen is warm.
view night_light_tile() {
    let nl = device.night_light
    ui.Button("Night light", icon: .moon, toggled: nl.on) {
        on press: device.toggle(device.night_light, on: not nl.on)
    }
}

/// Airplane mode: on while every wireless radio is off.
view airplane_tile() {
    let net = device.network
    ui.Button("Airplane mode", icon: .plane, toggled: net.airplane_mode) {
        on press: device.set_airplane_mode(device.network, not net.airplane_mode)
    }
}

/// Mobile data: on while the modem's data is, and nothing on a device without one.
view mobile_tile() {
    let mobile = device.network.mobile
    if mobile.available {
        ui.Button("Mobile data", icon: .globe, toggled: mobile.on) {
            on press: device.set_mobile_data(device.network, not mobile.on)
        }
    }
}

/// The power mode, its own menu choosing between the modes the machine offers; nothing on a
/// machine that offers none.
view power_mode_tile() {
    let p = device.power
    if not p.modes.is_empty() {
        let current = p.mode ?? .balanced
        ui.MenuButton(power_mode_label(current), icon: .gauge) {
            ui.MenuItem("Power saver", checked: current == .saver) {
                on choose: device.set_power_mode(device.power, mode: "saver")
            }
            ui.MenuItem("Balanced", checked: current == .balanced) {
                on choose: device.set_power_mode(device.power, mode: "balanced")
            }
            ui.MenuItem("Performance", checked: current == .performance) {
                on choose: device.set_power_mode(device.power, mode: "performance")
            }
        }
    }
}

/// What a power mode is called.
fn power_mode_label(mode: device.PowerMode) -> Text {
    if mode == .saver {
        "Power saver"
    } else if mode == .performance { "Performance" } else { "Balanced" }
}

/// The brightness and the volume, each where the device has it: dragging sets it as the device
/// reads it back, and the volume's mute button silences it in one press.
view levels() {
    // One slider per display that can be dimmed, the focused display first: each sets that
    // display's own brightness.
    for display in device.displays.displays {
        if let level = display.brightness {
            row(gap: .related, align: .center) {
                ui.Icon(.sun)
                ui.Slider(value: level, label: display.name, fill) {
                    on change(v): device.set_display_brightness(
                        device.displays,
                        display.connector,
                        v,
                    )
                }
            }
        }
    }
    let sound = device.sound
    if let volume = sound.volume {
        row(gap: .related, align: .center) {
            ui.Button(
                if sound.muted { "Unmute" } else { "Mute" },
                icon: if sound.muted { .volume_x } else { .volume_2 },
                icon_only: true,
                toggled: sound.muted,
                role: .plain,
            ) {
                on press: device.set_muted(device.sound, not sound.muted)
            }
            ui.Slider(value: volume, label: "Volume", fill) {
                on change(v): device.set_volume(device.sound, v)
            }
        }
    }
}

/// Where the sound goes and where it is recorded from: the microphone's level and mute, the
/// outputs to play through, and the inputs to record from, each chosen by a press.
view sound_section() {
    let sound = device.sound
    if sound.volume != .none or not sound.outputs.is_empty() or not sound.inputs.is_empty() {
        ui.Section("Sound") {
            if let level = sound.input_volume {
                row(gap: .related, align: .center) {
                    ui.Button(
                        if sound.input_muted {
                            "Unmute the microphone"
                        } else { "Mute the microphone" },
                        icon: if sound.input_muted { .mic_off } else { .mic },
                        icon_only: true,
                        toggled: sound.input_muted,
                        role: .plain,
                    ) {
                        on press: device.toggle_input_muted(device.sound)
                    }
                    ui.Slider(value: level, label: "Microphone", fill) {
                        on change(v): device.set_input_volume(device.sound, v)
                    }
                }
            }
            for output in sound.outputs {
                ui.ListRow(
                    output.label,
                    secondary: if output.chosen { "Playing here" } else { "" },
                    selected: output.chosen,
                    icon: .speaker,
                ) {
                    on activate: device.route_output(device.sound, node: output.node)
                }
            }
            for input in sound.inputs {
                ui.ListRow(
                    input.label,
                    secondary: if input.chosen { "Recording here" } else { "Microphone" },
                    selected: input.chosen,
                    icon: .mic,
                ) {
                    on activate: device.select_input(device.sound, node: input.node)
                }
            }
        }
    }
}

/// The Wi-Fi networks in reach, the joined one first, each pressed to join it; and the saved
/// networks this device knows but cannot see. Nothing on a device without Wi-Fi.
view network_section() {
    state password_for: Text = ""
    state password: Text = ""
    state hidden: Bool = false
    state hidden_name: Text = ""
    state hidden_password: Text = ""
    let wifi = device.network.wifi
    if wifi.available {
        ui.Section("Wi-Fi") {
            if let wired = device.network.wired {
                ui.ListRow(
                    "Wired network",
                    secondary: if wired.connected { "Connected" } else { "Cable unplugged" },
                    selected: wired.connected,
                    icon: .ethernet_port,
                ) {
                    on activate: device.set_wired(device.network, not wired.connected)
                }
            }
            if not wifi.on {
                ui.Text("Wi-Fi is off", role: .caption, secondary: true)
            } else if wifi.networks.is_empty() {
                ui.EmptyState("Scanning for networks", icon: .wifi, inline: true)
            }
            for n in wifi.networks {
                ui.ListRow(
                    n.name,
                    secondary: if n.connected {
                        "Connected"
                    } else if n.known { "Known" } else if n.secure { "Secured" } else { "Open" },
                    selected: n.connected,
                    icon: if n.secure { .lock } else { .wifi },
                ) {
                    on activate: if n.secure and not n.known {
                        set(password_for, n.name)
                    } else { device.join_wifi(device.network, n.name) }
                }
            }
            if password_for != "" {
                ui.Text("Enter the password for {password_for}", role: .caption, secondary: true)
                row(gap: .related, align: .center, wrap: true) {
                    ui.TextInput(bind: password, placeholder: "Password", fill)
                    ui.Button("Join", role: .primary, size: .small) {
                        on press: device.connect_wifi(device.network, password_for, password)
                    }
                    ui.Button("Cancel", size: .small, role: .plain) {
                        on press: set(password_for, "")
                    }
                }
            }
            row(gap: .related, align: .center) {
                ui.Button("Hidden network", icon: .wifi, role: .plain, toggled: hidden) {
                    on press: set(hidden, not hidden)
                }
            }
            if hidden {
                ui.Text(
                    "Join a network that does not broadcast its name",
                    role: .caption,
                    secondary: true,
                )
                row(gap: .related, align: .center, wrap: true) {
                    ui.TextInput(bind: hidden_name, placeholder: "Network name", fill)
                    ui.TextInput(bind: hidden_password, placeholder: "Password", fill)
                    ui.Button("Join", role: .primary, size: .small) {
                        on press: device.connect_hidden_wifi(
                            device.network,
                            hidden_name,
                            hidden_password,
                        )
                    }
                    ui.Button("Cancel", size: .small, role: .plain) {
                        on press: set(hidden, false)
                    }
                }
            }
            let saved = device.network.saved.filter(s -> s.kind == .wifi)
            if not saved.is_empty() {
                ui.Text("Saved networks", role: .caption, secondary: true)
                for s in saved {
                    ui.ListRow(
                        s.name,
                        secondary: if s.autoconnect { "Joins by itself" } else { "Saved" },
                        icon: .wifi,
                    ) {
                        on activate: device.join_wifi(device.network, s.name)
                        trailing: {
                            ui.MenuButton(
                                "This network",
                                icon: .more_horizontal,
                                icon_only: true,
                                role: .plain,
                            ) {
                                ui.MenuItem(
                                    if s.autoconnect {
                                        "Stop joining by itself"
                                    } else { "Join by itself" },
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
    }
}

/// The paired accessories, connected first, each pressed to connect or disconnect it and offered
/// to forget; then the devices in reach, pressed to pair; and the way to look again. Nothing on a
/// device without Bluetooth.
view bluetooth_section() {
    let b = device.bluetooth
    if b.available {
        ui.Section("Bluetooth") {
            if not b.on {
                ui.Text("Bluetooth is off", role: .caption, secondary: true)
            }
            for accessory in b.accessories {
                ui.ListRow(
                    accessory.name,
                    secondary: if accessory.connected { "Connected" } else { "Paired" },
                    selected: accessory.connected,
                    icon: .bluetooth,
                    value: if let level = accessory.battery { "{level}%" } else { "" },
                ) {
                    on activate: if accessory.connected {
                        device.disconnect(device.bluetooth, address: accessory.address)
                    } else {
                        device.connect(device.bluetooth, address: accessory.address)
                    }
                    trailing: {
                        ui.Button(
                            "Forget this device",
                            icon: .trash,
                            icon_only: true,
                            size: .small,
                            role: .plain,
                        ) {
                            on press: device.forget(device.bluetooth, address: accessory.address)
                        }
                    }
                }
            }
            row(gap: .related, align: .center) {
                ui.Button(
                    if b.discovering { "Stop looking" } else { "Look for devices" },
                    icon: .search,
                    role: .plain,
                ) {
                    on press: device.scan(device.bluetooth, on: not b.discovering)
                }
            }
            for nearby in b.nearby {
                ui.ListRow(nearby.name, secondary: "In reach", icon: .bluetooth) {
                    on activate: device.pair(device.bluetooth, address: nearby.address)
                }
            }
            if b.on and b.accessories.is_empty() and b.nearby.is_empty() {
                ui.EmptyState("No devices found", icon: .bluetooth, inline: true)
            }
        }
    }
}

/// The battery's charge and whether it is charging, on a device that has one.
view battery_line() {
    let b = device.battery
    if b.present {
        row(gap: .tight, align: .center) {
            ui.Icon(.battery)
            ui.Text(battery_words(b.percentage, b.on_battery), role: .caption, secondary: true)
        }
    }
}

/// What the battery line says: the charge, and that it charges when it is plugged in.
fn battery_words(percent: Int, on_battery: Bool) -> Text {
    if on_battery { "{percent}%" } else { "{percent}%, charging" }
}

/// Locking the screen, and the ways to leave the session or the device.
view power_row() {
    let p = device.power
    row(gap: .related, align: .center, wrap: true) {
        ui.Button("Lock", icon: .lock) {
            on press: device.lock(p)
        }
        spacer
        ui.MenuButton("Power", icon: .power) {
            ui.MenuItem("Sleep") {
                on choose: device.suspend(p)
            }
            ui.MenuItem("Log out") {
                on choose: device.log_out(p)
            }
            ui.MenuItem("Restart") {
                on choose: device.restart(p)
            }
            ui.MenuItem("Shut down") {
                on choose: device.power_off(p)
            }
        }
    }
}
