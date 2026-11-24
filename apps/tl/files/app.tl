app so.tilo.file-explorer "Files" (
    icon: .folder,
    category: .utility,
    tagline: "Browse your files",
    keywords: ["files", "file manager", "folder", "folders", "browse", "explorer", "documents", "downloads", "trash", "dateien", "ordner", "durchsuchen"],
)

use so.tilo.device as device
use so.tilo.session as session

/// How a folder's entries read.
enum SortBy {
    /// By what they are called.
    name
    /// By how much they hold.
    size
    /// By when they last changed.
    modified
    /// By the kind of file they are.
    format
}

/// One entry of a folder: what its disk shows, kept where an action read it, since a view is
/// drawn again from what it is shown and never reads a disk itself.
type Entry {
    /// What it is called, as its folder shows it.
    name: Text = ""
    /// The path that reaches it.
    path: Text = ""
    /// Whether it is a folder.
    folder: Bool = false
    /// Its size in bytes; 0 for a folder.
    size: Int = 0
    /// When its content last changed.
    modified: Time
    /// Whether its name starts with a dot.
    hidden: Bool = false
    /// Where it stood before it was trashed; empty for one that never was.
    original: Text = ""
}

/// A folder as one action read it: its path, what it held and what went wrong, so the window
/// draws a listing a keystroke old at worst and says the reason a folder did not open.
type Listing {
    /// The place it lists: a path, or `trash` for this device's trash.
    path: Text = ""
    entries: List<Entry>
    /// Why it could not be read; empty when it was.
    error: Text = ""
    /// Whether an action has read it at all.
    read: Bool = false
    /// The watch this window keeps on the folder, so a change on the disk shows at once.
    watch: Text = ""
}

/// A place the sidebar offers, kept as a resource so the system's search finds it by name too.
type Place (title: label) {
    /// What the place is called.
    label: Text = ""
    /// Where it goes: a path, `~`, or `trash`, `recent` or `applications`.
    path: Text = ""
    /// The heading over it in the sidebar.
    group: Text = "Places"
}

/// A place the person marked in the sidebar.
type Favorite (title: place) {
    place: Text
}

/// A file recently opened, so it is one press away again.
type Recent (title: name) {
    name: Text = ""
    path: Text = ""
    opened: Time
}

/// The preview made for the picture at `path`, kept once so the window draws it again without
/// asking the device each time; the file is none while the device says it has no preview, and
/// `modified` is when the picture changed as it did when the preview was made, so one made
/// before the picture changed is made again.
type Preview (title: path) {
    path: Text = ""
    modified: Time
    file: Option<File> = .none
}

/// A folder an entry can be put in, offered by the rows that move, copy and extract into one.
type Destination (title: name) {
    name: Text = ""
    path: Text = ""
}

/// A copy or a move the device carries out as its own work, so a large one says how far it has
/// got and can be stopped: what it is doing, in the person's words, and how it stands.
type Transfer (title: label) {
    /// The device's job; empty once it ended.
    job: Text = ""
    /// What it does: "Copying 3 items to “Pictures”".
    label: Text = ""
    /// Bytes written, of how many.
    done: Int = 0
    total: Int = 0
    /// The entry it is on.
    current: Text = ""
    /// Why it stopped short; empty while it runs or once it finished.
    failed: Text = ""
}

/// What a file's header says it is: a picture's size, how long a recording plays, how many pages
/// a document has, and its codec and container; `0` or empty where it says nothing.
type FileFacts {
    width: Int = 0
    height: Int = 0
    /// How long it plays, in milliseconds.
    duration: Int = 0
    pages: Int = 0
    codec: Text = ""
    container: Text = ""
}

/// What a transfer asks of the device: the entries at `paths` put into `folder`.
type TransferAsk {
    paths: List<Text>
    folder: Text
    moving: Bool
}

/// How far a transfer has got, as the device records it.
type TransferState {
    done: Int
    total: Int
    current: Text
}

/// One person's window on this device's files: where they are, how they look, what they marked,
/// what they carry, and the folders this window has read. It is the reader's own, so two people
/// at one workspace never turn each other's pages.
type Session (readers: author) {
    /// The place shown: a path, `~` for the home folder, or `trash`, `recent` or `applications`.
    here: Text = "~"
    sort: SortBy = .name
    ascending: Bool = true
    /// Whether names starting with a dot show.
    hidden: Bool = false
    /// Whether the listing draws as a grid.
    grid: Bool = false
    /// How large the entries stand: 0 smaller, 1 regular, 2 larger.
    zoom: Int = 1
    /// The places gone before and put away, so back and forward walk the way the person came.
    went: List<Text>
    ahead: List<Text>
    /// What the person copied or cut here, and whether a paste moves it.
    carrying: List<Text>
    cutting: Bool = false
    /// The folders this window has read.
    listings: Collection<Listing>
    /// The previews it has drawn of the pictures it listed, by the path of each.
    previews: Collection<Preview>
    /// The picture a chosen file shows, when it is one, and the text a chosen file holds.
    preview: Option<File> = .none
    previewed: Text = ""
    words: Text = ""
    /// What the chosen file's own header says it is, as the device read it.
    facts: Option<FileFacts> = .none
    /// The entries the person chose, by their paths, the entry the inspector shows last among
    /// them; empty while nothing is chosen.
    chosen: List<Text>
    /// The entry the inspector shows, by its path; empty while nothing is chosen.
    selected: Text = ""
    /// The templates the person keeps in `~/Templates`, offered when a new file is named.
    templates: List<Text>
    /// The path of the entry the person asked to open with, and the apps that open it.
    opening: Text = ""
    openers: List<host.DocumentApp>
    /// The last thing that went wrong, for the window to say; empty while nothing did.
    notice: Text = ""
    /// The copies and moves under way from this window, and the ones that stopped short.
    transfers: Collection<Transfer>
}

/// The workspace's Files: one window per person, the places they marked, and what they opened.
type Files {
    sessions: Collection<Session>
    places: Collection<Place>
    favorites: Collection<Favorite>
    recent: Collection<Recent>
}

root files: Files

default view files for Files

/// A new Files starts with the places its sidebar offers.
initial action standard_places(f: Files) {
    f.places.insert(Place(label: "Home", path: "~"))
    f.places.insert(Place(label: "Recent", path: "recent"))
    f.places.insert(Place(label: "Applications", path: "applications", group: "Places"))
    f.places.insert(Place(label: "Desktop", path: "~/Desktop", group: "Folders"))
    f.places.insert(Place(label: "Documents", path: "~/Documents", group: "Folders"))
    f.places.insert(Place(label: "Downloads", path: "~/Downloads", group: "Folders"))
    f.places.insert(Place(label: "Pictures", path: "~/Pictures", group: "Folders"))
    f.places.insert(Place(label: "Music", path: "~/Music", group: "Folders"))
    f.places.insert(Place(label: "Videos", path: "~/Videos", group: "Folders"))
    f.places.insert(Place(label: "Remote", path: "~/Remote", group: "This computer"))
    f.places.insert(Place(label: "Root", path: "/", group: "This computer"))
    f.places.insert(Place(label: "Trash", path: "trash", group: "This computer"))
}

/// Opening Files reads the folder the person is in, unless this window has read it already.
opened action arrive(f: Files) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if s.listings.find(l -> l.path == s.here) == .none { read_listing(f, s.here) }
}

/// Reads the place `place` into this window's listing for it: a folder's entries, this device's
/// trash, or nothing for the places this app keeps itself.
action read_listing(f: Files, place: Text = "") uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let path = if place == "" { s.here } else { place }
    if path == "recent" or path == "applications" {
        return
    }
    let listing = s.listings.find(l -> l.path == path) ?? s.listings.insert(Listing(path: path))
    if path != "trash" and listing.watch == "" {
        match File.watch(path, "changed") {
            .ok(token) => {
                listing.watch = token
            }
            .err(_) => {}
        }
    }
    if path == "trash" {
        match File.trash_entries() {
            .ok(items) => {
                listing.entries = items.map(
                    t -> Entry(
                        name: t.name,
                        path: t.path,
                        folder: false,
                        size: 0,
                        modified: t.deleted,
                        original: t.original,
                    ),
                )
                listing.error = ""
                listing.read = true
            }
            .err(why) => {
                listing.error = why
            }
        }
        return
    }
    match File.list(path) {
        .ok(found) => {
            listing.entries = found.map(
                e -> Entry(
                    name: e.name,
                    path: e.path,
                    folder: e.folder,
                    size: e.size,
                    modified: e.modified,
                    hidden: e.hidden,
                ),
            )
            listing.error = ""
            listing.read = true
        }
        .err(why) => {
            listing.error = why
        }
    }
}

