/// The window frames every tilolang app gets: a list of resources, a document, a page and a
/// transcript, each the native one. They are written over the toolkit's components with the
/// same language any app has (action reflection, lenses, resource metadata, enum cases and
/// events that carry their value), so an app that wants something else composes `ui.*`
/// itself and loses nothing native.

/// A window that lists resources: the header holds its owner's actions, the primary one as
/// the Primary button; each item is a row whose actions are on its context menu and its swipe,
/// a destructive one undone rather than confirmed; with no items it says `empty` and offers
/// the primary action. The places in the sidebar are the item type's filters that are on or
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
    row: (T) -> View
) {
    window query: Text = place
    let kind = meta(T)
    let offered = actions(of)
    let places = filters(T).filter(f -> f.toggle)
    let shown = items.matching(query)
    let title = places.find(p -> p.query == query)?.label ?? if places.is_empty() { meta(of).title } else { everything }
    // An empty list offers the primary action while nothing was made yet: on the place a
    // window starts on, or on all of the items. One another filter emptied says only that.
    let primary = if query == "" or query == place { offered.find(a -> a.primary) } else { .none }
    ui.NavShell(
        content_kind: if kind.opens { .document } else { .list },
        sidebar_title: if places.is_empty() { .none } else { .some(meta(of).title) },
        overview_title: if kind.opens { .some(title) } else { .none },
        overview_count: if kind.opens { .some(shown.len()) } else { .none },
        title: if kind.opens { .none } else { .some(title) },
        count: if kind.opens { .none } else { .some(shown.len()) }
    ) {
        sidebar: {
            if not places.is_empty() {
                ui.NavList(selected: if query == "" { "all" } else { query }) {
                    ui.NavItem("all", everything, icon: kind.icon, count: .some(items.len())) {
                        on select: set(query, "")
                    }
                    for p in places {
                        ui.NavItem(p.query, p.label, icon: p.icon, count: .some(items.matching(p.query).len())) {
                            on select: set(query, p.query)
                        }
                    }
                }
            }
        }
        overview_actions: {
            if kind.opens {
                std.header(offered, sort: sort)
            }
        }
        actions: {
            if not kind.opens {
                std.header(offered, sort: sort)
            }
        }
        overview: {
            if kind.opens {
                std.rows(shown, empty, primary, kind.icon) { x -> row(x) }
            }
        }
        content: {
            if kind.opens {
                ui.EmptyState(phrase("tilolang-none-selected", kind: kind.noun), icon: kind.icon)
            } else {
                std.rows(shown, empty, primary, kind.icon) { x -> row(x) }
            }
        }
    }
}

/// The items of a list as the rows of a column, or what it says while it has none.
view rows<T>(
    items: List<T>,
    empty: Text,
    primary: Option<host.Action>,
    icon: Option<Text>,
    row: (T) -> View
) {
    if items.is_empty() {
        if let a = primary {
            ui.EmptyState(empty, icon: icon, action: a.label) {
                on act: a.run
            }
        } else {
            ui.EmptyState(empty, icon: icon)
        }
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            on select(x: T): open(x, front: false)
            on activate(x: T): open(x)
            for x in items {
                let offered = actions(x)
                ui.ListItem(item: x, presence: lenses(x).find(l -> not l.mine)?.who) {
                    menu: {
                        for a in offered {
                            ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive) {
                                on choose: a.run
                            }
                        }
                    }
                    swipe: {
                        if let a = offered.find(.destructive) {
                            ui.SwipeAction(a.label, icon: a.icon, destructive: true) {
                                on swipe: a.run
                            }
                        } else if let a = offered.first() {
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
}

/// A header's controls, as native headers place them: the sort menu and search first, then
/// `extra`, then each action with an icon as an icon button (drawn on while the state it
/// toggles is), a More menu of those without one, and the primary action last as the labelled
/// Primary button.
view header<S>(
    offered: List<host.Action>,
    sort: Option<State<S>> = .none,
    extra: View
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
    for a in offered {
        if not a.primary and a.icon != .none {
            ui.Button(a.label, icon: a.icon, icon_only: true, toggled: a.on == .some(true)) {
                on press: a.run
            }
        }
    }
    let more = offered.filter(a -> not a.primary and a.icon == .none)
    if not more.is_empty() {
        ui.MenuButton(phrase("tilolang-more"), icon: .more_horizontal, icon_only: true) {
            for a in more {
                ui.MenuItem(a.label, destructive: a.destructive) {
                    on choose: a.run
                }
            }
        }
    }
    for a in offered {
        if a.primary {
            ui.Button(a.label, icon: a.icon, role: .primary) {
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
/// page's own controls. A page with nothing on it (`empty`) says so and offers its primary
/// action.
view page<R>(of: R, canvas: Bool = false, empty: Bool = false, header: View, content: View) {
    ui.NavShell(content_kind: .page, title: meta(of).title) {
        actions: {
            std.header(actions(of)) {
                header
            }
        }
        content: {
            if empty {
                if let a = actions(of).find(.primary) {
                    ui.EmptyState(phrase("tilolang-empty"), icon: meta(of).icon, action: a.label) {
                        on act: a.run
                    }
                } else {
                    ui.EmptyState(phrase("tilolang-empty"), icon: meta(of).icon)
                }
            } else {
                ui.Scroll {
                    if canvas {
                        page(full) {
                            stack(gap: .section) {
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
view transcript<R, T>(of: R, items: List<T>, composer: View, row: (T) -> View) {
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
                ui.VirtualList(transcript: true) {
                    for m in items {
                        let made = created(m)
                        let offered = actions(m)
                        ui.ListItem(item: m, author: made?.who, time: made?.at) {
                            menu: {
                                for a in offered {
                                    ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive) {
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
                let what = if let run = s.task { run.title } else if let v = s.versions.last() { if v.description == "" { v.what } else { v.description } } else { "" }
                stack(gap: .related) {
                    row(gap: .related, align: .center) {
                        avatar(s.who)
                        stack(gap: .tight) {
                            ui.Text("{s.who}", role: .label)
                            ui.Text(what, role: .caption, secondary: true)
                            if not single {
                                ui.Text(phrase("tilolang-history-changes", count: s.versions.len()), role: .caption, secondary: true)
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
                                    ui.Text(if v.description == "" { v.what } else { v.description }, role: .caption)
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
