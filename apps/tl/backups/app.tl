app so.tilo.backups "Backups" (
    icon: .archive,
    category: .system,
    tagline: "Protects a folder with snapshots you can put back",
    keywords: ["backup", "backups", "snapshot", "restore", "retention", "archive", "datensicherung", "sicherung", "wiederherstellen"],
    splash_budget: 250,
)

op "catalog"
op "backup"
op "verify"
op "restore" changes device

/// Was a snapshot taken by the schedule or by hand?
enum Taken {
    automatic
    manual
    unknown
}

/// The retention class a snapshot counts in.
enum Kept {
    hourly
    daily
    weekly
    monthly
    kept
    unknown
}

/// Whether a stored snapshot is whole, being checked or known broken.
enum Health {
    complete
    verifying
    failed
}

/// What a source is, as much as its path says without naming it.
enum Origin {
    home
    system
    workspace
    custom
}

/// When a source backs itself up.
enum Cadence {
    on_demand
    hourly
    daily
    weekly
}

/// Why the last look went wrong.
enum Trouble {
    none
    unreadable
    missing
}

/// One entry of a snapshot's file index, as the restore preview shows it.
type Entry (title: name) {
    name: Text
    /// Its path inside the snapshot: what a folder's contents are folded away by.
    path: Text = ""
    /// How deep in the snapshot it sits.
    depth: Int
    /// Its size in bytes; zero for a folder.
    size: Int
    folder: Bool
}

/// A folder of a snapshot's file index the person folded away.
type Folded (title: path) {
    /// The folder's path inside the snapshot.
    path: Text
}

/// One stored snapshot of the folder this device protects.
type Snapshot (title: label, untitled: "Snapshot") {
    /// The name a person gave it, when they gave one.
    label: Text = ""
    /// The second it was taken, counted from 1970.
    at: Int
    taken: Taken = .unknown
    kept: Kept = .unknown
    health: Health = .complete
    size_bytes: Int
    delta_bytes: Int
    files_changed: Int
    /// Why it failed, when it did.
    failure: Text = ""
    files: Collection<Entry>
    /// The folders of its file index the person folded away.
    folded: Collection<Folded>

    search {
        sort at newest
        hide files
        hide folded
        hide failure
    }
}

type Policy {
    /// Snapshots kept of each class.
    hourly: Int = 24
    daily: Int = 7
    weekly: Int = 4
    monthly: Int = 12
}

/// The folder a person picked in the picker, before it becomes the backup's folder.
type Place (title: name) {
    name: Text
    path: Text
}

/// The workspace's Backups app: the folder this device protects, where its snapshots go, the
/// snapshots it holds, and the retention policy that prunes them.
type Backups (untitled: "Backups") {
    /// How many snapshots of each class a backup keeps.
    policy: Policy
    /// The folder this device protects; empty until a person picks one.
    source: Text = ""
    /// The folder snapshots are written to; empty until a person picks one.
    destination: Text = ""
    /// The name the source folder goes by.
    source_name: Text = ""
    source_origin: Origin = .custom
    source_cadence: Cadence = .on_demand
    source_failing: Bool = false
    /// When the next backup runs, counted from 1970; zero when nothing schedules one.
    next_run: Int = 0
    /// The name the destination folder goes by, and what it holds.
    destination_name: Text = ""
    used_bytes: Int = 0
    /// The drive's capacity; zero when it does not say.
    total_bytes: Int = 0
    encrypted: Bool = false
    destination_failing: Bool = false
    /// The stored snapshots, newest first.
    snapshots: Collection<Snapshot>
    /// The place the sidebar shows: `all`, `source`, `destination` or `retention`.
    place: Text = "all"
    /// Whether the timeline lists the newest first.
    newest_first: Bool = true
    /// The look at the repository went wrong, in the device's words.
    trouble: Text = ""
    /// The work failed or was refused, in the device's words; a look does not clear it.
    failure: Text = ""
    /// One line under the list: what was done or stopped.
    notice: Text = ""
    /// The work under way on this device, by the job the device gave it.
    job: Text = ""
    /// Which work the job is: `backup`, `verify` or `restore`.
    working: Text = ""
    /// How far the work has got, in files and bytes.
    progress: Text = ""
    /// What a verification or a restore found, once it finished.
    result: Text = ""
    /// The snapshot being verified or restored, by its second.
    subject: Int = 0
    /// Which folder the person is picking: `source`, `destination` or `restore`.
    picking: Text = ""
    /// The folder the picker walks, and the folders in it.
    picker: Text = "~"
    picks: Collection<Place>
    /// The folder a restore writes into, and whether it may replace what is there.
    restore_into: Text = ""
    overwrite: Bool = false
}

device root backups: Backups

default view window for Backups

/// Looks at the repository again: the source, the destination and the snapshots it holds.
// ---------------------------------------------------------------------------
// What a person can do
// ---------------------------------------------------------------------------
primary action refresh(b: Backups) uses time, files (icon: .refresh, label: "Look again") {
    if b.source == "" or b.destination == "" {
        return
    }
    match engine.call(
        "catalog",
        json.encode(CatalogAsk(source: b.source, destination: b.destination)),
    ) {
        .err(why) => {
            b.trouble = why
        }
        .ok(text) => keep_catalog(b, text),
    }
}