/// Runs when a watched folder changes: reads the folder shown again.
action changed(f: Files) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    read_listing(f, s.here)
}

/// Makes the previews of a few pictures in the folder shown that have none yet, as the window
/// shows it: a folder of photographs fills with their pictures while the person looks at it,
/// without the window waiting for every one when the folder is read.
action preview_step(f: Files) uses files, image {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if let l = place_listing(s) {
        let waiting = l.entries.filter(
            e -> not e.folder and is_picture(e.name) and not preview_fresh(s, e),
        )
        for e in waiting.take(6) {
            let made = File.preview(e.path, 512)
            if let held = s.previews.find(p -> p.path == e.path) {
                held.modified = e.modified
                match made {
                    .ok(found) => {
                        held.file = found
                    }
                    .err(_) => {
                        held.file = .none
                    }
                }
            } else {
                match made {
                    .ok(found) => {
                        s.previews.insert(Preview(path: e.path, modified: e.modified, file: found))
                    }
                    .err(_) => {
                        s.previews.insert(Preview(path: e.path, modified: e.modified))
                    }
                }
            }
        }
    }
}

/// The preview drawn for the picture at `path`, or none while it has not been made.
fn preview_of(s: Session, path: Text) -> Option<Preview> {
    s.previews.find(p -> p.path == path)
}

/// Whether the preview drawn for the entry `e` was made from the picture as it stands now.
fn preview_fresh(s: Session, e: Entry) -> Bool {
    if let p = preview_of(s, e.path) { p.modified == e.modified } else { false }
}

/// Whether the folder shown holds a picture with no preview of it as it stands now.
fn previews_waiting(s: Session) -> Bool {
    if let l = place_listing(s) {
        l.entries.any(e -> not e.folder and is_picture(e.name) and not preview_fresh(s, e))
    } else { false }
}

/// Goes to the place `place`, reading it: a folder, the trash, the recent files or the apps.
action go(f: Files, place: Text) uses files, time {
    require place != "" "A place is named by its path"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if s.here != place {
        s.went = s.went + [s.here]
        s.ahead = []
    }
    s.here = place
    s.selected = ""
    s.chosen = []
    s.opening = ""
    s.openers = []
    s.preview = .none
    s.previewed = ""
    s.words = ""
    read_listing(f, place)
}

/// Goes back to the place the person came from.
action back(f: Files) uses files, time {
    require can_go_back(f) "There is nowhere to go back to"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if let last = s.went.last() {
        s.went = s.went.take(s.went.len() - 1)
        s.ahead = s.ahead + [s.here]
        s.here = last
        read_listing(f, last)
    }
}

/// Goes forward again to the place the person left.
action forward(f: Files) uses files, time {
    require can_go_forward(f) "There is nowhere to go forward to"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if let next = s.ahead.last() {
        s.ahead = s.ahead.take(s.ahead.len() - 1)
        s.went = s.went + [s.here]
        s.here = next
        read_listing(f, next)
    }
}

/// Goes to the folder above the one shown.
action up(f: Files) uses files, time {
    require not at_top(f) "This is already the top folder"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    go(f, if folder_place(s.here) { parent_of(s.here) } else { "~" })
}

/// Shows `path` in this window: the folder itself when it is one, and the folder that holds it with
/// the entry chosen when it is a file. It is how another app sends a person to a file it made, and
/// how an agent or a link brings them to one, so the person reads where it stands in its folder.
action show(f: Files, path: Text) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if path == "" {
        go(f, "~")
        return
    }
    match File.stat(path) {
        .err(why) => {
            s.notice = "Couldn’t look at “{name_of(path)}”: {why}"
        }
        .ok(found) => {
            if let e = found {
                if e.folder {
                    go(f, e.path)
                } else {
                    let folder = parent_of(e.path)
                    if s.here != folder {
                        s.went = s.went + [s.here]
                        s.ahead = []
                    }
                    s.here = folder
                    s.selected = ""
                    s.chosen = []
                    s.opening = ""
                    s.openers = []
                    s.preview = .none
                    s.previewed = ""
                    s.words = ""
                    read_listing(f, folder)
                    if let picked = place_entry(f, e.path) {
                        s.chosen = [e.path]
                        s.selected = e.path
                        inspect(f, picked.path)
                    }
                }
            } else {
                s.notice = "“{name_of(path)}” is not here anymore"
            }
        }
    }
}

/// Extracts the archive at `path` into `folder`, into a folder named after it there: what the
/// rows that offer a folder to extract into ask.
action extract_into(f: Files, path: Text, folder: Text) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let to = free_name(f, folder, stem_of(name_of(path)))
    match File.extract(path, join(folder, to)) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t extract “{name_of(path)}”: {why}"
        }
    }
}

/// Moves the entries the person chose into `folder`, keeping a taken name both.
action move_into(f: Files, folder: Text) uses files, time {
    put_into(f, acted_on(f), folder, true)
}

/// Moves the entries the person dragged onto the folder `folder`, by their `paths`, as "Move to…"
/// does, keeping a taken name both. A folder let go on itself, inside itself or on the folder its
/// entries already stand in moves nothing.
action drop_into(f: Files, paths: List<Text>, folder: Text) uses files, time {
    if paths.is_empty()
        or paths.any(p -> p == folder or folder.starts_with(p + "/") or parent_of(p) == folder) {
        return
    }
    put_into(f, paths, folder, true)
}

/// Copies the entries the person chose into `folder`, keeping a taken name both.
action copy_into(f: Files, folder: Text) uses files, time {
    put_into(f, acted_on(f), folder, false)
}

/// Puts the entries at `paths` into `folder`, moving them or copying them, as the device's own work:
/// the window shows how far it has got and can stop it. What arrives under a name the folder
/// already holds lands as “name (2)”, so nothing is replaced.
action put_into(f: Files, paths: List<Text>, folder: Text, moving: Bool) uses files, time {
    require not paths.is_empty() "Choose a file first"
    require folder_place(folder) "Choose a folder to put them in"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let what = if paths.len() == 1 {
        "“{name_of(paths.first() ?? "")}”"
    } else { "{paths.len()} items" }
    let label = if moving {
        "Moving {what} to “{name_of(folder)}”"
    } else {
        "Copying {what} to “{name_of(folder)}”"
    }
    let t = s.transfers.insert(Transfer(label: label))
    match engine.start(
        "transfer",
        json.encode(TransferAsk(paths: paths, folder: folder, moving: moving)),
        done: j -> transfer_ended(f, t, j),
        progress: j -> transfer_moved(t, j),
    ) {
        .err(why) => {
            s.transfers.remove(t)
            s.notice = "Couldn’t put them there: {why}"
        }
        .ok(job) => {
            t.job = job
        }
    }
}

/// How far the transfer `t` has got, as the device records each move.
action transfer_moved(t: Transfer, j: host.Job) {
    match json.decode<TransferState>(j.detail) {
        .ok(state) => {
            t.done = state.done
            t.total = state.total
            t.current = state.current
        }
        .err(_) => {}
    }
}

/// The transfer `t` ended: one that finished leaves the window and the folder is read again; one
/// that failed stays, saying why, until it is dismissed.
action transfer_ended(f: Files, t: Transfer, j: host.Job) uses files, time {
    transfer_moved(t, j)
    t.job = ""
    if j.state == .failed {
        t.failed = j.error
    }
    if let s = f.sessions.first() {
        read_listing(f, s.here)
        if j.state != .failed { s.transfers.remove(t) }
    }
}

/// Stops the transfer `transfer`: the entry it was on is taken away, and what it finished stays.
action stop_transfer(f: Files, transfer: Transfer) uses files, time (icon: .x) {
    require transfer.job != "" "It has already stopped"
    engine.cancel(transfer.job)
}

/// Takes a transfer that stopped short out of the window.
action dismiss_transfer(f: Files, transfer: Transfer) {
    if let s = f.sessions.first() { s.transfers.remove(transfer) }
}

/// The files a drag of `e` carries: every chosen entry when `e` is one of them, else `e` alone.
fn dragged(s: Session, e: Entry) -> List<Text> {
    if s.chosen.contains(e.path) { s.chosen } else { [e.path] }
}

