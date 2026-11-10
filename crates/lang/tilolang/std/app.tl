app tilo.std "Standard library"

/// The window frames every tilolang app gets: a list of resources, a document, a page and a
/// transcript, each the native one. They are written over the toolkit's components with the
/// same language any app has (lenses, resource metadata, enum cases and events that carry
/// their value), so an app that wants something else composes `ui.*` itself and loses nothing
/// native.
///
/// A window that lists resources composes it: `std.list` puts the items down a column of rows
/// with what the platform does on a row, `std.places` the places the app names in the sidebar,
/// `std.add_entry` the entry at the list's end, and the app writes the shell around them and
/// says where each stands. A window that is only that is `std.browse`, which takes the resource
/// its window is of, the items and the places, and writes that shell the standard way.
///
/// The bar's actions are the ones the owner's type declares, which the host places there — the
/// primary one as the Primary button, the rest behind the bar's one menu; the primary's key is
/// the entry's at the list's end, so the key and the button lead the person there to type the
/// item; each item is a row whose actions the host puts on its context menu and its swipe, a
/// destructive one undone rather than confirmed. Items whose type has a window of their own
/// open in the next column; the list follows which one is open.
///
/// The items of a list down a column of rows, each built as it shows: a row with a document of
/// its own opens into it where it stands — the row grows into the item's page, and pressing it
/// again, Escape or a press beside it closes it — while a row whose item the window shows
/// elsewhere opens it beside the list when chosen and in front when activated, as every list of
/// the platform does; and the row says who else is at the item.
///
/// `row` draws an item, handed the item and who else is at it, and writes the `ui.ListItem` the
/// list shows: its `group:`, its `parent:` and its `tail:` say the list's grouping, nesting and
/// fold, which are the app's to say, and what it puts inside is the item's row. A list that
/// nests its rows gives `tree`; one that groups says the name of the items that group under
/// nothing (`ungrouped`) and of the run it folds under its tail (`tail_title`); one that does
/// neither gives neither. The list is the column's scroller unless `in_column` is off: a list
/// standing in a page's own scroller is as tall as its rows, and the page scrolls them with the
/// rest, so no list scrolls inside a scroller that scrolls it. A window too narrow for a page
/// beside the list gives the row's item a page of its own, opened in front of the list.
///
/// The row the person opened is the list's own state, so a window that lists and nothing else
/// declares none. A view that opens a row from elsewhere in the window — what `on show` does —
/// keeps that state itself and passes it as `opened:`.
template view list<T>(
    items: List<T>,
    opened: State<Option<T>> = open_item,
    tree: Bool = false,
    ungrouped: Option<Text> = .none,
    tail_title: Option<Text> = .none,
    in_column: Bool = true,
    row: (T, Option<Principal>) -> View,
) {
    // The item the person opened where it stands: the row grows into it, and pressing it again,
    // Escape, or a press beside it closes it. A row without a document opens elsewhere, in the
    // window's pane, so the list keeps nothing of it.
    state open_item: Option<T> = .none
    ui.VirtualList(
        selection_follows_focus: true,
        multi_select: true,
        in_column: in_column,
        narrow: space.compact,
        tree: tree,
        ungrouped_title: ungrouped,
        tail_title: tail_title,
        open: opened,
    ) {
        on select(x: T): open(x, front: false)
        on activate(x: T): open(x)
        on opened(x: T): set(opened, x)
        on closed: set(opened, .none)
        for x in items {
            row(x, lenses(x).find(l -> not l.mine)?.who)
        }
    }
}

/// The places a window offers in its sidebar: all of the items (`everything`, with `icon`), then
/// each of the filters the app names as one, the place chosen naming the window's `chosen` query
/// as people type it in search (`is:done`), and every one of them naming itself the same way.
/// What a window offers is what its app declares, never every filter its item type could be
/// found by.
template view places<T>(
    chosen: State<Text>,
    of: List<Filter<T>>,
    everything: Text,
    icon: Option<Text>,
) {
    ui.NavList(selected: if chosen == "" { "all" } else { chosen }) {
        ui.NavItem("all", everything, icon: icon) {
            on select: set(chosen, "")
        }
        for p in of {
            ui.NavItem(p.query, p.label, icon: p.icon) {
                on select: set(chosen, p.query)
            }
        }
    }
}

/// The mark a window shows beside the name of a service it reads its items from while that
/// service does not stand well: a quiet glyph while it cannot be reached, a warning one while it
/// was reached and what it fetched could not be kept, each saying why on hover, and nothing
/// while it stands well. `name` is what the person calls the service (their account's address, a
/// site), `online` whether it answered when it was last asked, and `trouble` why not, in the
/// service's own words.
template view service_mark(name: Text, online: Bool, trouble: Text) {
    if trouble != "" {
        if online {
            ui.Icon(.triangle_alert, size: .small, tone: .warning, description: "{name}: {trouble}")
        } else {
            ui.Icon(.wifi_off, size: .small, secondary: true, description: "{name}: {trouble}")
        }
    }
}

