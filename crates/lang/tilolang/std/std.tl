/// The window frames every tilolang app gets: a list of resources, a document, a page and a
/// transcript, each the native one. They are written over the toolkit's components with the
/// same language any app has (lenses, resource metadata, enum cases and events that carry
/// their value), so an app that wants something else composes `ui.*` itself and loses nothing
/// native.
///
/// A window that lists resources: its bar's actions are the ones its owner's type declares,
/// which the host places there — the primary one as the Primary button, the rest behind the
/// bar's one menu; the primary's key is the entry's at the list's end, so the key and the
/// button lead the person there to type the item; each item is a row whose actions the host
/// puts on its context menu and its swipe, a destructive one undone rather than confirmed; a
/// command the app marked `frequent` a button of its own, never folded into a run; with no
/// items it says `empty` and the bar keeps the type's actions. The places in the sidebar are
/// the item type's filters that are on or off (`filters(T)`), after all of the items, each with
/// how many it holds where its role makes a count people read; the one chosen is the window's
/// query, as people type it in search (`is:done`), so everyone looking sees the same list; a
/// window starts on `place` (all of the items when empty). The place of all of the items is
/// called `everything`, "All" and the item type's plural noun unless the app says. A `sort` view
/// state is the bar's sort menu. Items whose type has a window of their own open in the next
/// column; the list follows which one is open.
view list<R, T, S>(
    of: R,
    items: List<T>,
    sort: Option<State<S>> = .none,
    place: Text = "",
    everything: Text = phrase("tilolang-all", kind: meta(T).plural),
    empty: Text = phrase("tilolang-empty"),
    entry: Option<(Text) -> Action> = .none,
    entry_placeholder: Text = phrase("tilolang-add"),
    row: (T) -> View,
) {
    window query: Text = place
    let kind = meta(T)
    let places = filters(T).filter(f -> f.toggle)
    let shown = items.matching(query)
    let chosen = places.find(p -> p.query == query)
    let title = chosen?.label ?? if places.is_empty() { meta(of).title } else { everything }
    ui.NavShell(
        content_kind: if kind.opens { .document } else { .list },
        sidebar_title: if places.is_empty() { .none } else { .some(meta(of).title) },
        overview_title: if kind.opens { .some(title) } else { .none },
        overview_count: if kind.opens { .some(shown.len()) } else { .none },
        title: if kind.opens { .none } else { .some(title) },
        count: if kind.opens { .none } else { .some(shown.len()) },
    ) {
        sidebar: {
            if not places.is_empty() {
                ui.NavList(selected: if query == "" { "all" } else { query }) {
                    ui.NavItem("all", everything, icon: kind.icon) {
                        on select: set(query, "")
                    }
                    for p in places {
                        ui.NavItem(
                            p.query,
                            p.label,
                            icon: p.icon,
                            count: if p.counts {
                                .some(items.matching(p.query).len())
                            } else { .none },
                        ) {
                            on select: set(query, p.query)
                        }
                    }
                }
            }
        }
        overview_actions: {
            if kind.opens {
                std.header(sort: sort)
            }
        }
        actions: {
            if not kind.opens {
                std.header(sort: sort)
            }
        }
        overview: {
            if kind.opens {
                std.rows(
                    shown,
                    empty,
                    kind.icon,
                    role: chosen?.role ?? "",
                    place: query,
                    entry: entry,
                    entry_placeholder: entry_placeholder,
                ) { (x, under) ->
                    row(x)
                }
            }
        }
        content: {
            if kind.opens {
                ui.EmptyState(phrase("tilolang-none-selected", kind: kind.noun), icon: kind.icon)
            } else {
                std.rows(
                    shown,
                    empty,
                    kind.icon,
                    role: chosen?.role ?? "",
                    place: query,
                    entry: entry,
                    entry_placeholder: entry_placeholder,
                ) { (x, under) ->
                    row(x)
                }
            }
        }
    }
}

