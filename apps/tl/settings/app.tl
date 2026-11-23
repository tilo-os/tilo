app so.tilo.settings "Settings" (icon: .settings, category: .system)

use so.tilo.settings as settings
use so.tilo.device as device

/// The window's root. Settings keeps nothing of its own here: every row below changes a
/// resource the session publishes — the person's appearance, notification and accessibility
/// settings, the device's sound, network, Bluetooth, displays and power — so a change made
/// here, in the shell's quick settings or by an agent is one change, with one history.
type Settings {
}

root Settings

default view settings_window for Settings

/// The window: the panes down the side under what they are for, the chosen one beside them.
view settings_window(s: Settings) {
    state pane: Text = "appearance"
    ui.NavShell(content_kind: .page, title: pane_title(pane), sidebar_title: "Settings") {
        sidebar: {
            ui.NavList(selected: pane) {
                ui.NavSection("Connectivity") {
                    ui.NavItem("network", "Network", icon: .wifi) {
                        on select: set(pane, "network")
                    }
                    ui.NavItem("bluetooth", "Bluetooth", icon: .bluetooth) {
                        on select: set(pane, "bluetooth")
                    }
                    ui.NavItem("speakers", "Speakers", icon: .speaker) {
                        on select: set(pane, "speakers")
                    }
                }
                ui.NavSection("Devices") {
                    ui.NavItem("displays", "Displays", icon: .monitor) {
                        on select: set(pane, "displays")
                    }
                    ui.NavItem("sound", "Sound", icon: .speaker) {
                        on select: set(pane, "sound")
                    }
                    ui.NavItem("power", "Power", icon: .battery) {
                        on select: set(pane, "power")
                    }
                }
                ui.NavSection("Personal") {
                    ui.NavItem("appearance", "Appearance", icon: .palette) {
                        on select: set(pane, "appearance")
                    }
                    ui.NavItem("notifications", "Notifications", icon: .bell) {
                        on select: set(pane, "notifications")
                    }
                }
            }
        }
        content: {
            ui.Scroll {
                stack(padding: .group, max: 64rem, gap: .section) {
                    if pane == "network" {
                        network_pane()
                    } else if pane == "bluetooth" {
                        bluetooth_pane()
                    } else if pane == "speakers" {
                        speakers_page()
                    } else if pane == "displays" {
                        displays_pane()
                    } else if pane == "sound" {
                        sound_pane()
                    } else if pane == "power" {
                        power_pane()
                    } else if pane == "notifications" {
                        notifications_pane()
                    } else {
                        appearance_pane()
                    }
                }
            }
        }
    }
}

fn pane_title(pane: Text) -> Text {
    if pane == "network" {
        "Network"
    } else if pane == "bluetooth" {
        "Bluetooth"
    } else if pane == "speakers" {
        "Speakers"
    } else if pane == "displays" {
        "Displays"
    } else if pane == "sound" {
        "Sound"
    } else if pane == "power" {
        "Power"
    } else if pane == "notifications" {
        "Notifications"
    } else {
        "Appearance"
    }
}

/// One setting: what it is, a line about it, and the control that changes it at the row's
/// trailing edge.
template view setting(title: Text, about: Text, control: View) {
    ui.ListRow(title, secondary: about) {
        trailing: { control }
    }
}

/// The look of the person's workspace and of this device: the theme they chose, the motion and
/// contrast they want, and how large text and controls are.
view appearance_pane() {
    let a = settings.appearance
    ui.Section("Look") {
        setting(
            "Mode",
            "The look the whole desktop takes; automatic follows the sun",
            ui.ToggleGroup(
                selected: if a.mode == "dark" {
                    1
                } else if a.mode == "light" { 2 } else { 0 },
                label: "Mode",
            ) {
                ui.Segment("Automatic")
                ui.Segment("Light")
                ui.Segment("Dark")
                on change(i): settings.set_dark_mode(a, i == 2)
            },
        )
        setting(
            "High contrast",
            "Stronger ink and surfaces for everything drawn",
            ui.Toggle(bind: a.high_contrast),
        )
        setting(
            "Reduce motion",
            "Turn animations down, for a screen that is easier to follow",
            ui.Toggle(bind: a.reduce_motion),
        )
        setting(
            "Reduce transparency",
            "Draw panels solid instead of seeing through them",
            ui.Toggle(bind: a.reduce_transparency),
        )
        setting(
            "Text size",
            "The size of text everywhere, before an app's own",
            ui.Slider(
                bind: a.text_scale,
                label: "Text size",
                min: 0.8,
                max: 1.6,
                step: 0.05,
                shows_value: true,
            ),
        )
        setting(
            "Control size",
            "How large buttons, switches and rows are",
            ui.Slider(
                bind: a.ui_scale,
                label: "Control size",
                min: 0.75,
                max: 1.5,
                step: 0.05,
                shows_value: true,
            ),
        )
    }
}

/// What this device says and shows when something happens: the banners, the sounds, and the
/// hour the person wants quiet.
view notifications_pane() {
    let n = settings.notifications
    ui.Section("Do not disturb") {
        setting(
            "Do not disturb",
            "Hold every banner until it is turned off",
            ui.Toggle(bind: n.do_not_disturb),
        )
    }
    ui.Section("Banners") {
        setting(
            "Show banners",
            "Say what happened as it happens",
            ui.Toggle(bind: n.show_banners),
        )
        setting(
            "Show what it says",
            "Let a banner carry the message it is about",
            ui.Toggle(bind: n.show_previews),
        )
        setting(
            "Show on the lock screen",
            "Let a banner arrive while the screen is locked",
            ui.Toggle(bind: n.show_on_lock_screen),
        )
        setting(
            "Show counts",
            "A count on an app's icon for what it holds unread",
            ui.Toggle(bind: n.show_badges),
        )
    }
    ui.Section("Sounds") {
        setting(
            "Play sounds",
            "A sound with what happens, in the system's own set",
            ui.Toggle(bind: n.play_sounds),
        )
    }
}


