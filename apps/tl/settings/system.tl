/// What this machine runs, what update waits for a restart and what it can go back to.
view updates_pane() {
    let u = device.update
    let prefs = settings.updates
    ui.Section("This machine") {
        if let booted = u.booted {
            let called = booted.version ?? booted.image ?? ""
            ui.PropertyList {
                if called != "" {
                    ui.Property("Version", called)
                }
                if let built = booted.built {
                    ui.Property("Built", built)
                }
            }
        } else {
            ui.Text(
                "This machine's system details are kept by whoever administers it.",
                role: .caption,
                secondary: true,
            )
        }
    }
    if let staged = u.staged {
        ui.Section("Next start") {
            let called = staged.version ?? staged.image ?? ""
            ui.PropertyList {
                if called != "" {
                    ui.Property("Version", called)
                }
                if let built = staged.built {
                    ui.Property("Built", built)
                }
            }
            if u.restart_needed {
                ui.Button("Restart to finish the update", icon: .rotate_ccw, role: .primary) {
                    on press: device.restart_to_update(device.update)
                }
            } else if u.applied {
                ui.Text("It already runs.", role: .caption, secondary: true)
            }
        }
    }
    if u.desktop_pending {
        ui.Section("The desktop") {
            ui.Text(
                "Part of the update waits for the desktop to start again.",
                role: .caption,
                secondary: true,
            )
            ui.Button("Restart the desktop", icon: .rotate_ccw) {
                on press: device.restart_desktop(device.update)
            }
        }
    }
    ui.Section("Updating") {
        if u.updating {
            ui.StatusLine("Fetching and applying the newest system", status: .active)
        } else {
            ui.Button("Update now", icon: .download, role: .primary) {
                on press: device.update_now(device.update)
            }
        }
        setting("Update by itself", "Fetch and apply a new system while the machine is idle") {
            ui.Button(if prefs.auto { "On" } else { "Off" }, toggled: prefs.auto) {
                on press: settings.record_automatic_updates(settings.updates, on: not prefs.auto)
            }
        }
        if prefs.last_checked != "" {
            ui.Text("Last checked: {prefs.last_checked}", role: .caption, secondary: true)
        }
    }
    if let failed = u.failed {
        ui.Section("Taken back") {
            ui.StatusLine(failed, status: .warning)
        }
    }
    if let firmware = u.firmware {
        ui.Section("Firmware") {
            ui.Text(
                "{firmware} firmware updates are offered for this machine's devices.",
                role: .body,
            )
        }
    }
    if not u.fell_back.is_empty() {
        ui.Section("Older systems") {
            for older in u.fell_back {
                ui.ListRow(
                    if let version = older.version { version } else { older.id },
                    secondary: "Did not start; this machine started the one before",
                    icon: .history,
                )
            }
        }
    }
    if let problem = u.problem {
        ui.StatusLine(problem, status: .error)
    }
    ui.Section("Going back") {
        if let rollback = u.rollback {
            let called = rollback.version ?? rollback.image ?? "The system before"
            ui.ListRow(called, secondary: "What this machine can go back to", icon: .history) {
                trailing: {
                    ui.Button("Go back and restart", role: .destructive) {
                        on press: device.rollback(device.update)
                    }
                }
            }
        } else {
            ui.Text("There is nothing to go back to.", role: .caption, secondary: true)
        }
    }
}

/// The person's own devices, which of them is this one, and the colour they show as.
view devices_pane() {
    let listed = device.devices
    let me = settings.identity
    ui.Section("Your devices") {
        if listed.devices.is_empty() {
            ui.Text(
                "This is the only device this workspace knows yet.",
                role: .caption,
                secondary: true,
            )
        }
        for one in listed.devices {
            ui.ListRow(
                one.label,
                secondary: device_words(one),
                selected: one.local,
                icon: if one.display { .monitor } else { .laptop },
            )
        }
    }
    ui.Section("You") {
        setting(
            "Your colour",
            "The colour your pointer, your presence and your changes show in; empty follows your name",
        ) {
            ui.TextInput(bind: me.colour, placeholder: "From your name")
        }
    }
}

/// How one of the person's devices stands, in a few words.
fn device_words(one: device.PersonalDeviceView) -> Text {
    if one.local {
        "This device"
    } else if one.online and one.nearby {
        "Here beside you"
    } else if one.online { "Online" } else { "Asleep" }
}