/// The folders a "Move to…", "Copy to…" or "Extract to…" row offers: the folders beside the
/// entries, the places the sidebar knows, and the person's favorites.
fn destinations(f: Files, s: Session) -> List<Destination> {
    let beside = if let l = place_listing(s) {
        l.entries.filter(e -> e.folder).map(e -> Destination(name: e.name, path: e.path))
    } else { [] }
    let places = f.places
        .filter(p -> folder_place(p.path) and p.path != s.here and p.path != "/")
        .map(p -> Destination(name: p.label, path: p.path))
    let favorites = f.favorites.map(p -> Destination(name: p.place, path: p.place))
    beside + places + favorites
}

/// Sends the entries the person chose to the nearby device `nearby`, as Nearby Send offers it.
action send_nearby(f: Files, nearby: Text) uses files {
    require not acted_on(f).is_empty() "Choose a file to send first"
    let sending = acted_on(f)
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    device.send(device.nearby_send, items: sending, device: nearby)
    s.notice = ""
}

/// Dismisses what the window last said went wrong.
action dismiss_notice(f: Files) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.notice = ""
}

/// Reads the place shown again, so what changed on the disk shows.
action refresh(f: Files) uses files, time (icon: .refresh, key: "Ctrl+R") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    read_listing(f, s.here)
}

/// Sorts the folder shown by `by`, the way `ascending` says.
action sort_by(f: Files, by: SortBy) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.sort = by
}

/// Sorts the folder shown oldest or smallest first, or the other way.
action ascending(f: Files, first: Bool) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.ascending = first
}

/// Draws the folder as a grid of cards, or as rows.
action show_grid(f: Files, grid: Bool) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.grid = grid
}

/// Draws the folder the way the segment at place `i` says: the list at 0, the grid at 1.
action choose_view(f: Files, i: Int) {
    show_grid(f, i == 1)
}

/// Shows or hides the names that start with a dot.
action toggle_hidden(f: Files, hidden: Bool) (icon: .eye, label: "Show hidden files") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.hidden = hidden
}

/// Draws the entries at `step`: 0 smaller, 1 regular, 2 larger, as the two keys step it.
action set_zoom(f: Files, step: Int) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.zoom = step.max(0).min(2)
}

/// Draws the entries one step larger.
action zoom_in(f: Files) {
    if let s = f.sessions.first() { set_zoom(f, s.zoom + 1) }
}

/// Draws the entries one step smaller.
action zoom_out(f: Files) {
    if let s = f.sessions.first() { set_zoom(f, s.zoom - 1) }
}

/// Opens the entry `e`: a folder in this window, a file in the app that opens its kind.
action open_entry(f: Files, path: Text) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let found = match File.stat(path) {
        .ok(entry) => entry,
        .err(why) => {
            s.notice = "Couldn’t open “{name_of(path)}”: {why}"
            return
        }
    }
    if let e = found {
        if e.folder {
            go(f, e.path)
        } else if is_archive(e.name) {
            s.notice = "“{e.name}” is an archive: extract it to read what it holds"
        } else {
            match File.open(e.path) {
                .ok => { note_recent(f, e.name, e.path) }
                .err(why) => {
                    s.notice = "Nothing here opens “{e.name}”: {why}"
                }
            }
        }
    }
}

/// Finds the apps that open the entry at `path`, for the person to choose one.
action find_openers(f: Files, path: Text) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match tool.document_apps(path) {
        .ok(apps) => {
            s.openers = apps
            s.opening = path
        }
        .err(why) => {
            s.notice = "Couldn’t see which apps open “{name_of(path)}”: {why}"
        }
    }
}

/// Opens the entry the person asked to open with in `chosen`, one of the apps offered.
action open_with(f: Files, chosen: Text) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if s.opening == "" {
        return
    }
    match tool.open_with(s.opening, chosen) {
        .ok => {
            s.opening = ""
            s.openers = []
        }
        .err(why) => {
            s.notice = "Couldn’t open it: {why}"
        }
    }
}

/// Puts away the apps offered for the entry the person asked to open with.
action close_openers(f: Files) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.opening = ""
    s.openers = []
}

/// Opens a terminal in the folder at `folder`.
action open_terminal(f: Files, folder: Text) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match tool.terminal(folder) {
        .ok => {}
        .err(why) => {
            s.notice = "Couldn’t open a terminal here: {why}"
        }
    }
}

/// Packs the entry at `path` into a new archive beside it, named after it: `zip` picks a ZIP,
/// and TAR.ZST otherwise.
action compress_entry(f: Files, path: Text, zip: Bool) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let ext = if zip { ".zip" } else { ".tar.zst" }
    let to = join(parent_of(path), name_of(path) + ext)
    match File.compress([path], to) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t compress “{name_of(path)}”: {why}"
        }
    }
}

/// Extracts the archive at `path` into a new folder beside it, named after it.
action extract_entry(f: Files, path: Text) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let to = join(parent_of(path), stem_of(name_of(path)))
    match File.extract(path, to) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t extract “{name_of(path)}”: {why}"
        }
    }
}

/// A press on the entry at `path`: a folder opens as a person presses it once, and a file is
/// read into the inspector beside the list.
action touch_entry(f: Files, path: Text) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let found = match File.stat(path) {
        .ok(entry) => entry,
        .err(_) => .none,
    }
    s.selected = path
    if let e = found {
        if e.folder {
            go(f, e.path)
        } else {
            s.chosen = [e.path]
            inspect(f, e.path)
        }
    }
}

/// The entries the list reports as chosen, in the order they stand there.
action choose(f: Files, paths: List<Text>) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.chosen = paths
    s.selected = paths.last() ?? ""
    if let last = paths.last() { inspect(f, last) }
}

/// Chooses every entry the folder shown places: what a press on "Select all" does.
action select_all(f: Files) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if let l = place_listing(s) {
        s.chosen = l.entries.filter(e -> not e.hidden or s.hidden).map(e -> e.path)
        s.selected = s.chosen.last() ?? ""
    }
}

/// Chooses nothing.
action clear_selection(f: Files) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.chosen = []
    s.selected = ""
}

/// The entry at `path` in the folder shown, when that is the place it lists.
fn place_entry(f: Files, path: Text) -> Option<Entry> {
    if let s = f.sessions.first() {
        if let l = place_listing(s) { l.entries.find(e -> e.path == path) } else { .none }
    } else { .none }
}

/// The entries an action runs on: what the person chose, or the one the inspector shows when the
/// list holds no selection of its own.
fn acted_on(f: Files) -> List<Text> {
    if let s = f.sessions.first() {
        if not s.chosen.is_empty() {
            s.chosen
        } else if s.selected != "" { [s.selected] } else { [] }
    } else { [] }
}

/// Notes that the file `path` was opened, so it stands under Recent.
action note_recent(f: Files, name: Text, path: Text) uses time {
    if f.recent.any(r -> r.path == path) {
        if let held = f.recent.find(r -> r.path == path) {
            held.opened = now
            held.name = name
        }
    } else {
        f.recent.insert(Recent(name: name, path: path, opened: now))
    }
    if f.recent.len() > 60 {
        if let oldest = f.recent.sort_by(r -> r.opened).first() { f.recent.remove(oldest) }
    }
}

/// Makes a folder named `name` in the place shown.
action new_folder(f: Files, name: Text = "New folder") uses files, time (icon: .folder_plus) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if not folder_place(s.here) {
        s.notice = "Open a folder to make a folder in it"
        return
    }
    let named = if name.trim() == "" { "New folder" } else { name.trim() }
    if named.contains("/") {
        s.notice = "A name cannot hold a slash"
        return
    }
    match File.stat(join(s.here, named)) {
        .ok(found) => {
            if found != .none {
                s.notice = "“{named}” is already here"
                return
            }
        }
        .err(_) => {}
    }
    match File.mkdir(join(s.here, named)) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t make the folder: {why}"
        }
    }
}

/// Reads the person's `~/Templates`, so naming a new file can offer them.
action start_new_file(f: Files) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.list("~/Templates") {
        .ok(found) => {
            s.templates = found.filter(e -> not e.folder and not e.hidden).map(e -> e.name)
        }
        .err(_) => {
            s.templates = []
        }
    }
}

/// Copies the template `template` into the place shown under `typed`'s name, or the template's.
action new_from_template(f: Files, template: Text, typed: Text) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if not folder_place(s.here) {
        s.notice = "Open a folder to make a file in it"
        return
    }
    let named = if typed.trim() == "" { template } else { typed.trim() }
    if named.contains("/") {
        s.notice = "A name cannot hold a slash"
        return
    }
    match File.read(join("~/Templates", template)) {
        .ok(bytes) => {
            match File.write(join(s.here, named), bytes) {
                .ok(_) => {
                    read_listing(f, s.here)
                    note_recent(f, named, join(s.here, named))
                }
                .err(why) => {
                    s.notice = "Couldn’t make the file: {why}"
                }
            }
        }
        .err(why) => {
            s.notice = "Couldn’t read the template: {why}"
        }
    }
}

