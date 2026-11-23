app so.tilo.photos "Photos" (
    icon: .image,
    category: .graphics,
    tagline: "Your photos, together",
    keywords: ["photo", "photos", "picture", "pictures", "image", "images", "album", "albums", "favorites", "gallery", "camera roll", "foto", "fotos", "bild", "bilder", "alben", "favoriten"],
)

use so.tilo.vision as vision

/// A photo in the library: its name, the address of its picture, and what the library knows of
/// it. The address names the bytes in the workspace's content store, so every member of the
/// workspace reads the same picture wherever the library reached them.
type Photo (untitled: "Photo") {
    name: Text
    /// Where the picture's bytes are: an address `tilo-blob://<reference>` the window's pictures
    /// read from the content store. Empty where the bytes are not here.
    picture: Text = ""
    /// The small preview the grid draws, so a library of thousands opens cheaply. Empty where no
    /// preview was made and the grid draws the picture itself.
    thumb: Text = ""
    /// The picture after the person's turns, where they made any: what the window shows and a save
    /// writes. Empty where the photograph stands as it came.
    edited: Text = ""
    /// The quarter turns clockwise the person made.
    rotation: Int = 0
    favorite: Bool = false
    /// When the photo was taken, as the camera wrote it: kept as words until something needs
    /// to read a time from them.
    captured_at: Text = ""
    /// The folder it came in from, where the library knows one.
    folder: Text = ""
    tags: Set<Text>
    people: Set<Text>
    /// The place the photo was taken, as words a person set.
    location: Text = ""
    /// The coordinates that place names: nought where none is set.
    latitude: Float = 0.0
    longitude: Float = 0.0
    /// What the picture shows, as a person or an analysis wrote it.
    description: Text = ""
    /// Objects and pets the picture shows, named.
    objects: Set<Text>
    pets: Set<Text>

    search {
        filter favorite (icon: .star)
        filter tags
        filter people
    }
}

/// A collection of photos a person named: its photos, by the ids they have in the library.
type Album (untitled: "Album") {
    name: Text
    photos: Set<Text>
}

/// A person the library knows: their name and the photos they are on, by id. The vision service
/// proposes groups of faces; people name them, merge them and take photos off them here.
type Person (untitled: "Person") {
    name: Text
    photos: Set<Text>
}

/// The workspace's photo library: every photo, the albums people made of them, and the people
/// the photos show.
type Library {
    photos: Collection<Photo>
    albums: Collection<Album>
    people: Collection<Person>
}

root library: Library

default view library for Library

default view photo for Photo

default view person for Person

row view person_row for Person

row view photo_row for Photo

place view photos for Library

/// Imports a photograph the person picks: the library keeps the address of its bytes, the name it
/// came in with, what its own container says of it (when and where it was taken) and a small
/// preview the grid draws. A picture whose bytes the window already holds reads at once.
primary action import_photo(lib: Library, file: File) -> Photo uses image (
    icon: .image,
    did: "added a photo",
    opens: ["image/png", "image/jpeg", "image/webp", .png, .jpg, .jpeg, .webp],
) {
    let photo = Photo(name: base_name(file.name), picture: file.url, folder: folder_name(file.path))
    if let info = file.image_info() {
        if let when = info.captured_at {
            photo.captured_at = when
        }
        if let north = info.latitude {
            photo.latitude = north
        }
        if let east = info.longitude {
            photo.longitude = east
        }
    }
    if let small = file.thumbnail(512) {
        photo.thumb = small.url
    }
    lib.photos.insert(photo)
}

/// The folder a file came in, where it came in one: the name of the folder above it, and empty for
/// a file the person picked on its own.
fn folder_name(path: Text) -> Text {
    let parts = path.split("/")
    if parts.len() < 2 { "" } else { parts.at(parts.len() - 2) ?? "" }
}

/// Imports a photograph a caller already knows the capture time and place of (the camera's
/// handoff). It opens no file kind of its own: the `opens` action above is what Files and the
/// person's own file system hand a picture to.
action import_photo_meta(
    lib: Library,
    file: File,
    captured_at: Text = "",
    location: Text = "",
) -> Photo uses image (icon: .image, did: "added a photo") {
    let photo = Photo(
        name: base_name(file.name),
        picture: file.url,
        captured_at: captured_at,
        location: location,
    )
    if let info = file.image_info() {
        if let when = info.captured_at {
            photo.captured_at = when
        }
        if let north = info.latitude {
            photo.latitude = north
        }
        if let east = info.longitude {
            photo.longitude = east
        }
    }
    if let small = file.thumbnail(512) {
        photo.thumb = small.url
    }
    lib.photos.insert(photo)
}

/// The file's own name, without the folders it came in.
fn base_name(path: Text) -> Text {
    path.split("/").last() ?? path
}

