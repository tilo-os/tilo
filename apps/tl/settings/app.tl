app so.tilo.settings "Settings" (
    icon: .settings,
    category: .system,
    tagline: "This device, your workspace and the accounts they keep",
    keywords: ["settings", "preferences", "configuration", "einstellungen", "system", "options"],
)

use so.tilo.settings.sections as settings
use so.tilo.device as device

/// The window's root. Settings keeps nothing of its own here: every row below changes a
/// resource the session publishes — the person's appearance, notification, accessibility and
/// input settings, the device's sound, network, Bluetooth, displays, printers and power — so a
/// change made here, in the shell's quick settings or by an agent is one change, with one history.
type Settings {
    /// The pane in front, by its own name: a search row, an agent or the sidebar's own presses
    /// set it, and the window shows that pane.
    pane: Text = "appearance"
}

root Settings

default view settings_window for Settings

/// The window: the panes down the side under what they are for, the chosen one beside them.
view settings_window(s: Settings) {
    ui.NavShell(content_kind: .page, title: pane_title(s.pane), sidebar_title: "Settings") {
        sidebar: {
            ui.NavList(selected: s.pane) {
                ui.NavSection("Connectivity") {
                    ui.NavItem("network", "Network", icon: .wifi) {
                        on select: open_pane("network")
                    }
                    ui.NavItem("bluetooth", "Bluetooth", icon: .bluetooth) {
                        on select: open_pane("bluetooth")
                    }
                    ui.NavItem("usage", "Data usage", icon: .bar_chart) {
                        on select: open_pane("usage")
                    }
                }
                ui.NavSection("Devices") {
                    ui.NavItem("displays", "Displays", icon: .monitor) {
                        on select: open_pane("displays")
                    }
                    ui.NavItem("sound", "Sound", icon: .speaker) {
                        on select: open_pane("sound")
                    }
                    ui.NavItem("speakers", "Speakers", icon: .speaker) {
                        on select: open_pane("speakers")
                    }
                    ui.NavItem("power", "Power", icon: .battery) {
                        on select: open_pane("power")
                    }
                    ui.NavItem("printers", "Printers", icon: .printer) {
                        on select: open_pane("printers")
                    }
                    ui.NavItem("drives", "Drives", icon: .hard_drive) {
                        on select: open_pane("drives")
                    }
                    ui.NavItem("server", "This device's server", icon: .server) {
                        on select: open_pane("server")
                    }
                    ui.NavItem("input", "Keyboard and pointing", icon: .keyboard) {
                        on select: open_pane("input")
                    }
                }
                ui.NavSection("Personal") {
                    ui.NavItem("appearance", "Appearance", icon: .palette) {
                        on select: open_pane("appearance")
                    }
                    ui.NavItem("themes", "Themes", icon: .palette) {
                        on select: open_pane("themes")
                    }
                    ui.NavItem("accessibility", "Accessibility", icon: .accessibility) {
                        on select: open_pane("accessibility")
                    }
                    ui.NavItem("notifications", "Notifications", icon: .bell) {
                        on select: open_pane("notifications")
                    }
                    ui.NavItem("shortcuts", "Keyboard shortcuts", icon: .command) {
                        on select: open_pane("shortcuts")
                    }
                    ui.NavItem("region", "Region and formats", icon: .globe) {
                        on select: open_pane("region")
                    }
                    ui.NavItem("spelling", "Spelling and text", icon: .spell_check) {
                        on select: open_pane("spelling")
                    }
                    ui.NavItem("default_apps", "Default apps", icon: .layout_grid) {
                        on select: open_pane("default_apps")
                    }
                    ui.NavItem("time", "Date and time", icon: .clock) {
                        on select: open_pane("time")
                    }
                }
                ui.NavSection("Accounts and access") {
                    ui.NavItem("accounts", "Accounts", icon: .mail) {
                        on select: open_pane("accounts")
                    }
                    ui.NavItem("users", "Users", icon: .user) {
                        on select: open_pane("users")
                    }
                    ui.NavItem("agent_models", "Agent models", icon: .sparkles) {
                        on select: open_pane("agent_models")
                    }
                    ui.NavItem("devices", "Devices and access", icon: .monitor_smartphone) {
                        on select: open_pane("devices")
                    }
                    ui.NavItem("continuity", "Continuity", icon: .cloud) {
                        on select: open_pane("continuity")
                    }
                }
                ui.NavSection("System") {
                    ui.NavItem("firmware", "Firmware", icon: .package) {
                        on select: open_pane("firmware")
                    }
                    ui.NavItem("compatibility", "Compatibility", icon: .puzzle) {
                        on select: open_pane("compatibility")
                    }
                    ui.NavItem("disk_unlock", "Disk unlock", icon: .lock) {
                        on select: open_pane("disk_unlock")
                    }
                    ui.NavItem("storage", "Storage", icon: .database) {
                        on select: open_pane("storage")
                    }
                    ui.NavItem("updates", "Updates", icon: .download) {
                        on select: open_pane("updates")
                    }
                    ui.NavItem("about", "About", icon: .info) {
                        on select: open_pane("about")
                    }
                }
            }
        }
        content: {
            ui.Scroll {
                stack(padding: .group, max: 64rem, gap: .section) {
                    if s.pane == "network" {
                        network_pane()
                    } else if s.pane == "bluetooth" {
                        bluetooth_pane()
                    } else if s.pane == "usage" {
                        data_usage_pane()
                    } else if s.pane == "displays" {
                        displays_pane()
                    } else if s.pane == "sound" {
                        sound_pane()
                    } else if s.pane == "speakers" {
                        speakers_page()
                    } else if s.pane == "power" {
                        power_pane()
                    } else if s.pane == "printers" {
                        printers_pane()
                    } else if s.pane == "drives" {
                        drives_pane()
                    } else if s.pane == "server" {
                        device_server_pane()
                    } else if s.pane == "input" {
                        input_pane()
                    } else if s.pane == "accessibility" {
                        accessibility_pane()
                    } else if s.pane == "notifications" {
                        notifications_pane()
                    } else if s.pane == "shortcuts" {
                        shortcuts_pane()
                    } else if s.pane == "region" {
                        region_pane()
                    } else if s.pane == "spelling" {
                        spelling_pane()
                    } else if s.pane == "default_apps" {
                        default_apps_pane()
                    } else if s.pane == "time" {
                        date_time_pane()
                    } else if s.pane == "accounts" {
                        accounts_pane()
                    } else if s.pane == "agent_models" {
                        agent_models_pane()
                    } else if s.pane == "devices" {
                        devices_pane()
                    } else if s.pane == "continuity" {
                        continuity_pane()
                    } else if s.pane == "firmware" {
                        firmware_pane()
                    } else if s.pane == "compatibility" {
                        compatibility_pane()
                    } else if s.pane == "disk_unlock" {
                        disk_unlock_pane()
                    } else if s.pane == "storage" {
                        storage_pane()
                    } else if s.pane == "updates" {
                        updates_pane()
                    } else if s.pane == "themes" {
                        themes_pane(s)
                    } else if s.pane == "users" {
                        users_pane()
                    } else if s.pane == "about" {
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
    } else if pane == "users" { "Users" } else if pane == "about" { "About" } else { "Appearance" }
}

/// One setting: what it is, a line about it, and the control that changes it at the row's
/// trailing edge.
template view setting(title: Text, about: Text, control: View) {
    ui.ListRow(title, secondary: about) {
        trailing: {
            control
        }
    }
}

/// The look of the person's workspace and of this device: the theme they chose, the motion and
/// contrast they want, and how large text and controls are.
view appearance_pane() {
    let a = settings.appearance
    ui.Section("Look") {
        setting("Mode", "The look the whole desktop takes; automatic follows the sun") {
            ui.ToggleGroup(selected: mode_index(a.mode), label: "Mode") {
                ui.Segment("Automatic") {
                    on press: settings.set_mode(a, "automatic")
                }
                ui.Segment("Light") {
                    on press: settings.set_mode(a, "light")
                }
                ui.Segment("Dark") {
                    on press: settings.set_mode(a, "dark")
                }
            }
        }
        setting("High contrast", "Stronger ink and surfaces for everything drawn") {
            ui.Toggle(bind: a.high_contrast)
        }
        setting("Reduce motion", "Turn animations down, for a screen that is easier to follow") {
            ui.Toggle(bind: a.reduce_motion)
        }
        setting("Reduce transparency", "Draw panels solid instead of seeing through them") {
            ui.Toggle(bind: a.reduce_transparency)
        }
        setting("Text size", "The size of text everywhere, before an app's own") {
            ui.Slider(
                bind: a.text_scale,
                label: "Text size",
                min: 0.8,
                max: 1.6,
                step: 0.05,
                shows_value: true,
            )
        }
        setting("Control size", "How large buttons, switches and rows are") {
            ui.Slider(
                bind: a.ui_scale,
                label: "Control size",
                min: 0.75,
                max: 1.5,
                step: 0.05,
                shows_value: true,
            )
        }
        setting("Accent", "The colour presses and selections take; empty follows the theme") {
            ui.TextInput(bind: a.accent, placeholder: "Follow the theme")
        }
        setting("Interface face", "The typeface the desktop is drawn in; empty follows the theme") {
            ui.TextInput(bind: a.ui_face, placeholder: "Follow the theme")
        }
    }
    ui.Section("Reading") {
        setting("Wallpaper", "The picture the desktop stands on; empty follows the theme") {
            ui.TextInput(bind: a.wallpaper, placeholder: "Follow the theme")
        }
        setting("Where it shows", "Behind the windows, at the lock screen, or both") {
            ui.ToggleGroup(
                selected: wallpaper_target_index(a.wallpaper_target),
                label: "Where it shows",
            ) {
                ui.Segment("Desktop") {
                    on press: settings.set_wallpaper_target(a, "background")
                }
                ui.Segment("Lock screen") {
                    on press: settings.set_wallpaper_target(a, "lockscreen")
                }
                ui.Segment("Both") {
                    on press: settings.set_wallpaper_target(a, "both")
                }
            }
        }
        setting(
            "Whose wallpaper",
            "The one this workspace shows, or the one every workspace does",
        ) {
            ui.ToggleGroup(
                selected: if a.wallpaper_scope == "all" { 1 } else { 0 },
                label: "Whose wallpaper",
            ) {
                ui.Segment("This workspace") {
                    on press: settings.set_wallpaper_scope(a, "workspace")
                }
                ui.Segment("Every workspace") {
                    on press: settings.set_wallpaper_scope(a, "all")
                }
            }
        }
        setting("Distinguish similar letters", "Set I, l, 1, 0 and O apart in all text") {
            ui.Toggle(bind: a.distinct_letters)
        }
        setting("Windows and panels", "Let the theme's translucency show; 0 draws them solid") {
            ui.Slider(
                bind: a.translucency,
                label: "Translucency",
                min: 0.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            )
        }
        setting(
            "Force high-contrast colours",
            "The accessibility palette over whatever theme shows",
        ) {
            ui.Toggle(bind: a.forced_colors)
        }
    }
    ui.Section("Day and night") {
        setting("Where the day ends", "What automatic light and dark, and the night light, go by") {
            ui.ToggleGroup(
                selected: if a.schedule == "custom" { 1 } else { 0 },
                label: "Day and night",
            ) {
                ui.Segment("Follow the sun") {
                    on press: settings.set_schedule(a, "sun")
                }
                ui.Segment("Fixed times") {
                    on press: settings.set_schedule(a, "custom")
                }
            }
        }
        setting(
            "Sunrise",
            "When the day starts, as a clock reads it; it stands in when the sun cannot be followed",
        ) {
            ui.TextInput(bind: a.sunrise, placeholder: "07:00")
        }
        setting(
            "Sunset",
            "When the night starts, as a clock reads it; it stands in when the sun cannot be followed",
        ) {
            ui.TextInput(bind: a.sunset, placeholder: "19:00")
        }
        if a.schedule != "custom" {
            ui.Text(
                "Following the sun, today's times come from the location your time zone names. Without one, the fixed times above stand in.",
                role: .caption,
                secondary: true,
            )
        }
    }
    ui.Section("Desktop") {
        let desktop = settings.desktop_controls
        let on_device = settings.desktop_controls_device
        setting("Vim keys", "Move between what shows with h, j, k and l") {
            ui.Toggle(bind: desktop.vim_mode)
        }
        setting("Hide the top bar", "Give the screen to what it shows on this device") {
            ui.Toggle(bind: on_device.hide_top_bar)
        }
    }
}

/// What this device says and shows when something happens: the banners, the sounds, and the
/// hour the person wants quiet.
view notifications_pane() {
    state chosen_app: Text = ""
    state duration: Text = "For 8 hours"
    let n = settings.notifications
    ui.Section("Do not disturb") {
        setting("Do not disturb", "Hold every banner until it is turned off") {
            ui.Toggle(bind: n.do_not_disturb)
        }
    }
    ui.Section("Banners") {
        setting("Show banners", "Say what happened as it happens") {
            ui.Toggle(bind: n.show_banners)
        }
        setting("Show what it says", "Let a banner carry the message it is about") {
            ui.Toggle(bind: n.show_previews)
        }
        setting("Show on the lock screen", "Let a banner arrive while the screen is locked") {
            ui.Toggle(bind: n.show_on_lock_screen)
        }
        setting("Show counts", "A count on an app's icon for what it holds unread") {
            ui.Toggle(bind: n.show_badges)
        }
    }
    ui.Section("Sounds") {
        setting("Play sounds", "A sound with what happens, in the system's own set") {
            ui.Toggle(bind: n.play_sounds)
        }
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
            ui.TextInput(bind: chosen_app, placeholder: "An app's id, such as so.tilo.notes", fill)
            ui.Dropdown(
                bind: duration,
                options: ["Not muted", "For 1 hour", "For 8 hours", "For 24 hours"],
                caption: "For how long",
            )
            ui.Button("Mute it", role: .primary, size: .small) {
                on press: settings.mute_app(
                    settings.notifications,
                    app: chosen_app,
                    duration: duration,
                )
            }
        }
    }
    ui.Section("Quiet hours") {
        let focus = n.focus
        setting("Quiet hours", "Hold banners and sounds between the hours below") {
            ui.Toggle(bind: n.focus.enabled)
        }
        if focus.enabled {
            setting("From", "Minutes after midnight the quiet starts") {
                ui.NumberInput(
                    bind: n.focus.start_minute,
                    label: "Minutes after midnight",
                    min: 0.0,
                    max: 1439.0,
                    step: 15.0,
                )
            }
            setting("Until", "Minutes after midnight the quiet ends") {
                ui.NumberInput(
                    bind: n.focus.end_minute,
                    label: "Minutes after midnight",
                    min: 0.0,
                    max: 1439.0,
                    step: 15.0,
                )
            }
            ui.Text(
                "Quiet between {quiet_clock(focus.start_minute)} and {quiet_clock(focus.end_minute)}, {quiet_days(focus.weekdays)}.",
                role: .caption,
                secondary: true,
            )
            setting("On these days", "Every day, weekdays, or the weekend") {
                ui.ToggleGroup(selected: quiet_days_index(focus.weekdays), label: "Days") {
                    ui.Segment("Every day") {
                        on press: settings.set_quiet_hours(
                            settings.notifications,
                            start_minute: focus.start_minute,
                            end_minute: focus.end_minute,
                            weekdays: 0,
                        )
                    }
                    ui.Segment("Weekdays") {
                        on press: settings.set_quiet_hours(
                            settings.notifications,
                            start_minute: focus.start_minute,
                            end_minute: focus.end_minute,
                            weekdays: 31,
                        )
                    }
                    ui.Segment("Weekend") {
                        on press: settings.set_quiet_hours(
                            settings.notifications,
                            start_minute: focus.start_minute,
                            end_minute: focus.end_minute,
                            weekdays: 96,
                        )
                    }
                }
            }
            setting(
                "Priority apps still come through",
                "An app or a person marked as priority reaches you anyway",
            ) {
                ui.Toggle(bind: n.focus.allow_priority)
            }
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
    } else if days == 96 { "at the weekend" } else { "on some days" }
}

/// Which of the days pickers the quiet hours' bitmask stands for.
fn quiet_days_index(days: Int) -> Int {
    if days == 31 { 1 } else if days == 96 { 2 } else { 0 }
}

/// This device's screens: the brightness of each, whether the picture turns with it, and the
/// warmth of the night light.
view displays_pane() {
    let d = settings.display
    let night = settings.display_person
    let screens = device.displays
    ui.Section("Arrangement") {
        ui.Text(
            "How this device shows what is on its screens. The desktop keeps where each display stands; choose the way it shows them here.",
            role: .caption,
            secondary: true,
        )
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Extend across them", icon: .monitor) {
                on press: device.set_mode(device.displays, mode: "extend")
            }
            ui.Button("Mirror them", icon: .copy) {
                on press: device.set_mode(device.displays, mode: "mirror")
            }
            ui.Button("Only the built-in screen", icon: .laptop) {
                on press: device.set_mode(device.displays, mode: "built-in")
            }
            ui.Button("Only the external screens", icon: .monitor) {
                on press: device.set_mode(device.displays, mode: "external")
            }
        }
        ui.Button("Arrange screens…", icon: .layout_grid) {
            on press: device.open_sheet(device.displays)
        }
    }
    ui.Section("This device") {
        setting("Automatic brightness", "Follow the light in the room") {
            ui.Toggle(bind: d.auto_brightness)
        }
        setting("Scale", "How large this screen draws what it shows") {
            ui.Slider(
                bind: d.scale,
                label: "Scale",
                min: 0.5,
                max: 3.0,
                step: 0.25,
                shows_value: true,
            )
        }
        setting("Rotation lock", "Keep the picture as it is when the device turns") {
            ui.Toggle(bind: d.rotation_lock)
        }
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
        setting("Night light", "Warm the screen's colours in the evening") {
            ui.Toggle(bind: night.night_light)
        }
        setting("Warmth", "How far toward amber the screen goes") {
            ui.Slider(
                bind: night.night_light_temperature,
                label: "Warmth",
                min: 1000.0,
                max: 6500.0,
                step: 50.0,
                shows_value: true,
            )
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Subtle", size: .small) {
                on press: settings.set_night_light_temperature(night, 4500)
            }
            ui.Button("Warm", size: .small) {
                on press: settings.set_night_light_temperature(night, 3700)
            }
            ui.Button("Very warm", size: .small) {
                on press: settings.set_night_light_temperature(night, 2800)
            }
        }
        setting("Follow the evening", "Turn on at sunset and off at sunrise by itself") {
            ui.Toggle(bind: night.night_light_schedule)
        }
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
                source: input.name,
                output: false,
                running: listening,
                label: "Microphone level",
            )
            ui.Button(
                if listening { "Stop listening" } else { "Listen to the microphone" },
                toggled: listening,
            ) {
                on press: set(listening, not listening)
            }
        } else {
            ui.Text("No microphone is chosen above.", role: .caption, secondary: true)
        }
        if let output = sound.outputs.find(x -> x.chosen) {
            ui.LevelMeter(
                source: output.name,
                output: true,
                running: listening,
                label: "Output level",
            )
        }
    }
    ui.Section("Balance and shape") {
        setting("Balance", "Which side the sound favours; the middle is even") {
            ui.Slider(
                bind: audio.balance,
                label: "Balance",
                min: -1.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            )
        }
        setting("One channel", "Play the same sound from every speaker") {
            ui.Toggle(bind: audio.mono_output)
        }
        setting("Echo cancellation", "Take the speakers out of what the microphone hears") {
            ui.Toggle(bind: audio.echo_cancellation)
        }
    }
    ui.Section("System sounds") {
        let prefs = audio.system_sounds
        setting("Play them", "A sound for a message, a screenshot, a connection") {
            ui.Toggle(bind: prefs.enabled)
        }
        setting("Their volume", "How loud the system's own sounds are") {
            ui.Slider(
                bind: prefs.volume,
                label: "System sound volume",
                min: 0.0,
                max: 1.0,
                step: 0.05,
                shows_value: true,
            )
        }
    }
}

