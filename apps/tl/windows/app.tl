app so.tilo.windows "Windows Apps" (
    icon: .app_window,
    category: .system,
    tagline: "Windows programs on this computer, and the Windows appliance",
    keywords: ["windows", "wine", "proton", "exe", "msi", "win32", "compatibility", "run windows", "dos", "legacy", "windows apps", "windows-apps", "windows programme", "windows anwendungen"],
    splash_budget: 250,
)

use so.tilo.device as device

/// How a program opens: with Wine or Proton in a private capsule, or in the Windows appliance.
enum OpenWith {
    wine
    proton
    vm
}

/// The launch routes the people sharing this workspace agreed on: the workspace's, so every
/// member opens a program the same way. Reporting what the computer runs is the device's
/// `windows-apps` resource; this only holds the choices.
type Windows {
    routes: Map<OpenWith>
}

root windows: Windows

/// What this device asked of Windows support, so its arrival is heard once.
type SupportWatch {
    /// The person started the install here; when the support is ready, they are told once.
    installing: Bool = false
}

device root support_watch: SupportWatch

default view window for Windows

/// The route stored for `program`, when someone chose one.
fn chosen_route(w: Windows, program: Text) -> Option<OpenWith> {
    w.routes.get(program)
}

/// How `program` opens now, as the workspace chose it or the catalogue recommends.
fn route_of(w: Windows, program: device.WindowsApp) -> device.Route {
    if let route = chosen_route(w, program.id) {
        match route {
            .wine => .wine,
            .proton => .proton,
            .vm => .vm,
        }
    } else { program.route }
}

/// The device's route for a workspace choice.
fn device_route(route: OpenWith) -> device.Route {
    match route {
        .wine => .wine,
        .proton => .proton,
        .vm => .vm,
    }
}

/// Whether a program opens with Wine or Proton, which needs Windows support installed.
fn needs_support(route: device.Route) -> Bool {
    route == .wine or route == .proton
}

/// How a route is said in the words of the person reading it.
fn route_words(route: device.Route) -> Text {
    if route == .wine {
        std.say("Wine")
    } else if route == .proton { std.say("Proton") } else { std.say("Windows VM") }
}

/// How an installation stands.
fn status_words(status: device.AppStatus) -> Text {
    if status == .ready {
        std.say("Ready")
    } else if status == .running {
        std.say("Running")
    } else if status == .needs-attention {
        std.say("Setup needs attention")
    } else if status == .installing {
        std.say("Installing")
    } else if status == .uninstalling {
        std.say("Uninstalling")
    } else if status == .failed { std.say("Failed") } else { std.say("Uninstalled") }
}

/// How well this computer's catalogue knows that a program runs.
fn confidence_words(confidence: device.Confidence) -> Text {
    if confidence == .verified {
        std.say("A verified recipe")
    } else if confidence == .community {
        std.say("A community recipe")
    } else if confidence == .heuristic { std.say("A best match") } else { std.say("Untested") }
}

/// Where this computer learned that a program runs.
fn source_words(source: device.Source) -> Text {
    if source == .catalogue {
        std.say("Tilo compatibility catalogue")
    } else if source == .default {
        std.say("Default settings")
    } else if source == .installed-in-windows {
        std.say("Installed in Windows")
    } else if source == .added-from-file {
        std.say("Added from a file")
    } else { std.say("Found by Windows setup") }
}

/// How many places a program may reach, in words.
fn places(count: Int) -> Text {
    if count == 0 {
        std.say("Off")
    } else if count == 1 { std.say("One place") } else { std.say("{count} places") }
}

fn on_off(on: Bool) -> Text {
    if on { std.say("On") } else { std.say("Off") }
}

/// What a program's network allows, in words.
fn network_words(network: Text) -> Text {
    if network == "none" {
        std.say("Off")
    } else if network == "unrestricted" { std.say("On") } else { network }
}

/// How far a setup got.
fn phase_words(phase: device.AppliancePhase) -> Text {
    if phase == .not-set-up {
        std.say("Not set up")
    } else if phase == .installing {
        std.say("Installing")
    } else if phase == .finishing-setup {
        std.say("Finishing setup")
    } else if phase == .ready {
        std.say("Ready")
    } else if phase == .needs-recovery { std.say("Needs recovery") } else { std.say("Unavailable") }
}

/// What Windows is doing now.
fn power_words(state: device.PowerState) -> Text {
    if state == .running {
        std.say("Running")
    } else if state == .suspended {
        std.say("Suspended")
    } else if state == .paused {
        std.say("Paused")
    } else if state == .shut-down { std.say("Shut down") } else { std.say("Unknown") }
}

