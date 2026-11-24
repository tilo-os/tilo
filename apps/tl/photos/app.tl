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
    /// read from the content store. Empty where the record names no picture at all.
    picture: Text = ""
    /// The small preview the grid draws, so a library of thousands opens cheaply. Empty where no
    /// preview was made and the grid draws the picture itself.
    thumb: Text = ""
    /// The picture the device drew from the person's edits: the part they kept, then the turns
    /// they made. Empty where they changed nothing; the photograph itself is never written.
    edited: Text = ""
    /// The quarter turns clockwise the person made.
    rotation: Int = 0
    /// The part of the photograph they kept, as fractions of the upright picture: its left, top,
    /// width and height. Empty where they kept the whole of it.
    crop: List<Float>
    /// How this device stands on the picture's bytes: empty before anything looked, `here` when
    /// it holds them, `waiting` while they are still arriving from another device, and
    /// `unreadable` when what it holds is no picture this device opens.
    picture_state: Text = ""
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
    /// The words in the picture, as this device read them: a sign, a page, a shopfront. Search
    /// finds the photo by them.
    recognized: Text = ""
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

/// One face the device found, and the photos it is in. A face has no name until the person who
/// scanned gives it one, so it is that person's alone (`readers: author`) until then; approving a
/// name sends it to the workspace, and from that moment everyone in it reads it.
type Suggestion (readers: author until sent) {
    /// The person's name, empty while the suggestion has none.
    name: Text
    /// The photos this face is in, by their ids in the library.
    photos: Set<Text>
    /// Whether a name was approved: a sent suggestion is the workspace's, not only its author's.
    sent: Bool = false
}

/// What an edit does to one field of every selected photo: keeps what each photo has, takes it
/// off all of them, gives them all one value, or adds or takes off names (tags, people).
enum Change {
    /// Each photo keeps the value it has.
    keep
    /// The value is taken off every photo.
    clear
    /// Every photo is given the same value.
    set
    /// Every photo gains these names; what it already has stays.
    add
    /// These names are taken off every photo; what it keeps stays.
    remove
}

/// The workspace's photo library: every photo, the albums people made of them, and the people
/// the photos show.
type Library {
    photos: Collection<Photo>
    albums: Collection<Album>
    people: Collection<Person>
    /// Faces the image model found and nobody has named yet: each is its scanner's alone until
    /// they approve a name, which sends it to the workspace.
    suggestions: Collection<Suggestion>
    /// The last folder import: how many files it looked at, how many became photos, and what it
    /// could not read, so a person knows what arrived and what did not.
    import_found: Int = 0
    import_done: Int = 0
    import_issues: List<Text>
    /// The import running on this device, by its job: the window reads how far it has got from the
    /// job's record, and the import's `done:` brings what it read into the library.
    import_job: Text = ""
    /// Whether the last import was stopped before it read everything.
    import_stopped: Bool = false
    /// The pictures the last import could not read where trying again could help.
    unread: Collection<Unread>
    /// Each person's face scan: their own alone, so one person's switch and progress are never
    /// another's.
    face_scans: Collection<FaceScan>
    /// What the last thing the library did has to say to the person: an album saved, a change
    /// another member made first. Empty where there is nothing to say.
    notice: Text = ""
    /// Whether an album draft is open, and if so its name and the photos chosen for it: the
    /// album itself changes only when the person saves the draft.
    album_draft_open: Bool = false
    /// The album the draft is of, and empty for one it will start.
    album_draft_id: Text = ""
    album_draft_name: Text = ""
    album_draft_photos: List<Text>
}

/// A picture an import could not read: what it is called, where its bytes are, where it stood in
/// what the person chose, and why.
type Unread {
    name: Text
    picture: Text
    path: Text
    why: Text
    retry: Bool
}

/// One picture an import hands the device to read.
type ChosenPicture {
    name: Text
    url: Text
    path: Text
}

/// What an import asks of the device: the pictures to read.
type ImportAsk {
    files: List<ChosenPicture>
}

/// A picture the device read: what the library keeps of it.
type ReadPicture {
    name: Text
    picture: Text
    thumb: Text
    folder: Text
    captured_at: Text
    latitude: Float
    longitude: Float
}

/// An import's record, as its job says it: `running`, `done` or `stopped`, how many pictures it
/// has of how many, and, once it ends, what it read and what it could not.
type ImportRecord {
    job: Text
    state: Text
    found: Int
    done: Int
    photos: List<ReadPicture>
    issues: List<Unread>
}

/// One person's face scan on this library: whether they let the device look for faces in the
/// photos, whether a scan runs or is paused, and the photos it has read, so it goes on with the
/// next ones. Faces are personal data: this is its person's alone.
type FaceScan (readers: author) {
    /// Whether this device may look for faces at all: off until the person turns it on.
    on: Bool = false
    /// `running`, `paused`, or empty when no scan is under way.
    state: Text = ""
    /// The photos the scan has read, by id.
    scanned: Set<Text>
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
    let photo = absorb_picture(lib, file)
    lib.import_found = 1
    lib.import_done = 1
    lib.import_issues = []
    photo
}

/// Adds the picture a file holds to the library: the photo the same picture already made, brought
/// up to what the file says now, or a new one. What the person set on a photo — its place, its
/// tags — is theirs, so reading the file again never overwrites it.
action absorb_picture(lib: Library, file: File) -> Photo uses image {
    let photo = if let held = lib.photos.find(p -> p.picture == file.url) {
        held
    } else {
        lib.photos.insert(
            Photo(name: base_name(file.name), picture: file.url, folder: folder_name(file.path)),
        )
    }
    if photo.folder == "" {
        photo.folder = folder_name(file.path)
    }
    if let info = file.image_info() {
        if photo.captured_at == "" {
            if let when = info.captured_at {
                photo.captured_at = when
            }
        }
        if photo.latitude == 0.0 and photo.longitude == 0.0 {
            if let north = info.latitude {
                photo.latitude = north
            }
            if let east = info.longitude {
                photo.longitude = east
            }
        }
    }
    if let small = file.thumbnail(512) {
        photo.thumb = small.url
    }
    // The person chose this file, so its bytes are on this device now.
    photo.picture_state = "here"
    photo
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
    let photo = absorb_picture(lib, file)
    if captured_at != "" {
        photo.captured_at = captured_at
    }
    if location != "" {
        photo.location = location
    }
    lib.import_found = 1
    lib.import_done = 1
    lib.import_issues = []
    photo
}

/// The file's own name, without the folders it came in.
fn base_name(path: Text) -> Text {
    path.split("/").last() ?? path
}

/// Imports the pictures a person drops on the window or picks a folder of, as a job on this
/// device: it reads each picture, when and where it was taken and its preview, saying how far it
/// has got as it goes, and can be stopped. A file that is no picture is left alone and said so.
/// Picking the same folder again brings what the files say to the photos already there, rather
/// than making a second copy of each.
action import_folder(lib: Library, files: List<File>) uses image (
    icon: .folder,
    label: "Import folder",
    did: "imported photos",
) {
    let pictures = files.filter(f -> f.media.starts_with("image/"))
    let chosen = pictures.map(f -> ChosenPicture(name: f.name, url: f.url, path: f.path))
    start_import(lib, json.encode(ImportAsk(files: chosen)), files.len())
    lib.import_issues = files
        .filter(f -> not f.media.starts_with("image/"))
        .map(f -> "{base_name(f.name)} is not a picture")
}