/// The items of a list as the rows of a column, or what it says while it has none. Give
/// `group` and the list stands under a heading for each run of items it names, the heading held
/// at the top while the person scrolls it (each item's day, written the platform's way); the
/// items that name none stand under a heading of their own, never under the last group that
/// names one; give `closed` and the items it is true of stand last, under `closed_title`,
/// folded away until the person opens them. Give `parent` and the items stand as a tree: each
/// indented under the item it is a subtask of, with a disclosure that folds its subtasks away.
///
/// Give `role` and `place`, the place a list of items is of, and its role says how the list
/// reads instead of `group`, `closed` and `all_done`: the agenda and what is due later stand
/// under the days they are due on, soonest first, a day gone by as overdue; the logbook stands
/// under the days it was finished on, newest first; an inbox lists its items plainly, for there
/// is nothing about them to sort by yet. A place does not fold what is done with away: it holds
/// only what belongs in it.
///
/// `row` draws an item with the item it stands under (`.none` at the top), so a child leaves
/// out what its parent already says — the day they are both due, the project they both are of.
view rows<T>(
    items: List<T>,
    empty: Text,
    icon: Option<Text>,
    group: Option<(T) -> Option<Date>> = .none,
    closed: Option<(T) -> Bool> = .none,
    closed_title: Text = phrase("tilolang-done"),
    role: Text = "",
    place: Text = "",
    parent: Option<(T) -> Option<T>> = .none,
    parent_title: Option<(T) -> Option<Text>> = .none,
    progress: Option<(T) -> Option<Text>> = .none,
    all_done: Bool = false,
    entry: Option<(Text) -> Action> = .none,
    entry_placeholder: Text = phrase("tilolang-add"),
    row: (T, Option<T>) -> View,
) {
    // A list that is no place's is grouped by the caller, and folds what it leaves done.
    let all_done = all_done and role == "" and not items.is_empty()
    // The entry at the list's end is the list's own way to add: the host gives it the window's
    // primary action's key, so the key and the bar's button lead the person there, and what is
    // typed is what the list gains.
    if let commit = entry {
        ui.Docked {
            foot: {
                stack {
                    // Everything here is done with: the rows fold under the entry that adds
                    // the next one.
                    if all_done {
                        rows_list(
                            items,
                            icon,
                            group,
                            closed,
                            closed_title,
                            role,
                            place,
                            parent,
                            parent_title,
                            progress,
                        ) { (x, under) ->
                            row(x, under)
                        }
                    }
                    std.add_entry(entry_placeholder, commit)
                }
            }
            if all_done {
                ui.EmptyState(phrase("tilolang-all-done"), icon: icon, inline: true)
            } else if items.is_empty() {
                ui.EmptyState(empty, icon: icon)
            } else {
                rows_list(
                    items,
                    icon,
                    group,
                    closed,
                    closed_title,
                    role,
                    place,
                    parent,
                    parent_title,
                    progress,
                ) { (x, under) ->
                    row(x, under)
                }
            }
        }
    } else if items.is_empty() {
        ui.EmptyState(empty, icon: icon)
    } else if all_done {
        // Everything here is done with: say so calmly, over the foot that folds it away.
        ui.Docked {
            foot: {
                rows_list(
                    items,
                    icon,
                    group,
                    closed,
                    closed_title,
                    role,
                    place,
                    parent,
                    parent_title,
                    progress,
                ) { (x, under) ->
                    row(x, under)
                }
            }
            ui.EmptyState(phrase("tilolang-all-done"), icon: icon, inline: true)
        }
    } else {
        rows_list(
            items,
            icon,
            group,
            closed,
            closed_title,
            role,
            place,
            parent,
            parent_title,
            progress,
        ) { (x, under) ->
            row(x, under)
        }
    }
}

/// The entry at a list's end: what is typed and Entered there — or left there with — makes an
/// item of it, through `commit`; nothing typed makes nothing, and Escape leaves none. The host
/// gives it the window's primary action's key, so the key and the bar's button come here.
view add_entry(placeholder: Text, commit: (Text) -> Action) {
    ui.TextInput(placeholder: placeholder, entry: true) {
        on submit(title: Text): commit(title)
    }
}