/// Imports the pictures a person drops on the window or picks a folder of: each picture file
/// becomes a photo, with the folder it came in, and a file that is no picture is left alone.
action import_dropped(lib: Library, files: List<File>) uses image (did: "added photos") {
    for file in files {
        if file.media.starts_with("image/") {
            let photo = Photo(
                name: base_name(file.name),
                picture: file.url,
                folder: folder_name(file.path),
            )
            if let info = file.image_info() {
                if let when = info.captured_at {
                    photo.captured_at = when
                }
                if let north = info.latitude {
                    photo.latitude = north
                }
                if let east = info.longitude {
                    photo.longitude = east
                }
            }
            if let small = file.thumbnail(512) {
                photo.thumb = small.url
            }
            lib.photos.insert(photo)
        }
    }
}

/// Marks a photo as a favorite, or takes that back.
action toggle_favorite(p: Photo, favorite: Bool = not p.favorite) (icon: .star, group: "Favorite") {
    p.favorite = favorite
}

/// Turns the photograph a quarter clockwise, as its person sees it: the window draws the original
/// turned, and a save writes what it draws.
action rotate_right(p: Photo) uses image (icon: .rotate_cw, label: "Rotate right", group: "Edits") {
    let quarters = if p.rotation >= 3 { 0 } else { p.rotation + 1 }
    match File.at(p.name, p.picture).turned(quarters) {
        .ok(turned) => {
            p.rotation = quarters
            p.edited = turned.url
        }
        .err(_) => {}
    }
}

/// Turns the photograph a quarter anticlockwise.
action rotate_left(p: Photo) uses image (icon: .rotate_ccw, label: "Rotate left", group: "Edits") {
    let quarters = if p.rotation == 0 { 3 } else { p.rotation - 1 }
    match File.at(p.name, p.picture).turned(quarters) {
        .ok(turned) => {
            p.rotation = quarters
            p.edited = turned.url
        }
        .err(_) => {}
    }
}

/// Puts the photograph back as it came, taking the person's own turns off it.
action reset_edits(p: Photo) (label: "Reset edits", group: "Edits") {
    p.rotation = 0
    p.edited = ""
}

/// Keeps the part of the photograph its person chose: `x`, `y`, `width` and `height` are
/// fractions of the picture as the window shows it, so every member and device reads the crop.
action crop_photo(p: Photo, x: Float, y: Float, width: Float, height: Float) uses image (
    icon: .crop,
    label: "Crop",
    group: "Edits",
) {
    if width <= 0.0 or height <= 0.0 {
        return
    }
    match File.at(p.name, shown_picture(p)).cropped(x, y, width, height) {
        .ok(cropped) => {
            p.edited = cropped.url
        }
        .err(_) => {}
    }
}

/// The day a capture time names, as a person reads it: the camera's `2026:07:12 18:16:00` and
/// an ISO `2026-07-12T18:16:00Z` both name the day first.
fn capture_day(when: Text) -> Text {
    if when.contains("T") {
        when.split("T").at(0) ?? when
    } else if when.contains(" ") { when.split(" ").at(0) ?? when } else { when }
}

/// The day as a person writes it: `2026-07-12`, never the camera's colons.
fn day_word(day: Text) -> Text {
    day.replace(":", "-")
}

/// The picture a window shows for `p`: the person's edited one where they made one, else the
/// photograph as it came.
fn shown_picture(p: Photo) -> Text {
    if p.edited != "" { p.edited } else { p.picture }
}

/// The picture the grid draws for `p`: the small preview where one was made, else the picture.
fn card_picture(p: Photo) -> Text {
    if p.thumb != "" { p.thumb } else { p.picture }
}

/// The photo ids in a list, toggled: the one there is taken out, the one that is not is added.
fn toggled(ids: List<Text>, id: Text) -> List<Text> {
    if ids.contains(id) { ids.filter(x -> x != id) } else { [ids, [id]].flat_map(x -> x) }
}

/// The ids of the photos a place shows: what Select all takes.
fn place_ids(lib: Library, place: Text) -> List<Text> {
    lib.photos.filter(p -> shown(lib, place, p)).map(p -> p.id)
}

/// Marks each named photo a favorite.
action favorite_selected(lib: Library, ids: List<Text>) (icon: .star, label: "Favorite selected") {
    for p in lib.photos {
        if ids.contains(p.id) {
            p.favorite = true
        }
    }
}

/// Takes each named photo out of the library.
destructive action remove_selected(lib: Library, ids: List<Text>) (
    icon: .trash,
    label: "Remove selected",
) {
    for id in ids {
        if let p = lib.photos.find(x -> x.id == id) { lib.photos.remove(p) }
    }
}

/// Puts each named photo in an album.
action add_selected_to_album(a: Album, ids: List<Text>) (icon: .folder, label: "Add to album") {
    for id in ids {
        a.photos.add(id)
    }
}

