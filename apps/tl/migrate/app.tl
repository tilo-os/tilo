app so.tilo.migrate "Migrations" (
    icon: .truck,
    category: .system,
    tagline: "Bring in what your computers and accounts hold",
    keywords: ["migrate", "import", "move", "bring in", "thunderbird", "browsers", "cloud storage"],
)

use so.tilo.migrate.engine as engine
use so.tilo.session as session
use tilo.accounts as accounts

/// Where a migration's run stands: offered, a source the device can read that nobody looked at
/// yet; reviewed, with what the source holds waiting to be chosen;
/// running, while the device that holds the data brings in what was chosen; stopped by a person,
/// keeping what it brought until then; failed, its failure saying why; or complete. The device
/// that holds the data moves it on as its run goes.
enum RunState {
    /// Not looked at yet
    offered (icon: .circle_plus)
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
    /// Why the device cannot read this source at all (no Thunderbird profile, no browser), as it
    /// said when it offered it; empty for one it can.
    unavailable: Text = ""
    /// What connecting asks for before the source can be read: the server's address, as its
    /// example reads (`https://cloud.example.com`), or empty when it takes none.
    asks_url: Text = ""
    /// The sign-in name connecting asks for, as its example reads (`you@example.com`), or empty.
    asks_login: Text = ""
    /// Whether the source is an account to connect before it is read.
    connects: Bool = false
    /// Who started the last run: told when it ends.
    started_by: Option<Actor> = .none
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
        if g.state == .offered {
            g.state = .reviewed
        }
        for what in g.unreadable {
            g.unreadable.remove(what)
        }
        g
    } else {
        m.migrations.insert(Migration(source: source, title: title, device: device, handle: handle))
    }
}

/// A source the device that holds the data can read, as it offers it.
type Offered {
    /// The engine's key for the source.
    source: Text
    title: Text
    /// Why the device cannot read it at all; empty when it can.
    unavailable: Text = ""
    /// Whether it is an account to connect before it is read.
    connects: Bool = false
    /// The server's address connecting asks for, as its example reads, or empty.
    asks_url: Text = ""
    /// The sign-in name connecting asks for, as its example reads, or empty.
    asks_login: Text = ""
}

/// What the engine's `sources` answers: the device that read them, and each source as the list
/// shows it.
type OfferedSources {
    /// The key of the device that holds the data.
    device: Text = ""
    /// Each source the device reads, as the list offers it.
    sources: List<Offered>
}

/// The device `device` offers the sources `sources`: each is a migration the list shows for a
/// person to pick, made the first time, saying why the device cannot read it and what connecting
/// it asks for. The app writes this of the engine's answer, so the list is the app's own records
/// and a device without the app still lists the same sources to `tilo migrate sources`.
action offer(m: Migrations, device: Text, sources: List<Offered>) (
    icon: .circle_plus,
    did: "found sources",
) {
    for o in sources {
        let found = m.migrations.filter(x -> x.source == o.source and x.device == device).first()
        if let g = found {
            if g.unavailable != o.unavailable {
                g.unavailable = o.unavailable
            }
            if g.connects != o.connects {
                g.connects = o.connects
            }
            if g.asks_url != o.asks_url {
                g.asks_url = o.asks_url
            }
            if g.asks_login != o.asks_login {
                g.asks_login = o.asks_login
            }
        } else {
            m.migrations.insert(
                Migration(
                    source: o.source,
                    title: o.title,
                    device: device,
                    state: .offered,
                    unavailable: o.unavailable,
                    connects: o.connects,
                    asks_url: o.asks_url,
                    asks_login: o.asks_login,
                ),
            )
        }
    }
}

