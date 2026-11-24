use so.tilo.device as device
use so.tilo.settings.sections as settings

/// The quick settings: the switches and sliders a person reaches for most, what is live now, then
/// the sound, the networks in reach, the Bluetooth devices and the ways to leave the session. Each reads the
/// resource the session publishes (the sound, the network, Bluetooth, the VPN, the night light, the
/// power, the person's appearance settings) and each press is that resource's own published action,
/// so the settings app, the keys and the agent change the same thing this panel shows, and nothing
/// here holds a copy of it.
///
/// It is a floating panel at the shell's trailing edge, over what it covers, beside the
/// notification centre and on the same material; the bar's status opens it.
view quick_settings() {
    row(fill, distribute: .end, padding: .related) {
        quick_settings_panel(fill_height: false)
    }
}

/// The panel itself, where the session places it: hanging under the bar's trailing end by
/// itself, or beside the caret that asked for it (`device.overlays.place`). A panel measured a room
/// takes that height and scrolls inside it.
view quick_settings_panel(fill_height: Bool) {
    ui.FloatingPanel(
        width: if not space.compact { 360.0 },
        fill: space.compact,
        fill_height: fill_height,
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
                    // What was copied recently, above everything else: choosing one pastes it
                    // where the person types, which is why the key that opens this panel opens
                    // it.
                    clipboard_section()
                    // The tiles the person keeps on the panel, in their order: the same
                    // setting the editor below changes.
                    grid(min: 10rem, gap: .related) {
                        for c in device.quick_settings.controls {
                            control_tile(c)
                        }
                        // The appearance the whole desktop takes is not one of the
                        // person's panel controls: it stays on the grid whatever they keep.
                        dark_tile()
                    }
                    levels()
                    // What runs now — the song playing, the call, the timer — stands here: its
                    // resource's own `live` view, which the shell composes from the index.
                    live_section()
                    sound_section()
                    network_section()
                    bluetooth_section()
                    devices_section()
                    power_row()
                    controls_editor()
                }
            }
        }
    }
}

/// One of the panel's tiles, by the name the person's setting keeps it under: the panel draws the
/// controls it lists and nothing else, so a tile taken off is really gone.
view control_tile(control: Text) {
    if control == "wifi" {
        wifi_tile()
    } else if control == "bluetooth" {
        bluetooth_tile()
    } else if control == "vpn" {
        vpn_tile()
    } else if control == "night_light" {
        night_light_tile()
    } else if control == "airplane_mode" {
        airplane_tile()
    } else if control == "mobile_data" {
        mobile_tile()
    } else if control == "do_not_disturb" {
        quiet_tile()
    } else if control == "rotation_lock" {
        rotation_tile()
    } else if control == "power_mode" {
        power_mode_tile()
    }
}

/// Rotation lock: on while the screen stays put when the device turns.
view rotation_tile() {
    let d = settings.display
    ui.Button("Rotation lock", icon: .rotate_cw, toggled: d.rotation_lock) {
        on press: settings.set_rotation_lock(settings.display, locked: not d.rotation_lock)
    }
}