/// Why the last Windows action did not happen, in the reader's words, with what the device said
/// in its own words kept beside it.
fn problem_words(problem: device.WindowsProblem) -> Text {
    let detail = problem.detail
    if problem.code == .command {
        std.say("The Windows appliance command failed: {detail}")
    } else if problem.code == .open {
        std.say("Couldn't open it: {detail}")
    } else if problem.code == .recovery-open {
        std.say("Couldn't open the Windows recovery desktop: {detail}")
    } else {
        let line = if problem.code == .unmanageable {
            std.say("The Windows appliance can't be managed in this session.")
        } else if problem.code == .busy {
            std.say("A Windows appliance action is already in progress.")
        } else if problem.code == .recovery-unavailable {
            std.say(
                "The Windows recovery desktop is unavailable until the appliance is provisioned.",
            )
        } else if problem.code == .set-up-power {
            std.say("Set up the Windows appliance before managing its power state.")
        } else if problem.code == .set-up-connection {
            std.say("Connect it temporarily only after setting up the Windows appliance.")
        } else if problem.code == .not-running {
            std.say("Windows must be running before it can be connected temporarily.")
        } else if problem.code == .connection-no-lease {
            std.say("There is no active temporary Windows connection to revoke.")
        } else if problem.code == .power-state {
            std.say("That isn't available in the appliance's current power state.")
        } else if problem.code == .app-gone {
            std.say("The Windows application is no longer available.")
        } else {
            std.say("The Windows operation is no longer available.")
        }
        if detail == "" { line } else { "{line} {detail}" }
    }
}

/// The one sentence the appliance page states its status in.
fn appliance_statement(appliance: device.Appliance) -> Text {
    if appliance.phase == .ready {
        if appliance.power_state == .running {
            std.say("Windows is running.")
        } else if appliance.power_state == .suspended {
            std.say("Windows is suspended.")
        } else if appliance.power_state == .paused {
            std.say("Windows is paused.")
        } else if appliance.power_state == .shut-down {
            std.say("Windows is shut down.")
        } else {
            std.say("Windows is ready for your apps.")
        }
    } else if appliance.phase == .not-set-up {
        std.say("The Windows appliance is not set up.")
    } else if appliance.phase == .installing {
        std.say("Windows is being set up.")
    } else if appliance.phase == .finishing-setup {
        std.say("Windows is finishing its setup.")
    } else if appliance.phase == .needs-recovery {
        std.say("Windows needs the recovery desktop.")
    } else {
        std.say("The Windows appliance is unavailable.")
    }
}

/// What Windows says of its licence.
fn license_words(license: device.License) -> Text {
    if license == .digital {
        std.say("Activated with a digital license")
    } else if license == .product-key {
        std.say("Activated with a product key")
    } else if license == .managed {
        std.say("Managed by an organisation")
    } else if license == .evaluation {
        std.say("An evaluation copy")
    } else if license == .unlicensed { std.say("Not activated") } else { std.say("Unknown") }
}

/// What the licence means when it is not the ordinary one.
fn license_detail(license: device.License) -> Text {
    if license == .unlicensed {
        std.say(
            "Windows says it isn't activated. Tilo doesn't keep product keys, so activate Windows inside the appliance.",
        )
    } else if license == .evaluation {
        std.say("Windows runs as an evaluation copy; it may ask for a product key later.")
    } else if license == .unknown {
        std.say("Windows has not said how it is activated.")
    } else { "" }
}

/// A duration as the person reads it.
fn duration_words(seconds: Int) -> Text {
    if seconds <= 0 {
        std.say("it has ended")
    } else if seconds < 60 {
        std.say("{seconds} seconds")
    } else if seconds < 3600 {
        std.say("{seconds / 60} minutes")
    } else { std.say("{seconds / 3600} hours") }
}

/// What a temporary connection was opened for, in the person's words.
fn purpose_words(purpose: Text) -> Text {
    if purpose == "activation" {
        std.say("Windows activation")
    } else if purpose == "update" { std.say("Windows Update") } else { purpose }
}

/// What Windows support's install is doing now, as the helper reports it.
fn support_progress(support: device.WindowsPrograms) -> Text {
    if let step = support.step {
        if let done = step.megabytes_done {
            if let total = step.megabytes_total {
                std.say("{step.label}: {done} of {total} MB")
            } else { std.say("{step.label}: {done} MB") }
        } else {
            std.say("{step.label} (step {step.number} of {step.count})")
        }
    } else { support.summary }
}

/// What installing Windows support asks of the person: what it is and how large the download is.
fn support_offer(support: device.WindowsPrograms) -> Text {
    if let size = support.size {
        std.say("{support.summary} It is a download of about {size}.")
    } else { support.summary }
}

/// Why Windows support is not installed: what the helper said, and what it blamed.
fn support_trouble(support: device.WindowsPrograms) -> Text {
    if let problem = support.problem {
        std.say("{support.summary} {problem}")
    } else { support.summary }
}