/// What a window says over the items it holds while the service they come from does not stand
/// well: one calm notice naming the service and what its trouble means, with `Try again` where
/// `retry` asks it again, and nothing while it stands well.
///
/// It stands as a card in the container the app gives it, a gutter in from the column it stands
/// in — the list's own, where it stands over rows — so its rounded corners are never cut by the
/// edge it would touch, and it is read where the items are without taking their place. What it
/// says is what the service cannot say of itself: the title is what the window cannot do (reach
/// it, save what it fetched), and under it the service's own reason in its words, capitalised,
/// and when the items under it were last read from it — `fetched`, as the clock a person reads
/// (`10:41`, `10:41 AM` in the style they chose), empty while the app knows no time to say.
template view service_banner(
    name: Text,
    online: Bool,
    trouble: Text,
    retry: Action,
    fetched: Text = "",
) {
    if trouble != "" {
        // The reason in a sentence of its own: "connection refused" is what the server said,
        // and the line it leads starts with a capital as every sentence does.
        let reason = trouble.slice(0, 1).upper() + trouble.slice(1, trouble.len())
        ui.Banner(
            if online { "Can't save what {name} fetched" } else { "Can't reach {name}" },
            message: if fetched == "" { reason } else { "{reason}. Last fetched {fetched}" },
            kind: if online { .warning } else { .neutral },
            icon: if online { .triangle_alert } else { .wifi_off },
            action: "Try again",
        ) {
            on act: retry
        }
    }
}

/// The entry at a list's end: what is typed and Entered there — or left there with — makes an
/// item of it, through `create`, an action whose one `Text` parameter takes the words nothing
/// else does. What the words say of the action's other parameters ("Call the landlord fri 10am
/// @Bo") fills them: the entry shows each as a chip under what is typed, and removing a chip
/// takes its words away. A parameter the call gave is the default there: it stands where the
/// words say nothing of it, and the words win where they do, so a list in a place that starts
/// its items one way (`new_task(tasks, due: if in_today { today })`) reads
/// "fri" as the day the person said. With no words of its own — nothing typed, space, or only
/// what the other parameters take — the entry makes nothing: Enter does nothing and leaves the
/// text there, and Escape leaves none. The host gives it the window's primary action's key, so
/// the key and the bar's button come here.
template view add_entry(placeholder: Text, create: Action) {
    state typed: Text = ""
    let parts = entry_parts(create, typed)
    stack(gap: .tight) {
        if not parts.is_empty() {
            row(gap: .tight, wrap: true) {
                for part in parts {
                    ui.Chip(part.label, icon: part.icon) {
                        on remove: set(typed, part.without)
                    }
                }
            }
        }
        ui.TextInput(bind: typed, placeholder: placeholder, entry: true) {
            on submit: create.then(set(typed, ""))
        }
    }
}

/// The window of `of` that lists `items`: the places the app names (`places`) in the sidebar,
/// with all of them called `everything`; the entry at the list's end when it gives `entry`; and
/// nothing to list when the place chosen finds nothing, where it says `empty`. The window owns
/// the query — the place a person chose and what the bar's search holds are its state, kept for
/// them alone — and the list is that one query, the title over it and its count naming that
/// place. The search's text narrows the list beside the place, found by the same search the
/// store answers every field from; closing the search widens it again.
///
/// The rows open beside the list when their type has a window of its own (`opens`, the type's own
/// fact): choosing one opens it in the column beside the list, and until one is chosen the list
/// lists for the whole window. A window whose rows open in front of it gives `opens: false`
/// instead: the list is then its content, under its own title, with no column to open beside it.
///
/// `row` draws one item's row and the window puts it down the list, with who else is at the item
/// as every list shows it. The row the person opened is the list's own, unless the app keeps one
/// (`opened`) to open a row from elsewhere. A window that needs more — grouped or nested rows, a
/// sidebar of its own — composes `std.list`, `std.places` and `std.tree` itself.
template view browse<R, T>(
    of: R,
    items: List<T>,
    opened: Option<State<Option<T>>> = .none,
    places: List<Filter<T>> = [],
    everything: Text = phrase("tilolang-all", kind: meta(T).plural),
    empty: Text = phrase("tilolang-empty-kind", kind: meta(T).plural),
    entry: Option<Action> = .none,
    entry_placeholder: Text = phrase("tilolang-add"),
    opens: Bool = meta(T).opens,
    row: (T) -> View,
) {
    state query: Text = ""
    state found: Text = ""
    let kind = meta(T)
    // The place chosen and what the window's search holds: one query.
    let asked = if not found.is_empty() { query + " " + found } else { query }
    let shown = items.matching(asked)
    let title = places.find(p -> p.query == query)?.label ?? everything
    let sidebar_title = if not places.is_empty() { meta(of).title }
    if opens {
        ui.NavShell(
            content_kind: .document,
            sidebar_title: sidebar_title,
            overview_title: title,
            overview_count: shown.len(),
        ) {
            on search(t: Text): set(found, t)
            sidebar: {
                if not places.is_empty() {
                    std.places(query, places, everything, icon: kind.icon)
                }
            }
            overview: {
                browse_rows(shown, opened, empty, kind.icon, entry, entry_placeholder) { x ->
                    row(x)
                }
            }
        }
    } else {
        ui.NavShell(
            content_kind: .list,
            title: title,
            count: shown.len(),
            sidebar_title: sidebar_title,
        ) {
            on search(t: Text): set(found, t)
            sidebar: {
                if not places.is_empty() {
                    std.places(query, places, everything, icon: kind.icon)
                }
            }
            content: {
                browse_rows(shown, opened, empty, kind.icon, entry, entry_placeholder) { x ->
                    row(x)
                }
            }
        }
    }
}