/// What this person lets their devices hand to each other: the clipboard, what they were told,
/// what plays, and a window that goes on where it was left.
view continuity_pane() {
    let c = settings.continuity
    ui.Section("Continuity") {
        setting(
            "Let your devices work together",
            "The things below pass between the devices you are signed in on",
        ) {
            ui.Button(if c.enabled { "On" } else { "Off" }, toggled: c.enabled) {
                on press: settings.allow_continuity(settings.continuity, on: not c.enabled)
            }
        }
        setting("The clipboard", "What you copy here can be pasted there") {
            ui.Toggle(bind: c.clipboard)
        }
        setting("What you were told", "A message or a reminder reaches the device you are at") {
            ui.Toggle(bind: c.notifications)
        }
        setting("What plays", "Sound follows you from one device to the next") {
            ui.Toggle(bind: c.media)
        }
        setting("Wi-Fi settings", "A network one device joined, the others know") {
            ui.Toggle(bind: c.wifi_settings)
        }
        setting(
            "The screen and input",
            "One device's screen and keyboard can be used from another",
        ) {
            ui.Toggle(bind: c.display_input)
        }
        setting("Files", "Send a file straight to a device beside you") {
            ui.Toggle(bind: c.file_transfer)
        }
        setting("Live sessions", "Speak, watch or share a screen with someone") {
            ui.Toggle(bind: c.realtime_sessions)
        }
        setting("Gamepads", "A gamepad plugged in here works in a game there") {
            ui.Toggle(bind: c.typed_gamepad)
        }
        setting(
            "Windows that go on",
            "A window picked up on one device goes on where it was left on another",
        ) {
            ui.Toggle(bind: c.app_continuations)
        }
    }
    if not c.audit.is_empty() {
        ui.Section("What changed") {
            for entry in c.audit {
                if let capability = entry.capability {
                    ui.ListRow(
                        capability_words(capability),
                        secondary: if entry.enabled { "Allowed" } else { "Stopped" },
                        icon: .clock,
                    )
                }
            }
        }
    }
}

/// This copy of Tilo: the system it runs, what it is, and the workspace it belongs to.
view about_pane() {
    let u = device.update
    let apps = device.apps
    let host = device.host_info
    ui.Section("Tilo") {
        ui.Text(
            "Tilo is your workspace: one set of things, shared by the people and agents you work with, on the devices you signed in on.",
            role: .reading,
        )
        ui.Text(
            "Every change is kept, and can be taken back; what others change shows here as it happens.",
            role: .body,
            secondary: true,
        )
    }
    ui.Section("This system") {
        if let booted = u.booted {
            let called = booted.version ?? booted.image ?? ""
            ui.PropertyList {
                if called != "" {
                    ui.Property("Version", called)
                }
                if let digest = booted.digest {
                    ui.Property("Digest", digest, copyable: true)
                }
                if let built = booted.built {
                    ui.Property("Built", built)
                }
            }
        } else {
            ui.Text(
                "The system's details are kept by whoever administers this machine.",
                role: .caption,
                secondary: true,
            )
        }
        let app_count = apps.installed.len()
        ui.PropertyList {
            ui.Property("Apps", "{app_count}")
        }
    }
    ui.Section("This device") {
        ui.Property("Name", host.hostname)
        if host.pretty_name != "unknown" {
            ui.Property("System", host.pretty_name)
        }
        if host.kernel != "unknown" {
            ui.Property("Kernel", host.kernel)
        }
        ui.Property("Architecture", host.arch)
        if host.uptime != "unknown" {
            ui.Property("Up for", host.uptime)
        }
        ui.Button("Read these facts again", icon: .rotate_cw, size: .small) {
            on press: device.refresh(device.host_info)
        }
    }
    ui.Section("Hardware") {
        if host.cpu != "unknown" {
            ui.Property("Processor", host.cpu)
        }
        if host.memory != "unknown" {
            ui.Property("Memory", host.memory)
        }
        if host.unknown {
            ui.Text(
                "Some of this device's facts could not be read.",
                role: .caption,
                secondary: true,
            )
        }
    }
}

/// What a capability passing between devices is called, in words.
fn capability_words(capability: settings.ContinuityCapability) -> Text {
    if capability == .clipboard {
        "The clipboard"
    } else if capability == .notifications {
        "What you were told"
    } else if capability == .wifi_settings {
        "Wi-Fi settings"
    } else if capability == .media {
        "What plays"
    } else if capability == .display_input {
        "The screen and input"
    } else if capability == .file_transfer {
        "Files"
    } else if capability == .realtime_sessions {
        "Live sessions"
    } else if capability == .typed_gamepad {
        "Gamepads"
    } else if capability == .app_continuations { "Windows that go on" } else { "A capability" }
}
