app so.tilo.migrate "Migrations" (
    icon: .truck,
    category: .system,
    tagline: "Bring in what your computers and accounts hold",
    keywords: ["migrate", "import", "move", "bring in", "thunderbird", "browsers", "cloud storage"],
)

use so.tilo.migrate.engine as engine

/// Where a migration's run stands: reviewed, with what the source holds waiting to be chosen;
/// running, while the device that holds the data brings in what was chosen; stopped by a person,
/// keeping what it brought until then; failed, its failure saying why; or complete. The device
/// that holds the data moves it on as its run goes.
enum RunState {
    /// Reviewed
    reviewed
    /// Running
    running (icon: .refresh, tone: .info)
    /// Stopped
    stopped (icon: .square)
    /// Failed
    failed (icon: .triangle_alert, tone: .danger)
    /// Complete
    complete (icon: .circle_check, tone: .success)
}

/// Something the review found in the source: an account, a folder, an address book, a calendar.
/// Items stand under each other by `within`, the key of the item they sit under.
type Item {
    /// What the item is in the source, as the engine names it, unique within the migration.
    key: Text
    /// What sort of thing it is: "account", "folder", "address book".
    kind: Text
    title: Text
    /// How many things it holds, counted as `counts` says.
    count: Int = 0
    /// The plural noun of what `count` counts: "messages", "contacts".
    counts: Text = ""
    /// The key of the item it sits under; empty for one at the top.
    within: Text = ""
    /// Whether it is to be brought in.
    chosen: Bool = false
    /// What the review has to say about it: "Shared with you, read only".
    note: Text = ""
}

/// How many things of one kind the last run brought in: "contacts" 120.
type Count {
    kind: Text
    count: Int merge counter
}

/// One run of a migration and the keys of what it brought in, so undoing it forgets exactly
/// those.
type Run {
    /// The id of the run: the invocation of the engine's `start` that made it.
    id: Text
    title: Text
    keys: Set<Text>
}

/// Bringing what one source holds into the workspace: a computer's home folders, its browsers,
/// a Thunderbird profile, a mail account, a cloud drive, a service's pages or issues. The device
/// that holds the data reads the source, lists what it found here, and brings in what people
/// choose; any member opens it and follows along.
type Migration (title: title) {
    /// The engine's key for the source: home, browsers, thunderbird, mail, gdrive, onedrive,
    /// dropbox, icloud, nextcloud, webdav, outline, linear, forgejo.
    source: Text
    /// What the source is, as a person reads it.
    title: Text
    /// The key of the device that holds the data and runs the source; empty for a source any
    /// device can run.
    device: Text = ""
    /// The profile, account or home the review read.
    handle: Text = ""
    items: Collection<Item>
    /// What the review could not read, each in a sentence.
    unreadable: Set<Text>
    state: RunState = .reviewed
    /// The id of the current or last run.
    run: Text = ""
    run_title: Text = ""
    failure: Text = ""
    /// Why the device refused the last step asked of the migration; empty once a step goes
    /// through.
    refused: Text = ""
    /// How many of each kind the last run brought in.
    brought: Collection<Count>
    /// How many things the last run found already in the workspace.
    already_there: Int merge counter
    /// The keys of the items from this source already in the workspace.
    landed: Set<Text>
    /// What each run brought in.
    runs: Collection<Run>
}

/// The workspace's migrations.
type Migrations {
    migrations: Collection<Migration>
}

root Migrations

default view migrations for Migrations

default view migration_page for Migration

row view migration_row for Migration

/// The device that reviewed `source` lists it: the migration for that source on that device,
/// made the first time, with its title and the handle the review read. What it could not read
/// is cleared; the review's items follow with `review_item`.
action reviewed(m: Migrations, source: Text, title: Text, device: Text, handle: Text) -> Migration (
    icon: .search,
    did: "reviewed a source",
) {
    let found = m.migrations.filter(x -> x.source == source and x.device == device).first()
    if let g = found {
        g.title = title
        g.handle = handle
        g.refused = ""
        for what in g.unreadable {
            g.unreadable.remove(what)
        }
        g
    } else {
        m.migrations.insert(Migration(source: source, title: title, device: device, handle: handle))
    }
}