/// The items of a list as the rows of a column, with `rows`' grouping, nesting and folded tail.
/// `role` and `place` are the place the list is of, when it is one of a place's: what its role
/// groups the list by, and the place's key (`is:today`), which reads the day each item stands
/// at in it.
///
/// `row` draws an item with the item it stands under, as `rows` does.
view rows_list<T>(
    items: List<T>,
    icon: Option<Text>,
    group: Option<(T) -> Option<Date>>,
    closed: Option<(T) -> Bool>,
    closed_title: Text,
    role: Text,
    place: Text,
    parent: Option<(T) -> Option<T>>,
    parent_title: Option<(T) -> Option<Text>>,
    progress: Option<(T) -> Option<Text>>,
    row: (T, Option<T>) -> View,
) {
    // An item that names no day stands under a heading of its own, in the order the caller
    // gave, rather than under the last group's heading. A place lists by its role's days, and
    // its role's order is the order they come in, so its list is not sorted here.
    let by_role = role != "" and role != "inbox"
    let grouped: Bool = if let g = group { true } else { false }
    // A place lists what it holds under no other heading: an item it holds that stands at no
    // day stands at none, not under a heading of "No date".
    let ungrouped: Option<Text> = if role != "" {
        .none
    } else if grouped { .some(phrase("tilolang-no-date")) } else { .none }
    // The items done with stand last, under the one heading their group folds: a sort is
    // stable, so the order the caller gave holds among them and among the rest.
    let shown = if role != "" {
        items
    } else if let c = closed { items.sort_by(x -> c(x)) } else { items }
    let nested = if let p = parent { true } else { false }
    ui.VirtualList(
        selection_follows_focus: true,
        in_column: true,
        tail_title: .some(closed_title),
        tree: nested,
        ungrouped_title: ungrouped,
    ) {
        on select(x: T): open(x, front: false)
        on activate(x: T): open(x)
        for x in shown {
            let heading: Option<Date> = if role == "inbox" {
                .none
            } else if by_role { placed_day(x, place) } else if let g = group { g(x) } else { .none }
            let tail = if role != "" { false } else if let c = closed { c(x) } else { false }
            // The row just over this one in its group, as the list shows them: the item before
            // it when that one has the same heading (the items naming none stand together
            // under the list's own) and is on this side of the fold. That is the place the
            // actions offered on the row get, and none for the first of a group. The list's
            // rows are the host's, so a row reaching one back costs one step, however far
            // along the list it stands.
            let above: Option<T> = if let p = shown.previous(x) {
                let same_heading = if role == "inbox" {
                    true
                } else if by_role {
                    placed_day(p, place) == heading
                } else if let g = group { g(p) == heading } else { true }
                let same_side = if let c = closed { c(p) == tail } else { true }
                if same_heading and same_side { .some(p) } else { .none }
            } else { .none }
            let under: Option<T> = if let p = parent { p(x) } else { .none }
            let belongs: Option<Text> = if let t = parent_title { t(x) } else { .none }
            let count: Option<Text> = if let c = progress { c(x) } else { .none }
            ui.ListItem(
                item: x,
                presence: lenses(x).find(l -> not l.mine)?.who,
                group: heading,
                group_done: role == "logbook",
                tail: tail,
                above: above,
                parent: under,
                parent_title: belongs,
                progress: count,
            ) {
                row(x, under)
            }
        }
    }
}