/// What a window that lists resources shows of them: their rows down the list, or `empty` when
/// the list holds nothing; with `entry`, the list's end holds the field that adds an item, and
/// the empty state stands over it rather than in its place, so the next one is typed as easily
/// as the first.
template view browse_rows<T>(
    items: List<T>,
    opened: Option<State<Option<T>>> = .none,
    empty: Text,
    icon: Option<Text>,
    entry: Option<Action>,
    entry_placeholder: Text,
    row: (T) -> View,
) {
    if let create = entry {
        ui.Docked {
            foot: {
                stack {
                    std.add_entry(entry_placeholder, create)
                }
            }
            if items.is_empty() {
                ui.EmptyState(empty, icon: icon, inline: true)
            } else {
                std.list(items, opened) { (x, at) ->
                    ui.ListItem(item: x, presence: at) {
                        row(x)
                    }
                }
            }
        }
    } else if items.is_empty() {
        ui.EmptyState(empty, icon: icon)
    } else {
        std.list(items, opened) { (x, at) ->
            ui.ListItem(item: x, presence: at) {
                row(x)
            }
        }
    }
}

/// Items as an outline, each item over its own to any depth, opened and closed where a person
/// puts them, its row the one `row` draws. The outline is named `name` for a reader; the name
/// is the page's, not a row over the items, so the page states it once. A move — a drag, `Tab`
/// and `Shift-Tab`, the accessibility move actions — names the item that moved and the item it
/// lands at, and `place` puts it in the one place they name:
/// `(moved, .none, .none)` is the end of the top level, `(moved, x, .none)` the end of `x`'s
/// own, and `(moved, .none, x)` where `x` stands. One step, so two people moving
/// items at once never leave one in two places. A row's checkbox asks `toggled`. Give `progress`
/// and a row with items under it says how far its branch got ("1/3"), as the list of the same
/// items says it. An item whose parent is not among `items` stands at the top, so a deleted
/// parent never hides what was inside it. `row` draws an item with the item it stands under
/// (`.none` at the top), so a child leaves out what its parent already says.
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
    row: (T, Option<T>) -> View,
) {
    if items.is_empty() {
        ui.EmptyState(empty, icon: icon)
    } else {
        ui.TreeView(name: name, fill: fill, reorder: true, tab_indents: true, hide_root: true) {
            on drop(moved: T, at: Option<T>): place(moved, .none, at)
            on into(moved: T, at: Option<T>): place(moved, at, .none)
            for x in items {
                // The node just above this one under the same parent, as the outline shows
                // them: the place the actions offered on it get. None for a first child.
                let above = items
                    .take(items.index_of(x) ?? 0)
                    .filter(y -> parent(y) == parent(x))
                    .last()
                let count = if let c = progress { c(x) }
                // How deep it stands in this outline and whether it holds any of its items, said
                // by the row itself: the tree builds the rows the window shows, so a row whose
                // parent stands out of view keeps the place this gives it. An item above the
                // outline's own is no step of it, so it counts none.
                let deep = items.ancestors(x, .parent).filter(a -> items.contains(a)).len()
                let holds = items.any(y -> parent(y) == .some(x))
                ui.TreeViewNode(
                    x,
                    label(x),
                    parent: parent(x),
                    depth: deep,
                    branch: holds,
                    above: above,
                    progress: count,
                ) {
                    on check: toggled(x)
                    row: {
                        row(x, parent(x))
                    }
                }
            }
        }
    }
}