/// The programs a setup put on this computer.
fn setup_programs(ws: device.WindowsApps, capsule: Text) -> List<device.WindowsApp> {
    ws.apps.filter(program -> program.capsule == capsule and program.route != .vm)
}

/// The name a setup goes by: what its installer called it, else the programs it put there.
fn setup_name(ws: device.WindowsApps, capsule: Text) -> Text {
    let programs = setup_programs(ws, capsule)
    if let first = programs.first() {
        if let name = first.setup_name { name } else { "{programs.len()} programs" }
    } else { capsule }
}

/// Whether a program's setup can lose its data now: some program of it can.
fn setup_deletable(ws: device.WindowsApps, capsule: Text) -> Bool {
    setup_programs(ws, capsule).any(program -> program.can_delete_data)
}

/// Whether `program` opens in a private capsule of its own.
fn keeps_capsule(program: device.WindowsApp) -> Bool {
    program.route != .vm
}

/// What a grant of the appliance's shared session reaches, in words.
fn shared_grant_words(grant: Text) -> Text {
    if grant == "audio-playback" {
        std.say("Sound")
    } else if grant == "clipboard" {
        std.say("Clipboard")
    } else if grant == "microphone" {
        std.say("Microphone")
    } else if grant == "network-egress" {
        std.say("Network")
    } else if grant == "portal-files-read-only" {
        std.say("Files to read")
    } else if grant == "portal-files-read-write" { std.say("Files to change") } else { grant }
}

/// Choose how a program opens from now on, and carry the choice to this computer.
action set_route(w: Windows, program: Text, route: OpenWith) (
    did: match route {
    .wine => "set an app to open with Wine",
    .proton => "set an app to open with Proton",
    .vm => "set an app to open in the Windows VM",
},
) {
    w.routes.put(program, route)
    device.set_route(device.windows_apps, program: program, route: .some(device_route(route)))
    notice("This app opens this way from its next start.", kind: .success)
}

/// Let a program open the way the catalogue recommends, and carry that to this computer.
action use_recommendation(w: Windows, program: Text) (
    did: "let an app open the way the catalogue recommends",
) {
    w.routes.remove(program)
    device.set_route(device.windows_apps, program: program, route: .none)
    notice("This app opens the recommended way again.", kind: .success)
}

/// Open a program, saying so while it opens.
action open_program(program: Text) (icon: .play) {
    notice("Opening the app…", kind: .info)
    device.launch(device.windows_apps, program: program)
}

/// End a program, saying so while it ends.
action quit_program(program: Text) (icon: .square) {
    notice("Ending the app…", kind: .info)
    device.stop(device.windows_apps, program: program)
}

/// Open a program in the Windows appliance, saying so while it opens.
action open_appliance_program(program: Text) (icon: .monitor) {
    notice("Opening the app in the Windows appliance…", kind: .info)
    device.launch_vm_fallback(device.windows_apps, program: program)
}

/// Install the installer the person chose, or the one a file manager opened here.
primary action installed(w: Windows, file: File) (
    icon: .plus,
    label: "Install Windows application",
    opens: ["application/vnd.microsoft.portable-executable", "application/x-msi", .exe, .msi],
) {
    notice("Checking the installer and starting setup…", kind: .info)
    device.install(device.windows_apps, path: .some(file.path))
}

/// Set up the appliance from the Windows ISO the person chose.
action imported_iso(file: File) (icon: .monitor, label: "Import Windows ISO") {
    notice("Checking the Windows installation media…", kind: .info)
    device.import_iso(device.windows_apps, path: .some(file.path))
    notice("Windows media was accepted.", kind: .success)
}

/// Set a waiting setup up in a private capsule, saying so while it starts.
action install_in_capsule() (icon: .shield) {
    notice("Starting setup in a private capsule…", kind: .info)
    device.install_private_capsule(device.windows_apps)
}

/// Set a waiting setup up in the Windows appliance, saying so while Windows opens.
action install_in_windows() (icon: .monitor) {
    notice("Opening Windows setup…", kind: .info)
    device.install_windows_appliance(device.windows_apps)
}

/// Add a program the appliance published to Windows Apps, saying so while it is added.
action add_to_windows_apps(program: Text) {
    notice("Adding the app to Windows Apps…", kind: .info)
    device.publish(device.windows_apps, program: program)
}

/// Run a program's setup again, saying so while it retries.
action run_setup_again(program: Text) {
    notice("Retrying setup…", kind: .info)
    device.retry_install(device.windows_apps, capsule: program)
}

/// Continue a setup in the Windows appliance, saying so while Windows opens.
action continue_in_windows(capsule: Text) {
    notice("Continuing setup in the Windows appliance…", kind: .info)
    device.escalate_install(device.windows_apps, capsule: capsule)
}

