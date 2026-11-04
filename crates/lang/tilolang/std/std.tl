/// The window frames every tilolang app gets: a list of resources, a document, a page and a
/// transcript, each the native one. They are written over the toolkit's components with the
/// same language any app has (action reflection, lenses, resource metadata, enum cases and
/// events that carry their value), so an app that wants something else composes `ui.*`
/// itself and loses nothing native.
///
/// A window that lists resources: the header holds its owner's actions, the primary one as
/// the Primary button; each item is a row whose actions are on its context menu and its swipe,
/// a destructive one undone rather than confirmed; a command the app marked `frequent` a row of
/// its own, as it is in the header, never folded into a run; with no items it says `empty` and
/// offers the primary action. The places in the sidebar are the item type's filters that are on or
/// off (`filters(T)`), after all of the items, each with how many it holds; the one chosen is
/// the window's query, as people type it in search (`is:done`), so everyone looking sees the
/// same list; a window starts on `place` (all of the items when empty). The place of all of the
/// items is called `everything`, "All" and the item type's plural noun unless the app says. A `sort` view state is
/// the header's sort menu. Items whose type has a window of
/// their own open in the next column; the list follows which one is open.
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
    let offered = actions(of)
    let places = filters(T).filter(f -> f.toggle)
    let shown = items.matching(query)
    let title = places.find(p -> p.query == query)?.label
        ?? if places.is_empty() { meta(of).title } else { everything }
    // A view with an entry adds there: its primary action stands in the header's menu, not as
    // the header's button, so the list keeps the one way to make an item.
    let adding = if let commit = entry { true } else { false }
    // An empty list offers the primary action while nothing was made yet: on the place a
    // window starts on, or on all of the items. One another filter emptied says only that.
    let primary = if query == "" or query == place {
        offered.find(a -> a.primary and a.available)
    } else { .none }
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
                    ui.NavItem("all", everything, icon: kind.icon, count: .some(items.len())) {
                        on select: set(query, "")
                    }
                    for p in places {
                        ui.NavItem(
                            p.query,
                            p.label,
                            icon: p.icon,
                            count: .some(items.matching(p.query).len()),
                        ) {
                            on select: set(query, p.query)
                        }
                    }
                }
            }
        }
        overview_actions: {
            if kind.opens {
                std.header(offered, sort: sort, entry: adding)
            }
        }
        actions: {
            if not kind.opens {
                std.header(offered, sort: sort, entry: adding)
            }
        }
        overview: {
            if kind.opens {
                std.rows(
                    shown,
                    empty,
                    primary,
                    kind.icon,
                    entry: entry,
                    entry_placeholder: entry_placeholder,
                ) { x ->
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
                    primary,
                    kind.icon,
                    entry: entry,
                    entry_placeholder: entry_placeholder,
                ) { x ->
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
view rows<T>(
    items: List<T>,
    empty: Text,
    primary: Option<host.Action>,
    icon: Option<Text>,
    group: Option<(T) -> Option<Date>> = .none,
    closed: Option<(T) -> Bool> = .none,
    closed_title: Text = phrase("tilolang-done"),
    parent: Option<(T) -> Option<T>> = .none,
    parent_title: Option<(T) -> Option<Text>> = .none,
    progress: Option<(T) -> Option<Text>> = .none,
    all_done: Bool = false,
    entry: Option<(Text) -> Action> = .none,
    entry_placeholder: Text = phrase("tilolang-add"),
    row: (T) -> View,
) {
    let all_done = all_done and not items.is_empty()
    // The entry at the list's end is its one way to add: the view's primary action keeps its
    // key there instead of making an item nobody typed, an empty list says nothing over it,
    // and what is typed is what the list gains.
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
                            parent,
                            parent_title,
                            progress,
                        ) { x ->
                            row(x)
                        }
                    }
                    std.add_entry(primary, entry_placeholder, commit)
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
                    parent,
                    parent_title,
                    progress,
                ) { x ->
                    row(x)
                }
            }
        }
    } else if items.is_empty() {
        if let a = primary {
            ui.EmptyState(empty, icon: icon, action: a.label) {
                on act: a.run
            }
        } else {
            ui.EmptyState(empty, icon: icon)
        }
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
                    parent,
                    parent_title,
                    progress,
                ) { x ->
                    row(x)
                }
            }
            ui.EmptyState(phrase("tilolang-all-done"), icon: icon, inline: true)
        }
    } else {
        rows_list(items, icon, group, closed, closed_title, parent, parent_title, progress) { x ->
            row(x)
        }
    }
}

