/// The catalogue: the apps the sources offer, by the place a person chose, with the app's page
/// beside the list. The bar reads the sources again, reviews a package, and opens the
/// repositories and the trust page.
// The App Store's views: the catalogue and its places, an app's page, a repository's page and a
// report's page. Browsing is the window's own; the workspace's reports and its people's blocks
// are resources every device reads.
opened action read_sources(s: Store) {
    if s.apps.is_empty() and s.reading == "" { refresh(s) }
}

/// Opening an app's page reads what its source says about it, once.
opened action read_details(a: App) uses web {
    if not a.details_read { show_details(root Store, a) }
}

view catalogue(s: Store) {
    state chosen: Text = ""
    state searched: Text = ""
    state page: Text = "apps"
    let places = places_of()
    let place = places.find(f -> f.query == chosen)
    let category = if chosen.starts_with("category:") {
        chosen.slice("category:".len(), chosen.len())
    } else { "" }
    let by_place = if let p = place { browsable(s).matching(p) } else { browsable(s) }
    let listed = if category == "" {
        by_place
    } else {
        by_place.filter(a -> a.categories.contains(category))
    }
    let found = if searched.is_empty() { listed } else { listed.matching(searched) }
    let title = if searched != "" {
        "Search"
    } else if let p = place { p.label } else if category != "" { category } else { "All apps" }
    if page == "apps" {
        ui.NavShell(
            content_kind: .document,
            overview_title: title,
            overview_count: found.len(),
            sidebar_title: "App Store",
        ) {
            on search(q: Text): set(searched, q)
            actions: {
                catalogue_actions(s, page)
            }
            sidebar: {
                catalogue_sidebar(s, places, chosen)
            }
            overview: {
                stack(gap: .related) {
                    if s.reading != "" {
                        ui.Banner("Reading the sources", message: s.status, kind: .info)
                    } else if s.trouble != "" {
                        ui.Banner("Could not read the sources", message: s.trouble, kind: .error) {
                            on act: refresh(s)
                        }
                    }
                    if chosen == "" and searched == "" {
                        discover(s)
                    } else if found.is_empty() {
                        ui.EmptyState(
                            "No apps found",
                            message: "Try a different search or category.",
                            icon: .store,
                        )
                    } else {
                        app_list(found, s)
                    }
                }
            }
        }
    } else {
        ui.NavShell(
            content_kind: .page,
            title: if page == "remotes" {
                "Flatpak repositories"
            } else { "Trust and moderation" },
            sidebar_title: "App Store",
        ) {
            actions: {
                catalogue_actions(s, page)
            }
            sidebar: {
                catalogue_sidebar(s, places, chosen)
            }
            content: {
                if page == "remotes" {
                    remotes_page(s)
                } else {
                    trust_page(s)
                }
            }
        }
    }
}

/// The bar's actions: read the sources, review a package, open the repositories and the trust
/// page, and go back to the apps.
view catalogue_actions(s: Store, page: State<Text>) {
    ui.Button("Refresh apps", icon: .refresh, icon_only: true) {
        on press: refresh(s)
    }
    if page == "apps" {
        ui.Button("Update all apps", icon: .arrow_down) {
            on press: update_all(s)
        }
    }
    // A package file is reviewed whichever kind it is: the app reads a `.tapp`, and a
    // `.flatpakref` is a reference to a repository the source itself installs.
    ui.Button("Review package", icon: .package) {
        on press: file.choose(import_package(s), accept: [".tapp", ".flatpakref"])
    }
    ui.Button("Flatpak repositories", icon: .server) {
        on press: set(page, "remotes").then(refresh_remotes(s))
    }
    ui.Button("Trust and moderation", icon: .shield_check) {
        on press: set(page, "trust")
    }
    if page != "apps" {
        ui.Button("Back to apps", icon: .arrow_left) {
            on press: set(page, "apps")
        }
    }
}

