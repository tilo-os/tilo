app so.tilo.camera "Camera" (
    icon: .camera,
    category: .multimedia,
    tagline: "Photos, clips and codes from this computer's cameras",
    keywords: ["camera", "photo", "photos", "picture", "video", "record", "code", "qr", "barcode", "scan", "scanner", "document", "webcam", "kamera", "foto", "fotos", "bild", "video", "aufnehmen", "code", "qr", "barcode", "scannen", "scanner", "dokument", "webcam"],
    splash_budget: 250,
)

use so.tilo.device as device
use so.tilo.session as session

/// Which lens the viewfinder shows: the rear one, one facing the person, or the wide one.
enum Facing {
    back
    front
    wide
}

/// A capture is a still picture or a clip.
enum CaptureKind {
    photo
    video
}

/// One code the person scanned, what it said and when it was seen.
type ScanEntry {
    raw: Text
    ts: Time
}

/// The capture taken last, handed to the app that owns its bytes: the picture or clip itself,
/// what it is, and the lens and size it was taken with.
type SharedCapture {
    file: File
    kind: CaptureKind
    ts: Time
    w: Int
    h: Int
    dur_ms: Int = 0
    facing: Facing = .back
    label: Text = ""
}

/// What one code says, read out of its payload.
type Wifi {
    ssid: Text
    auth: Text
    password: Text
    hidden: Bool
}

/// A contact card's fields, read out of a vCard or MECARD payload.
type Contact {
    name: Text
    phone: Text
    email: Text
    org: Text
}

/// How this person takes pictures: their choices, theirs alone, on whichever device they open
/// the camera.
type CapturePreferences (readers: author) {
    /// `12mp`, `4k`, `1080p` or `720p`.
    resolution: Text = "12mp"
    /// `jpeg` or `png`.
    photo_format: Text = "jpeg"
    /// Seconds between the shutter and the capture: 0, 3 or 10.
    timer: Int = 0
    grid: Bool = true
    level: Bool = false
    mirror_front: Bool = true
    geotag: Bool = false
    hdr: Bool = true
    default_lens: Facing = .back
    /// `photo` or `video`: the mode the viewfinder opens in.
    mode: Text = "photo"
    /// The lens the viewfinder shows.
    lens: Facing = .back
    /// The camera this device uses, by its node; empty asks the platform's default.
    device: Text = ""
    /// The newest frame the camera showed, kept as a picture.
    preview: Text = ""
    /// The first code the newest frame holds, and where it is in it.
    found: Text = ""
    found_x: Float = 0.0
    found_y: Float = 0.0
    found_w: Float = 0.0
    found_h: Float = 0.0
    /// The clip being recorded, by its engine job.
    recording: Text = ""
    /// The moment the self-timer fires, while it counts down.
    shutter_at: Option<Time> = .none
    /// Why the last capture, recording or look failed.
    trouble: Text = ""
}

/// One scanned page: its picture, the text read off it, and where it stands in the document.
type ScannedPage {
    file: File
    text: Text = ""
    order: Int = 0
}

/// The document being scanned: this person's own, so a half-finished document never reaches
/// anyone else.
type ScanSession (readers: author) {
    /// The scanner's device node; empty until one is chosen.
    scanner: Text = ""
    resolution: Int = 300
    /// `color`, `grayscale` or `lineart`.
    colour: Text = "color"
    /// `flatbed`, `feeder` or `feeder-duplex`.
    source: Text = "flatbed"
    /// `eng`, `deu`, `fra` or `spa`.
    language: Text = "eng"
    pages: Collection<ScannedPage>
    /// The scan running now, by its engine job.
    job: Text = ""
    failed: Text = ""
    selected: Int = 0
}

/// The camera's own resource: the codes it scanned, the capture it hands on, and each person's
/// preferences and document.
type Camera {
    scans: Collection<ScanEntry>
    recent: Option<SharedCapture>
    preferences: Collection<CapturePreferences>
    sessions: Collection<ScanSession>
}

root camera: Camera

default view viewfinder for Camera

// ---------------------------------------------------------------- what the engine is asked
type StillAsk {
    device: Text
    width: Int
    height: Int
    format: Text
}

type FrameAsk {
    device: Text
    width: Int
    height: Int
}

type CodesAsk {
    url: Text
    width: Int
}

type DecodedCode {
    text: Text
    x: Float
    y: Float
    w: Float
    h: Float
}

type CodesAnswer {
    codes: List<DecodedCode>
}

type CameraDevice {
    id: Text
    label: Text
}

type CamerasAnswer {
    devices: List<CameraDevice>
    prompt: Bool
}

type JobAnswer {
    job: Text
}

type JobState {
    state: Text
    url: Text
    error: Text
}

type RecordAsk {
    device: Text
    width: Int
    height: Int
}

type RecordAskJob {
    job: Text
}

type RecordStopped {
    video: RecordedClip
}

type RecordedClip {
    name: Text
    duration_ms: Int
    w: Int
    h: Int
    size: Int
}

type ScanAsk {
    scanner: Text
    resolution: Int
    colour: Text
    source: Text
    name: Text
}

type OcrAsk {
    url: Text
    language: Text
}

type OcrAnswer {
    text: Text
    words: Int
    deskew: Float
}

type PdfPage {
    url: Text
    text: Text
}

type PagesAsk {
    pages: List<PdfPage>
    name: Text
}

type UrlAnswer {
    url: Text
}

