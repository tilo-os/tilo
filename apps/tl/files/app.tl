app so.tilo.file-explorer "Files" (
    icon: .folder,
    category: .utility,
    tagline: "Browse your files",
    keywords: ["files", "file manager", "folder", "folders", "browse", "explorer", "documents", "downloads", "trash", "dateien", "ordner", "durchsuchen"],
)

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
    f.places.insert(Place(label: "Root", path: "/", group: "This computer"))
    f.places.insert(Place(label: "Trash", path: "trash", group: "This computer"))
}

/// Opening Files reads the folder the person is in, unless this window has read it already.
opened action arrive(f: Files) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if s.listings.find(l -> l.path == s.here) == .none {
        read_listing(f, s.here)
    }
}

/// Reads the place `place` into this window's listing for it: a folder's entries, this device's
/// trash, or nothing for the places this app keeps itself.
action read_listing(f: Files, place: Text = "") uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    let path = if place == "" { s.here } else { place }
    if path == "recent" or path == "applications" {
        return
    }
    let listing = s.listings.find(l -> l.path == path)
        ?? s.listings.insert(Listing(path: path))
    if path != "trash" and listing.watch == "" {
        match File.watch(path, "changed") {
            .ok(token) => { listing.watch = token },
            .err(_) => {},
        }
    }
    if path == "trash" {
        match File.trash_entries() {
            .ok(items) => {
                listing.entries = items.map(t -> Entry(
                    name: t.name,
                    path: t.path,
                    folder: false,
                    size: 0,
                    modified: t.deleted,
                    original: t.original,
                ))
                listing.error = ""
                listing.read = true
            },
            .err(why) => { listing.error = why },
        }
        return
    }
    match File.list(path) {
        .ok(found) => {
            listing.entries = found.map(e -> Entry(
                name: e.name,
                path: e.path,
                folder: e.folder,
                size: e.size,
                modified: e.modified,
                hidden: e.hidden,
            ))
            listing.error = ""
            listing.read = true
        },
        .err(why) => { listing.error = why },
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
    s.preview = .none
    s.previewed = ""
    s.words = ""
    read_listing(f, place)
}

/// Goes back to the place the person came from.
action back(f: Files) uses files, time {
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
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    go(f, if folder_place(s.here) { parent_of(s.here) } else { "~" })
}

/// Dismisses what the window last said went wrong.
action dismiss_notice(f: Files) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.notice = ""
}

/// Reads the place shown again, so what changed on the disk shows.
action refresh(f: Files) uses files, time (icon: .refresh_cw, key: "Ctrl+R") {
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

/// Shows or hides the names that start with a dot.
action toggle_hidden(f: Files) (icon: .eye, label: "Show hidden files") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.hidden = not s.hidden
}

/// Opens the entry `e`: a folder in this window, a file in the app that opens its kind.
action open_entry(f: Files, e: Entry) uses files, time {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if e.folder {
        go(f, e.path)
    } else {
        match File.open(e.path) {
            .ok(_) => { note_recent(f, e.name, e.path) },
            .err(why) => { s.notice = "Nothing here opens “{e.name}”: {why}" },
        }
    }
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
        if let oldest = f.recent.sort_by(r -> r.opened).first() {
            f.recent.remove(oldest)
        }
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
        .ok(_) => { read_listing(f, s.here) },
        .err(why) => { s.notice = "Couldn’t make the folder: {why}" },
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
        },
        .err(why) => { s.notice = "Couldn’t make the file: {why}" },
    }
}

/// Renames the entry at `path` to `name` in its own folder.
action rename_entry(f: Files, path: Text, name: Text) uses files, time (icon: .pencil) {
    require name.trim() != "" "A name cannot be empty"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.rename(path, name.trim()) {
        .ok(_) => { read_listing(f, s.here) },
        .err(why) => { s.notice = "Couldn’t rename it: {why}" },
    }
}