/// The places a person browses: what is new, what updates, what runs here, the bots, and each
/// category the catalogue names.
view catalogue_sidebar(s: Store, places: List<Filter<App>>, chosen: State<Text>) {
    std.places(chosen, places, "All apps", icon: "package")
    let categories = categories_of(browsable(s))
    if not categories.is_empty() {
        ui.NavSection("Categories") {
            for c in categories {
                ui.NavItem("category:{c}", c, icon: .layout_grid) {
                    on select: set(chosen, "category:{c}")
                }
            }
        }
    }
}

/// What the catalogue's front page shows: what was recently updated, what its sources feature,
/// and the categories to narrow a browse by.
view discover(s: Store) {
    if s.apps.is_empty() {
        ui.EmptyState(
            "No apps available",
            message: "Add or enable a repository you trust.",
            icon: .package,
        )
    } else {
        let recent = browsable(s)
            .filter(a -> a.updated != "")
            .sort_by(a -> a.updated, desc)
            .take(12)
        let featured = browsable(s).filter(a -> a.featured > 0).sort_by(a -> a.featured).take(12)
        let fresh = browsable(s).sort_by(a -> a.name.lower())
        stack(gap: .section) {
            if not featured.is_empty() {
                ui.Section("Featured") {
                    ui.MediaGrid(size: .regular) {
                        for a in featured {
                            app_card(a)
                        }
                    }
                }
            }
            if not recent.is_empty() {
                ui.Section("Recently updated") {
                    ui.MediaGrid(size: .regular) {
                        for a in recent {
                            app_card(a)
                        }
                    }
                }
            }
            ui.Section("All apps") {
                app_list(fresh, s)
            }
        }
    }
}

/// One app as a card of a shelf: its icon or picture, its name, where it comes from and what it
/// offers now.
view app_card(a: App) {
    ui.MediaCard(a, title: a.name, picture: a.icon, meta: [origin_short(a), device_words(a)]) {
        on activate: open(a)
    }
}

/// Apps as rows, the first `shown` of them.
view app_list(items: List<App>, s: Store) {
    state opened: Option<App> = .none
    state shown: Int = 48
    stack(gap: .related) {
        std.list(items.take(shown), opened) { (a, at) ->
            ui.ListItem(item: a, presence: at) {
                app_row(a)
            }
        }
        if items.len() > shown {
            ui.Button("Show more", icon: .chevron_down) {
                on press: set(shown, shown + 48)
            }
        }
    }
}

fn origin_short(a: App) -> Text {
    if a.bot {
        "Bot"
    } else if a.origin == "tilo" {
        "Tilo"
    } else if a.origin == "flathub" {
        "Flathub"
    } else if a.origin == "remote" { a.registry } else { "Local package" }
}

/// An app as a row: what it is, where it comes from, what it offers now, and where its change
/// stands.
view app_row(a: App) {
    let s = root Store
    let verb = verb_of(a, s.flatpak)
    let why = refusal(a, s.flatpak)
    ui.ListRow(
        a.name,
        secondary: if verb != "" and verb != "open" {
            "{action_label(a, s.flatpak)} · {device_words(a)}"
        } else if a.summary != "" { a.summary } else { device_words(a) },
        meta: if why != "" { why } else if a.job != "" { "Working…" } else { origin_short(a) },
        meta_tone: if why != "" { .warning } else if a.job != "" { .info } else { .neutral },
        icon: if a.origin == "tilo" {
            .package
        } else if a.origin == "flathub" {
            .box
        } else if a.origin == "remote" { .server } else { .file_archive },
        mark: if a.bot {
            .bot
        } else if a.installed != "" and a.pinned {
            .pin
        } else if a.updatable {
            .arrow_down
        } else if a.installed != "" { .circle_check } else { .download },
    )
}