/// The entry at a list's end: what is typed and Entered there — or left there with — makes an
/// item of it, through `commit`; nothing typed makes nothing, and Escape leaves none. Its
/// `primary` is the type's create action: its key puts the keyboard in this entry (the window's
/// primary surface presses it) instead of making an item nobody typed.
view add_entry(primary: Option<host.Action>, placeholder: Text, commit: (Text) -> Action) {
    let key: Option<Text> = if let a = primary { a.key } else { .none }
    ui.TextInput(placeholder: placeholder, entry: true, key: key) {
        on submit(title: Text): commit(title)
    }
}

/// The items of a list as the rows of a column, with `rows`' grouping, nesting and folded tail.
view rows_list<T>(
    items: List<T>,
    icon: Option<Text>,
    group: Option<(T) -> Option<Date>>,
    closed: Option<(T) -> Bool>,
    closed_title: Text,
    parent: Option<(T) -> Option<T>>,
    parent_title: Option<(T) -> Option<Text>>,
    progress: Option<(T) -> Option<Text>>,
    row: (T) -> View,
) {
    // An item that names no group stands under a heading of its own, in the order the
    // caller gave, rather than under the last group's heading.
    let ungrouped: Option<Text> = if let g = group {
        .some(phrase("tilolang-no-date"))
    } else { .none }
    // The items done with stand last, under the one heading their group folds: a sort is
    // stable, so the order the caller gave holds among them and among the rest.
    let shown = if let c = closed { items.sort_by(x -> c(x)) } else { items }
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
            let offered = actions(x)
            let heading: Option<Date> = if let g = group { g(x) } else { .none }
            let tail = if let c = closed { c(x) } else { false }
            let under: Option<T> = if let p = parent { p(x) } else { .none }
            let belongs: Option<Text> = if let t = parent_title { t(x) } else { .none }
            let count: Option<Text> = if let c = progress { c(x) } else { .none }
            ui.ListItem(
                item: x,
                presence: lenses(x).find(l -> not l.mine)?.who,
                group: heading,
                tail: tail,
                parent: under,
                parent_title: belongs,
                progress: count,
            ) {
                menu: {
                    for a in offered {
                        ui.MenuItem(
                            a.label,
                            icon: a.icon,
                            destructive: a.destructive,
                            key: a.key,
                            disabled: not a.available,
                            reason: if a.available { .none } else { .some(a.reason) },
                            group: if a.frequent { .none } else { a.group },
                        ) {
                            on choose: a.run
                        }
                    }
                }
                swipe: {
                    if let a = offered.find(a -> a.available and a.destructive) {
                        ui.SwipeAction(a.label, icon: a.icon, destructive: true) {
                            on swipe: a.run
                        }
                    } else if let a = offered.find(a -> a.available) {
                        ui.SwipeAction(a.label, icon: a.icon) {
                            on swipe: a.run
                        }
                    }
                }
                row(x)
            }
        }
    }
}