/// Remove a program's launcher, and say what stayed.
action remove_launcher(program: Text) {
    device.remove_launcher(device.windows_apps, program: program)
    notice("Launcher removed. The app and its data were kept.", kind: .info)
}

/// Run a program's own uninstaller, saying so while it runs.
action uninstall_program(program: Text) {
    let ws = device.windows_apps
    let in_appliance = if let item = ws.apps.find(item -> item.id == program) {
        item.route == .vm
    } else { false }
    if in_appliance {
        notice("Running the app's uninstaller in the Windows appliance…", kind: .info)
    } else {
        notice("Running the app's uninstaller…", kind: .info)
    }
    device.uninstall(device.windows_apps, program: program)
}

/// Delete a setup and everything it stored, and say what went.
action delete_setup_data(capsule: Text) {
    let ws = device.windows_apps
    let programs = setup_programs(ws, capsule)
    device.delete_data(device.windows_apps, capsule: capsule)
    if programs.len() == 1 {
        notice("The app and all its data were deleted.", kind: .info)
    } else {
        notice("The setup's apps and all their data were deleted.", kind: .info)
    }
}

/// Install Windows support, and hear when it is there.
action install_support() (icon: .download) {
    support_watch.installing = true
    device.install(device.windows_compat)
}

/// Says the support the person asked for is installed, once it is ready.
action support_settled() {
    if support_watch.installing and device.windows_apps.support.state == .ready {
        notice("Windows support is installed. You can open the program now.", kind: .success)
        support_watch.installing = false
    }
}

/// Open the appliance's recovery desktop.
// The appliance's own operations, offered while the app is closed: search and the app's menu
// reach them without opening a window. The computer answers them, and says so when it cannot.
action recovery_desktop() (icon: .wrench, label: "Open recovery desktop") {
    device.recovery(device.windows_apps)
    notice("The Windows setup and recovery desktop is open.", kind: .success)
}

/// Start the appliance.
action start_windows() (icon: .play, label: "Start Windows") {
    notice("Starting Windows…", kind: .info)
    device.start(device.windows_apps)
    notice("Windows has started.", kind: .success)
}

/// Restart the appliance.
action restart_windows() (icon: .rotate_cw, label: "Restart Windows") {
    notice("Restarting Windows…", kind: .info)
    device.restart(device.windows_apps)
    notice("Windows has restarted.", kind: .success)
}

/// Suspend the appliance.
action suspend_windows() (icon: .pause, label: "Suspend Windows") {
    notice("Suspending Windows…", kind: .info)
    device.suspend(device.windows_apps)
    notice("Windows has been suspended.", kind: .success)
}

/// Resume the appliance.
action resume_windows() (icon: .play, label: "Resume Windows") {
    notice("Resuming Windows…", kind: .info)
    device.resume(device.windows_apps)
    notice("Windows has resumed.", kind: .success)
}

/// Shut the appliance down.
action shutdown_windows() (icon: .power, label: "Shut down Windows") {
    notice("Shutting down Windows…", kind: .info)
    device.shutdown(device.windows_apps)
    notice("Windows has been shut down.", kind: .success)
}

/// Connect the appliance for activation, for a while.
action temporary_network() (icon: .wifi, label: "Connect for activation") {
    notice("Connecting Windows temporarily…", kind: .info)
    device.temporary_network(device.windows_apps)
    notice("Windows has a temporary activation connection.", kind: .success)
}

/// Connect the appliance for Windows Update, for a while.
action update_network() (icon: .download, label: "Connect for Windows Update") {
    notice("Connecting Windows temporarily…", kind: .info)
    device.update_network(device.windows_apps)
    notice("Windows has a temporary update connection.", kind: .success)
}

/// End the appliance's temporary connection.
action revoke_network() (icon: .x, label: "End temporary network access") {
    device.revoke_network(device.windows_apps)
    notice("The temporary Windows connection was revoked.", kind: .info)
}

/// Open the document the person chose in `program`.
action opened_file(w: Windows, program: device.WindowsApp, file: File) {
    require program.portal_file_open_available "This program cannot receive a file"
    notice("Opening the file read-only in Windows…", kind: .info)
    device.open_file(device.windows_apps, program: program.id, path: .some(file.path))
}