/// An app's page: what it is, what a person can do with it here, what its source says about it,
/// and the reports people made of it.
view app_page(a: App) {
    state technical: Bool = false
    let s = root Store
    let verb = verb_of(a, s.flatpak)
    let why = refusal(a, s.flatpak)
    let report = s.reports.find(r -> r.identity == a.identity)
    ui.Scroll {
        stack(gap: .section) {
            ui.ObjectHeader(
                a.name,
                caption: origin_words(a),
                icon: if a.origin == "tilo" {
                    .package
                } else if a.origin == "flathub" {
                    .box
                } else if a.origin == "remote" { .server } else { .file_archive },
            )
            row(gap: .tight, wrap: true) {
                if why == "" and verb != "" {
                    ui.Button(
                        action_label(a, s.flatpak),
                        icon: if verb == "open" {
                            .play
                        } else if verb == "update" {
                            .arrow_down
                        } else if verb == "add-bot" { .bot } else { .download },
                        role: .primary,
                    ) {
                        on press: apply_action(a, verb)
                    }
                }
                if a.installed != "" and can_pin(a) {
                    if a.pinned {
                        ui.Button("Allow updates", icon: .pin_off) {
                            on press: unpin(a)
                        }
                    } else {
                        ui.Button("Keep this version", icon: .pin) {
                            on press: pin(a)
                        }
                    }
                }
                if a.installed != "" or a.fetched != "" {
                    ui.Button("Uninstall", icon: .trash) {
                        on press: remove(a)
                    }
                }
                if a.job != "" {
                    ui.Button("Stop", icon: .square) {
                        on press: cancel_change(a)
                    }
                }
                if s.blocks.any(b -> b.identity == a.identity) {
                    ui.Button("Unblock", icon: .circle_check) {
                        on press: unblock_app(s, a)
                    }
                } else {
                    ui.Button("Block app", icon: .ban) {
                        on press: block_app(s, a)
                    }
                }
                ui.Button("Technical details", icon: .info, toggled: technical) {
                    on press: set(technical, not technical)
                }
            }
            if why != "" {
                ui.Banner("Cannot change this app", message: why, kind: .warning)
            }
            if a.job != "" {
                ui.ProgressBar(label: "Working…")
            }
            if s.reviewed_app == a.identity {
                ui.Banner(
                    "Package verified",
                    message: "Review its details before installing it.",
                    kind: .success,
                ) {
                    on act: install_reviewed(s)
                }
                ui.Button("Install reviewed package", icon: .download, role: .primary) {
                    on press: install_reviewed(s)
                }
            }
            row(gap: .tight, wrap: true) {
                ui.Fact(device_words(a), icon: .circle_check)
                if a.available != "" {
                    ui.Fact("Latest: version {a.available}", icon: .package)
                }
                if a.size > 0 {
                    ui.Fact(size_words(a.size), icon: .download)
                }
                if a.awaiting_developer {
                    ui.Fact(
                        "Waiting for an update from the developer",
                        icon: .hourglass,
                        priority: 1,
                    )
                }
                if not a.runnable {
                    ui.Fact("Only built for {a.processors}", icon: .triangle_alert, priority: 1)
                }
                ui.Fact(trust_short(a), icon: .shield_check)
            }
            if a.summary != "" and a.description == "" {
                text(a.summary, role: .reading)
            }
            if a.description != "" {
                ui.Section("About this app") {
                    text(a.description, role: .reading)
                }
            }
            if not a.screenshots.is_empty() {
                ui.Section("Screenshots") {
                    ui.Scroll(across: true) {
                        row(gap: .related) {
                            for shot in a.screenshots {
                                ui.Image(shot, alt: "Screenshot of {a.name}", fit: .cover)
                            }
                        }
                    }
                }
            }
            if not a.releases.is_empty() {
                ui.Section("What's new") {
                    for r in a.releases {
                        stack(gap: .tight) {
                            ui.Text("Version {r.version}", role: .subheading)
                            if r.notes != "" {
                                text(r.notes, role: .body)
                            }
                        }
                    }
                }
            }
            permissions_section(a)
            ui.Section("Trust") {
                text(trust_words(a), role: .body)
            }
            facts_section(a, technical)
            if a.bot {
                bot_section(a)
            }
            report_section(s, a, report)
        }
    }
}

/// The action a page's one button runs, by the name the catalogue decided.
action apply_action(a: App, verb: Text) {
    let s = root Store
    if verb == "open" {
        open_app(a)
    } else if verb == "get" {
        get(a)
    } else if verb == "install" {
        get(a)
    } else if verb == "update" {
        update(a)
    } else if verb == "install-runtime" {
        install_runtime(a)
    } else if verb == "add-bot" {
        add_bot(s, a)
    } else if verb == "flatpak-setup" {
        s.trouble = "Install Flatpak on this computer to use this app."
    }
}

