app so.tilo.lang.settings "Settings" (
    icon: .settings,
    category: .system,
    tagline: "This device, your workspace and the accounts they keep",
    keywords: ["settings", "preferences", "configuration", "einstellungen", "system", "options"],
)

use so.tilo.settings as settings
use so.tilo.device as device

/// The window's root. Settings keeps nothing of its own here: every row below changes a
/// resource the session publishes — the person's appearance, notification, accessibility and
/// input settings, the device's sound, network, Bluetooth, displays, printers and power — so a
/// change made here, in the shell's quick settings or by an agent is one change, with one history.
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
                    ui.NavItem("usage", "Data usage", icon: .bar_chart) {
                        on select: set(pane, "usage")
                    }
                }
                ui.NavSection("Devices") {
                    ui.NavItem("displays", "Displays", icon: .monitor) {
                        on select: set(pane, "displays")
                    }
                    ui.NavItem("sound", "Sound", icon: .speaker) {
                        on select: set(pane, "sound")
                    }
                    ui.NavItem("speakers", "Speakers", icon: .speaker) {
                        on select: set(pane, "speakers")
                    }
                    ui.NavItem("power", "Power", icon: .battery) {
                        on select: set(pane, "power")
                    }
                    ui.NavItem("printers", "Printers", icon: .printer) {
                        on select: set(pane, "printers")
                    }
                    ui.NavItem("drives", "Drives", icon: .hard_drive) {
                        on select: set(pane, "drives")
                    }
                    ui.NavItem("server", "This device's server", icon: .server) {
                        on select: set(pane, "server")
                    }
                    ui.NavItem("input", "Keyboard and pointing", icon: .keyboard) {
                        on select: set(pane, "input")
                    }
                }
                ui.NavSection("Personal") {
                    ui.NavItem("appearance", "Appearance", icon: .palette) {
                        on select: set(pane, "appearance")
                    }
                    ui.NavItem("themes", "Themes", icon: .palette) {
                        on select: set(pane, "themes")
                    }
                    ui.NavItem("accessibility", "Accessibility", icon: .accessibility) {
                        on select: set(pane, "accessibility")
                    }
                    ui.NavItem("notifications", "Notifications", icon: .bell) {
                        on select: set(pane, "notifications")
                    }
                    ui.NavItem("shortcuts", "Keyboard shortcuts", icon: .command) {
                        on select: set(pane, "shortcuts")
                    }
                    ui.NavItem("region", "Region and formats", icon: .globe) {
                        on select: set(pane, "region")
                    }
                    ui.NavItem("spelling", "Spelling and text", icon: .spell_check) {
                        on select: set(pane, "spelling")
                    }
                    ui.NavItem("default_apps", "Default apps", icon: .layout_grid) {
                        on select: set(pane, "default_apps")
                    }
                    ui.NavItem("time", "Date and time", icon: .clock) {
                        on select: set(pane, "time")
                    }
                }
                ui.NavSection("Accounts and access") {
                    ui.NavItem("accounts", "Accounts", icon: .mail) {
                        on select: set(pane, "accounts")
                    }
                    ui.NavItem("users", "Users", icon: .user) {
                        on select: set(pane, "users")
                    }
                    ui.NavItem("agent_models", "Agent models", icon: .sparkles) {
                        on select: set(pane, "agent_models")
                    }
                    ui.NavItem("devices", "Devices and access", icon: .monitor_smartphone) {
                        on select: set(pane, "devices")
                    }
                    ui.NavItem("continuity", "Continuity", icon: .cloud) {
                        on select: set(pane, "continuity")
                    }
                }
                ui.NavSection("System") {
                    ui.NavItem("firmware", "Firmware", icon: .package) {
                        on select: set(pane, "firmware")
                    }
                    ui.NavItem("compatibility", "Compatibility", icon: .puzzle) {
                        on select: set(pane, "compatibility")
                    }
                    ui.NavItem("disk_unlock", "Disk unlock", icon: .lock) {
                        on select: set(pane, "disk_unlock")
                    }
                    ui.NavItem("storage", "Storage", icon: .database) {
                        on select: set(pane, "storage")
                    }
                    ui.NavItem("updates", "Updates", icon: .download) {
                        on select: set(pane, "updates")
                    }
                    ui.NavItem("about", "About", icon: .info) {
                        on select: set(pane, "about")
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
                    } else if pane == "usage" {
                        data_usage_pane()
                    } else if pane == "displays" {
                        displays_pane()
                    } else if pane == "sound" {
                        sound_pane()
                    } else if pane == "speakers" {
                        speakers_page()
                    } else if pane == "power" {
                        power_pane()
                    } else if pane == "printers" {
                        printers_pane()
                    } else if pane == "drives" {
                        drives_pane()
                    } else if pane == "server" {
                        device_server_pane()
                    } else if pane == "input" {
                        input_pane()
                    } else if pane == "accessibility" {
                        accessibility_pane()
                    } else if pane == "notifications" {
                        notifications_pane()
                    } else if pane == "shortcuts" {
                        shortcuts_pane()
                    } else if pane == "region" {
                        region_pane()
                    } else if pane == "spelling" {
                        spelling_pane()
                    } else if pane == "default_apps" {
                        default_apps_pane()
                    } else if pane == "time" {
                        date_time_pane()
                    } else if pane == "accounts" {
                        accounts_pane()
                    } else if pane == "agent_models" {
                        agent_models_pane()
                    } else if pane == "devices" {
                        devices_pane()
                    } else if pane == "continuity" {
                        continuity_pane()
                    } else if pane == "firmware" {
                        firmware_pane()
                    } else if pane == "compatibility" {
                        compatibility_pane()
                    } else if pane == "disk_unlock" {
                        disk_unlock_pane()
                    } else if pane == "storage" {
                        storage_pane()
                    } else if pane == "updates" {
                        updates_pane()
                    } else if pane == "themes" {
                        themes_pane()
                    } else if pane == "users" {
                        users_pane()
                    } else if pane == "about" {
                        about_pane()
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
    } else if pane == "usage" {
        "Data usage"
    } else if pane == "displays" {
        "Displays"
    } else if pane == "sound" {
        "Sound"
    } else if pane == "speakers" {
        "Speakers"
    } else if pane == "power" {
        "Power"
    } else if pane == "printers" {
        "Printers"
    } else if pane == "drives" {
        "Drives"
    } else if pane == "server" {
        "This device's server"
    } else if pane == "input" {
        "Keyboard and pointing"
    } else if pane == "themes" {
        "Themes"
    } else if pane == "accessibility" {
        "Accessibility"
    } else if pane == "notifications" {
        "Notifications"
    } else if pane == "shortcuts" {
        "Keyboard shortcuts"
    } else if pane == "region" {
        "Region and formats"
    } else if pane == "spelling" {
        "Spelling and text"
    } else if pane == "default_apps" {
        "Default apps"
    } else if pane == "time" {
        "Date and time"
    } else if pane == "accounts" {
        "Accounts"
    } else if pane == "agent_models" {
        "Agent models"
    } else if pane == "devices" {
        "Devices and access"
    } else if pane == "continuity" {
        "Continuity"
    } else if pane == "firmware" {
        "Firmware"
    } else if pane == "compatibility" {
        "Compatibility"
    } else if pane == "disk_unlock" {
        "Disk unlock"
    } else if pane == "storage" {
        "Storage"
    } else if pane == "updates" {
        "Updates"
    } else if pane == "users" {
        "Users"
    } else if pane == "about" {
        "About"
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
                } else if a.mode == "light" {
                    2
                } else {
                    0
                },
                label: "Mode",
            ) {
                ui.Segment("Automatic")
                ui.Segment("Light")
                ui.Segment("Dark")
                on change(i): settings.set_dark_mode(a, on: i == 2)
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
        setting(
            "Accent",
            "The colour presses and selections take; empty follows the theme",
            ui.TextInput(bind: a.accent, placeholder: "Follow the theme"),
        )
        setting(
            "Interface face",
            "The typeface the desktop is drawn in; empty follows the theme",
            ui.TextInput(bind: a.ui_face, placeholder: "Follow the theme"),
        )
    }
    ui.Section("Reading") {
        setting(
            "Wallpaper",
            "The picture the desktop stands on; empty follows the theme",
            ui.TextInput(bind: a.wallpaper, placeholder: "Follow the theme"),
        )
        setting(
            "Distinguish similar letters",
            "Set I, l, 1, 0 and O apart in all text",
            ui.Toggle(bind: a.distinct_letters),
        )
        setting(
            "Windows and panels",
            "Let the theme's translucency show; 0 draws them solid",
            ui.Slider(
                bind: a.translucency,
                label: "Translucency",
                min: 0.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ),
        )
    }
    ui.Section("Desktop") {
        let desktop = settings.desktop_controls
        let on_device = settings.desktop_controls_device
        setting(
            "Vim keys",
            "Move between what shows with h, j, k and l",
            ui.Toggle(bind: desktop.vim_mode),
        )
        setting(
            "Hide the top bar",
            "Give the screen to what it shows on this device",
            ui.Toggle(bind: on_device.hide_top_bar),
        )
    }
}

