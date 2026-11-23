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
                if place == "whats-new" {
                    whats_new(h, on_see_history: .some(set(place, "history")))
                } else if place == "history" {
                    changes(h, found)
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
view whats_new(h: history.Activity, on_see_history: Option<Action> = .none) {
    if let s = h.summary {
        if s.pending {
            ui.Text("Loading changes", role: .caption, secondary: true)
        }
        if s.away {
            ui.Text("While you were away", role: .heading)
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
                if let see = on_see_history {
                    ui.Button("See history") {
                        on press: see
                    }
                }
            }
        }
        for brief in s.briefs {
            brief_card(brief, on_see_history: on_see_history)
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

/// One work session: what it achieved, who did it, and the actions it took.
view brief_card(brief: history.Brief, on_see_history: Option<Action> = .none) {
    ui.Card(title: brief.title) {
        ui.Text(brief.detail, role: .caption, secondary: true)
        for step in brief.actions {
            row(gap: .related, align: .center) {
                ui.Text(step.author.name, role: .caption)
                ui.Text(step.summary, role: .body)
            }
        }
        if not brief.entries.is_empty() {
            if let see = on_see_history {
                ui.Button("Open in History") {
                    on press: see
                }
            }
        }
    }
}

/// Every change, narrowed by what the person chose, with the range they selected shown beside
/// it: what it did, what can be done with it, and what changed either side. The filters and the
/// presses that change them live here, in the view that declares what they change.
view changes(h: history.Activity, found: Text) {
    state scope: Text = "this-workspace"
    state period: Text = "all-time"
    state writer: Text = ""
    state lens: Text = ""
    state first: Text = ""
    state last: Text = ""
    // The page is the session's reading of exactly the filters this window shows, keyed by
    // them: another window on another period reads its own page.
    let key_scope = if scope == "all-workspaces" { "all" } else { "this" }
    let key_lens = if lens == "" { "-" } else { lens }
    let page = resource(
        "workspace.activity/feed/{key_scope}/{period}/{key_lens}",
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
        row(gap: .section, align: .stretch) {
            stack(fill, gap: .related) {
                if f.entries.is_empty() {
                    ui.Text("No changes here", role: .body)
                }
                let changes = matched(f.entries, writer, found)
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

/// The range the person selected: what it did, what can be done with it, and what changed either
/// side of it. Its detail is the session's reading of that one range, so two windows showing two
/// ranges each read their own.
view detail_card(first: Text, last: Text) {
    let shown = resource("workspace.activity/range/{first}-{last}", of: history.RangeDetail)
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
            if let merged = d.offers.find(o -> o.verb == "merge-groups") {
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
            if not d.changes.is_empty() {
                ui.Section("What changed") {
                    for change in d.changes {
                        ui.Text("{change.field}: {change.before} → {change.after}", role: .body)
                    }
                }
            }
            if let comparison = d.comparison {
                ui.Section("Compare") {
                    ui.Text(comparison, role: .body)
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
