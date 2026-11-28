/// How written text is checked as it is typed, in which language, and the words this person
/// writes their own way.
view spelling_pane() {
    state new_word: Text = ""
    state new_language: Text = ""
    let t = settings.text_assistance
    let d = settings.personal_dictionary
    ui.Section("Spelling") {
        setting("Check spelling", "Mark words no dictionary knows as they are typed") {
            ui.Toggle(bind: t.check_spelling)
        }
        setting("Correct as I type", "Replace a misspelling with the word it most likely is") {
            ui.Toggle(bind: t.auto_correct)
        }
        setting(
            "The dictionary's language",
            "A language tag such as de-DE, or one dictionary's name",
        ) {
            ui.TextInput(bind: t.spelling_language, placeholder: "The workspace's language")
        }
    }
    ui.Section("Your words") {
        ui.Text(
            "Words you keep are spelled as you write them, in every language or in the one you give, and are offered when a correction is chosen.",
            role: .caption,
            secondary: true,
        )
        if d.words.words.is_empty() {
            ui.Text("You keep no words of your own yet.", role: .caption, secondary: true)
        }
        for word in d.words.words {
            ui.ListRow(
                word.spelling,
                secondary: if let language = word.language {
                    "In {language}"
                } else { "Every language" },
                icon: .spell_check,
            ) {
                trailing: {
                    ui.Button("Remove", size: .small) {
                        on press: settings.remove(
                            settings.personal_dictionary,
                            spelling: word.spelling,
                            language: word.language,
                        )
                    }
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(bind: new_word, placeholder: "A word, spelled as you write it", fill)
            ui.TextInput(
                bind: new_language,
                placeholder: "Its language tag, or empty for every language",
                fill,
            )
            ui.Button("Add the word", role: .primary) {
                on press: settings
                    .add(
                        settings.personal_dictionary,
                        spelling: new_word,
                        language: if new_language == "" { .none } else { .some(new_language) },
                    )
                    .then(set(new_word, ""))
            }
        }
    }
    ui.Section("Writing") {
        ui.Text(
            "Corrected words are underlined until the text is sent; a right press offers the words the dictionary knows.",
            role: .caption,
            secondary: true,
        )
        if not d.words.words.is_empty() {
            ui.Button("Remove every word", role: .destructive) {
                on press: settings.clear(settings.personal_dictionary)
            }
        }
    }
}

/// Which app opens each kind of file and link. The handlers are the workspace's: every member
/// opens a kind the way the workspace chose.
view default_apps_pane() {
    state kind: Text = ""
    state chosen_app: Text = ""
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
            ui.ListRow(handler, secondary: handlers.handlers.get(handler) ?? "", icon: .app_window)
        }
    }
    ui.Section("Choose one") {
        ui.TextInput(bind: kind, placeholder: "A kind: text/plain, image/png, mailto")
        ui.TextInput(bind: chosen_app, placeholder: "The app's id, such as so.tilo.notes")
        ui.Button("Open this kind with it", role: .primary) {
            on press: settings.set_default_app_handler(
                settings.default_apps,
                kind: kind,
                app: chosen_app,
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
    state zone_search: Text = ""
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
        ) {
            ui.Button(if system.automatic { "On" } else { "Off" }, toggled: system.automatic) {
                on press: device.set_automatic_zone(device.time_zone, on: not system.automatic)
            }
        }
        ui.TextInput(bind: zone, placeholder: "A zone, such as Europe/Amsterdam")
        ui.Button("Use this zone", size: .small) {
            on press: device.set_zone(device.time_zone, zone: zone)
        }
        let here = shown_zone(system)
        ui.TextInput(bind: zone_search, placeholder: "Search the time zones")
        let found = system.zones
            .filter(
                z -> zone_search == "" or z.name.lower().contains(zone_search.lower()),
            )
            .take(12)
        if not system.zones.is_empty() and found.is_empty() {
            ui.Text("No zone's name holds those words.", role: .caption, secondary: true)
        }
        for z in found {
            ui.ListRow(
                z.name,
                secondary: if z.name == here { "The zone it shows" } else { "" },
                selected: z.name == here,
                icon: .globe,
            ) {
                on activate: device.set_zone(device.time_zone, zone: z.name)
            }
        }
        if not system.zones.is_empty() {
            ui.Text(
                "The mark on the map is where the zone's clock is kept.",
                role: .caption,
                secondary: true,
            )
            ui.Drawing(
                zone_map_shapes(system.zones, here),
                meaning: .picture,
                label: .some("The time zone's place on the map"),
                height: .some(220.0),
            )
        }
        setting(
            "Keep the clock right from the network",
            "Take the time from the network's time servers",
        ) {
            ui.Button(
                if network_time { "On" } else { "Off" },
                toggled: network_time,
                enabled: system.network_time != .none,
                description: "This device does not keep the time from the network.",
            ) {
                on press: device.set_network_time(device.time_zone, on: not network_time)
            }
        }
        if let problem = system.problem {
            ui.StatusLine(problem, status: .error)
        }
        if not system.synchronized {
            ui.Text(
                "The clock is not in step with the network yet.",
                role: .caption,
                secondary: true,
            )
        }
    }
    ui.Section("Formats") {
        setting("Dates", "The order of a date's parts") {
            ui.EnumField(bind: r.date_format, label: "Date format")
        }
        setting("Times", "A twenty-four hour clock, or one with morning and evening") {
            ui.EnumField(bind: r.time_format, label: "Time format")
        }
        setting(
            "The clock in the bar",
            "Hour and minute: the device's own zone, and what it follows",
        ) {
            ui.Toggle(bind: clock.automatic_zone)
        }
    }
}

/// The zone the clock shows, or the first the system lists while it shows none.
fn shown_zone(system: device.TimeZone) -> Text {
    if let named = system.zone {
        named
    } else if let first = system.zones.at(0) { first.name } else { "" }
}

/// The mark the zone `chosen` stands at on the map, or nothing where the system lists no place
/// for it.
fn zone_point(zones: List<device.ZonePlace>, chosen: Text) -> Option<List<Float>> {
    if let zone = zones.find(z -> z.name == chosen) {
        .some([(zone.longitude + 180.0) / 360.0, (90.0 - zone.latitude) / 180.0])
    } else { .none }
}

/// A world with the continents sketched, a graticule and a mark where `chosen`'s clock is kept:
/// longitude across, latitude down.
fn zone_map_shapes(zones: List<device.ZonePlace>, chosen: Text) -> List<ui.Shape> {
    let uprights = [1, 2, 4, 5]
        .map(
            i -> ui.Shape.line(
                x1: i.to_float() / 6.0,
                y1: 0.0,
                x2: i.to_float() / 6.0,
                y2: 1.0,
                stroke: .track,
                weight: .hairline,
            ),
        )
    let level = [1, 2]
        .map(
            i -> ui.Shape.line(
                x1: 0.0,
                y1: i.to_float() / 3.0,
                x2: 1.0,
                y2: i.to_float() / 3.0,
                stroke: .track,
                weight: .hairline,
            ),
        )
    let mark = if let point = zone_point(zones, chosen) {
        [
            ui.Shape.ellipse(
                cx: point.at(0) ?? 0.5,
                cy: point.at(1) ?? 0.5,
                rx: 0.014,
                ry: 0.028,
                fill: .some(.accent),
            ),
        ]
    } else { [] }
    [ui.Shape.rect(x: 0.0, y: 0.0, w: 1.0, h: 1.0, fill: .some(.surface))]
        + uprights
        + level
        + continent_shapes()
        + mark
}

/// The continents as rough outlines, longitude across and latitude down: what places a zone's
/// mark against land.
fn continent_shapes() -> List<ui.Shape> {
    land(
        [
            0.0333,
            0.1389,
            0.1111,
            0.1111,
            0.2222,
            0.1,
            0.3333,
            0.2222,
            0.3194,
            0.25,
            0.2778,
            0.3611,
            0.2222,
            0.3889,
            0.1806,
            0.3333,
            0.1528,
            0.2778,
        ],
    )
        + land(
            [
                0.2778,
                0.4444,
                0.3333,
                0.4444,
                0.4028,
                0.5278,
                0.3889,
                0.6111,
                0.3472,
                0.6944,
                0.3056,
                0.8056,
                0.2917,
                0.75,
                0.2778,
                0.6111,
            ],
        )
        + land(
            [
                0.4722,
                0.3,
                0.5,
                0.2222,
                0.5833,
                0.1111,
                0.6667,
                0.1111,
                0.7778,
                0.0833,
                0.8889,
                0.1111,
                1.0,
                0.1389,
                0.9722,
                0.2222,
                0.8889,
                0.2778,
                0.8333,
                0.3333,
                0.7778,
                0.4444,
                0.6944,
                0.3889,
                0.625,
                0.3333,
                0.5833,
                0.2778,
                0.5278,
                0.2556,
            ],
        )
        + land(
            [
                0.4528,
                0.4167,
                0.5278,
                0.3056,
                0.5889,
                0.3333,
                0.6111,
                0.4444,
                0.625,
                0.5556,
                0.5972,
                0.6667,
                0.5556,
                0.6944,
                0.5278,
                0.6389,
                0.5,
                0.5556,
                0.4722,
                0.4722,
            ],
        )
        + land([0.8139, 0.6222, 0.8611, 0.5667, 0.9167, 0.6389, 0.9028, 0.7111, 0.8472, 0.6889])
}

/// One outlined land mass over flat map coordinates.
fn land(corners: List<Float>) -> List<ui.Shape> {
    [
        ui.Shape.path(
            points: corners,
            smooth: false,
            fill: .some(.track),
            stroke: .some(.track),
            wash: false,
        ),
    ]
}
