app so.tilo.activity "Activity" (
    icon: .clock,
    category: .office,
    tagline: "What changed in your workspace, and who changed it",
    keywords: ["history", "activity", "changes", "undo", "replay", "work sessions", "who changed", "verlauf", "änderungen"],
)

use so.tilo.history as history

/// The workspace's activity as this window shows it. The history itself is the session's,
/// read through `so.tilo.history`: this app keeps no copy of it, and every change it shows
/// reaches every other window as the session publishes it.
type Window {
    /// The view the window last showed: what is new, the changes, or the replay.
    place: Text = "whats-new"
}

root window: Window

default view activity for Window

/// The window: what is new since the person last looked, every change with what can be done
/// with it, or the workspace played back change by change. The places stand in the sidebar, as
/// the app's own, and the search is the window's.
view activity(w: Window) {
    state place: Text = "whats-new"
    state found: Text = ""
    state scope: Text = "this-workspace"
    state period: Text = "all-time"
    state writer: Text = ""
    state lens: Text = ""
    state first: Text = ""
    state last: Text = ""
    // The name the person is giving the range they selected.
    state name: Text = ""
    // The ask from outside this window this one has already shown.
    state claimed: Int = 0
    let h = history.history
    ui.NavShell(content_kind: .page, title: "Activity", sidebar_title: "Activity") {
        // The window's search is the system's, narrowed to this app: what is typed reaches the
        // frame as the chosen place narrows it, and the app keeps no field of its own.
        on search(q: Text): set(found, q)
        actions: {
            ui.SearchButton()
        }
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavItem("whats-new", "What's new", icon: .sparkles) {
                    on select: set(place, "whats-new")
                }
                ui.NavItem("history", "History", icon: .clock) {
                    on select: set(place, "history")
                }
                ui.NavItem("replay", "Replay", icon: .play) {
                    on select: set(place, "replay")
                }
            }
        }
        content: {
            std.page(w) {
                // A chrome verb outside the window — catch up, explore one person's doings, open
                // a change — asks it to show a place, narrowed or selected. The window takes the
                // ask into its own state and says it was shown, so the session stops asking.
                if let f = h.focus {
                    if f.serial > claimed {
                        every 1s: set(claimed, f.serial)
                            .then(set(place, place_of(f.view)))
                            .then(set(scope, "this-workspace"))
                            .then(set(writer, f.author ?? ""))
                            .then(set(lens, f.lens ?? ""))
                            .then(set(first, f.entry ?? ""))
                            .then(set(last, f.entry ?? ""))
                            .then(
                                history.focused(history.history, serial: f.serial),
                            )
                    }
                }
                if place == "whats-new" {
                    whats_new(h, place, first, last)
                } else if place == "history" {
                    changes(h, found, scope, period, writer, lens, first, last, name)
                } else {
                    replay(h)
                }
            }
        }
    }
}