/// Moves the entries at `paths` to this device's trash.
action trash_entries(f: Files, paths: List<Text>) uses files, time (icon: .trash_2, label: "Move to Trash", key: "Delete") {
    require not paths.is_empty() "Select files first"
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    for path in paths {
        match File.trash(path) {
            .ok(_) => {},
            .err(why) => { s.notice = "Couldn’t move “{name_of(path)}” to Trash: {why}" },
        }
    }
    read_listing(f, s.here)
}

/// Restores the trashed entry `name` to where it came from.
action restore_entry(f: Files, name: Text) uses files, time (icon: .arrow_up_from_line, label: "Restore") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.restore(name) {
        .ok(_) => { read_listing(f, s.here) },
        .err(why) => { s.notice = "Couldn’t restore it: {why}" },
    }
}

/// Removes the entry at `path` for good.
action delete_permanently(f: Files, path: Text) uses files (icon: .trash, label: "Delete permanently") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.remove(path) {
        .ok(_) => { read_listing(f, s.here) },
        .err(why) => { s.notice = "Couldn’t delete it: {why}" },
    }
}

/// Empties this device's trash: every entry, and the receipt that would put it back.
action empty_trash(f: Files) uses files, time (icon: .trash, label: "Empty Trash") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    match File.trash_entries() {
        .ok(items) => {
            for t in items {
                match File.remove(t.path) {
                    .ok(_) => {},
                    .err(why) => { s.notice = "Couldn’t empty Trash: {why}" },
                }
                let info = parent_of(parent_of(t.path)) + "/info/" + t.name + ".trashinfo"
                match File.remove(info) {
                    .ok(_) => {},
                    .err(_) => {},
                }
            }
        },
        .err(why) => { s.notice = "Couldn’t empty Trash: {why}" },
    }
    read_listing(f, s.here)
}

/// Remembers the entries at `paths` as what a paste copies; `cut` makes it a move.
action carry(f: Files, paths: List<Text>, cut: Bool) {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    s.carrying = paths
    s.cutting = cut
}

/// Pastes what the person carries into the place shown.
action paste(f: Files) uses files, time (icon: .clipboard_paste, label: "Paste", key: "Ctrl+V") {
    let s = f.sessions.first() ?? f.sessions.insert(Session())
    if not folder_place(s.here) or s.carrying.is_empty() {
        s.notice = if s.carrying.is_empty() { "Nothing has been copied or cut" } else { "Open a folder to paste into it" }
        return
    }
    let moving = s.cutting
    for path in s.carrying {
        let outcome = if moving { File.move(path, s.here) } else { File.copy(path, s.here) }
        match outcome {
            .ok(_) => {},
            .err(why) => { s.notice = "Couldn’t paste “{name_of(path)}”: {why}" },
        }
    }
    if moving {
        s.carrying = []
        s.cutting = false
    }
    read_listing(f, s.here)
}

/// A key pressed on the row `focused` while the entries `paths` are chosen: what Files does with
/// a copy, a cut or a delete.
action key_on(f: Files, paths: List<Text>, focused: Text, key: Text) uses files, time {
    let chosen = if paths.contains(focused) { paths } else { [focused] }
    if key == "Ctrl+C" {
        carry(f, chosen, false)
    } else if key == "Ctrl+X" {
        carry(f, chosen, true)
    } else if key == "Delete" {
        trash_entries(f, chosen)
    }
}

/// Marks the place `place` in the sidebar.
action add_favorite(f: Files, place: Text) (icon: .star, label: "Add to Favorites") {
    require not f.favorites.any(x -> x.place == place) "It already stands in Favorites"
    f.favorites.insert(Favorite(place: place))
}

/// Takes the favorite `favorite` out of the sidebar.
action remove_favorite(f: Files, favorite: Favorite) (icon: .star_off, label: "Remove from Favorites") {
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
            },
            .err(_) => {},
        }
    } else if is_text(e.name) and e.size < 1000000 {
        match File.read(e.path) {
            .ok(bytes) => {
                let text = bytes.text()
                s.words = text.slice(0, text.len().min(4000))
            },
            .err(_) => {},
        }
    }
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
    } else if folder.ends_with("/") {
        folder + name
    } else {
        folder + "/" + name
    }
}