/// What kind of payload a code holds: `link`, `wifi`, `contact`, `email`, `phone`, `place` or
/// `text`.
// ---------------------------------------------------------------- what a code says
fn kind_of(raw: Text) -> Text {
    let lower = raw.trim().lower()
    if lower.starts_with("http://") or lower.starts_with("https://") {
        "link"
    } else if lower.starts_with("wifi:") {
        "wifi"
    } else if lower.starts_with("begin:vcard") or lower.starts_with("mecard:") {
        "contact"
    } else if lower.starts_with("mailto:") {
        "email"
    } else if lower.starts_with("tel:") {
        "phone"
    } else if lower.starts_with("geo:") { "place" } else { "text" }
}

/// The value of the property `key` on one line of a payload, or empty.
fn field_after(line: Text, key: Text) -> Text {
    if let at = line.find(":") {
        let prop = line.slice(0, at).split(";").at(0) ?? ""
        if prop.trim().lower() == key.trim().lower() {
            line.slice(at + 1, line.len()).trim()
        } else { "" }
    } else { "" }
}

/// The value the field `key` of a `;`-separated payload holds, or empty.
fn field_value(fields: List<Text>, key: Text) -> Text {
    for field in fields {
        if let at = field.find(":") {
            if field.slice(0, at).trim().upper() == key {
                return field.slice(at + 1, field.len()).trim()
            }
        }
    }
    ""
}

fn wifi_of(raw: Text) -> Wifi {
    let body = if raw.starts_with("WIFI:") { raw.slice(5, raw.len()) } else { raw }
    let fields = body.split(";")
    let auth = field_value(fields, "T")
    let hidden = field_value(fields, "H").lower() == "true"
    Wifi(
        ssid: field_value(fields, "S"),
        auth: if auth == "" { "Open" } else { auth },
        password: field_value(fields, "P"),
        hidden: hidden,
    )
}

/// The value the property `key` of a contact payload holds, or empty.
fn property_value(lines: List<Text>, key: Text) -> Text {
    for line in lines {
        let found = field_after(line.trim(), key)
        if found != "" {
            return found
        }
    }
    ""
}

fn contact_of(raw: Text) -> Contact {
    let lines = raw.split("\n")
    let named = property_value(lines, "FN")
    let name = if named != "" {
        named
    } else {
        property_value(lines, "N").replace(";", " ").trim()
    }
    Contact(
        name: if name != "" { name } else { mecard_name(raw) },
        phone: property_value(lines, "TEL"),
        email: property_value(lines, "EMAIL"),
        org: property_value(lines, "ORG"),
    )
}

/// The name a MECARD payload names, or empty.
fn mecard_name(raw: Text) -> Text {
    for field in raw.split(";") {
        if field.starts_with("MECARD:N:") {
            return field.slice(9, field.len())
        } else if field.starts_with("N:") {
            return field.slice(2, field.len())
        }
    }
    ""
}

fn without_scheme(raw: Text, scheme: Text) -> Text {
    if raw.lower().starts_with(scheme) { raw.slice(scheme.len(), raw.len()) } else { raw }
}

/// The payload's one main fact: a network's name, a person, an address.
fn code_summary(raw: Text) -> Text {
    let kind = kind_of(raw)
    if kind == "wifi" {
        wifi_of(raw).ssid
    } else if kind == "contact" {
        contact_of(raw).name
    } else if kind == "email" {
        without_scheme(raw, "mailto:")
    } else if kind == "phone" {
        without_scheme(raw, "tel:")
    } else if kind == "place" {
        without_scheme(raw, "geo:").replace(",", ", ")
    } else { raw }
}

/// What kind of code it is, in the words the result card shows under its main fact.
fn code_description(raw: Text) -> Text {
    let kind = kind_of(raw)
    if kind == "wifi" {
        let wifi = wifi_of(raw)
        if wifi.auth != "" and wifi.auth != "nopass" {
            "Wi-Fi network, {wifi.auth}"
        } else { "Wi-Fi network" }
    } else if kind == "contact" {
        let contact = contact_of(raw)
        if contact.phone != "" { "Contact, {contact.phone}" } else { "Contact card" }
    } else if kind == "link" {
        "Website"
    } else if kind == "email" {
        "Email address"
    } else if kind == "phone" {
        "Phone number"
    } else if kind == "place" { "Location" } else { "Plain text" }
}

/// The actions the result card offers for a code, primary first.
fn code_actions(raw: Text) -> List<Text> {
    let kind = kind_of(raw)
    if kind == "link" {
        ["Open link", "Copy link"]
    } else if kind == "wifi" {
        ["Copy password", "Copy network name"]
    } else if kind == "contact" {
        let contact = contact_of(raw)
        if contact.phone != "" { ["Call", "Copy contact"] } else { ["Copy contact"] }
    } else if kind == "email" {
        ["Write email", "Copy address"]
    } else if kind == "phone" {
        ["Call", "Copy number"]
    } else if kind == "place" { ["Open map", "Copy location"] } else { ["Copy text"] }
}

fn facing_words(facing: Facing) -> Text {
    if facing == .front { "Front" } else if facing == .wide { "Ultra-wide" } else { "Rear" }
}

fn facing_key(facing: Facing) -> Text {
    if facing == .front { "front" } else if facing == .wide { "wide" } else { "back" }
}

fn facing_of(name: Text) -> Facing {
    if name == "front" { .front } else if name == "wide" { .wide } else { .back }
}

fn next_facing(facing: Facing) -> Facing {
    if facing == .back { .front } else if facing == .front { .wide } else { .back }
}

fn photo_width(resolution: Text) -> Int {
    if resolution == "4k" {
        3840
    } else if resolution == "1080p" { 1920 } else if resolution == "720p" { 1280 } else { 4032 }
}