/// Gives a photo a tag.
action tag(p: Photo, t: Text) (icon: .tag, group: "Tags") {
    p.tags.add(t)
}

/// Takes a tag off a photo.
action untag(p: Photo, t: Text) (label: "Untag", group: "Tags") {
    p.tags.remove(t)
}

/// Takes a photo out of the library. The file it came from is not touched.
destructive action remove(lib: Library, p: Photo) (icon: .trash, label: "Remove from library") {
    lib.photos.remove(p)
}

/// Starts an album, and puts the photo it is started from in it.
action new_album(lib: Library, p: Option<Photo>) -> Album (icon: .folder_plus, label: "New album") {
    let made = lib.albums.insert(Album(name: "New album"))
    if let photo = p { made.photos.add(photo.id) }
    made
}

/// Renames an album.
action rename_album(a: Album, to: Text) (icon: .pencil, label: "Rename album") {
    a.name = to
}

/// Deletes an album. Its photos stay in the library.
destructive action remove_album(lib: Library, a: Album) (icon: .trash, label: "Delete album") {
    lib.albums.remove(a)
}

/// Puts a photo in an album, or takes it out of one when it is already in it.
action album_contains_toggle(a: Album, p: Photo) (icon: .folder, label: "Album") {
    if a.photos.contains(p.id) { a.photos.remove(p.id) } else { a.photos.add(p.id) }
}

/// The link that reaches a photo's picture: the address of its bytes, which every device of the
/// workspace reads from the content store.
action share(p: Photo) -> Text (icon: .screen_share, label: "Share") {
    photo_link(p)
}

/// Names a person on a photo, and puts the photo on that person when the library knows them.
action add_person(lib: Library, p: Photo, person: Text) (
    icon: .user,
    label: "Add person",
    group: "People",
) {
    p.people.add(person)
    if let known = lib.people.find(x -> x.name == person) { known.photos.add(p.id) }
}

/// Takes a person off a photo, and the photo off that person.
action remove_person(lib: Library, p: Photo, person: Text) (
    label: "Remove person",
    group: "People",
) {
    p.people.remove(person)
    if let known = lib.people.find(x -> x.name == person) { known.photos.remove(p.id) }
}

/// Names a person: the photos they are on say the new name too.
action rename_person(lib: Library, person: Person, to: Text) (
    icon: .pencil,
    label: "Rename person",
) {
    let from = person.name
    person.name = to
    for p in lib.photos {
        if p.people.contains(from) {
            p.people.remove(from)
            p.people.add(to)
        }
    }
}

/// Starts a person the library knows, with the photos it already has of them.
action new_person(lib: Library, name: Text, photos: List<Text>) -> Person (
    icon: .user,
    label: "New person",
) {
    let made = lib.people.insert(Person(name: name))
    for id in photos {
        made.photos.add(id)
        if let p = lib.photos.find(x -> x.id == id) { p.people.add(name) }
    }
    made
}

/// Merges one person into another: their photos come together under the name kept, and the name
/// merged away leaves every photo it was on.
action merge_people(lib: Library, from: Person, into: Person) (
    icon: .users,
    label: "Merge people",
) {
    for id in from.photos {
        into.photos.add(id)
    }
    for p in lib.photos {
        if p.people.contains(from.name) {
            p.people.remove(from.name)
            p.people.add(into.name)
        }
    }
    lib.people.remove(from)
}

/// Takes one photo off a person, and the person off that photo.
action split_photo(lib: Library, person: Person, photo: Text) (label: "Take off this person") {
    person.photos.remove(photo)
    if let p = lib.photos.find(x -> x.id == photo) { p.people.remove(person.name) }
}

/// Forgets a person: their name leaves every photo they were on.
destructive action remove_person_resource(lib: Library, person: Person) (
    icon: .trash,
    label: "Forget person",
) {
    for p in lib.photos {
        if p.people.contains(person.name) { p.people.remove(person.name) }
    }
    lib.people.remove(person)
}

/// Sets the place a photo was taken from where the person points on the map: the picker reports
/// the point as its latitude then its longitude.
action set_place(p: Photo, at: List<Float>) (icon: .map_pin, label: "Set place") {
    p.latitude = at.at(0) ?? 0.0
    p.longitude = at.at(1) ?? 0.0
}

/// Takes the place off a photo.
action clear_place(p: Photo) (label: "Clear place", group: "Place") {
    p.latitude = 0.0
    p.longitude = 0.0
    p.location = ""
}

/// The original picture as a file, to save where the person says.
action export_photo(p: Photo) -> File (icon: .download, label: "Save photograph") {
    if shown_picture(p) == "" {
        File.of_text("{p.name}.txt", "The picture of {p.name} is not here.")
    } else { File.at(p.name, shown_picture(p)) }
}