/// Keeps what the device answered about its repository, so the window and an agent read the
/// same snapshots.
action keep_catalog(b: Backups, answer: Text) uses time {
    match json.parse(answer) {
        .err(why) => {
            b.trouble = why
        }
        .ok(catalog) => {
            b.trouble = ""
            b.source_name = ""
            b.destination_name = ""
            for source in entries(field(catalog, "sources")) {
                b.source_name = text_at(source, "name")
                b.source_origin = origin_of(text_at(source, "kind"))
                b.source_cadence = cadence_of(text_at(source, "schedule"))
                b.source_failing = text_at(source, "health") == "warning"
            }
            for dest in entries(field(catalog, "destinations")) {
                b.destination_name = text_at(dest, "name")
                b.used_bytes = int_at(dest, "used_bytes")
                b.total_bytes = int_at(dest, "total_bytes")
                b.encrypted = flag_at(dest, "encrypted")
                b.destination_failing = text_at(dest, "health") == "warning"
            }
            // A snapshot the repository still holds keeps its record, so the window's
            // selection and every reference to it stay the ones a person and an agent hold.
            let seen: List<Int> = b.snapshots.map(s -> s.at)
            let listed = entries(field(catalog, "snapshots"))
                .sort_by(s -> number_at(s, "taken_unix_seconds"), desc)
            let stored_as: List<Int> = listed.map(s -> number_at(s, "taken_unix_seconds"))
            let fresh = listed.any(s -> not seen.contains(number_at(s, "taken_unix_seconds")))
            if fresh or not stored_as.contains(b.subject) {
                if let first = listed.first() {
                    b.subject = number_at(first, "taken_unix_seconds")
                }
            }
            for old in b.snapshots {
                if not stored_as.contains(old.at) { b.snapshots.remove(old) }
            }
            for stored in listed {
                let at = number_at(stored, "taken_unix_seconds")
                let snapshot = if let found = b.snapshots.find(s -> s.at == at) {
                    found
                } else {
                    b.snapshots.insert(
                        Snapshot(at: at, size_bytes: 0, delta_bytes: 0, files_changed: 0),
                    )
                }
                snapshot.label = text_at(stored, "label")
                snapshot.taken = taken_of(text_at(stored, "kind"))
                snapshot.kept = kept_of(text_at(stored, "retention"))
                snapshot.health = health_of(text_at(stored, "status"))
                snapshot.size_bytes = int_at(stored, "size_bytes")
                snapshot.delta_bytes = int_at(stored, "delta_bytes")
                snapshot.files_changed = int_at(stored, "files_changed")
                snapshot.failure = text_at(stored, "failure")
                for old in snapshot.files {
                    snapshot.files.remove(old)
                }
                for file in entries(field(stored, "files")) {
                    snapshot.files.insert(
                        Entry(
                            name: text_at(file, "name"),
                            path: text_at(file, "path"),
                            depth: int_at(file, "depth"),
                            size: int_at(file, "size"),
                            folder: flag_at(file, "is_dir"),
                        ),
                    )
                }
            }
            // The schedule says when the next run is due: at once for a source nothing has
            // backed up yet, and one cadence after the newest whole snapshot otherwise.
            let step = cadence_seconds(b.source_cadence)
            b.next_run = if step == 0 {
                0
            } else if let last = last_complete(b) {
                last.at + step
            } else { (now - moment(0)).seconds() }
        }
    }
}