fn photo_height(resolution: Text) -> Int {
    if resolution == "4k" {
        2160
    } else if resolution == "1080p" { 1080 } else if resolution == "720p" { 720 } else { 3024 }
}

/// The preference record the person's window reads: theirs, made now when they have none.
fn prefs_read(c: Camera) -> CapturePreferences {
    c.preferences.first() ?? CapturePreferences()
}

/// The document the person is scanning: theirs, made now when they have none.
fn session_read(c: Camera) -> ScanSession {
    c.sessions.first() ?? ScanSession()
}

fn actions(word: Text, raw: Text) -> Bool {
    code_actions(raw).contains(word)
}

/// Takes what the engine kept as the capture the app hands on.
// ---------------------------------------------------------------- actions
action register_capture(c: Camera, capture: SharedCapture) uses time {
    c.recent = .some(capture)
}

/// Keeps a scanned code in the history; the same code seen again moves to the front rather than
/// repeating.
action record_scan(c: Camera, raw: Text) uses time {
    if raw == "" {
        return
    }
    for entry in c.scans {
        if entry.raw == raw { c.scans.remove(entry) }
    }
    c.scans.insert(ScanEntry(raw: raw, ts: now))
    if c.scans.len() > 24 {
        if let oldest = c.scans.last() { c.scans.remove(oldest) }
    }
}

/// Shows the composition grid, or takes it away.
action set_grid(c: Camera, on: Bool) {
    let p = prefs_of(c)
    p.grid = on
}

/// Shows the level guide, or takes it away.
action set_level(c: Camera, on: Bool) {
    let p = prefs_of(c)
    p.level = on
}

/// The person's preferences, made the first time they are written.
action prefs_of(c: Camera) -> CapturePreferences {
    c.preferences.first() ?? c.preferences.insert(CapturePreferences())
}

/// The person's document, made the first time it is written.
action session_of(c: Camera) -> ScanSession {
    c.sessions.first() ?? c.sessions.insert(ScanSession())
}

/// Opens the viewfinder.
///
/// Opens the code scanner.
///
/// Opens the document scanner.
///
/// Opens the capture settings.
///
/// Switches the viewfinder between photos and clips.
action use_mode(c: Camera, mode: Text) {
    let p = prefs_of(c)
    if p.recording != "" {
        p.trouble = "Stop recording to switch modes"
        return
    }
    p.mode = if mode == "video" { "video" } else { "photo" }
    p.trouble = ""
}

/// Shows one lens.
action use_lens(c: Camera, lens: Facing) {
    let p = prefs_of(c)
    p.lens = lens
}

/// Switches to the next lens.
action cycle_lens(c: Camera) {
    let p = prefs_of(c)
    p.lens = next_facing(p.lens)
}

/// Uses the camera at this device node.
action use_camera(c: Camera, device: Text) {
    let p = prefs_of(c)
    p.device = device
    p.trouble = ""
}

/// Forgets the chosen camera and asks the engine for the cameras again.
action retry_camera(c: Camera) {
    let p = prefs_of(c)
    p.device = ""
    p.trouble = ""
}

action set_resolution(c: Camera, resolution: Text) {
    let p = prefs_of(c)
    p.resolution = resolution
}

action set_photo_format(c: Camera, format: Text) {
    let p = prefs_of(c)
    p.photo_format = format
}

action set_timer(c: Camera, seconds: Int) {
    let p = prefs_of(c)
    p.timer = seconds.max(0).min(10)
}

action set_mirror_front(c: Camera, on: Bool) {
    let p = prefs_of(c)
    p.mirror_front = on
}

action set_geotag(c: Camera, on: Bool) {
    let p = prefs_of(c)
    p.geotag = on
}

action set_hdr(c: Camera, on: Bool) {
    let p = prefs_of(c)
    p.hdr = on
}

action set_default_lens(c: Camera, lens: Facing) {
    let p = prefs_of(c)
    p.default_lens = lens
}

/// Looks through the camera once: the newest frame becomes the picture the viewfinder shows and
/// the one the code scanner reads.
action look(c: Camera) uses time {
    let p = prefs_of(c)
    let ask = FrameAsk(device: p.device, width: 1280, height: 720)
    match engine.picture("frame", json.encode(ask)) {
        .ok(file) => {
            p.preview = file.url
            p.trouble = ""
            read_codes(c, file.url)
        }
        .err(why) => {
            p.trouble = why
            p.preview = ""
            p.found = ""
        }
    }
}

/// Decodes the codes in the picture at `url`, keeping the first one the scanner view shows.
action read_codes(c: Camera, url: Text) {
    let p = prefs_of(c)
    let ask = CodesAsk(url: url, width: 1024)
    match engine.call("codes", json.encode(ask)) {
        .ok(text) => match json.decode<CodesAnswer>(text) {
            .ok(answer) => {
                if let first = answer.codes.first() {
                    p.found = first.text
                    p.found_x = first.x
                    p.found_y = first.y
                    p.found_w = first.w
                    p.found_h = first.h
                } else {
                    p.found = ""
                }
            }
            .err(why) => {
                p.trouble = why
            }
        },
        .err(why) => {
            p.trouble = why
        }
    }
}

/// The shutter: takes a photo now, starts the self-timer, or records.
action shutter(c: Camera) uses time {
    let p = prefs_of(c)
    if p.recording != "" {
        stop_recording(c)
        return
    }
    if if let due = p.shutter_at { due != now } else { false } {
        p.shutter_at = .none
        p.trouble = ""
        return
    }
    p.shutter_at = .none
    if p.mode == "video" {
        start_recording(c)
        return
    }
    if p.timer > 0 {
        p.shutter_at = .some(now + Duration(p.timer))
        return
    }
    capture(c)
}