/// The address a photo's picture reads at, or an empty text where the bytes are not here.
fn photo_link(p: Photo) -> Text {
    shown_picture(p)
}

/// The photo after `p` in the library's order, if there is one.
fn next_id(lib: Library, p: Photo) -> Text {
    let at = lib.photos.index_of(p) ?? 0
    if let after = lib.photos.at(at + 1) { after.id } else { p.id }
}

/// The photo before `p` in the library's order, if there is one.
fn previous_id(lib: Library, p: Photo) -> Text {
    let at = lib.photos.index_of(p) ?? 0
    if at > 0 {
        if let before = lib.photos.at(at - 1) { before.id } else { p.id }
    } else { p.id }
}

/// Whether `place` shows the photo `p`: the whole library, the favorites, or an album it is in.
fn shown(lib: Library, place: Text, p: Photo) -> Bool {
    if place == "library" or place == "timeline" {
        true
    } else if place == "favorites" {
        p.favorite
    } else if place == "people" {
        not p.people.is_empty()
    } else if place == "tags" {
        not p.tags.is_empty()
    } else if place == "folders" {
        p.folder != ""
    } else if place == "places" {
        p.location != "" or p.latitude != 0.0 or p.longitude != 0.0
    } else if let person = lib.people.find(x -> x.id == place) {
        person.photos.contains(p.id)
    } else if let album = lib.albums.find(a -> a.id == place) {
        album.photos.contains(p.id)
    } else { false }
}

/// What a place is called, over the library's content.
fn place_title(lib: Library, place: Text) -> Text {
    if place == "library" {
        "Library"
    } else if place == "timeline" {
        "Timeline"
    } else if place == "favorites" {
        "Favorites"
    } else if place == "people" {
        "People"
    } else if place == "tags" {
        "Tags"
    } else if place == "folders" {
        "Folders"
    } else if place == "places" {
        "Places"
    } else if let person = lib.people.find(x -> x.id == place) {
        person.name
    } else if let album = lib.albums.find(a -> a.id == place) { album.name } else { "Photos" }
}

/// The workspace's photo library: its photos as a grid, the albums people made beside them, and
/// the one photo a person opened large in the window.
view library(lib: Library) {
    state place: Text = "library"
    state viewing: Text = ""
    state selecting: Bool = false
    state selection: List<Text> = []
    state presenting: Bool = false
    state slide: Int = 0
    state zoom: Float = 1.0
    state confirming: Bool = false
    state shared: Text = ""
    state cropping: Bool = false
    state crop_edges: List<Float> = [0.0, 0.0, 1.0, 1.0]
    ui.NavShell(
        content_kind: .page,
        title: if let p = lib.photos.find(x -> x.id == viewing) {
            p.name
        } else { place_title(lib, place) },
        sidebar_title: "Photos",
    ) {
        // A picture dropped anywhere on the window joins the library.
        on drop(files: List<File>): import_dropped(lib, files)
        actions: {
            ui.SearchButton()
            if selecting {
                ui.Button("Select all", icon: .check) {
                    on press: set(selection, place_ids(lib, place))
                }
                ui.Button("Clear selection", icon: .trash) {
                    on press: set(selection, [])
                }
                if not selection.is_empty() {
                    ui.Button("Favorite selected", icon: .star) {
                        on press: favorite_selected(lib, selection)
                    }
                    ui.Button("Remove selected", icon: .trash, role: .destructive) {
                        on press: remove_selected(lib, selection)
                    }
                }
                ui.Button("Done", icon: .check, role: .primary) {
                    on press: set(selecting, false).then(set(selection, []))
                }
            } else {
                ui.Button("Open", icon: .image, role: .primary) {
                    on press: file.choose(
                        import_photo(lib),
                        accept: ["image/png", "image/jpeg", "image/webp"],
                    )
                }
                ui.Button("Import folder", icon: .folder) {
                    on press: file.choose_folder(
                        import_dropped(lib),
                        accept: ["image/png", "image/jpeg", "image/webp"],
                    )
                }
                ui.Button("Select", icon: .check) {
                    on press: set(selecting, true)
                }
                if not lib.photos.is_empty() {
                    ui.Button("Present", icon: .play) {
                        on press: set(slide, 0).then(set(presenting, true))
                    }
                }
                if vision.analysis.phase == "running" {
                    ui.Button(
                        "Analyzing {vision.analysis.completed} of {vision.analysis.total}",
                        icon: .pause,
                    ) {
                        on press: vision.pause(vision.analysis)
                    }
                } else if vision.analysis.phase == "paused" {
                    ui.Button("Analysis paused", icon: .play) {
                        on press: vision.resume(vision.analysis)
                    }
                } else if not lib.photos.is_empty() and not vision.image_model.available {
                    // Why no model answers, in its own words: there is nothing to start.
                    ui.Text(vision.image_model.detail, role: .caption, secondary: true)
                } else if not lib.photos.is_empty() {
                    ui.Button("Analyze photos", icon: .sparkles) {
                        on press: vision.start(vision.analysis)
                    }
                }
            }
        }
        sidebar: {
            ui.NavList(selected: place) {
                ui.NavSection("Photos") {
                    ui.NavItem("library", "Library", icon: .image) {
                        on select: set(place, "library").then(set(viewing, ""))
                    }
                    ui.NavItem("favorites", "Favorites", icon: .star) {
                        on select: set(place, "favorites").then(set(viewing, ""))
                    }
                    ui.NavItem("timeline", "Timeline", icon: .clock) {
                        on select: set(place, "timeline").then(set(viewing, ""))
                    }
                    ui.NavItem("people", "People", icon: .users) {
                        on select: set(place, "people").then(set(viewing, ""))
                    }
                    ui.NavItem("tags", "Tags", icon: .tag) {
                        on select: set(place, "tags").then(set(viewing, ""))
                    }
                    ui.NavItem("folders", "Folders", icon: .folder) {
                        on select: set(place, "folders").then(set(viewing, ""))
                    }
                    ui.NavItem("places", "Places", icon: .map_pin) {
                        on select: set(place, "places").then(set(viewing, ""))
                    }
                }
                if not lib.albums.is_empty() {
                    ui.NavSection("Albums") {
                        for a in lib.albums {
                            ui.NavItem(a.id, a.name, icon: .folder) {
                                on select: set(place, a.id).then(set(viewing, ""))
                            }
                        }
                    }
                }
                if not lib.people.is_empty() {
                    ui.NavSection("People") {
                        for person in lib.people {
                            ui.NavItem(person.id, person.name, icon: .user) {
                                on select: set(place, person.id).then(set(viewing, ""))
                            }
                        }
                    }
                }
            }
        }
        content: {
            if presenting {
                present(lib, slide, presenting)
            } else if let p = lib.photos.find(x -> x.id == viewing) {
                ui.PageSlide(level: 1) {
                    viewer(lib, p, viewing, zoom, confirming, shared, cropping, crop_edges)
                }
            } else if place == "people" {
                people_list(lib, place, viewing)
            } else if lib.photos.filter(p -> shown(lib, place, p)).is_empty() {
                empty_place(lib, place)
            } else if place == "timeline" {
                timeline_page(lib, viewing)
            } else if place == "places" {
                places_page(lib, viewing)
            } else {
                library_grid(lib, place, viewing, selecting, selection, confirming, shared)
            }
        }
    }
}

