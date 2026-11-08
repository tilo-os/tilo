app tilo.std "Standard library"

/// The window frames every tilolang app gets: a list of resources, a document, a page and a
/// transcript, each the native one. They are written over the toolkit's components with the
/// same language any app has (lenses, resource metadata, enum cases and events that carry
/// their value), so an app that wants something else composes `ui.*` itself and loses nothing
/// native.
///
/// A window that lists resources composes it: `std.list` puts the items down a column of rows
/// with what the platform does on a row, `std.places` the places the app names in the sidebar,
/// `std.header` the sort menu, `std.add_entry` the entry at the list's end, and the app writes
/// the shell around them and says where each stands. A window that is only that is `std.browse`,
/// which takes the resource its window is of, the items, the places and the sort, and writes that
/// shell the standard way.
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
/// neither gives neither.
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
    row: (T, Option<Principal>) -> View,
) {
    // The item the person opened where it stands: the row grows into it, and pressing it again,
    // Escape, or a press beside it closes it. A row without a document opens elsewhere, in the
    // window's pane, so the list keeps nothing of it.
    state open_item: Option<T> = .none
    ui.VirtualList(
        selection_follows_focus: true,
        multi_select: true,
        in_column: true,
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

/// The entry at a list's end: what is typed and Entered there — or left there with — makes an
/// item of it, through `create`, an action whose one `Text` parameter takes the words nothing
/// else does. What the words say of the action's other parameters ("Call the landlord fri 10am
/// @Bo") fills them: the entry shows each as a chip under what is typed, and removing a chip
/// takes its words away. With no words of its own — nothing typed, space, or only what the
/// other parameters take — the entry makes nothing: Enter does nothing and leaves the text
/// there, and Escape leaves none. The host gives it the window's primary action's key, so the
/// key and the bar's button come here.
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
/// with all of them called `everything`; the sort menu when it gives `sort`, each case asked of
/// the order as `by` writes it (`"title"`, `"-due"`, `""` for the items as they are); the entry
/// at the list's end when it gives `entry`; and nothing to list when the place chosen finds
/// nothing, where it says `empty`. The window owns the query — the place a person chose and the
/// order they chose are its state, kept for them alone — and the list is that one query, the
/// title over it and its count naming that place.
///
/// The rows open beside the list when their type has a window of its own (`opens`, the type's own
/// fact): choosing one opens it in the column beside the list, and the window says beside them
/// that nothing is chosen. A window whose rows open in front of it gives `opens: false` instead:
/// the list is then its content, under its own title, with no column to open beside it.
///
/// `row` draws one item's row and the window puts it down the list, with who else is at the item
/// as every list shows it. The row the person opened is the list's own, unless the app keeps one
/// (`opened`) to open a row from elsewhere. A window that needs more — grouped or nested rows, a
/// sidebar of its own — composes `std.list`, `std.places` and `std.header` itself.
template view browse<R, T, S>(
    of: R,
    items: List<T>,
    opened: Option<State<Option<T>>> = .none,
    places: List<Filter<T>> = [],
    sort: Option<State<S>> = .none,
    by: Option<(S) -> Text> = .none,
    everything: Text = phrase("tilolang-all", kind: meta(T).plural),
    empty: Text = phrase("tilolang-empty-kind", kind: meta(T).plural),
    entry: Option<Action> = .none,
    entry_placeholder: Text = phrase("tilolang-add"),
    opens: Bool = meta(T).opens,
    row: (T) -> View,
) {
    state query: Text = ""
    let kind = meta(T)
    let order = if let s = sort { if let ask = by { ask(s) } else { "" } } else { "" }
    let shown = items.matching(if order == "" { query } else { query + " sort:" + order })
    let title = places.find(p -> p.query == query)?.label ?? everything
    let sidebar_title: Option<Text> = if places.is_empty() { .none } else { .some(meta(of).title) }
    if opens {
        ui.NavShell(
            content_kind: .document,
            sidebar_title: sidebar_title,
            overview_title: .some(title),
            overview_count: .some(shown.len()),
        ) {
            sidebar: {
                if not places.is_empty() {
                    std.places(query, places, everything, icon: kind.icon)
                }
            }
            overview_actions: {
                std.header(sort: sort)
            }
            overview: {
                browse_rows(shown, opened, empty, kind.icon, entry, entry_placeholder) { x ->
                    row(x)
                }
            }
            content: {
                ui.EmptyState(phrase("tilolang-none-selected", kind: kind.noun), icon: kind.icon)
            }
        }
    } else {
        ui.NavShell(
            content_kind: .list,
            title: .some(title),
            count: .some(shown.len()),
            sidebar_title: sidebar_title,
        ) {
            sidebar: {
                if not places.is_empty() {
                    std.places(query, places, everything, icon: kind.icon)
                }
            }
            actions: {
                std.header(sort: sort)
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
/// and `Shift-Tab`, the accessibility move actions — is reported with the item it lands under
/// and the item it lands before, and `place` puts it in the one place they name:
/// `(moved, .none, .none)` is the end of the top level, `(moved, .some(x), .none)` the end of
/// `x`'s own, and `(moved, .none, .some(x))` where `x` stands. One step, so two people moving
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
        ui.TreeView(
            name: .some(name),
            fill: fill,
            reorder: true,
            tab_indents: true,
            hide_root: true,
        ) {
            on drop(moved: T): place(moved, .none, .none)
            for x in items {
                // The node just above this one under the same parent, as the outline shows
                // them: the place the actions offered on it get. None for a first child.
                let above = items
                    .take(items.index_of(x) ?? 0)
                    .filter(y -> parent(y) == parent(x))
                    .last()
                let count: Option<Text> = if let c = progress { c(x) } else { .none }
                ui.TreeViewNode(x, label(x), parent: parent(x), above: above, progress: count) {
                    on drop(moved: T): place(moved, .none, .some(x))
                    on into(moved: T): place(moved, .some(x), .none)
                    on check: toggled(x)
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
/// that toggles a state of the window's resource as a control of its own, and all the rest
/// behind the bar's one menu, so every action stays reachable at every width without the view
/// having read them.
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

/// One of an app's places on another app's page: a card that stands for `of`, names the place
/// as its app calls it (`name`), says how many items it holds, and shows the first of them as
/// their own rows (`row`, the type's row view, as everywhere else). The items are the place's,
/// not a filter rewritten here: an app that declares its place as a filter (`Task.today`) gives
/// its words and what it holds (`items.matching(Task.today)`), and one whose place no filter
/// asks, such as the conversations with anything left unread, writes a view of its own over
/// this. A card is a summary, so a list longer than the rows it shows ends with the way into
/// the rest: `Show all`, which opens `of`, as pressing the card does. The count is how many the
/// place holds, so a card never says more than it shows.
///
/// A place with nothing in it takes no room on the page: one quiet line says it is caught up,
/// which is what a morning's page is for. Nothing else of it stands there — no count of none,
/// no empty list.
template view place<R, T>(of: R, name: Text, items: List<T>, row: (T) -> View) {
    let n = items.count()
    ui.Card(item: of, title: name, count: if n == 0 { .none } else { .some(n) }) {
        if n == 0 {
            ui.Text(phrase("tilolang-all-caught-up"), secondary: true)
        } else {
            list(items) { x ->
                row(x)
            }
            if n > preview_rows() {
                ui.Button("Show all {n}") {
                    on press: open(of)
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
                ui.VirtualList(follow_end: true, unseen_from: unread) {
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

/// The foot of a conversation: the field a reply is typed into, and the button that sends it.
/// Enter or the button runs `send`, then clears the field. The field takes everything the button
/// leaves and ends at its edge, so the two read as one control at any width. What a view says
/// over it (the message being answered, who is typing, what is being edited) is its own to place.
view composer(draft: State<RichText>, placeholder: Text, send: Action) {
    row(gap: .none) {
        rich_input(bind: draft, placeholder: placeholder, fill) {
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
        let glyph: Option<Text> = if let i = kind.icon { .some(i) } else { .some("link") }
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
        let glyph: Option<Text> = if let i = kind.icon { .some(i) } else { .some("link") }
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
/// under the face of the step that opens them. Undo ends the step that offers it, as quiet as
/// the words around it, a run's undo taking the whole run; each change of theirs in a step of
/// several is undone on its own, where the step opens. One change made over many items at
/// once — a command run on a choice of them — is one step: it says how many items it changed,
/// and its Undo takes them all back together.
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
                            // name it opens with — an undo's own word is as long as its language
                            // says — and what can be undone is offered at its end, quietly.
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
                                if let run = s.run {
                                    ui.Button(
                                        phrase("tilolang-history-undo-run"),
                                        role: .plain,
                                        size: .small,
                                    ) {
                                        on press: undo(run)
                                    }
                                } else if s.changes == 1 and s.mine {
                                    if let v = s.versions.last() {
                                        ui.Button(
                                            phrase("tilolang-undo"),
                                            role: .plain,
                                            size: .small,
                                        ) {
                                            on press: undo(v)
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
                                                if v.mine {
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