/// The window: this computer's Windows programs down one side, the chosen one or the appliance
/// beside them.
view window(w: Windows) {
    state chosen: Text = ""
    state confirm_kind: Text = ""
    state confirm_target: Text = ""
    // Search finds every program this computer has and opens the window on one: it shows the
    // program, as choosing its row does.
    on show(program: device.WindowsApp): set(chosen, program.id)
    let ws = device.windows_apps
    let program = ws.apps.find(item -> item.id == chosen)
    ui.NavShell(content_kind: .page, title: "Windows Apps", sidebar_title: "Windows Apps") {
        sidebar: {
            stack {
                if not ws.serving {
                    ui.StatusLine(
                        "The Windows compatibility service is not running",
                        status: .warning,
                    )
                }
                if let problem = ws.problem {
                    ui.StatusLine(problem_words(problem), status: .error)
                }
                if let pending = ws.pending_install {
                    ui.ListRow(
                        "Install {pending.display_name}",
                        secondary: "Choose where it goes",
                        icon: .download,
                        selected: chosen == "pending",
                    ) {
                        on activate: set(chosen, "pending")
                    }
                }
                if ws.apps.is_empty() {
                    ui.EmptyState(
                        "No Windows apps",
                        message: "Install one with the button above.",
                        icon: .app_window,
                        inline: true,
                    )
                }
                stack(gap: .none) {
                    for item in ws.apps {
                        app_row(w, item, chosen)
                    }
                }
                ui.Section("The machine") {
                    appliance_row(ws, chosen)
                }
            }
        }
        actions: {
            ui.Button("Install a Windows app", icon: .plus, icon_only: true) {
                on press: file.choose(installed(w), accept: [".exe", ".msi"])
            }
        }
        stack {
            // The support the person asked for arrives in the device's reading: the window
            // hears it and says so once.
            if support_watch.installing {
                every 2s: support_settled()
            }
            if let end = ws.ended {
                if let why = end.error {
                    ui.Banner("{end.name} could not open", message: why, kind: .error)
                } else if not end.success {
                    ui.Banner(
                        "{end.name} stopped",
                        message: if let code = end.exit_code {
                            if let said = end.said {
                                "It stopped with code {code}: {said}"
                            } else { "It stopped with code {code}." }
                        } else if let said = end.said { said } else { "It stopped." },
                        kind: .warning,
                    )
                }
            }
            if chosen == "pending" {
                pending_page(ws)
            } else if chosen == "appliance" {
                appliance_page(w, ws)
            } else if let program = program {
                app_page(w, ws, program, confirm_kind, confirm_target)
            } else {
                ui.EmptyState(
                    "Windows apps",
                    message: if ws.serving {
                        if ws.apps.is_empty() {
                            "Install a Windows program with the button above, or set up the Windows appliance."
                        } else {
                            "Choose a program on the left to open it, choose how it opens, or manage it."
                        }
                    } else {
                        "This computer's Windows compatibility service is not running."
                    },
                    icon: .app_window,
                )
            }
        }
    }
    confirmation_dialogs(ws, program, confirm_kind, confirm_target)
}

/// One program in the list: its name, how it stands, and how it opens.
view app_row(w: Windows, item: device.WindowsApp, chosen: State<Text>) {
    ui.ListRow(
        item.name,
        secondary: "{item.publisher} - {status_words(item.status)}",
        badge: route_words(route_of(w, item)),
        meta_tone: if item.status == .failed {
            .danger
        } else if item.status == .needs-attention { .warning } else { .neutral },
        icon: .app_window,
        selected: chosen == item.id,
    ) {
        on activate: set(chosen, item.id)
    }
}

/// The appliance in the list: how it stands.
view appliance_row(ws: device.WindowsApps, chosen: State<Text>) {
    ui.ListRow(
        "Windows appliance",
        secondary: if ws.appliance.configured {
            "{phase_words(ws.appliance.phase)} - {power_words(ws.appliance.power_state)}"
        } else { phase_words(ws.appliance.phase) },
        icon: .monitor,
        selected: chosen == "appliance",
    ) {
        on activate: set(chosen, "appliance")
    }
}

/// The banner that says Windows support must be installed before a program can open: is it
/// missing and how large the download is, is it being fetched and how far it has got, or did it
/// fail and why.
view support_banner(ws: device.WindowsApps) {
    if ws.support.state == .needs-install {
        ui.Banner(
            "Windows support isn't installed",
            message: support_offer(ws.support),
            kind: .warning,
            action: "Install",
        ) {
            on act: install_support()
        }
    } else if ws.support.state == .failed {
        ui.Banner(
            "Windows support couldn't be installed",
            message: support_trouble(ws.support),
            kind: .error,
            action: "Try again",
        ) {
            on act: install_support()
        }
    } else if ws.support.state == .installing {
        ui.Banner(
            "Installing Windows support…",
            message: support_progress(ws.support),
            kind: .info,
        )
    } else if ws.support.state == .waiting-for-network {
        ui.Banner(
            "Windows support installs once you're online",
            message: "The download starts by itself when this device has a network.",
            kind: .info,
        )
    }
}

