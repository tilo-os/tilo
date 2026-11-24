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
/// fold, which are the app's to say, and what it puts inside is the item's row. A row whose item
/// the person can finish says so as `done:`: a place counts what waits, so the heading over the
/// rows counts what is left to do, and a row just finished keeps its place without counting. A
/// list that nests its rows gives `tree`; one that groups says the name of the items that group
/// under nothing (`ungrouped`) and of the run it folds under its tail (`tail_title`); one that
/// does neither gives neither. The list is the column's scroller unless `in_column` is off: a
/// list in a page's own scroller is as tall as its rows, and the page scrolls them with
/// the rest, so no list scrolls inside a scroller that scrolls it. A window too narrow for a page
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
    reveal: Option<Text> = .none,
    row: (T, Option<Actor>) -> View,
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
        reveal: reveal,
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
/// well: one calm notice naming the service and what its trouble means, with `retry_label` where
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
    retry_label: Text = "Try again",
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
            action: retry_label,
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

/// The headings of a document, as places under the tree it stands in: each heading a row, nested
/// under the nearest shallower heading before it, choosing one showing that heading. `lines` is
/// what an editor reports of the document it edits (`on headings`): each heading's own words, and
/// `levels` how deep each stands (`2` for `##`), one level for each line. `show` is where a chosen
/// row is kept: the heading's place among the document's headings, then a count that changes with
/// each ask (`3:1`), which is what an editor's `reveal` takes, so the same heading shows again
/// when it is chosen again. The row a person chose stays chosen while the document is the one
/// they read.
///
/// A document with no headings draws nothing: the place is not there to choose from.
template view headings(
    name: Text,
    lines: List<Text>,
    levels: List<Int>,
    show: State<Option<Text>>,
) {
    // How many times a row was chosen: each ask is told apart from the one before, so a heading
    // chosen twice shows twice.
    state asked: Int = 0
    if not lines.is_empty() {
        let chosen = if let shown = show { shown.split(":").first() }
        ui.NavList(title: name, selected: chosen) {
            // Each heading's words are its row's, never the marker a writer typed. A heading
            // stands under the nearest heading before it that stands shallower; one that skips a
            // level is given no row made up for the level it skipped.
            for line in lines {
                let at = lines.index_of(line) ?? 0
                let level = levels.at(at) ?? 0
                let above = lines
                    .take(at)
                    .filter(
                        other -> (levels.at(lines.index_of(other) ?? 0) ?? 0) < level,
                    )
                    .last()
                ui.NavItem(
                    "{at}",
                    if line.trim() == "" { phrase("tilolang-untitled") } else { line },
                    parent: if let over = above { "{lines.index_of(over) ?? 0}" },
                ) {
                    on select: set(asked, asked + 1).then(set(show, "{at}:{asked}"))
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
/// `name` for a reader, and a tree taller than the room it is given scrolls itself (`fill`);
/// that tree is its pane's list, so its rows stand on the pane's content edge as a list's do,
/// and a window a phone holds, where the pane is the whole screen, keeps its page gutter.
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
/// in the sidebar's header, given the item open (or none), so what can be done with it
/// stands in the window's bar at every width rather than on a line of the page.
///
/// `below` draws what stands under the tree in the sidebar — a map of the open item the tree
/// cannot hold, such as its headings (`std.headings`) — and `doc` the item open, as a page the
/// window's own reading takes. Both are the caller's blocks, so they read the state the caller
/// keeps, its `opened` among it: the page is the caller's to draw for the item it shows.
///
/// The window keeps which item it shows itself (`opened`): a view that answers `on show` with
/// the item passes its own state here (`opened: open`), so a link, a breadcrumb or a search that
/// shows an item moves the tree's choice to it and the page the window shows stays the tree's
/// own, never a second page pressed over it.
/// The bar over the page names where the open item stands (`path`: "Plans / Meetings", nothing
/// at the top level) and keeps the window's own actions.
///
/// `above` is a place the app keeps over the tree — a filter of its items, such as the ones
/// pinned — with the filter's own words over them, only while it holds anything.
/// Choosing one opens it beside as any row of the tree does. A window whose tree holds nothing
/// says `empty` in its sidebar, and the body beside it says what starts the first one; the way to
/// start one stands in the empty state a person faces. A tree with nothing open says so beside it.
///
/// The bar's search is the tree's: what is typed narrows it to the items the store finds
/// (`tree_rows`' `found`), with the items they stand under shown for them, and closing the
/// search shows the tree as the person had it. The page beside them is found in too, where the
/// window shows one.
///
/// `drop_files` is what a file the person drops anywhere on the window becomes: the window keeps
/// each file and hands it over as a `File` — its name, its media type and the address its bytes
/// are at — so the app decides, without ever reading a path.
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
    below: View,
    above: List<Filter<T>> = [],
    empty: Text = phrase("tilolang-empty-kind", kind: meta(T).plural),
    head: (Option<T>) -> View,
    row: (T) -> View,
    doc: View,
    drop_files: (List<File>) -> Action,
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
        // A file dropped anywhere on the window is the app's to take: the tree stands for the
        // whole window, so the app decides what a dropped file becomes.
        on drop(files: List<File>): drop_files(files)
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
                // The tree takes the room a map of the open item leaves under it, so a few
                // headings never push the tree past the foot of the sidebar.
                ui.Docked {
                    foot: {
                        if found.is_empty() {
                            below
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
        }
        content: {
            if opened != .none {
                ui.Scroll {
                    // The page is the view's: `doc` gives its blocks as one stack at the page's
                    // own rhythm, so a heading keeps its content a step under it and more room
                    // under the block over it.
                    page {
                        doc
                    }
                }
            } else if items.is_empty() {
                // A body with nothing open yet says what to do: the first one starts here, and
                // the window offers the app's primary action in this state.
                ui.EmptyState(
                    phrase("tilolang-empty-start", noun: meta(T).noun),
                    icon: meta(of).icon,
                )
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
            std.document_page {
                content
            }
        }
    }
}

/// A document's page as the content column of a window that shows it beside a list: the page's
/// scroll, its gutters and the group gutter under each block, without a shell of its own. A
/// window of the document alone wraps it in one (`std.document`); a window that lists its type
/// beside it gives it the content column of its own shell, so the list stays beside the page
/// instead of the page growing out of the row it was opened from.
template view document_page(content: View) {
    ui.Scroll {
        page {
            // A document's blocks are groups of their own: the gutter between groups stands
            // over each section's heading, which keeps its content a step under it, so a
            // heading reads with what it opens.
            stack(gap: .group) {
                content
            }
        }
    }
}

/// What follows a document's heading, one row lead in: the body starts under the text the
/// heading's control leads to, so a task's notes, subtasks and facts stand in the column its
/// title's text does, and the box that says the item is done stands in the gutter to their
/// left. A document whose heading opens with its title alone has no lead to make, and a page
/// on a phone's screen keeps none either: the column there is too narrow to give a control's
/// cell out of every line, so the body starts on the page's gutter and the heading keeps its
/// control beside its title.
template view body(content: View) {
    // A document's blocks under its heading are groups of their own, a group gutter apart: a
    // section's heading keeps its content a step under it and more room over it.
    stack(lead: 1, gap: .group) {
        content
    }
}

/// The page's own sections after the writing it holds: what links here, the discussion, the
/// history. A rule says where a person's writing ends, so a heading the page goes by never reads
/// as another line of what was written, and each section stands a group gutter under it, further
/// from the writing than the writing's own headings stand from each other.
template view after_body(content: View) {
    stack(gap: .group) {
        divider()
        content
    }
}

/// A window that is a page of `of`: its content at the readable measure, or as a `canvas` at
/// the window's full width, under `header`, the page's own controls, and the actions the host
/// puts in the bar with them. A `fill` page is exactly the window's height and scrolls inside
/// its own parts, which is what a board's lists need; a page that grows with what it holds (a
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
    transcript_by(
        of,
        items,
        author: m -> created(m)?.who,
        unread: unread,
        composer: composer,
        row: row,
    )
}

/// [`transcript`] with `author` saying who wrote an item where it stands: the person or agent
/// whose face and name open the run of their messages. An item that answers `.none` opens no
/// run, which is how a message carried in from another service shows the account that wrote it
/// there instead of whoever carried it.
view transcript_by<R, T>(
    of: R,
    items: List<T>,
    author: (T) -> Option<Actor>,
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
                            author: author(m),
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
view composer(draft: State<RichText>, placeholder: Text, send: Action, speakers: List<Actor> = []) {
    row(gap: .none) {
        rich_input(bind: draft, placeholder: placeholder, speakers: speakers, fill) {
            on submit: send.then(set(draft, RichText("")))
        }
        ui.Button("Send", icon: .send, icon_only: true) {
            on press: send.then(set(draft, RichText("")))
        }
    }
}

/// The foot of a conversation whose text is a draft record the app keeps, one its author alone
/// reads (`readers: author`): what is typed is the record's `body`, so it survives the window and
/// reaches the person's other devices, and `send` is the action that makes the message from it and
/// empties it, so one press is one change to undo. A record shared by everyone looking cannot be
/// bound here: two people typing into it would overwrite each other.
/// `attach`, when a view gives one, stands beside the field as the way to add files to what is
/// written: the view asks the person for them its own way (the system's chooser), and shows what
/// it added itself.
template view composer_draft<D>(
    draft: D,
    placeholder: Text,
    send: Action,
    speakers: List<Actor> = [],
    attach: Option<Action> = .none,
) {
    row(gap: .none) {
        if let a = attach {
            ui.Button("Attach", icon: .paperclip, icon_only: true) {
                on press: a
            }
        }
        rich_input(bind: draft.body, placeholder: placeholder, speakers: speakers, fill) {
            on submit: send
        }
        ui.Button("Send", icon: .send, icon_only: true) {
            on press: send
        }
    }
}

/// The system's call held on `of`: who is in it, each with their face and what they do there
/// (talking, muted, deafened, on camera, sharing their screen), and the person's own controls:
/// Join while they are not in it; mute, deafen, camera, screen and Leave while they are. Whoever
/// is on camera or sharing a screen shows as a tile of the call's own picture; everyone else shows
/// as their face and a word of what they do. Who is in it is the call service's presence, never
/// stored; who may speak or listen is the workspace's, and the microphone, camera and screen are
/// asked for through the system's prompts.
view call<R>(of: R) {
    let people = call_members(of)
    let mine = people.filter(m -> m.mine)
    let pictured = people.filter(m -> m.camera or m.screen)
    let inputs = call_inputs(of)
    let microphones = inputs.filter(i -> not i.camera)
    let cameras = inputs.filter(i -> i.camera)
    ui.NavShell(content_kind: .page, title: meta(of).title) {
        content: {
            ui.Docked {
                foot: {
                    row(gap: .tight, distribute: .center, wrap: true) {
                        if let me = mine.first() {
                            if microphones.len() > 1 {
                                call_input_menu(of, microphones, phrase("tilolang-call-microphone"))
                            }
                            ui.Button(
                                phrase(
                                    if me.muted {
                                        "tilolang-call-unmute"
                                    } else { "tilolang-call-mute" },
                                ),
                                icon: if me.muted { .mic_off } else { .mic },
                                icon_only: true,
                            ) {
                                on press: mute_call(of, not me.muted)
                            }
                            ui.Button(
                                phrase(
                                    if me.deafened {
                                        "tilolang-call-undeafen"
                                    } else { "tilolang-call-deafen" },
                                ),
                                icon: if me.deafened { .volume_x } else { .headphones },
                                icon_only: true,
                            ) {
                                on press: deafen_call(of, not me.deafened)
                            }
                            ui.Button(
                                phrase(
                                    if me.camera {
                                        "tilolang-call-camera-off"
                                    } else { "tilolang-call-camera-on" },
                                ),
                                icon: .video,
                                icon_only: true,
                            ) {
                                on press: camera_call(of, not me.camera)
                            }
                            if cameras.len() > 1 {
                                call_input_menu(of, cameras, phrase("tilolang-call-camera"))
                            }
                            ui.Button(
                                phrase(
                                    if me.screen {
                                        "tilolang-call-screen-off"
                                    } else { "tilolang-call-screen-on" },
                                ),
                                icon: .screen_share,
                                icon_only: true,
                            ) {
                                on press: screen_call(of, not me.screen)
                            }
                            ui.Button(
                                phrase("tilolang-call-leave"),
                                icon: .phone_off,
                                role: .destructive,
                            ) {
                                on press: leave_call(of)
                            }
                        } else {
                            ui.Button(phrase("tilolang-call-join"), icon: .phone, role: .primary) {
                                on press: join_call(of)
                            }
                        }
                    }
                }
                if people.is_empty() {
                    ui.EmptyState(
                        phrase("tilolang-call-empty"),
                        icon: .volume_2,
                        message: phrase("tilolang-call-empty-message"),
                    )
                } else {
                    ui.Scroll {
                        page {
                            stack(gap: .related) {
                                if not pictured.is_empty() {
                                    grid(
                                        columns: if pictured.len() == 1 { 1 } else { 2 },
                                        gap: .group,
                                    ) {
                                        for m in pictured {
                                            if m.screen {
                                                ui.CallTile(
                                                    who: m.who,
                                                    name: m.name,
                                                    source: .screen,
                                                    speaking: m.speaking,
                                                    muted: m.muted,
                                                    fill,
                                                )
                                            } else {
                                                ui.CallTile(
                                                    who: m.who,
                                                    name: m.name,
                                                    source: .camera,
                                                    speaking: m.speaking,
                                                    muted: m.muted,
                                                    fill,
                                                )
                                            }
                                        }
                                    }
                                }
                                if not people.is_empty() {
                                    section(phrase("tilolang-call-people")) {
                                        stack(gap: .tight) {
                                            for m in people {
                                                call_member(of, m)
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
    }
}

/// One person in a call, as [`call`] lists them: their face and name, what they do there, and how
/// loud this device plays them, which the person sets for themselves and nobody else's call
/// hears. Someone a bridge carries in from another service shows as that service shows them,
/// their name and picture there, marked as from it.
view call_member<R>(of: R, m: host.CallMember) {
    row(gap: .tight, align: .center) {
        if m.service != "" {
            if m.avatar != "" {
                ui.Avatar(name: m.name, picture: m.avatar)
            } else {
                ui.Avatar(name: m.name)
            }
            ui.Text(m.name, role: .label, fill: 1.0, truncate)
            ui.Text(
                if m.service == "discord" {
                    phrase("tilolang-call-from-discord")
                } else {
                    phrase("tilolang-call-from", service: m.service)
                },
                role: .caption,
                secondary: true,
            )
        } else {
            avatar(m.who)
            ui.Text(m.who.name(), role: .label, fill: 1.0, truncate)
        }
        if m.speaking {
            ui.Text(phrase("tilolang-call-speaking"), role: .caption, secondary: true)
        }
        if m.screen {
            ui.Text(phrase("tilolang-call-sharing"), role: .caption, secondary: true)
        }
        if m.camera {
            ui.Text(phrase("tilolang-call-on-camera"), role: .caption, secondary: true)
        }
        if m.deafened {
            ui.Text(phrase("tilolang-call-deafened"), role: .caption, secondary: true)
        } else if m.muted {
            ui.Text(phrase("tilolang-call-muted"), role: .caption, secondary: true)
        }
        if not m.mine {
            stack(min: 8rem, max: 14rem) {
                ui.MenuButton(
                    phrase("tilolang-call-volume", percent: m.volume),
                    icon: if m.volume == 0 { .volume_x } else { .volume_2 },
                ) {
                    for percent in call_volumes() {
                        ui.MenuItem(
                            phrase("tilolang-call-volume", percent: percent),
                            checked: m.volume == percent,
                        ) {
                            on choose: set_call_volume(of, m.who, percent.to_float())
                        }
                    }
                }
            }
        }
    }
}

/// The microphones or the cameras a call can send from, as a menu named by the one it sends from:
/// choosing another sends from it.
view call_input_menu<R>(of: R, inputs: List<host.CallInput>, what: Text) {
    ui.MenuButton(
        if let c = inputs.find(i -> i.current) { c.label } else { what },
        icon: if inputs.any(i -> i.camera) { .video } else { .mic },
    ) {
        for i in inputs {
            ui.MenuItem(i.label, checked: i.current) {
                on choose: select_call_input(of, i.id)
            }
        }
    }
}

/// How loud a member can be played here, in percent of as they arrive.
fn call_volumes() -> List<Int> {
    [0, 25, 50, 75, 100, 150, 200]
}

/// Under a message that started a thread: how many replies it has, opening the thread.
view thread_chip<T>(thread: T, count: Int) {
    ui.Chip(phrase("tilolang-thread-replies", count: count), icon: .message_square) {
        on press: open(thread)
    }
}

/// How far the person's own change of `of` has gone, as one quiet line under it: that it has
/// reached nobody yet while no other device that keeps the workspace has taken it in (it waits on
/// this device while they are all offline), and that the workspace refused it once it did.
/// Nothing once it reached another device, nor for a change that came from one.
view reach_note<R>(of: R) {
    let state = reach(of).state
    if state == "pending" {
        ui.Text(phrase("tilolang-reach-pending"), role: .caption, secondary: true)
    } else if state == "refused" {
        ui.Text(phrase("tilolang-reach-refused"), role: .caption, secondary: true)
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
/// beside it. What they did is what its one change says (`version_said`), worked out from the
/// fields the change wrote, the person looking as "You" — or, where every change the step folds
/// is one action's and that action's words say how many, the step's own words ("wrote 7
/// messages"), so the count stands in the sentence and no count stands beside it. A step of
/// several other changes says how many ("You · 2 changes"), and opened, each says what it did.
/// What the sentence leaves out stands under it, muted (`version_notes`): each field the change
/// wrote, what it held before beside what it holds now, each as the field's type shows it.
/// Steps of one hand, within the hour, group under the face of the step that opens them; each
/// step says where it stands among the visits, so an entry reads nothing of the steps around it.
///
/// A change is taken back where a person means it. The newest step is the change the page shows
/// the result of, so it offers its undo where it stands, a small button that reads as one, a
/// run's undo taking the whole run. Every older step keeps its undo in the step's own menu: a
/// history reads as a timeline, not as a column of buttons. The step that made the resource
/// offers nothing to take back — reversing it would remove the thing whose page a person is
/// reading it on. A step of several changes is opened to each of them, each taken back on its
/// own there. One change made over many items at once — a command run on a choice of them — is
/// one step: it says how many items it changed, and its undo takes them all back together.
///
/// It is what a window shows when its bar's History is pressed, of the resource the window shows,
/// so no app places it on its pages; its heading names that resource ("History of Trip to
/// Lisbon"), since a window shows a list as readily as one item.
view history<R>(of: R) {
    state open: Text = ""
    let steps = steps(of)
    if not steps.is_empty() {
        // It stands where the page did, so it keeps the page's gutters.
        let titled = meta(of).title
        let name = if titled.trim() == "" { phrase("tilolang-untitled") } else { titled }
        ui.Scroll {
            page {
                section(phrase("tilolang-history-of", title: name)) {
                    for s in steps {
                        let key = "{s.at}/{s.who}"
                        // The step above this one: where it is the same hand's a moment before, this
                        // one joins the visit that step's face opened. Where it stands among the visits
                        // is the step's own, so an entry reads nothing of the steps around it.
                        let joined = not s.opens
                        // The newest step, whose result is what the page shows.
                        let newest = s.latest
                        // The step's first change made the resource: the step holds its making, and
                        // nothing of it is taken back from the page of the thing it made. A step's
                        // versions run oldest first, so its newest is what its sentence says.
                        let made = if let v = s.versions.first() {
                            v.kind == "created"
                        } else { false }
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
                                            // what it did, and it names them all. The step's own words
                                            // stand where they say how many changes it folds. A step of
                                            // several changes says whose they are and how many: each of
                                            // them says what it did where the step is opened, so the
                                            // heading repeats none of them.
                                            if s.run == .none {
                                                if s.said != "" {
                                                    ui.Text(s.said, role: .label)
                                                } else if s.changes > 1 {
                                                    ui.Text("·", role: .caption, secondary: true)
                                                    ui.Text(
                                                        phrase(
                                                            "tilolang-history-changes",
                                                            count: s.changes,
                                                        ),
                                                        role: .label,
                                                    )
                                                } else {
                                                    std.version_said(v)
                                                }
                                            }
                                        }
                                        ui.Text("·", role: .caption, secondary: true)
                                        ui.Time(s.at, role: .caption, secondary: true)
                                        spacer()
                                        // Taken back here, on the step the page ends with, as a button
                                        // that reads as one; in the step's menu anywhere older.
                                        if undoable {
                                            if newest {
                                                if let run = s.run {
                                                    ui.Button(
                                                        phrase("tilolang-undo"),
                                                        icon: .undo_2,
                                                        size: .small,
                                                    ) {
                                                        on press: undo(run)
                                                    }
                                                } else if let v = s.versions.last() {
                                                    ui.Button(
                                                        phrase("tilolang-undo"),
                                                        icon: .undo_2,
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
                                                            phrase("tilolang-history-revert"),
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
                                        if run.title != "" {
                                            ui.Text(run.title, role: .caption, secondary: true)
                                        }
                                    }
                                    if s.changes == 1 {
                                        if let v = s.versions.last() {
                                            std.version_notes(v, spoken: s.run == .none)
                                        }
                                    } else if s.changes > 1 {
                                        if open == key {
                                            ui.Button(
                                                phrase("tilolang-history-hide"),
                                                icon: .chevron_down,
                                                size: .small,
                                            ) {
                                                on press: set(open, "")
                                            }
                                            for v in s.versions {
                                                stack(gap: .tight) {
                                                    row(gap: .tight, align: .center, wrap: true) {
                                                        ui.Text(v.actor, role: .label)
                                                        std.version_said(v)
                                                        ui.Text(
                                                            "·",
                                                            role: .caption,
                                                            secondary: true,
                                                        )
                                                        ui.Time(
                                                            v.at,
                                                            role: .caption,
                                                            secondary: true,
                                                        )
                                                        spacer()
                                                        if v.mine and v.kind != "created" {
                                                            ui.Button(
                                                                phrase("tilolang-undo"),
                                                                icon: .undo_2,
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
                                                icon: .chevron_right,
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
        stack(gap: .tight) {
            for c in unsaid {
                // The value stands beside its field's name, one line read left to right.
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text(c.label, role: .caption, secondary: true)
                    std.value_change {
                        before: {
                            c.before
                        }
                        after: {
                            c.after
                        }
                    }
                }
            }
        }
    }
}

/// What a value held before and what it holds now, read left to right with an arrow between:
/// one change of one value, wherever a change is shown.
template view value_change(before: View, after: View) {
    row(gap: .tight, align: .center, wrap: true) {
        before
        ui.Icon(
            .arrow_right,
            size: .small,
            secondary: true,
            description: phrase("tilolang-change-to"),
        )
        after
    }
}

/// The native reactions `keys` kept, each key naming an emoji and then who reacted with it, as
/// words: each emoji and how many reacted with it, in the order they were first used. Who
/// reacted is not carried: an app names people as its own members, and the native one kept its
/// own. An app carrying a native conversation, message or page over says its reactions with
/// this rather than counting them again.
fn reactions_said(keys: List<Text>) -> Text {
    each_said(keys.map(k -> emoji_of(k)), 0, [])
}

/// [`reactions_said`] of `emoji` from the `at`th, past the emoji already `told`.
fn each_said(emoji: List<Text>, at: Int, told: List<Text>) -> Text {
    if let e = emoji.at(at) {
        if told.contains(e) {
            each_said(emoji, at + 1, told)
        } else {
            let here = "{e} {emoji.count(x -> x == e)}"
            let rest = each_said(emoji, at + 1, [told, [e]].flat_map(x -> x))
            if rest == "" { here } else { "{here}  {rest}" }
        }
    } else { "" }
}

/// The emoji a native reaction's key names: what stands before its first separator. The key
/// ends in who reacted, written in hex, after the last one.
fn emoji_of(key: Text) -> Text {
    let unhexed = without_hex(key)
    let separator = unhexed.slice(unhexed.len() - 1, unhexed.len())
    key.split(separator).first() ?? key
}

/// `key` without the hex digits it ends in.
fn without_hex(key: Text) -> Text {
    if key.len() > 0 and "0123456789abcdef".contains(key.slice(key.len() - 1, key.len())) {
        without_hex(key.slice(0, key.len() - 1))
    } else { key }
}

/// The file picker: the person chooses files, a folder, or where to save one, among the folders
/// this device holds. It reads no disk itself. The view it stands in reads the folder shown with
/// an action that uses files (`File.list`), keeps it read as it changes (`File.watch`), and gives
/// what it read as `entries`; going elsewhere asks it to read another folder (`go`), so what the
/// picker lists is always what the disk holds now, whoever changed it.
///
/// It lists the folder's entries, folders first, sorted by name, by when they changed or by size,
/// and only the types the app takes (`types`: each type's name, then its kinds as extensions,
/// `[["Images", ".png", ".jpg"], ["All files"]]`; none takes every file). The sidebar holds the
/// places (`places`: home, the usual folders under it, the workspace folders) and the recent
/// files when there are any; the path over the list leads back up, as far as the root the folder
/// is in (`roots`) and no further; `home` is this device's home folder, which it calls Home. The
/// system search narrows the folder to what it asks (`query`): the picker keeps no search of its
/// own.
///
/// What the person chooses comes back through `pick`, as `mode` asks: the files (`"open"`,
/// several with `multiple`), the folder (`"folder"`: the one chosen, or the one shown), or the
/// one path to write (`"save"`), and a name that is already a file there asks before replacing
/// it. `cancel` closes it and gives nothing. Every step is the keyboard's too: arrows move, Enter
/// opens a folder or takes a file, and the dialog that asks before replacing takes Enter and
/// Escape.
template view file_picker(
    folder: Text,
    entries: List<host.FileEntry>,
    go: (Text) -> Action,
    pick: (List<Text>) -> Action,
    cancel: Action,
    mode: Text = "open",
    multiple: Bool = false,
    types: List<List<Text>> = [],
    name: Text = "",
    accept: Text = "",
    places: List<host.FileEntry> = [],
    recent: List<host.FileEntry> = [],
    roots: List<Text> = [],
    home: Text = "",
    query: Text = "",
) {
    // What the person chose here: each row's value, a folder's ending in a slash.
    state chosen: List<Text> = []
    state at_recent: Bool = false
    state sort: Text = "Name"
    state kind: Text = types.first()?.first() ?? ""
    state typed: Text = name
    state hidden: Bool = false
    state replacing: Bool = false
    let kinds = (types.find(t -> t.first() == .some(kind)) ?? []).filter(k -> k != kind)
    let listed = if at_recent { recent } else { entries }
    let shown = picker_sorted(
        listed.filter(e -> picker_shows(e, hidden, mode == "folder", kinds, query)),
        sort,
    )
    let root = picker_root(folder, roots)
    let up = picker_parent(folder)
    let named = typed.trim()
    let target = picker_join(folder, named)
    let file_named = mode == "save" and entries.any(e -> e.name == named and not e.folder)
    let folder_named = entries.find(e -> e.name == named and e.folder)?.path ?? ""
    let files = chosen.filter(v -> not v.ends_with("/"))
    let folder_chosen = picker_unslashed(chosen.find(v -> v.ends_with("/")) ?? folder + "/")
    let ready = if mode == "save" {
        named != "" and not named.contains("/") and not at_recent
    } else if mode == "folder" { not at_recent } else { not files.is_empty() }
    let place = places
        .filter(p -> folder == p.path or folder.starts_with(p.path + "/"))
        .sort_by(p -> p.path.len(), desc)
        .first()
        ?.path
        ?? ""
    stack(gap: .related, fill) {
        row(gap: .related, align: .center) {
            ui.Button(
                "Up",
                icon: .arrow_up,
                icon_only: true,
                enabled: not at_recent and folder != root and up != "",
                description: "There is no folder above this one to go to",
            ) {
                on press: set(chosen, []).then(go(up))
            }
            if at_recent {
                ui.Text("Recent", role: .subheading)
            } else {
                ui.Breadcrumb(root_icon: .folder) {
                    for crumb in picker_crumbs(folder, root) {
                        ui.BreadcrumbSegment(picker_name(crumb, home)) {
                            on activate: set(chosen, []).then(go(crumb))
                        }
                    }
                }
            }
            spacer
            if types.len() > 1 {
                ui.Dropdown(bind: kind, options: types.map(t -> t.first() ?? ""), size: .small)
            }
            ui.Dropdown(bind: sort, options: ["Name", "Modified", "Size"], size: .small)
            ui.Button(
                "Hidden files",
                icon: if hidden { .eye } else { .eye_off },
                icon_only: true,
                toggled: hidden,
            ) {
                on press: set(hidden, not hidden)
            }
        }
        split(side: .leading, collapse: .stack) {
            secondary(min: 12rem) {
                ui.NavList(selected: if at_recent { "recent" } else { place }) {
                    if not recent.is_empty() {
                        ui.NavItem("recent", "Recent", icon: "history") {
                            on select: set(at_recent, true).then(set(chosen, []))
                        }
                    }
                    for p in places {
                        ui.NavItem(
                            p.path,
                            picker_name(p.path, home),
                            icon: picker_place_icon(p.path, home),
                        ) {
                            on select: set(at_recent, false).then(set(chosen, [])).then(go(p.path))
                        }
                    }
                }
            }
            primary(min: 18rem) {
                if shown.is_empty() {
                    ui.EmptyState(
                        if query != "" {
                            "Nothing here matches “{query}”"
                        } else if at_recent {
                            "No recent files"
                        } else if mode == "folder" {
                            "No folders here"
                        } else { "This folder is empty" },
                        icon: .folder_open,
                    )
                } else {
                    ui.VirtualList(
                        name: if at_recent { "Recent" } else { picker_name(folder, home) },
                        multi_select: multiple and mode == "open",
                        chosen_values: chosen,
                        in_column: false,
                    ) {
                        on select_value(v: Text): set(
                            typed,
                            picker_saved_name(v, typed, mode == "save"),
                        )
                        on selected_values(vs: List<Text>): set(chosen, vs)
                        for e in shown {
                            ui.ListItem(value: if e.folder { e.path + "/" } else { e.path }) {
                                ui.ListRow(
                                    title: e.name,
                                    icon: if e.folder { .folder } else { .file },
                                    meta: if e.folder { "Folder" } else { picker_size(e.size) },
                                ) {
                                    on activate: if e.folder {
                                        set(at_recent, false).then(set(chosen, [])).then(go(e.path))
                                    } else if mode == "save" {
                                        set(typed, e.name)
                                    } else if mode == "open" {
                                        pick([e.path])
                                    } else { set(chosen, []) }
                                    trailing: {
                                        ui.Time(e.modified, role: .caption, secondary: true)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        row(gap: .related, align: .center) {
            if mode == "save" {
                ui.TextInput(bind: typed, placeholder: "Name", autofocus: true, fill)
            } else if mode == "open" and multiple and files.len() > 1 {
                text("{files.len()} files chosen", role: .caption, secondary: true)
            }
            spacer
            ui.Button("Cancel") {
                on press: cancel
            }
            ui.Button(
                if accept != "" {
                    accept
                } else if mode == "save" {
                    "Save"
                } else if mode == "folder" { "Choose folder" } else { "Open" },
                role: .primary,
                enabled: ready,
                description: if at_recent {
                    "Recent files hold no folder to save in or to choose"
                } else if mode == "save" { "Name the file first" } else { "Choose a file first" },
            ) {
                on press: if mode == "save" {
                    if folder_named != "" {
                        set(typed, "").then(go(folder_named))
                    } else if file_named { set(replacing, true) } else { pick([target]) }
                } else if mode == "folder" { pick([folder_chosen]) } else { pick(files) }
            }
        }
        ui.ModalDialog(
            title: "Replace “{named}”?",
            open: replacing,
            primary: "Replace",
            secondary: "Cancel",
            destructive: true,
            icon: .triangle_alert,
        ) {
            on act_primary: set(replacing, false).then(pick([target]))
            on act_secondary: set(replacing, false)
            on dismiss: set(replacing, false)
            ui.Text(
                "{picker_name(folder, home)} already holds a file with this name. Replacing it writes over what it holds.",
            )
        }
    }
}

/// Whether the picker lists `e`: a hidden entry only with `hidden`, only folders when it picks
/// one (`folders`), a file only of `kinds`, and only what the search asks (`query`).
fn picker_shows(
    e: host.FileEntry,
    hidden: Bool,
    folders: Bool,
    kinds: List<Text>,
    query: Text,
) -> Bool {
    let kept = hidden or not e.hidden
    let fits = if folders { e.folder } else { e.folder or picker_fits(e.name, kinds) }
    let found = query == "" or e.name.lower().contains(query.lower())
    kept and fits and found
}

/// Whether a file called `name` is of one of `kinds` (extensions, with or without the dot or a
/// leading `*`); every file is when there are none, or one of them is `*`.
fn picker_fits(name: Text, kinds: List<Text>) -> Bool {
    let lower = name.lower()
    kinds.is_empty() or kinds.any(k -> picker_kind_fits(lower, k))
}

/// Whether the file called `lower` (in lower case) is of the kind `kind`.
fn picker_kind_fits(lower: Text, kind: Text) -> Bool {
    let ext = kind.lower().replace("*", "").replace(".", "")
    ext == "" or lower.ends_with("." + ext)
}

/// `entries` in the order `by` names, folders first.
fn picker_sorted(entries: List<host.FileEntry>, by: Text) -> List<host.FileEntry> {
    let folders = entries.filter(e -> e.folder)
    let files = entries.filter(e -> not e.folder)
    if by == "Modified" {
        [folders.sort_by(e -> e.modified, desc), files.sort_by(e -> e.modified, desc)]
            .flat_map(x -> x)
    } else if by == "Size" {
        [folders.sort_by(e -> e.name.lower()), files.sort_by(e -> e.size, desc)].flat_map(x -> x)
    } else {
        [folders.sort_by(e -> e.name.lower()), files.sort_by(e -> e.name.lower())].flat_map(x -> x)
    }
}

/// The root of `roots` that holds `folder`, the deepest when they nest; none when none does.
fn picker_root(folder: Text, roots: List<Text>) -> Text {
    roots
        .filter(r -> folder == r or folder.starts_with(r + "/"))
        .sort_by(r -> r.len(), desc)
        .first()
        ?? ""
}

/// The folder that holds `path`; none for the top of the disk or of home.
fn picker_parent(path: Text) -> Text {
    let parts = path.split("/")
    if parts.len() < 2 {
        ""
    } else if parts.len() == 2 and parts.first() == .some("") {
        if path == "/" { "" } else { "/" }
    } else { parts.take(parts.len() - 1).join("/") }
}

/// The path of `name` in `folder`.
fn picker_join(folder: Text, name: Text) -> Text {
    if folder.ends_with("/") { folder + name } else { folder + "/" + name }
}

/// `value` without the slash a folder's value ends in.
fn picker_unslashed(value: Text) -> Text {
    if value.ends_with("/") and value.len() > 1 { value.slice(0, value.len() - 1) } else { value }
}

/// What the path over the list holds: each folder from `root` (or the top) down to `folder`.
fn picker_crumbs(folder: Text, root: Text) -> List<Text> {
    let above = picker_parent(folder)
    if folder == root or above == "" or folder == "" {
        [folder]
    } else {
        [picker_crumbs(above, root), [folder]].flat_map(x -> x)
    }
}

/// What a path is called where the picker shows it: its last name, home as Home.
fn picker_name(path: Text, home: Text) -> Text {
    if path == "~" or path == home {
        "Home"
    } else if path == "/" {
        "Computer"
    } else {
        picker_unslashed(path).split("/").last() ?? path
    }
}

/// The name a Save stands at once the row `value` is chosen: a file's own name, so writing over
/// it is one step; a folder leaves what was typed.
fn picker_saved_name(value: Text, typed: Text, saving: Bool) -> Text {
    if saving and not value.ends_with("/") { picker_name(value, "") } else { typed }
}

/// The glyph of a place at `path`, by the usual folder it is.
fn picker_place_icon(path: Text, home: Text) -> Text {
    let name = picker_name(path, "")
    if path == "~" or path == home {
        "home"
    } else if name == "Documents" {
        "file_text"
    } else if name == "Downloads" {
        "download"
    } else if name == "Pictures" {
        "image"
    } else if name == "Music" {
        "music"
    } else if name == "Videos" { "video" } else if name == "Desktop" { "monitor" } else { "folder" }
}

/// A file's size, as a person reads it.
/// A size in bytes as a person reads it, in the decimal units disks are sold by: `999 B`,
/// `1.5 kB`, `84.2 GB`. A whole unit keeps no decimal (`1 kB`), and a tenth rounds towards the
/// nearest: what a file takes, what a folder holds, what a disk has left.
fn size(bytes: Int) -> Text {
    let n = if bytes < 0 { 0 } else { bytes }
    if n < 1000 {
        "{n} B"
    } else if n < 1000000 {
        tenths(n, 1000, "kB")
    } else if n < 1000000000 {
        tenths(n, 1000000, "MB")
    } else if n < 1000000000000 {
        tenths(n, 1000000000, "GB")
    } else { tenths(n, 1000000000000, "TB") }
}

/// `n` in units of `unit`, to a tenth when it is not whole: `1.5 kB`, `1 kB`.
fn tenths(n: Int, unit: Int, said: Text) -> Text {
    let parts = (n * 10 + unit / 2) / unit
    let whole = parts / 10
    let rest = parts - whole * 10
    if rest == 0 { "{whole} {said}" } else { "{whole}.{rest} {said}" }
}

/// A size in bytes as a limit set in mebibytes reads it: `25 MiB`, `1.5 MiB`. What a host keeps
/// is set and kept in mebibytes, so a limit and a refusal past it read in the same unit.
fn mib(bytes: Int) -> Text {
    tenths(bytes, 1048576, "MiB")
}

fn picker_size(bytes: Int) -> Text {
    if bytes < 1000 {
        "{bytes} bytes"
    } else if bytes < 1000000 {
        "{(bytes + 500) / 1000} KB"
    } else if bytes < 1000000000 {
        "{(bytes + 500000) / 1000000} MB"
    } else { "{(bytes + 500000000) / 1000000000} GB" }
}

/// Reads a JSON document's own shape.
///
/// The value at `key` of a JSON object, and nothing for anything else.
fn json_field(value: json.Value, key: Text) -> Option<json.Value> {
    match value {
        .object(fields) => fields.get(key),
        .array(items) => .none,
        .string(text) => .none,
        .number(number) => .none,
        .bool(flag) => .none,
        .null => .none,
    }
}

/// The items of the JSON list at `key`, and nothing for anything else.
fn json_list(value: json.Value, key: Text) -> List<json.Value> {
    match json_field(value, key) {
        .none => [],
        .some(held) => match held {
            .array(items) => items,
            .object(fields) => [],
            .string(text) => [],
            .number(number) => [],
            .bool(flag) => [],
            .null => [],
        },
    }
}

/// The text at `key`, and empty when it holds none.
fn json_text(value: json.Value, key: Text) -> Text {
    match json_field(value, key) {
        .none => "",
        .some(held) => json_text_plain(held),
    }
}

/// The text of a JSON value itself, and empty when it is no text.
fn json_text_plain(value: json.Value) -> Text {
    match value {
        .string(text) => text,
        .object(fields) => "",
        .array(items) => "",
        .number(number) => "",
        .bool(flag) => "",
        .null => "",
    }
}

/// The texts of the JSON list at `key`, and nothing for anything else.
fn json_texts(value: json.Value, key: Text) -> List<Text> {
    json_list(value, key).map(item -> json_text_plain(item)).filter(text -> text != "")
}

/// The number at `key` as a whole count, and zero when it holds none.
fn json_number(value: json.Value, key: Text) -> Int {
    match json_field(value, key) {
        .none => 0,
        .some(held) => match held {
            .number(number) => number.to_int(),
            .object(fields) => 0,
            .array(items) => 0,
            .string(text) => 0,
            .bool(flag) => 0,
            .null => 0,
        },
    }
}

/// The numbers of the JSON list at `key`, in order.
fn json_numbers(value: json.Value, key: Text) -> List<Int> {
    json_list(value, key)
        .map(
            item -> match item {
                .number(number) => number.to_int(),
                .object(fields) => 0,
                .array(items) => 0,
                .string(text) => 0,
                .bool(flag) => 0,
                .null => 0,
            },
        )
}

/// The flag at `key`, and false when it holds none.
fn json_flag(value: json.Value, key: Text) -> Bool {
    match json_field(value, key) {
        .none => false,
        .some(held) => match held {
            .bool(flag) => flag,
            .object(fields) => false,
            .array(items) => false,
            .string(text) => false,
            .number(number) => false,
            .null => false,
        },
    }
}
