use so.tilo.device as device
use so.tilo.settings as settings

/// The quick settings: Wi-Fi, Bluetooth, dark mode and do-not-disturb as tiles, the volume and
/// the brightness as sliders, the battery and the ways to leave the session. Each reads the
/// resource the session publishes (the sound, the network, the person's appearance settings)
/// and each press is that resource's own published action, so the settings app, the keys and
/// the agent change the same thing this panel shows, and nothing here holds a copy of it.
///
/// It is a floating panel at the desk's trailing edge, over what it covers, beside the
/// notification centre and on the same material; the bar's status opens it.
view quick_settings(d: Desk) {
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
                grid(min: 10rem, gap: .related) {
                    wifi_tile()
                    bluetooth_tile()
                    dark_tile()
                    quiet_tile()
                }
                levels()
                power_row()
            }
        }
    }
}

/// Wi-Fi, on while it is on, naming the network it is joined to; it cannot be pressed on a
/// device without it.
view wifi_tile() {
    let wifi = device.network.wifi
    let joined = wifi.networks.filter(n -> n.connected).first()
    let label = if let n = joined { n.name } else { "Wi-Fi" }
    ui.Button(label, icon: .wifi, toggled: wifi.on, enabled: wifi.available) {
        on press: device.set_wifi(device.network, not wifi.on)
    }
}

/// Bluetooth, on while it is on; it cannot be pressed on a device without it.
view bluetooth_tile() {
    let b = device.bluetooth
    ui.Button("Bluetooth", icon: .bluetooth, toggled: b.on, enabled: b.available) {
        on press: device.set_bluetooth(b, not b.on)
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

/// The volume and the brightness, each where the device has it: dragging sets it as the
/// device reads it back.
view levels() {
    if let volume = device.sound.volume {
        row(gap: .related, align: .center) {
            ui.Icon(.speaker)
            ui.Slider(value: volume, label: "Volume", fill) {
                on change(v): device.set_volume(device.sound, v)
            }
        }
    }
    if let level = device.displays.level {
        row(gap: .related, align: .center) {
            ui.Icon(.sun)
            ui.Slider(value: level, label: "Brightness", fill) {
                on change(v): device.set_brightness(device.displays, v)
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