/// What this device says and shows when something happens: the banners, the sounds, and the
/// hour the person wants quiet.
view notifications_pane() {
    state app: Text = ""
    state duration: Text = "For 8 hours"
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
    ui.Section("Apps") {
        if n.per_app.keys().is_empty() {
            ui.Text(
                "An app follows the rules above until one is given its own.",
                role: .caption,
                secondary: true,
            )
        }
        for app_id in n.per_app.keys() {
            if let rule = n.per_app.get(app_id) {
                ui.ListRow(app_id, secondary: rule_words(rule), icon: .layout_grid) {
                    trailing: {
                        row(gap: .tight, align: .center, wrap: true) {
                            ui.Button("Mute for 8 hours", size: .small) {
                                on press: settings.mute_app(
                                    settings.notifications,
                                    app: app_id,
                                    duration: "For 8 hours",
                                )
                            }
                            ui.Button("Unmute", size: .small) {
                                on press: settings.mute_app(
                                    settings.notifications,
                                    app: app_id,
                                    duration: "Not muted",
                                )
                            }
                        }
                    }
                }
                if rule.banners == .some(false) {
                    ui.Text("Banners off", role: .caption, secondary: true)
                }
                if rule.sounds == .some(false) {
                    ui.Text("Sounds off", role: .caption, secondary: true)
                }
                if rule.previews == .some(false) {
                    ui.Text("Previews off", role: .caption, secondary: true)
                }
                if rule.lock_screen == .some(false) {
                    ui.Text("Nothing on the lock screen", role: .caption, secondary: true)
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(bind: app, placeholder: "An app's id, such as so.tilo.notes", fill)
            ui.Dropdown(
                bind: duration,
                options: ["Not muted", "For 1 hour", "For 8 hours", "For 24 hours"],
                caption: "For how long",
            )
            ui.Button("Mute it", role: .primary, size: .small) {
                on press: settings.mute_app(
                    settings.notifications,
                    app: app,
                    duration: duration,
                )
            }
        }
    }
    ui.Section("Quiet hours") {
        let focus = n.focus
        setting(
            "Quiet hours",
            "Hold banners and sounds between the hours below",
            ui.Toggle(bind: n.focus.enabled),
        )
        if focus.enabled {
            ui.Text(
                "Quiet between {quiet_clock(focus.start_minute)} and {quiet_clock(focus.end_minute)}, {quiet_days(focus.weekdays)}.",
                role: .caption,
                secondary: true,
            )
            setting(
                "Priority apps still come through",
                "An app or a person marked as priority reaches you anyway",
                ui.Toggle(bind: n.focus.allow_priority),
            )
        }
    }
}

/// The time of day `minutes` after midnight, as a clock reads it: `22:05`.
fn quiet_clock(minutes: Int) -> Text {
    let at = minutes % 1440
    "{two_digits(at / 60)}:{two_digits(at % 60)}"
}

/// `n` with a leading zero below ten, as a clock's hour and minute read.
fn two_digits(n: Int) -> Text {
    if n < 10 { "0{n}" } else { "{n}" }
}

/// The days quiet hours keep, from the bitmask Monday as bit zero: zero is every day.
fn quiet_days(days: Int) -> Text {
    if days == 0 {
        "every day"
    } else if days == 31 {
        "on weekdays"
    } else if days == 96 {
        "at the weekend"
    } else {
        "on some days"
    }
}

/// This device's screens: the brightness of each, whether the picture turns with it, and the
/// warmth of the night light.
view displays_pane() {
    let d = settings.display
    let night = settings.night_light
    let screens = device.displays
    ui.Section("This device") {
        setting(
            "Automatic brightness",
            "Follow the light in the room",
            ui.Toggle(bind: d.auto_brightness),
        )
        setting(
            "Scale",
            "How large this screen draws what it shows",
            ui.Slider(
                bind: d.scale,
                label: "Scale",
                min: 0.5,
                max: 3.0,
                step: 0.25,
                shows_value: true,
            ),
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
    ui.Section("Night light") {
        setting(
            "Night light",
            "Warm the screen's colours in the evening",
            ui.Toggle(bind: night.night_light),
        )
        setting(
            "Warmth",
            "How far toward amber the screen goes",
            ui.Slider(
                bind: night.night_light_temperature,
                label: "Warmth",
                min: 1000.0,
                max: 6500.0,
                step: 50.0,
                shows_value: true,
            ),
        )
        setting(
            "Follow the evening",
            "Turn on at sunset and off at sunrise by itself",
            ui.Toggle(bind: night.night_light_schedule),
        )
    }
}

/// Where sound comes out and goes in, and how it is shaped.
view sound_pane() {
    state listening: Bool = false
    let sound = device.sound
    let audio = settings.audio
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
    ui.Section("Meters") {
        if let input = sound.inputs.find(x -> x.chosen) {
            ui.LevelMeter(
                source: input.node,
                output: false,
                running: listening,
                label: "Microphone level",
            )
            ui.Button(if listening { "Stop listening" } else { "Listen to the microphone" }, toggled: listening) {
                on press: set(listening, not listening)
            }
        } else {
            ui.Text("No microphone is chosen above.", role: .caption, secondary: true)
        }
        if let output = sound.outputs.find(x -> x.chosen) {
            ui.LevelMeter(
                source: output.node,
                output: true,
                running: listening,
                label: "Output level",
            )
        }
    }
    ui.Section("Balance and shape") {
        setting(
            "Balance",
            "Which side the sound favours; the middle is even",
            ui.Slider(
                bind: audio.balance,
                label: "Balance",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ),
        )
        setting(
            "One channel",
            "Play the same sound from every speaker",
            ui.Toggle(bind: audio.mono_output),
        )
        setting(
            "Echo cancellation",
            "Take the speakers out of what the microphone hears",
            ui.Toggle(bind: audio.echo_cancellation),
        )
    }
    ui.Section("System sounds") {
        let prefs = audio.system_sounds
        setting(
            "Play them",
            "A sound for a message, a screenshot, a connection",
            ui.Toggle(bind: prefs.enabled),
        )
        setting(
            "Their volume",
            "How loud the system's own sounds are",
            ui.Slider(
                bind: prefs.volume,
                label: "System sound volume",
                min: 0.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            ),
        )
    }
}

/// How this device uses its battery: the power mode it runs in, what it charges to and when it
/// goes to sleep.
view power_pane() {
    let power = device.power
    let battery = device.battery
    let prefs = settings.power
    ui.Section("Battery") {
        if battery.present {
            ui.ListRow(
                "{battery.percentage}%",
                secondary: if let left = battery.minutes_left {
                    "{left} minutes left"
                } else {
                    battery_words(battery.state)
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
                } else if mode == .performance {
                    2
                } else {
                    1
                }
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
                } else if i == 2 {
                    "performance"
                } else {
                    "balanced"
                },
            )
        }
    }
    ui.Section("Sleep") {
        setting(
            "Sleep by itself",
            "Go to sleep after a while away",
            ui.Toggle(bind: prefs.auto_suspend),
        )
        setting(
            "Go to sleep after",
            "Minutes away before the screen goes dark and the device sleeps",
            ui.NumberInput(
                bind: prefs.suspend_minutes,
                label: "Minutes",
                min: 1.0,
                max: 240.0,
                step: 1.0,
            ),
        )
        setting(
            "Turn the screen off after",
            "Minutes away before the screen goes dark",
            ui.NumberInput(
                bind: prefs.blank_minutes,
                label: "Minutes",
                min: 1.0,
                max: 240.0,
                step: 1.0,
            ),
        )
        setting(
            "Lock after",
            "Minutes away before the screen locks",
            ui.NumberInput(
                bind: prefs.lock_minutes,
                label: "Minutes",
                min: 0.0,
                max: 240.0,
                step: 1.0,
            ),
        )
        setting(
            "Sleep locks the screen",
            "Ask for the person's key when it wakes",
            ui.Toggle(bind: prefs.lock_on_sleep),
        )
    }
    ui.Section("What the device's buttons do") {
        setting(
            "Closing the lid",
            "What happens when a laptop's lid is shut",
            ui.EnumField(bind: prefs.lid_close_action, label: "Closing the lid"),
        )
        setting(
            "The power button",
            "What pressing it does",
            ui.EnumField(bind: prefs.power_key_action, label: "The power button"),
        )
        setting(
            "When the battery is critical",
            "What the device does as it runs out",
            ui.EnumField(bind: prefs.critical_action, label: "Critical battery action"),
        )
    }
    ui.Section("When the battery is low") {
        setting(
            "Say so at",
            "Per cent left before the device says the battery is low",
            ui.NumberInput(
                bind: prefs.low_battery_percent,
                label: "Low battery percent",
                min: 1.0,
                max: 50.0,
                step: 1.0,
            ),
        )
        setting(
            "Act at",
            "Per cent left before the critical action",
            ui.NumberInput(
                bind: prefs.critical_battery_percent,
                label: "Critical battery percent",
                min: 1.0,
                max: 30.0,
                step: 1.0,
            ),
        )
    }
}

/// How the battery stands, in a word.
fn battery_words(state: device.BatteryState) -> Text {
    if state == .charging {
        "Charging"
    } else if state == .discharging {
        "On the battery"
    } else if state == .empty {
        "Empty"
    } else if state == .full {
        "Full"
    } else if state == .pending_charge {
        "About to charge"
    } else if state == .pending_discharge {
        "About to drain"
    } else {
        "Plugged in"
    }
}

/// What an app's own notification rule says of itself, in a word or two.
fn rule_words(rule: settings.AppNotificationRule) -> Text {
    if rule.muted_until != .none {
        "Muted for now"
    } else {
        "Its own rules"
    }
}