/// Wi-Fi, on while it is on, naming the network it is joined to; it cannot be pressed on a
/// device without it, or while a switch on the device holds the radio off.
view wifi_tile() {
    let wifi = device.network.wifi
    let joined = wifi.networks.find(n -> n.connected)
    let label = if let n = joined { n.name } else { "Wi-Fi" }
    ui.Button(
        label,
        icon: .wifi,
        toggled: wifi.on,
        enabled: wifi.available and not wifi.hardware_blocked,
        description: if not wifi.available {
            "This device has no Wi-Fi"
        } else {
            "A switch on this device holds Wi-Fi off"
        },
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
        connected.disconnect()
    } else if let first = v.profiles.first() { first.connect() }
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

/// The clipboard's recent copies, newest first: choosing one puts it back on the clipboard and
/// pastes it where the person types, which is the same verb the panel's own list and an agent
/// call. A copy is kept or dropped from its row, and the rest clear at once, pinned copies staying.
/// A person with many copies narrows them by typing, as they would in any list of the panel's, so
/// finding the line copied an hour ago does not mean scrolling the whole history.
view clipboard_section() {
    state query: Text = ""
    let clip = device.clipboard
    if not clip.copies.is_empty() {
        let needle = query.trim().lower()
        let shown = if needle == "" {
            clip.copies
        } else {
            clip.copies.filter(
                c -> copy_label(c).lower().contains(needle)
                    or copy_from(c).lower().contains(needle),
            )
        }
        ui.Section("Clipboard") {
            ui.TextInput(bind: query, placeholder: "Search copies")
            if query != "" {
                ui.Text(
                    if shown.len() == 1 {
                        "1 of {clip.copies.len()} copies"
                    } else {
                        "{shown.len()} of {clip.copies.len()} copies"
                    },
                    role: .caption,
                    secondary: true,
                )
            }
            for copy in shown {
                ui.ListRow(copy_label(copy), secondary: copy_from(copy), icon: .clipboard) {
                    on activate: device.paste(device.clipboard, copy: copy.key)
                    trailing: {
                        row(gap: .related, align: .center) {
                            ui.Button(
                                if copy.pinned {
                                    "Stop keeping {copy_label(copy)}"
                                } else { "Keep {copy_label(copy)}" },
                                icon: .pin,
                                icon_only: true,
                                size: .small,
                                toggled: copy.pinned,
                                role: .plain,
                            ) {
                                on press: device.pin(
                                    device.clipboard,
                                    copy: copy.key,
                                    pinned: not copy.pinned,
                                )
                            }
                            ui.Button(
                                "Remove {copy_label(copy)}",
                                icon: .x,
                                icon_only: true,
                                size: .small,
                                role: .plain,
                            ) {
                                on press: device.remove(device.clipboard, copy: copy.key)
                            }
                        }
                    }
                }
            }
            if shown.is_empty() {
                ui.EmptyState("No copy matches", icon: .search, inline: true)
            }
            row(fill, distribute: .end) {
                ui.Button("Clear the rest", size: .small, role: .plain) {
                    on press: device.clear(device.clipboard)
                }
            }
        }
    }
}

/// What a copy is called on its row: its first line of words, or what it is when it holds no text.
fn copy_label(copy: device.ClipboardCopy) -> Text {
    if let text = copy.text {
        return text
    }
    if copy.mime == "image/png" or copy.mime == "image/jpeg" or copy.mime == "image/webp" {
        return "A picture"
    }
    return "A copy"
}

/// Where a copy came from: another of the person's devices, or nothing to say.
fn copy_from(copy: device.ClipboardCopy) -> Text {
    if let from = copy.from {
        return "From {from}"
    }
    return ""
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
            // Each app playing now: its own level and mute, and where it plays, so one app can
            // go to a speaker across the room while the rest stay here.
            for stream in sound.streams {
                ui.ListRow(
                    stream.application,
                    secondary: if let media = stream.media {
                        media
                    } else if stream.playback { "Playing" } else { "Paused" },
                    icon: if stream.muted { .volume_x } else { .volume_2 },
                ) {
                    trailing: {
                        row(gap: .related, align: .center) {
                            ui.Button(
                                if stream.muted {
                                    "Unmute {stream.application}"
                                } else { "Mute {stream.application}" },
                                icon: if stream.muted { .volume_x } else { .volume_2 },
                                icon_only: true,
                                size: .small,
                                toggled: stream.muted,
                                role: .plain,
                            ) {
                                on press: device.set_stream_muted(
                                    device.sound,
                                    stream: stream.node,
                                    muted: not stream.muted,
                                )
                            }
                            ui.MenuButton("Play {stream.application} through", icon: .speaker) {
                                for output in sound.outputs {
                                    ui.MenuItem(
                                        output.label,
                                        checked: if let target = stream.target {
                                            target == output.node
                                        } else { false },
                                    ) {
                                        on choose: device.route_stream(
                                            device.sound,
                                            stream: stream.node,
                                            device: output.node,
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
                if let level = stream.volume {
                    ui.Slider(value: level, label: stream.application, fill) {
                        on change(v): device.set_stream_volume(
                            device.sound,
                            stream: stream.node,
                            level: v,
                        )
                    }
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
            if wifi.hardware_blocked {
                // A physical switch holds the radio off: say so where the person looks for the
                // networks, rather than showing an empty list that reads as "none in reach".
                ui.StatusLine("A switch on this device holds Wi-Fi off", status: .warning)
            } else if not wifi.on {
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
                    } else { n.join() }
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
            if not wifi.hardware_blocked {
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
                        on activate: s.join()
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
                                    on choose: s.set_autoconnect(not s.autoconnect)
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
                        accessory.disconnect()
                    } else { accessory.connect() }
                    trailing: {
                        row(gap: .related, align: .center) {
                            if accessory.connected {
                                bluetooth_hand_off(accessory.address)
                            }
                            ui.Button(
                                "Forget this device",
                                icon: .trash,
                                icon_only: true,
                                size: .small,
                                role: .plain,
                            ) {
                                on press: accessory.forget()
                            }
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
                    on activate: nearby.pair()
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

/// Locking the screen, and the ways to leave the session or the device. What leaves the session
/// is the power menu's, opened here at its button: the session says what it offers and asks about
/// a destructive row where the person presses it.
view power_row() {
    let m = device.power_menu
    row(gap: .related, align: .center, wrap: true) {
        if m.offers.contains("lock") {
            ui.Button("Lock", icon: .lock) {
                on press: device.power_lock(m)
            }
        }
        ui.Button("Settings", icon: .settings) {
            on press: session.open_settings(link: "settings://")
        }
        spacer
        ui.Button("Power", icon: .power) {
            on press: device.open(m)
        }
    }
}

/// The Devices section: the person's other devices, what this one can hand them through Nearby
/// Send, the offers that wait for an answer, and what became of each transfer. Every row is the
/// session's published resource, read here and changed by its own actions, so a press here, a
/// press in Files and an agent's call change the same thing.
///
/// The remote audio service's rows come first: devices asking to play this device's sound (answered
/// here, in place), the sounds on their way between devices (stopped, or dismissed once stopped),
/// and the sounds this device can send (chosen here, then sent by pressing a device). A device this
/// machine can drive offers its pointer row beside it.
view devices_section() {
    let mine = device.devices
    let send = device.nearby_send
    if not mine.devices.is_empty()
        or not mine.sounds.is_empty()
        or not mine.routes.is_empty()
        or not mine.asks.is_empty()
        or not send.devices.is_empty()
        or not send.transfers.is_empty() {
        ui.Section("Devices") {
            for ask in mine.asks {
                ui.ListRow(
                    "Play {ask.media} here?",
                    secondary: "{ask.source} asks",
                    icon: .volume_2,
                ) {
                    trailing: {
                        row(gap: .related, align: .center) {
                            ui.Button("Allow", icon: .check, size: .small, role: .primary) {
                                on press: device.press(
                                    device.devices,
                                    row: ask_row(ask.id),
                                    part: .allow,
                                )
                            }
                            ui.Button(
                                "Decline",
                                icon: .x,
                                icon_only: true,
                                size: .small,
                                role: .plain,
                            ) {
                                on press: device.press(
                                    device.devices,
                                    row: ask_row(ask.id),
                                    part: .decline,
                                )
                            }
                        }
                    }
                }
            }
            for route in mine.routes {
                ui.ListRow(route.label, secondary: route.detail, icon: .send) {
                    trailing: {
                        ui.Button(
                            if route.stopped { "Dismiss" } else { "Stop" },
                            size: .small,
                            role: .plain,
                        ) {
                            on press: device.press(
                                device.devices,
                                row: route_row(route.id),
                                part: .action,
                            )
                        }
                    }
                }
            }
            if not mine.sounds.is_empty() {
                ui.Text("Sounds you can send", role: .caption, secondary: true)
                for s in mine.sounds {
                    ui.ListRow(
                        s.label,
                        secondary: if s.application {
                            "One app's sound"
                        } else { "Everything this device plays" },
                        selected: chosen_sound(mine.chosen, s.token),
                        icon: .volume_2,
                    ) {
                        on activate: device.press(
                            device.devices,
                            row: sound_row(s.token),
                            part: .row,
                        )
                    }
                }
            }
            // The person's own devices, this machine left out: pressing one wakes it, sends it
            // what plays here, or opens where it is arranged.
            for d in mine.devices {
                if not d.local {
                    ui.ListRow(
                        d.label,
                        secondary: device_line(d, mine.chosen),
                        selected: d.online,
                        icon: .laptop,
                    ) {
                        on activate: device.press(device.devices, row: d.id, part: .row)
                        trailing: {
                            row(gap: .related, align: .center) {
                                if d.online {
                                    ui.Button(
                                        "Drive {d.label} with this pointer",
                                        icon: .mouse,
                                        icon_only: true,
                                        size: .small,
                                        role: .plain,
                                    ) {
                                        on press: device.press(
                                            device.devices,
                                            row: d.id,
                                            part: .control,
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if not send.devices.is_empty() {
                ui.Text("Nearby Send", role: .caption, secondary: true)
                for d in send.devices {
                    ui.ListRow(
                        d.name,
                        secondary: nearby_place(d.kind == .lab_receiver, d.reachable),
                        selected: d.reachable,
                        icon: .laptop,
                    )
                }
            }
            for t in send.transfers {
                if t.incoming and t.state == .offered {
                    ui.ListRow(
                        t.name,
                        secondary: if let d = send.destinations.first() {
                            "{t.peer} offers this; keep it in {d.name}"
                        } else { "{t.peer} offers this" },
                        icon: .download,
                    ) {
                        trailing: {
                            row(gap: .related, align: .center) {
                                ui.Button(
                                    "Keep it",
                                    size: .small,
                                    role: .primary,
                                    enabled: not send.destinations.is_empty(),
                                    description: "No folder here takes the file",
                                ) {
                                    on press: device.accept(device.nearby_send, transfer: t.handle)
                                }
                                ui.Button(
                                    "Turn down",
                                    icon: .x,
                                    icon_only: true,
                                    size: .small,
                                    role: .plain,
                                ) {
                                    on press: device.decline(device.nearby_send, transfer: t.handle)
                                }
                            }
                        }
                    }
                } else if t.state == .sending {
                    ui.ListRow(
                        t.name,
                        secondary: transfer_moving(t.peer, t.progress, t.incoming),
                        icon: if t.incoming { .download } else { .upload },
                    ) {
                        trailing: {
                            ui.Button("Stop", size: .small, role: .plain) {
                                on press: device.cancel(device.nearby_send, transfer: t.handle)
                            }
                        }
                    }
                } else if t.state == .failed {
                    ui.ListRow(t.name, secondary: "It did not finish", icon: .triangle_alert) {
                        trailing: {
                            ui.Button("Try again", size: .small, role: .plain) {
                                on press: device.retry_transfer(
                                    device.nearby_send,
                                    transfer: t.handle,
                                )
                            }
                        }
                    }
                } else if t.state == .done {
                    ui.ListRow(
                        t.name,
                        secondary: if t.incoming { "Kept here" } else { "Sent" },
                        icon: .circle_check,
                    )
                } else {
                    ui.ListRow(t.name, secondary: "Turned down", icon: .x)
                }
            }
            if send.devices.is_empty()
                and send.transfers.is_empty()
                and mine.devices.is_empty()
                and mine.sounds.is_empty()
                and mine.routes.is_empty()
                and mine.asks.is_empty() {
                ui.EmptyState("No devices in reach", icon: .laptop, inline: true)
            }
        }
    }
}

/// The row key of one of this device's sounds, as the devices resource names it.
fn sound_row(token: Int) -> Text {
    "audio:sound:{token}"
}

/// The row key of one sound's route, as the devices resource names it.
fn route_row(id: Text) -> Text {
    "audio:route:{id}"
}

/// The row key of one device asking for this device's sound.
fn ask_row(id: Text) -> Text {
    "audio:ask:{id}"
}

/// Whether `token` is the sound a press on a device would send.
fn chosen_sound(chosen: Option<Int>, token: Int) -> Bool {
    if let c = chosen {
        return c == token
    }
    return false
}

/// Whether a sound is chosen to send at all.
fn sound_chosen(chosen: Option<Int>) -> Bool {
    if let _ = chosen {
        return true
    }
    return false
}

/// Where one of the person's devices stands: here, away, or takes the sound chosen to send.
fn device_line(d: device.PersonalDeviceView, chosen: Option<Int>) -> Text {
    return if sound_chosen(chosen) and d.online and d.audio_output {
        "Takes the sound you send"
    } else if d.online { "Here now" } else { "Away" }
}

/// Where a device of the person's stands in the Nearby Send list.
fn nearby_place(lab: Bool, reachable: Bool) -> Text {
    if not reachable { "Away" } else if lab { "Receiver" } else { "In reach" }
}

/// What a transfer that is moving says: to whom, or from whom, and how far it got.
fn transfer_moving(peer: Text, progress: Int, incoming: Bool) -> Text {
    if incoming { "From {peer}, {progress}%" } else { "To {peer}, {progress}%" }
}

/// The way to move a paired Bluetooth accessory to another of the person's devices: the accessory
/// stays paired here and plays there.
view bluetooth_hand_off(address: Text) {
    let mine = device.devices
    let others = mine.devices.filter(d -> not d.local and d.online)
    if not others.is_empty() {
        ui.MenuButton("Move it to…", icon: .laptop) {
            for d in others {
                ui.MenuItem(d.label) {
                    on choose: device.hand_off(device.devices, address: address, device: d.id)
                }
            }
        }
    }
}

/// The controls the panel shows, and the way every person changes them: each tile is one row here,
/// with the press that hides it or moves it to the front of the panel, and the ones the panel does
/// not show are offered back. The order and the choice are the person's own setting, published as
/// `device.quick_settings`, so the panel, an agent and a script change them the same way.
view controls_editor() {
    state open: Bool = false
    let qs = device.quick_settings
    row(fill, distribute: .end) {
        ui.Button(
            "Customise the panel",
            icon: .sliders_horizontal,
            size: .small,
            role: .plain,
            toggled: open,
        ) {
            on press: set(open, not open)
        }
    }
    if open {
        ui.Section("Panel controls") {
            for c in qs.controls {
                ui.ListRow(
                    control_label(c),
                    icon: if c == "bluetooth" {
                        .bluetooth
                    } else if c == "vpn" {
                        .shield
                    } else if c == "night_light" {
                        .moon
                    } else if c == "airplane_mode" {
                        .plane
                    } else if c == "mobile_data" {
                        .globe
                    } else if c == "do_not_disturb" {
                        .bell_off
                    } else if c == "rotation_lock" {
                        .rotate_cw
                    } else if c == "power_mode" { .gauge } else { .wifi },
                ) {
                    trailing: {
                        row(gap: .related, align: .center) {
                            ui.Button(
                                "Move {control_label(c)} to the front",
                                icon: .arrow_up,
                                icon_only: true,
                                size: .small,
                                role: .plain,
                            ) {
                                on press: device.move_control(
                                    device.quick_settings,
                                    control: c,
                                    before: qs.controls.first(),
                                )
                            }
                            ui.Button(
                                "Hide {control_label(c)}",
                                icon: .x,
                                icon_only: true,
                                size: .small,
                                role: .plain,
                            ) {
                                on press: device.hide_control(device.quick_settings, control: c)
                            }
                        }
                    }
                }
            }
            for c in hidden_controls(qs.controls) {
                ui.ListRow(control_label(c), secondary: "Not on the panel", icon: .plus) {
                    trailing: {
                        ui.Button(
                            "Show {control_label(c)}",
                            icon: .plus,
                            size: .small,
                            role: .plain,
                        ) {
                            on press: device.show_control(device.quick_settings, control: c)
                        }
                    }
                }
            }
            row(fill, distribute: .end) {
                ui.Button("Reset the panel", size: .small, role: .plain) {
                    on press: device.reset_controls(device.quick_settings)
                }
            }
        }
    }
}

/// The controls the panel does not show, in the one order they are offered back in.
fn hidden_controls(shown: List<Text>) -> List<Text> {
    let every = [
        "wifi",
        "bluetooth",
        "vpn",
        "night_light",
        "airplane_mode",
        "mobile_data",
        "do_not_disturb",
        "rotation_lock",
        "power_mode",
    ]
    return every.filter(c -> not shown.contains(c))
}

/// What a control is called on the panel and here.
fn control_label(control: Text) -> Text {
    if control == "wifi" {
        "Wi-Fi"
    } else if control == "bluetooth" {
        "Bluetooth"
    } else if control == "vpn" {
        "VPN"
    } else if control == "night_light" {
        "Night light"
    } else if control == "airplane_mode" {
        "Airplane mode"
    } else if control == "mobile_data" {
        "Mobile data"
    } else if control == "do_not_disturb" {
        "Do not disturb"
    } else if control == "rotation_lock" {
        "Rotation lock"
    } else if control == "power_mode" { "Power mode" } else { control }
}