/// Items as an outline, each item over its own to any depth, opened and closed where a person
/// puts them, its row the one `row` draws. The outline is named `name` for a reader; the name
/// is the page's, not a row over the items, so the page states it once. A move — a drag, `Tab`
/// and `Shift-Tab`, the accessibility move actions — is reported with the item it lands under
/// and the item it lands before, and `place` puts it in the one place they name:
/// `(moved, .none, .none)` is the end of the top level, `(moved, .some(x), .none)` the end of
/// `x`'s own, and `(moved, .none, .some(x))` where `x` stands. One step, so two people moving
/// items at once never leave one in two places. A row's checkbox asks `toggled`. Give `progress`
/// and a row with items under it says how far its branch got ("1/3"), as the list of the same
/// items says it. An item whose parent is not among `items` stands at the top, so a deleted
/// parent never hides what was inside it.
view outline<T>(
    name: Text,
    items: List<T>,
    parent: (T) -> Option<T>,
    label: (T) -> Text,
    empty: Text,
    place: (T, Option<T>, Option<T>) -> Action,
    toggled: (T) -> Action,
    progress: Option<(T) -> Option<Text>> = .none,
    icon: Option<Text> = .none,
    fill: Bool = true,
    row: (T) -> View,
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
            on drop(moved: T): place(moved, .none, .none)
            for x in items {
                let offered = actions(x)
                let count: Option<Text> = if let c = progress { c(x) } else { .none }
                ui.TreeViewNode(x, label(x), parent: parent(x), progress: count) {
                    on drop(moved: T): place(moved, .none, .some(x))
                    on into(moved: T): place(moved, .some(x), .none)
                    on check: toggled(x)
                    menu: {
                        for a in offered {
                            ui.MenuItem(
                                a.label,
                                icon: a.icon,
                                destructive: a.destructive,
                                key: a.key,
                                disabled: not a.available,
                                reason: if a.available { .none } else { .some(a.reason) },
                                group: if a.frequent { .none } else { a.group },
                            ) {
                                on choose: a.run
                            }
                        }
                    }
                    row: {
                        row(x)
                    }
                }
            }
        }
    }
}

/// A header's controls, as native headers place them: the sort menu and search first, then
/// `extra`, then the window's actions — its primary action as the labelled Primary button, a
/// command the app marked `frequent` as a button of its own, and all the rest behind the
/// header's one menu, as a desktop bar holds them. The primary action comes first among
/// them so that a narrow window gives way with the menus, never with the one main thing to
/// do. A view with an `entry` at its list's end adds there, so its primary action is not a
/// button of the header: it stands first in the header's menu, and its key puts the keyboard in
/// the entry. The window's header lays them out: what does not fit gives way to where the system
/// lists the window's actions (the desktop bar's window menu), else behind the header's own
/// door, so every action stays reachable at every width.
view header<S>(
    offered: List<host.Action>,
    sort: Option<State<S>> = .none,
    entry: Bool = false,
    extra: View,
) {
    if let s = sort {
        ui.MenuButton(phrase("tilolang-sort"), icon: .arrow_up_down, icon_only: true) {
            for c in cases(S) {
                ui.MenuItem(c.label(), checked: c == s) {
                    on choose: set(s, c)
                }
            }
        }
    }
    ui.SearchButton()
    extra
    // The view with an entry at its list's end adds there: the primary action keeps its key on
    // that entry, so the header does not press it, and stays in the menu where its label shows.
    for a in offered {
        if a.primary and not entry {
            ui.Button(
                a.label,
                icon: a.icon,
                role: .primary,
                key: a.key,
                enabled: a.available,
                description: a.reason,
            ) {
                on press: a.run
            }
        }
    }
    for a in offered {
        if a.frequent and not a.primary and a.icon != .none {
            ui.Button(
                a.label,
                icon: a.icon,
                icon_only: true,
                toggled: a.on == .some(true),
                key: a.key,
                enabled: a.available,
                description: a.reason,
            ) {
                on press: a.run
            }
        } else if a.frequent and not a.primary {
            ui.Button(
                a.label,
                toggled: a.on == .some(true),
                key: a.key,
                enabled: a.available,
                description: a.reason,
            ) {
                on press: a.run
            }
        }
    }
    for a in offered {
        if (not a.primary or entry) and not a.frequent {
            ui.Button(
                a.label,
                icon: a.icon,
                role: if a.destructive { .destructive } else { .secondary },
                toggled: a.on == .some(true),
                key: a.key,
                in_menu: true,
                group: a.group,
                enabled: a.available,
                description: a.reason,
            ) {
                on press: a.run
            }
        }
    }
}