/// The review found the item `key`: it is kept as found, and one found before keeps whether a
/// person chose it; a new one is chosen as `chosen` says.
action review_item(
    g: Migration,
    key: Text,
    kind: Text,
    title: Text,
    count: Int,
    counts: Text,
    within: Text,
    note: Text,
    chosen: Bool,
) -> Item {
    if let i = g.items.filter(x -> x.key == key).first() {
        i.kind = kind
        i.title = title
        i.count = count
        i.counts = counts
        i.within = within
        i.note = note
        i
    } else {
        g.items.insert(
            Item(
                key: key,
                kind: kind,
                title: title,
                count: count,
                counts: counts,
                within: within,
                note: note,
                chosen: chosen,
            ),
        )
    }
}

/// A fresh review no longer found the items whose keys are not in `keep`: they go.
action forget_items(g: Migration, keep: List<Text>) {
    g.items.remove_where(i -> not keep.contains(i.key))
}

/// The review could not read `what`.
action unreadable(g: Migration, what: Text) {
    g.unreadable.add(what)
}

/// Chooses an item of `g` to be brought in, or leaves it out: the device that holds the data
/// keeps the choice, and the migration follows it there, as `choose_keys`.
action choose(g: Migration, i: Item, chosen: Bool = not i.chosen) (
    icon: .check,
    label: "Bring in",
    on: i.chosen,
) {
    engine.choose(source: g.source, items: [i.key], leave_out: not chosen)
}

/// The device chose the items whose keys are `keys`, or left them out; with `only`, every other
/// item is left out.
action choose_keys(g: Migration, keys: List<Text>, chosen: Bool, only: Bool) {
    g.refused = ""
    for k in keys {
        if let i = g.items.filter(x -> x.key == k).first() {
            i.chosen = chosen
        }
    }
    if only {
        for i in g.items.filter(x -> x.chosen) {
            if not keys.contains(i.key) {
                i.chosen = false
            }
        }
    }
}

/// The device began the run `run`, or resumed it: it is running. A new run starts its counts
/// again; a resumed one goes on counting.
action begun(g: Migration, run: Text, title: Text) {
    g.state = .running
    if g.run != run {
        for c in g.brought {
            g.brought.remove(c)
        }
        // A counter starts again by taking off what it holds.
        g.already_there -= g.already_there
    }
    g.run = run
    g.run_title = title
    g.failure = ""
    g.refused = ""
}

/// The run `run` brought in the items `keys`: `count` things of `kind`, and found `already` of
/// them in the workspace before.
action landed(g: Migration, run: Text, keys: List<Text>, kind: Text, count: Int, already: Int) {
    for k in keys {
        g.landed.add(k)
    }
    let r = if let x = g.runs.filter(x -> x.id == run).first() {
        x
    } else {
        g.runs.insert(Run(id: run, title: if run == g.run { g.run_title } else { "" }))
    }
    for k in keys {
        r.keys.add(k)
    }
    if count > 0 {
        if let c = g.brought.filter(x -> x.kind == kind).first() {
            c.count += count
        } else {
            g.brought.insert(Count(kind: kind, count: count))
        }
    }
    g.already_there += already
}

/// The run ended as `state`: stopped, failed with `failure`, or complete.
action ended(g: Migration, state: RunState, failure: Text) {
    require state == .stopped or state == .failed or state == .complete "A run ends stopped, failed or complete"
    g.state = state
    g.failure = failure
}

/// The device took out of the workspace what the run `run` brought in: its keys are no longer
/// landed and the run is gone. Undoing the current run puts the migration back to reviewed.
action forgot_run(g: Migration, run: Text) {
    if let r = g.runs.filter(x -> x.id == run).first() {
        for k in r.keys {
            g.landed.remove(k)
        }
        g.runs.remove(r)
    }
    if g.run == run {
        g.state = .reviewed
        for c in g.brought {
            g.brought.remove(c)
        }
        g.already_there -= g.already_there
        g.failure = ""
    }
    g.refused = ""
}

/// The device refused a step asked of `g`, for `why`: nothing chosen to bring in, a source it
/// cannot read, a run it cannot take back.
action refused(g: Migration, why: Text) {
    g.refused = why
}

/// Read `g`'s source again: what it holds now is what there is to choose from.
action review(g: Migration) (icon: .refresh, label: "Look again", did: "looked at a source again") {
    engine.review(source: g.source)
}

/// Bring in what is chosen of `g`'s source: the device that holds the data runs it as one run,
/// and the migration follows it there, as `begun`, `landed` and `ended`. The action lasts as long
/// as the run, and stopping it stops the run.
action start(g: Migration) (icon: .play, did: "started bringing in") {
    engine.start(source: g.source)
}

/// Stop `g`'s run after the item it is on: what it brought stays, and starting again resumes it.
action stop(g: Migration) (icon: .square, did: "stopped bringing in") {
    engine.stop(source: g.source)
}

