app so.tilo.logs "Logs" (
    icon: .file_text,
    category: .system,
    tagline: "This computer's system journal, filtered and followed",
    keywords: ["logs", "journal", "journalctl", "systemd", "errors", "warnings", "diagnostics", "protokoll", "fehler"],
    splash_budget: 250,
)

use so.tilo.device as device

/// The least severe record the window shows.
enum Severity {
    /// Only emergencies.
    emerg
    /// Alerts and worse.
    alert
    /// Critical and worse.
    crit
    /// Errors and worse.
    err
    /// Warnings and worse.
    warning
    /// Notices and worse.
    notice
    /// Information and worse.
    info
    /// Every record, including debug.
    debug
}

/// The shared journal filter: what the workspace shows of this device's journal.
type Logs {
    /// The least severe record shown.
    /// The least severe record shown: `emerg`, `alert`, `crit`, `err`, `warning`, `notice`, `info` or `debug`.
    severity: Text = "debug"
    /// One unit, or empty for every unit.
    unit: Text = ""
    /// One boot's id, or empty for every boot.
    boot: Text = ""
    /// Words to find in a message, a unit or a program.
    search: Text = ""
    /// Whether this device's journal is followed live.
    following: Bool = false
    /// The record the inspector shows, by the journal's own timestamp and program. It is the
    /// workspace's, so a second window and an agent read the same record.
    selected: Text = ""
}

root logs: Logs

default view window for Logs

/// One record as the export writes it.
type ExportEntry {
    realtime_us: Int
    unit: Text
    identifier: Text
    priority: Int
    message: Text
    pid: Option<Int>
    hostname: Text
    boot_id: Text
    transport: Text
    extra: List<ExportField>
}

type ExportField {
    name: Text
    value: Text
}

/// The level of `severity`, 0 (emergency) to 7 (debug).
fn level_of(severity: Text) -> Int {
    if severity == "emerg" {
        0
    } else if severity == "alert" {
        1
    } else if severity == "crit" {
        2
    } else if severity == "err" {
        3
    } else if severity == "warning" {
        4
    } else if severity == "notice" { 5 } else if severity == "info" { 6 } else { 7 }
}

/// A severity as the person reads it.
fn severity_words(level: Int) -> Text {
    if level <= 0 {
        std.say("Emergency")
    } else if level == 1 {
        std.say("Alert")
    } else if level == 2 {
        std.say("Critical")
    } else if level == 3 {
        std.say("Error")
    } else if level == 4 {
        std.say("Warning")
    } else if level == 5 {
        std.say("Notice")
    } else if level == 6 { std.say("Info") } else { std.say("Debug") }
}

/// Whether `entry` survives every facet of the filter.
fn passes(l: Logs, entry: device.JournalEntry) -> Bool {
    entry.priority <= level_of(l.severity)
        and (l.unit == "" or entry.unit == l.unit)
        and (l.boot == "" or entry.boot_id == l.boot)
        and (l.search == "" or matches(entry, l.search))
}

/// Whether `entry` holds `query` in its message, unit or program.
fn matches(entry: device.JournalEntry, query: Text) -> Bool {
    let wanted = query.trim().lower()
    wanted == ""
        or entry.message.lower().contains(wanted)
        or entry.unit.lower().contains(wanted)
        or entry.identifier.lower().contains(wanted)
}

/// The records `l` shows, oldest first.
fn filtered(l: Logs, entries: List<device.JournalEntry>) -> List<device.JournalEntry> {
    entries.filter(entry -> passes(l, entry))
}

/// The units the records name, each once, by name.
fn unit_options(entries: List<device.JournalEntry>) -> List<Text> {
    let names: List<Text> = []
    for entry in entries.sort_by(one -> one.unit) {
        if entry.unit != "" and not names.contains(entry.unit) {
            names = names + [entry.unit]
        }
    }
    names
}

/// The boots the records name, each once, by id.
fn boot_options(entries: List<device.JournalEntry>) -> List<Text> {
    let ids: List<Text> = []
    for entry in entries.sort_by(one -> one.boot_id) {
        if entry.boot_id != "" and not ids.contains(entry.boot_id) {
            ids = ids + [entry.boot_id]
        }
    }
    ids
}

/// A record's key: the journal's own timestamp and its message.
fn entry_key(entry: device.JournalEntry) -> Text {
    "{entry.realtime_us}:{entry.identifier}"
}

/// The record `key` names.
fn entry_at(entries: List<device.JournalEntry>, key: Text) -> Option<device.JournalEntry> {
    entries.find(entry -> entry_key(entry) == key)
}