/// A window that is a document of `of`: its content at the readable measure, scrolling under a
/// header of `of`'s actions. A document edits its title as its large title.
view document<R>(of: R, content: View) {
    ui.NavShell(content_kind: .document, title: meta(of).title) {
        actions: {
            std.header(actions(of))
        }
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

/// A window that is a page of `of`: its content at the readable measure, or as a `canvas` at
/// the window's full width, scrolling under a header of `of`'s actions and `header`, the
/// page's own controls. A `fill` page is exactly the window's height and scrolls inside its
/// own parts, which is what a board's lanes need; a page that grows with what it holds (a grid
/// of cards, a gallery) leaves it off. A page with nothing on it (`empty`) says so and offers
/// its primary action, unless an `entry` in its parts adds instead.
view page<R>(
    of: R,
    canvas: Bool = false,
    fill: Bool = false,
    empty: Bool = false,
    entry: Bool = false,
    header: View,
    content: View,
) {
    ui.NavShell(content_kind: .page, title: meta(of).title) {
        actions: {
            std.header(actions(of), entry: entry) {
                header
            }
        }
        content: {
            if empty {
                // A page whose lanes or parts hold their own entry adds there, not here.
                let primary: Option<host.Action> = if entry {
                    .none
                } else {
                    actions(of).find(a -> a.primary and a.available)
                }
                if let a = primary {
                    ui.EmptyState(phrase("tilolang-empty"), icon: meta(of).icon, action: a.label) {
                        on act: a.run
                    }
                } else {
                    ui.EmptyState(phrase("tilolang-empty"), icon: meta(of).icon)
                }
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
/// menu offers its actions, after whatever menu the `row` attaches (`ui.ContextMenu`), so every
/// action stays reachable; the row's hover toolbar and swipe are its own.
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
        actions: {
            std.header(actions(of))
        }
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
                        let offered = actions(m)
                        ui.ListItem(item: m, author: made?.who, time: made?.at) {
                            menu: {
                                for a in offered {
                                    ui.MenuItem(
                                        a.label,
                                        icon: a.icon,
                                        destructive: a.destructive,
                                        key: a.key,
                                        disabled: not a.available,
                                        reason: if a.available { .none } else { .some(a.reason) },
                                        group: if a.frequent { .none } else { a.group },
                                    ) {
                                        on choose: a.run
                                    }
                                }
                            }
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
/// they did and how long ago. A step of one change shows each field it changed, what it held
/// before beside what it holds now, each as the field's type shows it; a step of several opens
/// to each of them. A step the person looking made offers Undo for it, and a run's step Undo
/// for the whole run; each change of theirs in a longer step is undone on its own.
view history<R>(of: R) {
    lens open: Text = ""
    let steps = steps(of)
    if not steps.is_empty() {
        section(phrase("tilolang-history")) {
            for s in steps {
                let key = "{s.at}/{s.who}"
                let single = s.versions.len() == 1
                let what = if let run = s.task {
                    run.title
                } else if let v = s.versions.last() {
                    if v.description == "" { v.what } else { v.description }
                } else { "" }
                stack(gap: .related) {
                    row(gap: .related, align: .center) {
                        avatar(s.who)
                        stack(gap: .tight) {
                            ui.Text(s.who.name(), role: .label)
                            ui.Text(what, role: .caption, secondary: true)
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
                                    ui.Text(
                                        if v.description == "" { v.what } else { v.description },
                                        role: .caption,
                                    )
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
                ui.NavItem(p.key, p.label, icon: p.icon, count: .some(p.count)) {
                    on select: set(place, p.key)
                }
            }
        }
        for g in side.groups {
            ui.NavSection(g.title, add: g.add) {
                for p in side.places.filter(x -> x.group == .some(g.title)) {
                    ui.NavItem(p.key, p.label, icon: p.icon, count: .some(p.count)) {
                        on select: set(place, p.key)
                    }
                }
            }
        }
    }
}