/// Starts the device reading the pictures `ask` names, stopping an import still under way first.
/// When the job ends, `imported` brings what it read into the library, as the one who started it.
action start_import(lib: Library, ask: Text, found: Int) {
    if lib.import_job != "" { engine.cancel(lib.import_job) }
    lib.import_found = found
    lib.import_done = 0
    lib.import_issues = []
    lib.import_stopped = false
    match engine.start("import", ask, done: j -> imported(lib, j)) {
        .err(why) => {
            lib.import_job = ""
            lib.import_issues = ["The import could not start: {why}"]
        }
        .ok(job) => {
            lib.import_job = job
        }
    }
}

/// An import's end: each picture it read becomes a photo, or brings what it says to the photo the
/// same picture already made, and what it could not read is kept to try again. What the person set
/// on a photo (its time, its place) is theirs, so reading it again never overwrites it.
action imported(lib: Library, j: host.Job) (did: "added photos") {
    match json.decode<ImportRecord>(j.detail) {
        .err(why) => {
            lib.import_issues = [why]
            lib.import_job = ""
        }
        .ok(record) => {
            for read in record.photos {
                let photo = if let held = lib.photos.find(p -> p.picture == read.picture) {
                    held
                } else {
                    lib.photos.insert(
                        Photo(name: read.name, picture: read.picture, folder: read.folder),
                    )
                }
                if photo.folder == "" {
                    photo.folder = read.folder
                }
                if read.thumb != "" {
                    photo.thumb = read.thumb
                }
                // The device read these bytes from its own store: here they are.
                photo.picture_state = "here"
                if photo.captured_at == "" {
                    photo.captured_at = read.captured_at
                }
                if photo.latitude == 0.0 and photo.longitude == 0.0 {
                    photo.latitude = read.latitude
                    photo.longitude = read.longitude
                }
            }
            for u in lib.unread {
                lib.unread.remove(u)
            }
            for issue in record.issues.filter(i -> i.retry) {
                lib.unread.insert(issue)
            }
            lib.import_done = record.photos.len()
            lib.import_issues = [lib.import_issues, record.issues.map(i -> "{i.name}: {i.why}")]
                .flat_map(x -> x)
            lib.import_stopped = record.state == "stopped"
            if j.id == lib.import_job {
                lib.import_job = ""
            }
        }
    }
}

/// Stops the import under way: it ends after the picture it is reading, and what it read so far
/// joins the library.
action stop_import(lib: Library) (icon: .square, label: "Stop importing") {
    require lib.import_job != "" "No import is under way"
    engine.cancel(lib.import_job)
}

/// Tries again the pictures the last import could not read where trying again could help.
action retry_import(lib: Library) (icon: .refresh, label: "Try again") {
    require not lib.unread.is_empty() "Every picture of the last import was read"
    let chosen = lib.unread.map(u -> ChosenPicture(name: u.name, url: u.picture, path: u.path))
    start_import(lib, json.encode(ImportAsk(files: chosen)), chosen.len())
}

/// Reads the photos of a folder again, or of the whole library when no folder is named: their
/// previews, and when and where they were taken where nobody set it.
action reindex(lib: Library, folder: Text = "") (icon: .refresh, label: "Refresh folder") {
    require not lib.photos
        .filter(p -> p.picture != "" and (folder == "" or p.folder == folder))
        .is_empty() "No photos to read again"
    let held = lib.photos.filter(p -> p.picture != "" and (folder == "" or p.folder == folder))
    let chosen = held.map(
        p -> ChosenPicture(
            name: p.name,
            url: p.picture,
            path: if p.folder == "" { p.name } else { "{p.folder}/{p.name}" },
        ),
    )
    start_import(lib, json.encode(ImportAsk(files: chosen)), chosen.len())
}

/// Takes the last import's report off the window.
action dismiss_import(lib: Library) (icon: .x, label: "Dismiss the import report") {
    lib.import_found = 0
    lib.import_done = 0
    lib.import_issues = []
    lib.import_stopped = false
}

/// Looks at where this device stands on a photo's picture: whether it holds the bytes yet, and
/// whether what it holds is a picture it opens. A record reaches this device from another one
/// before its picture's bytes do, so a photo says the picture is still arriving instead of
/// drawing nothing; the window looks again while it waits.
action check_picture(p: Photo) uses image {
    let state = if p.picture == "" {
        "unreadable"
    } else if not File.at(p.name, p.picture).here() {
        "waiting"
    } else {
        match File.at(p.name, p.picture).image_info() {
            .some(_) => "here",
            .none => "unreadable",
        }
    }
    if p.picture_state != state {
        p.picture_state = state
    }
}

/// Marks a photo as a favorite, or takes that back.
action toggle_favorite(p: Photo, favorite: Bool = not p.favorite) (icon: .star, group: "Favorite") {
    p.favorite = favorite
}

/// Turns the photograph a quarter clockwise, as its person sees it.
action rotate_right(p: Photo) uses image (icon: .rotate_cw, label: "Rotate right", group: "Edits") {
    develop(p, if p.rotation >= 3 { 0 } else { p.rotation + 1 }, p.crop)
}

/// Turns the photograph a quarter anticlockwise.
action rotate_left(p: Photo) uses image (icon: .rotate_ccw, label: "Rotate left", group: "Edits") {
    develop(p, if p.rotation == 0 { 3 } else { p.rotation - 1 }, p.crop)
}

/// Puts the photograph back as it came, taking every edit off it.
action reset_edits(p: Photo) uses image (icon: .undo_2, label: "Reset edits", group: "Edits") {
    develop(p, 0, [])
}

/// Records an edit on the photograph and draws it: the device keeps the part the person chose,
/// then turns it, and keeps the result in the content store. Nothing is written over the
/// photograph, so every edit can be taken back and the person can see the picture as it came
/// beside the one they made.
action develop(p: Photo, rotation: Int, crop: List<Float>) uses image {
    p.rotation = rotation
    p.crop = crop
    if crop.is_empty() and rotation == 0 {
        p.edited = ""
        return
    }
    let file = File.at(p.name, p.picture)
    // The part they kept, in the upright picture's own fractions, drawn first: a turn after it
    // leaves the crop meaning the same part of the photograph.
    let kept = if crop.is_empty() {
        file
    } else {
        match file.cropped(
            crop.at(0) ?? 0.0,
            crop.at(1) ?? 0.0,
            crop.at(2) ?? 1.0,
            crop.at(3) ?? 1.0,
        ) {
            .ok(part) => part,
            .err(_) => file,
        }
    }
    if rotation == 0 {
        p.edited = kept.url
    } else {
        match kept.turned(rotation) {
            .ok(turned) => {
                p.edited = turned.url
            }
            .err(_) => {}
        }
    }
}

/// What a picture shows, in a sentence, as the device's image model reads it: the sentence the
/// library's details show and search finds. The work is the device's own, asked of this session
/// on the app's behalf, so the app never runs a model itself.
action describe_photo(p: Photo) (icon: .sparkles, label: "Describe", group: "Details") {
    match try vision.describe(vision.image_model, image: shown_picture(p)) {
        .ok(sentence) => {
            p.description = sentence
        }
        .err(_) => {}
    }
}

/// The words in the photograph, read on this device and kept with the photo, so the library
/// finds it by what it says. Needs no model: a device without one still reads its text.
action recognize_text(p: Photo) uses image (icon: .scan_line, label: "Read the text") {
    match try vision.ocr(vision.image_model, image: shown_picture(p)) {
        .ok(words) => {
            p.recognized = words
        }
        .err(_) => {}
    }
}

/// Copies the photograph to the person's clipboard as a picture, so it pastes into another app.
action copy_photo(p: Photo) uses clipboard (icon: .copy, label: "Copy") {
    if shown_picture(p) != "" {
        clipboard.copy_image(File.at(p.name, shown_picture(p)))
    }
}