/// The slide after `slide`, back at the first after the last: a presentation goes round.
fn next_slide(slide: Int, count: Int) -> Int {
    if count <= 0 { 0 } else if slide + 1 >= count { 0 } else { slide + 1 }
}

/// A presentation: the library one picture at a time, each on screen for its time, the slide the
/// window keeps advanced on the frame's clock. Previous, Next and Stop work beside it.
view present(lib: Library, slide: State<Int>, presenting: State<Bool>) {
    if let p = lib.photos.at(slide) {
        stack(fill) {
            ui.Slideshow(image: shown_picture(p), alt: p.name, seconds: 5.0, fill) {
                on next: set(slide, next_slide(slide, lib.photos.len()))
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Button("Previous", icon: .chevron_left) {
                    on press: set(slide, (slide - 1).max(0))
                }
                ui.Button("Next", icon: .chevron_right) {
                    on press: set(slide, next_slide(slide, lib.photos.len()))
                }
                ui.Button("Stop", icon: .square) {
                    on press: set(presenting, false)
                }
            }
        }
    } else {
        ui.Button("Stop", icon: .square) {
            on press: set(presenting, false)
        }
    }
}

/// The people the library knows, each a row opening the photos they are on.
view people_list(lib: Library, place: State<Text>, viewing: State<Text>) {
    if lib.people.is_empty() {
        ui.EmptyState(
            "No people yet",
            message: "The vision service proposes the people in your photos; name them, merge them and split them here.",
            icon: .users,
        )
    } else {
        ui.VirtualList(selection_follows_focus: true, fill) {
            on select(person: Person): open(person)
            for person in lib.people {
                ui.ListItem(item: person) {
                    person_row(person)
                }
            }
        }
    }
}

/// One person in a list: their name and how many photos they are on.
view person_row(p: Person) {
    ui.ListRow(title: p.name, secondary: "{p.photos.len()} photos")
}

/// A person's own page: their name, and the photos the library keeps of them.
view person(p: Person) {
    ui.NavShell(content_kind: .page, title: p.name) {
        content: {
            stack(padding: .group, gap: .related, max: 44rem) {
                ui.TextInput(bind: p.name, title: true)
                ui.Text("{p.photos.len()} photos", role: .caption, secondary: true)
            }
        }
    }
}