/// The device refused a step asked of `source` before anything of it was read, for `why`: the
/// source it offered says so until the next step goes through.
action source_refused(m: Migrations, source: Text, device: Text, why: Text) {
    if let g = m.migrations.filter(x -> x.source == source and x.device == device).first() {
        g.refused = why
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
action ended(g: Migration, state: RunState, failure: Text) uses notify {
    require state == .stopped or state == .failed or state == .complete "A run ends stopped, failed or complete"
    g.state = state
    g.failure = failure
    if let who = g.started_by {
        if state == .complete {
            let brought = brought_said(g)
            notify(
                to: who,
                title: g.title,
                body: if brought != "" {
                    "Brought in {brought}"
                } else { "Everything chosen is here" },
                opens: g,
            )
        } else if state == .failed {
            notify(to: who, title: g.title, body: "Bringing in stopped: {failure}", opens: g)
        }
    }
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

/// Lists every source this device can bring things in from, each as a migration nobody looked at
/// yet: what a person picks from to start one. The device's engine lists them, and the app puts
/// what it answered in its own records.
primary action find_sources(m: Migrations) -> Result<Int, Text> (
    icon: .plus,
    label: "Bring something in",
    did: "looked for sources",
) {
    match json.decode<OfferedSources>(engine.sources()) {
        .ok(found) => {
            offer(m, found.device, found.sources)
            return .ok(found.sources.len())
        }
        // The engine answers these itself, in the shape this app reads: an answer that is not
        // one is the device's fault, said as it is, and the list stays as it was.
        .err(why) => .err("the device answered sources this app cannot read: {why}"),
    }
}

/// Read `g`'s source again: what it holds now is what there is to choose from.
action review(g: Migration) (icon: .refresh, label: "Look again", did: "looked at a source again") {
    engine.review(source: g.source)
}

/// Signs in to the account `g` is, with the server's address and the sign-in name it asks for
/// and the password or key the person typed; the device keeps the secret in its vault and reads
/// the account once it is connected.
action connect(g: Migration, url: Text = "", login: Text = "", secret: Text = "") (
    icon: .link,
    label: "Connect",
    did: "connected a source",
) {
    require g.asks_url == "" or url.trim() != "" "Type the server's address"
    require g.asks_login == "" or login.trim() != "" "Type the name you sign in with"
    engine.connect(
        source: g.source,
        url: if url.trim() == "" { .none } else { .some(url.trim()) },
        login: if login.trim() == "" { .none } else { .some(login.trim()) },
        secret: if secret == "" { .none } else { .some(secret) },
    )
}

/// Bring in what is chosen of `g`'s source: the device that holds the data runs it as one run,
/// and the migration follows it there, as `begun`, `landed` and `ended`. The action lasts as long
/// as the run, and stopping it stops the run.
action start(g: Migration) (icon: .play, did: "started bringing in") {
    g.started_by = .some(invoker)
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
    std.browse(
        m,
        m.migrations,
        empty: "Nothing is being brought in. Choose Bring something in to see what this computer can bring.",
    ) { g ->
        migration_row(g)
    }
}

/// A migration as a row: what the source is, where its run stands, and what it brought or why
/// it failed.
view migration_row(g: Migration) {
    let brought = brought_said(g)
    ui.ListRow(
        g.title,
        secondary: if g.unavailable != "" {
            g.unavailable
        } else if g.state == .failed and g.failure != "" {
            g.failure
        } else if g.refused != "" { g.refused } else if brought != "" { "{brought} brought in" },
        meta: if g.unavailable != "" { "Not here" } else { g.state.label() },
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
        if g.state == .offered {
            source_start(g)
        } else {
            migration_body(g)
        }
    }
}

/// A source nobody looked at yet: why the device cannot read it, the account to connect first,
/// or looking at what it holds.
view source_start(g: Migration) {
    state url: Text = ""
    state login: Text = ""
    state secret: Text = ""
    stack(gap: .related) {
        if g.unavailable != "" {
            ui.Banner("This computer cannot bring this in", message: g.unavailable, kind: .info)
        } else if g.connects {
            text(
                "Connect the account first. Nothing is brought in until you choose what.",
                secondary: true,
            )
            form {
                if g.asks_url != "" {
                    field("Server") {
                        ui.TextInput(bind: url, placeholder: g.asks_url, autofocus: true)
                    }
                }
                if g.asks_login != "" {
                    field("Sign in as") {
                        ui.TextInput(bind: login, placeholder: g.asks_login)
                    }
                }
                field("Password or key", help: "Kept in this computer's vault.") {
                    ui.TextInput(bind: secret, secret: true)
                }
            }
            ui.Button("Connect", icon: .link) {
                on press: connect(g, url, login, secret).then(set(secret, ""))
            }
        } else {
            text("See what it holds, then choose what to bring in.", secondary: true)
            ui.Button("Look at what it holds", icon: .search) {
                on press: review(g)
            }
        }
        if g.refused != "" {
            ui.Banner("That could not be done", message: g.refused, kind: .warning)
        }
    }
}

/// A migration that was looked at: where its run stands, what the source holds to choose from,
/// what could not be read, and the runs that brought things in.
view migration_body(g: Migration) {
    let signed_in = g.items.filter(x -> x.kind == "account-to-connect")
    stack(gap: .group) {
        run_status(g)
        if g.state == .complete and not signed_in.is_empty() {
            section("Accounts to connect") {
                accounts_to_connect(g)
            }
        }
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
                    for x in g.items.filter(x -> x.kind != "account-to-connect") {
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

/// The accounts the browsers a run read were signed in to, offered once the run is done: one
/// confirmation begins each one's own sign-in, and nothing is connected until the person
/// approves it on that page. A browser's cookies never connect an account.
view accounts_to_connect(g: Migration) {
    state confirmed: Bool = false
    let found = g.items.filter(x -> x.kind == "account-to-connect")
    stack(gap: .related) {
        text(
            "These browsers were also signed in to {found.map(x -> x.title).join(", ")}. Connect them as the workspace's accounts, with their mail, calendars and files? Each asks on its own sign-in page, and nothing is connected until you approve it there.",
            secondary: true,
        )
        for x in found {
            account_to_connect(g, x, confirmed)
        }
        if not confirmed {
            ui.Button("Connect them", icon: .link, role: .primary) {
                on press: set(confirmed, true)
            }
        }
    }
}

/// One account a browser was signed in to: its own sign-in, begun once the person confirmed
/// connecting them, the page it asks on, and whether the workspace has it. Microsoft's asks for
/// the address it signs in with when no saved password named it; Spotify's is Music's.
view account_to_connect(g: Migration, x: Item, begin: Bool) {
    state open: Text = ""
    state code: Text = ""
    state typed: Text = x.note
    let spotify = x.key.ends_with("/spotify")
    let provider = if x.key.ends_with("/google") { "google" } else { "microsoft" }
    let connected = not spotify
        and accounts.accounts.accounts.any(
            a -> a.provider == provider and (typed == "" or a.address == typed),
        )
    stack(gap: .related) {
        ui.ListRow(
            x.title,
            secondary: if x.note != "" { x.note } else { parent_of(g, x)?.title ?? "" },
            meta: if connected { "Connected" },
            meta_tone: if connected { .success } else { .neutral },
        )
        if begin and not connected {
            if spotify {
                text(
                    "Spotify connects in Music, on Spotify's own page.",
                    role: .caption,
                    secondary: true,
                )
                ui.Button("Open Music", icon: .link) {
                    on press: session.open_app(app: "so.tilo.music")
                }
            } else if open != "" {
                ui.Link(open, label: "Sign in on {x.title}'s page")
                if code != "" {
                    text(code, role: .caption, secondary: true)
                }
            } else if provider == "microsoft" and x.note == "" {
                ui.TextInput(bind: typed, placeholder: "The address you sign in to Microsoft with")
                ui.Button("Sign in", icon: .link) {
                    on press: {
                        let step = accounts.begin_sign_in(
                            accounts.accounts,
                            provider: provider,
                            address: typed,
                            mail: true,
                            calendar: true,
                            files: true,
                        )
                        set(open, step.open).then(set(code, step.code))
                    }
                }
            } else {
                after 0s: {
                    let step = accounts.begin_sign_in(
                        accounts.accounts,
                        provider: provider,
                        address: typed,
                        mail: true,
                        calendar: true,
                        files: true,
                    )
                    set(open, step.open).then(set(code, step.code))
                }
            }
        }
    }
}

/// Where the run stands: its state, what it brought so far, and why it failed.
view run_status(g: Migration) {
    stack(gap: .related) {
        row(gap: .related, wrap: true) {
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

/// This computer's browsers brought in, and the accounts their profiles were signed in to
/// offered to connect.
scenario "browsers signed in to accounts" -> Migration {
    let m = root Migrations
    let g = m.migrations.insert(
        Migration(
            source: "browsers",
            title: "Browsers on this computer",
            device: "5b0e9c41d2a7f3e8",
            handle: "/home/sam",
            state: .complete,
            run: "run-1",
            run_title: "Bring 412 bookmarks and the sign-ins to 38 sites",
        ),
    )
    g.items.insert(
        Item(
            key: "firefox/default-release",
            kind: "browser-profile",
            title: "Firefox · default-release",
            count: 450,
            counts: "things",
            chosen: true,
        ),
    )
    g.items.insert(
        Item(
            key: "firefox/default-release/bookmarks",
            kind: "bookmarks",
            title: "Firefox · default-release bookmarks",
            count: 412,
            counts: "bookmarks",
            within: "firefox/default-release",
            chosen: true,
        ),
    )
    g.items.insert(
        Item(
            key: "firefox/default-release/sign-ins",
            kind: "sign-ins",
            title: "Firefox · default-release sign-ins",
            count: 38,
            counts: "sites",
            within: "firefox/default-release",
            chosen: true,
        ),
    )
    g.items.insert(
        Item(
            key: "firefox/default-release/account/google",
            kind: "account-to-connect",
            title: "Google",
            within: "firefox/default-release",
            note: "sam.rivera@gmail.com",
        ),
    )
    g.items.insert(
        Item(
            key: "firefox/default-release/account/microsoft",
            kind: "account-to-connect",
            title: "Microsoft",
            within: "firefox/default-release",
        ),
    )
    g.items.insert(
        Item(
            key: "firefox/default-release/account/spotify",
            kind: "account-to-connect",
            title: "Spotify",
            within: "firefox/default-release",
        ),
    )
    g.brought.insert(Count(kind: "bookmarks", count: 412))
    g.brought.insert(Count(kind: "sign-ins", count: 3))
    g
}

scenario "a mail account not looked at yet" -> Migration {
    let m = root Migrations
    m.migrations.insert(
        Migration(
            source: "imap",
            title: "A mail account on its server",
            device: "5b0e9c41d2a7f3e8",
            state: .offered,
            connects: true,
            asks_url: "imaps://mail.example.com",
            asks_login: "you@example.com",
        ),
    )
}

scenario "a source this computer cannot read" -> Migration {
    let m = root Migrations
    m.migrations.insert(
        Migration(
            source: "outlook",
            title: "Outlook data files",
            device: "5b0e9c41d2a7f3e8",
            state: .offered,
            unavailable: "Outlook keeps its mail on Windows, and this computer runs Linux.",
        ),
    )
}