/// How this device uses its battery: the power mode it runs in, what it charges to and when it
/// goes to sleep.
view power_pane() {
    state week: Bool = false
    let power = device.power
    let battery = device.battery
    let history = device.battery_history
    let prefs = settings.power
    ui.Section("Battery") {
        if battery.present {
            ui.ListRow(
                "{battery.percentage}%",
                secondary: if let left = battery.minutes_left {
                    "{left} minutes left"
                } else { battery_words(battery.state) },
                icon: .battery,
            )
        }
    }
    if battery.present {
        ui.Section("Its charge over time") {
            ui.ToggleGroup(selected: if week { 1 } else { 0 }, label: "Over") {
                ui.Segment("Today") {
                    on press: set(week, false)
                }
                ui.Segment("This week") {
                    on press: set(week, true)
                }
            }
            ui.Drawing(
                history_shapes(history.points, week),
                meaning: .chart,
                label: history_label(history.points, week),
                height: .some(170.0),
            )
            ui.Text(
                "A mark along the bottom is a stretch on the charger. Readings land every quarter hour.",
                role: .caption,
                secondary: true,
            )
        }
    }
    if let limit = battery.charge_limit {
        ui.Section("Charging") {
            setting(
                "Charge to",
                "Stop charging at this per cent, which keeps the pack's life; choose the top for a trip",
            ) {
                ui.MenuButton("Charge to {limit.current}%", icon: .battery) {
                    for choice in limit.choices {
                        ui.MenuItem("{choice}%", checked: choice == limit.current) {
                            on choose: device.set_charge_limit(device.battery, percent: choice)
                        }
                    }
                }
            }
        }
    }
    ui.Section("Power mode") {
        ui.ToggleGroup(
            selected: if let mode = power.mode {
                if mode == .saver { 0 } else if mode == .performance { 2 } else { 1 }
            } else { 1 },
            label: "Power mode",
        ) {
            ui.Segment("Power saver") {
                on press: device.set_power_mode(device.power, mode: "saver")
            }
            ui.Segment("Balanced") {
                on press: device.set_power_mode(device.power, mode: "balanced")
            }
            ui.Segment("Performance") {
                on press: device.set_power_mode(device.power, mode: "performance")
            }
        }
    }
    ui.Section("Sleep") {
        setting("Sleep by itself", "Go to sleep after a while away") {
            ui.Toggle(bind: prefs.auto_suspend)
        }
        setting(
            "Go to sleep after",
            "Minutes away before the screen goes dark and the device sleeps",
        ) {
            ui.NumberInput(
                bind: prefs.suspend_minutes,
                label: "Minutes",
                min: 1.0,
                max: 240.0,
                step: 1.0,
            )
        }
        setting(
            "On the battery",
            "Minutes away before it sleeps while its charger is out; empty uses the minutes above",
        ) {
            ui.NumberInput(
                bind: prefs.battery_suspend_minutes,
                label: "Minutes on the battery",
                min: 0.0,
                max: 240.0,
                step: 1.0,
            )
        }
        setting("Turn the screen off after", "Minutes away before the screen goes dark") {
            ui.NumberInput(
                bind: prefs.blank_minutes,
                label: "Minutes",
                min: 1.0,
                max: 240.0,
                step: 1.0,
            )
        }
        setting("Lock after", "Minutes away before the screen locks") {
            ui.NumberInput(
                bind: prefs.lock_minutes,
                label: "Minutes",
                min: 0.0,
                max: 240.0,
                step: 1.0,
            )
        }
        setting("Sleep locks the screen", "Ask for the person's key when it wakes") {
            ui.Toggle(bind: prefs.lock_on_sleep)
        }
    }
    ui.Section("Sleep in each mode") {
        ui.Text(
            "Minutes away before the device sleeps in a mode of its own; empty uses the minutes above.",
            role: .caption,
            secondary: true,
        )
        setting("Power saver", "Minutes away in power saver") {
            ui.NumberInput(
                bind: prefs.saver_suspend_minutes,
                label: "Minutes in power saver",
                min: 0.0,
                max: 240.0,
                step: 1.0,
            )
        }
        setting("Balanced", "Minutes away in balanced") {
            ui.NumberInput(
                bind: prefs.balanced_suspend_minutes,
                label: "Minutes in balanced",
                min: 0.0,
                max: 240.0,
                step: 1.0,
            )
        }
        setting("Performance", "Minutes away in performance") {
            ui.NumberInput(
                bind: prefs.performance_suspend_minutes,
                label: "Minutes in performance",
                min: 0.0,
                max: 240.0,
                step: 1.0,
            )
        }
    }
    ui.Section("What the device's buttons do") {
        setting("Closing the lid", "What happens when a laptop's lid is shut") {
            ui.EnumField(bind: prefs.lid_close_action, label: "Closing the lid")
        }
        setting("The power button", "What pressing it does") {
            ui.EnumField(bind: prefs.power_key_action, label: "The power button")
        }
        setting("When the battery is critical", "What the device does as it runs out") {
            ui.EnumField(bind: prefs.critical_action, label: "Critical battery action")
        }
    }
    ui.Section("When the battery is low") {
        setting("Say so at", "Per cent left before the device says the battery is low") {
            ui.NumberInput(
                bind: prefs.low_battery_percent,
                label: "Low battery percent",
                min: 1.0,
                max: 50.0,
                step: 1.0,
            )
        }
        setting("Act at", "Per cent left before the critical action") {
            ui.NumberInput(
                bind: prefs.critical_battery_percent,
                label: "Critical battery percent",
                min: 1.0,
                max: 30.0,
                step: 1.0,
            )
        }
    }
}