/// Take what the run `r` brought in back out of the workspace.
action undo(g: Migration, r: Run) (icon: .undo_2, did: "took a run back") {
    engine.undo(run: r.id)
}

/// What the last run brought in, as a person reads it: "120 contacts, 3 accounts".
fn brought_said(g: Migration) -> Text {
    g.brought.map(c -> "{c.count} {c.kind}").join(", ")
}

/// The item `x` sits under, when it sits under one.
fn parent_of(g: Migration, x: Item) -> Option<Item> {
    if x.within == "" {
        .none
    } else {
        g.items.filter(p -> p.key == x.within).first()
    }
}

/// Every migration of the workspace, each opening beside the list.
view migrations(m: Migrations) {
    std.browse(m, m.migrations, empty: "Nothing is being brought in") { g ->
        migration_row(g)
    }
}

/// A migration as a row: what the source is, where its run stands, and what it brought or why
/// it failed.
view migration_row(g: Migration) {
    let brought = brought_said(g)
    ui.ListRow(
        g.title,
        secondary: if g.state == .failed and g.failure != "" {
            g.failure
        } else if brought != "" { "{brought} brought in" },
        meta: g.state.label(),
        meta_tone: if g.state == .failed {
            .danger
        } else if g.state == .complete {
            .success
        } else if g.state == .running { .info } else { .neutral },
        mark: g.state.icon(),
    )
}

/// A migration's page: where its run stands, what the source holds to choose from, what could
/// not be read, and the runs that brought things in.
view migration_page(g: Migration) {
    std.document(g) {
        run_status(g)
        if not g.unreadable.is_empty() {
            section("Could not be read") {
                for what in g.unreadable {
                    text(what, secondary: true)
                }
            }
        }
        section("What to bring in") {
            if g.items.is_empty() {
                ui.EmptyState("The review found nothing to bring in", inline: true)
            } else {
                // The rows are built as they scroll into view, so a source of ten thousand items
                // costs the rows on screen. An item has no page of its own: its row is all of it.
                ui.VirtualList(tree: true, in_column: false, name: "What to bring in") {
                    for x in g.items {
                        ui.ListItem(
                            item: x,
                            presence: lenses(x).find(l -> not l.mine)?.who,
                            parent: parent_of(g, x),
                        ) {
                            item_row(g, x)
                        }
                    }
                }
            }
        }
        if not g.runs.is_empty() {
            section("Runs") {
                for r in g.runs {
                    run_row(g, r)
                }
            }
        }
    }
}

/// Where the run stands: its state, what it brought so far, and why it failed.
view run_status(g: Migration) {
    stack(gap: .related) {
        row(gap: .related) {
            ui.Badge(
                g.state.label(),
                tone: if g.state == .failed {
                    .danger
                } else if g.state == .complete {
                    .success
                } else if g.state == .running { .info } else { .neutral },
            )
            if g.state != .reviewed and g.run_title != "" {
                text(g.run_title, secondary: true)
            }
        }
        if g.handle != "" {
            text(g.handle, role: .caption, secondary: true)
        }
        if g.state == .running {
            ui.ProgressBar(label: "Bringing in")
            ui.Button("Stop", icon: .square) {
                on press: stop(g)
            }
        } else {
            row(gap: .related) {
                if g.state != .complete and g.items.any(x -> x.chosen) {
                    ui.Button(
                        if g.state == .stopped {
                            "Resume"
                        } else if g.state == .failed { "Try again" } else { "Bring in" },
                        icon: .play,
                    ) {
                        on press: start(g)
                    }
                }
                ui.Button("Look again", icon: .refresh, role: .plain) {
                    on press: review(g)
                }
            }
        }
        if g.state == .failed and g.failure != "" {
            ui.Banner("The run stopped on an error", message: g.failure, kind: .error)
        }
        if g.refused != "" {
            ui.Banner("That could not be done", message: g.refused, kind: .warning)
        }
        if not g.brought.is_empty() or g.already_there > 0 {
            ui.Facts {
                for c in g.brought {
                    ui.Fact("{c.count} {c.kind}")
                }
                if g.already_there > 0 {
                    ui.Fact("{g.already_there} already here")
                }
            }
        }
    }
}

/// An item of the source as a row: whether it is chosen, what it is, how many things it holds
/// and what the review says of it.
view item_row(g: Migration, x: Item) {
    row(
        x.title,
        checkbox: x.chosen,
        check: choose(g, x),
        secondary: if x.note != "" { x.note },
        meta: if x.count > 0 { "{x.count} {x.counts}" },
    )
}

