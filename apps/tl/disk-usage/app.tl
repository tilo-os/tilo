app so.tilo.disk-usage "Disk Usage Analyzer" (
    icon: .hard_drive,
    category: .system,
    tagline: "Shows which folders and files take up space",
    keywords: ["disk space", "storage", "space", "large files", "cleanup", "clean up", "free space", "what takes space", "disk usage analyzer", "baobab", "filelight", "speicherplatz", "festplattenplatz", "platz", "belegt", "aufräumen", "aufraeumen", "große dateien", "grosse dateien", "speicherbelegung"],
)

use so.tilo.session as session

op "scan"
op "entries"

/// One folder in the folder measured: what it holds, all the way down.
type Measure {
    name: Text
    path: Text
    bytes: Int
    files: Int
}

/// A folder measured lately, offered again.
type Recent (title: label) {
    path: Text
    label: Text
    /// When it was measured last, newest first.
    seen: Time
}

/// The analyzer: the folder measured now or last, what its folders take, and the folders
/// measured lately.
type Analyzer (title: label, untitled: "Disk Usage") {
    /// The folder measured or being measured; empty while the places page shows.
    folder: Text
    /// What the folder is called: a place by its name, any other folder by its own.
    label: Text
    /// The folders directly in it, each with what it holds all the way down.
    entries: Collection<Measure>
    /// What the measured folders hold together, and how many files they hold.
    total: Int
    files: Int
    /// How many entries of the folder could not be read.
    unreadable: Int
    /// Why the folder could not be read; empty while it can.
    trouble: Text
    /// What the window says under the folders: that a measuring was stopped, and when.
    notice: Text
    /// The measuring under way, by the job the device gave it; empty when nothing runs.
    job: Text
    /// How much the measuring has seen so far.
    seen_bytes: Int
    seen_files: Int
    /// The folder an analysis started from: going back from it shows the places page again.
    root: Text
    /// The folders measured lately, newest first.
    recents: Collection<Recent>
    /// Whether the window asks for the folder to measure.
    choosing: Bool
    /// The folder the person walks through while choosing, and the folders in it.
    picker: Text
    picks: Collection<Measure>
}

root analyzer: Analyzer

default view analyzer for Analyzer

/// The places the app offers: this device's home folder, the workspace's files and the file
/// system.
fn place_label(path: Text) -> Text {
    if path == "~" {
        "Home"
    } else if path == "/" {
        "File system"
    } else if path == "/var/workspace" {
        "Workspace"
    } else if let name = path.split("/").filter(p -> p != "").last() { name } else { path }
}

/// The folder above `path`, or `/` at the top.
fn parent_of(path: Text) -> Text {
    if path == "/" or path == "~" or path == "" {
        "/"
    } else {
        let parts = path.split("/").filter(p -> p != "")
        if parts.len() <= 1 {
            "/"
        } else {
            "/" + parts.take(parts.len() - 1).join("/")
        }
    }
}

/// The folder the person chose, made the folder measured now and its own chain's start.
action analyze(a: Analyzer, path: Text, label: Text) uses files, time {
    let rooted = if path == "" { "~" } else { path }
    if a.job != "" { engine.cancel(a.job) }
    a.folder = rooted
    a.label = if label == "" { place_label(rooted) } else { label }
    a.root = a.folder
    a.job = ""
    a.notice = ""
    a.trouble = ""
    a.total = 0
    a.files = 0
    a.unreadable = 0
    a.seen_bytes = 0
    a.seen_files = 0
    for m in a.entries {
        a.entries.remove(m)
    }
    match engine.start(
        "scan",
        json.encode(ScanAsk(path: rooted)),
        done: j -> scan_ended(a, j),
        progress: j -> scan_moved(a, j),
    ) {
        .err(why) => {
            a.trouble = why
        }
        .ok(job) => {
            a.job = job
        }
    }
}

/// Measures a folder inside the one measured, keeping the way back.
action drill(a: Analyzer, m: Measure) uses files, time (icon: .folder, label: "Measure {m.name}") {
    if m.path == "" or m.path.ends_with("/.") {
        return
    }
    let rooted = a.root
    analyze(a, path: m.path, label: m.name)
    a.root = if rooted == "" { m.path } else { rooted }
}

/// Measures a folder offered on the places page, starting a chain of its own.
action open_place(a: Analyzer, path: Text) uses files, time (icon: .folder) {
    analyze(a, path: path, label: place_label(path))
}

/// Stops the measuring under way.
action stop(a: Analyzer) (icon: .square, label: "Stop measuring") {
    if a.job != "" { engine.cancel(a.job) }
}