/// How the battery stands, in a word.
fn battery_words(state: Text) -> Text {
    if state == "charging" {
        "Charging"
    } else if state == "discharging" {
        "On the battery"
    } else if state == "empty" {
        "Empty"
    } else if state == "full" or state == "fully-charged" {
        "Full"
    } else if state == "pending-charge" {
        "About to charge"
    } else if state == "pending-discharge" { "About to drain" } else { "Plugged in" }
}

/// The last minute a reading was taken at, or zero without readings.
fn newest_minute(points: List<device.HistoryPoint>) -> Int {
    if let last = points.last() { last.unix_minute } else { 0 }
}

/// Where `minute` stands in the window from `from` to `from + span`, as a fraction of the chart's
/// width.
fn span_fraction(minute: Int, span: Int) -> Float {
    (minute.to_float() / span.to_float()).clamp(0.0, 1.0)
}

/// The battery's charge over the last day or week as a line, the area under it washed out
/// downward, and each stretch on the charger marked along the bottom.
fn history_shapes(points: List<device.HistoryPoint>, week: Bool) -> List<ui.Shape> {
    let span = if week { 10080 } else { 1440 }
    let end = newest_minute(points)
    let from = end - span
    let shown = points.filter(p -> p.unix_minute >= from and p.unix_minute <= end)
    if shown.len() < 2 {
        return [
            ui.Shape.text(
                x: 0.5,
                y: 0.5,
                text: "Not enough readings yet",
                role: .caption,
                ink: .muted,
                anchor: .middle,
            ),
        ]
    }
    let coords = shown.flat_map(
        p -> [
            span_fraction(p.unix_minute - from, span),
            0.88 - 0.76 * p.capacity.to_float() / 100.0,
        ],
    )
    let line = ui.Shape.path(
        points: coords,
        smooth: false,
        fill: .some(.accent),
        stroke: .some(.accent),
        wash: true,
    )
    let grid = [0.12, 0.31, 0.5, 0.69, 0.88]
        .map(
            y -> ui.Shape.line(x1: 0.0, y1: y, x2: 1.0, y2: y, stroke: .track, weight: .hairline),
        )
    let charged = shown
        .filter(p -> p.on_ac)
        .map(
            p -> ui.Shape.rect(
                x: span_fraction(p.unix_minute - from, span) - 0.004,
                y: 0.92,
                w: 0.008,
                h: 0.04,
                round: 0.5,
                fill: .some(.success),
            ),
        )
    let labels = [
        ui.Shape.text(
            x: 0.0,
            y: 0.04,
            text: if week { "A week ago" } else { "A day ago" },
            role: .caption,
            ink: .muted,
            anchor: .start,
        ),
        ui.Shape.text(x: 1.0, y: 0.04, text: "Now", role: .caption, ink: .muted, anchor: .end),
        ui.Shape.text(x: 0.0, y: 0.97, text: "100%", role: .caption, ink: .muted, anchor: .start),
    ]
    [grid, [line], charged, labels].flat_map(s -> s)
}