/// What changed while the person was away: the work sessions told as outcomes, what a model on
/// this device made of them, where they left off, and the sources this device hears about each
/// morning.
view whats_new(h: history.Activity, place: State<Text>, first: State<Text>, last: State<Text>) {
    if let s = h.summary {
        if s.pending {
            ui.Text("Loading changes", role: .caption, secondary: true)
        }
        // What this device showed as new is seen once the person says so, so a later visit
        // starts from here rather than counting the same changes again.
        let shown = s.briefs.flat_map(brief -> brief.entries)
        if not shown.is_empty() {
            ui.Button("Mark as viewed", icon: .check) {
                on press: history.mark_viewed(history.history, entries: shown, later: false)
            }
        }
        if let narrative = s.narrative {
            ui.Section("On-device summary") {
                ui.Text(narrative)
            }
        }
        if let resume = s.resume {
            ui.Section("Where you left off") {
                row(gap: .related, align: .center) {
                    ui.Text(resume.target.label, role: .body)
                    let made_in = resume.app ?? resume.target.app_label
                    if let named = made_in {
                        ui.Text(named, role: .caption, secondary: true)
                    }
                    ui.Text(when(resume.at_ms), role: .caption, secondary: true)
                    let target = resume.target
                    ui.Button("Resume") {
                        on press: history.open(
                            history.history,
                            id: target.id,
                            kind: target.kind,
                            label: target.label,
                        )
                    }
                }
            }
        }
        // The work sessions told as outcomes, under the words that say why they are here: the
        // catch-up after time away, or the day's work.
        if not s.briefs.is_empty() {
            ui.Section(if s.away { "While you were away" } else { "Work sessions" }) {
                for brief in s.briefs {
                    brief_card(brief, place, first, last)
                }
            }
        } else if not s.pending {
            ui.Text("You're caught up", role: .body)
            ui.Button("See history") {
                on press: set(place, "history")
            }
        }
        if s.sources_available {
            ui.Section("Morning sources") {
                if s.sources.is_empty() {
                    ui.Text(
                        "Choose a resource to hear about each morning from a change's details.",
                        role: .caption,
                        secondary: true,
                    )
                }
                for source in s.sources {
                    ui.ListRow(
                        title: source.label,
                        secondary: source.text,
                        meta: source.status,
                        icon: .sparkles,
                    ) {
                        // Activating a source opens what it heard about this morning.
                        on activate: history.source(
                            history.history,
                            key: source.key,
                            command: .open,
                        )
                        trailing: {
                            row(gap: .tight, align: .center) {
                                if source.refreshable {
                                    ui.Button("Refresh source", icon: .refresh, icon_only: true) {
                                        on press: history.source(
                                            history.history,
                                            key: source.key,
                                            command: .refresh,
                                        )
                                    }
                                }
                                ui.Button("Remove morning source", icon: .x, icon_only: true) {
                                    on press: history.source(
                                        history.history,
                                        key: source.key,
                                        command: .remove,
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One work session: what it achieved, who did it, and the actions it took. Opening it in
/// History selects the changes it made, not only the place that lists them.
view brief_card(brief: history.Brief, place: State<Text>, first: State<Text>, last: State<Text>) {
    ui.Card(title: brief.title) {
        ui.Text(brief.detail, role: .caption, secondary: true)
        ui.Text(
            "{when(brief.last_ms)}, {join(brief.authors.map(a -> a.name))}",
            role: .caption,
            secondary: true,
        )
        if brief.actions.is_empty() {
            ui.Text("No changes in this work session", role: .caption, secondary: true)
        }
        for step in brief.actions {
            ui.ListRow(
                title: step.summary,
                secondary: step.author.name,
                meta: when(step.at_ms),
                icon: .history,
            )
        }
        if let from = brief.entries.first() {
            if let to = brief.entries.last() {
                ui.Button("Open in History") {
                    on press: set(place, "history").then(set(first, from)).then(set(last, to))
                }
            }
        }
    }
}

/// Every change, narrowed by what the person chose, with the range they selected shown beside
/// it: what it did, what can be done with it, and what changed either side. The filters and the
/// presses that change them live here, in the view that declares what they change.
view changes(
    h: history.Activity,
    found: Text,
    scope: State<Text>,
    period: State<Text>,
    writer: State<Text>,
    lens: State<Text>,
    first: State<Text>,
    last: State<Text>,
    name: State<Text>,
) {
    // How many of the newest changes the page holds: one page, and two more at each "Show more".
    state limit: Int = 0
    let page_limit = if limit < 200 { 200 } else { limit }
    // While the search is on the page reads further back: one page is not enough to search the
    // history.
    let asked = if found != "" { page_limit * 4 } else { limit }
    // The page is the session's reading of exactly the filters this window shows, keyed by
    // them: another window on another period reads its own page.
    let key_scope = if scope == "all-workspaces" { "all" } else { "this" }
    let key_lens = if lens == "" { "-" } else { lens }
    let page = resource(
        "workspace.activity/feed~{key_scope}~{period}~{key_lens}~{asked}",
        of: history.FeedReading,
    )
    let feed = page.feed
    row(gap: .related, wrap: true, align: .center) {
        ui.Button("This workspace", toggled: scope == "this-workspace") {
            on press: set(scope, "this-workspace")
        }
        ui.Button("All workspaces", toggled: scope == "all-workspaces") {
            on press: set(scope, "all-workspaces")
        }
        spacer
        ui.Text("Time", role: .caption, secondary: true)
        for label in ["All time", "Today", "7 days", "30 days"] {
            ui.Button(label, toggled: period == period_of(label)) {
                on press: set(period, period_of(label))
            }
        }
    }
    if let why = unreadable(page) {
        std.unreadable_row("The changes could not be read", why)
    } else if let f = feed {
        row(gap: .related, wrap: true, align: .center) {
            ui.Text("People", role: .caption, secondary: true)
            ui.Button("Everyone", toggled: writer == "") {
                on press: set(writer, "")
            }
            for person in f.people {
                ui.Button(person, toggled: writer == person) {
                    on press: set(writer, person)
                }
            }
            spacer
            ui.Text("Browse by", role: .caption, secondary: true)
            ui.Button("Everything", toggled: lens == "") {
                on press: set(lens, "")
            }
            for one in f.lenses {
                ui.Button(one.label, toggled: lens == one.key) {
                    on press: set(lens, one.key)
                }
            }
            if first != "" {
                ui.Button("Clear filters", icon: .x) {
                    on press: set(scope, "this-workspace")
                        .then(set(period, "all-time"))
                        .then(set(writer, ""))
                        .then(set(lens, ""))
                        .then(set(first, ""))
                        .then(set(last, ""))
                }
            }
        }
        if not f.mutations {
            ui.Text("This device cannot change the history here.", role: .caption, secondary: true)
        }
        let changes = matched(f.entries, writer, found)
        let kept = changes.count()
        let held = f.entries.count()
        // What the words and the person left of the changes the page holds.
        if found != "" or writer != "" {
            ui.Text("{kept} of {held} changes", role: .caption, secondary: true)
        }
        row(gap: .section, align: .stretch) {
            stack(fill, gap: .related) {
                if f.entries.is_empty() {
                    ui.Text("No changes here", role: .body)
                }
                if changes.is_empty() and not f.entries.is_empty() {
                    ui.Text("No changes match these filters", role: .body)
                }
                // The changes sit under the work session that made them, or else under their
                // day, as the history grouped them.
                for chunk in chunks(changes, f.groups) {
                    ui.Section(chunk.label) {
                        if chunk.work_session {
                            ui.Button("Select", size: .small, role: .plain) {
                                on press: set(first, chunk.from).then(set(last, chunk.to))
                            }
                        }
                        for entry in chunk.entries {
                            change_card(entry, first, last)
                        }
                    }
                }
                // Older changes are reachable: the page grows a page at a time toward the
                // oldest the history holds.
                if f.total > f.entries.count() {
                    ui.Button("Show earlier changes", icon: .chevron_down) {
                        on press: set(limit, page_limit + 200)
                    }
                }
            }
            // The range the person selected, beside the list as the window's inspector: its
            // detail is the session's reading of that one range, so another window showing
            // another range reads its own. Until one is chosen the column says what to do.
            if first != "" {
                stack(gap: .related) {
                    detail_card(first, last, name)
                }
            } else {
                stack(gap: .related) {
                    ui.EmptyState(
                        "Select a work session or a range of changes",
                        icon: .history,
                        inline: true,
                    )
                }
            }
        }
    } else {
        ui.Text("Loading changes", role: .caption, secondary: true)
    }
}

/// One change: what it was, who made it, where it landed, and which of the person's devices
/// refused it and why. Its own press selects it and what follows it.
view change_card(entry: history.Entry, first: State<Text>, last: State<Text>) {
    ui.Card(title: entry.summary) {
        row(gap: .related, align: .center) {
            ui.Text(entry.author.name, role: .caption)
            ui.Text(when(entry.at_ms), role: .caption, secondary: true)
            if entry.id == first or entry.id == last {
                ui.Badge("Selected", tone: .info)
            }
            if let made_in = entry.app {
                ui.Badge(made_in, tone: .neutral)
            }
            if let changed = entry.resource {
                ui.Text(changed, role: .caption, secondary: true)
            }
            if let workspace = entry.workspace {
                ui.Text(workspace, role: .caption, secondary: true)
            }
        }
        for refusal in entry.refused {
            ui.Text("Not on {refusal.label}: {refusal.reason}", role: .caption, secondary: true)
        }
        ui.Button("Select through here") {
            on press: if first == "" {
                set(first, entry.id).then(set(last, entry.id))
            } else { set(last, entry.id) }
        }
    }
}

/// A run of changes under one heading: the work session they belong to, or the day they were
/// made on.
type Chunk {
    key: Text
    label: Text
    /// Whether a work session, whose changes are selected together, made them.
    work_session: Bool
    from: Text
    to: Text
    entries: List<history.Entry>
}

/// What a change sits under: the work session the history grouped it in, or the day it was made
/// on. A change without a time is undated.
fn heading(entry: history.Entry, groups: List<history.Group>) -> Chunk {
    if let id = entry.group {
        if let group = groups.find(one -> one.id == id) {
            let end = group.end ?? group.start
            return Chunk(
                key: "group:{id}",
                label: if group.label == "" { "Work group" } else { group.label },
                work_session: true,
                from: group.start,
                to: end,
                entries: [],
            )
        }
        return Chunk(
            key: "group:{id}",
            label: "Work group",
            work_session: true,
            from: entry.id,
            to: entry.id,
            entries: [],
        )
    }
    let day = if entry.at_ms <= 0 { "Undated" } else { "{moment(entry.at_ms).day()}" }
    Chunk(
        key: "day:{day}",
        label: day,
        work_session: false,
        from: entry.id,
        to: entry.id,
        entries: [],
    )
}

/// The changes in order, each run of them under the heading it shares with its neighbours.
fn chunks(entries: List<history.Entry>, groups: List<history.Group>) -> List<Chunk> {
    let out: List<Chunk> = []
    for entry in entries {
        let under = heading(entry, groups)
        let joined = false
        if let tail = out.last() {
            if tail.key == under.key {
                let grown = Chunk(
                    key: tail.key,
                    label: tail.label,
                    work_session: tail.work_session,
                    from: tail.from,
                    to: tail.to,
                    entries: tail.entries + [entry],
                )
                out = out.take(out.len() - 1) + [grown]
                joined = true
            }
        }
        if not joined {
            out = out
                + [
                    Chunk(
                        key: under.key,
                        label: under.label,
                        work_session: under.work_session,
                        from: under.from,
                        to: under.to,
                        entries: [entry],
                    ),
                ]
        }
    }
    out
}

/// The range the person selected: what it did, what can be done with it, what changed either
/// side of it, and — when the changes are an agent's work — the session that asked for it and
/// each record's own undo. Its detail is the session's reading of that one range, so two
/// windows showing two ranges each read their own.
view detail_card(first: State<Text>, last: State<Text>, name: State<Text>) {
    // The comparison is read only while the person asks for it: what changed either side of the
    // range, and the fields it changed.
    state compare: Bool = false
    // How many of a session's changes still in effect its card holds: a page, and more at each
    // "Show more".
    state more: Int = 0
    let pages = if more < 20 { 20 } else { more }
    let shown = resource("workspace.activity/range~{first}~{last}", of: history.RangeDetail)
    if let d = shown.detail {
        ui.Section("Details") {
            if d.gone {
                ui.Text(
                    "These changes are no longer in the history.",
                    role: .caption,
                    secondary: true,
                )
            }
            if d.title != "" {
                ui.Text(d.title, role: .heading)
            }
            if d.provenance != "" {
                ui.Text(d.provenance, role: .caption, secondary: true)
            }
            if not d.authors.is_empty() {
                ui.Text("By {d.authors.join(", ")}", role: .caption, secondary: true)
            }
            if let session = d.group_label {
                ui.Text("Work session: {session}", role: .caption, secondary: true)
            }
            if not d.resources.is_empty() {
                ui.Section("Changed") {
                    for target in d.resources {
                        ui.Button(target.label, icon: .arrow_up_right) {
                            on press: history.open(
                                history.history,
                                id: target.id,
                                kind: target.kind,
                                label: target.label,
                            )
                        }
                    }
                }
            }
            // Each offer the model words for this range is its own press: an offer names one
            // of the model's verbs, and the press calls that verb.
            if let undo = d.offers.find(o -> o.action == .undo) {
                ui.Button(undo.label) {
                    on press: history.undo(history.history, from: first, to: last)
                }
            }
            if let restore = d.offers.find(o -> o.action == .restore) {
                ui.Button(restore.label) {
                    on press: history.restore(history.history, from: first, to: last)
                }
            }
            if let split = d.offers.find(o -> o.action == .split) {
                ui.Button(split.label) {
                    on press: history.split(history.history, from: first, to: last)
                }
            }
            // The offer names the action it does, as the model declares it.
            if let merged = d.offers.find(o -> o.action == .merge) {
                ui.Button(merged.label) {
                    on press: history.merge(history.history, from: first, to: last)
                }
            }
            if let session = d.offers.find(o -> o.action == .undo-session) {
                ui.Button(session.label) {
                    on press: history.undo_session(history.history, from: first, to: last)
                }
            }
            if let source = d.offers.find(o -> o.action == .choose-source) {
                ui.Button(source.label) {
                    on press: history.choose_source(history.history, from: first, to: last)
                }
            }
            // Comparing asks the session for the workspace either side of the range: the offer
            // names that read, which the window takes when the person turns it on.
            if let offer = d.offers.find(o -> o.action == .compare) {
                ui.Button(offer.label, toggled: compare) {
                    on press: set(compare, not compare)
                }
            }
        }
        if compare {
            compare_card(first, last)
        }
        name_section(first, last, d.group_label, name)
        // The session the changes are, and how many of its changes still in effect this window
        // shows: its own page, so another window showing another page reads its own.
        let sessions = resource("workspace.activity/sessions~this~{pages}", of: history.Sessions)
        if let s = session_in(sessions.sessions, d.group, first, last) {
            session_card(s, more, pages, first, last)
        }
    }
}

/// The workspace before and after the selected range, as the session read it: each field that
/// moved, or else how the content differs, line by line.
view compare_card(first: State<Text>, last: State<Text>) {
    let shown = resource("workspace.activity/compare~{first}~{last}", of: history.RangeDetail)
    if let c = shown.detail {
        if c.gone {
            ui.Text("These changes are no longer in the history.", role: .caption, secondary: true)
        } else if not c.changes.is_empty() {
            // Each field the range changed, with what it holds now and what it held before; a
            // field that now holds nothing says so.
            ui.Section("Fields") {
                for change in c.changes {
                    ui.ListRow(
                        change.field,
                        secondary: was_line(change),
                        value: change.after,
                        icon: .pencil,
                    )
                }
            }
        } else if let comparison = c.comparison {
            match comparison {
                .changes(lines) => {
                    ui.Section("What changed") {
                        ui.Text(lines.join("\n"), role: .body)
                    }
                }
                .unavailable => {
                    ui.EmptyState(
                        "Preview unavailable",
                        message: "The workspace as it was before and after this change could not be read.",
                        icon: .history,
                        inline: true,
                    )
                }
            }
        } else {
            ui.Text("No content differences in this selection", role: .caption, secondary: true)
        }
    }
}

/// What a field held before the range changed it, in the words a person reads.
fn was_line(change: history.FieldChange) -> Text {
    if change.after == "" {
        std.say("Nothing now")
    } else if change.before == "" { "" } else { std.say("Was {change.before}") }
}

/// The range's name as a work session: typed, then saved with its button. The field starts empty
/// and its placeholder says what the words name.
view name_section(first: State<Text>, last: State<Text>, kept: Option<Text>, name: State<Text>) {
    ui.Section("Name") {
        if let named = kept {
            ui.Text(named, role: .caption, secondary: true)
        }
        row(gap: .related, align: .center) {
            ui.TextInput(bind: name, placeholder: "Name this work session")
            ui.Button("Save name") {
                on press: history.name(
                    history.history,
                    range: history.Range(from: first, to: last),
                    label: name,
                )
            }
        }
    }
}

/// The range's agent work: why it was asked for, how it was decided, whether it can be undone,
/// and every change it still leaves in place, each with where it landed, a link to it, and the
/// system's undo on its own.
view session_card(
    s: history.AgentSession,
    more: State<Int>,
    pages: Int,
    first: State<Text>,
    last: State<Text>,
) {
    ui.Section("Agent task") {
        ui.Text("Why this work", role: .caption, secondary: true)
        ui.Text(s.instruction)
        ui.Text(started_by(s.agent), role: .caption, secondary: true)
        ui.Text("Decision", role: .caption, secondary: true)
        if let decision = s.decision {
            ui.Text(decision)
        } else {
            ui.Text("No reason was recorded.", role: .caption, secondary: true)
        }
        if let blocked = s.undo_blocked {
            ui.Text(blocked, role: .caption, secondary: true)
        }
    }
    ui.Section("Changes still in effect") {
        if s.in_effect.is_empty() {
            ui.Text(
                "This session leaves nothing in place: every change it made was undone.",
                role: .caption,
                secondary: true,
            )
        }
        for c in s.in_effect {
            ui.ListRow(title: c.summary, secondary: changed_where(c), icon: .clock) {
                trailing: {
                    row(gap: .tight, align: .center) {
                        if c.target.id != "" {
                            ui.Button(
                                c.target.label,
                                icon: .arrow_up_right,
                                size: .small,
                                role: .plain,
                            ) {
                                on press: history.open(
                                    history.history,
                                    id: c.target.id,
                                    kind: c.target.kind,
                                    label: c.target.label,
                                )
                            }
                        }
                        if let undo = c.undo {
                            ui.Button(undo.label, size: .small, role: .plain) {
                                on press: history.undo_record(
                                    history.history,
                                    range: history.Range(from: c.range.from, to: c.range.to),
                                    app_id: c.app_id,
                                    id: c.record,
                                    step: c.step,
                                )
                            }
                        }
                    }
                }
                // The change is the row: pressing it selects it, so the range's detail above
                // shows what it did and where.
                on activate: set(first, c.range.from).then(set(last, c.range.from))
            }
        }
        // A session can run for thousands of changes: the card holds a page and grows toward the
        // oldest of them.
        if s.changes_total > s.in_effect.count() {
            ui.Button("Show more", icon: .chevron_down) {
                on press: set(more, pages + 20)
            }
        }
    }
}

/// Where a session's change landed, as its row says it: the app, and the thing it changed.
fn changed_where(c: history.SessionChange) -> Text {
    let in_app = c.target.app_label ?? ""
    if in_app == "" {
        c.target.label
    } else if c.target.label == "" { in_app } else { "{in_app} · {c.target.label}" }
}

/// The workspace played back change by change: each frame the layout as it stood, and the person
/// who made it. A person steps through it, or plays it on the window's own clock.
view replay(h: history.Activity) {
    state at: Int = 0
    state playing: Bool = false
    if let r = h.replay {
        let total = r.frames.count()
        let running = playing and total > 0 and at < total - 1
        // Playing moves the window's place on its own clock, one checkpoint a second, and stops
        // at the newest one.
        if running {
            every 1s: set(at, (at + 1).min(total - 1))
        }
        if r.pending {
            ui.Text(
                "Replay is waiting for current edits to finish",
                role: .caption,
                secondary: true,
            )
        }
        if total > 0 {
            ui.Text(
                "Checkpoint {at.max(0).min(total - 1) + 1} of {total}",
                role: .caption,
                secondary: true,
            )
            row(gap: .related, align: .center) {
                ui.Button("Back", icon: .chevron_left) {
                    on press: set(at, (at - 1).max(0)).then(set(playing, false))
                }
                ui.Button(
                    if running { "Pause" } else { "Play" },
                    icon: if running { .pause } else { .play },
                ) {
                    on press: set(at, if playing { at } else if at >= total - 1 { 0 } else { at })
                        .then(set(playing, not playing))
                }
                ui.Button("Forward", icon: .chevron_right) {
                    on press: set(at, (at + 1).min(total - 1)).then(set(playing, false))
                }
            }
        } else if not r.pending {
            if r.unavailable > 0 {
                if let note = r.note {
                    ui.EmptyState(
                        "Workspace replay is unavailable for these checkpoints",
                        message: note,
                        icon: .history,
                    )
                } else {
                    ui.EmptyState(
                        "Workspace replay is unavailable for these checkpoints",
                        icon: .history,
                    )
                }
            } else {
                ui.Text("No replayable workspace checkpoints yet", role: .body)
            }
        }
        if total > 0 {
            let shown = r.frames.at(at.max(0).min(total - 1))
            if let frame = shown {
                ui.Section(frame.summary) {
                    ui.Text(
                        "{when(frame.at_ms)}, {frame.author.name}",
                        role: .caption,
                        secondary: true,
                    )
                    if frame.workspaces.is_empty() {
                        ui.Text(
                            "The workspace did not exist at this checkpoint",
                            role: .caption,
                            secondary: true,
                        )
                    }
                    for workspace in frame.workspaces {
                        ui.Section(workspace.name) {
                            row(gap: .related, wrap: true, align: .center) {
                                for group in workspace.groups {
                                    window_card(group)
                                }
                            }
                        }
                    }
                    for pointer in frame.pointers {
                        ui.Text("Present: {pointer.name}", role: .caption, secondary: true)
                    }
                    if let note = r.note {
                        ui.Text(note, role: .caption, secondary: true)
                    }
                }
            }
        }
    }
}

/// One window of the layout at a checkpoint: its front tab, the tabs behind it, and how much of
/// the strip it took.
view window_card(group: history.ReplayGroup) {
    let front = group.tabs.at(group.active) ?? ""
    let behind = group.tabs.filter(tab -> tab != front)
    let share = (group.width_permille + 9) / 10
    ui.Card(title: if front == "" { "Empty window" } else { front }) {
        if front != "" {
            if behind.is_empty() {
                ui.Text("One tab, {share}% of the width", role: .caption, secondary: true)
            } else {
                ui.Text("{join(behind)}, {share}% of the width", role: .caption, secondary: true)
            }
        }
    }
}

fn period_of(label: Text) -> Text {
    if label == "Today" {
        return "today"
    }
    if label == "7 days" {
        return "week"
    }
    if label == "30 days" {
        return "month"
    }
    "all-time"
}

/// The place a chrome verb names, as this window's places are keyed.
fn place_of(named: history.View) -> Text {
    match named {
        .history => "history",
        .replay => "replay",
        .whats-new => "whats-new",
    }
}

/// How the agent of a session is named: who it worked for, when it is not the reader.
fn started_by(a: history.Author) -> Text {
    if a.for_reader {
        std.say("Started by you")
    } else if let name = a.on_behalf_of {
        if name.trim().is_empty() { a.name } else { std.say("Started by {name}") }
    } else { a.name }
}

/// The agent session the selected changes are, when they are one: the session whose range they
/// are, the work group the history named them, or one whose action they belong to.
fn session_in(
    sessions: List<history.AgentSession>,
    group: Option<Text>,
    first: Text,
    last: Text,
) -> Option<history.AgentSession> {
    sessions.find(
        s -> s.range.from == first and s.range.to == last
            or group == .some(s.id)
            or s.in_effect.any(c -> c.range.from == first or c.range.to == last),
    )
}

/// The changes the page keeps of the person's filters: the scope, period and lens are the
/// session's to narrow (the page is read for them); the person and the searched words narrow the
/// page a person reads, which is a match over what it already holds.
fn matched(entries: List<history.Entry>, writer: Text, found: Text) -> List<history.Entry> {
    entries.filter(e -> writer == "" or e.author.name == writer).filter(e -> matches(e, found))
}

/// Whether `entry` says the searched words anywhere a person reads it.
fn matches(entry: history.Entry, found: Text) -> Bool {
    if found == "" {
        return true
    }
    let needle = found.lower()
    if entry.summary.lower().contains(needle) {
        return true
    }
    if entry.author.name.lower().contains(needle) {
        return true
    }
    (entry.app ?? "").lower().contains(needle)
}

fn join(parts: List<Text>) -> Text {
    let out = ""
    for part in parts {
        if out == "" {
            out = part
        } else {
            out = "{out} · {part}"
        }
    }
    out
}

/// When `at_ms` was: the day and the clock time.
fn when(at_ms: Int) -> Text {
    let at = moment(at_ms)
    "{at.day()}, {at.clock()}"
}

/// The instant a count of milliseconds names.
fn moment(at_ms: Int) -> Time {
    Time("1970-01-01T00:00:00Z") + Duration(at_ms / 1000)
}
