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
                    changes(h, found, scope, period, writer, lens, first, last)
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
        if s.away {
            ui.Text("While you were away", role: .heading)
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
                    let target = resume.target
                    ui.Button("Resume") {
                        on press: history.open_change(
                            history.history,
                            id: target.id,
                            kind: target.kind,
                            label: target.label,
                        )
                    }
                }
            }
        }
        if s.briefs.is_empty() {
            if not s.pending {
                ui.Text("You're caught up", role: .body)
                ui.Button("See history") {
                    on press: set(place, "history")
                }
            }
        }
        for brief in s.briefs {
            brief_card(brief, place, first, last)
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
                    row(gap: .related, align: .center) {
                        ui.Text(source.label, role: .body)
                        ui.Text(source.text, role: .caption, secondary: true)
                        if let status = source.status {
                            ui.Text(status, role: .caption, secondary: true)
                        }
                        if source.refreshable {
                            ui.Button("Refresh source", icon: .refresh) {
                                on press: history.source(
                                    history.history,
                                    key: source.key,
                                    command: "refresh",
                                )
                            }
                        }
                        ui.Button("Remove morning source", icon: .x) {
                            on press: history.source(
                                history.history,
                                key: source.key,
                                command: "remove",
                            )
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
        for step in brief.actions {
            row(gap: .related, align: .center) {
                ui.Text(step.author.name, role: .caption)
                ui.Text(step.summary, role: .body)
            }
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
        for label in ["All time", "Today", "7 days", "30 days"] {
            ui.Button(label, toggled: period == period_of(label)) {
                on press: set(period, period_of(label))
            }
        }
    }
    if let f = feed {
        row(gap: .related, wrap: true, align: .center) {
            ui.Button("Everyone", toggled: writer == "") {
                on press: set(writer, "")
            }
            for person in f.people {
                ui.Button(person, toggled: writer == person) {
                    on press: set(writer, person)
                }
            }
            spacer
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
                for entry in changes {
                    ui.Card(title: entry.summary) {
                        row(gap: .related, align: .center) {
                            ui.Text(entry.author.name, role: .caption)
                            if entry.id == first or entry.id == last {
                                ui.Badge("Selected", tone: .info)
                            }
                            if let made_in = entry.made_in {
                                ui.Badge(made_in, tone: .neutral)
                            }
                            if let changed = entry.resource {
                                ui.Text(changed, role: .caption, secondary: true)
                            }
                            for refusal in entry.refused {
                                ui.Text(
                                    "Not on {refusal.label}: {refusal.reason}",
                                    role: .caption,
                                    secondary: true,
                                )
                            }
                        }
                        ui.Button("Select through here") {
                            on press: if first == "" {
                                set(first, entry.id).then(set(last, entry.id))
                            } else { set(last, entry.id) }
                        }
                    }
                }
                // Older changes are reachable: the page grows a page at a time toward the
                // oldest the history holds.
                if f.total > f.entries.count() {
                    ui.Button("Show more", icon: .chevron_down) {
                        on press: set(limit, page_limit + 200)
                    }
                }
            }
            // The range the person selected, beside the list as the window's inspector: its
            // detail is the session's reading of that one range, so another window showing
            // another range reads its own.
            if first != "" {
                stack(gap: .related) {
                    detail_card(first, last)
                }
            }
        }
    }
}

/// The range the person selected: what it did, what can be done with it, what changed either
/// side of it, and — when the changes are an agent's work — the session that asked for it and
/// each record's own undo. Its detail is the session's reading of that one range, so two
/// windows showing two ranges each read their own.
view detail_card(first: State<Text>, last: State<Text>) {
    // The comparison is read only while the person asks for it: what changed either side of the
    // range, and the fields it changed.
    state compare: Bool = false
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
                            on press: history.open_change(
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
            if let undo = d.offers.find(o -> o.verb == "undo") {
                ui.Button(undo.label) {
                    on press: history.undo_change(history.history, from: first, to: last)
                }
            }
            if let restore = d.offers.find(o -> o.verb == "restore") {
                ui.Button(restore.label) {
                    on press: history.restore(history.history, from: first, to: last)
                }
            }
            if let split = d.offers.find(o -> o.verb == "split") {
                ui.Button(split.label) {
                    on press: history.split(history.history, from: first, to: last)
                }
            }
            // The offer names the model's verb (`merge`), not this package's action
            // (`merge-groups`), as undo's offer says `undo` and not `undo-change`.
            if let merged = d.offers.find(o -> o.verb == "merge") {
                ui.Button(merged.label) {
                    on press: history.merge_groups(history.history, from: first, to: last)
                }
            }
            if let session = d.offers.find(o -> o.verb == "undo-session") {
                ui.Button(session.label) {
                    on press: history.undo_session(history.history, from: first, to: last)
                }
            }
            if let source = d.offers.find(o -> o.verb == "choose-source") {
                ui.Button(source.label) {
                    on press: history.choose_source(history.history, from: first, to: last)
                }
            }
            // Comparing asks the session for the workspace either side of the range: the offer
            // names that read, which the window takes when the person turns it on.
            if let offer = d.offers.find(o -> o.verb == "detail") {
                ui.Button(offer.label, toggled: compare) {
                    on press: set(compare, not compare)
                }
            }
        }
        if compare {
            compare_card(first, last)
        }
        if let s = session_in(history.history.sessions, d.group, first, last) {
            session_card(s)
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
            ui.Section("What changed") {
                for change in c.changes {
                    ui.Text("{change.field}: {change.before} → {change.after}", role: .body)
                }
            }
        } else if let comparison = c.comparison {
            ui.Section("Compare") {
                ui.Text(comparison, role: .body)
            }
        } else {
            ui.Text("No content differences in this selection", role: .caption, secondary: true)
        }
    }
}