/// What the chart shows, as assistive technology hears it.
fn history_label(points: List<device.HistoryPoint>, week: Bool) -> Text {
    let end = newest_minute(points)
    let span = if week { 10080 } else { 1440 }
    let shown = points.filter(p -> p.unix_minute >= end - span)
    let over = if week { "week" } else { "day" }
    if let last = shown.last() {
        "The battery's charge over the last {over}: {last.capacity}% at the last reading"
    } else {
        "The battery's charge over the last {over}"
    }
}

/// The segment the mode picker shows for the person's mode.
fn mode_index(mode: Text) -> Int {
    if mode == "dark" { 2 } else if mode == "light" { 1 } else { 0 }
}

/// The segment the wallpaper's place picker shows: the desktop, the lock screen, or both.
fn wallpaper_target_index(target: Text) -> Int {
    if target == "lockscreen" { 1 } else if target == "both" { 2 } else { 0 }
}

/// What an app's own notification rule says of itself, in a word or two.
fn rule_words(rule: settings.AppNotificationRule) -> Text {
    if rule.muted_until != .none { "Muted for now" } else { "Its own rules" }
}

/// The window's pane a search page names: the settings index's own slugs, which are not all the
/// window's names ("external_accounts" is Accounts, "keyboard" is Keyboard and pointing).
fn pane_of_page(page: Text) -> Text {
    if page == "keyboard" or page == "pointing" {
        "input"
    } else if page == "keyboard_shortcuts" {
        "shortcuts"
    } else if page == "region_formats" {
        "region"
    } else if page == "time_date" {
        "time"
    } else if page == "default-apps" {
        "default_apps"
    } else if page == "external_accounts" or page == "billing" { "accounts" } else { page }
}