/// What an app asks for, as its source says.
view permissions_section(a: App) {
    ui.Section("What it can do") {
        if a.origin == "tilo" or a.origin == "local" {
            text(
                "Runs in Tilo's sandbox and asks before it uses your files, camera or microphone.",
                role: .body,
            )
            if not a.permissions.is_empty() {
                for p in a.permissions {
                    ui.Fact(
                        permission_words(p),
                        icon: if p.broad { .triangle_alert } else { .check },
                    )
                }
            }
        } else if a.permissions.is_empty() {
            text("Its source has not said what this app can do.", role: .body)
        } else {
            for p in a.permissions {
                ui.Fact(permission_words(p), icon: if p.broad { .triangle_alert } else { .check })
            }
        }
    }
}

/// An app's facts: where it comes from, its version, its licence and where to read more.
view facts_section(a: App, technical: Bool) {
    ui.Section("Facts") {
        ui.PropertyList {
            ui.Property("Source", origin_words(a))
            ui.Property("Version", if a.available != "" { a.available } else { a.installed })
            if size_words(a.size) != "" {
                ui.Property("Download size", size_words(a.size))
            }
            if size_words(a.installed_size) != "" {
                ui.Property("Installed size", size_words(a.installed_size))
            }
            if a.developer != "" {
                ui.Property("Developer", a.developer)
            }
            if a.license != "" {
                ui.Property("Licence", a.license)
            }
            if a.updated != "" {
                ui.Property("Updated", a.updated)
            }
            if not a.categories.is_empty() {
                ui.Property("Categories", a.categories.sort_by(x -> x).join(", "))
            }
            if not a.languages.is_empty() {
                ui.Property("Supported languages", a.languages.sort_by(x -> x).join(", "))
            }
            if technical {
                ui.Property("App ID", a.identity, copyable: true)
                if a.publisher != "" {
                    ui.Property("Publisher key", a.publisher, copyable: true)
                }
                if a.digest != "" {
                    ui.Property("Package digest", a.digest, copyable: true)
                }
                if a.runtime != "" {
                    ui.Property("Required runtime", a.runtime)
                }
            }
        }
        row(gap: .tight, wrap: true) {
            if a.homepage != "" {
                ui.Link(a.homepage, label: "Open website")
            }
            if a.source_code != "" {
                ui.Link(a.source_code, label: "Open source code")
            }
        }
    }
}

/// What a directory bot does, what it may reach, and where it is added.
view bot_section(a: App) {
    ui.Section("What it does") {
        for kind in a.bot_triggers.sort_by(x -> x) {
            ui.Fact(bot_trigger_words(kind), icon: .zap)
        }
    }
    ui.Section("What it can reach") {
        if a.bot_apps.is_empty() and a.bot_connectors.is_empty() {
            text("Nothing beyond the workspace it runs in.", role: .body)
        }
        for other in a.bot_apps.sort_by(x -> x) {
            ui.Fact("Uses {other}'s actions and watches its changes", icon: .package)
        }
        for connector in a.bot_connectors.sort_by(x -> x) {
            ui.Fact("Connector, version {connector} or newer", icon: .plug)
        }
        if a.bot_appearance {
            text("May post under other names and avatars. Always tagged as a bot.", role: .body)
        }
    }
}

/// The report a person can make of an app: a draft the workspace shares, its evidence, and the
/// registry's answer once there is one.
view report_section(s: Store, a: App, report: Option<Report>) {
    ui.Section("Report a problem") {
        text(
            "Reports are signed with your workspace identity and stay drafts your workspace shares until you submit them.",
            role: .body,
        )
        if let r = report {
            report_form(s, r)
        } else {
            ui.Button("Create report draft", icon: .flag) {
                on press: draft_report(s, a)
            }
        }
    }
}