/// The library as a grid of pictures: each a card the person opens large. The grid builds only the
/// cards it shows, so a library of ten thousand scrolls as one.
view library_grid(
    lib: Library,
    place: Text,
    viewing: State<Text>,
    selecting: State<Bool>,
    selection: State<List<Text>>,
    confirming: State<Bool>,
    shared: State<Text>,
) {
    let photos = lib.photos.filter(p -> shown(lib, place, p))
    if selecting {
        ui.MediaGrid(size: .small, fill) {
            // A press with shift, or a drag over the cards, takes the range of cards it covers
            // as the selection: what a person expects of a grid of photographs.
            on select(picked: List<Photo>): set(selection, picked.map(x -> x.id))
            for p in photos {
                ui.MediaCard(
                    p,
                    title: p.name,
                    picture: card_picture(p),
                    selected: selection.contains(p.id),
                ) {
                    on activate: set(selection, toggled(selection, p.id))
                }
            }
        }
    } else {
        ui.MediaGrid(size: .small, fill) {
            for p in photos {
                ui.MediaCard(p, title: p.name, picture: card_picture(p)) {
                    on activate: set(viewing, p.id)
                        .then(set(confirming, false))
                        .then(set(shared, ""))
                }
            }
        }
    }
}

/// The library by the day each photograph was taken: newest first, a heading for each day and
/// its photographs under it, and those the camera gave no time for at the end.
view timeline_page(lib: Library, viewing: State<Text>) {
    let dated = lib.photos.filter(p -> p.captured_at != "").sort_by(p -> p.captured_at, desc)
    let undated = lib.photos.filter(p -> p.captured_at == "")
    stack(gap: .section, fill) {
        for day in dated.group(p -> capture_day(p.captured_at)).sort_by(g -> g.key, desc) {
            stack(gap: .related) {
                ui.Text(day_word(day.key), role: .subheading)
                ui.MediaGrid(size: .small, fill) {
                    for p in day.items {
                        ui.MediaCard(p, title: p.name, picture: card_picture(p)) {
                            on activate: set(viewing, p.id)
                        }
                    }
                }
            }
        }
        if not undated.is_empty() {
            stack(gap: .related) {
                ui.Text("No capture date", role: .subheading)
                ui.MediaGrid(size: .small, fill) {
                    for p in undated {
                        ui.MediaCard(p, title: p.name, picture: card_picture(p)) {
                            on activate: set(viewing, p.id)
                        }
                    }
                }
            }
        }
    }
}

/// The places the library's photographs were taken: a map with a pin for each one that says
/// where it was taken, opening the photograph, and the ones whose place is only a name under it.
view places_page(lib: Library, viewing: State<Text>) {
    let mapped = lib.photos.filter(p -> p.latitude != 0.0 or p.longitude != 0.0)
    let named = lib.photos.filter(
        p -> p.location != "" and p.latitude == 0.0 and p.longitude == 0.0,
    )
    stack(gap: .group, fill) {
        // The photographs on the world: each a card that keeps the size it is given on screen
        // at every zoom, standing on the place it was taken.
        if let first = mapped.first() {
            ui.Map(
                name: "Places",
                latitude: first.latitude,
                longitude: first.longitude,
                world: true,
                aspect: 1.6,
            ) {
                for p in mapped {
                    ui.MapPin(
                        name: p.name,
                        latitude: p.latitude,
                        longitude: p.longitude,
                        width: 96,
                        height: 96,
                    ) {
                        ui.MediaCard(p, picture: card_picture(p)) {
                            on activate: set(viewing, p.id)
                        }
                    }
                }
            }
        }
        if not named.is_empty() {
            ui.Text("Named places", role: .caption, secondary: true)
            ui.MediaGrid(size: .small, fill) {
                for p in named {
                    ui.MediaCard(p, title: p.name, picture: card_picture(p)) {
                        on activate: set(viewing, p.id)
                    }
                }
            }
        }
    }
}

/// What a place says while it holds no photos.
view empty_place(lib: Library, place: Text) {
    if place == "favorites" {
        ui.EmptyState("No favorites yet", message: "Star a photo to find it here.", icon: .star)
    } else if place == "people" {
        ui.EmptyState(
            "No people yet",
            message: "Name the people on a photo in its details.",
            icon: .users,
        )
    } else if place == "tags" {
        ui.EmptyState("No tags yet", message: "Tag a photo in its details.", icon: .tag)
    } else if place == "folders" {
        ui.EmptyState(
            "No folders yet",
            message: "A photo's folder is where the library knows it came from.",
            icon: .folder,
        )
    } else if place == "places" {
        ui.EmptyState(
            "No places yet",
            message: "Give a photo a place on the map in its details.",
            icon: .map_pin,
        )
    } else if place != "library" and place != "timeline" {
        ui.EmptyState(
            "This album is empty",
            message: "Add photos to it from the library.",
            icon: .folder,
        )
    } else {
        ui.EmptyState(
            "Your library is empty",
            message: "Open a photo, or drop pictures on the window, to start your library.",
            icon: .image,
        )
    }
}