/// Arms the self-timer the person chose, or cancels the one counting down.
action arm_timer(c: Camera) uses time {
    let p = prefs_of(c)
    if p.shutter_at != .none {
        p.shutter_at = .none
        return
    }
    if p.timer <= 0 {
        capture(c)
    } else {
        p.shutter_at = .some(now + Duration(p.timer))
    }
}

/// Takes a photo of the frame the camera shows, and hands it on.
action capture(c: Camera) uses time {
    let p = prefs_of(c)
    let ask = StillAsk(
        device: p.device,
        width: photo_width(p.resolution),
        height: photo_height(p.resolution),
        format: p.photo_format,
    )
    match engine.picture("still", json.encode(ask)) {
        .ok(file) => {
            c.recent = .some(
                SharedCapture(
                    file: file,
                    kind: .photo,
                    ts: now,
                    w: photo_width(p.resolution),
                    h: photo_height(p.resolution),
                    facing: p.lens,
                ),
            )
            p.preview = file.url
            p.trouble = ""
        }
        .err(why) => {
            p.trouble = why
        }
    }
}

/// Starts recording from the frames the camera's engine thread takes.
action start_recording(c: Camera) uses time {
    let p = prefs_of(c)
    if p.recording != "" {
        return
    }
    let ask = RecordAsk(
        device: p.device,
        width: photo_width(p.resolution),
        height: photo_height(p.resolution),
    )
    match engine.start("record", json.encode(ask)) {
        .ok(text) => match json.decode<JobAnswer>(text) {
            .ok(answer) => {
                p.recording = answer.job
                p.trouble = ""
            }
            .err(why) => {
                p.trouble = why
            }
        },
        .err(why) => {
            p.trouble = why
        }
    }
}

/// Stops the clip recording and hands it on once the engine has muxed it.
action stop_recording(c: Camera) {
    let p = prefs_of(c)
    if p.recording == "" {
        return
    }
    match engine.call("stop-record", json.encode(RecordAskJob(job: p.recording))) {
        .ok(text) => match json.decode<RecordStopped>(text) {
            .ok(answer) => {
                p.trouble = ""
            }
            .err(why) => {
                p.trouble = why
            }
        },
        .err(why) => {
            p.trouble = why
            p.recording = ""
        }
    }
}

/// Asks how far the recording and the document scan have got, finishing what is done.
action poll(c: Camera) uses time {
    let p = prefs_of(c)
    if p.recording != "" {
        match engine.status(p.recording) {
            .ok(text) => match json.decode<JobState>(text) {
                .ok(state) => {
                    if state.state == "done" {
                        c.recent = .some(
                            SharedCapture(
                                file: File.at("VID_{now}.avi", state.url),
                                kind: .video,
                                ts: now,
                                w: photo_width(p.resolution),
                                h: photo_height(p.resolution),
                                facing: p.lens,
                            ),
                        )
                        p.recording = ""
                    } else if state.state == "failed" {
                        p.trouble = state.error
                        p.recording = ""
                    } else if state.state == "cancelled" {
                        p.recording = ""
                    }
                }
                .err(why) => {
                    p.trouble = why
                    p.recording = ""
                }
            },
            .err(why) => {
                p.trouble = why
                p.recording = ""
            }
        }
    }
    for session in c.sessions {
        if session.job != "" {
            match engine.status(session.job) {
                .ok(text) => match json.decode<JobState>(text) {
                    .ok(state) => {
                        if state.state == "done" {
                            session.pages.insert(
                                ScannedPage(
                                    file: File.at("scan-{session.pages.len() + 1}.png", state.url),
                                    order: session.pages.len(),
                                ),
                            )
                            session.selected = session.pages.len() - 1
                            session.job = ""
                            session.failed = ""
                        } else if state.state == "failed" {
                            session.failed = state.error
                            session.job = ""
                        } else if state.state == "cancelled" {
                            session.job = ""
                        }
                    }
                    .err(why) => {
                        session.failed = why
                        session.job = ""
                    }
                },
                .err(why) => {
                    session.failed = why
                    session.job = ""
                }
            }
        }
    }
    if let due = p.shutter_at {
        if now >= due {
            p.shutter_at = .none
            capture(c)
        }
    }
}

/// The viewfinder and the document scanner ask about their work once a second.
trigger watch(c: Camera) every 1s: poll(c)

/// Opening the camera shows what it sees.
opened action look_on_open(c: Camera) uses time {
    look(c)
}

/// Takes a chosen image as the code to read, without importing it.
action import_image(c: Camera, file: File) uses time {
    let p = prefs_of(c)
    read_codes(c, file.url)
    if p.found != "" { record_scan(c, p.found) }
}

/// Copies what the scanned code says, or its password or link.
action copy_code(c: Camera, which: Text) uses clipboard {
    let p = prefs_of(c)
    let raw = p.found
    if raw == "" {
        return
    }
    let kind = kind_of(raw)
    if which == "password" and kind == "wifi" {
        clipboard.copy(wifi_of(raw).password)
    } else if which == "name" and kind == "wifi" {
        clipboard.copy(wifi_of(raw).ssid)
    } else if which == "summary" { clipboard.copy(code_summary(raw)) } else { clipboard.copy(raw) }
}

/// Keeps the scanned code in the history.
action keep_scan(c: Camera) uses time {
    record_scan(c, prefs_of(c).found)
}

/// Chooses the scanner the document scans with.
action set_scanner(c: Camera, device: Text) {
    let session = session_of(c)
    session.scanner = device
}