/// A run as a row: its title, how many things it brought in, and taking it back.
view run_row(g: Migration, r: Run) {
    row(gap: .related) {
        ui.ListRow(r.title, secondary: "{r.keys.len()} items")
        if g.state != .running {
            ui.Button("Undo", icon: .undo_2, role: .plain) {
                on press: undo(g, r)
            }
        }
    }
}

/// A Thunderbird profile being brought in, halfway through its mail, and this computer's home
/// folders reviewed with the documents and pictures chosen.
scenario "a migration halfway" -> Migration {
    let m = root Migrations
    let home = m.migrations.insert(
        Migration(
            source: "home",
            title: "Home folders on this computer",
            device: "5b0e9c41d2a7f3e8",
            handle: "/home/sam",
        ),
    )
    home.items.insert(
        Item(
            key: "Documents",
            kind: "folder",
            title: "Documents",
            count: 1284,
            counts: "files",
            chosen: true,
        ),
    )
    home.items.insert(
        Item(
            key: "Pictures",
            kind: "folder",
            title: "Pictures",
            count: 9312,
            counts: "files",
            chosen: true,
        ),
    )
    home.items.insert(
        Item(
            key: "Downloads",
            kind: "folder",
            title: "Downloads",
            count: 407,
            counts: "files",
            note: "Installers and archives, most of them older than a year",
        ),
    )
    home.items.insert(
        Item(key: "Music", kind: "folder", title: "Music", count: 2210, counts: "files"),
    )
    home.items.insert(
        Item(
            key: "Videos",
            kind: "folder",
            title: "Videos",
            count: 38,
            counts: "files",
            chosen: true,
        ),
    )
    let tb = m.migrations.insert(
        Migration(
            source: "thunderbird",
            title: "Thunderbird on this computer: mail, address books, calendars",
            device: "5b0e9c41d2a7f3e8",
            handle: "default-release",
            state: .running,
            run: "run-2",
            run_title: "Mail, contacts and calendars",
            already_there: 14,
        ),
    )
    tb.items.insert(
        Item(
            key: "account:sam@rivera.example",
            kind: "account",
            title: "sam@rivera.example",
            count: 4,
            counts: "folders",
            chosen: true,
        ),
    )
    tb.items.insert(
        Item(
            key: "folder:sam@rivera.example/Inbox",
            kind: "folder",
            title: "Inbox",
            count: 2412,
            counts: "messages",
            within: "account:sam@rivera.example",
            chosen: true,
        ),
    )
    tb.items.insert(
        Item(
            key: "folder:sam@rivera.example/Sent",
            kind: "folder",
            title: "Sent",
            count: 861,
            counts: "messages",
            within: "account:sam@rivera.example",
            chosen: true,
        ),
    )
    tb.items.insert(
        Item(
            key: "folder:sam@rivera.example/Archive",
            kind: "folder",
            title: "Archive",
            count: 5120,
            counts: "messages",
            within: "account:sam@rivera.example",
            chosen: true,
        ),
    )
    tb.items.insert(
        Item(
            key: "folder:sam@rivera.example/Junk",
            kind: "folder",
            title: "Junk",
            count: 96,
            counts: "messages",
            within: "account:sam@rivera.example",
        ),
    )
    tb.items.insert(
        Item(
            key: "addressbook:personal",
            kind: "address book",
            title: "Personal Address Book",
            count: 120,
            counts: "contacts",
            chosen: true,
        ),
    )
    tb.items.insert(
        Item(
            key: "addressbook:collected",
            kind: "address book",
            title: "Collected Addresses",
            count: 1873,
            counts: "contacts",
            note: "Everyone you ever wrote to, gathered by Thunderbird",
        ),
    )
    tb.items.insert(
        Item(
            key: "calendar:home",
            kind: "calendar",
            title: "Home",
            count: 312,
            counts: "events",
            chosen: true,
        ),
    )
    tb.unreadable.add("Local Folders: the index of Archives is damaged; repair it in Thunderbird")
    tb.brought.insert(Count(kind: "contacts", count: 120))
    tb.brought.insert(Count(kind: "events", count: 312))
    tb.brought.insert(Count(kind: "messages", count: 1840))
    let earlier = tb.runs.insert(Run(id: "run-1", title: "Contacts"))
    earlier.keys.add("addressbook:personal")
    let now = tb.runs.insert(Run(id: "run-2", title: "Mail, contacts and calendars"))
    now.keys.add("calendar:home")
    tb
}