/// Items as the tree of a window's sidebar: `items` — the collection itself, so the tree asks
/// the store for the rows it shows and for the place of each — each under the item it names as
/// its parent (`parent`), its row the one `row` draws and its words `label`. A row says how deep
/// it stands (`depth`) and whether it holds any (`branch`), so the rows a person does not see
/// cost nothing: the tree builds the rows the window shows, of ten notes as of ten thousand.
/// Each row says who else is at its item, as a list of them does (`std.list`).
///
/// `found` is what the window's search holds: the tree then shows the items the store finds for
/// those words, each with the items it stands under for context, in the quietest ink, and opens
/// the branches to them. Nothing found says so in the place of the tree. Empty, the tree is the
/// whole collection again, with the branches a person had opened as they left them.
///
/// The row chosen is the one the window has open (`opened`), and the tree keeps which branches a
/// person opened. A move — a drag before, after or inside a row, `Tab` and `Shift-Tab`, `Alt-Up`
/// and `Alt-Down` — names the item that moved and the item it lands at, and `place` puts it in
/// the one place they name: `(moved, .none, .none)` is the end of the top level,
/// `(moved, x, .none)` the end of `x`'s own, and `(moved, .none, x)` where `x` stands. A move
/// after a row and everything under it is `place_after(moved, x)`: what stands after them is a
/// row the tree may not have built, and only the view can name the ones under it. A tree a
/// search narrowed moves nothing: the rows it shows are not its order.
///
/// `add` draws the button a row shows over its trailing corner while it is hovered or holds
/// focus, so the tree makes what a person needs beside where it will stand. The tree is named
/// `name` for a reader, and a tree taller than the room it is given scrolls itself (`fill`).
template view tree_rows<T>(
    name: Text,
    items: Collection<T>,
    opened: State<Option<T>>,
    parent: (T) -> Option<T>,
    depth: (T) -> Option<Int>,
    branch: (T) -> Bool,
    label: (T) -> Text,
    place: (T, Option<T>, Option<T>) -> Action,
    place_after: (T, T) -> Action,
    add: (T) -> View,
    found: Text = "",
    nothing_found: Text = phrase("tilolang-nothing-found"),
    fill: Bool = true,
    row: (T) -> View,
) {
    let searching = not found.is_empty()
    // What the search found, the rows it shows and the branches it opens: the store's own answer
    // for the words, and nothing while the tree is the whole collection.
    let hits: List<T> = if searching { items.matching(found).flat_map(h -> [h]) } else { [] }
    // Each match, and the items it stands under from the top of the tree down, in the order the
    // tree gives them: a row once, however many matches stand under it. Each match costs the
    // items above it, never the items beside it.
    let shown: List<T> = if searching {
        hits.flat_map(
            h -> [items.ancestors(h, .parent).sort_by(a -> depth(a) ?? 0), [h]]
                .flat_map(rows -> rows),
        )
    } else { [] }
    // The branches a search opens: every item a match stands under, so each match is in sight.
    let open_branches: Option<List<T>> = if searching {
        .some(hits.flat_map(h -> items.ancestors(h, .parent)))
    } else { .none }
    let matched: Option<List<T>> = if searching { .some(hits) } else { .none }
    if searching and shown.is_empty() {
        ui.EmptyState(nothing_found, inline: true)
    } else {
        ui.TreeView(
            name: name,
            fill: fill,
            reorder: not searching,
            tab_indents: true,
            hide_root: true,
            single_click: true,
            selection_follows_focus: true,
            selection: if let x = opened { [x] },
            expanded: if let rows = open_branches { rows },
            matching: if let rows = matched { rows },
        ) {
            on select(x: T): set(opened, x)
            on activate(x: T): set(opened, x)
            on drop(moved: T, at: Option<T>): place(moved, .none, at)
            on into(moved: T, at: Option<T>): place(moved, at, .none)
            on after(moved: T, at: T): place_after(moved, at)
            // The matches and the rows they stand under, or the whole collection: one row each,
            // in the place the tree keeps it.
            for x in if searching { shown } else { items } {
                // The item just before this one under the same parent: the place the actions
                // offered on it get and the row `Tab` indents it under. The store's index
                // answers the run of `p`'s own, never a walk of the rows before it.
                let p = parent(x)
                let above: Option<T> = items.filter(y -> y.parent == p).previous(x)
                ui.TreeViewNode(
                    x,
                    label(x),
                    parent: p,
                    presence: lenses(x).find(l -> not l.mine)?.who,
                    depth: depth(x),
                    branch: branch(x),
                    above: above,
                    navigates: false,
                ) {
                    toolbar: {
                        add(x)
                    }
                    row: {
                        row(x)
                    }
                }
            }
        }
    }
}