/// The value at `key` of a JSON object, and nothing for anything else.
fn field(value: json.Value, key: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(key),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The entries of a JSON list, and nothing for anything else.
fn entries(value: Option<json.Value>) -> List<json.Value> {
    match value {
        .none => [],
        .some(held) => match held {
            .array(items) => items,
            .object(fields) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
    }
}

/// The text at `key`, and empty when it holds none.
fn text_at(value: json.Value, key: Text) -> Text {
    match field(value, key) {
        .none => "",
        .some(held) => match held {
            .string(text) => text,
            .object(fields) => "",
            .array(items) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
    }
}

/// The number at `key` as a whole count, and zero when it holds none.
fn number_at(value: json.Value, key: Text) -> Int {
    match field(value, key) {
        .none => 0,
        .some(held) => match held {
            .number(number) => number.to_int(),
            .object(fields) => 0,
            .array(items) => 0,
            .string(text) => 0,
            .bool(flag) => 0,
            .null => 0,
        },
    }
}

fn int_at(value: json.Value, key: Text) -> Int {
    number_at(value, key)
}

/// The flag at `key`, and false when it holds none.
fn flag_at(value: json.Value, key: Text) -> Bool {
    match field(value, key) {
        .none => false,
        .some(held) => match held {
            .bool(flag) => flag,
            .object(fields) => false,
            .array(items) => false,
            .string(text) => false,
            .number(number) => false,
            .null => false,
        },
    }
}

/// Backs the folder up now, keeping what the retention policy keeps.
action back_up_now(b: Backups) uses time, files (icon: .archive, label: "Back up now") {
    require b.source != "" "Choose a folder to protect first"
    require b.destination != "" "Choose where the snapshots go first"
    require not busy(b) "One piece of work is running already"
    start_work(
        b,
        "backup",
        json.encode(
            BackupAsk(
                source: b.source,
                destination: b.destination,
                policy_hourly: b.policy.hourly,
                policy_daily: b.policy.daily,
                policy_weekly: b.policy.weekly,
                policy_monthly: b.policy.monthly,
            ),
        ),
    )
}

/// Checks that every file of a snapshot is still what the snapshot recorded.
action verify(b: Backups, s: Snapshot) uses time, files (
    icon: .shield_check,
    label: "Verify the snapshot",
) {
    require not busy(b) "One piece of work is running already"
    b.subject = s.at
    start_work(
        b,
        "verify",
        json.encode(SnapshotAsk(destination: b.destination, snapshot: id_of(s))),
    )
}

/// Opens the sheet that asks where a snapshot goes back to.
action ask_restore(b: Backups, s: Snapshot) (icon: .history, label: "Restore the snapshot") {
    require not busy(b) "One piece of work is running already"
    b.subject = s.at
    b.restore_into = ""
    b.overwrite = false
    b.result = ""
    b.notice = ""
    begin_restore(b)
}

/// Starts finding the folder a restore writes into, without running anything yet.
action begin_restore(b: Backups) {
    b.picking = "restore"
    read_picker(b)
}

/// Restores the snapshot into the folder the person chose.
///
/// What it writes over there is gone, and the workspace's undo cannot bring it back, so a press
/// always asks first.
destructive action restore_now(b: Backups, s: Snapshot) uses time, files (
    icon: .history,
    label: "Restore",
) {
    require b.restore_into != "" "Choose a folder to restore into"
    require not busy(b) "One piece of work is running already"
    b.picking = ""
    work_begins(b)
    let started = engine.start(
        "restore",
        json.encode(
            RestoreAsk(
                destination: b.destination,
                snapshot: id_of(s),
                target: b.restore_into,
                overwrite: b.overwrite,
            ),
        ),
        done: j -> work_ended(b, "restore", j),
        progress: j -> work_moved(b, j),
    )
    match started {
        .err(why) => {
            b.failure = why
        }
        .ok(job) => {
            b.job = job
            b.working = "restore"
        }
    }
}

/// Stops the work under way; what it was writing goes.
action stop_work(b: Backups) (icon: .square, label: "Stop the work") {
    if b.job != "" { engine.cancel(b.job) }
    b.job = ""
    b.working = ""
    b.progress = ""
    b.notice = "The work was stopped. Nothing half-written stays behind."
}

/// The work moved on: how far it has got, in files and bytes.
action work_moved(b: Backups, j: host.Job) {
    if b.job != j.id {
        return
    }
    match json.decode<WorkDetail>(j.detail) {
        .ok(detail) => {
            b.progress = detail.progress
        }
        .err(why) => {}
    }
}

/// The work ended: the repository as it now stands, what a check found, or why it failed.
action work_ended(b: Backups, kind: Text, j: host.Job) uses time {
    if b.job != j.id {
        return
    }
    b.job = ""
    b.working = ""
    b.progress = ""
    match json.decode<WorkDetail>(j.detail) {
        .err(why) => {
            b.trouble = why
        }
        .ok(detail) => {
            if j.state == .done {
                if kind == "backup" {
                    keep_catalog(b, detail.answer)
                    b.notice = "The folder is backed up."
                } else {
                    match json.decode<Checked>(detail.answer) {
                        .err(why) => {
                            b.trouble = why
                        }
                        .ok(checked) => {
                            b.result = "{files_text(checked.files)}, {size_text(checked.bytes)}"
                            b.notice = if kind == "verify" {
                                "The snapshot checks out."
                            } else { "The snapshot is back." }
                        }
                    }
                    refresh(b)
                }
            } else if j.state == .failed {
                b.failure = if j.error == "" { "The work failed." } else { j.error }
                if kind == "backup" { refresh(b) }
            }
        }
    }
}

/// Shows one snapshot's sheet: what it holds and what can be done with it.
action open_snapshot(b: Backups, s: Snapshot) (did: "opened a snapshot") {
    b.subject = s.at
    b.restore_into = ""
    b.overwrite = false
    b.result = ""
}

/// Lists the timeline newest first, or oldest first.
action set_order(b: Backups, newest_first: Bool) (icon: .arrow_up_down, label: "Order") {
    b.newest_first = newest_first
}

/// Shows one of the sidebar's places.
action show_place(b: Backups, place: Text) (icon: .layout_dashboard, label: "Show the place") {
    b.place = place
}

/// Keeps one more snapshot of a class.
action keep_more(b: Backups, which: Text) (icon: .plus, label: "Keep one more") {
    if which == "hourly" {
        b.policy.hourly = bound(b.policy.hourly + 1)
    }
    if which == "daily" {
        b.policy.daily = bound(b.policy.daily + 1)
    }
    if which == "weekly" {
        b.policy.weekly = bound(b.policy.weekly + 1)
    }
    if which == "monthly" {
        b.policy.monthly = bound(b.policy.monthly + 1)
    }
}

/// Keeps one fewer snapshot of a class.
action keep_fewer(b: Backups, which: Text) (icon: .minus, label: "Keep one fewer") {
    if which == "hourly" {
        b.policy.hourly = bound(b.policy.hourly - 1)
    }
    if which == "daily" {
        b.policy.daily = bound(b.policy.daily - 1)
    }
    if which == "weekly" {
        b.policy.weekly = bound(b.policy.weekly - 1)
    }
    if which == "monthly" {
        b.policy.monthly = bound(b.policy.monthly - 1)
    }
}

/// Dismisses the line under the list.
action dismiss(b: Backups) {
    b.notice = ""
    b.trouble = ""
    b.failure = ""
    b.result = ""
}

/// Asks the person which folder this device protects.
// ---------------------------------------------------------------------------
// Picking a folder on this device
// ---------------------------------------------------------------------------
action choose_source(b: Backups) uses files (
    icon: .folder_open,
    label: "Choose the folder to protect",
) {
    b.picking = "source"
    read_picker(b)
}

/// Asks the person where the snapshots go.
action choose_destination(b: Backups) uses files (
    icon: .folder_open,
    label: "Choose where snapshots go",
) {
    b.picking = "destination"
    read_picker(b)
}

/// Walks the picker to another folder and shows the folders in it.
action go(b: Backups, path: Text) uses files {
    b.picker = if path == "" { "~" } else { path }
    read_picker(b)
}

/// Walks the picker up one folder.
action go_up(b: Backups) uses files (icon: .arrow_left, label: "Go up") {
    b.picker = parent_of(b.picker)
    read_picker(b)
}

/// Takes the folder the picker shows: as what this device protects, where snapshots go, or
/// where a restore writes.
action pick(b: Backups) uses files (icon: .check, label: "Use this folder") {
    let chosen = b.picker
    if b.picking == "source" {
        if b.destination != "" and (chosen == b.destination or chosen.starts_with(b.destination)) {
            b.trouble = "Choose a folder outside the one your snapshots are written to"
            return
        }
        b.source = chosen
        b.source_name = name_of(chosen)
        b.source_origin = .custom
    } else if b.picking == "destination" {
        if b.source != "" and (chosen == b.source or chosen.starts_with(b.source)) {
            b.trouble = "Choose a folder outside the one being protected"
            return
        }
        b.destination = chosen
        b.destination_name = name_of(chosen)
    } else if b.picking == "restore" {
        b.restore_into = chosen
    }
    b.picking = ""
    b.picker = "~"
    refresh(b)
}

/// Leaves the picker without changing anything.
action cancel_pick(b: Backups) (icon: .x, label: "Cancel") {
    b.picking = ""
    b.picker = "~"
}

/// Reads the folders of the one the picker shows.
action read_picker(b: Backups) uses files {
    for place in b.picks {
        b.picks.remove(place)
    }
    b.trouble = ""
    match File.list(b.picker) {
        .err(why) => {
            b.trouble = why
        }
        .ok(found) => {
            for entry in found.filter(item -> item.folder) {
                b.picks.insert(Place(name: entry.name, path: entry.path))
            }
        }
    }
}

/// The folder above `path`, or `~` at the top of the home folder.
fn parent_of(path: Text) -> Text {
    if path == "~" or path == "/" or path == "" {
        "~"
    } else if path == "~/" {
        "~"
    } else {
        let parts = path.split("/").filter(part -> part != "")
        if parts.len() <= 1 {
            if path.starts_with("/") { "/" } else { "~" }
        } else {
            let head = if path.starts_with("/") { "/" } else { "" }
            head + parts.take(parts.len() - 1).join("/")
        }
    }
}

/// What a folder is called: its own name, or the folder it stands at.
fn name_of(path: Text) -> Text {
    if path == "~" {
        "Home"
    } else if path == "/" {
        "File system"
    } else if let name = path.split("/").filter(part -> part != "").last() { name } else { path }
}

// ---------------------------------------------------------------------------
// The engine's words and the app's
// ---------------------------------------------------------------------------
type CatalogAsk {
    source: Text
    destination: Text
}

type BackupAsk {
    source: Text
    destination: Text
    policy_hourly: Int
    policy_daily: Int
    policy_weekly: Int
    policy_monthly: Int
}

type SnapshotAsk {
    destination: Text
    snapshot: Text
}

type RestoreAsk {
    destination: Text
    snapshot: Text
    target: Text
    overwrite: Bool
}

/// What the device says of a piece of work as it runs and when it ends. Its failure is the
/// job's `error`; `answer` is the device's own JSON, read once `done`.
type WorkDetail {
    progress: Text = ""
    answer: Text = ""
}

type Checked {
    files: Int = 0
    bytes: Int = 0
}

/// The id a stored snapshot goes by: the second it was taken.
fn id_of(s: Snapshot) -> Text {
    "{s.at}"
}

/// The snapshot the app holds whose second is `subject`.
fn stored(b: Backups, s: Snapshot) -> Option<Snapshot> {
    b.snapshots.find(item -> item.at == s.at)
}

fn origin_of(kind: Text) -> Origin {
    if kind == "home" {
        .home
    } else if kind == "system" {
        .system
    } else if kind == "workspace" { .workspace } else { .custom }
}

fn cadence_of(schedule: Text) -> Cadence {
    if schedule == "hourly" {
        .hourly
    } else if schedule == "daily" {
        .daily
    } else if schedule == "weekly" { .weekly } else { .on_demand }
}

fn taken_of(kind: Text) -> Taken {
    if kind == "automatic" { .automatic } else if kind == "manual" { .manual } else { .unknown }
}

fn kept_of(retention: Text) -> Kept {
    if retention == "hourly" {
        .hourly
    } else if retention == "daily" {
        .daily
    } else if retention == "weekly" {
        .weekly
    } else if retention == "monthly" {
        .monthly
    } else if retention == "keep" { .kept } else { .unknown }
}

fn health_of(status: Text) -> Health {
    if status == "verifying" {
        .verifying
    } else if status == "failed" { .failed } else { .complete }
}

/// A policy count inside its bounds.
fn bound(n: Int) -> Int {
    if n < 0 { 0 } else if n > 999 { 999 } else { n }
}

/// The most snapshots the policy keeps per source.
fn policy_total(p: Policy) -> Int {
    p.hourly + p.daily + p.weekly + p.monthly
}

fn busy(b: Backups) -> Bool {
    b.job != ""
}

/// Clears what the last piece of work said, as a new one begins.
action work_begins(b: Backups) {
    b.trouble = ""
    b.failure = ""
    b.notice = ""
    b.result = ""
    b.progress = ""
}

/// Starts a backup or a verification and keeps the job the device gave it.
///
/// A restore starts its own ([`restore_now`]): what it writes over cannot be taken back, so it
/// is asked apart.
action start_work(b: Backups, kind: Text, ask: Text) {
    work_begins(b)
    let started = if kind == "backup" {
        engine.start(
            "backup",
            ask,
            done: j -> work_ended(b, kind, j),
            progress: j -> work_moved(b, j),
        )
    } else {
        engine.start(
            "verify",
            ask,
            done: j -> work_ended(b, kind, j),
            progress: j -> work_moved(b, j),
        )
    }
    match started {
        .err(why) => {
            b.failure = why
        }
        .ok(job) => {
            b.job = job
            b.working = kind
        }
    }
}

/// The instant a count of seconds names.
// ---------------------------------------------------------------------------
// Words and small readings
// ---------------------------------------------------------------------------
fn moment(at: Int) -> Time {
    Time("1970-01-01T00:00:00Z") + Duration(at)
}

/// When a snapshot was taken, said plainly: its day and its clock time.
fn when(at: Int) -> Text {
    "{moment(at).day()}, {moment(at).clock()}"
}

/// A byte count in the platform's units for stored data.
fn size_text(bytes: Int) -> Text {
    if bytes < 1000 {
        "{bytes} B"
    } else if bytes < 1000000 {
        "{tenths(bytes, 1000)} kB"
    } else if bytes < 1000000000 {
        "{tenths(bytes, 1000000)} MB"
    } else if bytes < 1000000000000 {
        "{tenths(bytes, 1000000000)} GB"
    } else { "{tenths(bytes, 1000000000000)} TB" }
}

/// `bytes` divided by `unit`, to one place, without a trailing zero.
fn tenths(bytes: Int, unit: Int) -> Text {
    let whole = bytes / unit
    let part = bytes % unit * 10 / unit
    if part == 0 { "{whole}" } else { "{whole}.{part}" }
}

fn files_text(n: Int) -> Text {
    if n == 1 { "1 file" } else { "{n} files" }
}

fn taken_words(t: Taken) -> Text {
    if t == .automatic { "Automatic" } else if t == .manual { "Manual" } else { "Unknown" }
}

fn kept_words(k: Kept) -> Text {
    if k == .hourly {
        "Hourly"
    } else if k == .daily {
        "Daily"
    } else if k == .weekly {
        "Weekly"
    } else if k == .monthly { "Monthly" } else if k == .kept { "Until removed" } else { "Unknown" }
}

fn health_words(h: Health) -> Text {
    if h == .verifying { "Verifying" } else if h == .failed { "Failed" } else { "Complete" }
}

fn cadence_words(c: Cadence) -> Text {
    if c == .hourly {
        "Every hour"
    } else if c == .daily { "Every day" } else if c == .weekly { "Every week" } else { "By hand" }
}

fn origin_words(o: Origin) -> Text {
    if o == .home {
        "Home folder"
    } else if o == .system {
        "System files"
    } else if o == .workspace { "Workspace" } else { "Custom folder" }
}

/// The snapshots a place lists, newest first unless the person ordered them otherwise.
fn listed(b: Backups) -> List<Snapshot> {
    if b.newest_first {
        b.snapshots.sort_by(s -> s.at, desc)
    } else { b.snapshots.sort_by(s -> s.at) }
}

/// The newest snapshot, or nothing yet.
fn latest(b: Backups) -> Option<Snapshot> {
    b.snapshots.sort_by(s -> s.at, desc).first()
}

/// The newest snapshot that finished whole, or nothing yet.
fn last_complete(b: Backups) -> Option<Snapshot> {
    b.snapshots.filter(s -> s.health == .complete).sort_by(s -> s.at, desc).first()
}

/// How many seconds apart a cadence's runs are; zero when nothing runs them.
fn cadence_seconds(c: Cadence) -> Int {
    if c == .hourly {
        3600
    } else if c == .daily { 86400 } else if c == .weekly { 604800 } else { 0 }
}

fn failed_count(b: Backups) -> Int {
    b.snapshots.count(s -> s.health == .failed)
}

/// The period a snapshot taken at `at` counts in, for the retention class: 0 hourly, 1 daily,
/// 2 weekly, 3 monthly.
fn period_of(at: Int, class: Int) -> Int {
    let days = at / 86400
    if class == 0 {
        at / 3600
    } else if class == 1 { days } else if class == 2 { (days + 3) / 7 } else { month_of(days) }
}

/// The month a day counted from 1970-01-01 falls in, as `year * 12 + month`.
fn month_of(days: Int) -> Int {
    let z = days + 719468
    let era = z / 146097
    let doe = z - era * 146097
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
    let shifted = (5 * doy + 2) / 153
    let month = if shifted < 10 { shifted + 3 } else { shifted - 9 }
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 }
    year * 12 + month
}

/// The second each snapshot the policy keeps was taken: the newest, the newest of each hour,
/// day, week and month class, and the ones kept until removed.
fn kept_seconds(b: Backups) -> List<Int> {
    let newest_first = b.snapshots.sort_by(s -> s.at, desc)
    let kept: List<Int> = []
    if let newest = newest_first.first() {
        kept = kept + [newest.at]
    }
    for s in newest_first {
        if s.kept == .kept and not kept.contains(s.at) {
            kept = kept + [s.at]
        }
    }
    for class in [0, 1, 2, 3] {
        let count = if class == 0 {
            b.policy.hourly
        } else if class == 1 {
            b.policy.daily
        } else if class == 2 { b.policy.weekly } else { b.policy.monthly }
        let seen: List<Int> = []
        for s in newest_first {
            if seen.len() < count {
                let period = period_of(s.at, class)
                if not seen.contains(period) {
                    seen = seen + [period]
                    if not kept.contains(s.at) {
                        kept = kept + [s.at]
                    }
                }
            }
        }
    }
    kept
}

/// What the counts on screen would remove at the next backup, said as one sentence.
fn removal_line(b: Backups) -> Text {
    let kept = kept_seconds(b)
    let doomed = b.snapshots.filter(s -> not kept.contains(s.at)).sort_by(s -> s.at, desc)
    if doomed.is_empty() {
        std.say("Nothing would be removed: the next backup keeps every snapshot.")
    } else if doomed.len() == 1 {
        if let one = doomed.first() {
            std.say("The next backup would remove one snapshot, taken {when(one.at)}.")
        } else { "" }
    } else {
        std.say(
            "The next backup would remove {doomed.len()} snapshots: {doomed.map(s -> when(s.at)).join(", ")}.",
        )
    }
}

fn last_backup_line(b: Backups) -> Text {
    if let s = latest(b) { when(s.at) } else { "Never" }
}

/// When the next backup is due, or that the person runs each one.
fn next_run_line(b: Backups) -> Text {
    if b.source_cadence == .on_demand {
        "By hand"
    } else if b.next_run == 0 { "Not known" } else { when(b.next_run) }
}

fn protected_bytes(b: Backups) -> Int {
    if let s = latest(b) { s.size_bytes } else { 0 }
}

/// How full the destination is, `0..=1`, when it says its capacity.
fn used_fraction(b: Backups) -> Option<Float> {
    if b.total_bytes <= 0 {
        .none
    } else {
        .some((b.used_bytes.to_float() / b.total_bytes.to_float()).clamp(0.0, 1.0))
    }
}

fn used_line(b: Backups) -> Text {
    if b.total_bytes <= 0 {
        "{size_text(b.used_bytes)} used"
    } else {
        "{size_text(b.used_bytes)} of {size_text(b.total_bytes)}"
    }
}

/// The snapshot the window shows beside the timeline, when the person picked one.
fn chosen_snapshot(b: Backups) -> Option<Snapshot> {
    b.snapshots.find(s -> s.at == b.subject)
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------
view window(b: Backups) {
    if b.picking != "" {
        picker(b)
    } else if b.source == "" or b.destination == "" {
        setup(b)
    } else {
        shell(b)
    }
}

/// One folder the picker offers.
view picker(b: Backups) {
    std.page(b) {
        content: {
            ui.ObjectHeader(
                title: if b.picking == "source" {
                    "Choose the folder to protect"
                } else if b.picking == "destination" {
                    "Choose where snapshots go"
                } else { "Choose the folder to restore into" },
                caption: b.picker,
                icon: .folder_open,
            )
            row(gap: .related, align: .center) {
                ui.Button("Up", icon: .arrow_left) {
                    on press: go_up(b)
                }
                if b.picking == "restore" {
                    ui.Button("Restore into this folder", icon: .check, role: .primary) {
                        on press: pick(b)
                    }
                } else {
                    ui.Button("Use this folder", icon: .check, role: .primary) {
                        on press: pick(b)
                    }
                }
                ui.Button("Cancel", role: .plain) {
                    on press: cancel_pick(b)
                }
            }
            if b.trouble != "" {
                ui.Banner("That folder could not be read", message: b.trouble, kind: .warning)
            }
            ui.Section(title: "Folders") {
                if b.picks.is_empty() {
                    ui.EmptyState("No folders inside", inline: true)
                }
                for place in b.picks.sort_by(p -> p.name) {
                    ui.ListRow(title: place.name, icon: .folder, secondary: place.path) {
                        on activate: go(b, place.path)
                    }
                }
            }
        }
    }
}

/// The first run: what the app needs before it can protect anything.
view setup(b: Backups) {
    std.page(b) {
        content: {
            ui.ObjectHeader(
                title: "Set up Backups",
                caption: "Choose a folder to protect and where its snapshots go.",
                icon: .archive,
            )
            ui.Section(title: "The folder to protect") {
                if b.source == "" {
                    ui.EmptyState(
                        "Nothing chosen yet",
                        message: "Snapshots copy this folder as it is now.",
                        icon: .folder_open,
                        inline: true,
                        action: "Choose the folder to protect",
                    ) {
                        on act: choose_source(b)
                    }
                } else {
                    ui.PropertyList {
                        ui.Property("Folder", b.source)
                        ui.Property("Called", b.source_name)
                        ui.Property("Kind", origin_words(b.source_origin))
                    }
                    ui.Button("Choose another folder", icon: .folder_open) {
                        on press: choose_source(b)
                    }
                }
            }
            ui.Section(title: "Where snapshots go") {
                if b.destination == "" {
                    ui.EmptyState(
                        "Nothing chosen yet",
                        message: "Each snapshot is written here, outside the folder it copies.",
                        icon: .hard_drive,
                        inline: true,
                        action: "Choose where snapshots go",
                    ) {
                        on act: choose_destination(b)
                    }
                } else {
                    ui.PropertyList {
                        ui.Property("Folder", b.destination)
                        ui.Property("Used", used_line(b))
                    }
                    ui.Button("Choose another folder", icon: .hard_drive) {
                        on press: choose_destination(b)
                    }
                }
            }
            if b.trouble != "" {
                ui.Banner("That could not be done", message: b.trouble, kind: .warning)
            }
            if b.source != "" and b.destination != "" {
                ui.Button("Back up now", icon: .archive, role: .primary) {
                    on press: back_up_now(b)
                }
            }
        }
    }
}

/// The window: the places down one side, the timeline or the chosen place beside them.
view shell(b: Backups) {
    ui.NavShell(
        content_kind: .page,
        title: page_title(b),
        sidebar_title: "Backups",
        overview_title: "All backups",
        overview_count: b.snapshots.len(),
    ) {
        sidebar: {
            ui.NavList(selected: b.place) {
                ui.NavItem("source", "This device", icon: .hard_drive, live: failed_count(b)) {
                    on select: show_place(b, "source")
                }
                ui.NavItem("destination", "Destination", icon: .archive) {
                    on select: show_place(b, "destination")
                }
                ui.NavItem("retention", "Retention", icon: .hourglass) {
                    on select: show_place(b, "retention")
                }
                ui.NavSection("Snapshots", collapsible: true) {
                    for s in listed(b) {
                        ui.NavItem(
                            "snapshot:{s.at}",
                            if s.label != "" {
                                s.label
                            } else if s.health == .failed {
                                "Failed {when(s.at)}"
                            } else { when(s.at) },
                            icon: if s.health == .failed {
                                .triangle_alert
                            } else if s.health == .verifying { .loader_circle } else { .archive },
                            count: s.files.len(),
                            item: s,
                        ) {
                            on select: open_snapshot(b, s)
                        }
                    }
                }
            }
        }
        actions: {
            if busy(b) {
                ui.Button("Stop", icon: .square) {
                    on press: stop_work(b)
                }
            } else {
                ui.Button("Back up now", icon: .archive, role: .primary) {
                    on press: back_up_now(b)
                }
            }
            ui.Button("Look again", icon: .refresh, role: .plain) {
                on press: refresh(b)
            }
        }
        content: {
            std.document_page {
                if busy(b) {
                    working_line(b)
                }
                if b.job != "" {
                    ui.ProgressBar(label: "Work under way")
                }
                if b.trouble != "" {
                    ui.Banner(
                        "Something needs a look",
                        message: b.trouble,
                        kind: .error,
                        action: "Dismiss",
                    ) {
                        on act: dismiss(b)
                    }
                }
                if b.notice != "" or b.result != "" {
                    ui.Banner(
                        if b.notice != "" { b.notice } else { b.result },
                        kind: .success,
                        action: "Dismiss",
                    ) {
                        on act: dismiss(b)
                    }
                }
                if b.place == "source" {
                    source_page(b)
                } else if b.place == "destination" {
                    destination_page(b)
                } else if b.place == "retention" {
                    retention_page(b)
                } else {
                    timeline(b)
                }
            }
        }
    }
    if let s = chosen_snapshot(b) {
        if b.subject != 0 and b.place != "retention" {
            snapshot_sheet(b, s)
        }
    }
}

fn page_title(b: Backups) -> Text {
    if b.place == "source" {
        if b.source_name != "" { b.source_name } else { "This device" }
    } else if b.place == "destination" {
        if b.destination_name != "" { b.destination_name } else { "Destination" }
    } else if b.place == "retention" { "Retention" } else { "All backups" }
}

/// The work under way: what it is, how far it has got, and the way to stop it.
view working_line(b: Backups) {
    ui.Banner(
        if b.working == "backup" {
            "Backing up"
        } else if b.working == "verify" {
            "Verifying the snapshot"
        } else { "Restoring the snapshot" },
        message: b.progress,
        kind: .info,
        action: "Stop",
    ) {
        on act: stop_work(b)
    }
}

/// The timeline: a week of snapshots worth of facts, the order to read them in, and the list.
view timeline(b: Backups) {
    ui.StatGroup {
        ui.StatTile("Last backup", last_backup_line(b))
        ui.StatTile("Next run", next_run_line(b))
        ui.StatTile("Snapshots", "{b.snapshots.len()}")
        ui.StatTile("Protected", size_text(protected_bytes(b)))
        if failed_count(b) > 0 {
            ui.StatTile("Need a look", "{failed_count(b)}", tone: .warning)
        }
    }
    row(gap: .related, align: .center) {
        ui.Button(
            if b.newest_first { "Newest first" } else { "Oldest first" },
            icon: .arrow_up_down,
        ) {
            on press: set_order(b, not b.newest_first)
        }
        if let s = latest(b) {
            ui.Text("Newest {when(s.at)}", role: .caption, secondary: true)
        }
    }
    ui.Section(title: "Snapshots") {
        if b.snapshots.is_empty() {
            ui.EmptyState(
                "No snapshots yet",
                message: "Back up the folder to make the first one.",
                icon: .archive,
                inline: true,
            )
        } else {
            ui.VirtualList(selection_follows_focus: true, in_column: false) {
                on activate(s: Snapshot): open_snapshot(b, s)
                for s in listed(b) {
                    ui.ListItem(item: s) {
                        snapshot_row(b, s)
                    }
                }
            }
        }
    }
}

/// One row of the timeline: when, how big, how much changed, and how it stands.
view snapshot_row(b: Backups, s: Snapshot) {
    ui.ListRow(
        title: if s.label != "" { s.label } else { when(s.at) },
        secondary: "{size_text(s.size_bytes)}, {files_text(s.files_changed)} changed",
        meta: health_words(s.health),
        meta_tone: if s.health == .failed {
            .danger
        } else if s.health == .verifying { .info } else { .success },
        icon: .archive,
        selected: b.subject == s.at,
    ) {
        on activate: open_snapshot(b, s)
    }
}

/// What one snapshot holds, and what can be done with it.
view snapshot_sheet(b: Backups, s: Snapshot) {
    ui.ModalDialog(
        title: if s.label != "" { s.label } else { when(s.at) },
        icon: .archive,
        open: true,
        primary: "Restore…",
        secondary: .some("Verify"),
    ) {
        on act_primary: ask_restore(b, s)
        on act_secondary: verify(b, s)
        on dismiss: close_snapshot(b)
        body: {
            ui.PropertyList {
                ui.Property("Taken", when(s.at))
                ui.Property(
                    "Stored on",
                    if b.destination_name != "" { b.destination_name } else { b.destination },
                )
                ui.Property("How", taken_words(s.taken))
                ui.Property("Kept as", kept_words(s.kept))
                ui.Property("Size", size_text(s.size_bytes))
                ui.Property("Changed", "{size_text(s.delta_bytes)}, {files_text(s.files_changed)}")
                ui.Property("State", health_words(s.health))
                if s.failure != "" {
                    ui.Property("Why it failed", s.failure)
                }
            }
            if b.result != "" {
                ui.StatusLine(b.result, status: .active)
            }
            ui.Section(title: "Files") {
                if s.files.is_empty() {
                    ui.EmptyState("The snapshot holds no files", inline: true)
                } else {
                    ui.VirtualList(in_column: true, name: "The snapshot's files") {
                        for entry in s.files {
                            if not folded_away(s, entry) {
                                ui.ListItem(item: entry) {
                                    if entry.folder {
                                        ui.ListRow(
                                            title: entry.name,
                                            prefix: if entry.depth > 0 { "└" } else { "" },
                                            icon: if is_folded(s, entry.path) {
                                                .folder
                                            } else { .folder_open },
                                            dense: true,
                                        ) {
                                            on activate: fold_folder(
                                                b,
                                                s,
                                                entry.path,
                                                not is_folded(s, entry.path),
                                            )
                                        }
                                    } else {
                                        file_row(entry)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if b.restore_into != "" {
                ui.Banner(
                    "Restore into {b.restore_into}",
                    message: if b.overwrite {
                        "What the folder holds is replaced by the snapshot."
                    } else { "The folder must be new or empty." },
                    kind: .info,
                    action: "Restore here",
                ) {
                    on act: restore_now(b, s)
                }
                ui.Button("Choose another folder", icon: .folder_open) {
                    on press: begin_restore(b)
                }
            } else {
                ui.Text(
                    "The whole snapshot is restored into a new or empty folder you choose, and every file is checked. Nothing that already exists is replaced.",
                    role: .caption,
                    secondary: true,
                )
                ui.Button("Choose the folder to restore into", icon: .folder_open) {
                    on press: begin_restore(b)
                }
            }
            if b.overwrite {
                ui.Toggle(bind: b.overwrite, label: "Replace what the folder holds")
            } else {
                ui.Button("Replace what the folder holds", role: .plain) {
                    on press: allow_overwrite(b)
                }
            }
        }
    }
}

action close_snapshot(b: Backups) {
    b.subject = 0
    b.restore_into = ""
    b.overwrite = false
    b.result = ""
}

action allow_overwrite(b: Backups) {
    b.overwrite = true
}

/// One file of the snapshot's index, indented by how deep it sits.
view file_row(entry: Entry) {
    ui.ListRow(
        title: entry.name,
        prefix: if entry.depth > 0 { "└" } else { "" },
        icon: if entry.folder { .folder } else { .file },
        meta: if entry.folder { .none } else { .some(size_text(entry.size)) },
        dense: true,
    )
}

/// Folds a folder of the snapshot's file index away, or opens it again.
action fold_folder(b: Backups, s: Snapshot, path: Text, folded: Bool) {
    s.folded.remove_where(f -> f.path == path)
    if folded and path != "" { s.folded.insert(Folded(path: path)) }
}

/// Whether the person folded `path` away.
fn is_folded(s: Snapshot, path: Text) -> Bool {
    s.folded.find(f -> f.path == path) != .none
}

/// Whether `entry` stands inside a folder the person folded away.
fn folded_away(s: Snapshot, entry: Entry) -> Bool {
    s.folded.find(
        f -> f.path != "" and entry.path != f.path and entry.path.starts_with("{f.path}/"),
    )
        != .none
}

/// This device: the folder it protects and the snapshots of it.
view source_page(b: Backups) {
    ui.PropertyList {
        ui.Property("Folder", b.source)
        ui.Property("Called", b.source_name)
        ui.Property("Kind", origin_words(b.source_origin))
        ui.Property("Schedule", cadence_words(b.source_cadence))
        ui.Property("State", if b.source_failing { "Needs a look" } else { "Fine" })
    }
    ui.Button("Choose another folder", icon: .folder_open) {
        on press: choose_source(b)
    }
    ui.Section(title: "Its snapshots") {
        for s in listed(b) {
            snapshot_row(b, s)
        }
    }
}

/// Where snapshots go: what it holds, how full it is, and its snapshots.
view destination_page(b: Backups) {
    ui.PropertyList {
        ui.Property("Folder", b.destination)
        ui.Property("Called", b.destination_name)
        ui.Property("Used", used_line(b))
        if b.total_bytes > 0 {
            ui.Property("Capacity", size_text(b.total_bytes))
            ui.Property("Free", size_text(b.total_bytes - b.used_bytes))
        }
        ui.Property("Encrypted", if b.encrypted { "Yes" } else { "No" })
        ui.Property("State", if b.destination_failing { "Needs a look" } else { "Fine" })
    }
    if let fraction = used_fraction(b) {
        ui.ProgressBar(value: fraction, label: "Used")
    }
    ui.Button("Choose another folder", icon: .hard_drive) {
        on press: choose_destination(b)
    }
    ui.Section(title: "Its snapshots") {
        for s in listed(b) {
            snapshot_row(b, s)
        }
    }
}

/// The retention policy: how many snapshots of each class a backup keeps, and what the
/// counts on screen would remove at the next backup.
view retention_page(b: Backups) {
    ui.Section(title: "Snapshots to keep") {
        policy_row(b, "hourly", "Hourly snapshots to keep", b.policy.hourly)
        policy_row(b, "daily", "Daily snapshots to keep", b.policy.daily)
        policy_row(b, "weekly", "Weekly snapshots to keep", b.policy.weekly)
        policy_row(b, "monthly", "Monthly snapshots to keep", b.policy.monthly)
        ui.Facts {
            ui.Fact(
                "Each source keeps at most {policy_total(b.policy)} snapshots, plus the ones kept until removed",
            )
        }
    }
    ui.Text(removal_line(b), role: .body)
}

view policy_row(b: Backups, which: Text, label: Text, kept: Int) {
    row(gap: .related, align: .center) {
        ui.Text(label, role: .body, fill: 1.0)
        ui.Button("One fewer {label}", icon: .minus, icon_only: true) {
            on press: keep_fewer(b, which)
        }
        ui.Text("{kept}", role: .label)
        ui.Button("One more {label}", icon: .plus, icon_only: true) {
            on press: keep_more(b, which)
        }
    }
}

/// The app before anything is chosen.
// ---------------------------------------------------------------------------
// Captures
// ---------------------------------------------------------------------------
scenario "nothing chosen yet" {
}

/// A week of snapshots across one folder, newest first.
scenario "a week of backups" {
    let b = root Backups
    b.source = "/home/you/Documents"
    b.source_name = "Documents"
    b.destination = "/media/backup-drive"
    b.destination_name = "backup-drive"
    b.used_bytes = 312000000000
    b.total_bytes = 500000000000
    b.encrypted = true
    let first = b.snapshots.insert(
        Snapshot(
            at: 1795080000,
            taken: .automatic,
            kept: .hourly,
            health: .complete,
            size_bytes: 9700000000,
            delta_bytes: 240000000,
            files_changed: 312,
        ),
    )
    first.files.insert(Entry(name: "Documents", depth: 0, size: 0, folder: true))
    first.files.insert(Entry(name: "Projects", depth: 1, size: 0, folder: true))
    first.files.insert(Entry(name: "plan.md", depth: 2, size: 4200, folder: false))
    first.files.insert(Entry(name: "Pictures", depth: 1, size: 0, folder: true))
    first.files.insert(Entry(name: "Quarterly report.odt", depth: 2, size: 4200000, folder: false))
    b.snapshots.insert(
        Snapshot(
            at: 1795076400,
            taken: .automatic,
            kept: .hourly,
            health: .complete,
            size_bytes: 9500000000,
            delta_bytes: 180000000,
            files_changed: 211,
        ),
    )
    b.snapshots.insert(
        Snapshot(
            label: "Before the system upgrade",
            at: 1795072800,
            taken: .manual,
            kept: .kept,
            health: .complete,
            size_bytes: 48200000000,
            delta_bytes: 1100000000,
            files_changed: 1840,
        ),
    )
    b.snapshots.insert(
        Snapshot(
            at: 1795069200,
            taken: .automatic,
            kept: .hourly,
            health: .complete,
            size_bytes: 9300000000,
            delta_bytes: 96000000,
            files_changed: 142,
        ),
    )
    b.snapshots.insert(
        Snapshot(
            at: 1794993600,
            taken: .automatic,
            kept: .daily,
            health: .complete,
            size_bytes: 9100000000,
            delta_bytes: 150000000,
            files_changed: 233,
        ),
    )
    b.snapshots.insert(
        Snapshot(
            at: 1794907200,
            taken: .automatic,
            kept: .daily,
            health: .complete,
            size_bytes: 8900000000,
            delta_bytes: 410000000,
            files_changed: 402,
        ),
    )
}

/// A backup under way, and how far it has got.
scenario "backing up" {
    let b = root Backups
    b.source = "/home/you/Documents"
    b.source_name = "Documents"
    b.destination = "/media/backup-drive"
    b.destination_name = "backup-drive"
    b.job = "backup-1"
    b.working = "backup"
    b.progress = "1430 of 9218 files, 42.0 GB"
}

/// A snapshot a person is about to put back.
scenario "restoring a snapshot" {
    let b = root Backups
    b.source = "/home/you/Documents"
    b.source_name = "Documents"
    b.destination = "/media/backup-drive"
    b.destination_name = "backup-drive"
    let snapshot = b.snapshots.insert(
        Snapshot(
            at: 1794993600,
            taken: .manual,
            kept: .kept,
            health: .complete,
            size_bytes: 48200000000,
            delta_bytes: 1100000000,
            files_changed: 1840,
        ),
    )
    snapshot.files.insert(Entry(name: "Documents", depth: 0, size: 0, folder: true))
    snapshot.files.insert(Entry(name: "plan.md", depth: 1, size: 4200, folder: false))
    b.subject = snapshot.at
    b.place = "all"
}

/// A snapshot that failed, and why.
scenario "a failed backup" {
    let b = root Backups
    b.source = "/home/you/Documents"
    b.source_name = "Documents"
    b.destination = "/media/backup-drive"
    b.destination_name = "backup-drive"
    b.trouble = "Couldn’t read the saved backup folder: the drive is unavailable."
    let snapshot = b.snapshots.insert(
        Snapshot(
            at: 1795076400,
            taken: .automatic,
            kept: .hourly,
            health: .failed,
            size_bytes: 0,
            delta_bytes: 0,
            files_changed: 0,
            failure: "It was stopped before it finished.",
        ),
    )
    snapshot.files.insert(Entry(name: "Documents", depth: 0, size: 0, folder: true))
}

/// The retention policy as a person set it.
scenario "a retention policy" {
    let b = root Backups
    b.source = "/home/you/Documents"
    b.source_name = "Documents"
    b.destination = "/media/backup-drive"
    b.destination_name = "backup-drive"
    b.place = "retention"
    b.policy.hourly = 48
    b.policy.daily = 14
    b.policy.weekly = 8
    b.policy.monthly = 24
}