action set_scan_resolution(c: Camera, resolution: Int) {
    let session = session_of(c)
    session.resolution = resolution
}

action set_scan_colour(c: Camera, colour: Text) {
    let session = session_of(c)
    session.colour = colour
}

action set_scan_source(c: Camera, source: Text) {
    let session = session_of(c)
    session.source = source
}

action set_ocr_language(c: Camera, language: Text) {
    let session = session_of(c)
    session.language = language
}

/// Captures one page with the chosen scanner and settings.
action capture_page(c: Camera) {
    let session = session_of(c)
    if session.job != "" {
        return
    }
    if session.scanner == "" {
        session.failed = "Choose a document scanner first"
        return
    }
    let ask = ScanAsk(
        scanner: session.scanner,
        resolution: session.resolution,
        colour: session.colour,
        source: session.source,
        name: "scan-{session.pages.len() + 1}.png",
    )
    match engine.start("scan", json.encode(ask)) {
        .ok(text) => match json.decode<JobAnswer>(text) {
            .ok(answer) => {
                session.job = answer.job
                session.failed = ""
            }
            .err(why) => {
                session.failed = why
            }
        },
        .err(why) => {
            session.failed = why
        }
    }
}

/// The page the document shows.
fn selected_page(c: Camera) -> Option<ScannedPage> {
    let session = session_read(c)
    let ordered = session.pages.sort_by(page -> page.order)
    ordered.at(session.selected)
}

action select_page(c: Camera, page: ScannedPage) {
    let session = session_of(c)
    let ordered = session.pages.sort_by(one -> one.order)
    if let at = ordered.index_of(page) {
        session.selected = at
    }
}

/// Turns the selected page a quarter clockwise; its text is read again when the person asks.
action rotate_page(c: Camera) uses image {
    if let page = selected_page(c) {
        match page.file.turned(1) {
            .ok(turned) => {
                page.file = turned
                page.text = ""
            }
            .err(_) => {}
        }
    }
}

/// Reads the text off the selected page in the chosen language.
action recognize_text(c: Camera) {
    if let page = selected_page(c) {
        let language = session_read(c).language
        let ask = OcrAsk(url: page.file.url, language: language)
        match engine.call("ocr", json.encode(ask)) {
            .ok(text) => match json.decode<OcrAnswer>(text) {
                .ok(answer) => {
                    page.text = answer.text
                }
                .err(_) => {}
            },
            .err(_) => {}
        }
    }
}

/// Copies the selected page's recognized text.
action copy_text(c: Camera) uses clipboard {
    if let page = selected_page(c) {
        if page.text != "" { clipboard.copy(page.text) }
    }
}

/// Moves the selected page one place earlier, or later.
action move_page(c: Camera, later: Bool) {
    let session = session_of(c)
    let ordered = session.pages.sort_by(page -> page.order)
    let index = session.selected
    let to = if later { index + 1 } else { index - 1 }
    if index < 0 or index >= ordered.len() or to < 0 or to >= ordered.len() {
        return
    }
    if let page = ordered.at(index) {
        if let other = ordered.at(to) {
            let order = page.order
            page.order = other.order
            other.order = order
            session.selected = to
        }
    }
}

/// Throws the selected page away.
action remove_page(c: Camera) {
    let session = session_of(c)
    if let page = selected_page(c) {
        session.pages.remove(page)
        if session.selected >= session.pages.len() {
            session.selected = (session.pages.len() - 1).max(0)
        }
    }
}

/// The selected page's picture, as the crop frame works on it.
action crop_source(c: Camera) -> Option<File> {
    if let page = selected_page(c) { .some(page.file) } else { .none }
}

/// Crops the selected page to the part the person kept; its text is read again when they ask.
action crop_page(c: Camera, left: Float, top: Float, right: Float, bottom: Float) uses image {
    if let page = selected_page(c) {
        let width = right - left
        let height = bottom - top
        if width <= 0.0 or height <= 0.0 {
            return
        }
        match page.file.cropped(left, top, width, height) {
            .ok(cropped) => {
                page.file = cropped
                page.text = ""
            }
            .err(_) => {}
        }
    }
}

/// The document as one searchable PDF: pages with recognized text carry it, the rest are
/// pictures alone.
action document_pdf(c: Camera) -> File {
    let session = session_read(c)
    let ordered = session.pages.sort_by(page -> page.order)
    let pages = ordered.map(page -> PdfPage(url: page.file.url, text: page.text))
    match engine.call(
        "pages_pdf",
        json.encode(PagesAsk(pages: pages, name: "scanned document.pdf")),
    ) {
        .ok(text) => match json.decode<UrlAnswer>(text) {
            .ok(answer) => File.at("scanned document.pdf", answer.url),
            .err(_) => File.of_text("scanned document.txt", ""),
        },
        .err(_) => File.of_text("scanned document.txt", ""),
    }
}

/// Opens the capture that was taken last in the app that owns its bytes.
action open_recent(c: Camera) uses files {
    if let recent = c.recent { File.open(recent.file) }
}

/// Opens the library the captures go to.
action open_photos() uses files {
    session.open_app(app: "so.tilo.photos")
}

/// Opens this device's camera privacy settings.
action open_camera_privacy() uses files {
    session.open_app(app: "so.tilo.settings")
}