/// One photo open large: its picture, what the library knows of it, and what a person does next —
/// the photo after it, the one before, favorite, share and remove.
view viewer(
    lib: Library,
    p: Photo,
    viewing: State<Text>,
    zoom: State<Float>,
    confirming: State<Bool>,
    shared: State<Text>,
    cropping: State<Bool>,
    crop_edges: State<List<Float>>,
) {
    stack(padding: .group, gap: .related, max: 72rem) {
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Zoom out", icon: .minus, icon_only: true) {
                on press: set(zoom, (zoom - 0.25).max(1.0))
            }
            ui.Button("Zoom in", icon: .plus, icon_only: true) {
                on press: set(zoom, (zoom + 0.25).min(4.0))
            }
            ui.Button("Actual size", icon: .maximize_2, icon_only: true) {
                on press: set(zoom, 1.0)
            }
            ui.Button("Back", icon: .arrow_left) {
                on press: set(viewing, "").then(set(cropping, false))
            }
            ui.Button("Previous", icon: .chevron_left) {
                on press: set(viewing, previous_id(lib, p))
            }
            ui.Button("Next", icon: .chevron_right) {
                on press: set(viewing, next_id(lib, p))
            }
            ui.Button(if p.favorite { "Remove from favorites" } else { "Favorite" }, icon: .star) {
                on press: toggle_favorite(p)
            }
            ui.Button("Rotate left", icon: .rotate_ccw) {
                on press: rotate_left(p)
            }
            ui.Button("Rotate right", icon: .rotate_cw) {
                on press: rotate_right(p)
            }
            ui.Button("Crop", icon: .crop) {
                on press: set(cropping, true)
            }
            if p.edited != "" {
                ui.Button("Reset edits", icon: .undo_2) {
                    on press: reset_edits(p)
                }
            }
            ui.Button("Share", icon: .screen_share) {
                on press: set(shared, photo_link(p))
            }
            ui.Button("Remove", icon: .trash) {
                on press: set(confirming, true)
            }
        }
        if confirming {
            row(gap: .related, align: .center, wrap: true) {
                ui.Text("Take this photo out of the library?", role: .caption)
                ui.Button("Remove from library", icon: .trash, role: .destructive) {
                    on press: remove(lib, p)
                }
                ui.Button("Cancel") {
                    on press: set(confirming, false)
                }
            }
        }
        if cropping {
            ui.ImageCropEditor(
                shown_picture(p),
                left: crop_edges.at(0) ?? 0.0,
                top: crop_edges.at(1) ?? 0.0,
                right: crop_edges.at(2) ?? 1.0,
                bottom: crop_edges.at(3) ?? 1.0,
                aspect: 1.5,
            ) {
                on change(edges: List<Float>): set(crop_edges, edges)
            }
            row(gap: .related, align: .center, wrap: true) {
                ui.Text("Drag the corners to the part you keep.", role: .caption, secondary: true)
                ui.Button("Apply crop", icon: .check, role: .primary) {
                    on press: set(cropping, false).then(
                        crop_photo(
                            p,
                            crop_edges.at(0) ?? 0.0,
                            crop_edges.at(1) ?? 0.0,
                            (crop_edges.at(2) ?? 1.0) - (crop_edges.at(0) ?? 0.0),
                            (crop_edges.at(3) ?? 1.0) - (crop_edges.at(1) ?? 0.0),
                        ),
                    )
                }
                ui.Button("Cancel") {
                    on press: set(cropping, false)
                }
            }
        } else {
            ui.ImageViewer(shown_picture(p), alt: p.name, aspect: 1.5, zoom: zoom)
        }
        ui.Text(p.name, role: .title)
        if p.captured_at != "" {
            ui.Text(p.captured_at, role: .caption, secondary: true)
        }
        if not p.tags.is_empty() {
            row(gap: .related, wrap: true) {
                for t in p.tags {
                    ui.Chip(t, icon: .tag)
                }
            }
        }
        if shared != "" {
            ui.Text(shared, role: .caption, secondary: true)
        }
        details(lib, p)
    }
}

