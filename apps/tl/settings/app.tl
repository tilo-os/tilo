app so.tilo.settings "Settings" (
    icon: .settings,
    category: .system,
    tagline: "This device, your workspace and the accounts they keep",
    keywords: ["settings", "preferences", "configuration", "einstellungen", "system", "options"],
    windows: .personal,
)

use so.tilo.settings.sections as settings
use so.tilo.file-explorer as files
use so.tilo.device as device
use tilo.actions as actions
use tilo.crash-reports as crashes
use tilo.models as models
use tilo.workspace as workspace
use tilo.accounts as accounts
use tilo.host-account as billing
use tilo.mcp-server as mcp
use so.tilo.session as session
use so.tilo.migrate.engine as engine

/// The window's root. Settings keeps nothing of its own here: every row below changes a
/// resource the session publishes — the person's appearance, notification, accessibility and
/// input settings, the device's sound, network, Bluetooth, displays and power — so a
/// change made here, in the shell's quick settings or by an agent is one change, with one history.
type Settings {
    /// The pane in front, by its own name: a search row, an agent or the sidebar's own presses
    /// set it, and the window shows that pane.
    pane: Text = "appearance"
}

/// One screen on the arrangement: whose it is, where it stands in the person's layout, and how
/// this device knows it.
type Screen {
    device: Text
    device_label: Text
    output: Text
    connector: Text
    name: Text
    x: Int
    y: Int
    own_x: Int
    own_y: Int
    width: Int
    height: Int
    local: Bool
    online: Bool
}

root Settings

default view settings_window for Settings