/// Items as an outline, each item over its own to any depth, opened and closed where a person
/// puts them, its row the one `row` draws. The outline is named `name` for a reader; the name
/// is the page's, not a row over the items, so the page states it once. A move — a drag, `Tab`
/// and `Shift-Tab`, the accessibility move actions — puts the item in the place named: the end
/// of the top level, the end of the items of the one it lands under, or where the one it lands
/// before stands. One step, so two people moving items at once never leave one in two places.
/// A row's checkbox checks its item off, or opens it again. Give `progress` and a row with
/// items under it says how far its branch got ("1/3"), as the list of the same items says it.
/// An item whose parent is not among `items` stands at the top, so a deleted parent never hides
/// what was inside it. `row` draws an item with the item it stands under (`.none` at the top),
/// so a child leaves out what its parent already says.
///
/// A type whose items nest gets the actions the outline moves and checks them with from its
/// `(parent)` and `(done)` fields, so the page names none of them: `std.move` and `std.check`
/// reach the type's own. A type without them has an outline that moves and checks nothing.
view outline<T>(
    name: Text,
    items: List<T>,
    parent: (T) -> Option<T>,
    label: (T) -> Text,
    empty: Text,
    progress: Option<(T) -> Option<Text>> = .none,
    icon: Option<Text> = .none,
    fill: Bool = true,
    row: (T, Option<T>) -> View,
) {
    if items.is_empty() {
        ui.EmptyState(empty, icon: icon)
    } else {
        ui.TreeView(
            name: .some(name),
            fill: fill,
            reorder: true,
            tab_indents: true,
            hide_root: true,
        ) {
            on drop(moved: T): std.move(moved, .none, .none)
            for x in items {
                // The node just above this one under the same parent, as the outline shows
                // them: the place the actions offered on it get. None for a first child.
                let above = items
                    .take(items.index_of(x) ?? 0)
                    .filter(y -> parent(y) == parent(x))
                    .last()
                let count: Option<Text> = if let c = progress { c(x) } else { .none }
                ui.TreeViewNode(x, label(x), parent: parent(x), above: above, progress: count) {
                    on drop(moved: T): std.move(moved, .none, .some(x))
                    on into(moved: T): std.move(moved, .some(x), .none)
                    on check: std.check(x)
                    row: {
                        row(x, parent(x))
                    }
                }
            }
        }
    }
}

/// A window's own controls, before the actions the host puts in the bar: the sort menu, and
/// `extra`, a page's controls. The window's actions come from what its type declares, and the
/// host places them after these: its primary action as the labelled Primary button, a command
/// the app marked `frequent` as a button of its own, and all the rest behind the bar's one
/// menu, so every action stays reachable at every width without the view having read them.
view header<S>(sort: Option<State<S>> = .none, extra: View) {
    if let s = sort {
        ui.MenuButton(phrase("tilolang-sort"), icon: .arrow_up_down, icon_only: true) {
            for c in cases(S) {
                ui.MenuItem(c.label(), checked: c == s) {
                    on choose: set(s, c)
                }
            }
        }
    }
    extra
}

/// A window that is a document of `of`: its content at the readable measure. A document edits
/// its title as its large title, and the host puts what can be done with it behind the "…"
/// beside that title.
view document<R>(of: R, content: View) {
    ui.NavShell(content_kind: .document, title: meta(of).title) {
        content: {
            ui.Scroll {
                page {
                    stack(gap: .section) {
                        content
                    }
                }
            }
        }
    }
}

/// What follows a document's heading, one row lead in: the body starts under the text the
/// heading's control leads to, so a task's notes, subtasks and facts stand in the column its
/// title's text does, and the box that says the item is done stands in the gutter to their
/// left. A document whose heading opens with its title alone has no lead to make.
view body(content: View) {
    stack(lead: 1) {
        content
    }
}

