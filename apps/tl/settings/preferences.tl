/// How written text is checked as it is typed, and in which language.
view spelling_pane() {
    let t = settings.text_assistance
    ui.Section("Spelling") {
        setting(
            "Check spelling",
            "Mark words no dictionary knows as they are typed",
            ui.Toggle(bind: t.check_spelling),
        )
        setting(
            "Correct as I type",
            "Replace a misspelling with the word it most likely is",
            ui.Toggle(bind: t.auto_correct),
        )
        setting(
            "The dictionary's language",
            "A language tag such as de-DE, or one dictionary's name",
            ui.TextInput(bind: t.spelling_language, placeholder: "The workspace's language"),
        )
    }
    ui.Section("Writing") {
        ui.Text(
            "Corrected words are underlined until the text is sent; a right press offers the words the dictionary knows.",
            role: .caption,
            secondary: true,
        )
    }
}

/// Which app opens each kind of file and link. The handlers are the workspace's: every member
/// opens a kind the way the workspace chose.
view default_apps_pane() {
    state kind: Text = ""
    state app: Text = ""
    let handlers = settings.default_apps
    let installed = device.apps
    ui.Section("Handlers") {
        if handlers.handlers.keys().is_empty() {
            ui.Text(
                "Every kind opens with the app that names it, unless another is chosen here.",
                role: .caption,
                secondary: true,
            )
        }
        for handler in handlers.handlers.keys() {
            ui.ListRow(
                handler,
                secondary: handlers.handlers.get(handler) ?? "",
                icon: .app_window,
            )
        }
    }
    ui.Section("Choose one") {
        ui.TextInput(bind: kind, placeholder: "A kind: text/plain, image/png, mailto")
        ui.TextInput(bind: app, placeholder: "The app's id, such as so.tilo.notes")
        ui.Button("Open this kind with it", role: .primary) {
            on press: settings.set_default_app_handler(
                settings.default_apps,
                kind: kind,
                app: app,
            )
        }
        if not installed.installed.is_empty() {
            ui.Text("The apps this device has:", role: .caption, secondary: true)
            for shown in installed.installed.take(12) {
                ui.ListRow(shown.name, secondary: shown.key, icon: .app_window)
            }
        }
    }
}

/// This device's clock: its zone, whether the zone follows the device and the network's time,
/// and the formats dates and times are shown in.
view date_time_pane() {
    state zone: Text = ""
    let clock = settings.clock
    let system = device.time_zone
    let network_time = if let on = system.network_time { on } else { false }
    let r = settings.locale
    let now = device.time
    ui.Section("Now") {
        ui.ListRow(now.clock, secondary: "This device's clock", icon: .clock)
        if let named = system.zone {
            ui.ListRow(named, secondary: "The zone it shows", icon: .globe)
        }
    }
    ui.Section("Time zone") {
        setting(
            "Follow where the device is",
            "Take the zone from the device's location as it moves",
            ui.Button(if system.automatic { "On" } else { "Off" }, toggled: system.automatic) {
                on press: device.set_automatic_zone(
                    device.time_zone,
                    on: not system.automatic,
                )
            },
        )
        ui.TextInput(bind: zone, placeholder: "A zone, such as Europe/Amsterdam")
        ui.Button("Use this zone", size: .small) {
            on press: device.set_zone(device.time_zone, zone: zone)
        }
        setting(
            "Keep the clock right from the network",
            "Take the time from the network's time servers",
            ui.Button(
                if network_time { "On" } else { "Off" },
                toggled: network_time,
                enabled: system.network_time != .none,
            ) {
                on press: device.set_network_time(device.time_zone, on: not network_time)
            },
        )
        if let problem = system.problem {
            ui.StatusLine(problem, status: .error)
        }
        if not system.synchronized {
            ui.Text("The clock is not in step with the network yet.", role: .caption, secondary: true)
        }
    }
    ui.Section("Formats") {
        setting(
            "Dates",
            "The order of a date's parts",
            ui.EnumField(bind: r.date_format, label: "Date format"),
        )
        setting(
            "Times",
            "A twenty-four hour clock, or one with morning and evening",
            ui.EnumField(bind: r.time_format, label: "Time format"),
        )
        setting(
            "The clock in the bar",
            "Hour and minute: the device's own zone, and what it follows",
            ui.Toggle(bind: clock.automatic_zone),
        )
    }
}