/// Goes back one folder, and from the folder the chain started at to the places page.
action back(a: Analyzer) uses files, time (icon: .arrow_left, label: "Go back") {
    if a.folder == "" {
        return
    }
    if a.folder == a.root or a.folder == "/" or a.folder == "~" {
        home(a)
        return
    }
    let rooted = a.root
    let path = parent_of(a.folder)
    analyze(a, path: path, label: place_label(path))
    a.root = rooted
}

/// Shows the places, on this device and measured lately.
action home(a: Analyzer) (icon: .home, label: "Show places") {
    if a.job != "" { engine.cancel(a.job) }
    a.folder = ""
    a.label = ""
    a.root = ""
    a.job = ""
    a.trouble = ""
    a.notice = ""
    a.choosing = false
    for m in a.entries {
        a.entries.remove(m)
    }
}

/// Asks the person which folder to measure, showing the folders of this device.
primary action choose(a: Analyzer) uses files, time (icon: .folder_open, label: "Choose a folder") {
    a.choosing = true
    a.picker = "~"
    read_picker(a)
}

/// Walks the picker to `path` and shows the folders in it.
action go(a: Analyzer, path: Text) uses files, time {
    a.picker = if path == "" { "~" } else { path }
    read_picker(a)
}

/// Measures the folder the picker shows.
action pick(a: Analyzer) uses files, time (icon: .check, label: "Analyze this folder") {
    let chosen = a.picker
    a.choosing = false
    analyze(a, path: chosen, label: place_label(chosen))
}

/// Leaves the chooser without measuring anything.
action cancel_choose(a: Analyzer) (icon: .x, label: "Cancel") {
    a.choosing = false
}

/// Reads the folders of the one the picker shows.
action read_picker(a: Analyzer) uses files, time {
    for m in a.picks {
        a.picks.remove(m)
    }
    match File.list(a.picker) {
        .err(why) => {
            a.trouble = why
        }
        .ok(found) => {
            for entry in found.filter(e -> e.folder) {
                a.picks.insert(Measure(name: entry.name, path: entry.path, bytes: 0, files: 0))
            }
            a.trouble = ""
        }
    }
}

/// Opens the Disks app, where the device's drives and their use are.
action open_disks() (icon: .hard_drive, label: "Open Disks") {
    session.open_app(app: "so.tilo.disks")
}

/// The measuring moved on: how much of the folder it has seen so far.
action scan_moved(a: Analyzer, j: host.Job) {
    if a.job != j.id {
        return
    }
    match json.decode<ScanState>(j.detail) {
        .ok(state) => {
            a.seen_bytes = state.bytes
            a.seen_files = state.files
        }
        .err(why) => {}
    }
}

/// The measuring ended: the folders it found, why it could not read them, or that it was
/// stopped.
action scan_ended(a: Analyzer, j: host.Job) uses time {
    if a.job != j.id {
        return
    }
    a.job = ""
    if j.state == .done {
        match json.decode<ScanState>(j.detail) {
            .err(why) => {
                a.trouble = why
            }
            .ok(state) => {
                a.total = state.total
                a.files = state.files
                a.unreadable = state.unreadable
                a.trouble = state.error
                for m in a.entries {
                    a.entries.remove(m)
                }
                match engine.call("entries", json.encode(EntriesAsk(job: j.id))) {
                    .err(why) => {
                        a.trouble = why
                    }
                    .ok(listed) => match json.decode<List<Scanned>>(listed) {
                        .err(why) => {
                            a.trouble = why
                        }
                        .ok(scanned) => {
                            for m in scanned.take(8) {
                                a.entries.insert(
                                    Measure(
                                        name: m.name,
                                        path: m.path,
                                        bytes: m.bytes,
                                        files: m.files,
                                    ),
                                )
                            }
                            if scanned.len() > 8 {
                                a.entries.insert(
                                    Measure(
                                        name: "Other folders",
                                        path: "",
                                        bytes: held_from(scanned, 8),
                                        files: files_from(scanned, 8),
                                    ),
                                )
                            }
                            // The folder it measured is offered again: once, and the five
                            // newest of them.
                            for r in a.recents {
                                if r.path == a.folder { a.recents.remove(r) }
                            }
                            a.recents.insert(Recent(path: a.folder, label: a.label, seen: now))
                            let kept = a.recents.sort_by(r -> r.seen, desc)
                            if kept.len() > 5 {
                                if let oldest = kept.last() { a.recents.remove(oldest) }
                            }
                        }
                    },
                }
            }
        }
    } else if j.state == .failed {
        a.trouble = if j.error == "" { "The folder could not be read." } else { j.error }
    } else if j.state == .cancelled {
        a.notice = "Stopped measuring {a.label}."
    }
}

/// The newest first of the folders of `a`, each with what it holds.
fn largest(a: Analyzer) -> List<Measure> {
    a.entries.sort_by(m -> m.bytes, desc)
}