// ---------------------------------------------------------------- views
view viewfinder(c: Camera) {
    let p = prefs_read(c)
    let counting = p.shutter_at != .none
    let recording = p.recording != ""
    stack(gap: .related, fill) {
        row(gap: .tight, wrap: true, align: .center) {
            ui.Chip("Photo", selected: p.mode == "photo") {
                on press: use_mode(c, "photo")
            }
            ui.Chip("Video", selected: p.mode == "video") {
                on press: use_mode(c, "video")
            }
            ui.Chip("Scan") {
                on press: open(c, view: scanner_page)
            }
            ui.Chip("Documents") {
                on press: open(c, view: documents_page)
            }
            ui.Button("Photos", icon: .image) {
                on press: open_photos()
            }
            ui.Button("Settings", icon: .settings, icon_only: true) {
                on press: open(c, view: settings_page)
            }
        }
        if p.preview != "" {
            ui.Drawing(
                viewfinder_shapes(p),
                meaning: .decoration,
                label: .some("The viewfinder"),
                height: .some(420.0),
                motion: .still,
            )
        } else {
            ui.EmptyState(no_picture_title(p), message: no_picture_words(p), icon: .camera)
        }
        let trouble = p.trouble
        if trouble != "" {
            ui.StatusLine(trouble, status: .error)
        }
        if recording {
            ui.StatusLine("Recording", status: .active)
        }
        row(gap: .related, align: .center, wrap: true) {
            ui.Button(
                shutter_words(p, counting, recording),
                icon: if recording { .square } else { .camera },
                role: .primary,
            ) {
                on press: shutter(c)
            }
            ui.Button(facing_words(p.lens), icon: .rotate_cw) {
                on press: cycle_lens(c)
            }
            ui.Chip("Grid", selected: p.grid) {
                on press: set_grid(c, not p.grid)
            }
            ui.Chip("Level", selected: p.level) {
                on press: set_level(c, not p.level)
            }
            ui.Chip("HDR", selected: p.hdr) {
                on press: set_hdr(c, not p.hdr)
            }
        }
        if let recent = c.recent {
            ui.Section("Latest capture") {
                ui.ListRow(
                    recent.file.name,
                    secondary: if recent.kind == .video { "Clip" } else { "Photo" },
                    icon: if recent.kind == .video { .video } else { .image },
                ) {
                    on activate: open_recent(c)
                }
            }
        }
        ui.Section("Preferences") {
            ui.Text(
                "This device's camera: {device_words(p)} · {p.resolution} · {p.photo_format}",
                role: .caption,
                secondary: true,
            )
        }
    }
}

fn shutter_words(p: CapturePreferences, counting: Bool, recording: Bool) -> Text {
    if recording {
        "Stop recording"
    } else if counting {
        "Cancel capture timer"
    } else if p.mode == "video" { "Start recording" } else { "Take photo" }
}

fn device_words(p: CapturePreferences) -> Text {
    if p.device == "" { "this computer's default camera" } else { p.device }
}

fn no_picture_title(p: CapturePreferences) -> Text {
    if p.trouble == "" { "No camera connected" } else { "Camera unavailable" }
}

fn no_picture_words(p: CapturePreferences) -> Text {
    if p.trouble == "" { "Connect a camera, then look again." } else { p.trouble }
}

/// The frame, and over it what the person asked to see: the rule-of-thirds grid and the level.
fn viewfinder_shapes(p: CapturePreferences) -> List<ui.Shape> {
    let frame = [ui.Shape.picture(x: 0.0, y: 0.0, w: 1.0, h: 1.0, src: p.preview)]
    let grid = [
        ui.Shape.line(x1: 0.333, y1: 0.0, x2: 0.333, y2: 1.0, stroke: .track, weight: .hairline),
        ui.Shape.line(x1: 0.666, y1: 0.0, x2: 0.666, y2: 1.0, stroke: .track, weight: .hairline),
        ui.Shape.line(x1: 0.0, y1: 0.333, x2: 1.0, y2: 0.333, stroke: .track, weight: .hairline),
        ui.Shape.line(x1: 0.0, y1: 0.666, x2: 1.0, y2: 0.666, stroke: .track, weight: .hairline),
    ]
    let level = [
        ui.Shape.line(x1: 0.2, y1: 0.5, x2: 0.8, y2: 0.5, stroke: .success, weight: .regular),
    ]
    let with_grid = if p.grid { frame + grid } else { frame }
    if p.level { with_grid + level } else { with_grid }
}

view scanner_page(c: Camera) {
    let p = prefs_read(c)
    let found = p.found
    stack(gap: .related, fill) {
        row(gap: .tight, wrap: true, align: .center) {
            ui.Chip("Photo") {
                on press: open(c, view: viewfinder)
            }
            ui.Chip("Scan", selected: true) {
                on press: look(c)
            }
            ui.Chip("Documents") {
                on press: open(c, view: documents_page)
            }
            ui.Button("Scan an image", icon: .image) {
                on press: file.choose(import_image(c), accept: ["image/*"])
            }
            ui.Button("Look again", icon: .rotate_cw) {
                on press: look(c)
            }
        }
        if p.preview != "" {
            ui.Drawing(
                scan_shapes(p, found),
                meaning: .decoration,
                label: .some("What the camera shows"),
                height: .some(420.0),
                motion: .still,
            )
        } else {
            ui.EmptyState(
                "No picture yet",
                message: "Look through the camera, or scan an image you already have.",
                icon: .camera,
            )
        }
        if found != "" {
            ui.Section("Detected code") {
                ui.ListRow(
                    code_summary(found),
                    secondary: code_description(found),
                    icon: if kind_of(found) == "link" {
                        .link
                    } else if kind_of(found) == "wifi" {
                        .wifi
                    } else if kind_of(found) == "contact" {
                        .user
                    } else if kind_of(found) == "phone" {
                        .phone
                    } else if kind_of(found) == "email" {
                        .mail
                    } else if kind_of(found) == "place" { .map_pin } else { .type },
                )
                row(gap: .tight, wrap: true) {
                    for choice in code_actions(found) {
                        if choice == "Open link" {
                            ui.Link(found, label: choice)
                        } else if choice == "Open map"
                            or choice == "Write email"
                            or choice == "Call" {
                            ui.Link(found, label: choice)
                        } else if choice == "Copy password" {
                            ui.Button(choice, icon: .key) {
                                on press: copy_code(c, "password")
                            }
                        } else if choice == "Copy network name" {
                            ui.Button(choice, icon: .wifi) {
                                on press: copy_code(c, "name")
                            }
                        } else {
                            ui.Button(choice, icon: .copy) {
                                on press: copy_code(c, "raw")
                            }
                        }
                    }
                    ui.Button("Keep in history", icon: .archive) {
                        on press: keep_scan(c)
                    }
                }
            }
        }
        if not c.scans.is_empty() {
            ui.Section("Recent codes") {
                for entry in c.scans {
                    ui.ListRow(
                        code_summary(entry.raw),
                        secondary: code_description(entry.raw),
                        icon: if kind_of(entry.raw) == "link" {
                            .link
                        } else if kind_of(entry.raw) == "wifi" {
                            .wifi
                        } else if kind_of(entry.raw) == "contact" {
                            .user
                        } else if kind_of(entry.raw) == "phone" {
                            .phone
                        } else if kind_of(entry.raw) == "email" {
                            .mail
                        } else if kind_of(entry.raw) == "place" { .map_pin } else { .type },
                    )
                }
            }
        }
    }
}