/// What the device's image model finds in a picture, added to what the photo already lists: the
/// objects, and the pets apart from them.
action tag_photo(p: Photo) (icon: .tag, label: "Tags", group: "Details") {
    match try vision.shows(vision.image_model, image: shown_picture(p)) {
        .ok(found) => {
            if p.description == "" {
                p.description = found.description
            }
            for word in found.objects {
                p.objects.add(word)
            }
            for pet in found.pets {
                p.pets.add(pet)
            }
        }
        .err(_) => {}
    }
}

/// What the photograph shows, as the device's model reads it in one go: the sentence search finds
/// it by, the words in it, and the objects and pets it holds. It replaces what the photo said
/// before, so analyzing again after the picture changed says what it shows now.
action analyze_photo(p: Photo) (icon: .sparkles, label: "Analyze", group: "Details") {
    match try vision.shows(vision.image_model, image: shown_picture(p)) {
        .ok(found) => {
            p.description = found.description
            p.recognized = found.text
            for word in found.objects {
                p.objects.add(word)
            }
            for pet in found.pets {
                p.pets.add(pet)
            }
        }
        .err(_) => {}
    }
}

/// Keeps the part of the photograph its person chose: `x`, `y`, `width` and `height` are
/// fractions of the picture as the window shows it, so every member and device reads the crop.
action crop_photo(p: Photo, x: Float, y: Float, width: Float, height: Float) uses image (
    icon: .crop,
    label: "Crop",
    group: "Edits",
) {
    // Keeping the whole picture changes nothing, and a person aiming for it needs no round trip
    // to the device to be told so.
    if width <= 0.0 or height <= 0.0 or width >= 1.0 and height >= 1.0 {
        return
    }
    let chosen = turned_back([x, y, width, height], p.rotation)
    let kept = if p.crop.is_empty() { chosen } else { placed_in(p.crop, chosen) }
    develop(p, p.rotation, kept)
}

/// The part of the upright picture a person chose in the window: the picture there is turned
/// clockwise, so its place is turned back the same number of quarter turns before it is kept.
fn turned_back(rect: List<Float>, quarters: Int) -> List<Float> {
    let x = rect.at(0) ?? 0.0
    let y = rect.at(1) ?? 0.0
    let w = rect.at(2) ?? 1.0
    let h = rect.at(3) ?? 1.0
    let q = if quarters >= 3 { 3 } else if quarters <= 0 { 0 } else { quarters }
    if q == 0 {
        [x, y, w, h]
    } else if q == 1 {
        [y, 1.0 - (x + w), h, w]
    } else if q == 2 { [1.0 - (x + w), 1.0 - (y + h), w, h] } else { [1.0 - (y + h), x, h, w] }
}