/// The window: the panes down the side under what they are for, the chosen one beside them.
view settings_window(s: Settings) {
    ui.NavShell(
        content_kind: .page,
        title: pane_title(s.pane),
        content_key: s.pane,
        sidebar_title: "Settings",
    ) {
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
                    // Printers, their queues and scanners have one place: Print Centre.
                    ui.NavItem("printers", "Printers", icon: .printer) {
                        on select: session.open_app(app: "so.tilo.print-centre")
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
                    ui.NavItem("shell", "Shell", icon: .app_window) {
                        on select: open_pane("shell")
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
                ui.NavSection("Workspace") {
                    ui.NavItem("content", "Content limits", icon: .database) {
                        on select: open_pane("content")
                    }
                }
                ui.NavSection("Accounts and access") {
                    ui.NavItem("accounts", "Accounts", icon: .mail) {
                        on select: open_pane("accounts")
                    }
                    ui.NavItem("billing", "Billing", icon: .landmark) {
                        on select: open_pane("billing")
                    }
                    ui.NavItem("users", "Users", icon: .user) {
                        on select: open_pane("users")
                    }
                    ui.NavItem("permissions", "Permissions", icon: .shield) {
                        on select: open_pane("permissions")
                    }
                    ui.NavItem("apps", "Apps", icon: .app_window) {
                        on select: open_pane("apps")
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
            // The Apps page is its own view: two sections, the second the list of every app's and
            // person's grant, which the page scrolls with the section over it and virtualises, so
            // ten thousand grants keep only the rows in view.
            if s.pane == "apps" {
                read_pane(of: workspace.access, what: pane_title(s.pane)) {
                    apps_access_pane()
                }
            } else {
                ui.Page {
                    let what = pane_title(s.pane)
                    if s.pane == "network" {
                        read_pane(of: device.network, what: what) {
                            network_pane()
                        }
                    } else if s.pane == "bluetooth" {
                        read_pane(of: device.bluetooth, what: what) {
                            bluetooth_pane()
                        }
                    } else if s.pane == "usage" {
                        read_pane(of: settings.network_usage, what: what) {
                            data_usage_pane()
                        }
                    } else if s.pane == "displays" {
                        read_pane(of: device.displays, what: what) {
                            displays_pane()
                        }
                    } else if s.pane == "sound" {
                        read_pane(of: device.sound, what: what) {
                            sound_pane()
                        }
                    } else if s.pane == "speakers" {
                        read_pane(of: device.speakers, what: what) {
                            speakers_page()
                        }
                    } else if s.pane == "power" {
                        read_pane(of: device.power, what: what) {
                            power_pane()
                        }
                    } else if s.pane == "drives" {
                        read_pane(of: device.drives, what: what) {
                            drives_pane()
                        }
                    } else if s.pane == "server" {
                        read_pane(of: device.server, what: what) {
                            device_server_pane()
                        }
                    } else if s.pane == "input" {
                        read_pane(of: settings.input_device, what: what) {
                            input_pane()
                        }
                    } else if s.pane == "accessibility" {
                        read_pane(of: settings.accessibility, what: what) {
                            accessibility_pane()
                        }
                    } else if s.pane == "notifications" {
                        read_pane(of: settings.notifications, what: what) {
                            notifications_pane()
                        }
                    } else if s.pane == "shortcuts" {
                        read_pane(of: settings.shortcuts_table, what: what) {
                            shortcuts_pane()
                        }
                    } else if s.pane == "region" {
                        read_pane(of: settings.locale, what: what) {
                            region_pane()
                        }
                    } else if s.pane == "spelling" {
                        read_pane(of: settings.text_assistance, what: what) {
                            spelling_pane()
                        }
                    } else if s.pane == "default_apps" {
                        read_pane(of: device.apps, what: what) {
                            default_apps_pane()
                        }
                    } else if s.pane == "time" {
                        read_pane(of: device.time, what: what) {
                            date_time_pane()
                        }
                    } else if s.pane == "content" {
                        read_pane(of: workspace.content, what: what) {
                            content_pane()
                        }
                    } else if s.pane == "accounts" {
                        read_pane(of: accounts.accounts, what: what) {
                            accounts_pane()
                        }
                    } else if s.pane == "billing" {
                        billing_pane()
                    } else if s.pane == "agent_models" {
                        read_pane(of: models.models, what: what) {
                            agent_models_pane()
                        }
                    } else if s.pane == "devices" {
                        read_pane(of: device.devices, what: what) {
                            devices_pane()
                        }
                    } else if s.pane == "continuity" {
                        read_pane(of: settings.continuity, what: what) {
                            continuity_pane()
                        }
                    } else if s.pane == "firmware" {
                        read_pane(of: device.firmware, what: what) {
                            firmware_pane()
                        }
                    } else if s.pane == "compatibility" {
                        read_pane(of: device.codec_pack, what: what) {
                            compatibility_pane()
                        }
                    } else if s.pane == "disk_unlock" {
                        read_pane(of: device.disk_unlock, what: what) {
                            disk_unlock_pane()
                        }
                    } else if s.pane == "storage" {
                        read_pane(of: device.storage, what: what) {
                            storage_pane()
                        }
                    } else if s.pane == "updates" {
                        read_pane(of: device.update, what: what) {
                            updates_pane()
                        }
                    } else if s.pane == "themes" {
                        read_pane(of: settings.appearance, what: what) {
                            themes_pane(s)
                        }
                    } else if s.pane == "shell" {
                        shell_pane()
                    } else if s.pane == "users" {
                        read_pane(of: device.users, what: what) {
                            users_pane(s)
                        }
                    } else if s.pane == "permissions" {
                        permissions_pane()
                    } else if s.pane == "about" {
                        read_pane(of: device.host_info, what: what) {
                            about_pane()
                        }
                    } else {
                        read_pane(of: settings.appearance, what: what) {
                            appearance_pane()
                        }
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
    } else if pane == "drives" {
        "Drives"
    } else if pane == "server" {
        "This device's server"
    } else if pane == "input" {
        "Keyboard and pointing"
    } else if pane == "themes" {
        "Themes"
    } else if pane == "shell" {
        "Shell"
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
    } else if pane == "content" {
        "Content limits"
    } else if pane == "accounts" {
        "Accounts"
    } else if pane == "billing" {
        "Billing"
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
    } else if pane == "permissions" {
        "Permissions"
    } else if pane == "apps" { "Apps" } else if pane == "about" { "About" } else { "Appearance" }
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

/// A pane over what the device or the session publishes: where that resource could not be read,
/// the pane says so in one row rather than showing the empty values its type starts with — a
/// battery at no per cent, a network that is off, no display at all. `what` is the pane's own
/// name, as its title has it.
template view read_pane<R>(of: R, what: Text, content: View) {
    std.readable(of: of, message: "{what} could not be read") {
        content
    }
}

/// The pictures Tilo ships, each a thumbnail with what it credits; choosing one makes it the
/// picture the desktop stands on, through the setting the person's devices read.
template view wallpaper_picker(a: settings.Appearance) {
    read_pane(of: device.wallpaper, what: "Wallpaper") {
        let wallpaper = device.wallpaper
        if wallpaper.catalogue.is_empty() {
            ui.Text(
                "The pictures Tilo ships are not installed on this computer.",
                role: .caption,
                secondary: true,
            )
        } else {
            ui.MediaGrid(size: .small) {
                for picture in wallpaper.catalogue {
                    ui.MediaCard(
                        title: picture.title,
                        picture: picture.thumbnail,
                        meta: [picture.author, picture.license],
                        aspect: .wide,
                        selected: shown_wallpaper(wallpaper) == picture.id,
                    ) {
                        on activate: settings.set_wallpaper(a, picture.id)
                    }
                }
            }
        }
    }
}

/// The picture this workspace shows: the one the person chose, or the one the image ships while
/// they have chosen none.
fn shown_wallpaper(wallpaper: device.Wallpaper) -> Text {
    if wallpaper.picture == "" { wallpaper.shipped } else { wallpaper.picture }
}

/// A small desktop drawn in `theme`, so a person sees what a theme looks like beside the one in
/// use; an empty `theme` draws the theme this device uses now.
template view theme_preview(caption: Text, theme: Text) {
    stack(gap: .tight) {
        ui.Text(caption, role: .caption, secondary: true)
        ui.Drawing(
            theme_preview_shapes(),
            meaning: .picture,
            label: .some(caption),
            width: .some(150.0),
            height: .some(94.0),
            theme: if theme == "" { .none } else { .some(theme) },
        )
    }
}

/// The shapes of a small desktop: its surface, a window with its sidebar, a card, a heading, a
/// line of words, a press, and what went well beside what failed.
fn theme_preview_shapes() -> List<ui.Shape> {
    [
        ui.Shape.rect(x: 0.0, y: 0.0, w: 1.0, h: 1.0, fill: .some(.base)),
        ui.Shape.rect(x: 0.06, y: 0.1, w: 0.88, h: 0.8, round: 0.05, fill: .some(.surface)),
        ui.Shape.rect(x: 0.06, y: 0.1, w: 0.24, h: 0.8, round: 0.05, fill: .some(.sidebar)),
        ui.Shape.text(x: 0.13, y: 0.2, text: "Aa", role: .title, ink: .ink, anchor: .start),
        ui.Shape.rect(x: 0.38, y: 0.18, w: 0.48, h: 0.26, round: 0.04, fill: .some(.track)),
        ui.Shape.text(x: 0.4, y: 0.56, text: "Text", role: .body, ink: .muted, anchor: .start),
        ui.Shape.rect(x: 0.4, y: 0.66, w: 0.2, h: 0.1, round: 0.5, fill: .some(.accent)),
        ui.Shape.ellipse(cx: 0.68, cy: 0.71, rx: 0.025, ry: 0.04, fill: .some(.success)),
        ui.Shape.ellipse(cx: 0.76, cy: 0.71, rx: 0.025, ry: 0.04, fill: .some(.danger)),
    ]
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
    ui.Section("Preview") {
        ui.Text(
            "A desktop drawn in the themes your modes show, and in the one this device uses now, which follows every change as you make it.",
            role: .caption,
            secondary: true,
        )
        row(gap: .related, align: .start, wrap: true) {
            theme_preview("Light mode", a.light_theme)
            theme_preview("Dark mode", a.dark_theme)
            theme_preview("In use now", "")
        }
    }
    ui.Section("Reading") {
        wallpaper_picker(a)
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
        // The hints are the person's own record, on every device of theirs: the shell writes
        // what they learn, and these change the same record through its actions.
        let hints = settings.control_hints
        setting("Show control hints", "A short tip beside a control, the first time it helps") {
            ui.Toggle(shown: hints.learned.enabled, label: "Show control hints") {
                on check(on): settings.show_control_hints(hints, enabled: on)
            }
        }
        setting("Tips you have seen", "Show every control hint again, as if new") {
            ui.Button("Show them again", icon: .rotate_ccw, role: .plain) {
                on press: settings.show_control_hints_again(hints)
            }
        }
    }
    ui.Section("Widgets") {
        widget_setting(
            .music,
            "Music",
            "What is playing on this device or another of yours, with its own controls",
        )
        widget_setting(.team, "People here", "Who else is in this workspace")
        widget_setting(.calendar, "Calendar", "Today, and the month around it")
        widget_setting(.system_monitor, "System load", "This machine's processors, memory and heat")
    }
}

/// One widget's choices: whether the shell keeps it, when it shows itself, and where it rests.
/// Every change is the settings' own action, so the shell, Settings, a script and an agent keep
/// the same record.
view widget_setting(kind: ShellWidget, name: Text, about: Text) {
    let key = shell_widget_key(kind)
    let prefs = widget_prefs_of(kind)
    ui.Text(name, role: .label, secondary: true)
    setting("Keep it on the shell", about) {
        ui.Toggle(shown: prefs.enabled, label: "Keep {name} on the shell") {
            on check(on): settings.set_widget_enabled(settings.widgets, widget: key, enabled: on)
        }
    }
    setting("When it shows", "Back after a pause, always, or only when you ask") {
        ui.ToggleGroup(selected: show_index(prefs.show), label: "When {name} shows") {
            ui.Segment("After a pause") {
                on press: settings.set_widget_show(settings.widgets, widget: key, show: "pause")
            }
            ui.Segment("Always") {
                on press: settings.set_widget_show(settings.widgets, widget: key, show: "always")
            }
            ui.Segment("Only when I ask") {
                on press: settings.set_widget_show(settings.widgets, widget: key, show: "ask")
            }
        }
    }
    setting(
        "Where it rests",
        "The corner or edge you left it at; a drag from its header moves it",
    ) {
        ui.Button("Back to its corner", icon: .undo_2, role: .plain) {
            on press: settings.place_widget(
                settings.widgets,
                widget: key,
                display: device.screen.output.to_text(),
                x: widget_home_x(kind),
                y: widget_home_y(kind),
            )
        }
    }
}

/// A widget's choices as the person's settings hold them.
fn widget_prefs_of(kind: ShellWidget) -> settings.WidgetPrefs {
    if kind == .music {
        settings.widgets.music
    } else if kind == .team {
        settings.widgets.team
    } else if kind == .calendar {
        settings.widgets.calendar
    } else { settings.widgets.system_monitor }
}

/// Which of the three show modes is chosen, in the order the group lists them.
fn show_index(show: settings.ShowMode) -> Int {
    if show == .pause { 0 } else if show == .always { 1 } else { 2 }
}

/// How far across a widget first rests on a display, as a fraction of the room it moves in.
fn widget_home_x(kind: ShellWidget) -> Float {
    if kind == .system_monitor { 0.0 } else { 1.0 }
}

/// How far down a widget first rests on a display, as a fraction of the room it moves in.
fn widget_home_y(kind: ShellWidget) -> Float {
    if kind == .music or kind == .system_monitor { 1.0 } else if kind == .team { 0.5 } else { 0.25 }
}

/// The widgets the shell can keep, as Settings lists them.
enum ShellWidget {
    /// Music
    music
    /// People here
    team
    /// Calendar
    calendar
    /// System load
    system_monitor
}

/// The key the person's settings keep a widget's choices under.
fn shell_widget_key(kind: ShellWidget) -> Text {
    if kind == .music {
        "music"
    } else if kind == .team {
        "team"
    } else if kind == .calendar { "calendar" } else { "system_monitor" }
}

/// One app's own notifications: which kinds arrive, where they show and whether it may reach
/// the person while the quiet hours hold. A kind it leaves unset follows the rules above, so a
/// press sets that one kind and keeps the rest; the last button gives them all back.
template view app_rules(
    app_id: Text,
    rule: settings.AppNotificationRule,
    banners: Bool,
    previews: Bool,
    sounds: Bool,
    badges: Bool,
    lock_screen: Bool,
    cross_device: Bool,
) {
    let its_banners = rule.banners ?? banners
    setting("Show banners", "Say what happened as it happens") {
        ui.Button(if its_banners { "On" } else { "Off" }, toggled: its_banners) {
            on press: settings.set_app_kinds(
                settings.notifications,
                app: app_id,
                banners: .some(if its_banners { false } else { true }),
                previews: rule.previews,
                sounds: rule.sounds,
                badges: rule.badges,
            )
        }
    }
    let its_previews = rule.previews ?? previews
    setting("Show what it says", "Let a banner carry the message it is about") {
        ui.Button(if its_previews { "On" } else { "Off" }, toggled: its_previews) {
            on press: settings.set_app_kinds(
                settings.notifications,
                app: app_id,
                banners: rule.banners,
                previews: .some(if its_previews { false } else { true }),
                sounds: rule.sounds,
                badges: rule.badges,
            )
        }
    }
    let its_sounds = rule.sounds ?? sounds
    setting("Play sounds", "A sound with what happens") {
        ui.Button(if its_sounds { "On" } else { "Off" }, toggled: its_sounds) {
            on press: settings.set_app_kinds(
                settings.notifications,
                app: app_id,
                banners: rule.banners,
                previews: rule.previews,
                sounds: .some(if its_sounds { false } else { true }),
                badges: rule.badges,
            )
        }
    }
    let its_badges = rule.badges ?? badges
    setting("Show counts", "A count on its icon for what it holds unread") {
        ui.Button(if its_badges { "On" } else { "Off" }, toggled: its_badges) {
            on press: settings.set_app_kinds(
                settings.notifications,
                app: app_id,
                banners: rule.banners,
                previews: rule.previews,
                sounds: rule.sounds,
                badges: .some(if its_badges { false } else { true }),
            )
        }
    }
    let its_lock_screen = rule.lock_screen ?? lock_screen
    setting("Show on the lock screen", "Let a banner arrive while the screen is locked") {
        ui.Button(if its_lock_screen { "On" } else { "Off" }, toggled: its_lock_screen) {
            on press: settings.set_app_reach(
                settings.notifications,
                app: app_id,
                lock_screen: .some(if its_lock_screen { false } else { true }),
                cross_device: rule.cross_device,
            )
        }
    }
    let its_cross_device = rule.cross_device ?? cross_device
    setting("Your other devices", "Let it reach this person's other devices") {
        ui.Button(if its_cross_device { "On" } else { "Off" }, toggled: its_cross_device) {
            on press: settings.set_app_reach(
                settings.notifications,
                app: app_id,
                lock_screen: rule.lock_screen,
                cross_device: .some(if its_cross_device { false } else { true }),
            )
        }
    }
    setting("During quiet hours", "Let it reach you while the quiet hours hold") {
        ui.Button(if rule.priority { "On" } else { "Off" }, toggled: rule.priority) {
            on press: settings.set_app_priority(
                settings.notifications,
                app: app_id,
                priority: not rule.priority,
                priority_contacts: rule.priority_contacts,
            )
        }
    }
    setting("A priority contact's message", "Let a message from a priority contact through") {
        ui.Button(
            if rule.priority_contacts { "On" } else { "Off" },
            toggled: rule.priority_contacts,
        ) {
            on press: settings.set_app_priority(
                settings.notifications,
                app: app_id,
                priority: rule.priority,
                priority_contacts: not rule.priority_contacts,
            )
        }
    }
    row(gap: .related, align: .center, wrap: true) {
        ui.Button("Follow the rules above again", size: .small) {
            on press: settings.clear_app(settings.notifications, app: app_id)
        }
    }
}

/// What this device says and shows when something happens: the banners, the sounds, and the
/// hour the person wants quiet.
view notifications_pane() {
    state chosen_app: Text = ""
    state duration: Text = "For 8 hours"
    state editing: Text = ""
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
                            ui.Button("Its rules", size: .small, toggled: editing == app_id) {
                                on press: set(editing, app_id)
                            }
                        }
                    }
                }
            }
        }
        // One app's rules at a time: a person with many apps still sees a short pane, and the
        // row they pressed says which app the rules below are.
        if let rule = n.per_app.get(editing) {
            ui.Text(editing, role: .title)
            // Other devices follow the compositor's own rule when an app sets none, and no
            // setting changes that rule yet: an app's rule only takes it away.
            app_rules(
                editing,
                rule,
                banners: n.show_banners,
                previews: n.show_previews,
                sounds: n.play_sounds,
                badges: n.show_badges,
                lock_screen: n.show_on_lock_screen,
                cross_device: true,
            )
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
            if chosen_app != "" and n.per_app.get(chosen_app) == .none {
                ui.Button("Add this app", size: .small) {
                    on press: settings.set_app_kinds(
                        settings.notifications,
                        app: chosen_app,
                        banners: .none,
                        previews: .none,
                        sounds: .none,
                        badges: .none,
                    )
                }
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

/// What the arrangement canvas leaves around the screens it draws, as a fraction of its box.
fn canvas_margin() -> Float {
    0.06
}

/// How much of the canvas box the screens take, as a fraction: what is left after the margin.
fn canvas_span() -> Float {
    1.0 - 2.0 * canvas_margin()
}

/// The size a mode shows, as a person reads it: `1920 × 1080`.
fn mode_size(mode: device.Mode) -> Text {
    "{mode.width} × {mode.height}"
}

/// A number to at most two decimals, as a person reads it: `60`, `59.94`.
fn short_number(value: Float) -> Text {
    let hundredths = (value * 100.0).round()
    let whole = hundredths / 100
    let part = (hundredths - whole * 100).abs()
    if part == 0 {
        "{whole}"
    } else if part % 10 == 0 {
        "{whole}.{part / 10}"
    } else if part < 10 { "{whole}.0{part}" } else { "{whole}.{part}" }
}

/// How fast a mode refreshes, as a person reads it: `60 Hz` or `59.94 Hz`.
fn mode_rate(mode: device.Mode) -> Text {
    "{short_number(mode.refresh_mhz.to_float() / 1000.0)} Hz"
}

/// The sizes a display offers, each once, in the order it offers them.
fn mode_sizes(modes: List<device.Mode>) -> List<Text> {
    modes.group(m -> mode_size(m)).map(g -> g.key)
}

/// The modes a display offers at `size`.
fn modes_at_size(modes: List<device.Mode>, size: Text) -> List<device.Mode> {
    modes.filter(m -> mode_size(m) == size)
}

/// The size a display shows now, empty while its mode is not known.
fn shown_size(display: device.Display) -> Text {
    if let mode = display.mode { mode_size(mode) } else { "" }
}

/// Which of a display's sizes it shows now, counted from zero.
fn size_index(display: device.Display) -> Int {
    mode_sizes(display.modes).index_of(shown_size(display)) ?? 0
}

/// Which of a display's rates at its size it shows now, counted from zero.
fn rate_index(display: device.Display) -> Int {
    modes_at_size(display.modes, shown_size(display))
        .map(m -> m.refresh_mhz)
        .index_of(display.mode?.refresh_mhz ?? 0)
        ?? 0
}

/// How many physical pixels a display draws a logical pixel with, as a person reads it.
fn scale_label(scale: Float) -> Text {
    "{short_number(scale * 100.0)}%"
}

/// Which of the scales the pane offers `scale` is, counted from zero.
fn scale_index(scale: Float) -> Int {
    [1.0, 1.25, 1.5, 2.0].index_of(scale) ?? 0
}

/// Which way a display's picture is turned, counted from zero: upright, 90, 180 or 270.
fn turn_index(transform: Text) -> Int {
    ["normal", "rotate-90", "rotate-180", "rotate-270"].index_of(transform) ?? 0
}

/// Why a display cannot show HDR, as a person reads it.
fn hdr_unavailable_words(display: device.Display) -> Text {
    if not display.sink_hdr {
        "Not supported by this display"
    } else if let reason = display.hdr_unavailable {
        if reason == "driver-unsupported" {
            "The graphics driver cannot send an HDR signal to this display"
        } else if reason == "format-unsupported" {
            "The graphics card could not show a 10-bit picture on this display"
        } else {
            "The graphics driver refused to turn on HDR, so this display stays in SDR"
        }
    } else { "Not supported by this display" }
}

/// Why a display cannot vary its refresh rate, as a person reads it.
fn vrr_unavailable_words(display: device.Display) -> Text {
    if not display.sink_vrr {
        "Not supported by this display"
    } else if display.vrr_unavailable == .some("driver-rejected") {
        "The graphics driver refused to turn it on"
    } else {
        "The graphics driver does not offer it for this display"
    }
}

/// Which of a display's colour profiles it shows, counted from zero: the first choice is the
/// system default, so a profile is one past its place in the list.
fn colour_profile_index(display: device.Display) -> Int {
    if let current = display.colour_profile {
        (display.colour_profiles.map(p -> p.key).index_of(current) ?? -1) + 1
    } else { 0 }
}

/// Whether a screen a device reported shows anything.
fn showing(output: device.DeviceDisplayView) -> Bool {
    output.connected and output.enabled and output.width > 0 and output.height > 0
}

/// The corners of `outputs` in their device's own layout: its left, top, width and height.
fn output_bounds(outputs: List<device.DeviceDisplayView>) -> List<Int> {
    let left = outputs.map(o -> o.x).sort_by(x -> x).first() ?? 0
    let top = outputs.map(o -> o.y).sort_by(y -> y).first() ?? 0
    let right = outputs.map(o -> o.x + o.width).sort_by(x -> x, desc).first() ?? 0
    let bottom = outputs.map(o -> o.y + o.height).sort_by(y -> y, desc).first() ?? 0
    [left, top, (right - left).max(1), (bottom - top).max(1)]
}

/// The screen of `device` whose place decides where its group stands: the first one the person
/// placed, in the order the session lays the person's screens out.
fn anchor_of(device: Text, outputs: List<Text>) -> Option<settings.PlacedDisplay> {
    settings.display_person.placement
        .filter(entry -> entry.device == device and entry.placed)
        .filter(entry -> outputs.contains(entry.output))
        .sort_by(entry -> entry.output)
        .sort_by(entry -> entry.order)
        .first()
}

/// Where this device calls `output`: the connector its own screens read it by, empty for another
/// device's screen, which this device names only by the id the person's layout places it by. The
/// two readings name a screen their own ways, so the rectangle it shows stands for it here.
fn connector_of(one: device.PersonalDeviceView, output: device.DeviceDisplayView) -> Text {
    if not one.local {
        ""
    } else {
        device.displays.displays
            .find(
                d -> d.x == output.x
                    and d.y == output.y
                    and d.width == output.width
                    and d.height == output.height,
            )
            ?.connector
            ?? ""
    }
}

/// This device's screens as the arrangement draws them where no other device is known: where they
/// stand here is where they stand in the person's layout.
fn local_screens() -> List<Screen> {
    device.displays.displays.map(
        d -> Screen(
            device: "",
            device_label: "",
            output: d.connector,
            connector: d.connector,
            name: d.name,
            x: d.x,
            y: d.y,
            own_x: d.x,
            own_y: d.y,
            width: d.width,
            height: d.height,
            local: true,
            online: true,
        ),
    )
}

/// The person's screens `placed` and `moved` together, as the canvas reads them: left, top, width
/// and height.
fn union_bounds(placed: List<Int>, moved: List<Int>) -> List<Int> {
    if placed.is_empty() {
        moved
    } else {
        let left = (placed.at(0) ?? 0).min(moved.at(0) ?? 0)
        let top = (placed.at(1) ?? 0).min(moved.at(1) ?? 0)
        let right = ((placed.at(0) ?? 0) + (placed.at(2) ?? 0))
            .max((moved.at(0) ?? 0) + (moved.at(2) ?? 0))
        let bottom = ((placed.at(1) ?? 0) + (placed.at(3) ?? 0))
            .max((moved.at(1) ?? 0) + (moved.at(3) ?? 0))
        [left, top, right - left, bottom - top]
    }
}

/// One device's screens, its group standing `offset` from where its own layout puts them.
fn device_screens(
    one: device.PersonalDeviceView,
    outputs: List<device.DeviceDisplayView>,
    offset: List<Int>,
) -> List<Screen> {
    let across = offset.at(0) ?? 0
    let down = offset.at(1) ?? 0
    outputs.map(
        output -> Screen(
            device: one.id,
            device_label: one.label,
            output: output.output,
            connector: connector_of(one, output),
            name: output.name,
            x: output.x + across,
            y: output.y + down,
            own_x: output.x,
            own_y: output.y,
            width: output.width,
            height: output.height,
            local: one.local,
            online: one.online,
        ),
    )
}

/// Where one device's group of screens stands, `offset` from its own layout: left, top, width and
/// height.
fn group_bounds(outputs: List<device.DeviceDisplayView>, offset: List<Int>) -> List<Int> {
    let bounds = output_bounds(outputs)
    [
        (bounds.at(0) ?? 0) + (offset.at(0) ?? 0),
        (bounds.at(1) ?? 0) + (offset.at(1) ?? 0),
        bounds.at(2) ?? 1,
        bounds.at(3) ?? 1,
    ]
}

/// Every screen of the person's, as their layout places them: a device's own arrangement keeps its
/// screens together, and the person's placement decides where each device's group stands in their
/// one space. The session lays the groups out the same way, and the pointer crosses by that
/// layout, so a screen dragged here and there is one arrangement.
fn arranged_screens() -> List<Screen> {
    let listed = device.devices.devices
    if listed.is_empty() {
        return local_screens()
    }
    let screens: List<Screen> = []
    let placed: List<Int> = []
    let waiting: List<device.PersonalDeviceView> = []
    for one in listed.sort_by(d -> d.id) {
        let outputs = one.displays.filter(o -> showing(o))
        if not outputs.is_empty() {
            if let entry = anchor_of(one.id, outputs.map(o -> o.output)) {
                if let here = outputs.find(o -> o.output == entry.output) {
                    let offset = [entry.x - here.x, entry.y - here.y]
                    screens = screens + device_screens(one, outputs, offset)
                    placed = union_bounds(placed, group_bounds(outputs, offset))
                }
            } else {
                waiting = waiting + [one]
            }
        }
    }
    // A device the person has not placed lines up to the right of the groups placed before it, in
    // the order of the devices' ids, as the session lines them up.
    for one in waiting {
        let outputs = one.displays.filter(o -> showing(o))
        let bounds = output_bounds(outputs)
        let offset = if placed.is_empty() {
            [0 - (bounds.at(0) ?? 0), 0 - (bounds.at(1) ?? 0)]
        } else {
            [
                (placed.at(0) ?? 0) + (placed.at(2) ?? 0) - (bounds.at(0) ?? 0),
                (placed.at(1) ?? 0) - (bounds.at(1) ?? 0),
            ]
        }
        screens = screens + device_screens(one, outputs, offset)
        placed = union_bounds(placed, group_bounds(outputs, offset))
    }
    if not screens.any(s -> s.local) {
        // The record has not said what this device shows yet: draw its screens where they stand.
        screens = screens + local_screens()
    }
    screens.sort_by(s -> s.y).sort_by(s -> s.x)
}

/// The person's devices that show but have not said what their screens are: nothing to arrange
/// until they do.
fn silent_devices() -> List<Text> {
    device.devices.devices
        .filter(d -> d.display and d.displays.is_empty() and not d.local)
        .map(d -> d.label)
}

/// Where one screen of the person's stands, without its name: the device it belongs to, and the
/// size it shows.
fn screen_place_words(screen: Screen) -> Text {
    "{screen.device_label}, {screen.width} × {screen.height}"
}

/// The left of the leftmost screen, in shared pixels.
fn left_of(screens: List<Screen>) -> Int {
    screens.map(s -> s.x).sort_by(x -> x).first() ?? 0
}

/// The top of the highest screen, in shared pixels.
fn top_of(screens: List<Screen>) -> Int {
    screens.map(s -> s.y).sort_by(y -> y).first() ?? 0
}

/// The right of the rightmost screen, in shared pixels.
fn right_of(screens: List<Screen>) -> Int {
    screens.map(s -> s.x + s.width).sort_by(x -> x, desc).first() ?? 0
}

/// The bottom of the lowest screen, in shared pixels.
fn bottom_of(screens: List<Screen>) -> Int {
    screens.map(s -> s.y + s.height).sort_by(y -> y, desc).first() ?? 0
}

/// The person's desktop the screens stand on, as the canvas draws it: its left and top, and how
/// wide and tall it is — never nothing, so a single screen still fills the box.
fn screen_bounds(screens: List<Screen>) -> List<Float> {
    [
        left_of(screens).to_float(),
        top_of(screens).to_float(),
        (right_of(screens) - left_of(screens)).max(1).to_float(),
        (bottom_of(screens) - top_of(screens)).max(1).to_float(),
    ]
}

/// Keep what one of this device's screens says on the arrangement: its name, and the size it shows.
fn screen_words(display: device.Display) -> Text {
    let primary = if display.primary { ", the primary screen" } else { "" }
    "{display.name}, {display.width} × {display.height}{primary}"
}

/// What a person calls one screen of their layout: a screen of this device by its own name, a
/// screen of another by that name and the device it is on.
fn screen_name(screen: Screen) -> Text {
    if screen.local { screen.name } else { "{screen.name} on {screen.device_label}" }
}

/// Whether `a0..a1` and `b0..b1` overlap.
fn spans(a0: Int, a1: Int, b0: Int, b1: Int) -> Bool {
    a0 < b1 and b0 < a1
}

/// Whether two screens meet along an edge: one's right at the other's left, or one's bottom at
/// the other's top, with the other axis overlapping. The pointer crosses only there.
fn meets(one: Screen, other: Screen) -> Bool {
    (one.x + one.width == other.x or other.x + other.width == one.x)
        and spans(one.y, one.y + one.height, other.y, other.y + other.height)
        or (one.y + one.height == other.y or other.y + other.height == one.y)
            and spans(one.x, one.x + one.width, other.x, other.x + other.width)
}

/// Whether a screen meets a screen of another device, so the pointer reaches it.
fn screen_reachable(screen: Screen, screens: List<Screen>) -> Bool {
    screens.any(s -> s.device != screen.device and meets(screen, s))
}

/// Another device's screen and how the pointer reaches it.
fn screen_status(screen: Screen, screens: List<Screen>) -> Text {
    if not screen.online {
        "Offline"
    } else if not screen_reachable(screen, screens) {
        "Online, away from your other screens"
    } else { "Online" }
}

/// The layout as assistive technology hears it: every screen of every device, and where it
/// stands.
fn arrangement_label(screens: List<Screen>) -> Text {
    let places = screens.map(s -> screen_name(s))
    "Your screens, left to right and top to bottom: {places.join("; ")}"
}

/// Where a screen stands on the canvas, as fractions of its box: `[x, y, w, h]`. While it is
/// dragged from `start` to `drag`, the screen under the pointer moves by that much.
fn screen_box(
    screen: Screen,
    index: Int,
    picked: Int,
    moved: List<Float>,
    bounds: List<Float>,
) -> List<Float> {
    let move_x = if index == picked { moved.at(0) ?? 0.0 } else { 0.0 }
    let move_y = if index == picked { moved.at(1) ?? 0.0 } else { 0.0 }
    let x = (screen.x.to_float() - (bounds.at(0) ?? 0.0)) / (bounds.at(2) ?? 1.0)
    let y = (screen.y.to_float() - (bounds.at(1) ?? 0.0)) / (bounds.at(3) ?? 1.0)
    let w = screen.width.to_float() / (bounds.at(2) ?? 1.0)
    let h = screen.height.to_float() / (bounds.at(3) ?? 1.0)
    [
        canvas_margin() + (x + move_x) * canvas_span(),
        canvas_margin() + (y + move_y) * canvas_span(),
        w * canvas_span(),
        h * canvas_span(),
    ]
}

/// One screen on the arrangement: its rectangle, and its name over it. The screen picked stands
/// out from the others.
fn screen_shapes(screen: Screen, box: List<Float>, picked: Bool) -> List<ui.Shape> {
    let x = box.at(0) ?? canvas_margin()
    let y = box.at(1) ?? canvas_margin()
    let w = box.at(2) ?? canvas_span()
    let h = box.at(3) ?? canvas_span()
    [
        ui.Shape.rect(
            x: x,
            y: y,
            w: w,
            h: h,
            round: 0.02,
            fill: .some(.track),
            stroke: .some(if picked { .accent } else { .muted }),
            weight: if picked { .strong } else { .hairline },
        ),
        ui.Shape.text(
            x: x + w / 2.0,
            y: y + h / 2.0,
            text: screen_name(screen),
            role: .caption,
            ink: .ink,
            anchor: .middle,
        ),
    ]
}

/// The arrangement's shapes: every screen where it stands, the dragged one where the pointer is.
fn arrangement_shapes(
    screens: List<Screen>,
    picked: Int,
    start: List<Float>,
    drag: List<Float>,
) -> List<ui.Shape> {
    let bounds = screen_bounds(screens)
    let moved = [
        (drag.at(0) ?? 0.0) - (start.at(0) ?? 0.0),
        (drag.at(1) ?? 0.0) - (start.at(1) ?? 0.0),
    ]
    screens.flat_map(
        (screen, i) -> screen_shapes(
            screen,
            screen_box(screen, i, picked, moved, bounds),
            i == picked,
        ),
    )
}

/// Move the screen picked to where the pointer let it go. This device's own screens keep the
/// arrangement their desktop gives them; another device's screens move together by the person's
/// shared layout, this device's too when it has only one. A drag too small to mean anything is
/// not a change.
action move_screen(screens: List<Screen>, picked: Int, start: List<Float>, let_go: List<Float>) {
    if let chosen = screens.at(picked) {
        let across = (let_go.at(0) ?? 0.0) - (start.at(0) ?? 0.0)
        let down = (let_go.at(1) ?? 0.0) - (start.at(1) ?? 0.0)
        if across.abs() + down.abs() > 0.004 {
            let bounds = screen_bounds(screens)
            let span_x = (bounds.at(2) ?? 1.0) / canvas_span()
            let span_y = (bounds.at(3) ?? 1.0) / canvas_span()
            let dx = (across * span_x).round()
            let dy = (down * span_y).round()
            let here = screens.filter(s -> s.local)
            if chosen.local and here.len() > 1 {
                if chosen.connector != "" {
                    device.arrange_displays(
                        device.displays,
                        chosen.connector,
                        (chosen.own_x + dx) / 8 * 8,
                        (chosen.own_y + dy) / 8 * 8,
                    )
                }
            } else if chosen.device != "" and (not chosen.local or screens.any(s -> not s.local)) {
                // The device's screens move together: each says where it stands in the person's
                // shared layout, so the layout keeps them together.
                for screen in screens.filter(s -> s.device == chosen.device) {
                    settings.place_display(
                        settings.display_person,
                        screen.device,
                        screen.output,
                        screen.x + dx / 8 * 8,
                        screen.y + dy / 8 * 8,
                        .none,
                        .some(true),
                    )
                }
            }
        }
    }
}

/// Set how bright ordinary content looks in HDR, from the slider's value in cd/m².
action set_sdr_brightness(displays: device.Displays, display: Text, nits: Float) {
    device.set_display_sdr_brightness(displays, display, nits.round())
}

/// The person's screens: how they are arranged across their devices, what each one shows, and
/// the warmth of the night light.
view displays_pane() {
    let d = settings.display
    let night = settings.display_person
    let screens = device.displays
    let arranged = arranged_screens()
    let silent = silent_devices()
    let change = settings.display_change
    state picked: Int = 0
    state start: List<Float> = []
    state drag: List<Float> = []
    ui.Section("Arrangement") {
        ui.Text(
            "Your screens across your devices. Drag one to where it stands: this device keeps its own screens' arrangement, and another device's screens move together.",
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
        if not arranged.is_empty() {
            ui.Drawing(
                arrangement_shapes(arranged, picked, start, drag),
                meaning: .picture,
                label: arrangement_label(arranged),
                height: .some(240.0),
            ) {
                on press(at: Int): set(picked, (at / 2).min(arranged.len() - 1))
                on drag_start(x: Float, y: Float): set(start, [x, y]).then(set(drag, [x, y]))
                on drag_at(at: List<Float>): set(drag, at)
                on release_at(at: List<Float>): move_screen(arranged, picked, start, at)
            }
        }
        if not silent.is_empty() {
            ui.Text(
                "A device has not said what its screens are yet.",
                role: .caption,
                secondary: true,
            )
            for one in silent {
                ui.ListRow(one, icon: .monitor)
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Number the screens", icon: .layout_grid) {
                on press: device.identify_displays(device.displays)
            }
            ui.Button("The display sheet", icon: .sliders_horizontal) {
                on press: device.open_sheet(device.displays)
            }
        }
    }
    if let deadline = change.deadline_unix_ms {
        ui.Section("A change being tried") {
            ui.Text(
                "The desktop is trying new display settings. Keep them, or let the previous ones come back.",
                role: .body,
            )
            if let said = change.message {
                ui.StatusLine(said, status: .warning)
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Keep these settings", role: .primary) {
                    on press: device.keep_display_change(device.displays, screens.pending)
                }
                ui.Button("Put them back") {
                    on press: device.revert_display_change(device.displays, screens.pending)
                }
            }
        }
    }
    if let screen = arranged.at(picked) {
        if let chosen = screens.displays.find(d -> screen.local and d.connector == screen.connector) {
            ui.Section("The screen picked") {
                ui.ListRow(chosen.name, secondary: screen_words(chosen), icon: .monitor)
                if not chosen.modes.is_empty() {
                    setting("Resolution", "How many pixels it shows") {
                        ui.ToggleGroup(selected: .some(size_index(chosen)), caption: "Resolution") {
                            for size in mode_sizes(chosen.modes) {
                                ui.Segment(size) {
                                    on press: device.set_display_mode(
                                        device.displays,
                                        chosen.connector,
                                        modes_at_size(chosen.modes, size).first()?.width ?? 0,
                                        modes_at_size(chosen.modes, size).first()?.height ?? 0,
                                        refresh_mhz: .none,
                                    )
                                }
                            }
                        }
                    }
                    setting("Refresh rate", "How often it draws a new picture") {
                        ui.ToggleGroup(
                            selected: .some(rate_index(chosen)),
                            caption: "Refresh rate",
                        ) {
                            for mode in modes_at_size(chosen.modes, shown_size(chosen)) {
                                ui.Segment(mode_rate(mode)) {
                                    on press: device.set_display_mode(
                                        device.displays,
                                        chosen.connector,
                                        mode.width,
                                        mode.height,
                                        refresh_mhz: .some(mode.refresh_mhz),
                                    )
                                }
                            }
                        }
                    }
                }
                setting("Scale", "How large it draws what it shows") {
                    ui.ToggleGroup(selected: .some(scale_index(chosen.scale)), caption: "Scale") {
                        ui.Segment("100%") {
                            on press: device.set_display_scale(
                                device.displays,
                                chosen.connector,
                                1.0,
                            )
                        }
                        ui.Segment("125%") {
                            on press: device.set_display_scale(
                                device.displays,
                                chosen.connector,
                                1.25,
                            )
                        }
                        ui.Segment("150%") {
                            on press: device.set_display_scale(
                                device.displays,
                                chosen.connector,
                                1.5,
                            )
                        }
                        ui.Segment("200%") {
                            on press: device.set_display_scale(
                                device.displays,
                                chosen.connector,
                                2.0,
                            )
                        }
                    }
                }
                setting("Rotation", "Which way its picture is turned") {
                    ui.ToggleGroup(
                        selected: .some(turn_index(chosen.transform)),
                        caption: "Rotation",
                    ) {
                        ui.Segment("Upright") {
                            on press: device.rotate_display(
                                device.displays,
                                chosen.connector,
                                "normal",
                            )
                        }
                        ui.Segment("90°") {
                            on press: device.rotate_display(
                                device.displays,
                                chosen.connector,
                                "rotate-90",
                            )
                        }
                        ui.Segment("180°") {
                            on press: device.rotate_display(
                                device.displays,
                                chosen.connector,
                                "rotate-180",
                            )
                        }
                        ui.Segment("270°") {
                            on press: device.rotate_display(
                                device.displays,
                                chosen.connector,
                                "rotate-270",
                            )
                        }
                    }
                }
                if chosen.hdr {
                    setting(
                        "HDR",
                        "Brighter highlights and richer colour for HDR games, video and photos",
                    ) {
                        ui.Toggle(shown: chosen.hdr_on, label: "HDR") {
                            on check(on): device.set_display_hdr(
                                device.displays,
                                chosen.connector,
                                on,
                            )
                        }
                    }
                    setting(
                        "SDR content brightness",
                        "How bright everything that is not HDR looks while HDR is on",
                    ) {
                        ui.Slider(
                            value: chosen.sdr_brightness_nits.to_float(),
                            label: "SDR content brightness",
                            min: 80.0,
                            max: 480.0,
                            step: 1.0,
                            shows_value: true,
                        ) {
                            on change(v): set_sdr_brightness(device.displays, chosen.connector, v)
                        }
                    }
                } else {
                    ui.ListRow("HDR", secondary: hdr_unavailable_words(chosen))
                }
                if chosen.vrr {
                    setting(
                        "Variable refresh rate",
                        "Turns on by itself for fullscreen games and video",
                    ) {
                        ui.Toggle(shown: chosen.vrr_on, label: "Variable refresh rate") {
                            on check(on): device.set_display_vrr(
                                device.displays,
                                chosen.connector,
                                on,
                            )
                        }
                    }
                } else {
                    ui.ListRow("Variable refresh rate", secondary: vrr_unavailable_words(chosen))
                }
                if chosen.colour_profiles.is_empty() {
                    ui.ListRow("Colour profile", secondary: "System default")
                } else {
                    setting("Colour profile", "The colours it shows, as the screen was measured") {
                        ui.ToggleGroup(
                            selected: .some(colour_profile_index(chosen)),
                            caption: "Colour profile",
                        ) {
                            ui.Segment("System default") {
                                on press: device.set_display_colour_profile(
                                    device.displays,
                                    chosen.connector,
                                    .none,
                                )
                            }
                            for profile in chosen.colour_profiles {
                                ui.Segment(profile.label) {
                                    on press: device.set_display_colour_profile(
                                        device.displays,
                                        chosen.connector,
                                        .some(profile.key),
                                    )
                                }
                            }
                        }
                    }
                }
                if not chosen.primary {
                    row(gap: .related, align: .center, wrap: true) {
                        ui.Button("Make it the primary screen", icon: .monitor) {
                            on press: device.make_display_primary(device.displays, chosen.connector)
                        }
                    }
                }
            }
        } else if not screen.local {
            ui.Section("Another device's screen") {
                ui.ListRow(
                    screen_name(screen),
                    secondary: screen_status(screen, arranged),
                    icon: .monitor,
                )
            }
        }
    }
    ui.Section("This device") {
        setting("Automatic brightness", "Follow the light in the room") {
            ui.Toggle(bind: d.auto_brightness)
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
    if not screens.trusted_tools.is_empty() {
        ui.Section("Display tools") {
            ui.Text(
                "These programs can make risky display changes without asking you to keep them.",
                role: .caption,
                secondary: true,
            )
            for tool in screens.trusted_tools {
                ui.ListRow(tool.name, icon: .shield) {
                    trailing: {
                        ui.Button("Ask again") {
                            on press: device.forget_display_tool(device.displays, tool.executable)
                        }
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
                on activate: device
                    .route_output(device.sound, node: output.node)
                    .then(
                        settings.remember_output_device(
                            settings.audio_device,
                            device: output.label,
                        ),
                    )
                trailing: {
                    ui.Button(
                        "Play a test sound on {output.label}",
                        icon: .volume_2,
                        icon_only: true,
                        size: .small,
                        role: .plain,
                    ) {
                        on press: device.test_output(device.sound, node: output.node)
                    }
                }
            }
        }
        if let output = sound.outputs.find(x -> x.chosen) {
            if output.channels >= 2 {
                setting(
                    "Speaker test",
                    "A short, quiet tone in one ear, so you can hear which side it is",
                ) {
                    row(gap: .related, align: .center, wrap: true) {
                        ui.Button("Left", size: .small) {
                            on press: device.test_output(
                                device.sound,
                                node: output.node,
                                channel: "left",
                            )
                        }
                        ui.Button("Right", size: .small) {
                            on press: device.test_output(
                                device.sound,
                                node: output.node,
                                channel: "right",
                            )
                        }
                    }
                }
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
                on activate: device
                    .select_input(device.sound, node: input.node)
                    .then(
                        settings.remember_input_device(settings.audio_device, device: input.label),
                    )
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
        if prefs.enabled {
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
            if not sound.themes.is_empty() {
                setting("Theme", "Which set of sounds they come from") {
                    ui.Dropdown(
                        value: sound_theme_name(prefs.theme, sound.themes),
                        options: sound.themes.map(t -> t.name),
                        caption: "Sound theme",
                    ) {
                        on change(name): settings.set_system_sound_theme(audio, theme: name)
                    }
                }
            }
            setting("Notifications", "A sound when a notification arrives") {
                ui.Toggle(bind: prefs.events.notification)
            }
            setting("New messages", "A sound when a message arrives") {
                ui.Toggle(bind: prefs.events.message)
            }
            setting("Volume change feedback", "A tick as the volume changes") {
                ui.Toggle(bind: prefs.events.volume_change)
            }
            setting("Screenshot shutter", "A shutter sound when a screenshot is taken") {
                ui.Toggle(bind: prefs.events.screenshot)
            }
            setting("Device connected", "A sound when a device is plugged in") {
                ui.Toggle(bind: prefs.events.device_connected)
            }
            setting("Device disconnected", "A sound when a device is unplugged") {
                ui.Toggle(bind: prefs.events.device_disconnected)
            }
            setting("Low battery", "A warning sound when the battery runs low") {
                ui.Toggle(bind: prefs.events.low_battery)
            }
            setting("Errors", "A sound when something fails") {
                ui.Toggle(bind: prefs.events.error)
            }
        }
    }
}

/// The sound theme system sounds come from, as the theme picker shows it: the installed theme's
/// own name, or the freedesktop theme's while nothing is chosen.
fn sound_theme_name(chosen: Option<Text>, themes: List<device.SoundTheme>) -> Text {
    let held = chosen ?? "freedesktop"
    if let known = themes.find(t -> t.name == held or t.id == held) { known.name } else { held }
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
    if rule.muted_until != .none {
        "Muted for now, with its own rules"
    } else if rule.priority or rule.priority_contacts {
        "Its own rules, reaching through the quiet hours"
    } else { "Its own rules" }
}

/// The window's pane a search page names: the settings index's own slugs, which are not all the
/// window's names ("external_accounts" is Accounts, "keyboard" is Keyboard and pointing,
/// and the person's card, a device's own page and adding a device all open Devices and access
/// as "devices_access" or "devices_sync").
fn pane_of_page(page: Text) -> Text {
    if page == "devices_access" or page == "devices_sync" {
        "devices"
    } else if page == "keyboard" or page == "pointing" {
        "input"
    } else if page == "keyboard_shortcuts" {
        "shortcuts"
    } else if page == "region_formats" {
        "region"
    } else if page == "time_date" {
        "time"
    } else if page == "default-apps" {
        "default_apps"
    } else if page == "external_accounts" { "accounts" } else { page }
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