/// A window that is a page of `of`: its content at the readable measure, or as a `canvas` at
/// the window's full width, under `header`, the page's own controls, and the actions the host
/// puts in the bar with them. A `fill` page is exactly the window's height and scrolls inside
/// its own parts, which is what a board's lanes need; a page that grows with what it holds (a
/// grid of cards, a gallery) leaves it off. A page with nothing on it (`empty`) says so.
view page<R>(
    of: R,
    canvas: Bool = false,
    fill: Bool = false,
    empty: Bool = false,
    header: View,
    content: View,
) {
    ui.NavShell(content_kind: .page, title: meta(of).title) {
        actions: {
            std.header {
                header
            }
        }
        content: {
            if empty {
                ui.EmptyState(phrase("tilolang-empty"), icon: meta(of).icon)
            } else {
                ui.Scroll {
                    if canvas {
                        page(full, fill: fill) {
                            stack(gap: .section, fill: fill) {
                                content
                            }
                        }
                    } else {
                        page {
                            stack(gap: .section) {
                                content
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A window that is a conversation of `of`: its items in order, the latest at the foot, each a
/// `row`, built as they show, above the composer, which stays at the foot. Who wrote each item
/// and when come from the log (`created`), and the list draws them as every conversation does:
/// the author's avatar and name over each run of their messages, the time beside the name, and
/// a divider where the day changes; the `row` then shows only what the item says. Each item's
/// menu is whatever menu the `row` attaches (`ui.ContextMenu`), and after it the host puts the
/// actions the item's type declares, so every action stays reachable; the row's hover toolbar
/// and swipe are its own.
///
/// `unread` names the first item the person has not seen (`first_unseen(of)`, or `.none`): the
/// list draws the "New messages" divider over it, keeps the reader where they are as items
/// arrive, and offers the way back to it, or to what is new, while they read history.
view transcript<R, T>(
    of: R,
    items: List<T>,
    unread: Option<T> = .none,
    composer: View,
    row: (T) -> View,
) {
    ui.NavShell(content_kind: .page, title: meta(of).title) {
        content: {
            ui.Docked {
                foot: {
                    stack {
                        composer
                    }
                }
                ui.VirtualList(transcript: true, unread: unread) {
                    for m in items {
                        let made = created(m)
                        ui.ListItem(item: m, author: made?.who, time: made?.at) {
                            row(m)
                        }
                    }
                }
            }
        }
    }
}

/// A message quoted where another answers it: who wrote it and the start of what it says;
/// activating it opens it.
view quote<T>(of: T) {
    ui.Quote(meta(of).title, author: created(of)?.who)
}

/// Over a composer while it answers `to`: the message being answered, quoted, and a close
/// button that runs `cancel` (stop replying).
view reply_banner<T>(to: T, cancel: Action) {
    ui.Quote(meta(to).title, author: created(to)?.who, caption: phrase("tilolang-replying-to")) {
        on dismiss: cancel
    }
}

/// Under a message that started a thread: how many replies it has, opening the thread.
view thread_chip<T>(thread: T, count: Int) {
    ui.Chip(phrase("tilolang-thread-replies", count: count), icon: .message_square) {
        on press: open(thread)
    }
}

/// What happened to `of`, newest first, as the platform folds its history (`steps`): each step,
/// consecutive changes by one person or by one task run, is an entry saying who made it, what
/// they did and how long ago. What they did is the sentence its last change says
/// (`version_sentence`), worked out from the fields the change wrote, the person looking as
/// "You". A step of one change shows each field it changed, what it held before beside what it
/// holds now, each as the field's type shows it; a step of several opens to each of them. A step
/// the person looking made offers Undo for it, and a run's step Undo for the whole run; each
/// change of theirs in a longer step is undone on its own.
view history<R>(of: R) {
    lens open: Text = ""
    let steps = steps(of)
    if not steps.is_empty() {
        section(phrase("tilolang-history")) {
            for s in steps {
                let key = "{s.at}/{s.who}"
                let single = s.versions.len() == 1
                // What was done: the sentence its last change says, or, for a step of a run,
                // who ran it and what it ran.
                stack(gap: .related) {
                    row(gap: .related, align: .center) {
                        avatar(s.who)
                        stack(gap: .tight) {
                            if let run = s.task {
                                if let v = s.versions.last() {
                                    ui.Text(v.actor, role: .label)
                                }
                                ui.Text(run.title, role: .caption, secondary: true)
                            } else if let v = s.versions.last() {
                                std.version_sentence(v)
                            }
                            if not single {
                                ui.Text(
                                    phrase("tilolang-history-changes", count: s.versions.len()),
                                    role: .caption,
                                    secondary: true,
                                )
                            }
                        }
                        spacer()
                        ui.Time(s.at, role: .caption, secondary: true)
                        if let run = s.task {
                            ui.Button(phrase("tilolang-history-undo-run")) {
                                on press: undo(run)
                            }
                        } else if single and s.mine {
                            if let v = s.versions.last() {
                                ui.Button(phrase("tilolang-undo")) {
                                    on press: undo(v)
                                }
                            }
                        }
                    }
                    if single {
                        if let v = s.versions.last() {
                            std.version_changes(v)
                        }
                    } else if open == key {
                        ui.Button(phrase("tilolang-history-hide")) {
                            on press: set(open, "")
                        }
                        for v in s.versions {
                            stack(gap: .tight) {
                                row(gap: .related, align: .center) {
                                    std.version_sentence(v)
                                    spacer()
                                    ui.Time(v.at, role: .caption, secondary: true)
                                    if v.mine {
                                        ui.Button(phrase("tilolang-undo")) {
                                            on press: undo(v)
                                        }
                                    }
                                }
                                std.version_changes(v)
                            }
                        }
                    } else {
                        ui.Button(phrase("tilolang-history-show")) {
                            on press: set(open, key)
                        }
                    }
                }
            }
        }
    }
}

/// A version in one sentence: who made it, then what it did, joined by what the platform keeps
/// beside each change (`said`, `shows`). The words come from the fields the change wrote and
/// what they held before and after ("moved this to", then the day as a day), or from what the
/// action declares (`did:`); a version that says neither says what its action does.
view version_sentence(v: host.Version) {
    if v.did != "" {
        ui.Text("{v.actor} {v.did}", role: .label)
    } else if not v.changes.is_empty() {
        row(gap: .tight, wrap: true) {
            for c in v.changes {
                row(gap: .tight) {
                    ui.Text(c.said, role: .label)
                    if c.shows == "after" {
                        c.after
                    } else if c.shows == "before" {
                        c.before
                    }
                }
            }
        }
    } else {
        ui.Text(v.actor, role: .label)
        ui.Text(
            if v.description == "" { v.what } else { v.description },
            role: .caption,
            secondary: true,
        )
    }
}

/// What a version did to each field it changed: the field's name, what it held before and what
/// it holds after, each as the field's type shows it; or that it made or removed the resource.
view version_changes(v: host.Version) {
    if v.kind == "created" {
        ui.Text(phrase("tilolang-history-made"), role: .caption, secondary: true)
    } else if v.kind == "removed" {
        ui.Text(phrase("tilolang-history-removed"), role: .caption, secondary: true)
    } else if not v.changes.is_empty() {
        grid(columns: 3, gap: .related) {
            for c in v.changes {
                ui.Text(c.label, role: .caption, secondary: true)
                c.before
                c.after
            }
        }
    }
}

/// The places a collection app's window offers in its sidebar, derived from the schema of the
/// items and of the root that holds them: one for each of the item type's filters that is on or
/// off, and a group for each type the items name a resource of while the root holds a
/// collection of it (a task's projects, under "Projects", with an add that makes another).
/// Choosing a place sets `place`, the window's query: choosing one of the root's resources shows
/// the items that name it where they are, in the same window (`places(S, in: of)` is what it
/// draws).
view sidebar(side: host.Sidebar, place: State<Text>) {
    ui.NavList(selected: place) {
        for p in side.places {
            if p.group == .none {
                ui.NavItem(
                    p.key,
                    p.label,
                    icon: p.icon,
                    count: if p.counts { .some(p.count) } else { .none },
                ) {
                    on select: set(place, p.key)
                }
            }
        }
        for g in side.groups {
            ui.NavSection(g.title, add: g.add) {
                for p in side.places.filter(x -> x.group == .some(g.title)) {
                    ui.NavItem(
                        p.key,
                        p.label,
                        icon: p.icon,
                        count: if p.counts { .some(p.count) } else { .none },
                    ) {
                        on select: set(place, p.key)
                    }
                }
            }
        }
    }
}
