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
/// same list; a window starts on `place` (all of the items when empty). A `sort` view state is
/// the header's sort menu. Items whose type has a window of
/// their own open in the next column; the list follows which one is open.
view list<R, T, S>(
    of: R,
    items: List<T>,
    sort: Option<State<S>> = .none,
    place: Text = "",
    empty: Text = phrase("tilolang-empty"),
    row: (T) -> View
) {
    window query: Text = place
    let kind = meta(T)
    let everything = phrase("tilolang-all", kind: kind.plural)
    let offered = actions(of)
    let places = filters(T).filter(f -> f.toggle)
    let shown = items.matching(query)
    let title = match places.find(p -> p.query == query) {
        .some(p) => p.label,
        .none => if places.is_empty() { meta(of).title } else { everything },
    }
    // An empty list offers the primary action while nothing was made yet; one a filter emptied
    // says only that.
    let primary = if query == "" { offered.find(a -> a.primary) } else { .none }
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
        match primary {
            .some(a) => ui.EmptyState(empty, icon: icon, action: a.label) {
                on act: a.run
            },
            .none => ui.EmptyState(empty, icon: icon),
        }
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            on select(x: T): open(x, front: false)
            on activate(x: T): open(x)
            for x in items {
                let offered = actions(x)
                ui.ListItem(item: x, presence: match lenses(x).find(l -> not l.mine) {
                    .some(l) => .some(l.who),
                    .none => .none,
                }) {
                    menu: {
                        for a in offered {
                            ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive) {
                                on choose: a.run
                            }
                        }
                    }
                    swipe: {
                        match offered.find(a -> a.destructive) {
                            .some(a) => ui.SwipeAction(a.label, icon: a.icon, destructive: true) {
                                on swipe: a.run
                            },
                            .none => {
                                match offered.first() {
                                    .some(a) => ui.SwipeAction(a.label, icon: a.icon) {
                                        on swipe: a.run
                                    },
                                    .none => {}
                                }
                            },
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
    match sort {
        .some(s) => ui.MenuButton(phrase("tilolang-sort"), icon: .arrow_up_down, icon_only: true) {
            for c in cases(S) {
                ui.MenuItem(c.label(), checked: c == s) {
                    on choose: set(s, c)
                }
            }
        },
        .none => {}
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
/// page's own controls. A page with nothing on it (`empty`) says so.
view page<R>(of: R, canvas: Bool = false, empty: Bool = false, header: View, content: View) {
    ui.NavShell(content_kind: .page, title: meta(of).title) {
        actions: {
            std.header(actions(of)) {
                header
            }
        }
        content: {
            if empty {
                ui.EmptyState(phrase("tilolang-empty"), icon: meta(of).icon)
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
/// `row`, built as they show, above the composer, which stays at the foot.
view transcript<R, T>(of: R, items: List<T>, composer: View, row: (T) -> View) {
    ui.NavShell(content_kind: .page, title: meta(of).title) {
        actions: {
            std.header(actions(of))
        }
        content: {
            ui.Docked {
                foot: {
                    page(full) {
                        stack {
                            composer
                        }
                    }
                }
                ui.VirtualList(transcript: true) {
                    for m in items {
                        ui.ListItem(item: m) {
                            row(m)
                        }
                    }
                }
            }
        }
    }
}