/// The page of one program: opening it, how it opens, what it may reach, and managing it.
view app_page(
    w: Windows,
    ws: device.WindowsApps,
    program: device.WindowsApp,
    confirm_kind: State<Text>,
    confirm_target: State<Text>,
) {
    let route = route_of(w, program)
    if program.status == .failed {
        ui.Banner(
            "Its setup did not finish",
            message: if let detail = program.installation {
                detail
            } else {
                "The installer stopped before the program was ready."
            },
            kind: .error,
            action: if program.can_retry_install { "Run the setup again" } else { "" },
        ) {
            on act: run_setup_again(program.capsule)
        }
    }
    if needs_support(route) {
        support_banner(ws)
    }
    row(gap: .tight, wrap: true) {
        ui.Button("Open", icon: .play, role: .primary) {
            on press: open_program(program.id)
        }
        if program.status == .running and program.route != .vm {
            ui.Button("Stop", icon: .square, role: .secondary) {
                on press: quit_program(program.id)
            }
        }
        if program.route == .vm {
            ui.Button("Open a file in it", icon: .file_plus, role: .secondary) {
                on press: file.choose(opened_file(w, program))
            }
        } else if program.vm_fallback_available {
            ui.Button("Open in Windows", icon: .monitor, role: .secondary) {
                on press: open_appliance_program(program.id)
            }
        }
    }
    if not program.launch_available {
        if let reason = program.launch_unavailable_reason {
            ui.Text(reason, role: .caption, secondary: true)
        }
    }
    if not program.portal_file_open_available {
        if let reason = program.portal_file_open_unavailable_reason {
            ui.Text(reason, role: .caption, secondary: true)
        }
    }
    ui.Section("Open with") {
        if program.route_override_available {
            row(gap: .tight, wrap: true) {
                route_choice(w, program, route, .wine)
                route_choice(w, program, route, .proton)
                if program.route == .vm or program.recommended_route == .vm {
                    route_choice(w, program, route, .vm)
                }
            }
            if chosen_route(w, program.id) != .none {
                ui.Button("Let the catalogue decide", size: .small) {
                    on press: use_recommendation(w, program.id)
                }
            }
        } else {
            ui.Text(route_words(route), role: .body)
            if let reason = program.route_override_unavailable_reason {
                ui.Text(reason, role: .caption, secondary: true)
            }
        }
        if program.reason != "" {
            ui.Text(program.reason, role: .caption, secondary: true)
        }
    }
    ui.Section("What it may reach") {
        if program.route == .vm {
            ui.Text(
                "It shares Windows with your other Windows apps. Files reach it only when you open them with it.",
                role: .caption,
                secondary: true,
            )
        } else {
            ui.Text(
                "It runs in a private capsule that sees only the files and devices you allow.",
                role: .caption,
                secondary: true,
            )
        }
        ui.PropertyList {
            ui.Property("Files it can read", places(program.access.files_read_only))
            ui.Property("Files it can change", places(program.access.files_read_write))
            ui.Property("Network", network_words(program.access.network))
            ui.Property("Microphone", on_off(program.access.microphone))
            ui.Property("Sound", on_off(program.access.sound))
            if program.route == .vm {
                ui.Property(
                    "The shared session reaches",
                    if ws.appliance.shared_session.grants.is_empty() {
                        "Nothing"
                    } else {
                        ws.appliance.shared_session.grants
                            .map(grant -> shared_grant_words(grant))
                            .join(", ")
                    },
                )
                if not ws.appliance.shared_session.contributors.is_empty() {
                    ui.Property(
                        "Open with you",
                        ws.appliance.shared_session.contributors.join(", "),
                    )
                }
            }
        }
        if program.route == .vm {
            ui.Text(
                "Apps in the Windows appliance share one Windows user, so any file, clipboard or network access one of them gets is shared with the others.",
                role: .caption,
                secondary: true,
            )
        }
        if program.fallback_detail != "" {
            ui.Text(program.fallback_detail, role: .caption, secondary: true)
        }
    }
    ui.Section("About") {
        ui.PropertyList {
            if program.status != .failed {
                ui.Property("Status", status_words(program.status))
            }
            ui.Property("Publisher", program.publisher)
            ui.Property("Learned from", source_words(program.source))
            ui.Property("How well it is known", confidence_words(program.confidence))
            ui.Property("Runs on", program.runner)
            if let detail = program.installation {
                if program.status != .failed {
                    ui.Property("Installation", detail)
                }
            }
        }
    }
    if keeps_capsule(program) and setup_deletable(ws, program.capsule) {
        let name = setup_name(ws, program.capsule)
        let programs = setup_programs(ws, program.capsule)
        ui.Section("Its setup") {
            ui.Text(
                if programs.len() == 1 {
                    "{name} installed this program into a private capsule of its own."
                } else {
                    "{name} installed {programs.len()} programs into one private capsule they share."
                },
                role: .caption,
                secondary: true,
            )
        }
    }
    manage_row(ws, program, confirm_kind, confirm_target)
}