fn scan_shapes(p: CapturePreferences, found: Text) -> List<ui.Shape> {
    let frame = [ui.Shape.picture(x: 0.0, y: 0.0, w: 1.0, h: 1.0, src: p.preview)]
    let outline = [
        ui.Shape.rect(
            x: p.found_x,
            y: p.found_y,
            w: p.found_w,
            h: p.found_h,
            round: 0.01,
            fill: .some(ui.Ink.success),
        ),
    ]
    if found != "" and p.found_w > 0.0 { frame + outline } else { frame }
}

view documents_page(c: Camera) {
    state cropping: Bool = false
    state crop_edges: List<Float> = [0.0, 0.0, 1.0, 1.0]
    let session = session_read(c)
    let scanners = device.scanners
    stack(gap: .related, fill) {
        row(gap: .tight, wrap: true, align: .center) {
            ui.Chip("Photo") {
                on press: open(c, view: viewfinder)
            }
            ui.Chip("Scan") {
                on press: open(c, view: scanner_page)
            }
            ui.Chip("Documents", selected: true) {
                on press: capture_page(c)
            }
        }
        if scanners.found.is_empty() {
            ui.EmptyState(
                "No scanner found",
                message: "Connect a document scanner to this computer, then open this page again.",
                icon: .scan_line,
            )
        } else {
            ui.Section("Scanner") {
                for scanner in scanners.found {
                    ui.ListRow(
                        scanner.description,
                        secondary: scanner.device,
                        selected: session.scanner == scanner.device,
                        icon: .scan_line,
                    ) {
                        on activate: set_scanner(c, scanner.device)
                    }
                }
            }
        }
        form {
            field("Resolution") {
                row(gap: .tight, wrap: true) {
                    for dpi in [150, 300, 600] {
                        ui.Chip("{dpi} dpi", selected: session.resolution == dpi) {
                            on press: set_scan_resolution(c, dpi)
                        }
                    }
                }
            }
            field("Colour") {
                row(gap: .tight, wrap: true) {
                    ui.Chip("Colour", selected: session.colour == "color") {
                        on press: set_scan_colour(c, "color")
                    }
                    ui.Chip("Grayscale", selected: session.colour == "grayscale") {
                        on press: set_scan_colour(c, "grayscale")
                    }
                    ui.Chip("Line art", selected: session.colour == "lineart") {
                        on press: set_scan_colour(c, "lineart")
                    }
                }
            }
            field("Paper source") {
                row(gap: .tight, wrap: true) {
                    ui.Chip("Flatbed", selected: session.source == "flatbed") {
                        on press: set_scan_source(c, "flatbed")
                    }
                    ui.Chip("Feeder", selected: session.source == "feeder") {
                        on press: set_scan_source(c, "feeder")
                    }
                    ui.Chip("Feeder, both sides", selected: session.source == "feeder-duplex") {
                        on press: set_scan_source(c, "feeder-duplex")
                    }
                }
            }
            field("Text language") {
                row(gap: .tight, wrap: true) {
                    ui.Chip("English", selected: session.language == "eng") {
                        on press: set_ocr_language(c, "eng")
                    }
                    ui.Chip("German", selected: session.language == "deu") {
                        on press: set_ocr_language(c, "deu")
                    }
                    ui.Chip("French", selected: session.language == "fra") {
                        on press: set_ocr_language(c, "fra")
                    }
                    ui.Chip("Spanish", selected: session.language == "spa") {
                        on press: set_ocr_language(c, "spa")
                    }
                }
            }
        }
        if session.job != "" {
            ui.StatusLine("The scanner is working", status: .active)
        }
        let failed = session.failed
        if failed != "" {
            ui.StatusLine(failed, status: .error)
        }
        let pages = session.pages.sort_by(page -> page.order)
        if pages.is_empty() {
            ui.EmptyState("No pages yet", message: "Scanned pages appear here.", icon: .file_text)
        } else {
            ui.Section("Pages") {
                for page in pages {
                    ui.ListRow(
                        page.file.name,
                        secondary: if page.text == "" {
                            "Text not recognized"
                        } else { "{page.text.words()} words" },
                        selected: page.order == (pages.at(session.selected)?.order ?? -1),
                        icon: .file_text,
                    ) {
                        on activate: select_page(c, page)
                    }
                }
            }
            if let page = pages.at(session.selected) {
                ui.Image(src: page.file.url, alt: page.file.name, fit: .contain, aspect: 0.7727)
                row(gap: .tight, wrap: true) {
                    ui.Button("Scan page", icon: .scan_line, role: .primary) {
                        on press: capture_page(c)
                    }
                    ui.Button("Turn", icon: .rotate_cw) {
                        on press: rotate_page(c)
                    }
                    ui.Button("Earlier", icon: .arrow_up, icon_only: true) {
                        on press: move_page(c, false)
                    }
                    ui.Button("Later", icon: .arrow_down, icon_only: true) {
                        on press: move_page(c, true)
                    }
                    ui.Button("Remove page", icon: .trash_2) {
                        on press: remove_page(c)
                    }
                    ui.Button("Crop", icon: .scissors) {
                        on press: set(cropping, not cropping)
                    }
                }
                if cropping {
                    ui.ImageCropEditor(
                        page.file.url,
                        left: crop_edges.at(0) ?? 0.0,
                        top: crop_edges.at(1) ?? 0.0,
                        right: crop_edges.at(2) ?? 1.0,
                        bottom: crop_edges.at(3) ?? 1.0,
                    ) {
                        on change(edges: List<Float>): set(crop_edges, edges)
                    }
                    row(gap: .tight, wrap: true) {
                        ui.Button("Apply crop", icon: .check, role: .primary) {
                            on press: crop_page(
                                    c,
                                    crop_edges.at(0) ?? 0.0,
                                    crop_edges.at(1) ?? 0.0,
                                    crop_edges.at(2) ?? 1.0,
                                    crop_edges.at(3) ?? 1.0,
                                )
                                .then(set(cropping, false))
                        }
                        ui.Button("Cancel crop", icon: .x) {
                            on press: set(cropping, false)
                        }
                    }
                }
                row(gap: .tight, wrap: true) {
                    ui.Button("Recognize text", icon: .file_text) {
                        on press: recognize_text(c)
                    }
                    ui.Button("Copy text", icon: .copy) {
                        on press: copy_text(c)
                    }
                    ui.Button("Save as PDF", icon: .download, role: .primary) {
                        on press: file.save(document_pdf(c), name: "scanned document.pdf")
                    }
                }
            }
        }
    }
}