/// Makes a text file named `name` in the place shown, for the person to fill.
action new_file(f: Files, name: Text = "New file.txt") uses files, time (icon: .file_plus) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if not folder_place(s.here) {
        s.notice = "Open a folder to make a file in it"
        return
    }
    let named = if name.trim() == "" { "New file.txt" } else { name.trim() }
    if named.contains("/") {
        s.notice = "A name cannot hold a slash"
        return
    }
    match File.stat(join(s.here, named)) {
        .ok(found) => {
            if found != .none {
                s.notice = "“{named}” is already here"
                return
            }
        }
        .err(_) => {}
    }
    match File.write(join(s.here, named), Bytes.from_base64("")) {
        .ok(_) => {
            read_listing(f, s.here)
            note_recent(f, named, join(s.here, named))
        }
        .err(why) => {
            s.notice = "Couldn’t make the file: {why}"
        }
    }
}

/// Writes the files a person dropped on the window into the folder shown, each under its own
/// name; a name the folder already holds is kept both, so nothing is replaced.
action import_dropped(f: Files, files: List<File>) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if not folder_place(s.here) {
        s.notice = "Open a folder to drop files into it"
        return
    }
    for file in files {
        let name = free_name(f, s.here, file.name)
        match file.bytes() {
            .ok(bytes) => {
                match File.write(join(s.here, name), bytes) {
                    .ok(_) => {}
                    .err(why) => {
                        s.notice = "Couldn’t copy “{file.name}”: {why}"
                    }
                }
            }
            .err(why) => {
                s.notice = "Couldn’t read “{file.name}”: {why}"
            }
        }
    }
    read_listing(f, s.here)
}

/// Opens the drive `drive` and goes to where it is open, so a removable disk is a place like any
/// other folder. A drive that still has to open shows as such until the device says where it is.
action open_drive(f: Files, drive_id: Text) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let found = device.drives.drives.find(d -> d.id == drive_id)
    if let drive = found {
        if let at = drive.mounted {
            go(f, at.path)
        } else {
            device.mount(
                device.drives,
                drive: drive.id,
                safely: drive.preselect == .open_safely,
                remember: false,
            )
            s.notice = "Opening “{drive.label}”…"
        }
    }
}

/// Renames the entry at `path` to `name` in its own folder.
action rename_entry(f: Files, path: Text, name: Text) uses files, time (icon: .pencil) {
    require name.trim() != "" "A name cannot be empty"
    require not name.trim().contains("/") "A name cannot hold a slash"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if name.trim() != name_of(path) {
        match File.stat(join(parent_of(path), name.trim())) {
            .ok(found) => {
                if found != .none {
                    s.notice = "“{name.trim()}” is already here"
                    return
                }
            }
            .err(_) => {}
        }
    }
    match File.rename(path, name.trim()) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t rename it: {why}"
        }
    }
}

/// Moves the entries at `paths` to this device's trash.
action trash_entries(f: Files, paths: List<Text>) uses files, time (
    icon: .trash_2,
    label: "Move to Trash",
    key: "Delete",
) {
    require not paths.is_empty() "Select files first"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    for path in paths {
        match File.trash(path) {
            .ok(_) => {}
            .err(why) => {
                s.notice = "Couldn’t move “{name_of(path)}” to Trash: {why}"
            }
        }
    }
    read_listing(f, s.here)
}

/// Restores the trashed entry `name` to where it came from.
action restore_entry(f: Files, name: Text) uses files, time (
    icon: .arrow_up_from_line,
    label: "Restore",
) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.restore(name) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t restore it: {why}"
        }
    }
}

/// Removes the entry at `path` for good.
action delete_permanently(f: Files, path: Text) uses files (
    icon: .trash,
    label: "Delete permanently",
) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.remove(path) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t delete it: {why}"
        }
    }
}

/// Empties this device's trash: every entry, and the receipt that would put it back.
action empty_trash(f: Files) uses files, time (icon: .trash, label: "Empty Trash") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.trash_entries() {
        .ok(items) => {
            for t in items {
                match File.remove(t.path) {
                    .ok(_) => {}
                    .err(why) => {
                        s.notice = "Couldn’t empty Trash: {why}"
                    }
                }
                let info = parent_of(parent_of(t.path)) + "/info/" + t.name + ".trashinfo"
                match File.remove(info) {
                    .ok(_) => {}
                    .err(_) => {}
                }
            }
        }
        .err(why) => {
            s.notice = "Couldn’t empty Trash: {why}"
        }
    }
    read_listing(f, s.here)
}

/// Remembers the entries at `paths` as what a paste copies; `cut` makes it a move.
action carry(f: Files, paths: List<Text>, cut: Bool) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.carrying = paths
    s.cutting = cut
}

/// Remembers the entries the person chose as what a paste copies.
action carry_selection(f: Files, cut: Bool) {
    carry(f, acted_on(f), cut)
}

/// Packs the entries the person chose into one archive beside them: `zip` picks a ZIP, and TAR.ZST
/// otherwise. The archive is named after the entry alone, or after its folder when several go in.
action compress_selection(f: Files, zip: Bool) uses files, time {
    require not acted_on(f).is_empty() "Select files first"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let paths = acted_on(f)
    let ext = if zip { ".zip" } else { ".tar.zst" }
    let named = if paths.len() == 1 { name_of(paths.at(0) ?? "") } else { name_of(s.here) }
    let to = free_name(f, parent_of(paths.at(0) ?? s.here), named + ext)
    let folder = parent_of(paths.at(0) ?? s.here)
    match File.compress(paths, join(folder, to)) {
        .ok(_) => { read_listing(f, s.here) }
        .err(why) => {
            s.notice = "Couldn’t compress them: {why}"
        }
    }
}

/// Pastes what the person carries into the place shown, as the device's own work that says how
/// far it has got. A name the folder already holds is kept both: what arrives lands as
/// “name (2)”, so nothing is overwritten.
action paste(f: Files) uses files, time (icon: .clipboard_paste, label: "Paste", key: "Ctrl+V") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if not folder_place(s.here) or s.carrying.is_empty() {
        s.notice = if s.carrying.is_empty() {
            "Nothing has been copied or cut"
        } else { "Open a folder to paste into it" }
        return
    }
    let moving = s.cutting
    // A cut entry already here has nowhere to go.
    let going = s.carrying.filter(path -> not moving or parent_of(path) != s.here)
    if not going.is_empty() { put_into(f, going, s.here, moving) }
    if moving {
        s.carrying = []
        s.cutting = false
    }
}

/// A name `folder` does not hold yet, for an entry that would land as `name`: the name itself, or
/// “name (2)”, “name (3)” before the extension, so a paste or a drop never replaces a file.
action free_name(f: Files, folder: Text, name: Text) -> Text uses files {
    match File.list(folder) {
        .err(_) => name,
        .ok(found) => {
            if not found.any(e -> e.name == name) {
                return name
            }
            let parts = name.split(".")
            let stem = if parts.len() < 2 { name } else { parts.take(parts.len() - 1).join(".") }
            let ext = if parts.len() < 2 { "" } else { "." + (parts.last() ?? "") }
            let numbers = found.map((e, i) -> i + 2)
            let asked = numbers.map(i -> "{stem} ({i}){ext}")
            asked.find(c -> not found.any(e -> e.name == c)) ?? "{stem} (copy){ext}"
        }
    }
}

/// A key pressed on the row `focused`: what Files does with a copy, a cut or a delete over the
/// entries the person chose, or over that row when it is not among them.
action key_on(f: Files, focused: Text, key: Text) uses files, time {
    let chosen = if acted_on(f).contains(focused) { acted_on(f) } else { [focused] }
    if key == "Ctrl+C" {
        carry(f, chosen, false)
    } else if key == "Ctrl+X" {
        carry(f, chosen, true)
    } else if key == "Delete" { trash_entries(f, chosen) }
}

/// Remembers what a copy from the row at `path` takes: the entries the person chose when that row
/// is one of them, and the row alone otherwise.
action carry_row(f: Files, path: Text, cut: Bool) {
    if acted_on(f).contains(path) { carry(f, acted_on(f), cut) } else { carry(f, [path], cut) }
}