/// This device's screens: the brightness of each, and whether the picture turns with it.
view displays_pane() {
    let d = settings.display
    let screens = device.displays
    ui.Section("This device") {
        setting(
            "Automatic brightness",
            "Follow the light in the room",
            ui.Toggle(bind: d.auto_brightness),
        )
        setting(
            "Rotation lock",
            "Keep the picture as it is when the device turns",
            ui.Toggle(bind: d.rotation_lock),
        )
        for display in screens.displays {
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
    }
}

/// Where sound comes out and goes in, and how it is shaped.
view sound_pane() {
    let sound = device.sound
    ui.Section("Output") {
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
    }
    ui.Section("Input") {
        if let level = sound.input_volume {
            row(gap: .related, align: .center) {
                ui.Button(
                    if sound.input_muted {
                        "Unmute the microphone"
                    } else {
                        "Mute the microphone"
                    },
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
        for input in sound.inputs {
            ui.ListRow(
                input.label,
                secondary: if input.chosen { "Recording here" } else { "" },
                selected: input.chosen,
                icon: .mic,
            ) {
                on activate: device.select_input(device.sound, node: input.node)
            }
        }
    }
}

/// How this device uses its battery: the power mode it runs in and what it charges to.
view power_pane() {
    let power = device.power
    let battery = device.battery
    ui.Section("Battery") {
        if battery.present {
            ui.ListRow(
                "{battery.percentage}%",
                secondary: if let left = battery.minutes_left {
                    "{left} minutes left"
                } else {
                    battery.state
                },
                icon: .battery,
            )
        }
    }
    ui.Section("Power mode") {
        ui.ToggleGroup(
            selected: if let mode = power.mode {
                if mode == .saver {
                    0
                } else if mode == .performance { 2 } else { 1 }
            } else {
                1
            },
            label: "Power mode",
        ) {
            ui.Segment("Power saver")
            ui.Segment("Balanced")
            ui.Segment("Performance")
            on change(i): device.set_power_mode(
                device.power,
                mode: if i == 0 {
                    "saver"
                } else if i == 2 { "performance" } else { "balanced" },
            )
        }
    }
}

/// The networks in reach and the one this device is on.
view network_pane() {
    let net = device.network
    let wifi = net.wifi
    ui.Section("Wi-Fi") {
        ui.ListRow(
            "Wi-Fi",
            secondary: if wifi.on { "On" } else { "Off" },
            icon: .wifi,
        ) {
            trailing: {
                ui.Button(
                    if wifi.on { "Turn Wi-Fi off" } else { "Turn Wi-Fi on" },
                    icon: if wifi.on { .wifi } else { .wifi_off },
                    icon_only: true,
                    toggled: wifi.on,
                    role: .plain,
                ) {
                    on press: device.set_wifi(device.network, not wifi.on)
                }
            }
        }
        if wifi.on {
            ui.Button("Scan for networks", icon: .search) {
                on press: device.scan_wifi(device.network)
            }
            for network in wifi.networks {
                ui.ListRow(
                    network.ssid,
                    secondary: if network.connected { "Connected" } else { "" },
                    selected: network.connected,
                    icon: .wifi,
                ) {
                    on activate: device.join_wifi(device.network, network.ssid)
                }
            }
        }
    }
    ui.Section("Airplane mode") {
        setting(
            "Airplane mode",
            "Turn every radio off at once",
            ui.Button(
                "Airplane mode",
                icon: .plane,
                toggled: net.airplane_mode,
                role: .plain,
            ) {
                on press: device.set_airplane_mode(device.network, not net.airplane_mode)
            },
        )
    }
}

/// The Bluetooth devices in reach and the ones this device knows.
view bluetooth_pane() {
    let bt = device.bluetooth
    ui.Section("Bluetooth") {
        ui.ListRow(
            "Bluetooth",
            secondary: if bt.on { "On" } else { "Off" },
            icon: .bluetooth,
        ) {
            trailing: {
                ui.Button(
                    if bt.on {
                        "Turn Bluetooth off"
                    } else {
                        "Turn Bluetooth on"
                    },
                    icon: .bluetooth,
                    icon_only: true,
                    toggled: bt.on,
                    role: .plain,
                ) {
                    on press: device.set_bluetooth(device.bluetooth, not bt.on)
                }
            }
        }
        if bt.on {
            if bt.discovering {
                ui.Button("Stop scanning", icon: .search) {
                    on press: device.scan(device.bluetooth, on: false)
                }
            } else {
                ui.Button("Scan for devices", icon: .search) {
                    on press: device.scan(device.bluetooth, on: true)
                }
            }
            for nearby in bt.nearby {
                ui.ListRow(nearby.name, icon: .bluetooth) {
                    on activate: device.pair(device.bluetooth, nearby.address)
                }
            }
        }
        for accessory in bt.accessories {
            ui.ListRow(
                accessory.name,
                secondary: if accessory.connected { "Connected" } else { "" },
                selected: accessory.connected,
                icon: .headphones,
            ) {
                on activate: device.connect(device.bluetooth, accessory.address)
            }
        }
    }
}