view settings_page(c: Camera) {
    let p = prefs_read(c)
    stack(gap: .related, fill) {
        row(gap: .tight, wrap: true, align: .center) {
            ui.Button("Back to the camera", icon: .arrow_left) {
                on press: open(c, view: viewfinder)
            }
        }
        ui.Section("Capture") {
            form {
                field("Resolution") {
                    row(gap: .tight, wrap: true) {
                        for resolution in ["12mp", "4k", "1080p", "720p"] {
                            ui.Chip(
                                resolution_words(resolution),
                                selected: p.resolution == resolution,
                            ) {
                                on press: set_resolution(c, resolution)
                            }
                        }
                    }
                }
                field("Photo format") {
                    row(gap: .tight, wrap: true) {
                        ui.Chip("JPEG", selected: p.photo_format == "jpeg") {
                            on press: set_photo_format(c, "jpeg")
                        }
                        ui.Chip("PNG", selected: p.photo_format == "png") {
                            on press: set_photo_format(c, "png")
                        }
                    }
                }
                field("Self-timer") {
                    row(gap: .tight, wrap: true) {
                        ui.Chip("Off", selected: p.timer == 0) {
                            on press: set_timer(c, 0)
                        }
                        ui.Chip("3 seconds", selected: p.timer == 3) {
                            on press: set_timer(c, 3)
                        }
                        ui.Chip("10 seconds", selected: p.timer == 10) {
                            on press: set_timer(c, 10)
                        }
                    }
                }
                field("Default camera") {
                    row(gap: .tight, wrap: true) {
                        ui.Chip("Rear", selected: p.default_lens == .back) {
                            on press: set_default_lens(c, .back)
                        }
                        ui.Chip("Front", selected: p.default_lens == .front) {
                            on press: set_default_lens(c, .front)
                        }
                        ui.Chip("Ultra-wide", selected: p.default_lens == .wide) {
                            on press: set_default_lens(c, .wide)
                        }
                    }
                }
            }
        }
        ui.Section("Viewfinder and photos") {
            form {
                field("HDR") {
                    ui.Toggle(bind: p.hdr, label: "Use HDR") {
                        on check: set_hdr(c, p.hdr)
                    }
                }
                field("Rule-of-thirds grid") {
                    ui.Toggle(bind: p.grid, label: "Show the grid") {
                        on check: set_grid(c, p.grid)
                    }
                }
                field("Level guide") {
                    ui.Toggle(bind: p.level, label: "Show the level") {
                        on check: set_level(c, p.level)
                    }
                }
                field("Mirror front camera") {
                    ui.Toggle(bind: p.mirror_front, label: "Mirror front-camera photos") {
                        on check: set_mirror_front(c, p.mirror_front)
                    }
                }
                field("Save location with photos") {
                    ui.Toggle(bind: p.geotag, label: "Tag photos with where they were taken") {
                        on check: set_geotag(c, p.geotag)
                    }
                }
            }
        }
    }
}

fn resolution_words(resolution: Text) -> Text {
    if resolution == "12mp" {
        "12 MP"
    } else if resolution == "4k" { "4K" } else if resolution == "1080p" { "1080p" } else { "720p" }
}