/// Moves to Trash what a delete from the row at `path` takes: the entries the person chose when
/// that row is one of them, and the row alone otherwise.
action trash_row(f: Files, path: Text) uses files, time {
    if acted_on(f).contains(path) {
        trash_entries(f, acted_on(f))
    } else { trash_entries(f, [path]) }
}

/// Marks the place `place` in the sidebar.
action add_favorite(f: Files, place: Text) (icon: .star, label: "Add to Favorites") {
    require not f.favorites.any(x -> x.place == place) "It already stands in Favorites"
    f.favorites.insert(Favorite(place: place))
}

/// Takes the place `place` out of the sidebar's favorites.
action remove_favorite(f: Files, place: Text) (icon: .star_off, label: "Remove from Favorites") {
    if let favorite = f.favorites.find(x -> x.place == place) { f.favorites.remove(favorite) }
}

/// Reads what the chosen entry is, so the inspector says its size, its kind and its path, and
/// shows a picture file's picture or a text file's first words.
action inspect(f: Files, path: Text) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.previewed = path
    s.preview = .none
    s.words = ""
    s.facts = .none
    let found = match File.stat(path) {
        .ok(entry) => entry,
        .err(_) => .none,
    }
    if let e = found {
        if e.folder {
            return
        }
        match engine.call("facts", e.path) {
            .ok(text) => match json.decode<FileFacts>(text) {
                .ok(facts) => {
                    s.facts = .some(facts)
                }
                .err(_) => {}
            },
            .err(_) => {}
        }
        if is_picture(e.name) and e.size < 20000000 {
            match File.read(e.path) {
                .ok(bytes) => {
                    s.preview = .some(File.of_bytes(e.name, bytes))
                }
                .err(_) => {}
            }
        } else if is_text(e.name) and e.size < 1000000 {
            match File.read(e.path) {
                .ok(bytes) => {
                    let text = bytes.text()
                    s.words = text.slice(0, text.len().min(4000))
                }
                .err(_) => {}
            }
        }
    }
}

/// Whether this window has a place behind it.
fn can_go_back(f: Files) -> Bool {
    if let s = f.sessions.first() { not s.went.is_empty() } else { false }
}

/// Whether this window has a place ahead of it.
fn can_go_forward(f: Files) -> Bool {
    if let s = f.sessions.first() { not s.ahead.is_empty() } else { false }
}

/// Whether this window shows the top folder.
fn at_top(f: Files) -> Bool {
    if let s = f.sessions.first() { s.here == "/" } else { true }
}

/// The person's home folder: where Files opens, and the top of the places the system keeps.
fn home_place() -> Text {
    "~"
}

/// Whether `place` is a folder this app can make things in.
fn folder_place(place: Text) -> Bool {
    place != "trash" and place != "recent" and place != "applications"
}

/// The folder above `path`, or `/` for the top, and `~` for the places directly in the home
/// folder, which are named from it rather than from `/`.
fn parent_of(path: Text) -> Text {
    if path == "/" or path == "" or path == "~" {
        "/"
    } else {
        let home = path.starts_with("~")
        let parts = path.split("/").filter(p -> p != "" and p != "~")
        let above = if parts.len() <= 1 { "" } else { parts.take(parts.len() - 1).join("/") }
        if home {
            if above == "" { "~" } else { "~/" + above }
        } else if above == "" { "/" } else { "/" + above }
    }
}

/// What `path` is called: its last part, `Root` for `/` and `Home` for `~`.
fn name_of(path: Text) -> Text {
    if path == "/" {
        "Root"
    } else if path == "~" {
        "Home"
    } else if path == "trash" {
        "Trash"
    } else if path == "recent" {
        "Recent"
    } else if path == "applications" {
        "Applications"
    } else {
        path.split("/").filter(p -> p != "").last() ?? path
    }
}

/// The path of a folder `name` inside `folder`.
fn join(folder: Text, name: Text) -> Text {
    if folder == "/" {
        "/" + name
    } else if folder.ends_with("/") { folder + name } else { folder + "/" + name }
}

/// A file's extension in lower case, or nothing when its name has none.
fn extension(name: Text) -> Text {
    let parts = name.split(".")
    if parts.len() < 2 { "" } else { (parts.last() ?? "").lower() }
}

/// Whether `name` is a picture's.
fn is_picture(name: Text) -> Bool {
    let ext = extension(name)
    [
        ext == "png",
        ext == "jpg",
        ext == "jpeg",
        ext == "gif",
        ext == "webp",
        ext == "bmp",
        ext == "svg",
    ]
        .any(x -> x)
}

/// The archive extension `name` ends with, longest first, or empty for a file that is none.
fn archive_extension(name: Text) -> Text {
    let lower = name.lower()
    if let found = [
            ".tar.gz",
            ".tar.xz",
            ".tar.zst",
            ".tar.bz2",
            ".tgz",
            ".txz",
            ".tzst",
            ".tbz2",
            ".tar",
            ".zip",
            ".7z",
        ]
            .find(m -> lower.ends_with(m)) {
        found
    } else { "" }
}

/// Whether `name` is an archive this app can extract.
fn is_archive(name: Text) -> Bool {
    archive_extension(name) != ""
}

/// The name `name` without its archive extension, so an extracted folder is called after it.
fn stem_of(name: Text) -> Text {
    let ext = archive_extension(name)
    if ext == "" { name } else { name.slice(0, name.len() - ext.len()) }
}

/// Whether `name` is a text file's.
fn is_text(name: Text) -> Bool {
    let ext = extension(name)
    [
        ext == "txt",
        ext == "md",
        ext == "json",
        ext == "toml",
        ext == "csv",
        ext == "tl",
        ext == "rs",
        ext == "ftl",
    ]
        .any(x -> x)
}

/// How long a recording plays, as a clock reads it: `0:42`, `1:05`, `1:02:03`.
fn playing_time(ms: Int) -> Text {
    let seconds = ms / 1000
    let minutes = seconds / 60
    let clock = if seconds % 60 < 10 { "0{seconds % 60}" } else { "{seconds % 60}" }
    if minutes < 60 {
        return "{minutes}:{clock}"
    }
    let within = if minutes % 60 < 10 { "0{minutes % 60}" } else { "{minutes % 60}" }
    "{minutes / 60}:{within}:{clock}"
}

/// How many bytes read to a person: `4.2 MB`, `812 B`.
fn size_of(n: Int) -> Text {
    if n < 1000 {
        "{n} B"
    } else if n < 1000000 {
        "{n / 1000} KB"
    } else if n < 1000000000 { "{n / 1000000} MB" } else { "{n / 1000000000} GB" }
}

/// The entries of `l` the person may see, named `found` where they searched, in the order the
/// window sorts them: folders above files.
fn shown(l: Listing, s: Session, found: Text) -> List<Entry> {
    let visible = l.entries.filter(e -> not e.hidden or s.hidden)
    let asked = found.trim().lower()
    let named = if asked == "" {
        visible
    } else {
        visible.filter(e -> e.name.lower().contains(asked))
    }
    let folders = ordered(named.filter(e -> e.folder), s.sort, s.ascending)
    let files = ordered(named.filter(e -> not e.folder), s.sort, s.ascending)
    [folders, files].flat_map(x -> x)
}

/// The sort key of one entry as text, so a folder orders the way the person asked whatever it
/// holds.
fn by_name(e: Entry) -> Text {
    e.name.lower()
}

/// `entries` in the order `sort` and `ascending` say.
fn ordered(entries: List<Entry>, sort: SortBy, ascending: Bool) -> List<Entry> {
    if sort == .size {
        if ascending { entries.sort_by(e -> e.size) } else { entries.sort_by(e -> e.size, desc) }
    } else if sort == .modified {
        if ascending {
            entries.sort_by(e -> e.modified)
        } else { entries.sort_by(e -> e.modified, desc) }
    } else if sort == .format {
        if ascending {
            entries.sort_by(e -> extension(e.name))
        } else {
            entries.sort_by(e -> extension(e.name), desc)
        }
    } else if ascending {
        entries.sort_by(e -> by_name(e))
    } else { entries.sort_by(e -> by_name(e), desc) }
}

/// One step of a path, drawn in the bar: what it is called and the path that reaches it.
type Crumb {
    label: Text = ""
    path: Text = ""
}