/// The agent session the selected changes are: why the work was asked for, how it was decided,
/// whether it can be undone, and each record it changed with its own undo.
view session_card(s: history.AgentSession) {
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
    if not s.actions.is_empty() {
        ui.Section("Affected resources") {
            for a in s.actions {
                row(gap: .related, align: .center) {
                    ui.Text(
                        if a.target.label != "" { a.target.label } else { a.place },
                        role: .body,
                    )
                    ui.Text(a.summary, role: .caption, secondary: true)
                    if let label = a.undo_label {
                        ui.Button(label, role: .plain) {
                            on press: if a.undo_verb == .some("undo-entry") {
                                history.undo_record(
                                    history.history,
                                    range: history.Span(from: a.range.from, to: a.range.to),
                                    app_id: a.app_id,
                                    id: a.record,
                                )
                            } else {
                                history.undo_change(
                                    history.history,
                                    from: a.range.from,
                                    to: a.range.to,
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The workspace played back change by change: each frame the layout as it stood, and the person
/// who made it.
view replay(h: history.Activity) {
    state at: Int = 0
    if let r = h.replay {
        if r.pending {
            ui.Text(
                "Replay is waiting for current edits to finish",
                role: .caption,
                secondary: true,
            )
        }
        if r.frames.is_empty() {
            if not r.pending {
                ui.Text("No replayable workspace checkpoints yet", role: .body)
            }
        }
        let total = r.frames.count()
        if total > 0 {
            let shown = r.frames.at(at.max(0).min(total - 1))
            if let frame = shown {
                ui.Text("Checkpoint {at + 1} of {total}", role: .caption, secondary: true)
                row(gap: .related, align: .center) {
                    ui.Button("Back", icon: .chevron_left) {
                        on press: set(at, (at - 1).max(0))
                    }
                    ui.Button("Forward", icon: .chevron_right) {
                        on press: set(at, (at + 1).min(total - 1))
                    }
                }
                ui.Section(frame.summary) {
                    ui.Text(frame.author.name, role: .caption, secondary: true)
                    for workspace in frame.workspaces {
                        ui.Card(title: workspace.name) {
                            row(gap: .related, wrap: true, align: .center) {
                                for group in workspace.groups {
                                    ui.Text(join(group.tabs), role: .caption)
                                }
                            }
                        }
                    }
                    for pointer in frame.pointers {
                        ui.Text("Present: {pointer.name}", role: .caption, secondary: true)
                    }
                }
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
fn place_of(named: Text) -> Text {
    if named == "history" {
        return "history"
    }
    if named == "replay" {
        return "replay"
    }
    "whats-new"
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
    sessions: Option<history.Sessions>,
    group: Option<Text>,
    first: Text,
    last: Text,
) -> Option<history.AgentSession> {
    if let all = sessions {
        all.sessions.find(
            s -> s.range.from == first and s.range.to == last
                or group == .some(s.id)
                or s.actions.any(a -> a.range.from == first or a.range.to == last),
        )
    } else { .none }
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
    (entry.made_in ?? "").lower().contains(needle)
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