/// What the inspector shows when the person chose none of the records shown: the newest error or
/// worse, so a failing boot opens at what went wrong, else the first record.
fn default_selection(entries: List<device.JournalEntry>) -> Text {
    let found = ""
    for entry in entries {
        if entry.priority <= 3 {
            found = entry_key(entry)
        }
    }
    if found != "" { found } else if let first = entries.first() { entry_key(first) } else { "" }
}

/// A record as one line of text: its time, unit, program and message.
fn entry_copy(entry: device.JournalEntry) -> Text {
    "{entry.timestamp} {entry.unit}[{entry.identifier}]: {entry.message}"
}

/// The records `l` shows as JSON lines, as the export writes them.
fn export_text(l: Logs, entries: List<device.JournalEntry>) -> Text {
    let out: Text = ""
    for entry in filtered(l, entries) {
        let fields: List<ExportField> = entry.extra.map(
            field -> ExportField(name: field.name, value: field.value),
        )
        out = out
            + json.encode(
                ExportEntry(
                    realtime_us: entry.realtime_us,
                    unit: entry.unit,
                    identifier: entry.identifier,
                    priority: entry.priority,
                    message: entry.message,
                    pid: entry.pid,
                    hostname: entry.hostname,
                    boot_id: entry.boot_id,
                    transport: entry.transport,
                    extra: fields,
                ),
            )
            + "\n"
    }
    out
}

/// The records an export writes, one JSON object a line.
action made_export(l: Logs) -> Text {
    export_text(l, device.journal.entries)
}

/// Shows records of at least `value`'s severity.
action set_severity(l: Logs, value: Text) {
    l.severity = value
}

/// Shows one unit's records, or every unit's when `value` is empty.
action set_unit(l: Logs, value: Text) {
    l.unit = value
}

/// Shows one boot's records, or every boot's when `value` is empty.
action set_boot(l: Logs, value: Text) {
    l.boot = value
}

/// Narrows the records to those holding `value`.
action set_search(l: Logs, value: Text) {
    l.search = value
}

/// Shows one record's detail.
action choose(l: Logs, key: Text) {
    require key != l.selected "This record is already shown"
    l.selected = key
}

/// Clears every journal filter.
action clear_filters(l: Logs) (icon: .x, label: "Clear filters") {
    l.severity = "debug"
    l.unit = ""
    l.boot = ""
    l.search = ""
}

/// Reads the most recent records of this device's journal.
action refresh(l: Logs) (icon: .rotate_cw, label: "Refresh journal") {
    device.refresh(device.journal, limit: 500)
}

/// Follows new records as they arrive, or stops following them.
action follow(l: Logs, on: Bool) (icon: .play) {
    l.following = on
    device.set_following(device.journal, on: on)
}

/// Reads the records written since the last one read, while following.
action poll_tail(l: Logs) {
    if l.following and device.journal.serving {
        device.read_after(device.journal, cursor: device.journal.cursor, limit: 200)
    }
}

/// Copies one record, with its time, unit, program and message.
action copy_entry(entry: device.JournalEntry) uses clipboard (icon: .copy, label: "Copy entry") {
    clipboard.copy(entry_copy(entry))
}

/// Copies one record's message.
action copy_message(entry: device.JournalEntry) uses clipboard (
    icon: .copy,
    label: "Copy message",
) {
    clipboard.copy(entry.message)
}

/// The window: the filter down the side, the records beside their detail.
view window(l: Logs) {
    let now = device.journal
    let entries = filtered(l, now.entries)
    // The journal is followed in the window that shows it, on that window's device: each
    // device reads after its own cursor, and a window nobody looks at asks for nothing.
    if l.following and now.serving {
        every 1s: poll_tail(l)
    }
    ui.NavShell(content_kind: .page, title: "Logs", sidebar_title: "Logs") {
        // The window's search narrows the records to those holding the words, with the filters
        // beside it.
        on search(words: Text): set_search(l, words)
        sidebar: {
            stack(gap: .group) {
                ui.Section("Filter") {
                    ui.Dropdown(
                        bind: l.severity,
                        options: [
                            "emerg",
                            "alert",
                            "crit",
                            "err",
                            "warning",
                            "notice",
                            "info",
                            "debug",
                        ],
                        caption: "Severity",
                    )
                    ui.Dropdown(bind: l.unit, options: unit_options(now.entries), caption: "Unit")
                    ui.Dropdown(bind: l.boot, options: boot_options(now.entries), caption: "Boot")
                    ui.Button("Clear filters", icon: .x) {
                        on press: clear_filters(l)
                    }
                }
                if now.following {
                    ui.StatusLine("Following the journal", status: .active)
                }
                if now.pending.contains("refresh") or now.pending.contains("read-after") {
                    ui.StatusLine("Reading the journal…", status: .idle)
                }
            }
        }
        actions: {
            if now.serving {
                ui.Toggle(shown: l.following, label: "Follow live") {
                    on check(on): follow(l, on)
                }
                ui.Button("Refresh journal", icon: .rotate_cw, icon_only: true) {
                    on press: refresh(l)
                }
                ui.Button("Export filtered logs", icon: .download) {
                    on press: file.save(made_export(l), name: "journal.jsonl")
                }
            }
        }
        if not now.serving {
            unavailable(l, now)
        } else if now.entries.is_empty() {
            empty_journal(l)
        } else {
            journal_page(l, now, entries)
        }
    }
}