/// The steps of `path` from the root to it, each a place the bar goes back to.
fn crumbs(path: Text) -> List<Crumb> {
    if path == "/" or path == "~" or path == "" {
        []
    } else if not folder_place(path) {
        []
    } else {
        let above = parent_of(path)
        if above == "/" or above == path {
            [Crumb(label: "Root", path: "/"), Crumb(label: name_of(path), path: path)]
        } else {
            [crumbs(above), [Crumb(label: name_of(path), path: path)]].flat_map(x -> x)
        }
    }
}

/// The folder shown, when the place is one.
fn place_listing(s: Session) -> Option<Listing> {
    s.listings.find(l -> l.path == s.here)
}

/// The file type of `e`, as the inspector names it: "Folder", "PNG image", "Markdown text", or
/// its extension.
fn kind_of(e: Entry) -> Text {
    if e.folder {
        "Folder"
    } else if is_picture(e.name) {
        "Image"
    } else if is_text(e.name) {
        "Text"
    } else if extension(e.name) == "" { "File" } else { extension(e.name).upper() + " file" }
}

/// Files' window: the places down the side, the folder they chose beside it, and what is chosen
/// on the right.
view files(f: Files) {
    // What the person typed in the window's search: the folder shown narrows to it.
    state found: Text = ""
    // What the person typed in the bar, when they chose to type a path there.
    state typed: Text = ""
    state typing: Bool = false
    // The name being asked for: a path renamed, or `new` for a folder being made; empty for none.
    state naming: Text = ""
    state name: Text = ""
    if let s = f.sessions.first() {
        let listing = place_listing(s)
        let visible: List<Entry> = if let l = listing { shown(l, s, found) } else { [] }
        // The previews of the pictures in the folder shown fill in while the window shows it.
        if previews_waiting(s) {
            every 1s: preview_step(f)
        }
        ui.NavShell(
            content_kind: if s.grid { .grid } else { .list },
            title: name_of(s.here),
            count: if s.here == "recent" {
                f.recent.len()
            } else if s.here == "applications" {
                device.apps.installed.len()
            } else { visible.len() },
        ) {
            on search(t: Text): set(found, t)
            // A file dragged in from another app lands in the folder shown, under its own name.
            on drop(files: List<File>): import_dropped(f, files)
            sidebar_actions: {
                ui.Button(
                    "Add the folder shown to Favorites",
                    icon: .star,
                    icon_only: true,
                    role: .plain,
                ) {
                    on press: add_favorite(f, s.here)
                }
            }
            sidebar: {
                places(f, s)
            }
            actions: {
                ui.Button("New folder", icon: .folder_plus, role: .primary) {
                    on press: set(naming, "new").then(set(name, ""))
                }
                view_options(f, s, naming, name)
            }
            content: {
                stack {
                    path_bar(f, s, typing, typed)
                    if s.notice != "" {
                        ui.Banner(
                            s.notice,
                            kind: .warning,
                            icon: .triangle_alert,
                            action: "Dismiss",
                        ) {
                            on act: dismiss_notice(f)
                        }
                    }
                    for t in s.transfers {
                        transfer_row(f, t)
                    }
                    if naming != "" {
                        naming_row(f, s, naming, name)
                    }
                    if s.opening != "" {
                        openers_row(f, s)
                    }
                    if s.here == "recent" {
                        recent_files(f, s)
                    } else if s.here == "applications" {
                        applications(f)
                    } else if let l = listing {
                        if l.error != "" {
                            ui.EmptyState(
                                "Couldn’t open “{name_of(s.here)}”",
                                icon: .folder,
                                message: l.error,
                                action: "Try again",
                                error: true,
                            ) {
                                on act: refresh(f)
                            }
                        } else if not l.read {
                            ui.EmptyState("Reading “{name_of(s.here)}”…", icon: .folder)
                        } else if visible.is_empty() {
                            ui.EmptyState(
                                if found == "" {
                                    "This folder is empty"
                                } else { "Nothing here matches “{found}”" },
                                icon: .folder,
                            )
                        } else if s.grid {
                            entry_grid(f, s, visible)
                        } else {
                            entry_list(f, s, visible, naming, name)
                        }
                    } else {
                        ui.EmptyState("Reading “{name_of(s.here)}”…", icon: .folder)
                    }
                }
            }
            inspector: {
                inspected(f, s, visible)
            }
        }
    } else {
        ui.EmptyState("Files is starting…", icon: .folder)
    }
}

/// The places down the side: this person's, the system's folders, this computer's top, the trash,
/// the places the person marked, and the devices a file can be handed to.
view places(f: Files, s: Session) {
    let nearby = device.nearby_send.devices.filter(d -> d.reachable)
    ui.NavList(selected: s.here) {
        ui.NavSection("Places") {
            for place in f.places.filter(x -> x.group == "Places").sort_by(x -> x.label) {
                ui.NavItem(
                    place.path,
                    place.label,
                    icon: if place.path == "~" {
                        .home
                    } else if place.path == "trash" {
                        .trash_2
                    } else if place.path == "recent" {
                        .clock
                    } else if place.path == "applications" {
                        .store
                    } else if place.path == "/" {
                        .hard_drive
                    } else if place.path == "~/Pictures" {
                        .image
                    } else if place.path == "~/Music" {
                        .music
                    } else if place.path == "~/Videos" {
                        .video
                    } else if place.path == "~/Downloads" {
                        .download
                    } else if place.path == "~/Desktop" {
                        .monitor
                    } else if place.path == "~/Remote" { .cloud } else { .folder },
                ) {
                    on select: go(f, place.path)
                }
            }
        }
        let spaces = device.folders.folders.filter(w -> w.path != "")
        if not spaces.is_empty() {
            ui.NavSection("Workspace") {
                for w in spaces {
                    ui.NavItem(
                        w.path,
                        if w.name != "" { w.name } else { "Workspace" },
                        icon: .cloud,
                    ) {
                        on select: go(f, w.path)
                    }
                }
            }
        }
        let drives = device.drives.drives
        if not drives.is_empty() {
            ui.NavSection("Drives") {
                for d in drives {
                    if let at = d.mounted {
                        ui.NavItem(at.path, d.label, icon: .hard_drive) {
                            on select: go(f, at.path)
                        }
                    } else {
                        ui.NavItem("drive:{d.id}", d.label, icon: .hard_drive) {
                            on select: open_drive(f, d.id)
                        }
                    }
                }
            }
        }
        if not f.favorites.is_empty() {
            ui.NavSection("Favorites") {
                for mark in f.favorites {
                    ui.NavItem(mark.place, name_of(mark.place), icon: .star) {
                        on select: go(f, mark.place)
                    }
                }
            }
        }
        ui.NavSection("Folders") {
            for place in f.places.filter(x -> x.group == "Folders") {
                ui.NavItem(
                    place.path,
                    place.label,
                    icon: if place.path == "~" {
                        .home
                    } else if place.path == "trash" {
                        .trash_2
                    } else if place.path == "recent" {
                        .clock
                    } else if place.path == "applications" {
                        .store
                    } else if place.path == "/" {
                        .hard_drive
                    } else if place.path == "~/Pictures" {
                        .image
                    } else if place.path == "~/Music" {
                        .music
                    } else if place.path == "~/Videos" {
                        .video
                    } else if place.path == "~/Downloads" {
                        .download
                    } else if place.path == "~/Desktop" {
                        .monitor
                    } else if place.path == "~/Remote" { .cloud } else { .folder },
                ) {
                    on select: go(f, place.path)
                }
            }
        }
        ui.NavSection("This computer") {
            for place in f.places.filter(x -> x.group == "This computer") {
                ui.NavItem(
                    place.path,
                    place.label,
                    icon: if place.path == "~" {
                        .home
                    } else if place.path == "trash" {
                        .trash_2
                    } else if place.path == "recent" {
                        .clock
                    } else if place.path == "applications" {
                        .store
                    } else if place.path == "/" {
                        .hard_drive
                    } else if place.path == "~/Pictures" {
                        .image
                    } else if place.path == "~/Music" {
                        .music
                    } else if place.path == "~/Videos" {
                        .video
                    } else if place.path == "~/Downloads" {
                        .download
                    } else if place.path == "~/Desktop" {
                        .monitor
                    } else if place.path == "~/Remote" { .cloud } else { .folder },
                ) {
                    on select: go(f, place.path)
                }
            }
        }
        if not nearby.is_empty() {
            ui.NavSection("Nearby") {
                for d in nearby {
                    ui.NavItem(d.handle, d.name, icon: .laptop) {
                        on select: send_nearby(f, d.handle)
                    }
                }
            }
        }
    }
}