/// The window's pane a setting is on: the section its id begins with (`keyboard.layout` is on
/// Keyboard and pointing, `a11y.high_contrast` on Accessibility).
fn pane_of_setting(setting: Text) -> Text {
    let section = setting.split(".").at(0) ?? ""
    if section == "a11y" {
        "accessibility"
    } else if section == "audio" {
        "sound"
    } else if section == "dictionary" {
        "spelling"
    } else if section == "display" {
        "displays"
    } else if section == "keyboard" {
        "input"
    } else if section == "pointing" {
        "input"
    } else if section == "region" {
        "region"
    } else if section == "shortcuts" {
        "shortcuts"
    } else if section == "text" {
        "spelling"
    } else if section == "time" {
        "time"
    } else if section == "input" {
        "input"
    } else if section == "spelling" {
        "spelling"
    } else if section == "storage" {
        "storage"
    } else if section == "updates" {
        "updates"
    } else if section == "appearance" {
        "appearance"
    } else if section == "bluetooth" {
        "bluetooth"
    } else if section == "network" {
        "network"
    } else if section == "notifications" {
        "notifications"
    } else if section == "power" {
        "power"
    } else if section == "sound" { "sound" } else { "appearance" }
}

/// Show the pane a search page names, as the search's own row runs it from outside the window.
action open_pane(page: Text) (label: "Show a settings page") {
    let s = root Settings
    s.pane = pane_of_page(page)
}

/// Show the pane a setting is on, as search's own row for a setting runs it.
action show_setting(setting: Text) (label: "Show a setting") {
    let s = root Settings
    s.pane = pane_of_setting(setting)
}