/// One way a program can open.
view route_choice(w: Windows, program: device.WindowsApp, current: device.Route, route: OpenWith) {
    ui.Chip(route_words(device_route(route)), selected: current == device_route(route)) {
        on press: set_route(w, program.id, route)
    }
}

/// The operations that keep a program's installation in order.
view manage_row(
    ws: device.WindowsApps,
    program: device.WindowsApp,
    confirm_kind: State<Text>,
    confirm_target: State<Text>,
) {
    let setup = setup_deletable(ws, program.capsule) and keeps_capsule(program)
    if program.can_publish_vm_candidate
        or program.can_retry_install
        or program.can_escalate_install_to_vm
        or program.can_remove_launcher
        or program.can_vendor_uninstall
        or setup {
        ui.Section("Manage the installation") {
            row(gap: .tight, wrap: true) {
                if program.can_publish_vm_candidate {
                    ui.Button("Add to Windows Apps", role: .secondary) {
                        on press: add_to_windows_apps(program.id)
                    }
                }
                if program.can_retry_install {
                    ui.Button("Run the setup again", role: .secondary) {
                        on press: run_setup_again(program.capsule)
                    }
                }
                if program.can_escalate_install_to_vm {
                    ui.Button("Continue in Windows", role: .secondary) {
                        on press: set(confirm_target, program.capsule)
                            .then(set(confirm_kind, "escalate"))
                    }
                }
                if program.can_remove_launcher {
                    ui.Button("Remove the launcher", role: .secondary) {
                        on press: remove_launcher(program.id)
                    }
                }
                if program.can_vendor_uninstall {
                    ui.Button("Uninstall", icon: .trash, role: .destructive) {
                        on press: set(confirm_target, program.id)
                            .then(set(confirm_kind, "uninstall"))
                    }
                }
                if setup {
                    ui.Button("Delete its data", icon: .trash, role: .destructive) {
                        on press: set(confirm_target, program.capsule)
                            .then(set(confirm_kind, "delete"))
                    }
                }
            }
        }
    }
}

/// The appliance: what it is doing, its power, its recovery desktop, and its temporary
/// connection.
view appliance_page(w: Windows, ws: device.WindowsApps) {
    let appliance = ws.appliance
    if not appliance.configured or appliance.phase == .not-set-up {
        ui.Banner(
            "Set up the Windows appliance",
            message: "Choose a Windows ISO and Tilo sets up a Windows computer for the programs Wine cannot run.",
            kind: .info,
            action: "Choose a Windows ISO",
        ) {
            on act: file.choose(imported_iso(), accept: [".iso"])
        }
        if let detail = appliance.detail {
            ui.Text(detail, role: .caption, secondary: true)
        }
    } else {
        ui.Text(appliance_statement(appliance), role: .body)
        if appliance.phase == .unavailable {
            if let detail = appliance.detail {
                ui.Text(
                    "The Windows appliance reported a status this version can't read: {detail}",
                    role: .caption,
                    secondary: true,
                )
            }
        }
        ui.Text(
            "Apps open in their own windows. The full Windows desktop is only for setup and recovery.",
            role: .caption,
            secondary: true,
        )
        ui.PropertyList {
            ui.Property("Activation", license_words(appliance.license))
            ui.Property("Published apps", "{appliance.published_applications}")
        }
        let detail = license_detail(appliance.license)
        if detail != "" {
            ui.Text(detail, role: .caption, secondary: true)
        }
        ui.Section("Power") {
            row(gap: .tight, wrap: true) {
                if appliance.power_state == .shut-down or appliance.power_state == .unknown {
                    ui.Button("Start Windows", icon: .play, role: .secondary) {
                        on press: device.start(device.windows_apps)
                    }
                }
                if appliance.power_state == .suspended or appliance.power_state == .paused {
                    ui.Button("Resume Windows", icon: .play, role: .secondary) {
                        on press: device.resume(device.windows_apps)
                    }
                }
                if appliance.power_state == .running {
                    ui.Button("Restart Windows", icon: .rotate_cw, role: .secondary) {
                        on press: device.restart(device.windows_apps)
                    }
                    ui.Button("Suspend Windows", icon: .pause, role: .secondary) {
                        on press: device.suspend(device.windows_apps)
                    }
                    ui.Button("Shut Windows down", icon: .power, role: .destructive) {
                        on press: device.shutdown(device.windows_apps)
                    }
                }
            }
        }
        ui.Section("Setup recovery") {
            ui.Text(
                "Open a Windows desktop that starts even when Windows itself will not.",
                role: .caption,
                secondary: true,
            )
            row(gap: .tight, wrap: true) {
                ui.Button("Open the recovery desktop", icon: .wrench, role: .secondary) {
                    on press: device.recovery(device.windows_apps)
                }
            }
        }
        ui.Section("Temporary connection") {
            if let connection = appliance.temporary_connection {
                ui.Text(
                    "Connected for {purpose_words(connection.purpose)} for another {duration_words(connection.expires_in_seconds)}.",
                    role: .body,
                )
                row(gap: .tight, wrap: true) {
                    ui.Button("End the connection", icon: .x, role: .destructive) {
                        on press: device.revoke_network(device.windows_apps)
                    }
                }
            } else {
                ui.Text(
                    if appliance.power_state == .running {
                        "Windows reaches nothing on your network unless you connect it for a while: activation and Windows Update need that."
                    } else {
                        "Start Windows to connect it for activation or Windows Update."
                    },
                    role: .caption,
                    secondary: true,
                )
                row(gap: .tight, wrap: true) {
                    ui.Button("Connect for activation", icon: .wifi, role: .secondary) {
                        on press: device.temporary_network(device.windows_apps)
                    }
                    ui.Button("Connect for Windows Update", icon: .download, role: .secondary) {
                        on press: device.update_network(device.windows_apps)
                    }
                }
            }
        }
    }
}

