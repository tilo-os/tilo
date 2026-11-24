app so.tilo.file-explorer "Files" (
    icon: .folder,
    category: .utility,
    tagline: "Browse your files",
    keywords: ["files", "file manager", "folder", "folders", "browse", "explorer", "documents", "downloads", "trash", "dateien", "ordner", "durchsuchen"],
)

use so.tilo.device as device

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
    /// The places gone before and put away, so back and forward walk the way the person came.
    went: List<Text>
    ahead: List<Text>
    /// What the person copied or cut here, and whether a paste moves it.
    carrying: List<Text>
    cutting: Bool = false
    /// The folders this window has read.
    listings: Collection<Listing>
    /// The picture a chosen file shows, when it is one, and the text a chosen file holds.
    preview: Option<File> = .none
    previewed: Text = ""
    words: Text = ""
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

/// Opens the entry `e`: a folder in this window, a file in the app that opens its kind.
action open_entry(f: Files, e: Entry) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
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

/// Opens a file from Recent again, as the entry it was.
action open_recent(f: Files, r: Recent) uses files, time {
    open_entry(f, Entry(name: r.name, path: r.path, modified: r.opened))
}

/// A press on an entry: a folder opens as a person presses it once, and a file is read into the
/// inspector beside the list.
action touch_entry(f: Files, e: Entry) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if e.folder {
        go(f, e.path)
    } else {
        s.chosen = [e.path]
        s.selected = e.path
        inspect(f, e)
    }
}

/// The entries the list reports as chosen, in the order they stand there.
action choose(f: Files, paths: List<Text>) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.chosen = paths
    s.selected = paths.last() ?? ""
    if let last = paths.last() {
        if let e = place_entry(f, last) { inspect(f, e) }
    }
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

/// Opens the drive `drive` and goes to where it is open, so a removable disk is a place like any
/// other folder. A drive that still has to open shows as such until the device says where it is.
action open_drive(f: Files, drive: device.Drive) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
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

/// Renames the entry at `path` to `name` in its own folder.
action rename_entry(f: Files, path: Text, name: Text) uses files, time (icon: .pencil) {
    require name.trim() != "" "A name cannot be empty"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
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

/// Pastes what the person carries into the place shown. A name the folder already holds is kept
/// both: the copy lands as “name (2)”, and a cut entry is renamed before it moves, so nothing is
/// overwritten.
action paste(f: Files) uses files, time (icon: .clipboard_paste, label: "Paste", key: "Ctrl+V") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if not folder_place(s.here) or s.carrying.is_empty() {
        s.notice = if s.carrying.is_empty() {
            "Nothing has been copied or cut"
        } else { "Open a folder to paste into it" }
        return
    }
    let moving = s.cutting
    for path in s.carrying {
        if moving and parent_of(path) == s.here {
            // A cut entry is already here: there is nothing to do.
        } else {
            let name = free_name(f, s.here, name_of(path))
            let source = if moving and name != name_of(path) {
                match File.rename(path, name) {
                    .ok(moved) => moved.path,
                    .err(why) => {
                        s.notice = "Couldn’t rename “{name_of(path)}”: {why}"
                        path
                    }
                }
            } else { path }
            let outcome = if moving { File.move(source, s.here) } else { File.copy(path, s.here) }
            match outcome {
                .ok(_) => {}
                .err(why) => {
                    s.notice = "Couldn’t paste “{name_of(path)}”: {why}"
                }
            }
        }
    }
    if moving {
        s.carrying = []
        s.cutting = false
    }
    read_listing(f, s.here)
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

/// Takes the favorite `favorite` out of the sidebar.
action remove_favorite(f: Files, favorite: Favorite) (
    icon: .star_off,
    label: "Remove from Favorites",
) {
    f.favorites.remove(favorite)
}

/// Reads what the chosen entry is, so the inspector says its size, its kind and its path, and
/// shows a picture file's picture or a text file's first words.
action inspect(f: Files, e: Entry) uses files {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.previewed = e.path
    s.preview = .none
    s.words = ""
    if e.folder {
        return
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

/// The folder above `path`, or `/` for the top.
fn parent_of(path: Text) -> Text {
    if path == "/" or path == "" or path == "~" {
        "/"
    } else {
        let parts = path.split("/").filter(p -> p != "")
        if parts.len() <= 1 {
            "/"
        } else {
            "/" + parts.take(parts.len() - 1).join("/")
        }
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
                    if naming != "" {
                        naming_row(f, s, naming, name)
                    }
                    if s.opening != "" {
                        openers_row(f, s)
                    }
                    if s.here == "recent" {
                        recent_files(f, s)
                    } else if s.here == "applications" {
                        ui.EmptyState(
                            "No apps yet",
                            icon: .store,
                            message: "The apps on this device stand here once one is installed.",
                        )
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
                            on select: open_drive(f, d)
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
        on activate(e: Entry): open_entry(f, e)
        on select(e: Entry): touch_entry(f, e)
        on selected_values(paths: List<Text>): choose(f, paths)
        for e in entries {
            ui.ListItem(item: e, value: e.path, keys: ["Ctrl+C", "Ctrl+X", "Delete"]) {
                on shortcut(k): key_on(f, e.path, k)
                menu: {
                    if e.folder {
                        ui.MenuItem("Open", icon: .folder_open) {
                            on choose: go(f, e.path)
                        }
                    } else {
                        ui.MenuItem("Open", icon: .external_link) {
                            on choose: open_entry(f, e)
                        }
                    }
                    ui.MenuItem("Get info", icon: .info) {
                        on choose: inspect(f, e)
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
                row: {
                    entry_row(e)
                }
            }
        }
    }
}

/// A folder as cards, each opening what it stands for.
view entry_grid(f: Files, s: Session, entries: List<Entry>) {
    ui.Scroll {
        page(full) {
            ui.CardGrid(name: "Files", card_width: .regular) {
                for e in entries {
                    ui.CardGridCard(item: e, title: e.name) {
                        on activate: open_entry(f, e)
                        body: {
                            stack(gap: .tight) {
                                ui.Icon(
                                    if e.folder {
                                        .folder
                                    } else if is_picture(e.name) {
                                        .file_image
                                    } else if is_text(e.name) { .file_text } else { .file },
                                    size: .large,
                                    secondary: not e.folder,
                                )
                                if not e.folder {
                                    text(size_of(e.size), role: .caption, secondary: true)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One entry's row: what it is called, what it holds and when it changed.
view entry_row(e: Entry) {
    ui.ListRow(
        title: e.name,
        icon: if e.folder {
            .folder
        } else if is_picture(e.name) {
            .file_image
        } else if is_text(e.name) { .file_text } else { .file },
        meta: e.modified,
        value: if e.folder { "" } else { size_of(e.size) },
    )
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
            on activate(r: Recent): open_recent(f, r)
            for r in f.recent.sort_by(x -> x.opened, desc) {
                ui.ListItem(item: r) {
                    row: {
                        ui.ListRow(title: r.name, secondary: r.path, icon: .clock, meta: r.opened)
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