/// A file's extension in lower case, or nothing when its name has none.
fn extension(name: Text) -> Text {
    let parts = name.split(".")
    if parts.len() < 2 {
        ""
    } else {
        (parts.last() ?? "").lower()
    }
}

/// Whether `name` is a picture's.
fn is_picture(name: Text) -> Bool {
    let ext = extension(name)
    [ext == "png", ext == "jpg", ext == "jpeg", ext == "gif", ext == "webp", ext == "bmp", ext == "svg"]
        .any(x -> x)
}

/// Whether `name` is a text file's.
fn is_text(name: Text) -> Bool {
    let ext = extension(name)
    [ext == "txt", ext == "md", ext == "json", ext == "toml", ext == "csv", ext == "tl", ext == "rs", ext == "ftl"]
        .any(x -> x)
}

/// How many bytes read to a person: `4.2 MB`, `812 B`.
fn size_of(n: Int) -> Text {
    if n < 1000 {
        "{n} B"
    } else if n < 1000000 {
        "{n / 1000} KB"
    } else if n < 1000000000 {
        "{n / 1000000} MB"
    } else {
        "{n / 1000000000} GB"
    }
}

/// The sort key of one entry as text, so a folder orders the way the person asked whatever it
/// holds.
fn by_name(e: Entry) -> Text {
    e.name.lower()
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

/// `entries` in the order `sort` and `ascending` say.
fn ordered(entries: List<Entry>, sort: SortBy, ascending: Bool) -> List<Entry> {
    if sort == .size {
        if ascending { entries.sort_by(e -> e.size) } else { entries.sort_by(e -> e.size, desc) }
    } else if sort == .modified {
        if ascending { entries.sort_by(e -> e.modified) } else { entries.sort_by(e -> e.modified, desc) }
    } else if sort == .format {
        if ascending {
            entries.sort_by(e -> extension(e.name))
        } else {
            entries.sort_by(e -> extension(e.name), desc)
        }
    } else if ascending {
        entries.sort_by(by_name)
    } else {
        entries.sort_by(by_name, desc)
    }
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
    } else if extension(e.name) == "" {
        "File"
    } else {
        extension(e.name).upper() + " file"
    }
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
    // The entries chosen, by their paths.
    state chosen: List<Text> = []
    if let s = f.sessions.first() {
        let listing = place_listing(s)
        let visible = if let l = listing { shown(l, s, found) } else { [] }
        ui.NavShell(
            content_kind: if s.grid { .grid } else { .list },
            title: name_of(s.here),
            count: if s.here == "recent" { f.recent.len() } else if s.here == "applications" { 0 } else { visible.len() },
        ) {
            on search(t: Text): set(found, t)
            sidebar_actions: {
                ui.Button("Add the folder shown to Favorites", icon: .star, icon_only: true, role: .plain) {
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
                view_options(f, s, chosen, naming, name)
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
                        naming_row(f, naming, name)
                    }
                    if s.here == "recent" {
                        recent_files(f, s)
                    } else if s.here == "applications" {
                        ui.EmptyState("No apps yet", icon: .store, message: "The apps on this device stand here once one is installed.")
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
                                if found == "" { "This folder is empty" } else { "Nothing here matches “{found}”" },
                                icon: .folder,
                            )
                        } else if s.grid {
                            entry_grid(f, s, visible)
                        } else {
                            entry_list(f, s, visible, chosen, naming, name)
                        }
                    } else {
                        ui.EmptyState("Reading “{name_of(s.here)}”…", icon: .folder)
                    }
                }
            }
            inspector: {
                inspected(f, s, visible, chosen)
            }
        }
    } else {
        ui.EmptyState("Files is starting…", icon: .folder)
    }
}