/// The setup that waits for the person to choose where it goes.
view pending_page(ws: device.WindowsApps) {
    if let pending = ws.pending_install {
        ui.ObjectHeader(pending.display_name, caption: "Ready to install", icon: .download)
        if pending.detail != "" {
            ui.Text(pending.detail, role: .body)
        }
        ui.Section("Install it in") {
            if pending.recommended_route == .vm {
                ui.ListRow(
                    "Windows",
                    secondary: "In the Windows appliance, beside the programs Wine cannot run.",
                    value: "Recommended",
                    icon: .monitor,
                ) {
                    on activate: install_in_windows()
                }
                ui.ListRow(
                    "A private capsule",
                    secondary: "Only this program, kept apart from the rest of the computer.",
                    icon: .shield,
                ) {
                    on activate: install_in_capsule()
                }
            } else {
                ui.ListRow(
                    "A private capsule",
                    secondary: "Only this program, kept apart from the rest of the computer.",
                    value: "Recommended",
                    icon: .shield,
                ) {
                    on activate: install_in_capsule()
                }
                ui.ListRow(
                    "Windows",
                    secondary: "In the Windows appliance, beside the programs Wine cannot run.",
                    icon: .monitor,
                ) {
                    on activate: install_in_windows()
                }
            }
        }
    }
}

/// The confirmations the manage actions open.
view confirmation_dialogs(
    ws: device.WindowsApps,
    program: Option<device.WindowsApp>,
    confirm_kind: State<Text>,
    confirm_target: State<Text>,
) {
    // Each dialog stays while what it is about is still so: once the capsule's programs go, or
    // the setup starts again, the reading it is opened from has changed and it closes.
    if confirm_kind == "delete" and setup_deletable(ws, confirm_target) {
        let programs = setup_programs(ws, confirm_target)
        let names = programs.map(program -> program.name).join(", ")
        ui.ModalDialog(
            title: "Delete {setup_name(ws, confirm_target)}'s data?",
            description: .some(
                "Everything the setup stored in its private capsule goes: {names}.",
            ),
            open: true,
            primary: "Delete the data",
            secondary: "Cancel",
            destructive: true,
        ) {
            on act_primary: delete_setup_data(confirm_target)
            on act_secondary: set(confirm_kind, "")
            on dismiss: set(confirm_kind, "")
        }
    } else if confirm_kind == "escalate"
        and ws.apps.any(item -> item.capsule == confirm_target and item.can_escalate_install_to_vm) {
        ui.ModalDialog(
            title: "Continue the setup in Windows?",
            description: .some(
                "The installer runs again in the Windows appliance, and the program opens there from then on.",
            ),
            open: true,
            primary: "Continue in Windows",
            secondary: "Cancel",
            destructive: false,
        ) {
            on act_primary: continue_in_windows(confirm_target)
            on act_secondary: set(confirm_kind, "")
            on dismiss: set(confirm_kind, "")
        }
    } else if confirm_kind == "uninstall"
        and ws.apps.any(item -> item.id == confirm_target and item.can_vendor_uninstall) {
        ui.ModalDialog(
            title: "Uninstall {if let item = program { item.name } else { confirm_target }}?",
            description: .some(
                "The program's own uninstaller runs. Your files stay unless it removes them.",
            ),
            open: true,
            primary: "Uninstall",
            secondary: "Cancel",
            destructive: true,
        ) {
            on act_primary: uninstall_program(confirm_target)
            on act_secondary: set(confirm_kind, "")
            on dismiss: set(confirm_kind, "")
        }
    }
}