/// A window that is a tree of items: the tree down its sidebar and the item open beside it, as
/// the page `doc` draws — one stack of blocks at the page's rhythm — with `head` the controls
/// standing in the sidebar's header, given the item open (or none), so what can be done with it
/// stands in the window's bar at every width rather than on a line of the page.
///
/// The window keeps which item it shows itself (`opened`): a view that answers `on show` with
/// the item passes its own state here (`opened: open`), so a link, a breadcrumb or a search that
/// shows an item moves the tree's choice to it and the page the window shows stays the tree's
/// own, never a second page pressed over it.
/// The bar over the page names where the open item stands (`path`: "Plans / Meetings", nothing
/// at the top level) and keeps the window's own actions.
///
/// `above` is a place the app keeps over the tree — a filter of its items, such as the ones
/// pinned — with the filter's own words over them, standing only while it holds anything.
/// Choosing one opens it beside as any row of the tree does. A window whose tree holds nothing
/// says `empty` in its sidebar, and one with nothing open says so beside it.
///
/// The bar's search is the tree's: what is typed narrows it to the items the store finds
/// (`tree_rows`' `found`), with the items they stand under shown for them, and closing the
/// search shows the tree as the person had it. The page beside them is found in too, where the
/// window shows one.
template view tree<R, T>(
    of: R,
    items: Collection<T>,
    opened: State<Option<T>> = open_item,
    parent: (T) -> Option<T>,
    depth: (T) -> Option<Int>,
    branch: (T) -> Bool,
    label: (T) -> Text,
    place: (T, Option<T>, Option<T>) -> Action,
    place_after: (T, T) -> Action,
    add: (T) -> View,
    path: (T) -> Text,
    above: List<Filter<T>> = [],
    empty: Text = phrase("tilolang-empty-kind", kind: meta(T).plural),
    head: (Option<T>) -> View,
    row: (T) -> View,
    doc: (T) -> View,
) {
    // The item the window opens: the first of the tree until a person chooses another, kept for
    // them alone, as every window keeps what it shows.
    state open_item: Option<T> = items.first()
    // What the bar's search holds: the tree narrows to the items it finds.
    state found: Text = ""
    // A reading column: wide, the tree docks beside the page it opens; narrow, the tree is the
    // window's first screen and the page is pushed in front of it, with the way back to it.
    ui.NavShell(
        content_kind: .reading,
        sidebar_title: meta(of).title,
        title: if let x = opened { path(x) },
    ) {
        on search(t: Text): set(found, t)
        sidebar_actions: {
            head(opened)
        }
        sidebar: {
            stack {
                // The shelves a search shows rows of its own in place of: a match stands where
                // the tree puts it, not again over it.
                for f in above {
                    let held = items.matching(f)
                    if not held.is_empty() and found.is_empty() {
                        ui.Text(f.label, role: .label, secondary: true)
                        std.tree_rows(
                            f.label,
                            held,
                            opened,
                            parent,
                            depth,
                            branch,
                            label,
                            place,
                            place_after,
                            add,
                            fill: false,
                        ) { x ->
                            row(x)
                        }
                    }
                }
                if items.is_empty() {
                    ui.EmptyState(empty, icon: meta(of).icon)
                } else {
                    std.tree_rows(
                        meta(of).title,
                        items,
                        opened,
                        parent,
                        depth,
                        branch,
                        label,
                        place,
                        place_after,
                        add,
                        found: found,
                    ) { x ->
                        row(x)
                    }
                }
            }
        }
        content: {
            if let x = opened {
                ui.Scroll {
                    // The page is the view's: `doc` gives its blocks as one stack at the page's
                    // own rhythm, so a heading keeps its content a step under it and more room
                    // under the block over it.
                    page {
                        doc(x)
                    }
                }
            } else {
                ui.EmptyState(phrase("tilolang-nothing-open"), icon: meta(of).icon, inline: true)
            }
        }
    }
}

/// A window's own controls, before the actions the host puts in the bar: `extra`, a page's
/// controls. The window's actions come from what its type declares, and the host places them
/// after these: its primary action as the labelled Primary button, a command that toggles a
/// state of the window's resource as a control of its own, and all the rest behind the bar's one
/// menu, so every action stays reachable at every width without the view having read them.
template view header(extra: View) {
    extra
}