/// One report's form: the concern, the registry's case and status, and the verbs that send it,
/// refresh it, appeal a decision or save its evidence.
view report_form(s: Store, r: Report) {
    stack(gap: .related) {
        row(gap: .related, wrap: true) {
            ui.Badge(r.status, tone: if r.status == "Not submitted" { .neutral } else { .info })
            if r.case != "" {
                ui.Text("Case {r.case}", role: .caption, secondary: true)
            }
        }
        ui.TextInput(bind: r.note, lines: 4, placeholder: "Describe what happened")
        row(gap: .tight, wrap: true) {
            ui.Button("Submit report", icon: .send, role: .primary) {
                on press: submit_report(r)
            }
            if r.case != "" {
                ui.Button("Refresh status", icon: .refresh) {
                    on press: refresh_status(s, r)
                }
                ui.Button("Appeal decision", icon: .arrow_up) {
                    on press: appeal(r)
                }
            }
            ui.Button("Export evidence", icon: .download) {
                on press: file.save(report_file(r), name: "{r.identity}-report.json")
            }
        }
    }
}

/// A report's page: its app, its status and its form.
view report_page(r: Report) {
    let s = root Store
    stack(gap: .section) {
        ui.ObjectHeader("Report for {r.identity}", caption: r.status, icon: .flag)
        report_form(s, r)
        if let a = s.apps.find(x -> x.identity == r.identity) {
            ui.Button("Show the app", icon: .package) {
                on press: open(a)
            }
        }
    }
}

/// The Flatpak repositories this computer and this workspace offer, with the form that adds one.
view remotes_page(s: Store) {
    state name: Text = ""
    state url: Text = ""
    stack(gap: .section) {
        text(
            "Browse software from your configured repositories. Repositories supplied by this computer stay available; repositories you add belong to your workspace.",
            role: .body,
        )
        ui.Button("Refresh repositories", icon: .refresh) {
            on press: refresh_remotes(s)
        }
        if s.flatpak_trouble != "" {
            ui.Banner("Flatpak unavailable", message: s.flatpak_trouble, kind: .warning)
        }
        ui.Section("Configured repositories") {
            if s.remotes.is_empty() {
                ui.EmptyState("No repositories configured", icon: .server, inline: true)
            } else {
                for r in s.remotes {
                    ui.ListItem(item: r) {
                        remote_row(s, r)
                    }
                }
            }
        }
        ui.Section("Add a repository") {
            field("Name") {
                ui.TextInput(bind: name, placeholder: "Repository name")
            }
            field("Repository URL", help: "Repositories must use HTTPS.") {
                ui.TextInput(bind: url, placeholder: "https://…")
            }
            ui.Button("Add repository", icon: .plus, role: .primary) {
                on press: add_remote(s, name, url).then(set(name, "")).then(set(url, ""))
            }
        }
    }
}

view remote_row(s: Store, r: Remote) {
    ui.ListRow(
        r.title,
        secondary: r.url,
        meta: if r.trouble != "" {
            r.trouble
        } else if r.scope == "device" { "This computer" } else { "This workspace" },
        meta_tone: if r.trouble != "" { .warning } else { .neutral },
        icon: .server,
        mark: if r.enabled { .circle_check } else { .circle },
    )
    row(gap: .tight, wrap: true) {
        if r.scope == "account" {
            if r.enabled {
                ui.Button("Disable source", icon: .square) {
                    on press: disable_remote(s, r)
                }
            } else {
                ui.Button("Enable source", icon: .play) {
                    on press: enable_remote(s, r)
                }
            }
            ui.Button("Remove repository", icon: .trash) {
                on press: remove_remote(s, r)
            }
        }
        ui.Button("Browse apps", icon: .search) {
            on press: browse(s, r).then(open(r))
        }
    }
}