/// The places down the side: this person's, the system's folders, this computer's top, the trash,
/// and the places the person marked.
view places(f: Files, s: Session) {
    ui.NavList(selected: s.here) {
        ui.NavSection("Places") {
            for place in f.places.filter(x -> x.group == "Places").sort_by(x -> x.label) {
                ui.NavItem(place.path, place.label, icon: if place.path == "~" { .home } else if place.path == "trash" { .trash_2 } else if place.path == "recent" { .clock } else if place.path == "applications" { .store } else if place.path == "/" { .hard_drive } else if place.path == "~/Pictures" { .image } else if place.path == "~/Music" { .music } else if place.path == "~/Videos" { .video } else if place.path == "~/Downloads" { .download } else if place.path == "~/Desktop" { .monitor } else { .folder }) {
                    on select: go(f, place.path)
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
                ui.NavItem(place.path, place.label, icon: if place.path == "~" { .home } else if place.path == "trash" { .trash_2 } else if place.path == "recent" { .clock } else if place.path == "applications" { .store } else if place.path == "/" { .hard_drive } else if place.path == "~/Pictures" { .image } else if place.path == "~/Music" { .music } else if place.path == "~/Videos" { .video } else if place.path == "~/Downloads" { .download } else if place.path == "~/Desktop" { .monitor } else { .folder }) {
                    on select: go(f, place.path)
                }
            }
        }
        ui.NavSection("This computer") {
            for place in f.places.filter(x -> x.group == "This computer") {
                ui.NavItem(place.path, place.label, icon: if place.path == "~" { .home } else if place.path == "trash" { .trash_2 } else if place.path == "recent" { .clock } else if place.path == "applications" { .store } else if place.path == "/" { .hard_drive } else if place.path == "~/Pictures" { .image } else if place.path == "~/Music" { .music } else if place.path == "~/Videos" { .video } else if place.path == "~/Downloads" { .download } else if place.path == "~/Desktop" { .monitor } else { .folder }) {
                    on select: go(f, place.path)
                }
            }
        }
    }
}

/// The bar over the folder: the way back and up, and the steps to the folder shown.
view path_bar(f: Files, s: Session, typing: State<Bool>, typed: State<Text>) {
    if typing {
        stack {
            ui.TextInput(bind: typed, placeholder: "Type a path, e.g. ~/Documents", entry: true, autofocus: true) {
                on submit(path): go(f, path).then(set(typing, false))
                on cancel: set(typing, false)
            }
        }
    } else {
        row(gap: .related, align: .center) {
            ui.Button("Back", icon: .arrow_left, icon_only: true, role: .plain, enabled: not s.went.is_empty()) {
                on press: back(f)
            }
            ui.Button("Forward", icon: .arrow_right, icon_only: true, role: .plain, enabled: not s.ahead.is_empty()) {
                on press: forward(f)
            }
            ui.Button("Up one folder", icon: .arrow_up, icon_only: true, role: .plain, enabled: s.here != "/") {
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
view view_options(f: Files, s: Session, chosen: List<Text>, naming: State<Text>, name: State<Text>) {
    stack(gap: .related) {
        ui.ToggleGroup(selected: if s.grid { 1 } else { 0 }, label: "How the folder shows") {
            ui.Segment("List", icon: .list)
            ui.Segment("Grid", icon: .layout_grid)
            on change(i): show_grid(f, i == 1)
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
            ui.MenuSeparator()
            ui.MenuItem("Ascending", checked: s.ascending) {
                on choose: ascending(f, true)
            }
            ui.MenuItem("Descending", checked: not s.ascending) {
                on choose: ascending(f, false)
            }
        }
        ui.MenuButton("More", icon: .ellipsis, icon_only: true) {
            ui.MenuItem(if s.hidden { "Hide hidden files" } else { "Show hidden files" }, icon: .eye) {
                on choose: toggle_hidden(f)
            }
            ui.MenuItem("New folder", icon: .folder_plus) {
                on choose: set(naming, "new").then(set(name, ""))
            }
            ui.MenuItem("New file", icon: .file_plus) {
                on choose: set(naming, "new-file").then(set(name, ""))
            }
            ui.MenuItem("Refresh", icon: .refresh_cw) {
                on choose: refresh(f)
            }
            ui.MenuSeparator()
            ui.MenuItem("Copy", icon: .copy, key: "Ctrl+C", disabled: chosen.is_empty()) {
                on choose: carry(f, chosen, false)
            }
            ui.MenuItem("Cut", icon: .scissors, key: "Ctrl+X", disabled: chosen.is_empty()) {
                on choose: carry(f, chosen, true)
            }
            ui.MenuItem("Paste", icon: .clipboard_paste, key: "Ctrl+V") {
                on choose: paste(f)
            }
            ui.MenuItem("Move to Trash", icon: .trash_2, destructive: true, key: "Delete", disabled: chosen.is_empty()) {
                on choose: trash_entries(f, chosen)
            }
            if s.here == "trash" {
                ui.MenuItem("Empty Trash", icon: .trash, destructive: true) {
                    on choose: empty_trash(f)
                }
            }
        }
    }
}

/// The field asking for a name: a folder being made, a file, or an entry renamed.
view naming_row(f: Files, naming: State<Text>, name: State<Text>) {
    row(gap: .related, align: .center) {
        ui.TextInput(
            bind: name,
            placeholder: if naming == "new" { "Name of the new folder" } else if naming == "new-file" { "Name of the new file" } else { "New name" },
            entry: true,
            autofocus: true,
        ) {
            on submit(typed): name_it(f, naming, typed).then(set(naming, ""))
            on cancel: set(naming, "")
        }
    }
}

/// Does what the name asked of the person is for: makes the folder or the file, or renames the
/// entry.
action name_it(f: Files, naming: Text, typed: Text) uses files, time {
    if naming == "new" {
        new_folder(f, typed)
    } else if naming == "new-file" {
        new_file(f, typed)
    } else {
        rename_entry(f, naming, typed)
    }
}

/// The folder as rows, each with its menu.
view entry_list(f: Files, s: Session, entries: List<Entry>, chosen: List<Text>, naming: State<Text>, name: State<Text>) {
    ui.VirtualList(name: "Files", multi_select: true, in_column: true) {
        on selected(items): set(chosen, items.map(e -> e.path))
        on activate(e): open_entry(f, e)
        on select(e): set(chosen, [e.path]).then(inspect(f, e))
        for e in entries {
            ui.ListItem(item: e, keys: ["Ctrl+C", "Ctrl+X", "Delete"]) {
                on shortcut(k): key_on(f, chosen, e.path, k)
                menu: {
                    ui.ContextMenu {
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
                            ui.MenuItem("Rename", icon: .pencil) {
                                on choose: set(naming, e.path).then(set(name, e.name))
                            }
                            ui.MenuItem("Copy", icon: .copy, key: "Ctrl+C") {
                                on choose: carry(f, [e.path], false)
                            }
                            ui.MenuItem("Cut", icon: .scissors, key: "Ctrl+X") {
                                on choose: carry(f, [e.path], true)
                            }
                            ui.MenuItem("Move to Trash", icon: .trash_2, destructive: true, key: "Delete") {
                                on choose: trash_entries(f, [e.path])
                            }
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
            ui.CardGrid(name: "Files", card_width: .small) {
                for e in entries {
                    ui.CardGridCard(item: e, title: e.name) {
                        on activate: open_entry(f, e)
                        body: {
                            stack(gap: .tight) {
                                ui.Icon(
                                    if e.folder { .folder } else if is_picture(e.name) { .file_image } else if is_text(e.name) { .file_text } else { .file },
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
        icon: if e.folder { .folder } else if is_picture(e.name) { .file_image } else if is_text(e.name) { .file_text } else { .file },
        meta: e.modified,
        value: if e.folder { "" } else { size_of(e.size) },
    )
}

/// The files opened recently, each opening again.
view recent_files(f: Files, s: Session) {
    if f.recent.is_empty() {
        ui.EmptyState("No recent files yet", icon: .clock, message: "Files you open stand here, one press away.")
    } else {
        ui.VirtualList(name: "Recent files", in_column: true) {
            on activate(r: Recent): open_entry(f, Entry(name: r.name, path: r.path, modified: r.opened))
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
/// the first words a file holds.
view inspected(f: Files, s: Session, entries: List<Entry>, chosen: List<Text>) {
    let picked = entries.find(e -> chosen.contains(e.path))
    if let e = picked {
        stack(gap: .group) {
            text(e.name, role: .title)
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