/// The bar over the folder: the way back and up, and the steps to the folder shown.
view path_bar(f: Files, s: Session, typing: State<Bool>, typed: State<Text>) {
    if typing {
        stack {
            ui.TextInput(
                bind: typed,
                placeholder: "Type a path, e.g. ~/Documents",
                entry: true,
                autofocus: true,
            ) {
                on submit(path): go(f, path).then(set(typing, false))
                on cancel: set(typing, false)
            }
        }
    } else {
        row(gap: .related, align: .center) {
            ui.Button("Back", icon: .arrow_left, icon_only: true, role: .plain) {
                on press: back(f)
            }
            ui.Button("Forward", icon: .arrow_right, icon_only: true, role: .plain) {
                on press: forward(f)
            }
            ui.Button("Up one folder", icon: .arrow_up, icon_only: true, role: .plain) {
                on press: up(f)
            }
            ui.Breadcrumb(root_icon: .home) {
                on edit: set(typed, s.here).then(set(typing, true))
                ui.BreadcrumbSegment("Home") {
                    on activate: go(f, "~")
                }
                for step in crumbs(s.here) {
                    ui.BreadcrumbSegment(step.label) {
                        on activate: go(f, step.path)
                    }
                }
            }
        }
    }
}

/// How the folder is read: as rows or cards, in which order, and which names show.
view view_options(f: Files, s: Session, naming: State<Text>, name: State<Text>) {
    stack(gap: .related) {
        ui.ToggleGroup(selected: if s.grid { 1 } else { 0 }, label: "How the folder shows") {
            ui.Segment("List", icon: .list)
            ui.Segment("Grid", icon: .layout_grid)
            on change(i): choose_view(f, i)
        }
        ui.MenuButton("Sort", icon: .arrow_up_down, icon_only: true) {
            ui.MenuItem("Sort by name", checked: s.sort == .name) {
                on choose: sort_by(f, .name)
            }
            ui.MenuItem("Sort by size", checked: s.sort == .size) {
                on choose: sort_by(f, .size)
            }
            ui.MenuItem("Sort by kind", checked: s.sort == .format) {
                on choose: sort_by(f, .format)
            }
            ui.MenuItem("Sort by date", checked: s.sort == .modified) {
                on choose: sort_by(f, .modified)
            }
            ui.MenuItem("Ascending", checked: s.ascending) {
                on choose: ascending(f, true)
            }
            ui.MenuItem("Descending", checked: not s.ascending) {
                on choose: ascending(f, false)
            }
        }
        ui.MenuButton("View options", icon: .ellipsis, icon_only: true) {
            ui.MenuItem(
                if s.hidden { "Hide hidden files" } else { "Show hidden files" },
                icon: .eye,
            ) {
                on choose: toggle_hidden(f, not s.hidden)
            }
            ui.MenuItem("Larger", icon: .zoom_in, key: "Ctrl+=", disabled: s.zoom == 2) {
                on choose: zoom_in(f)
            }
            ui.MenuItem("Smaller", icon: .zoom_out, key: "Ctrl+-", disabled: s.zoom == 0) {
                on choose: zoom_out(f)
            }
            ui.MenuItem("New folder", icon: .folder_plus) {
                on choose: set(naming, "new").then(set(name, ""))
            }
            ui.MenuItem("New file", icon: .file_plus) {
                on choose: start_new_file(f).then(set(naming, "new-file")).then(set(name, ""))
            }
            ui.MenuItem("Refresh", icon: .refresh) {
                on choose: refresh(f)
            }
            ui.MenuItem("Select all", icon: .check, key: "Ctrl+A") {
                on choose: select_all(f)
            }
            ui.MenuItem("Clear selection", icon: .x, disabled: acted_on(f).is_empty()) {
                on choose: clear_selection(f)
            }
            ui.MenuItem("Copy", icon: .copy, key: "Ctrl+C", disabled: acted_on(f).is_empty()) {
                on choose: carry_selection(f, false)
            }
            ui.MenuItem("Cut", icon: .scissors, key: "Ctrl+X", disabled: acted_on(f).is_empty()) {
                on choose: carry_selection(f, true)
            }
            ui.MenuItem("Paste", icon: .clipboard_paste, key: "Ctrl+V") {
                on choose: paste(f)
            }
            ui.MenuItem("Compress to ZIP", icon: .file_archive, disabled: acted_on(f).is_empty()) {
                on choose: compress_selection(f, true)
            }
            ui.MenuItem(
                "Compress to TAR.ZST",
                icon: .file_archive,
                disabled: acted_on(f).is_empty(),
            ) {
                on choose: compress_selection(f, false)
            }
            ui.MenuItem(
                "Move to Trash",
                icon: .trash_2,
                destructive: true,
                key: "Delete",
                disabled: acted_on(f).is_empty(),
            ) {
                on choose: trash_entries(f, acted_on(f))
            }
            if s.here == "trash" {
                ui.MenuItem("Empty Trash", icon: .trash, destructive: true) {
                    on choose: empty_trash(f)
                }
            }
        }
    }
}

/// The apps that open the entry the person asked to open with, one press away each.
view openers_row(f: Files, s: Session) {
    row(gap: .tight, align: .center) {
        text("Open “{name_of(s.opening)}” with", role: .caption, secondary: true)
        for opener in s.openers {
            ui.Button(opener.name, role: .plain) {
                on press: open_with(f, opener.id)
            }
        }
        ui.Button("Cancel", role: .plain) {
            on press: close_openers(f)
        }
    }
}

/// The field asking for a name: a folder being made, a file, or an entry renamed.
view naming_row(f: Files, s: Session, naming: State<Text>, name: State<Text>) {
    row(gap: .related, align: .center) {
        ui.TextInput(
            bind: name,
            placeholder: if naming == "new" {
                "Name of the new folder"
            } else if naming == "new-file" { "Name of the new file" } else { "New name" },
            entry: true,
            autofocus: true,
        ) {
            on submit(typed): name_it(f, naming, typed).then(set(naming, ""))
            on cancel: set(naming, "")
        }
    }
    if naming == "new-file" and not s.templates.is_empty() {
        row(gap: .tight, align: .center) {
            text("From a template", role: .caption, secondary: true)
            for template in s.templates {
                ui.Button(template, role: .plain) {
                    on press: new_from_template(f, template, name).then(set(naming, ""))
                }
            }
        }
    }
}

/// Does what the name asked of the person is for: makes the folder or the file, or renames the
/// entry.
action name_it(f: Files, naming: Text, typed: Text) uses files, time {
    if naming == "new" {
        new_folder(f, typed)
    } else if naming == "new-file" { new_file(f, typed) } else { rename_entry(f, naming, typed) }
}

