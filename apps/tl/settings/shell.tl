/// The Shell pane: the parts of the shell a person can bind a view to, and the page of the one
/// they are changing.
view shell_pane() {
    state page: Text = ""
    state chosen: Text = ""
    if page == "shell-part" {
        shell_part_page(chosen, page)
    } else {
        shell_views_section(page, chosen)
    }
}

/// The views that draw the shell: each part of it a person can replace — the bar, the window
/// menu, the switcher and the rest — listed with the view that draws it, and a page where the
/// person binds it to a view an app of theirs publishes, or shows the shell's own again.
///
/// The binding is the person's own setting (`settings.shell_views`), so it follows them to
/// their other devices, and binding it here is the same change an agent or a script makes.
view shell_views_section(page: State<Text>, chosen: State<Text>) {
    let views = settings.shell_views
    ui.Section("Parts of the shell") {
        ui.Text(
            "Each part of the shell is drawn by a view: the one the shell ships, or one an app of yours publishes. A view you bind follows you to your other devices, and keeps what it draws on each, whatever the language it speaks.",
            role: .reading,
            secondary: true,
        )
        for part in shell_parts() {
            ui.ListRow(
                shell_part_name(part),
                secondary: shell_binding_words(views, part),
                icon: .app_window,
            ) {
                trailing: {
                    ui.Button("Change", role: .plain, size: .small) {
                        on press: set(chosen, part).then(set(page, "shell-part"))
                    }
                }
            }
        }
    }
}

/// One part's page: which view draws it now, and where the person names the app and view that
/// draw it instead.
view shell_part_page(part: Text, page: State<Text>) {
    state app_name: Text = ""
    state view_name: Text = ""
    state scope: Text = "every"
    let views = settings.shell_views
    let drawn = views.bindings.get(part)
    ui.Section(shell_part_name(part)) {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(page, "")
        }
        ui.PropertyList {
            ui.Property("Drawn by", shell_binding_words(views, part))
            for device_class in shell_device_kinds() {
                if let over = views.on_device.get(device_class) {
                    if let binding = over.get(part) {
                        ui.Property(
                            shell_device_name(device_class),
                            "{binding.app} - {binding.view}",
                        )
                    }
                }
            }
        }
        ui.Text(
            "An app of yours draws the part with the view it publishes. Where the app is not installed, the part stays with the shell's own view rather than going empty.",
            role: .caption,
            secondary: true,
        )
    }
    ui.Section("A view of your own") {
        setting("The app", "The app whose view draws this part") {
            ui.TextInput(bind: app_name, placeholder: "so.tilo.example.menu")
        }
        setting("The view", "The view the app publishes for this part") {
            ui.TextInput(bind: view_name, placeholder: "menu")
        }
        setting("On which devices", "All of yours, or one kind of device instead") {
            ui.ToggleGroup(selected: shell_on_index(scope), label: "On which devices") {
                ui.Segment("Every device") {
                    on press: set(scope, "every")
                }
                ui.Segment("A computer") {
                    on press: set(scope, "native")
                }
                ui.Segment("The browser") {
                    on press: set(scope, "web")
                }
            }
        }
        ui.Button("Bind", role: .primary) {
            on press: settings.bind(
                settings.shell_views,
                part: part,
                app: app_name.trim(),
                view: view_name.trim(),
                on: shell_on_choice(scope),
            )
        }
        if drawn != .none {
            ui.Button("Show the shell's own view again", icon: .undo_2) {
                on press: settings.reset(
                    settings.shell_views,
                    part: part,
                    on: shell_on_choice(scope),
                )
            }
        }
    }
}

/// The parts of the shell a person may bind a view to, by the names the shell knows them by.
fn shell_parts() -> List<Text> {
    [
        "wm",
        "window-decoration",
        "bar",
        "quick-settings",
        "launcher",
        "switcher",
        "overview",
        "strips",
        "shell-widgets",
        "notifications",
        "people",
        "emoji",
        "osd",
        "window-menu",
        "workspace-menu",
    ]
}

/// A part of the shell, in the person's words.
fn shell_part_name(part: Text) -> Text {
    if part == "wm" {
        std.say("The window manager")
    } else if part == "window-decoration" {
        std.say("Window edges")
    } else if part == "bar" {
        std.say("The bar")
    } else if part == "quick-settings" {
        std.say("Quick settings")
    } else if part == "launcher" {
        std.say("The app launcher")
    } else if part == "switcher" {
        std.say("The window switcher")
    } else if part == "overview" {
        std.say("The overview")
    } else if part == "strips" {
        std.say("The strips")
    } else if part == "shell-widgets" {
        std.say("The widgets on your shell")
    } else if part == "notifications" {
        std.say("Notifications")
    } else if part == "people" {
        std.say("Who is here")
    } else if part == "emoji" {
        std.say("The emoji picker")
    } else if part == "osd" {
        std.say("On-screen displays")
    } else if part == "window-menu" {
        std.say("The window menu")
    } else if part == "workspace-menu" {
        std.say("The workspace menu")
    } else { std.say("A part of the shell") }
}

/// What draws a part now: the view the person bound, or the one the shell ships.
fn shell_binding_words(views: settings.ShellViews, part: Text) -> Text {
    if let binding = views.bindings.get(part) {
        "{binding.app} - {binding.view}"
    } else { std.say("The view the shell ships") }
}

/// The kinds of device a view can be bound on, apart from every device.
fn shell_device_kinds() -> List<Text> {
    ["native", "web"]
}

/// A kind of device in the person's words.
fn shell_device_name(device_class: Text) -> Text {
    if device_class == "native" { std.say("A computer") } else { std.say("The browser") }
}

/// Which choice of the device group is chosen, in the order the group lists them.
fn shell_on_index(scope: Text) -> Int {
    if scope == "native" { 1 } else if scope == "web" { 2 } else { 0 }
}

/// The choice of devices as the binding takes it: nothing for every device, the kind otherwise.
fn shell_on_choice(scope: Text) -> Option<Text> {
    if scope == "native" or scope == "web" { .some(scope) } else { .none }
}