/// A repository's page: what it offers, once browsed.
view remote_page(r: Remote) {
    let s = root Store
    stack(gap: .section) {
        ui.ObjectHeader(r.title, caption: r.url, icon: .server)
        if r.trouble != "" {
            ui.Banner("Could not browse repository", message: r.trouble, kind: .error)
        }
        let mine = s.apps.filter(a -> a.origin == "remote" and a.registry == r.name)
        if mine.is_empty() {
            ui.EmptyState(
                "No apps loaded from this repository",
                message: "Browse apps to read what it offers.",
                icon: .server,
                inline: true,
            )
        } else {
            ui.Text("{parts_words(mine.len(), "app", "apps")} from {r.title}", role: .caption)
            for a in mine.sort_by(x -> x.name.lower()) {
                ui.ListItem(item: a) {
                    app_row(a)
                }
            }
        }
        row(gap: .tight, wrap: true) {
            if r.scope == "account" {
                if r.enabled {
                    ui.Button("Disable source", icon: .square) {
                        on press: disable_remote(s, r)
                    }
                } else {
                    ui.Button("Enable source", icon: .play) {
                        on press: enable_remote(s, r)
                    }
                }
            }
            ui.Button("Browse apps", icon: .search) {
                on press: browse(s, r)
            }
        }
    }
}

/// Trust and moderation: how reports and signatures work, and the reports this workspace made.
view trust_page(s: Store) {
    stack(gap: .section) {
        ui.Section("How reports work") {
            text(
                "Reports are signed with your workspace identity and stay drafts your workspace shares until you submit them. A new status shows only once it is verified, so an offline or refused request stays a draft you can send again.",
                role: .body,
            )
        }
        ui.Section("What a signature means") {
            text(
                "A verified package comes from its publisher and has not been changed. That does not make an app useful, private or honest, and the Store never invents a review badge.",
                role: .body,
            )
        }
        ui.Section("Publishing guidelines") {
            text(
                "Listings must describe what the app does, name any paid or online services it depends on, ask only for the permissions it needs, keep people's data across updates, work with assistive technology, and never impersonate others or track people without saying so.",
                role: .body,
            )
        }
        ui.Section("Reports") {
            if s.reports.is_empty() {
                ui.EmptyState(
                    "No report drafts",
                    message: "Open an app and choose Create report draft.",
                    icon: .flag,
                    inline: true,
                )
            } else {
                for r in s.reports.sort_by(x -> x.identity) {
                    ui.ListItem(item: r) {
                        report_row(s, r)
                    }
                }
            }
        }
        if not s.unanswered.is_empty() or not s.expired.is_empty() {
            ui.Section("Sources") {
                for name in s.unanswered.sort_by(x -> x) {
                    ui.Fact(
                        "{name} did not respond. Other apps remain available.",
                        icon: .triangle_alert,
                    )
                }
                for name in s.expired.sort_by(x -> x) {
                    ui.Fact(
                        "The app catalogue from {name} has expired. Apps you have keep working.",
                        icon: .hourglass,
                    )
                }
            }
        }
    }
}

view report_row(s: Store, r: Report) {
    ui.ListRow(
        "Report for {r.identity}",
        secondary: if r.note != "" { r.note } else { "No concern described yet" },
        meta: r.status,
        meta_tone: if r.status == "Not submitted" { .neutral } else { .info },
        icon: .flag,
    )
}

/// A report's evidence as the file a person saves and sends with it.
action report_file(r: Report) -> File {
    File.of_text("{r.identity}-report.json", evidence_json(r))
}

/// A report's evidence as one JSON file, so a person can send it with the report.
fn evidence_json(r: Report) -> Text {
    json.encode(
        Evidence(
            identity: r.identity,
            note: r.note,
            status: r.status,
            case: r.case,
            registry: r.registry,
            operation: r.operation,
        ),
    )
}

/// The places a person browses, named from the type's own filters and the categories the
/// catalogue holds.
fn places_of() -> List<Filter<App>> {
    [App.discover, App.updates, App.on_this_device, App.bots]
}

fn categories_of(items: List<App>) -> List<Text> {
    let seen: Map<Bool> = Map<Bool>()
    let out: List<Text> = []
    for a in items {
        for c in a.categories.sort_by(x -> x) {
            if seen.get(c) == .none {
                seen.put(c, true)
                out = [out, [c]].flat_map(x -> x)
            }
        }
    }
    out
}