/// What the library knows of a photo, and what its fields are: its name, when it was taken,
/// its folder, its place and what it shows, the tags and people on it, where it was taken on a
/// map, and the picture as a file to save.
view details_fields(lib: Library, p: Photo) {
    stack(gap: .related) {
        ui.TextInput(bind: p.name, title: true)
        row(gap: .related, wrap: true) {
            ui.TextInput(bind: p.captured_at, placeholder: "Captured time", fill)
            ui.TextInput(bind: p.folder, placeholder: "Folder", fill)
        }
        row(gap: .related, wrap: true) {
            ui.TextInput(bind: p.location, placeholder: "Place", fill)
            ui.TextInput(bind: p.description, placeholder: "Description", lines: 2, fill)
        }
        if not p.tags.is_empty() {
            row(gap: .related, wrap: true) {
                for t in p.tags {
                    ui.Chip(t, icon: .tag) {
                        on remove: untag(p, t)
                    }
                }
            }
        }
        if not p.people.is_empty() {
            row(gap: .related, wrap: true) {
                for name in p.people {
                    ui.Chip(name, icon: .user) {
                        on remove: remove_person(lib, p, name)
                    }
                }
            }
        }
        ui.Text("Where it was taken", role: .caption, secondary: true)
        ui.Map(
            name: "Where it was taken",
            latitude: p.latitude,
            longitude: p.longitude,
            zoom: 2.0,
            aspect: 1.8,
        ) {
            on at(at: List<Float>): set_place(p, at)
            ui.MapPin(
                name: p.name,
                latitude: p.latitude,
                longitude: p.longitude,
                width: 12,
                height: 12,
            ) {
                ui.Text(p.name)
            }
        }
        row(gap: .related) {
            ui.Button("Save photograph", icon: .download) {
                on press: file.save(export_photo(p), name: p.name)
            }
            if p.latitude != 0.0 or p.longitude != 0.0 {
                ui.Button("Clear place", icon: .map_pin) {
                    on press: clear_place(p)
                }
            }
        }
    }
}

/// The details of a photo, with the albums it can join.
view details(lib: Library, p: Photo) {
    details_fields(lib, p)
    if not lib.albums.is_empty() {
        ui.Text("Albums", role: .caption, secondary: true)
        row(gap: .related, wrap: true) {
            for a in lib.albums {
                ui.Button(a.name, toggled: a.photos.contains(p.id)) {
                    on press: album_contains_toggle(a, p)
                }
            }
        }
    }
}

/// A photo's own page, as search, a link or another app opens it.
view photo(p: Photo) {
    state zoom: Float = 1.0
    ui.NavShell(content_kind: .page, title: p.name) {
        content: {
            stack(padding: .group, gap: .related, max: 72rem) {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Button("Zoom out", icon: .minus, icon_only: true) {
                        on press: set(zoom, (zoom - 0.25).max(1.0))
                    }
                    ui.Button("Zoom in", icon: .plus, icon_only: true) {
                        on press: set(zoom, (zoom + 0.25).min(4.0))
                    }
                    ui.Button("Actual size", icon: .maximize_2, icon_only: true) {
                        on press: set(zoom, 1.0)
                    }
                }
                ui.ImageViewer(shown_picture(p), alt: p.name, aspect: 1.5, zoom: zoom)
                ui.Text(p.name, role: .title)
                if p.captured_at != "" {
                    ui.Text(p.captured_at, role: .caption, secondary: true)
                }
                row(gap: .related) {
                    ui.Button(
                        if p.favorite { "Remove from favorites" } else { "Favorite" },
                        icon: .star,
                    ) {
                        on press: toggle_favorite(p)
                    }
                }
            }
        }
    }
}

/// One photo in a list: its name, and whether it is a favorite.
view photo_row(p: Photo) {
    ui.ListRow(title: p.name, mark: if p.favorite { .star })
}

/// The photos on their own, as a place shows them: each a card opening beside the list.
view photos(lib: Library) {
    if lib.photos.is_empty() {
        ui.EmptyState("No photos yet", icon: .image)
    } else {
        ui.MediaGrid(size: .small) {
            for p in lib.photos {
                ui.MediaCard(p, title: p.name, picture: card_picture(p)) {
                    on activate: open(p, front: false)
                }
            }
        }
    }
}

/// The library a screenshot shows: a few photographs with a picture carried in its own source, so
/// the page shows pictures with no file to fetch.
scenario "a library" {
    let lib = root Library
    lib.photos.insert(Photo(name: "Dawn", picture: sample(), favorite: true))
    lib.photos.insert(Photo(name: "Blue hour", picture: sample(), captured_at: "2026-06-01T05:42Z"))
    lib.photos.insert(Photo(name: "Dunes", picture: sample(), captured_at: "2026-05-30T17:10Z"))
    lib.photos.insert(
        Photo(name: "Pines in fog", picture: sample(), captured_at: "2026-05-29T07:15Z"),
    )
}

/// The picture the scenario carries, so a page shows an image with no file behind it.
fn sample() -> Text {
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAYAAACqaXHeAAAAPElEQVR42u3OMQEAAAgDoC251a3gLwSgOXLVkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJkiRJ0iN1AAG5eZ4hAAAAAElFTkSuQmCC"
}