/// The folder as rows, each with its menu.
view entry_list(
    f: Files,
    s: Session,
    entries: List<Entry>,
    naming: State<Text>,
    name: State<Text>,
) {
    ui.VirtualList(
        name: "Files",
        multi_select: true,
        in_column: true,
        chosen_values: if s.chosen.is_empty() { .none } else { .some(s.chosen) },
    ) {
        on select_value(path: Text): touch_entry(f, path)
        on selected_values(paths: List<Text>): choose(f, paths)
        for e in entries {
            ui.ListItem(value: e.path, keys: ["Ctrl+C", "Ctrl+X", "Delete"]) {
                on shortcut(k): key_on(f, e.path, k)
                // Picked up and dragged, an entry leaves Files as the file itself — with the rest
                // of the chosen entries when it is one of them — for an app beside it to take, or
                // a folder's row here; a press that does not travel still opens it.
                ui.DragSource("", files: dragged(s, e), title: e.name) {
                    if e.folder {
                        ui.DropTarget {
                            on drop_files(paths: List<Text>): drop_into(f, paths, e.path)
                            entry_row(f, e, s.zoom == 0)
                        }
                    } else {
                        entry_row(f, e, s.zoom == 0)
                    }
                }
                menu: {
                    if e.folder {
                        ui.MenuItem("Open", icon: .folder_open) {
                            on choose: go(f, e.path)
                        }
                    } else {
                        ui.MenuItem("Open", icon: .external_link) {
                            on choose: open_entry(f, e.path)
                        }
                    }
                    ui.MenuItem("Get info", icon: .info) {
                        on choose: inspect(f, e.path)
                    }
                    ui.MenuSeparator()
                    if s.here == "trash" {
                        ui.MenuItem("Restore", icon: .arrow_up_from_line) {
                            on choose: restore_entry(f, e.name)
                        }
                        ui.MenuItem("Delete permanently", icon: .trash, destructive: true) {
                            on choose: delete_permanently(f, e.path)
                        }
                    } else {
                        if e.folder {
                            ui.MenuItem("Open in Terminal", icon: .terminal) {
                                on choose: open_terminal(f, e.path)
                            }
                        } else if is_archive(e.name) {
                            ui.MenuItem("Extract", icon: .archive) {
                                on choose: extract_entry(f, e.path)
                            }
                            for d in destinations(f, s) {
                                ui.MenuItem("Extract to “{d.name}”", group: "Extract to…") {
                                    on choose: extract_into(f, e.path, d.path)
                                }
                            }
                        } else {
                            ui.MenuItem("Open with…", icon: .external_link) {
                                on choose: find_openers(f, e.path)
                            }
                        }
                        ui.MenuItem("Compress to ZIP", icon: .file_archive) {
                            on choose: if acted_on(f).contains(e.path) {
                                compress_selection(f, true)
                            } else { compress_entry(f, e.path, true) }
                        }
                        ui.MenuItem("Compress to TAR.ZST", icon: .file_archive) {
                            on choose: if acted_on(f).contains(e.path) {
                                compress_selection(f, false)
                            } else { compress_entry(f, e.path, false) }
                        }
                        for d in destinations(f, s) {
                            ui.MenuItem("Move to “{d.name}”", group: "Move to…") {
                                on choose: move_into(f, d.path)
                            }
                        }
                        for d in destinations(f, s) {
                            ui.MenuItem("Copy to “{d.name}”", group: "Copy to…") {
                                on choose: copy_into(f, d.path)
                            }
                        }
                        ui.MenuItem("Rename", icon: .pencil) {
                            on choose: set(naming, e.path).then(set(name, e.name))
                        }
                        ui.MenuItem("Copy", icon: .copy, key: "Ctrl+C") {
                            on choose: carry_row(f, e.path, false)
                        }
                        ui.MenuItem("Cut", icon: .scissors, key: "Ctrl+X") {
                            on choose: carry_row(f, e.path, true)
                        }
                        ui.MenuItem(
                            "Move to Trash",
                            icon: .trash_2,
                            destructive: true,
                            key: "Delete",
                        ) {
                            on choose: trash_row(f, e.path)
                        }
                    }
                }
            }
        }
    }
}

/// A folder as cards, each opening what it stands for: a picture shows the preview the device
/// made of it, and everything else its kind's glyph.
view entry_grid(f: Files, s: Session, entries: List<Entry>) {
    ui.Scroll {
        page(full) {
            // A card stands for a resource, and an entry of this device's disk is no resource:
            // the card carries the entry's own words and a press names its path.
            grid(min: if s.zoom == 0 { 8rem } else if s.zoom == 2 { 16rem } else { 12rem }) {
                for e in entries {
                    ui.Card(title: e.name) {
                        on activate: open_entry(f, e.path)
                        body: {
                            ui.DragSource("", files: dragged(s, e), title: e.name) {
                                if e.folder {
                                    ui.DropTarget {
                                        on drop_files(paths: List<Text>): drop_into(
                                            f,
                                            paths,
                                            e.path,
                                        )
                                        entry_card(s, e)
                                    }
                                } else {
                                    entry_card(s, e)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A copy or a move from this window: what it does and the entry it is on, how far it has got,
/// and a press that stops it; one that stopped short says why.
view transfer_row(f: Files, t: Transfer) {
    stack(padding: .related) {
        ui.ListRow(
            title: t.label,
            secondary: if t.failed != "" {
                t.failed
            } else if t.total > 0 {
                "{size_of(t.done)} of {size_of(t.total)} · {t.current}"
            } else { t.current },
            mark: if t.failed != "" { .triangle_alert } else { .copy },
        )
        if t.failed == "" {
            ui.ProgressBar(
                value: if t.total > 0 { t.done.to_float() / t.total.to_float() } else { 0.0 },
                label: t.label,
            )
            ui.Button("Stop", icon: .x, role: .plain) {
                on press: stop_transfer(f, t)
            }
        } else {
            ui.Button("Dismiss", icon: .x, role: .plain) {
                on press: dismiss_transfer(f, t)
            }
        }
    }
}

/// One entry's card: a picture's preview the device made, or its kind's glyph, and a file's size.
view entry_card(s: Session, e: Entry) {
    stack(gap: .tight) {
        if let drawn = preview_of(s, e.path) {
            if let picture = drawn.file {
                ui.Image(src: picture.url, alt: e.name, fit: .cover)
            } else {
                ui.Icon(.file_image, size: .large, secondary: true)
            }
        } else {
            ui.Icon(
                if e.folder {
                    .folder
                } else if is_picture(e.name) {
                    .file_image
                } else if is_text(e.name) { .file_text } else { .file },
                size: .large,
                secondary: not e.folder,
            )
        }
        if not e.folder {
            text(size_of(e.size), role: .caption, secondary: true)
        }
    }
}

/// One entry's row: what it is called, what it holds and when it changed, opening it when pressed.
/// `smaller` draws it as one quiet line, as the smaller zoom step does.
view entry_row(f: Files, e: Entry, smaller: Bool) {
    ui.ListRow(
        title: e.name,
        icon: if e.folder {
            .folder
        } else if is_picture(e.name) {
            .file_image
        } else if is_text(e.name) { .file_text } else { .file },
        meta: e.modified,
        value: if e.folder { "" } else { size_of(e.size) },
        dense: smaller,
    ) {
        on activate: open_entry(f, e.path)
    }
}

/// Opens the app `a`, as pressing its row does.
action open_app(f: Files, app_key: Text) {
    session.open_app(app_key)
}

/// The apps this workspace runs, each opening on its own: what the Applications place shows.
view applications(f: Files) {
    let apps = device.apps.installed
    if apps.is_empty() {
        ui.EmptyState(
            "No apps yet",
            icon: .store,
            message: "An app this workspace installs stands here, one press from opening.",
        )
    } else {
        ui.VirtualList(name: "Applications", in_column: true) {
            for a in apps.sort_by(x -> x.name.lower()) {
                ui.ListItem(value: a.key) {
                    ui.ListRow(title: a.name, secondary: a.category, icon: .layout_dashboard) {
                        on activate: open_app(f, a.key)
                    }
                }
            }
        }
    }
}

/// The files opened recently, each opening again.
view recent_files(f: Files, s: Session) {
    if f.recent.is_empty() {
        ui.EmptyState(
            "No recent files yet",
            icon: .clock,
            message: "Files you open stand here, one press away.",
        )
    } else {
        ui.VirtualList(name: "Recent files", in_column: true) {
            for r in f.recent.sort_by(x -> x.opened, desc) {
                ui.ListItem(value: r.path) {
                    ui.ListRow(title: r.name, secondary: r.path, icon: .clock, meta: r.opened) {
                        on activate: open_entry(f, r.path)
                    }
                }
            }
        }
    }
}

/// What is chosen: its kind, its size, when it changed and where it stands, and the picture or
/// the first words a file holds. Several chosen show how many, and the entry the inspector shows
/// last.
view inspected(f: Files, s: Session, entries: List<Entry>) {
    let picked = entries.find(e -> e.path == s.selected)
    if let e = picked {
        stack(gap: .group) {
            text(e.name, role: .title)
            if s.chosen.len() > 1 {
                text("{s.chosen.len()} selected", role: .caption, secondary: true)
            }
            ui.PropertyList {
                ui.Property("Type", kind_of(e))
                if not e.folder {
                    ui.Property("Size", size_of(e.size))
                }
                if let x = s.facts {
                    if x.width > 0 {
                        ui.Property("Dimensions", "{x.width} × {x.height} px")
                    }
                    if x.duration > 0 {
                        ui.Property("Duration", playing_time(x.duration))
                    }
                    if x.pages > 0 {
                        ui.Property("Pages", "{x.pages}")
                    }
                    if x.codec != "" {
                        ui.Property("Codec", x.codec)
                    }
                    if x.container != "" and x.container != x.codec {
                        ui.Property("Container", x.container)
                    }
                }
                ui.Property("Modified", e.modified)
                ui.Property("Location", parent_of(e.path), copyable: true)
                if e.original != "" {
                    ui.Property("Came from", e.original, copyable: true)
                }
            }
            if let picture = s.preview {
                ui.Image(src: picture.url, alt: e.name, fit: .contain)
            }
            if s.words != "" {
                text(s.words, role: .caption)
            }
        }
    } else {
        stack(gap: .group) {
            text(name_of(s.here), role: .title)
            text("Select a file to see what it is.", role: .caption, secondary: true)
        }
    }
}