/// A window that is a document of `of`: its content at the readable measure. A document edits
/// its title as its large title, and the host puts what can be done with it behind the "…"
/// beside that title. The title is `meta(of).title`: the field its type says names it, or the
/// first text field when the type says none, so a draft whose type names its subject is called
/// by the subject and its heading never repeats a line of its body.
view document<R>(of: R, content: View) {
    ui.NavShell(content_kind: .document, title: meta(of).title) {
        content: {
            ui.Scroll {
                page {
                    // A document's blocks are groups of their own: the gutter between groups
                    // stands over each section's heading, which keeps its content a step under
                    // it, so a heading reads with what it opens.
                    stack(gap: .group) {
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
template view body(content: View) {
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
                            stack(gap: .group, fill: fill) {
                                content
                            }
                        }
                    } else {
                        page {
                            stack(gap: .group) {
                                content
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One of an app's places on another app's page: a card that stands for `of`, names the place
/// as its app calls it (`name`), says how many items it holds, and shows the first of them as
/// their own rows (`row`, the type's row view, as everywhere else). The items are the place's,
/// not a filter rewritten here: an app that declares its place as a filter (`Task.today`) gives
/// its words and what it holds (`items.matching(Task.today)`), and one whose place no filter
/// asks, such as the conversations with anything left unread, writes a view of its own over
/// this. A card is a summary, so a list longer than the rows it shows ends with the way into
/// the rest: `Show all`, which opens `of`, as pressing the card does. The count is the one the
/// store answers without reading the place: exact, and none where a filter of several tests is
/// checked item by item, so a card of a place of the workspace's size never pays for its number.
///
/// A place with nothing in it takes no room on the page: one quiet line says it is caught up,
/// which is what a morning's page is for. Nothing else of it stands there — no count of none,
/// no empty list.
template view place<R, T>(of: R, name: Text, items: List<T>, row: (T) -> View) {
    // Whether the place holds anything at all, and how many where the store answers it without
    // reading the place: counting a filter checked item by item reads every item behind the
    // rows it shows, each time the page opens.
    let empty = items.is_empty()
    let counted = items.counted()
    ui.Card(item: of, title: name, count: if empty { .none } else { counted }) {
        if empty {
            ui.Text(phrase("tilolang-all-caught-up"), secondary: true)
        } else {
            list(items) { x ->
                row(x)
            }
            if let n = counted {
                if n > preview_rows() {
                    ui.Button("Show all {n}") {
                        on press: open(of)
                    }
                }
            }
        }
        on activate: open(of)
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
/// arrive, and offers the way back to it, or to what is new, while they read history. Each item
/// also says who has read to it (`seen_by(of, m)`): the list shows their faces there, once each,
/// where their reading ends.
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
                ui.VirtualList(follow_end: true, page_column: true, unseen_from: unread) {
                    for m in items {
                        let made = created(m)
                        ui.ListItem(
                            item: m,
                            author: made?.who,
                            time: made?.at,
                            read_by: seen_by(of, m),
                        ) {
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

/// The foot of a conversation: the field a reply is typed into, and the button that sends it.
/// Enter or the button runs `send`, then clears the field. The field takes everything the button
/// leaves and ends at its edge, so the two read as one control at any width. What a view says
/// over it (the message being answered, who is typing, what is being edited) is its own to place.
///
/// `speakers` names who is in the conversation, newest first, which its `@` drawer offers first:
/// a view that shows the conversation's items says so with `speakers(items)`, and a mention in
/// it reads as the person it names wherever it lands.
view composer(
    draft: State<RichText>,
    placeholder: Text,
    send: Action,
    speakers: List<Principal> = [],
) {
    row(gap: .none) {
        rich_input(bind: draft, placeholder: placeholder, speakers: speakers, fill) {
            on submit: send.then(set(draft, RichText("")))
        }
        ui.Button("Send", icon: .send, icon_only: true) {
            on press: send.then(set(draft, RichText("")))
        }
    }
}

/// Under a message that started a thread: how many replies it has, opening the thread.
view thread_chip<T>(thread: T, count: Int) {
    ui.Chip(phrase("tilolang-thread-replies", count: count), icon: .message_square) {
        on press: open(thread)
    }
}

/// What `of` was made from, when an action made it from another resource: one quiet row under
/// the title, naming where it came from in a word and that resource's title, its type's glyph
/// before it, which opens it. A resource made on its own shows nothing.
view origin<R>(of: R) {
    if let made = created(of), f = made.from {
        let kind = meta(f)
        // The glyph the app gives the type, and a link for a type whose app gives none.
        let glyph = if let i = kind.icon { i } else { "link" }
        ui.ListRow(kind.title, prefix: phrase("tilolang-from"), item: f, icon: glyph)
    }
}

/// One resource `of` is linked to, as a reference: in its type's own row where the app declares
/// one, so its state shows there (a done item says done) and pressing it opens it; any other
/// type is a row saying its title under the glyph of its app. Dense is the quiet form: one
/// caption line, for a link under the content it belongs to.
view linked_row<R>(of: R, dense: Bool = false) {
    let kind = meta(of)
    if kind.row {
        show(of, row: true)
    } else {
        let glyph = if let i = kind.icon { i } else { "link" }
        ui.ListRow(kind.title, item: of, icon: glyph, dense: dense)
    }
}

/// What `linked(of)` returns, each one a reference that opens when pressed: under one heading,
/// at the density of a page of related things, or compact — quiet reference rows right under
/// the content they belong to, one caption line each, as what a message was made from. Links by
/// a field are left out: the page of the resource holding the field draws that reference as its
/// own section of related things, so drawing it here again would show it twice. `without` names
/// what the page draws itself, as a task's page draws its discussion: those are left out too.
view linked<R>(of: R, compact: Bool = false, without: List<host.Resource> = []) {
    let linked_to = linked(of).filter(l -> l.how != .field)
    let links = linked_to.filter(l -> not without.any(x -> x == l.of))
    if not links.is_empty() {
        if compact {
            stack(gap: .tight) {
                for l in links {
                    linked_row(l.of, dense: true)
                }
            }
        } else {
            section(phrase("tilolang-linked")) {
                for l in links {
                    linked_row(l.of)
                }
            }
        }
    }
}

/// What waits to be proposed of `of`: each pending proposal as the card a person accepts or
/// dismisses it with — the action's own `proposal` view where it declares one, else what the
/// action would do in words — naming who proposed it. Nothing waits: nothing shows.
view suggestions<R>(of: R) {
    proposals(of)
}

/// What happened to `of`, newest first, as the platform folds its history (`steps`): each step,
/// consecutive changes by one person or by one run, is an entry saying who made it, what
/// they did and when — one sentence, wrapping as a sentence does, the face of whoever made it
/// beside it. What they did is what its last change says (`version_said`), worked out from the
/// fields the change wrote, the person looking as "You". What the sentence leaves out stands
/// under it, muted (`version_notes`): each field the change wrote, what it held before beside
/// what it holds now, each as the field's type shows it. Steps of one hand, moments apart, group
/// under the face of the step that opens them.
///
/// A change is taken back where a person means it. The newest step is the change the page shows
/// the result of, so it offers its undo where it stands, as quiet as the words around it, a
/// run's undo taking the whole run. Every older step keeps its undo in the step's own menu: a
/// history reads as a timeline, not as a column of buttons. The step that made the resource
/// offers nothing to take back — reversing it would remove the thing whose page a person is
/// reading it on. A step of several changes is opened to each of them, each taken back on its
/// own there. One change made over many items at once — a command run on a choice of them — is
/// one step: it says how many items it changed, and its undo takes them all back together.
view history<R>(of: R) {
    state open: Text = ""
    let steps = steps(of)
    if not steps.is_empty() {
        section(phrase("tilolang-history")) {
            for s in steps {
                let key = "{s.at}/{s.who}"
                // The step above this one: where it is the same hand's a moment before, this
                // one joins the visit that step's face opened.
                let prev = steps.previous(s)
                let joined = if let p = prev { p.who == s.who and s.at - p.at < 1h } else { false }
                // The newest step, whose result is what the page shows.
                let newest = prev == .none
                // The step's first change made the resource: the step holds its making, and
                // nothing of it is taken back from the page of the thing it made. A step's
                // versions run oldest first, so its newest is what its sentence says.
                let made = if let v = s.versions.first() { v.kind == "created" } else { false }
                let undoable = not made and (s.run != .none or s.mine and s.changes == 1)
                stack(gap: .related) {
                    row(gap: .tight, align: .start) {
                        if joined {
                            // The room the face above takes, so one visit lines up under it.
                            stack(min: 2rem, max: 2rem) {
                            }
                        } else {
                            avatar(s.who)
                        }
                        stack(gap: .tight, fill: 1.0) {
                            // Who, what, when: one sentence. It wraps rather than squeezing the
                            // name it opens with, and what can be taken back at its end is one
                            // quiet control.
                            row(gap: .tight, align: .center, wrap: true) {
                                if let v = s.versions.last() {
                                    ui.Text(v.actor, role: .label)
                                    // A run's own words stand under its sentence: its changes are
                                    // what it did, and it names them all.
                                    if s.run == .none {
                                        std.version_said(v)
                                    }
                                }
                                ui.Text("·", role: .caption, secondary: true)
                                ui.Time(s.at, role: .caption, secondary: true)
                                spacer()
                                // Taken back here, quietly, on the step the page ends with; in
                                // the step's menu anywhere older.
                                if undoable {
                                    if newest {
                                        if let run = s.run {
                                            ui.Button(
                                                phrase("tilolang-history-undo-run"),
                                                role: .plain,
                                                size: .small,
                                            ) {
                                                on press: undo(run)
                                            }
                                        } else if let v = s.versions.last() {
                                            ui.Button(
                                                phrase("tilolang-undo"),
                                                role: .plain,
                                                size: .small,
                                            ) {
                                                on press: undo(v)
                                            }
                                        }
                                    } else {
                                        ui.MenuButton(
                                            phrase("tilolang-history-actions"),
                                            icon: .more_horizontal,
                                            icon_only: true,
                                            role: .plain,
                                        ) {
                                            if let run = s.run {
                                                ui.MenuItem(
                                                    phrase("tilolang-history-undo-run"),
                                                    icon: .undo_2,
                                                ) {
                                                    on choose: undo(run)
                                                }
                                            } else if let v = s.versions.last() {
                                                ui.MenuItem(
                                                    phrase("tilolang-history-revert"),
                                                    icon: .undo_2,
                                                ) {
                                                    on choose: undo(v)
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            if let run = s.run {
                                ui.Text(run.title, role: .caption, secondary: true)
                            }
                            if s.changes == 1 {
                                if let v = s.versions.last() {
                                    std.version_notes(v, spoken: s.run == .none)
                                }
                            } else if s.changes > 1 {
                                ui.Text(
                                    phrase("tilolang-history-changes", count: s.changes),
                                    role: .caption,
                                    secondary: true,
                                )
                                if open == key {
                                    ui.Button(
                                        phrase("tilolang-history-hide"),
                                        role: .plain,
                                        size: .small,
                                    ) {
                                        on press: set(open, "")
                                    }
                                    for v in s.versions {
                                        stack(gap: .tight) {
                                            row(gap: .tight, align: .center, wrap: true) {
                                                ui.Text(v.actor, role: .label)
                                                std.version_said(v)
                                                ui.Text("·", role: .caption, secondary: true)
                                                ui.Time(v.at, role: .caption, secondary: true)
                                                spacer()
                                                if v.mine and v.kind != "created" {
                                                    ui.Button(
                                                        phrase("tilolang-undo"),
                                                        role: .plain,
                                                        size: .small,
                                                    ) {
                                                        on press: undo(v)
                                                    }
                                                }
                                            }
                                            std.version_notes(v, spoken: s.run == .none)
                                        }
                                    }
                                } else {
                                    ui.Button(
                                        phrase("tilolang-history-show"),
                                        role: .plain,
                                        size: .small,
                                    ) {
                                        on press: set(open, key)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// What a version did, in the words a person reads after who did it, one sentence: the words the
/// app writes for the action (`did:`), the fields the change wrote and what they held ("set the
/// due date to", then the day as a day), or, for one that declares nothing and changed no field,
/// the action's label, which the platform reads by the action's own name when it declares none.
/// Never what the action's declaration explains to the person writing the app: history says what
/// happened, and a doc comment is not part of what happened.
template view version_said(v: host.Version) {
    if v.count > 1 {
        ui.Text(phrase("tilolang-did-many", count: v.count, kind: meta(v.of).plural), role: .label)
    } else if v.did != "" {
        ui.Text(v.did, role: .label)
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
        ui.Text(v.what, role: .label)
    }
}

/// What stands muted under a version's sentence: what the change did to each field it holds,
/// where the sentence does not say it, and who suggested it where someone did.
template view version_notes(v: host.Version, spoken: Bool = true) {
    std.version_changes(v, spoken: spoken)
    if v.suggested != "" {
        ui.Text(phrase("tilolang-suggested-by", name: v.suggested), role: .caption, secondary: true)
    }
}

/// What a version did to each field it changed, in muted text: the field's name, what it held
/// before and what it holds after, each as the field's type shows it. A sentence built from the
/// fields' own words already says the value it names, so only what it leaves out stands here;
/// a sentence that says the change in the app's words says nothing of the fields, so all of
/// them do. A change that made or removed the resource already says so above, so nothing stands
/// under it.
template view version_changes(v: host.Version, spoken: Bool = true) {
    let unsaid = if spoken {
        v.changes.filter(c -> v.count > 1 or v.did != "" or c.shows == "")
    } else { v.changes }
    if v.kind != "created" and v.kind != "removed" and not unsaid.is_empty() {
        grid(columns: 3, gap: .related) {
            for c in unsaid {
                ui.Text(c.label, role: .caption, secondary: true)
                c.before
                c.after
            }
        }
    }
}