/// What the chart shows: every folder measured, the rest of them as one part, so the donut is
/// the whole folder.
fn shares(a: Analyzer) -> List<ui.Slice> {
    largest(a).map(m -> ui.Slice.slice(label: m.name, value: m.bytes.to_float()))
}

/// What the folders of `scanned` from the `at`th on hold together.
fn held_from(scanned: List<Scanned>, at: Int) -> Int {
    if let m = scanned.at(at) { m.bytes + held_from(scanned, at + 1) } else { 0 }
}

/// How many files the folders of `scanned` from the `at`th on hold together.
fn files_from(scanned: List<Scanned>, at: Int) -> Int {
    if let m = scanned.at(at) { m.files + files_from(scanned, at + 1) } else { 0 }
}

/// How many files `n` is, in the person's words.
fn files_text(n: Int) -> Text {
    if n == 1 { "1 file" } else { "{n} files" }
}

/// How many entries `n` could not be read.
fn unreadable_text(n: Int) -> Text {
    if n == 1 { "1 item couldn’t be read" } else { "{n} items couldn’t be read" }
}

/// The window: the places to measure, the folder being measured, its folders as a donut and a
/// list, or why it could not be read.
view analyzer(a: Analyzer) {
    if a.choosing {
        chooser(a)
    } else if a.folder == "" {
        places_page(a)
    } else if a.job != "" {
        measuring_page(a)
    } else if a.trouble != "" {
        trouble_page(a)
    } else {
        measured_page(a)
    }
}

/// The places, and the folders measured lately on this device.
view places_page(a: Analyzer) {
    let recent = a.recents.sort_by(r -> r.seen, desc).take(5)
    std.page(a) {
        content: {
            ui.Section(title: "Places") {
                place_row(a, "~")
                place_row(a, "/var/workspace")
                place_row(a, "/")
            }
            if not recent.is_empty() {
                ui.Section(title: "Recent") {
                    for r in recent {
                        ui.ListRow(r.label, secondary: r.path, icon: .clock) {
                            on activate: open_place(a, r.path)
                        }
                    }
                }
            }
        }
    }
}

/// One place, pressed to measure it.
view place_row(a: Analyzer, path: Text) {
    ui.ListRow(
        place_label(path),
        secondary: path,
        icon: if path == "~" { .home } else if path == "/" { .hard_drive } else { .folder },
    ) {
        on activate: open_place(a, path)
    }
}

/// The folder being measured: how far it has got, and the way to stop it.
view measuring_page(a: Analyzer) {
    std.page(a) {
        content: {
            ui.Section(title: "Measuring {a.label}") {
                stack(gap: .group) {
                    ui.ProgressBar(label: "Measuring {a.label}")
                    ui.Text(
                        if a.seen_files > 0 {
                            "{std.size(a.seen_bytes)} in {files_text(a.seen_files)} so far"
                        } else { "Reading the folders…" },
                        role: .caption,
                        secondary: true,
                    )
                    row(gap: .related) {
                        ui.Button("Stop", icon: .square, role: .destructive) {
                            on press: stop(a)
                        }
                    }
                }
            }
        }
    }
}

/// Why the folder could not be read, and the way to another one.
view trouble_page(a: Analyzer) {
    std.page(a) {
        content: {
            ui.EmptyState(
                "Can’t read this folder",
                message: a.trouble,
                icon: .folder,
                action: "Choose another folder",
                error: true,
            ) {
                on act: choose(a)
            }
            if a.notice != "" {
                ui.Text(a.notice, role: .caption, secondary: true)
            }
        }
    }
}

/// The folder measured: the donut of its folders, the folders themselves, and what could not be
/// read.
view measured_page(a: Analyzer) {
    let shown = largest(a)
    std.page(a) {
        content: {
            ui.Section(title: a.label) {
                stack(gap: .group) {
                    ui.PieChart(
                        shares(a),
                        title: .some(a.label),
                        ring: true,
                        total: .some(std.size(a.total)),
                        unit: .some(files_text(a.files)),
                    )
                    if not a.notice.is_empty() {
                        ui.Text(a.notice, role: .caption, secondary: true)
                    }
                }
            }
            ui.Section(title: "Folders") {
                for m in shown {
                    measure_row(a, m)
                }
            }
            if a.unreadable > 0 {
                ui.Text(
                    unreadable_text(a.unreadable),
                    role: .caption,
                    secondary: true,
                    numeric: true,
                )
            }
            if a.total == 0 and shown.is_empty() {
                ui.EmptyState(
                    "This folder is empty",
                    message: "It holds no files and no folders.",
                    icon: .folder,
                )
            }
        }
    }
}