/// The part `chosen` of the part `kept` the person already kept: a second crop narrows the first
/// rather than throwing it away, so cropping a turned photograph never loses what they chose.
fn placed_in(kept: List<Float>, chosen: List<Float>) -> List<Float> {
    let left = kept.at(0) ?? 0.0
    let top = kept.at(1) ?? 0.0
    let across = kept.at(2) ?? 1.0
    let down = kept.at(3) ?? 1.0
    [
        left + (chosen.at(0) ?? 0.0) * across,
        top + (chosen.at(1) ?? 0.0) * down,
        (chosen.at(2) ?? 1.0) * across,
        (chosen.at(3) ?? 1.0) * down,
    ]
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

/// Changes the details of every named photo as one change: a field left `keep` keeps what each
/// photo has, `clear` takes it off all of them, `set` gives them all one value, and tags and
/// people are added or taken off across the selection, so every photo keeps the rest of its own.
/// The selection is names in a list, so a person's selection, an agent's and a program's are the
/// same change, undone at once.
action edit_selected(
    lib: Library,
    ids: List<Text>,
    tags: Change = .keep,
    tag_names: List<Text> = [],
    tags_off: List<Text> = [],
    people: Change = .keep,
    person_names: List<Text> = [],
    people_off: List<Text> = [],
    folder: Change = .keep,
    folder_text: Option<Text> = .none,
    captured_at: Change = .keep,
    captured_text: Option<Text> = .none,
    description: Change = .keep,
    description_text: Option<Text> = .none,
    place: Change = .keep,
    place_name: Option<Text> = .none,
    latitude: Float = 0.0,
    longitude: Float = 0.0,
) (label: "Edit details", did: "changed the details of photos") {
    require tags != .keep
        or not tag_names.is_empty()
        or not tags_off.is_empty()
        or people != .keep
        or not person_names.is_empty()
        or not people_off.is_empty()
        or folder != .keep
        or captured_at != .keep
        or description != .keep
        or place != .keep "Fill in a field to change something"
    let chosen = selected(lib, ids)
    for p in chosen {
        if tags == .clear or tags == .set {
            for held in p.tags.map(t -> t) {
                p.tags.remove(held)
            }
        }
        if tags == .add or tags == .set {
            for name in tag_names {
                if name != "" { p.tags.add(name) }
            }
        }
        if tags == .remove {
            for name in tag_names {
                p.tags.remove(name)
            }
        }
        for name in tags_off {
            p.tags.remove(name)
        }
        if people == .clear or people == .set {
            for held in p.people.map(n -> n) {
                p.people.remove(held)
            }
        }
        if people == .add or people == .set {
            for name in person_names {
                if name != "" { p.people.add(name) }
            }
        }
        if people == .remove {
            for name in person_names {
                p.people.remove(name)
            }
        }
        for name in people_off {
            p.people.remove(name)
        }
        if folder != .keep {
            if folder == .clear {
                p.folder = ""
            } else {
                p.folder = folder_text ?? ""
            }
        }
        if captured_at != .keep {
            if captured_at == .clear {
                p.captured_at = ""
            } else {
                p.captured_at = captured_text ?? ""
            }
        }
        if description != .keep {
            if description == .clear {
                p.description = ""
            } else {
                p.description = description_text ?? ""
            }
        }
        if place == .clear {
            p.location = ""
            p.latitude = 0.0
            p.longitude = 0.0
        } else if place == .set {
            p.location = place_name ?? ""
            p.latitude = latitude
            p.longitude = longitude
        }
    }
    // The people the library knows keep the same photos as the photos name: a person added or
    // taken off here shows on their page too.
    if people == .clear {
        for known in lib.people {
            for id in ids {
                known.photos.remove(id)
            }
        }
    } else if people == .add or people == .set {
        for name in person_names {
            if let known = lib.people.find(x -> x.name == name) {
                for id in ids {
                    known.photos.add(id)
                }
            }
        }
    } else if people == .remove {
        for name in person_names {
            if let known = lib.people.find(x -> x.name == name) {
                for id in ids {
                    known.photos.remove(id)
                }
            }
        }
    }
    for name in people_off {
        if let known = lib.people.find(x -> x.name == name) {
            for id in ids {
                known.photos.remove(id)
            }
        }
    }
}

/// The photos the library keeps under the names in `ids`, in the library's order.
fn selected(lib: Library, ids: List<Text>) -> List<Photo> {
    lib.photos.filter(p -> ids.contains(p.id))
}

/// The one value every photo holds for a field, and none where the photos differ or none is
/// selected: what a batch editor reads to say Mixed.
fn shared_value(values: List<Text>) -> Option<Text> {
    if let first = values.first() {
        if values.all(v -> v == first) { .some(first) } else { .none }
    } else { .none }
}

/// What a field says over the selected photos: the value they all hold, the word for differing
/// values where it differs between them, and the word for an empty one where it is empty.
fn shared_line(values: List<Text>) -> Text {
    if let one = shared_value(values) {
        if one == "" { std.say("Not set") } else { one }
    } else { std.say("Mixed") }
}

/// What a list of names on every photo says over the selection, the same way.
fn shared_names_line(values: List<Text>) -> Text {
    if let one = shared_value(values) {
        if one == "" { std.say("None") } else { one }
    } else { std.say("Mixed") }
}

/// The tags or people on a photo, sorted and read as one line, so two photos agree when they
/// carry the same names.
fn names_of(names: List<Text>) -> Text {
    names.sort_by(n -> n).join(", ")
}

/// The names a person typed in a field, separated by commas: what a batch editor adds or takes
/// off every selected photo.
fn names_from(typed: Text) -> List<Text> {
    typed.split(",").map(n -> n.trim()).filter(n -> n != "")
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

/// Opens an album draft: the album `id` names as it stands now, or an empty one to start. The
/// person chooses its photos in the library and nothing about the album changes until they save
/// the draft, so a change another member makes meanwhile is reviewed, not overwritten unseen.
action start_album_draft(lib: Library, id: Text) (icon: .pencil, label: "Edit album") {
    if let held = lib.albums.find(a -> a.id == id) {
        lib.album_draft_id = held.id
        lib.album_draft_name = held.name
        lib.album_draft_photos = held.photos.map(photo -> photo)
    } else {
        lib.album_draft_id = ""
        lib.album_draft_name = ""
        lib.album_draft_photos = []
    }
    lib.album_draft_open = true
    lib.notice = ""
}

/// Chooses a photo for the album draft, or takes it out of the draft when it is already in it.
action album_draft_toggle(lib: Library, p: Photo) (icon: .folder, group: "Album draft") {
    lib.album_draft_photos = toggled(lib.album_draft_photos, p.id)
}

/// Closes the album draft, saving nothing.
action cancel_album_draft(lib: Library) (label: "Cancel") {
    lib.album_draft_open = false
    lib.album_draft_id = ""
    lib.album_draft_name = ""
    lib.album_draft_photos = []
    lib.notice = "Album changes discarded"
}

/// Saves the album draft. A photo the draft names that the library no longer holds, or an album
/// another member deleted while it was open, is a change made first by someone else: the draft
/// stays as it is and says so, so they review it and save again. What the save does add and take
/// away merges with a change another member made meanwhile.
action save_album(lib: Library) (icon: .check, label: "Save album") {
    let known = lib.photos.map(p -> p.id)
    let chosen = lib.album_draft_photos.filter(id -> known.contains(id))
    if chosen.len() != lib.album_draft_photos.len() {
        lib.notice = "Someone else changed this album. Review it and save again."
        return
    }
    let name = lib.album_draft_name.trim()
    if name == "" {
        lib.notice = "Name the album first."
        return
    }
    if lib.album_draft_id == "" {
        let made = lib.albums.insert(Album(name: name))
        for id in chosen {
            made.photos.add(id)
        }
        lib.album_draft_id = made.id
        lib.album_draft_open = false
        lib.notice = "Album saved"
        return
    }
    if let album = lib.albums.find(a -> a.id == lib.album_draft_id) {
        album.name = name
        // Only the photos that differ are added and taken away, so two people's choices merge.
        for p in lib.photos {
            let listed = album.photos.contains(p.id)
            let wanted = chosen.contains(p.id)
            if wanted and not listed {
                album.photos.add(p.id)
            } else if listed and not wanted { album.photos.remove(p.id) }
        }
        lib.album_draft_open = false
        lib.notice = "Album saved"
    } else {
        lib.notice = "Someone else changed this album. Review it and save again."
    }
}

/// Deletes the album the draft is of. Its photos stay in the library.
destructive action delete_album_draft(lib: Library) (icon: .trash, label: "Delete album") {
    lib.album_draft_open = false
    if let album = lib.albums.find(a -> a.id == lib.album_draft_id) {
        lib.albums.remove(album)
        lib.notice = "Album deleted"
    } else {
        lib.notice = "Someone else changed this album. Review it and save again."
    }
    lib.album_draft_id = ""
    lib.album_draft_name = ""
    lib.album_draft_photos = []
}

/// Takes down what the library last had to say.
action dismiss_notice(lib: Library) (icon: .x, label: "Dismiss") {
    lib.notice = ""
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

/// This person's face scan, started where they have none.
action face_scan(lib: Library) -> FaceScan {
    if let held = lib.face_scans.first() { held } else { lib.face_scans.insert(FaceScan()) }
}

/// Lets this device look for faces in the photos, or stops it: turning it off ends a scan under
/// way, and keeps every name already given.
action set_recognition(lib: Library, on: Bool) (icon: .scan_line, label: "On-device recognition") {
    let scan = face_scan(lib)
    scan.on = on
    if not on {
        scan.state = ""
    }
}

/// Starts scanning the photos on this device for faces, or goes on with a paused scan: the model
/// reads twelve at a time, and the window takes the next twelve as each batch ends, until every
/// photo nobody is named on was read. Each face it finds is a suggestion of the scanner's own.
action scan_faces(lib: Library) uses image (icon: .scan_line, label: "Scan for faces") {
    require recognition_on(lib) "Turn recognition on to scan this device's photos for faces"
    let scan = face_scan(lib)
    scan.state = if read_batch(lib, scan) { "running" } else { "" }
}

/// Whether this person let the device look for faces.
fn recognition_on(lib: Library) -> Bool {
    if let s = lib.face_scans.first() { s.on } else { false }
}

/// One batch of a running scan: the next twelve photos it has not read.
action scan_step(lib: Library) uses image {
    let scan = face_scan(lib)
    if scan.state != "running" or not scan.on {
        return
    }
    if not read_batch(lib, scan) {
        scan.state = ""
    }
}

/// Reads the next twelve photos `scan` has not read and nobody is named on yet. Answers whether there were any to read.
action read_batch(lib: Library, scan: FaceScan) -> Bool uses image {
    let batch = lib.photos
        .filter(
            p -> p.picture != "" and p.people.is_empty() and not scan.scanned.contains(p.id),
        )
        .take(12)
    if batch.is_empty() {
        return false
    }
    let images = batch.map(p -> shown_picture(p))
    match try vision.faces(vision.image_model, images: images) {
        .ok(groups) => {
            for group in groups {
                let ids = batch.filter(p -> group.contains(shown_picture(p))).map(p -> p.id)
                if not ids.is_empty() and not known_face(lib, ids) {
                    let made = lib.suggestions.insert(Suggestion())
                    for id in ids {
                        made.photos.add(id)
                    }
                }
            }
        }
        .err(_) => {}
    }
    for p in batch {
        scan.scanned.add(p.id)
    }
    true
}

/// Pauses the scan under way: scanning again goes on with the next photos.
action pause_scan(lib: Library) (icon: .pause, label: "Pause scanning") {
    let scan = face_scan(lib)
    if scan.state == "running" {
        scan.state = "paused"
    }
}

/// Deletes the face data this person's scans made on this device: the faces nobody named yet and
/// what the scan had read. The people already named are kept.
destructive action delete_face_data(lib: Library) (icon: .trash, label: "Delete local face data") {
    for s in lib.suggestions.filter(x -> not x.sent) {
        lib.suggestions.remove(s)
    }
    let scan = face_scan(lib)
    scan.state = ""
    for id in scan.scanned {
        scan.scanned.remove(id)
    }
}

/// Whether a group of photos is already a suggestion, or already on a person: the same face read
/// again does not become a second suggestion.
fn known_face(lib: Library, ids: List<Text>) -> Bool {
    for s in lib.suggestions {
        if s.photos.len() == ids.len() and ids.all(id -> s.photos.contains(id)) {
            return true
        }
    }
    for p in lib.people {
        if ids.all(id -> p.photos.contains(id)) {
            return true
        }
    }
    false
}

/// Names a face: its photos join the person with that name, and the suggestion is sent, so the
/// workspace keeps it.
action approve_suggestion(lib: Library, s: Suggestion, name: Text = s.name) (
    icon: .check,
    label: "Approve the name",
) {
    require name != "" "Enter a name first"
    s.name = name
    let person = person_named(lib, name)
    for id in s.photos {
        person.photos.add(id)
        if let p = lib.photos.find(x -> x.id == id) { p.people.add(name) }
    }
    s.sent = true
}

/// Puts a face's photos on a person the library already knows, and sends the suggestion.
action merge_suggestion(lib: Library, s: Suggestion, person: Person) (icon: .users) {
    s.name = person.name
    for id in s.photos {
        person.photos.add(id)
        if let p = lib.photos.find(x -> x.id == id) { p.people.add(person.name) }
    }
    s.sent = true
}

/// Takes a suggestion away: the face is not offered again, since the photos are not rescanned
/// into one while they are unnamed only by choice.
destructive action dismiss_suggestion(lib: Library, s: Suggestion) (
    icon: .x,
    label: "Dismiss the suggestion",
) {
    lib.suggestions.remove(s)
}

/// The person the library knows by `name`, started where it knows none.
action person_named(lib: Library, name: Text) -> Person {
    if let held = lib.people.find(p -> p.name == name) {
        held
    } else { lib.people.insert(Person(name: name)) }
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
    state big: Bool = false
    state selection: List<Text> = []
    state presenting: Bool = false
    state slide: Int = 0
    state zoom: Float = 1.0
    state confirming: Bool = false
    state shared: Text = ""
    state cropping: Bool = false
    state crop_edges: List<Float> = [0.0, 0.0, 1.0, 1.0]
    // The details of many photos at once: open while the person edits the selection, and what
    // they typed in each field, empty for a field they left alone.
    state editing_details: Bool = false
    state details_tags_add: Text = ""
    state details_tags_remove: Text = ""
    state details_people_add: Text = ""
    state details_people_remove: Text = ""
    state details_folder: Text = ""
    state details_captured: Text = ""
    state details_description: Text = ""
    state details_place: Text = ""
    state details_setting_place: Bool = false
    state details_latitude: Float = 0.0
    state details_longitude: Float = 0.0
    // The month and the day the timeline is opened at, empty for every month and every day.
    state timeline_month: Text = ""
    state timeline_day: Text = ""
    // Whether the person is about to delete the album their draft is of.
    state album_delete_confirm: Bool = false
    // Whether the viewer shows the photograph as it came rather than as the person edited it.
    state comparing: Bool = false
    ui.NavShell(
        content_kind: .page,
        title: if let p = lib.photos.find(x -> x.id == viewing) {
            p.name
        } else { place_title(lib, place) },
        sidebar_title: "Photos",
    ) {
        // A picture dropped anywhere on the window joins the library.
        on drop(files: List<File>): import_folder(lib, files)
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
                    ui.Button("Edit details", icon: .pencil) {
                        on press: set(details_tags_add, "")
                            .then(set(details_tags_remove, ""))
                            .then(set(details_people_add, ""))
                            .then(set(details_people_remove, ""))
                            .then(set(details_folder, ""))
                            .then(set(details_captured, ""))
                            .then(set(details_description, ""))
                            .then(set(details_place, ""))
                            .then(set(details_setting_place, false))
                            .then(set(details_latitude, 0.0))
                            .then(set(details_longitude, 0.0))
                            .then(set(editing_details, true))
                    }
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
                        import_folder(lib),
                        accept: ["image/png", "image/jpeg", "image/webp"],
                    )
                }
                ui.Button("New album", icon: .folder_plus) {
                    on press: start_album_draft(lib, "")
                }
                ui.Button("Select", icon: .check) {
                    on press: set(selecting, true)
                }
                ui.Button(
                    if big { "Smaller thumbnails" } else { "Larger thumbnails" },
                    icon: if big { .layout_grid } else { .maximize },
                    icon_only: true,
                ) {
                    on press: set(big, not big)
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
                    ui.NavItem("analysis", "Analysis", icon: .sparkles) {
                        on select: set(place, "analysis").then(set(viewing, ""))
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
            stack(gap: .related, fill) {
                // What the last import did: what arrived, and what could not, so a folder of a
                // thousand pictures says at once how many of them the library has.
                if lib.import_job != "" {
                    import_progress(lib)
                } else if lib.import_found > 0 {
                    ui.Banner(
                        if lib.import_stopped {
                            "Import stopped: {lib.import_done} of {lib.import_found} imported"
                        } else {
                            "Imported {lib.import_done} of {lib.import_found}"
                        },
                        message: if lib.import_issues.is_empty() {
                            "Every file became a photo."
                        } else { lib.import_issues.join(", ") },
                        kind: if lib.import_issues.is_empty() { .info } else { .warning },
                        action: "Dismiss",
                    ) {
                        on act: dismiss_import(lib)
                    }
                    if not lib.unread.is_empty() {
                        ui.Button("Try again", icon: .refresh) {
                            on press: retry_import(lib)
                        }
                    }
                }
                if lib.notice != "" {
                    ui.Banner(lib.notice, kind: .info, action: "Dismiss") {
                        on act: dismiss_notice(lib)
                    }
                }
                if presenting {
                    present(lib, slide, presenting)
                } else if editing_details {
                    batch_details(
                        lib,
                        editing_details,
                        selecting,
                        selection,
                        details_tags_add,
                        details_tags_remove,
                        details_people_add,
                        details_people_remove,
                        details_folder,
                        details_captured,
                        details_description,
                        details_place,
                        details_setting_place,
                        details_latitude,
                        details_longitude,
                    )
                } else if lib.album_draft_open
                    and (place == "library" or place == lib.album_draft_id) {
                    album_editor(lib, album_delete_confirm)
                } else if let album = lib.albums.find(a -> a.id == place) {
                    album_place(
                        lib,
                        album,
                        viewing,
                        selecting,
                        selection,
                        confirming,
                        shared,
                        comparing,
                        big,
                    )
                } else if let p = lib.photos.find(x -> x.id == viewing) {
                    ui.PageSlide(level: 1) {
                        viewer(
                            lib,
                            p,
                            viewing,
                            zoom,
                            confirming,
                            shared,
                            cropping,
                            crop_edges,
                            comparing,
                        )
                    }
                } else if place == "people" {
                    people_list(lib, place, viewing)
                } else if place == "analysis" {
                    analysis_page(lib)
                } else if lib.photos.filter(p -> shown(lib, place, p)).is_empty() {
                    empty_place(lib, place)
                } else if place == "timeline" {
                    timeline_page(lib, viewing, timeline_month, timeline_day)
                } else if place == "places" {
                    places_page(lib, viewing)
                } else {
                    library_grid(
                        lib,
                        place,
                        viewing,
                        selecting,
                        selection,
                        confirming,
                        shared,
                        comparing,
                        big,
                    )
                }
            }
        }
    }
}

/// The import under way, as its job's record says it now: how many pictures it has read of how
/// many, what it could not, and a way to stop it.
view import_progress(lib: Library) {
    let record = if let j = engine.job(lib.import_job) {
        json.decode<ImportRecord>(j.detail)
    } else { .err("not started") }
    row(gap: .related, align: .center) {
        match record {
            .ok(now) => {
                ui.ProgressBar(
                    value: if now.found > 0 {
                        now.done.to_float() / now.found.to_float()
                    } else { 0.0 },
                    label: "Importing {now.done} of {now.found}",
                    fill,
                )
                if not now.issues.is_empty() {
                    ui.Text("{now.issues.len()} could not be read", role: .caption, secondary: true)
                }
            }
            .err(_) => {
                ui.ProgressBar(label: "Importing {lib.import_found} pictures", fill)
            }
        }
        ui.Button("Stop", icon: .square) {
            on press: stop_import(lib)
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
    stack(gap: .section, fill) {
        face_recognition(lib)
        // The faces nobody has named: each is this device's own until its name is approved, so
        // another member's suggestions never appear here.
        if not lib.suggestions.is_empty() {
            ui.Section("Faces to name") {
                for s in lib.suggestions {
                    stack(gap: .tight) {
                        row(gap: .tight, align: .center) {
                            ui.Avatar(name: s.name)
                            ui.TextInput(bind: s.name, placeholder: "Name this person", fill)
                            ui.Button("Approve", icon: .check, role: .primary) {
                                on press: approve_suggestion(lib, s, s.name)
                            }
                            if not lib.people.is_empty() {
                                ui.MenuButton("Merge", icon: .users) {
                                    for person in lib.people {
                                        ui.MenuItem(person.name) {
                                            on choose: merge_suggestion(lib, s, person)
                                        }
                                    }
                                }
                            }
                            ui.Button(
                                "",
                                icon: .x,
                                icon_only: true,
                                accessible_name: "Dismiss the suggestion",
                            ) {
                                on press: dismiss_suggestion(lib, s)
                            }
                        }
                        ui.Text(
                            "{s.photos.len()} photos suggested on this device, shared only once you name it.",
                            role: .caption,
                            secondary: true,
                        )
                    }
                }
            }
        }
        if lib.people.is_empty() and lib.suggestions.is_empty() {
            ui.EmptyState(
                "No people yet",
                message: "The vision service proposes the people in your photos; name them, merge them and split them here.",
                icon: .users,
            )
        } else if not lib.people.is_empty() {
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
}

/// This person's on-device recognition: the switch, how far a scan has got, pause and go on,
/// and deleting the face data it made. A running scan takes its next batch as each one ends.
view face_recognition(lib: Library) {
    let scan = lib.face_scans.first()
    let on = if let s = scan { s.on } else { false }
    let state = if let s = scan { s.state } else { "" }
    let scanned = if let s = scan { s.scanned.len() } else { 0 }
    let total = lib.photos.filter(p -> p.picture != "" and p.people.is_empty()).len()
    ui.Section("On-device recognition") {
        ui.Toggle(shown: on, label: "Recognize faces in the photos on this device") {
            on check(now): set_recognition(lib, now)
        }
        if not on {
            ui.Text(
                "Recognition is off. Turn it on to get suggestions from this device.",
                role: .caption,
                secondary: true,
            )
        } else if not vision.image_model.available {
            ui.Text(vision.image_model.detail, role: .caption, secondary: true)
        } else if state == "running" {
            every 1s: scan_step(lib)
            row(gap: .related, align: .center) {
                ui.ProgressBar(
                    value: if total > 0 { scanned.to_float() / total.to_float() } else { 0.0 },
                    label: "Scanned {scanned} of {total} photos",
                    fill,
                )
                ui.Button("Pause scanning", icon: .pause) {
                    on press: pause_scan(lib)
                }
            }
        } else {
            row(gap: .related, align: .center) {
                if state == "paused" {
                    ui.Text("Scanning is paused at {scanned} of {total} photos.", fill)
                }
                ui.Button(
                    if state == "paused" { "Go on scanning" } else { "Scan for faces" },
                    icon: .scan_line,
                ) {
                    on press: scan_faces(lib)
                }
            }
        }
        if scanned > 0 or not lib.suggestions.filter(x -> not x.sent).is_empty() {
            ui.Button("Delete local face data", icon: .trash, role: .destructive) {
                on press: delete_face_data(lib)
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

/// What the selected photos' places say: the one place they all hold, the word for differing
/// places where they were taken in different ones, and the word for an empty place where none
/// of them has one.
fn place_value(photos: List<Photo>) -> Text {
    let values = photos.map(
        p -> if p.location == "" and p.latitude == 0.0 and p.longitude == 0.0 {
            ""
        } else if p.location == "" {
            "{p.latitude}, {p.longitude}"
        } else {
            "{p.location} ({p.latitude}, {p.longitude})"
        },
    )
    shared_names_line(values)
}

/// The details of many photos at once: what the selected photos agree on, the word Mixed where
/// they differ, and the one change the person makes to all of them. Only a field they fill in is
/// changed; every other field keeps what each photo has.
view batch_details(
    lib: Library,
    editing: State<Bool>,
    selecting: State<Bool>,
    selection: State<List<Text>>,
    tags_add: State<Text>,
    tags_remove: State<Text>,
    people_add: State<Text>,
    people_remove: State<Text>,
    folder: State<Text>,
    captured: State<Text>,
    description: State<Text>,
    place: State<Text>,
    setting_place: State<Bool>,
    latitude: State<Float>,
    longitude: State<Float>,
) {
    let chosen = selected(lib, selection)
    stack(padding: .group, gap: .related, max: 44rem) {
        ui.Text("Details of {chosen.len()} photos", role: .title)
        ui.Text(
            "Only the fields you fill in change; every photo keeps the rest. A field the photos differ in says Mixed.",
            role: .caption,
            secondary: true,
        )
        ui.Section("Organize") {
            stack(gap: .related) {
                ui.Text(
                    "Folder: {shared_line(chosen.map(p -> p.folder))}",
                    role: .caption,
                    secondary: true,
                )
                ui.TextInput(bind: folder, placeholder: "Folder to give every photo", fill)
                ui.Text(
                    "Captured: {shared_line(chosen.map(p -> p.captured_at))}",
                    role: .caption,
                    secondary: true,
                )
                ui.TextInput(bind: captured, placeholder: "Captured time to give every photo", fill)
                ui.Text(
                    "Description: {shared_line(chosen.map(p -> p.description))}",
                    role: .caption,
                    secondary: true,
                )
                ui.TextInput(
                    bind: description,
                    placeholder: "Description to give every photo",
                    lines: 2,
                    fill,
                )
            }
        }
        ui.Section("Tags") {
            stack(gap: .related) {
                ui.Text(
                    "Tags: {shared_names_line(chosen.map(p -> names_of(p.tags.map(t -> t))))}",
                    role: .caption,
                    secondary: true,
                )
                row(gap: .related, wrap: true) {
                    ui.TextInput(
                        bind: tags_add,
                        placeholder: "Tags to add, separated by commas",
                        fill,
                    )
                    ui.TextInput(
                        bind: tags_remove,
                        placeholder: "Tags to take off, separated by commas",
                        fill,
                    )
                }
            }
        }
        ui.Section("People") {
            stack(gap: .related) {
                ui.Text(
                    "People: {shared_names_line(chosen.map(p -> names_of(p.people.map(n -> n))))}",
                    role: .caption,
                    secondary: true,
                )
                row(gap: .related, wrap: true) {
                    ui.TextInput(
                        bind: people_add,
                        placeholder: "People to add, separated by commas",
                        fill,
                    )
                    ui.TextInput(
                        bind: people_remove,
                        placeholder: "People to take off, separated by commas",
                        fill,
                    )
                }
            }
        }
        ui.Section("Place") {
            stack(gap: .related) {
                ui.Text("Place: {place_value(chosen)}", role: .caption, secondary: true)
                ui.Checkbox(bind: setting_place, label: "Give every photo the same place")
                if setting_place {
                    ui.TextInput(bind: place, placeholder: "Place name", fill)
                    row(gap: .related, wrap: true) {
                        ui.NumberInput(
                            bind: latitude,
                            min: -90.0,
                            max: 90.0,
                            precision: 6,
                            label: "Latitude",
                        )
                        ui.NumberInput(
                            bind: longitude,
                            min: -180.0,
                            max: 180.0,
                            precision: 6,
                            label: "Longitude",
                        )
                    }
                }
            }
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button("Save", icon: .check, role: .primary) {
                on press: edit_selected(
                        lib,
                        selection,
                        tags: if names_from(tags_add).is_empty() { .keep } else { .add },
                        tag_names: names_from(tags_add),
                        tags_off: names_from(tags_remove),
                        people: if names_from(people_add).is_empty() { .keep } else { .add },
                        person_names: names_from(people_add),
                        people_off: names_from(people_remove),
                        folder: if folder.trim() == "" { .keep } else { .set },
                        folder_text: if folder.trim() == "" { .none } else { .some(folder.trim()) },
                        captured_at: if captured.trim() == "" { .keep } else { .set },
                        captured_text: if captured.trim() == "" {
                            .none
                        } else { .some(captured.trim()) },
                        description: if description.trim() == "" { .keep } else { .set },
                        description_text: if description.trim() == "" {
                            .none
                        } else { .some(description.trim()) },
                        place: if setting_place { .set } else { .keep },
                        place_name: if place.trim() == "" { .none } else { .some(place.trim()) },
                        latitude: latitude,
                        longitude: longitude,
                    )
                    .then(set(editing, false))
                    .then(set(selecting, false))
                    .then(set(selection, []))
            }
            ui.Button("Cancel") {
                on press: set(editing, false)
            }
            ui.MenuButton("Clear", icon: .trash) {
                ui.MenuItem("Folder on every photo") {
                    on choose: edit_selected(lib, selection, folder: .clear)
                }
                ui.MenuItem("Captured time") {
                    on choose: edit_selected(lib, selection, captured_at: .clear)
                }
                ui.MenuItem("Description") {
                    on choose: edit_selected(lib, selection, description: .clear)
                }
                ui.MenuSeparator()
                ui.MenuItem("All tags") {
                    on choose: edit_selected(lib, selection, tags: .clear)
                }
                ui.MenuItem("Everyone") {
                    on choose: edit_selected(lib, selection, people: .clear)
                }
                ui.MenuSeparator()
                ui.MenuItem("The place") {
                    on choose: edit_selected(lib, selection, place: .clear)
                }
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
    comparing: State<Bool>,
    big: Bool,
) {
    let photos = lib.photos.filter(p -> shown(lib, place, p))
    if selecting {
        ui.MediaGrid(size: if big { .regular } else { .small }, fill) {
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
        ui.MediaGrid(size: if big { .regular } else { .small }, fill) {
            for p in photos {
                ui.MediaCard(p, title: p.name, picture: card_picture(p)) {
                    on activate: check_picture(p)
                        .then(set(viewing, p.id))
                        .then(set(confirming, false))
                        .then(set(shared, ""))
                        .then(set(comparing, false))
                }
            }
        }
    }
}

/// The month a capture time names, as `2026-07`: what the timeline opens a month at.
fn capture_month(when: Text) -> Text {
    day_word(capture_day(when)).slice(0, 7)
}

/// The library by the day each photograph was taken: newest first, a heading for each day and
/// its photographs under it, and those the camera gave no time for at the end. The month and the
/// day a person picks narrow it to that month or that day, and picking none shows them all.
view timeline_page(lib: Library, viewing: State<Text>, month: State<Text>, day: State<Text>) {
    let dated = lib.photos.filter(p -> p.captured_at != "").sort_by(p -> p.captured_at, desc)
    let undated = lib.photos.filter(p -> p.captured_at == "")
    let months = dated
        .group(p -> capture_month(p.captured_at))
        .sort_by(g -> g.key, desc)
        .map(g -> g.key)
    let days = dated
        .filter(p -> month == "" or capture_month(p.captured_at) == month)
        .group(p -> day_word(capture_day(p.captured_at)))
        .sort_by(g -> g.key, desc)
        .map(g -> g.key)
    let opened = dated.filter(
        p -> (month == "" or capture_month(p.captured_at) == month)
            and (day == "" or day_word(capture_day(p.captured_at)) == day),
    )
    stack(gap: .section, fill) {
        row(gap: .tight, align: .center, wrap: true) {
            ui.MenuButton(if month == "" { "All months" } else { month }, icon: .calendar) {
                ui.MenuItem("All months") {
                    on choose: set(month, "").then(set(day, ""))
                }
                for m in months {
                    ui.MenuItem(m) {
                        on choose: set(month, m).then(set(day, ""))
                    }
                }
            }
            if month != "" {
                ui.MenuButton(if day == "" { "Every day" } else { day }, icon: .calendar) {
                    ui.MenuItem("Every day") {
                        on choose: set(day, "")
                    }
                    for d in days {
                        ui.MenuItem(d) {
                            on choose: set(day, d)
                        }
                    }
                }
            }
        }
        for d in opened.group(p -> capture_day(p.captured_at)).sort_by(g -> g.key, desc) {
            stack(gap: .related) {
                ui.Text(day_word(d.key), role: .subheading)
                ui.MediaGrid(size: .small, fill) {
                    for p in d.items {
                        ui.MediaCard(p, title: p.name, picture: card_picture(p)) {
                            on activate: set(viewing, p.id)
                        }
                    }
                }
            }
        }
        if not undated.is_empty() and month == "" and day == "" {
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
    state list: Bool = false
    let mapped = lib.photos.filter(p -> p.latitude != 0.0 or p.longitude != 0.0)
    let named = lib.photos.filter(
        p -> p.location != "" and p.latitude == 0.0 and p.longitude == 0.0,
    )
    stack(gap: .group, fill) {
        row(gap: .tight) {
            ui.Button("Map", icon: .map_pin, toggled: not list) {
                on press: set(list, false)
            }
            ui.Button("List", icon: .list, toggled: list) {
                on press: set(list, true)
            }
        }
        if list {
            ui.VirtualList(selection_follows_focus: true, in_column: false) {
                on activate(p: Photo): set(viewing, p.id)
                for p in [mapped, named].flat_map(x -> x) {
                    ui.ListItem(item: p) {
                        ui.ListRow(
                            title: p.name,
                            secondary: if p.location != "" {
                                p.location
                            } else { "{p.latitude}, {p.longitude}" },
                            icon: .map_pin,
                        )
                    }
                }
            }
        } else {
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
    comparing: State<Bool>,
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
            ui.Text("{zoom * 100.0}%", role: .caption, numeric: true)
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
                ui.Button("Before", icon: .columns_2, toggled: comparing) {
                    on press: set(comparing, not comparing)
                }
            }
            ui.Button("Describe", icon: .sparkles) {
                on press: describe_photo(p)
            }
            ui.Button("Tags", icon: .tag) {
                on press: tag_photo(p)
            }
            ui.Button("Read the text", icon: .scan_line) {
                on press: recognize_text(p)
            }
            ui.Button("Copy", icon: .copy) {
                on press: copy_photo(p)
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
        if p.picture_state == "waiting" {
            ui.EmptyState(
                "Waiting for the photo",
                message: "It is still arriving from another device.",
                icon: .cloud_download,
            )
            // The bytes arrive from the other device while the person waits: look again.
            every 2s: check_picture(p)
        } else if p.picture_state == "unreadable" {
            ui.EmptyState(
                "Could not open this photo",
                message: "The file is not an image Photos can read.",
                icon: .circle_alert,
                error: true,
            )
            ui.Button("Try again", icon: .refresh) {
                on press: check_picture(p)
            }
        } else if cropping {
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
                    on press: set(cropping, false)
                        .then(
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
            if comparing {
                ui.Text("Before the edits", role: .caption, secondary: true)
            } else if p.edited != "" {
                ui.Text("After the edits", role: .caption, secondary: true)
            }
            ui.ImageViewer(
                if comparing { p.picture } else { shown_picture(p) },
                alt: p.name,
                aspect: 1.5,
                zoom: zoom,
            )
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

/// An album's photographs, and what the person does to the album itself: rename it, choose its
/// photos, or delete it.
view album_place(
    lib: Library,
    album: Album,
    viewing: State<Text>,
    selecting: State<Bool>,
    selection: State<List<Text>>,
    confirming: State<Bool>,
    shared: State<Text>,
    comparing: State<Bool>,
    big: Bool,
) {
    stack(gap: .related, fill) {
        row(gap: .related, align: .center, wrap: true) {
            ui.Text(album.name, role: .title)
            ui.Text("{album.photos.len()} photos", role: .caption, secondary: true)
            ui.Button("Edit album", icon: .pencil) {
                on press: start_album_draft(lib, album.id)
            }
        }
        if album.photos.is_empty() {
            ui.EmptyState(
                "This album is empty",
                message: "Edit the album to choose its photos.",
                icon: .folder,
            )
        } else {
            library_grid(
                lib,
                album.id,
                viewing,
                selecting,
                selection,
                confirming,
                shared,
                comparing,
                big,
            )
        }
    }
}

/// The album the person is choosing photos for: its name, the photos they picked, and the
/// library under them to pick from. The album changes when they save the draft, and not before.
view album_editor(lib: Library, deleting: State<Bool>) {
    stack(gap: .related, fill) {
        row(gap: .related, align: .center, wrap: true) {
            ui.TextInput(bind: lib.album_draft_name, title: true, placeholder: "Album name")
            ui.Text("{lib.album_draft_photos.len()} chosen", role: .caption, secondary: true)
            ui.Button("Save album", icon: .check, role: .primary) {
                on press: save_album(lib)
            }
            ui.Button("Cancel") {
                on press: cancel_album_draft(lib)
            }
            if lib.album_draft_id != "" {
                ui.Button("Delete album", icon: .trash) {
                    on press: set(deleting, true)
                }
            }
        }
        if deleting {
            row(gap: .related, align: .center, wrap: true) {
                ui.Text("Delete this album? Its photos stay in the library.", role: .caption)
                ui.Button("Delete", icon: .trash, role: .destructive) {
                    on press: delete_album_draft(lib).then(set(deleting, false))
                }
                ui.Button("Keep") {
                    on press: set(deleting, false)
                }
            }
        }
        ui.Text("Choose the photos for this album.", role: .caption, secondary: true)
        if lib.photos.is_empty() {
            ui.EmptyState(
                "Your library is empty",
                message: "Open a photo, or drop pictures on the window, to start your library.",
                icon: .image,
            )
        } else {
            ui.MediaGrid(size: .small, fill) {
                for p in lib.photos {
                    ui.MediaCard(
                        p,
                        title: p.name,
                        picture: card_picture(p),
                        selected: lib.album_draft_photos.contains(p.id),
                    ) {
                        on activate: album_draft_toggle(lib, p)
                    }
                }
            }
        }
    }
}

/// The library's analysis on this device: how far it has got, the model that read the pictures,
/// the photos it could not read, and the way to try them again. The work is the device's, so this
/// says what its run says and asks it to go on.
view analysis_page(lib: Library) {
    let run = vision.analysis
    stack(gap: .section, max: 72rem) {
        ui.Text(analysis_word(run.phase), role: .title)
        if run.total > 0 {
            ui.Text(
                "{run.completed} of {run.total} finished, {run.failed} failed",
                role: .body,
                secondary: true,
            )
            ui.ProgressBar(run.progress)
        } else if run.phase != "running" and run.phase != "paused" {
            ui.Text(
                if vision.image_model.available {
                    "Find photos by the text, scenes, objects and pets in them. The pictures stay on this device."
                } else { vision.image_model.detail },
                role: .body,
                secondary: true,
            )
        }
        row(gap: .related, align: .center, wrap: true) {
            if run.phase == "running" {
                ui.Button("Pause after this photo", icon: .pause) {
                    on press: vision.pause(vision.analysis)
                }
            } else if run.phase == "paused" {
                ui.Button("Resume", icon: .play) {
                    on press: vision.resume(vision.analysis)
                }
            } else if vision.image_model.available {
                ui.Button("Analyze photos", icon: .sparkles, role: .primary) {
                    on press: vision.start(vision.analysis)
                }
            }
            if run.failed > 0 and run.phase != "running" {
                ui.Button("Retry failed photos", icon: .refresh) {
                    on press: vision.retry(vision.analysis)
                }
            }
        }
        if run.model != "" {
            ui.Property("Model", run.model)
            ui.Property("Revision", run.revision.slice(0, 10), copyable: true)
        }
        if not run.failures.is_empty() {
            ui.Text("Needs attention", role: .subheading)
            for failure in run.failures {
                ui.ListRow(
                    title: failure_name(lib, failure.resource),
                    secondary: failure.reason,
                    icon: .circle_alert,
                )
            }
        }
    }
}

/// Where the library's analysis stands, as a heading a person reads.
fn analysis_word(phase: Text) -> Text {
    if phase == "running" {
        "Analyzing on this device"
    } else if phase == "paused" {
        "Paused"
    } else if phase == "complete" {
        "Finished"
    } else if phase == "cancelled" { "Stopped" } else { "Ready to analyze" }
}

/// The photo a failed analysis names: the kind and id its search entry carries end with the id
/// the library gave it, so the failure reads as the photo it is about.
fn failure_name(lib: Library, resource: Text) -> Text {
    if let p = lib.photos.find(x -> resource.ends_with(x.id)) { p.name } else { resource }
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
        if p.description != "" or not p.objects.is_empty() or not p.pets.is_empty() {
            ui.Section("What it shows") {
                if p.description != "" {
                    text(p.description, role: .body)
                }
                if not p.objects.is_empty() {
                    row(gap: .related, wrap: true) {
                        for word in p.objects {
                            ui.Chip(word, icon: .tag)
                        }
                    }
                }
                if not p.pets.is_empty() {
                    row(gap: .related, wrap: true) {
                        for pet in p.pets {
                            ui.Chip(pet, icon: .paw_print)
                        }
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
        row(gap: .related, wrap: true) {
            ui.NumberInput(bind: p.latitude, min: -90.0, max: 90.0, precision: 6, label: "Latitude")
            ui.NumberInput(
                bind: p.longitude,
                min: -180.0,
                max: 180.0,
                precision: 6,
                label: "Longitude",
            )
        }
        if p.recognized != "" {
            ui.Section("Text in the picture") {
                text(p.recognized, role: .reading)
            }
        }
        row(gap: .related, wrap: true) {
            ui.Button("Save photograph", icon: .download) {
                on press: file.save(export_photo(p), name: p.name)
            }
            ui.Button("Analyze", icon: .sparkles) {
                on press: analyze_photo(p)
            }
            ui.Button("Read the text", icon: .scan_line) {
                on press: recognize_text(p)
            }
            ui.Button("Copy", icon: .copy) {
                on press: copy_photo(p)
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
                if p.picture_state == "waiting" {
                    ui.EmptyState(
                        "Waiting for the photo",
                        message: "It is still arriving from another device.",
                        icon: .cloud_download,
                    )
                } else if p.picture_state == "unreadable" {
                    ui.EmptyState(
                        "Could not open this photo",
                        message: "The file is not an image Photos can read.",
                        icon: .circle_alert,
                        error: true,
                    )
                } else {
                    ui.ImageViewer(shown_picture(p), alt: p.name, aspect: 1.5, zoom: zoom)
                }
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