/// What a computer whose journal cannot be read says.
view unavailable(l: Logs, now: device.Journal) {
    ui.EmptyState(
        "The journal is not available",
        message: if let problem = now.read_error {
            problem
        } else {
            "This computer's system journal could not be read."
        },
        icon: .file_text,
        action: "Try again",
    ) {
        on act: refresh(l)
    }
}

/// What a journal read that found nothing says.
view empty_journal(l: Logs) {
    ui.EmptyState(
        "No records",
        message: "This device's journal holds no records yet.",
        icon: .file_text,
        action: "Read the journal",
    ) {
        on act: refresh(l)
    }
}

/// The records and the chosen one's detail.
view journal_page(l: Logs, now: device.Journal, entries: List<device.JournalEntry>) {
    // What the inspector shows: the record the person chose, while it is shown, else the newest
    // error or worse, else the first record. Opening a failing boot lands on what went wrong.
    let chosen = if entry_at(entries, l.selected) != .none {
        l.selected
    } else { default_selection(entries) }
    if entries.is_empty() {
        ui.EmptyState(
            "No records match",
            message: "The filter hides every record this journal holds.",
            icon: .search,
            action: "Clear filters",
        ) {
            on act: clear_filters(l)
        }
    } else {
        split(side: .trailing, collapse: .stack) {
            primary(min: 24rem) {
                ui.Text(
                    "{entries.len()} records of {now.entries.len()}",
                    role: .caption,
                    secondary: true,
                )
                scroll {
                    stack(gap: .none) {
                        for entry in entries {
                            let key = entry_key(entry)
                            row(gap: .related, align: .center) {
                                ui.Badge(
                                    severity_words(entry.priority),
                                    tone: if entry.priority <= 3 {
                                        .danger
                                    } else if entry.priority == 4 {
                                        .warning
                                    } else if entry.priority == 5 { .info } else { .neutral },
                                )
                                stack(gap: .none) {
                                    ui.Text(entry.message, role: .body, lines: 2)
                                    ui.Text(
                                        "{entry.time} · {entry.unit} · {entry.identifier}",
                                        role: .caption,
                                        secondary: true,
                                    )
                                }
                                spacer
                                ui.Button(
                                    "Show this record",
                                    icon: .chevron_right,
                                    icon_only: true,
                                ) {
                                    on press: choose(l, key)
                                }
                            }
                        }
                    }
                }
            }
            secondary(min: 18rem) {
                if let entry = entry_at(entries, chosen) {
                    entry_detail(entry)
                } else if let first = entries.first() {
                    entry_detail(first)
                } else {
                    ui.EmptyState(
                        "No record selected",
                        message: "Choose a record to see its fields.",
                        icon: .file_text,
                    )
                }
            }
        }
    }
}

/// One record's fields and its copy verbs.
view entry_detail(entry: device.JournalEntry) {
    let pid_words = if let pid = entry.pid { "{pid}" } else { "None" }
    stack(gap: .group) {
        ui.Text(entry.message, role: .body)
        ui.Section("Record") {
            ui.PropertyList {
                ui.Property("Time", entry.timestamp)
                ui.Property("Severity", severity_words(entry.priority))
                ui.Property("Unit", entry.unit)
                ui.Property("Program", entry.identifier)
                ui.Property("Process ID", pid_words)
                ui.Property("Host", entry.hostname)
                ui.Property("Boot ID", entry.boot_id, copyable: true)
                ui.Property("Transport", entry.transport)
            }
        }
        if not entry.extra.is_empty() {
            ui.Section("Fields") {
                ui.PropertyList {
                    for field in entry.extra {
                        ui.Property(field.name, field.value, copyable: true)
                    }
                }
            }
        }
        row(gap: .tight) {
            ui.Button("Copy message", icon: .copy) {
                on press: copy_message(entry)
            }
            ui.Button("Copy entry", icon: .copy) {
                on press: copy_entry(entry)
            }
        }
    }
}