/// One folder measured: pressed to measure it in turn, unless it is the files loose in this
/// folder, which are not a folder.
view measure_row(a: Analyzer, m: Measure) {
    if m.path == "" or m.path.ends_with("/.") {
        ui.ListRow(
            m.name,
            secondary: files_text(m.files),
            meta: std.size(m.bytes),
            icon: .chart_pie,
        )
    } else {
        ui.ListRow(m.name, secondary: files_text(m.files), meta: std.size(m.bytes), icon: .folder) {
            on activate: drill(a, m)
        }
    }
}

/// The chooser: the folders of the one it shows, its way up, and the folder to measure.
view chooser(a: Analyzer) {
    let up = parent_of(a.picker)
    std.page(a) {
        content: {
            ui.Section(title: "Choose a folder") {
                stack(gap: .group) {
                    row(gap: .related, align: .center) {
                        ui.Button("Up", icon: .arrow_up, icon_only: true) {
                            on press: go(a, up)
                        }
                        ui.Text(a.picker, role: .label, fill: 1.0)
                    }
                    if not a.trouble.is_empty() {
                        ui.Text(a.trouble, role: .caption, secondary: true)
                    }
                    ui.Section {
                        for m in a.picks.sort_by(m -> m.name) {
                            ui.ListRow(m.name, icon: .folder) {
                                on activate: go(a, m.path)
                            }
                        }
                    }
                    row(gap: .related) {
                        ui.Button("Analyze this folder", icon: .check, role: .primary) {
                            on press: pick(a)
                        }
                        ui.Button("Cancel", role: .plain) {
                            on press: cancel_choose(a)
                        }
                    }
                }
            }
        }
    }
}

type ScanAsk {
    path: Text
}

/// What the engine says of a measuring as it runs and when it ends.
type ScanState {
    state: Text = ""
    path: Text = ""
    bytes: Int = 0
    files: Int = 0
    total: Int = 0
    unreadable: Int = 0
    error: Text = ""
}

/// What asks the engine which folders a finished measuring found.
type EntriesAsk {
    job: Text
}

type Scanned {
    name: Text
    path: Text
    bytes: Int
    files: Int
}

/// The analyzer with a home folder measured and its folders, as a first run finds it.
scenario "a home folder measured" {
    let a = root Analyzer
    a.folder = "/home/you"
    a.label = "you"
    a.root = "/home/you"
    a.total = 210300000000
    a.files = 92127
    a.unreadable = 3
    for m in [
        Scanned(name: "Projects", path: "/home/you/Projects", bytes: 84200000000, files: 48310),
        Scanned(name: "Videos", path: "/home/you/Videos", bytes: 48600000000, files: 1204),
        Scanned(name: "Photos", path: "/home/you/Photos", bytes: 31800000000, files: 9870),
        Scanned(name: "Downloads", path: "/home/you/Downloads", bytes: 19400000000, files: 2615),
        Scanned(name: "Documents", path: "/home/you/Documents", bytes: 8700000000, files: 4132),
        Scanned(name: "Music", path: "/home/you/Music", bytes: 5100000000, files: 1086),
        Scanned(name: ".cache", path: "/home/you/.cache", bytes: 3900000000, files: 12408),
        Scanned(name: ".local", path: "/home/you/.local", bytes: 3400000000, files: 6121),
        Scanned(name: "Desktop", path: "/home/you/Desktop", bytes: 2600000000, files: 214),
        Scanned(name: "Files in this folder", path: "/home/you/.", bytes: 1200000000, files: 156),
    ] {
        a.entries.insert(Measure(name: m.name, path: m.path, bytes: m.bytes, files: m.files))
    }
    a.recents.insert(Recent(path: "/home/you", label: "Home", seen: now))
    a.recents.insert(Recent(path: "/home/you/Projects/tilo", label: "tilo", seen: now))
}

/// The places a first run shows, with a folder measured before.
scenario "the places" {
    let a = root Analyzer
    a.recents.insert(Recent(path: "/home/you", label: "Home", seen: now))
    a.recents.insert(Recent(path: "/home/you/Projects/tilo", label: "tilo", seen: now))
}

/// A folder being measured, how far it has got, and the way to stop it.
scenario "measuring the home folder" {
    let a = root Analyzer
    a.folder = "/home/you"
    a.label = "you"
    a.root = "/home/you"
    a.job = "scan-1"
    a.seen_bytes = 42000000000
    a.seen_files = 1430
}

/// A folder that cannot be read, and the way to another one.
scenario "a folder that cannot be read" {
    let a = root Analyzer
    a.folder = "/secret"
    a.label = "secret"
    a.root = "/secret"
    a.trouble = "It was moved or removed, or you don’t have permission to open it."
}
