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
    windows_support_section()
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

/// Everything that can act as the person — their computers, the agents they let in and the keys
/// they made for their own programs — with which of them reach this device and how; and the
/// colour they show as.
view devices_pane() {
    state renaming: Text = ""
    state name_draft: Text = ""
    let listed = device.devices
    let server = mcp.server
    let me = settings.identity
    if listed.refused != "" {
        ui.StatusLine(listed.refused, status: .error)
    }
    if not listed.signed_in {
        ui.Section("Add this device") {
            ui.Text(
                "This device is not one of yours yet, so nothing here acts as you. Add it to your own with a code from another of your devices.",
                role: .body,
            )
            ui.Button("Add a device", icon: .monitor_smartphone, role: .primary) {
                on press: device.show_pair_device(device.invite)
            }
        }
    } else {
        ui.Section("Everything that can act as you") {
            ui.Text("Remove anything you don't recognize.", role: .caption, secondary: true)
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Add a device", icon: .monitor_smartphone, role: .primary) {
                    on press: device.show_pair_device(device.invite)
                }
                ui.Button("Connect an agent or computer", icon: .plus) {
                    on press: device.show_connect(device.invite)
                }
            }
        }
        let agents = listed.access.filter(
            one -> one.kind == .agent or one.agent != "" or one.kind == .script,
        )
        ui.Section("Agents") {
            if agents.is_empty() {
                ui.Text("No agent acts as you yet.", role: .caption, secondary: true)
            }
            for one in agents {
                let grant = server.grants.find(g -> g.id == one.id)
                ui.ListRow(
                    one.label,
                    secondary: if one.revoked {
                        "Removed; every device of yours refuses it"
                    } else { agent_reach(grant) },
                    note: if one.revoked {
                        ""
                    } else if let g = grant { agent_working(g) } else { "" },
                    icon: if one.kind == .script { .key } else { .bot },
                ) {
                    trailing: {
                        row(gap: .tight, align: .center) {
                            if let g = grant {
                                if g.stopped {
                                    ui.Button("Let it go on", size: .small) {
                                        on press: mcp.go_on(
                                            mcp.server,
                                            session: mcp.AgentSessionRef(
                                                grant: g.id,
                                                session: g.agent,
                                            ),
                                        )
                                    }
                                }
                                for running in g.sessions {
                                    ui.Button(
                                        "Stop {running.name}",
                                        size: .small,
                                        role: .destructive,
                                    ) {
                                        on press: mcp.stop(
                                            mcp.server,
                                            session: mcp.AgentSessionRef(
                                                grant: g.id,
                                                session: running.name,
                                            ),
                                        )
                                    }
                                }
                            }
                            if not one.revoked {
                                manage_menu(one, renaming, name_draft, removable: true)
                            }
                        }
                    }
                }
                if not one.revoked {
                    agent_facts(one, grant)
                }
                if renaming == one.id {
                    name_field(one, renaming, name_draft)
                }
            }
        }
        let computers = listed.access.filter(one -> one.kind == .device and one.agent == "")
        ui.Section("Browsers and computers") {
            if computers.is_empty() {
                ui.Text(
                    "No browser or computer acts as you yet, beside this one.",
                    role: .caption,
                    secondary: true,
                )
            }
            for one in computers {
                ui.ListRow(
                    if one.current { "This computer" } else { one.label },
                    secondary: if one.current and not one.revoked {
                        "This computer; your other devices reach it"
                    } else { access_words(one) },
                    note: one.fingerprint,
                    selected: one.current,
                    icon: .monitor,
                ) {
                    trailing: {
                        row(gap: .tight, align: .center) {
                            if let when = one.last_active_ms {
                                if not one.current {
                                    ui.Time(
                                        instant(when),
                                        role: .caption,
                                        past: true,
                                        secondary: true,
                                    )
                                }
                            }
                            if not one.revoked {
                                manage_menu(one, renaming, name_draft, removable: not one.current)
                            }
                        }
                    }
                }
                if not one.revoked {
                    computer_facts(one)
                }
                if renaming == one.id {
                    name_field(one, renaming, name_draft)
                }
            }
        }
        ui.Section("How your devices reach each other") {
            let others = listed.devices.filter(one -> not one.local)
            if others.is_empty() {
                ui.Text(
                    "No other device of yours is in reach now.",
                    role: .caption,
                    secondary: true,
                )
            }
            for one in others {
                ui.ListRow(one.label, secondary: path_words(one), icon: .monitor)
            }
            if not listed.workspaces.is_empty() {
                ui.Text(
                    "A device is reached directly when it is on your network, and otherwise through the workspace this device syncs with.",
                    role: .caption,
                    secondary: true,
                )
            }
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

/// The row's own menu: give it another name, and — when the person may take it back here — the
/// four reasons a key's access ends, each its own item, since what a list of keys keeps is why it
/// ended.
template view manage_menu(
    one: device.AccessKey,
    renaming: State<Text>,
    draft: State<Text>,
    removable: Bool,
) {
    ui.MenuButton("Manage {one.label}", icon: .ellipsis) {
        ui.MenuItem("Name {one.label}…") {
            on choose: set(renaming, one.id).then(set(draft, one.label))
        }
        if removable {
            ui.MenuSeparator()
            ui.MenuItem("Remove {one.label} — no longer used", destructive: true, icon: .trash) {
                on choose: device.revoke(device.devices, device: one.id, reason: "retired")
            }
            ui.MenuItem("Remove {one.label} — it was lost", destructive: true, icon: .trash) {
                on choose: device.revoke(device.devices, device: one.id, reason: "lost")
            }
            ui.MenuItem("Remove {one.label} — it was stolen", destructive: true, icon: .trash) {
                on choose: device.revoke(device.devices, device: one.id, reason: "stolen")
            }
            ui.MenuItem(
                "Remove {one.label} — someone knows its key",
                destructive: true,
                icon: .trash,
            ) {
                on choose: device.revoke(device.devices, device: one.id, reason: "compromised")
            }
        }
    }
}

/// The field that names `one`, open after the row's own menu asked for it: what it reads by is in
/// the field, and Save hands the new name to the record of the person's devices.
template view name_field(one: device.AccessKey, renaming: State<Text>, draft: State<Text>) {
    row(gap: .related, align: .center, wrap: true) {
        ui.TextInput(bind: draft, placeholder: "The name {one.label} should read by", fill)
        ui.Button("Save name", role: .primary, size: .small) {
            on press: device
                .rename(device.devices, device: one.id, name: draft)
                .then(set(renaming, ""))
                .then(set(draft, ""))
        }
        ui.Button("Cancel", size: .small) {
            on press: set(renaming, "").then(set(draft, ""))
        }
    }
}

/// What an agent's grant lets it reach, as its row says it.
fn agent_reach(grant: Option<mcp.Grant>) -> Text {
    if let g = grant {
        if g.reach.kind == "workspace" {
            if let name = g.reach.name {
                std.say("Only in {name}")
            } else { std.say("Only in one workspace") }
        } else { std.say("In all your workspaces") }
    } else { std.say("Acts as you") }
}

/// How an agent's grant stands now: what it works in, when it last acted, or when it was let in.
fn agent_working(g: mcp.Grant) -> Text {
    let working = g.sessions.len()
    if g.stopped {
        std.say("Stopped; it can't act until you let it go on")
    } else if working == 1 {
        std.say("Working now")
    } else if working > 1 {
        std.say("{working} sessions working now")
    } else if let last = g.last_ms {
        std.say("Last active {day(last)}")
    } else { std.say("Let in {day(g.granted_ms)}") }
}

/// What a device key's row says under its name: whether it reaches this device, and when it last
/// did.
fn access_words(one: device.AccessKey) -> Text {
    if one.revoked {
        std.say("Removed; every device of yours refuses it")
    } else if one.current {
        std.say("This computer")
    } else if one.online and one.nearby {
        std.say("Online, on your network")
    } else if one.online {
        std.say("Online, through your workspace")
    } else if let when = one.last_active_ms {
        std.say("Offline, last seen {day(when)}")
    } else { std.say("Offline") }
}

/// How this device reaches another, if it does.
fn path_words(one: device.PersonalDeviceView) -> Text {
    if one.online and one.nearby {
        std.say("Directly, on your network")
    } else if one.online { std.say("Through your workspace") } else { std.say("Out of reach now") }
}

/// The facts a key holds that a person reads once, under its row: when it was added and when it
/// stops being good.
view agent_facts(one: device.AccessKey, grant: Option<mcp.Grant>) {
    row(gap: .tight, wrap: true) {
        if let when = one.issued_ms {
            ui.Text("Added {day(when)}", role: .caption, secondary: true)
        }
        if let when = one.expires_ms {
            ui.Text("Ends {day(when)}", role: .caption, secondary: true)
        }
        ui.Text(one.fingerprint, role: .caption, secondary: true)
        if let g = grant {
            if g.browser_pages {
                ui.Text("Operates web pages in Browser", role: .caption, secondary: true)
            } else {
                ui.Text(
                    "Asks before operating web pages in Browser",
                    role: .caption,
                    secondary: true,
                )
            }
        }
    }
}

/// The facts a computer's key holds: when it was added and when it stops being good.
view computer_facts(one: device.AccessKey) {
    row(gap: .tight, wrap: true) {
        if let when = one.issued_ms {
            ui.Text("Added {day(when)}", role: .caption, secondary: true)
        }
        if let when = one.expires_ms {
            ui.Text("Ends {day(when)}", role: .caption, secondary: true)
        }
        if one.current {
            ui.Text("To remove it, use another of your devices.", role: .caption, secondary: true)
        }
    }
}

/// The day and clock time `at_ms` names, as the person reads it.
fn day(at_ms: Int) -> Text {
    let at = instant(at_ms)
    std.say("{at.day()}, {at.clock()}")
}

/// The instant a count of milliseconds names.
fn instant(at_ms: Int) -> Time {
    Time("1970-01-01T00:00:00Z") + Duration(at_ms / 1000)
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
        setting(
            "A screen nearby becomes this one's input",
            "This device uses the screen and keyboard of an enrolled device beside it without asking each time",
        ) {
            ui.Button(
                if c.automatic_display_input { "On" } else { "Off" },
                toggled: c.automatic_display_input,
            ) {
                on press: settings.set_automatic_display_input(
                    settings.continuity,
                    on: not c.automatic_display_input,
                )
            }
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

/// This copy of Tilo: the system it runs, what it is, what it recorded of the times it went
/// wrong, and the page of one of those that the person opened.
view about_pane() {
    state problem: Text = ""
    if problem != "" {
        problem_page(problem, problem)
    } else {
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
        problems_section(problem)
    }
}

/// The problems this computer recorded — a desktop that stopped, the computer itself stopping, a
/// fault the system's core went on from — each opening the page of its report.
view problems_section(problem: State<Text>) {
    let reports = crashes.reports.reports
    ui.Section("Problems") {
        if reports.is_empty() {
            ui.Text("No problems recorded", role: .caption, secondary: true)
        } else {
            for report in reports.take(50) {
                ui.ListRow(
                    crash_kind_words(report.kind),
                    secondary: report.when,
                    icon: .triangle_alert,
                ) {
                    on activate: set(problem, report.id)
                }
            }
            ui.Text("These reports stay on this computer.", role: .caption, secondary: true)
        }
    }
}

/// One problem's page: what happened and when, and the report the computer wrote, its stacks
/// included.
view problem_page(id: Text, problem: State<Text>) {
    let listed = crashes.reports.reports
    let found = listed.find(r -> r.id == id)
    let record = resource("crash-report:{id}", of: crashes.CrashReportText)
    ui.Section("What happened") {
        ui.ListRow("Back", icon: .chevron_left) {
            on activate: set(problem, "")
        }
        if let report = found {
            ui.PropertyList {
                ui.Property("Problem", crash_kind_words(report.kind))
                ui.Property("When", report.when)
                if let reason = report.reason {
                    ui.Property("Why", crash_reason_words(reason))
                }
                if let version = report.version {
                    ui.Property("Version", version)
                }
                if let build = report.build {
                    ui.Property("Build", build)
                }
            }
        }
    }
    ui.Section("Record") {
        ui.Text(record.text, role: .code)
        if record.truncated {
            ui.Text("The record goes on beyond what is shown.", role: .caption, secondary: true)
        }
        if found == .none {
            ui.Text("This record is no longer on this computer.", role: .caption, secondary: true)
        }
    }
    ui.Section("What you can do") {
        ui.Text(
            "A record stays on this computer unless you send it or remove it.",
            role: .caption,
            secondary: true,
        )
        if let report = found {
            ui.Button("Send a report", icon: .flag, role: .primary) {
                on press: send_problem_report(problem_report_words(report, record.text))
            }
            ui.Text(
                "It is filed in the feedback channel of the workspace this window is in, where the people who make Tilo read what went wrong.",
                role: .caption,
                secondary: true,
            )
            ui.Button("Remove it from this computer", icon: .trash, role: .destructive) {
                on press: crashes.remove(crashes.reports, report: id)
            }
        }
    }
}

/// A problem report as Chat files it: the words it carries.
type ProblemReport {
    body: Text
}

/// Files this computer's record of a problem as a problem report in the workspace's feedback
/// channel, where the people who make Tilo read what went wrong, with the record's own words.
action send_problem_report(body: Text) (icon: .flag, label: "Send a report") {
    if let ask = problem_report_ask(body) {
        actions.call(address: "so.tilo.chat/conversations#report_problem", arguments: ask)
    }
}

/// What Chat's problem report is called with: the words this computer's record reads as, under the
/// name its action takes them by.
fn problem_report_ask(body: Text) -> Option<json.Value> {
    match json.parse(json.encode(ProblemReport(body: body))) {
        .ok(ask) => .some(ask),
        .err(why) => .none,
    }
}

/// The words a report about a problem carries: what happened and when, the system that wrote the
/// record and the exact build, then the record itself with its stacks.
fn problem_report_words(report: crashes.CrashReport, record: Text) -> Text {
    "A problem on this computer: "
        + crash_kind_words(report.kind)
        + ", "
        + report.when
        + report_reason_line(report)
        + report_version_line(report)
        + "\n\n"
        + record
}

/// Why the system stopped, when the report says, as the page reads it.
fn report_reason_line(report: crashes.CrashReport) -> Text {
    if let reason = report.reason { "\nWhy: " + crash_reason_words(reason) } else { "" }
}

/// The system and the exact build that wrote the report, when it names them.
fn report_version_line(report: crashes.CrashReport) -> Text {
    let version = report.version ?? ""
    let build = report.build ?? ""
    let system = if version != "" { "\nSystem: " + version } else { "" }
    if build != "" { system + "\nBuild: " + build } else { system }
}

/// What a report says happened, in the person's words.
fn crash_kind_words(kind: crashes.CrashKind) -> Text {
    if kind == .froze {
        std.say("The desktop stopped responding")
    } else if kind == .crashed {
        std.say("The desktop stopped unexpectedly")
    } else if kind == .part-restarted {
        std.say("Part of the desktop restarted")
    } else if kind == .computer-stopped {
        std.say("The computer stopped")
    } else {
        std.say("The system ran into a fault and kept running")
    }
}

/// What the kernel met, in the person's words.
fn crash_reason_words(reason: crashes.StopReason) -> Text {
    if reason == .panic {
        std.say("The system met an error it could not recover from")
    } else if reason == .oops {
        std.say("A fault in the system's core")
    } else {
        std.say("The system restarted itself at once")
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
