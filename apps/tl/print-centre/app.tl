app so.tilo.print-centre "Print Centre" (
    icon: .printer,
    category: .system,
    tagline: "The printers, their jobs, and the scanner on this computer",
    keywords: ["print", "printer", "printing", "scan", "scanner", "cups", "jobs", "print queue", "pdf export", "printers and scanners", "devices and printers", "printer settings", "system-config-printer", "drucken", "drucker", "scannen"],
    splash_budget: 250,
)

use so.tilo.device as device

op "options"
op "print"
op "scan"
op "ocr"
op "pages_pdf"

/// How a person prints and scans unless they choose otherwise for one document: the
/// workspace's, so every member prints the same way.
type Defaults {
    /// Print on both sides of the paper.
    two_sided: Bool = false
    /// Scan at 150, 300 or 600 dots an inch.
    scan_resolution: Int = 300
    /// Scan in colour (`color`), `grayscale` or `line-art`.
    scan_colour: Text = "color"
    /// Scan from the `flatbed`, the `feeder` or a `feeder-duplex`.
    scan_source: Text = "flatbed"
    /// The language text is recognized in: `eng`, `deu`, `fra` or `spa`.
    ocr_language: Text = "eng"
}

/// One page of a scanned document, as the scanner wrote it, turned and cropped as the person
/// asked, with the text read off it when they asked for that.
type ScannedPage {
    file: File
    text: Text = ""
    /// Where it stands in the document.
    order: Int = 0
    /// The text is being read off the page now, and the page says so while it is.
    reading: Bool = false
}

/// The scanned document in progress: the person's own, so it never reaches anyone else.
type ScanSession (readers: author) {
    scanner: Text = ""
    pages: Collection<ScannedPage>
    /// The scan running now, by its engine job.
    job: Text = ""
    failed: Text = ""
}

/// The document being printed, and how: the person's own, on their device.
type PrintDocument (readers: author) {
    file: Option<File> = .none
    /// The first page's picture, for the form.
    preview: Option<File> = .none
    name: Text = ""
    queue: Text = ""
    copies: Float = 1.0
    pages: Text = ""
    two_sided: Bool = false
    page_size: Text = ""
    colour: Text = ""
    duplex: Text = ""
    orientation: Text = ""
    /// The print running now, by its engine job.
    job: Text = ""
    failed: Text = ""
    /// Why the printer was not accepted, from the last try.
    trouble: Text = ""
    /// The page of the document the form previews, counted from 1, and how many the file holds.
    preview_page: Int = 1
    preview_pages: Int = 1
    /// Where the form's picture of the document stands: `preparing` while the device draws its
    /// first page, `unavailable` when it cannot, and empty once it is there.
    preview_state: Text = ""
    /// Another app handed it over to print: the person's own windows show the print form until
    /// they choose a place themselves. Their document, so no one else's window moves.
    handed: Bool = false
}

/// One page of the file being printed, as the device reads the file's own pages.
type PreviewPage {
    page: Int
    width: Float
    height: Float
}

/// An option a printer reports, with what it is called and whether it is its default.
type Choice {
    key: Text
    label: Text
    default: Bool = false
}

/// What one printer says it can do.
type PrinterOptions {
    page_sizes: List<Choice>
    colour_modes: List<Choice>
    duplex_modes: List<Choice>
    orientations: List<Choice>
}

/// The app's own record: the print and scan defaults, the document being printed and the pages
/// being scanned.
type PrintCentre {
    defaults: Defaults
    documents: Collection<PrintDocument>
    scans: Collection<ScanSession>
    options: PrinterOptions
}

root print_centre: PrintCentre

default view window for PrintCentre

/// What asks the engine for a printer's options.
type OptionsAsk {
    queue: Text
}

/// What the engine says of a scan that finished: where it kept the page's picture.
type ScanDetail {
    url: Text = ""
}

/// What the engine reads a page's text with.
type OcrAsk {
    url: Text
    language: Text
}

/// What the engine answers a page's text with.
type OcrAnswer {
    text: Text
}

/// One page for the scanned document's PDF.
type PdfPage {
    url: Text
    text: Text
}

/// What the engine writes the pages as.
type PagesAsk {
    pages: List<PdfPage>
    name: Text
}

type UrlAnswer {
    url: Text
}

/// What the engine prints.
type PrintAsk {
    queue: Text
    url: Text
    name: Text
    copies: Int
    pages: Text
    two_sided: Bool
    page_size: Text
    colour: Text
    duplex: Text
    orientation: Text
}

/// What the engine scans with.
type ScanAsk {
    scanner: Text
    resolution: Int
    colour: Text
    source: Text
    name: Text
}

/// How many pages the person's scanned document holds.
fn scan_page_count(p: PrintCentre) -> Int {
    if let scan = p.scans.first() { scan.pages.len() } else { 0 }
}

/// How a queue stands.
fn queue_words(state: device.QueueState) -> Text {
    if state == .ready {
        std.say("Ready")
    } else if state == .printing {
        std.say("Printing")
    } else if state == .paused { std.say("Paused") } else { std.say("Offline") }
}

/// Why a printer stands as it does, in the reader's words: the device's state reasons are the
/// code, and its own words stay beside them as the detail.
fn reason_words(code: device.PrinterReason) -> Text {
    if code == .out-of-paper {
        std.say("Add paper to continue.")
    } else if code == .jam {
        std.say("Clear the paper jam, then resume the printer.")
    } else if code == .cover-open {
        std.say("Close the printer cover.")
    } else if code == .supply-low {
        std.say("A supply is running low; printing can usually continue for now.")
    } else if code == .supply-empty {
        std.say("Replace the empty supply to continue.")
    } else if code == .offline {
        std.say("Check that the printer is awake and connected.")
    } else {
        std.say("The printer reported a condition that may need attention.")
    }
}

/// How a job stands.
fn job_words(state: device.JobState) -> Text {
    if state == .waiting {
        std.say("Waiting")
    } else if state == .held {
        std.say("Held")
    } else if state == .printing {
        std.say("Printing")
    } else if state == .stopped {
        std.say("Stopped")
    } else if state == .cancelled {
        std.say("Cancelled")
    } else if state == .failed { std.say("Failed") } else { std.say("Printed") }
}

fn job_active(job: device.PrintJob) -> Bool {
    job.state == .waiting or job.state == .held or job.state == .printing or job.state == .stopped
}

fn job_failed(job: device.PrintJob) -> Bool {
    job.state == .failed or job.state == .stopped
}

/// A size in the words a person reads.
fn size_words(bytes: Int) -> Text {
    if bytes < 1000 {
        std.say("{bytes} B")
    } else if bytes < 1000000 {
        std.say("{bytes / 1000} KB")
    } else { std.say("{bytes / 1000000} MB") }
}

/// Why `pages` is not a range this printer can take; empty when it is, or when every page prints.
///
/// The same shape `lp` takes: `1-3`, `1,4,9`, `2-`. Pages are counted from 1, a range never runs
/// backwards, and nothing else is let through, so a range nobody can print is refused before the
/// document leaves the window.
fn page_range_error(pages: Text) -> Text {
    let value = pages.trim()
    if value == "" {
        ""
    } else if value.len() > 64 {
        std.say("That page range is too long to be one job.")
    } else { parts_error(value.split(","), 0) }
}

/// Why any of `parts` is not a page or a page range; empty when they all are.
fn parts_error(parts: List<Text>, at: Int) -> Text {
    if at >= parts.len() {
        ""
    } else {
        let problem = part_error(parts.at(at) ?? "")
        if problem != "" { problem } else { parts_error(parts, at + 1) }
    }
}

/// Why `part` is not a page (`7`) or a page range (`2-9`, `2-`); empty when it is.
fn part_error(part: Text) -> Text {
    let value = part.trim()
    if value == "" {
        std.say("A page range cannot have an empty part.")
    } else {
        let bounds = value.split("-")
        let first = bounds.at(0) ?? ""
        let last = bounds.at(1) ?? ""
        if bounds.len() > 2 or not page_number(first) {
            std.say("Page numbers are written with digits, as in 1-3.")
        } else if bounds.len() == 2 and last != "" and not page_number(last) {
            std.say("Page numbers are written with digits, as in 1-3.")
        } else if bounds.len() == 2 and last != "" and number(last) < number(first) {
            std.say("A page range cannot end before it starts.")
        } else { "" }
    }
}

/// Whether `text` is a page's number: digits only, counted from 1.
fn page_number(text: Text) -> Bool {
    let value = text.trim()
    let parsed = number(value)
    value != "" and parsed > 0 and "{parsed}" == value
}

/// `text` as a whole number, or -1 when it is not one.
fn number(text: Text) -> Int {
    text.to_int() ?? -1
}

/// The jobs waiting on `queue`.
fn queue_jobs(jobs: List<device.PrintJob>, queue: Text, active: Bool) -> List<device.PrintJob> {
    jobs.filter(job -> job.destination == queue and job_active(job) == active)
}

/// The scan pages in the order the person put them.
fn ordered(pages: List<ScannedPage>) -> List<ScannedPage> {
    pages.sort_by(page -> page.order)
}

/// The person's document being printed, made the first time it is wanted.
action document_of(p: PrintCentre) -> PrintDocument {
    p.documents.first() ?? p.documents.insert(PrintDocument())
}

/// The person's scanned document, made the first time it is wanted.
action scan_of(p: PrintCentre) -> ScanSession {
    p.scans.first() ?? p.scans.insert(ScanSession())
}

/// The first page's picture, for the print form: a PDF's first page, or the picture itself.
action preview_of_file(file: File) -> Option<File> uses image {
    if file.media.starts_with("image/") {
        .some(file)
    } else if file.media == "application/pdf" or file.name.ends_with(".pdf") {
        match pdf.picture(file, 1, 480) {
            .ok(page) => .some(page),
            .err(why) => .none,
        }
    } else { .none }
}

/// How many pages the file holds, when the device can read them: one for a picture or a text, and
/// one for a file the device cannot count.
fn page_count(file: File) -> Int {
    if file.media == "application/pdf" or file.name.ends_with(".pdf") {
        match pdf.pages(file) {
            .ok(text) => match json.decode<List<PreviewPage>>(text) {
                .ok(pages) => pages.len().max(1),
                .err(why) => 1,
            },
            .err(why) => 1,
        }
    } else { 1 }
}

/// Whether the form's document has a page before the one it previews.
fn has_previous_page(p: PrintCentre) -> Bool {
    if let doc = p.documents.first() { doc.preview_page > 1 } else { false }
}

/// Whether the form's document has a page after the one it previews.
fn has_next_page(p: PrintCentre) -> Bool {
    if let doc = p.documents.first() { doc.preview_page < doc.preview_pages } else { false }
}

/// Shows the page before the one the print form previews.
action previous_page(p: PrintCentre) uses image {
    require has_previous_page(p) "This is the first page"
    let doc = document_of(p)
    show_page(doc, doc.preview_page - 1)
}

/// Shows the page after the one the print form previews.
action next_page(p: PrintCentre) uses image {
    require has_next_page(p) "This is the last page"
    let doc = document_of(p)
    show_page(doc, doc.preview_page + 1)
}

/// Shows `page` of `doc` in the form's preview.
action show_page(doc: PrintDocument, page: Int) uses image {
    let last = doc.preview_pages.max(1)
    let wanted = if page < 1 { 1 } else if page > last { last } else { page }
    if wanted == doc.preview_page {
        return
    }
    if let file = doc.file {
        match pdf.picture(file, wanted, 480) {
            .ok(picture) => {
                doc.preview = .some(picture)
                doc.preview_page = wanted
            }
            .err(why) => {}
        }
    }
}

/// Asks the engine what the chosen printer reports, so the form offers only what it can do.
action ask_options(p: PrintCentre) {
    let doc = document_of(p)
    if doc.queue == "" {
        return
    }
    match engine.call("options", json.encode(OptionsAsk(queue: doc.queue))) {
        .ok(text) => match json.decode<PrinterOptions>(text) {
            .ok(options) => {
                p.options = options
            }
            .err(why) => {
                doc.trouble = why
            }
        },
        .err(why) => {
            doc.trouble = why
        }
    }
}

/// Takes the file the person chose as what will be printed.
action absorb_document(p: PrintCentre, file: File) {
    let doc = document_of(p)
    doc.file = .some(file)
    doc.name = file.name
    doc.trouble = ""
    doc.preview = .none
    doc.preview_state = "preparing"
    doc.preview_page = 1
    doc.preview_pages = page_count(file)
    if doc.queue == "" {
        let printers = device.printers
        if let first = printers.queues.first() {
            doc.queue = first.name
        }
    }
    ask_options(p)
}

/// Draws the first page of the chosen document for the form: the device takes a moment, and the
/// form says what it is doing while it waits.
action prepare_preview(p: PrintCentre) uses image {
    let doc = document_of(p)
    if doc.preview_state != "preparing" {
        return
    }
    if let file = doc.file {
        if let picture = preview_of_file(file) {
            doc.preview = .some(picture)
            doc.preview_state = ""
        } else {
            doc.preview_state = "unavailable"
        }
    } else {
        doc.preview_state = ""
    }
}

/// Takes a document another app hands over as what this window prints, and shows the print
/// form so the person chooses the printer and how it prints: the one action an app calls to
/// print a file it made. The file's bytes are the workspace's content, so the app that hands
/// it over never needs this window's own files.
action print_file(p: PrintCentre, file: File) {
    absorb_document(p, file)
    let doc = document_of(p)
    doc.handed = true
}

/// Leaves the print form a handed-over document opened, so the person's own choice of place
/// stands.
action leave_handed(p: PrintCentre) {
    if let doc = p.documents.first() {
        doc.handed = false
    }
}

/// Whether a document another app handed over waits on the person's print form.
fn handed(p: PrintCentre) -> Bool {
    if let doc = p.documents.first() { doc.handed } else { false }
}

/// Prints the chosen document on the chosen printer, with the form's settings.
action send_to_printer(p: PrintCentre) {
    let doc = document_of(p)
    let range = page_range_error(doc.pages)
    if doc.queue == "" {
        doc.failed = std.say("Choose a printer")
    } else if range != "" {
        doc.failed = range
    } else if let file = doc.file {
        let asked = PrintAsk(
            queue: doc.queue,
            url: file.url,
            name: doc.name,
            copies: doc.copies.to_int(),
            pages: doc.pages,
            two_sided: doc.two_sided,
            page_size: doc.page_size,
            colour: doc.colour,
            duplex: doc.duplex,
            orientation: doc.orientation,
        )
        match engine.start("print", json.encode(asked), done: j -> print_ended(doc, j)) {
            .err(why) => {
                doc.failed = why
            }
            .ok(job) => {
                doc.job = job
                doc.failed = ""
                notice("Sent to {doc.queue}", kind: .success)
            }
        }
    } else {
        doc.failed = std.say("Choose a document to print")
    }
}

/// The print ended, printed or failed; it says which.
action print_ended(doc: PrintDocument, j: host.Job) {
    if doc.job != j.id {
        return
    }
    doc.job = ""
    if j.state == .failed {
        doc.failed = j.error
    } else if j.state == .done {
        doc.failed = ""
    }
}

/// Scans one page with the chosen scanner and the defaults.
action scan_page(p: PrintCentre) {
    let scan = scan_of(p)
    let scanners = device.scanners
    let scanner = if scan.scanner != "" {
        scan.scanner
    } else if let first = scanners.found.first() { first.device } else { "" }
    if scanner == "" {
        scan.failed = std.say("This computer has no scanner")
        return
    }
    scan.scanner = scanner
    let asked = ScanAsk(
        scanner: scanner,
        resolution: p.defaults.scan_resolution,
        colour: p.defaults.scan_colour,
        source: p.defaults.scan_source,
        name: "scan-{scan.pages.len() + 1}.png",
    )
    match engine.start("scan", json.encode(asked), done: j -> scan_ended(scan, j)) {
        .err(why) => {
            scan.failed = why
        }
        .ok(job) => {
            scan.job = job
            scan.failed = ""
        }
    }
}

/// The scan ended; a page that arrived is added to the document.
action scan_ended(scan: ScanSession, j: host.Job) {
    if scan.job != j.id {
        return
    }
    scan.job = ""
    if j.state == .done {
        match json.decode<ScanDetail>(j.detail) {
            .ok(detail) => {
                let name = "scan-{scan.pages.len() + 1}.png"
                scan.pages.insert(
                    ScannedPage(file: File.at(name, detail.url), order: scan.pages.len()),
                )
                scan.failed = ""
            }
            .err(why) => {
                scan.failed = why
            }
        }
    } else if j.state == .failed {
        scan.failed = j.error
    }
}

/// Turns a scanned page a quarter clockwise.
action turn_page(page: ScannedPage) uses image {
    match page.file.turned(1) {
        .ok(turned) => {
            page.file = turned
        }
        .err(why) => {}
    }
}

/// Crops a scanned page to the part the person kept, in fractions of the picture a viewer
/// shows. The text read off the whole page no longer describes it, so it is cleared.
action crop_page(
    p: PrintCentre,
    page: ScannedPage,
    left: Float,
    top: Float,
    right: Float,
    bottom: Float,
) uses image {
    let x = clamp_fraction(left)
    let y = clamp_fraction(top)
    let width = clamp_fraction(right) - x
    let height = clamp_fraction(bottom) - y
    if width <= 0.0 or height <= 0.0 {
        return
    }
    match page.file.cropped(x, y, width, height) {
        .ok(cropped) => {
            page.file = cropped
            page.text = ""
        }
        .err(why) => {
            if let scan = p.scans.first() {
                scan.failed = why
            }
        }
    }
}

fn clamp_fraction(value: Float) -> Float {
    if value < 0.0 { 0.0 } else if value > 1.0 { 1.0 } else { value }
}

/// Throws a scanned page away.
action remove_page(p: PrintCentre, page: ScannedPage) {
    for scan in p.scans {
        scan.pages.remove(page)
    }
}

/// Starts a new scanned document: the pages it held go. The change is the person's own and is
/// taken back from history; it is refused while a page is still being scanned, so a scan in
/// flight is never thrown away.
action new_scanned_document(p: PrintCentre) (icon: .file_plus, label: "New scanned document") {
    require not scanning(p) "Wait for the page to finish scanning before starting a new document"
    for scan in p.scans {
        for page in scan.pages {
            scan.pages.remove(page)
        }
        scan.failed = ""
    }
}

/// Whether a page is being scanned now.
fn scanning(p: PrintCentre) -> Bool {
    if let scan = p.scans.first() { scan.job != "" } else { false }
}

/// Moves a scanned page to `order`, and the page that stood there to where it was.
action move_page(p: PrintCentre, page: ScannedPage, order: Int, swapped: Int = page.order) {
    for scan in p.scans {
        if order >= 0 and order < scan.pages.len() {
            for other in scan.pages {
                if other.order == order {
                    other.order = swapped
                }
            }
            page.order = order
        }
    }
}

/// Starts reading the text off a scanned page in the person's language: the page says it is
/// reading until it has it.
action start_read(page: ScannedPage) {
    page.reading = true
}

/// Reads the text off a scanned page in the person's language.
action recognize(page: ScannedPage) {
    let p = root PrintCentre
    let language = p.defaults.ocr_language
    page.reading = false
    match engine.call("ocr", json.encode(OcrAsk(url: page.file.url, language: language))) {
        .ok(text) => match json.decode<OcrAnswer>(text) {
            .ok(answer) => {
                page.text = answer.text
            }
            .err(why) => {}
        },
        .err(why) => {}
    }
}

/// Copies a scanned page's text.
action copy_text(page: ScannedPage) uses clipboard {
    if page.text != "" { clipboard.copy(page.text) }
}

/// The scanned page as a file to save.
action page_to_save(page: ScannedPage) -> File {
    page.file
}

/// The scanned document as one searchable PDF, to save.
action document_to_save(p: PrintCentre) -> File {
    let pages = if let scan = p.scans.first() {
        ordered(scan.pages.map(page -> page))
            .map(page -> PdfPage(url: page.file.url, text: page.text))
    } else { [] }
    match engine.call(
        "pages_pdf",
        json.encode(PagesAsk(pages: pages, name: "scanned document.pdf")),
    ) {
        .ok(text) => match json.decode<UrlAnswer>(text) {
            .ok(answer) => File.at("scanned document.pdf", answer.url),
            .err(why) => page_placeholder(),
        },
        .err(why) => page_placeholder(),
    }
}

/// A file that is nothing, for a save that could not make its pages.
fn page_placeholder() -> File {
    File.of_text("scanned document.txt", "")
}

/// Chooses a printer, and offers what it reports.
action choose_printer(p: PrintCentre, queue: Text) {
    let doc = document_of(p)
    doc.queue = queue
    ask_options(p)
}

/// Prints on both sides of the paper, or one.
action set_two_sided(p: PrintCentre, two_sided: Bool) (
    did: if two_sided {
    "set printing to both sides from now on"
} else { "set printing to one side from now on" },
) {
    let doc = document_of(p)
    doc.two_sided = two_sided
    p.defaults.two_sided = two_sided
}

/// One of the printer's own options.
action set_print_option(p: PrintCentre, option: Text, value: Text) {
    let doc = document_of(p)
    if option == "PageSize" {
        doc.page_size = value
    } else if option == "ColorModel" {
        doc.colour = value
    } else if option == "Duplex" {
        doc.duplex = value
    } else if option == "Orientation" {
        doc.orientation = value
    }
}

/// How finely to scan.
action set_resolution(p: PrintCentre, dpi: Int) (did: "set scanning to {dpi} dpi from now on") {
    if dpi == 150 or dpi == 300 or dpi == 600 {
        p.defaults.scan_resolution = dpi
    }
}

/// How to scan: in colour, grayscale or line art.
action set_scan_colour(p: PrintCentre, colour: Text) (
    did: if colour == "grayscale" {
    "set scanning to grayscale from now on"
} else if colour == "line-art" {
    "set scanning to line art from now on"
} else { "set scanning to color from now on" },
) {
    if colour == "color" or colour == "grayscale" or colour == "line-art" {
        p.defaults.scan_colour = colour
    }
}

/// Where to scan from.
action set_scan_source(p: PrintCentre, source: Text) (
    did: if source == "feeder" {
    "set scanning from the feeder from now on"
} else if source == "feeder-duplex" {
    "set scanning both sides from the feeder from now on"
} else {
    "set scanning from the flatbed from now on"
},
) {
    if source == "flatbed" or source == "feeder" or source == "feeder-duplex" {
        p.defaults.scan_source = source
    }
}

/// The language text is recognized in.
action set_ocr_language(p: PrintCentre, language: Text) (
    did: if language == "deu" {
    "set text recognition to German from now on"
} else if language == "fra" {
    "set text recognition to French from now on"
} else if language == "spa" {
    "set text recognition to Spanish from now on"
} else {
    "set text recognition to English from now on"
},
) {
    if language == "eng" or language == "deu" or language == "fra" or language == "spa" {
        p.defaults.ocr_language = language
    }
}

/// The window: the printers, their jobs, the scanner, and the print and scan forms.
view window(p: PrintCentre) {
    state place: Text = "dashboard"
    // The page whose crop frame is open, by its id, and the part of it the person kept.
    state cropping: Text = ""
    state crop_edges: List<Float> = [0.0, 0.0, 1.0, 1.0]
    // The job the person asked to cancel, while they are asked to confirm it.
    state cancel_job: Text = ""
    let printers = device.printers
    let scanners = device.scanners
    // A document another app handed over opens the person's print form, so they choose the
    // printer and how it prints; any place they pick takes the window back.
    let here = if handed(p) { "print" } else { place }
    on show dashboard "Print overview": set(place, "dashboard").then(leave_handed(p))
    on show queue "Print queue": set(place, "queue").then(leave_handed(p))
    on show history "Print history": set(place, "history").then(leave_handed(p))
    on show print "Print a document": set(place, "print").then(leave_handed(p))
    on show scan "Scanner": set(place, "scan").then(leave_handed(p))
    ui.NavShell(content_kind: .page, title: "Print Centre", sidebar_title: "Print Centre") {
        sidebar: {
            stack {
                ui.NavList(selected: here) {
                    ui.NavSection("Printing") {
                        ui.NavItem("dashboard", "Overview", icon: .gauge) {
                            on select: set(place, "dashboard").then(leave_handed(p))
                        }
                        ui.NavItem(
                            "queue",
                            "Queue",
                            icon: .list,
                            count: printers.jobs.count(job -> job_active(job)),
                        ) {
                            on select: set(place, "queue").then(leave_handed(p))
                        }
                        ui.NavItem("history", "History", icon: .clock) {
                            on select: set(place, "history").then(leave_handed(p))
                        }
                        ui.NavItem("print", "Print a document", icon: .printer) {
                            on select: set(place, "print").then(leave_handed(p))
                        }
                    }
                    ui.NavSection("The machine") {
                        ui.NavItem("diagnostics", "Diagnostics", icon: .activity) {
                            on select: set(place, "diagnostics").then(leave_handed(p))
                        }
                        ui.NavItem("discover", "Find printers", icon: .search) {
                            on select: set(place, "discover").then(leave_handed(p))
                        }
                        ui.NavItem("scan", "Scan", icon: .scan_line, count: scan_page_count(p)) {
                            on select: set(place, "scan").then(leave_handed(p))
                        }
                    }
                }
                if not printers.serving {
                    ui.StatusLine("This computer has no printer service", status: .warning)
                }
            }
        }
        actions: {
            if here == "print" {
                ui.Button("Print", icon: .printer, role: .primary) {
                    on press: send_to_printer(p)
                }
            }
            if here == "scan" {
                ui.Button("Scan a page", icon: .scan_line, role: .primary) {
                    on press: scan_page(p)
                }
            }
            if here == "discover" {
                ui.Button("Look for printers", icon: .search) {
                    on press: device.find(device.printers)
                }
            }
        }
        stack(gap: .group) {
            if let problem = printers.problem {
                ui.Banner("The printer service refused that", message: problem, kind: .error)
            }
            if printers.pending.contains("find") {
                ui.StatusLine("Looking for printers on your network.", status: .active)
            }
            if not printers.serving {
                ui.EmptyState(
                    "No printer service",
                    message: "This computer cannot print: no printer service is answering.",
                    icon: .printer,
                )
            } else if here == "dashboard" {
                dashboard(p, printers, scanners, cancel_job)
            } else if here == "queue" {
                queue_pane(printers, cancel_job)
            } else if here == "history" {
                history_pane(printers, cancel_job)
            } else if here == "diagnostics" {
                diagnostics_pane(printers, scanners)
            } else if here == "discover" {
                discover_pane(printers)
            } else if here == "print" {
                if let doc = p.documents.first() {
                    print_pane(p, doc, printers)
                } else {
                    print_form(p, printers)
                }
            } else if let scan = p.scans.first() {
                scan_pane(p, scan, scanners, cropping, crop_edges)
            } else {
                scan_form(p, scanners)
            }
        }
    }
    cancel_dialog(printers, cancel_job)
}

/// Asks before a waiting document goes: taking it out of the printer's queue cannot be undone.
view cancel_dialog(printers: device.Printers, cancel: State<Text>) {
    let job = printers.jobs.find(job -> job.id == cancel and job_active(job))
    if let job = job {
        let name = if job.title != "" { job.title } else { std.say("Untitled document") }
        ui.ModalDialog(
            title: "Cancel this document?",
            description: .some("{name} will be removed from the printer. This cannot be undone."),
            open: true,
            primary: "Cancel this document",
            secondary: "Keep it",
            destructive: true,
        ) {
            on act_primary: device.cancel_job(device.printers, job: job.id).then(set(cancel, ""))
            on act_secondary: set(cancel, "")
            on dismiss: set(cancel, "")
        }
    }
}

/// What the computer prints to, what waits, and what it can scan, at a glance.
view dashboard(
    p: PrintCentre,
    printers: device.Printers,
    scanners: device.Scanners,
    cancel: State<Text>,
) {
    ui.StatGroup {
        ui.StatTile("Printers", "{printers.queues.len()}")
        ui.StatTile("Waiting", "{printers.jobs.count(job -> job_active(job))}")
        ui.StatTile("Scanners", "{scanners.found.len()}")
    }
    ui.Section("Printers") {
        if printers.queues.is_empty() {
            ui.EmptyState("No printers", message: "Look for one on the network.", icon: .printer)
        }
        for queue in printers.queues {
            printer_card(queue, printers)
        }
    }
    let waiting = printers.jobs.filter(job -> job_active(job))
    if not waiting.is_empty() {
        ui.Section("Printing now") {
            for job in waiting {
                job_row(job, printers, cancel)
            }
        }
    }
    let failed = printers.jobs.filter(job -> job_failed(job))
    if not failed.is_empty() {
        ui.Section("Needs attention") {
            for job in failed {
                job_row(job, printers, cancel)
            }
        }
    }
}

/// One printer: how it stands, its supplies, and what can be done to it.
view printer_card(queue: device.PrinterQueue, printers: device.Printers) {
    ui.Card(title: queue.name) {
        row(gap: .tight, align: .center, wrap: true) {
            ui.Badge(
                queue_words(queue.state),
                tone: if queue.state == .ready {
                    .success
                } else if queue.state == .paused or queue.state == .offline {
                    .warning
                } else { .info },
            )
            if queue.default {
                ui.Badge("Default", tone: .info)
            }
            if not queue.reachable {
                ui.Badge("Not reachable", tone: .danger)
            }
        }
        if let code = queue.reason_code {
            ui.Text(reason_words(code), role: .caption, secondary: true)
        }
        if let reason = queue.reason {
            ui.Text(reason, role: .caption, secondary: true)
        }
        for supply in queue.supplies {
            if let level = supply.level {
                ui.ProgressBar(
                    value: level.to_float() / 100.0,
                    label: supply.name,
                    warning: 0.2,
                    danger: 0.1,
                )
            } else {
                ui.Text("{supply.name}: its level is not reported", role: .caption, secondary: true)
            }
        }
        if not queue.media.is_empty() {
            ui.Text("Paper it prints on unless told otherwise", role: .caption, secondary: true)
            row(gap: .tight, wrap: true) {
                for size in queue.media {
                    ui.Chip(size, selected: queue.default_media == .some(size)) {
                        on press: device.set_media(
                            device.printers,
                            printer: queue.name,
                            media: size,
                        )
                    }
                }
            }
        }
        row(gap: .tight, wrap: true) {
            if not queue.default {
                ui.Button("Use first", size: .small) {
                    on press: device.set_default(device.printers, printer: queue.name)
                }
            }
            ui.Button(if queue.state == .paused { "Resume" } else { "Pause" }, size: .small) {
                on press: device.set_paused(
                    device.printers,
                    printer: queue.name,
                    paused: queue.state != .paused,
                )
            }
            ui.Button("Check printer", icon: .refresh, size: .small) {
                on press: device.find(device.printers)
            }
            ui.Button("Remove", icon: .trash, icon_only: true, role: .destructive, size: .small) {
                on press: device.remove(device.printers, printer: queue.name)
            }
        }
    }
}

/// One job: what it is, where it goes and how far it is.
view job_row(job: device.PrintJob, printers: device.Printers, cancel: State<Text>) {
    // A job that stopped or failed says why its printer stands as it does, when it says.
    let queue = printers.queues.find(q -> q.name == job.destination)
    let why: Option<Text> = if not job_failed(job) {
        .none
    } else if let q = queue {
        if let code = q.reason_code {
            .some("{reason_words(code)} {q.reason ?? ""}")
        } else { q.reason }
    } else { .none }
    ui.ListRow(
        if job.title != "" { job.title } else { std.say("Untitled document") },
        secondary: if let reason = why {
            "{job.destination} - {reason}"
        } else { "{job.destination} - {job.owner}" },
        value: job_words(job.state),
        meta: size_words(job.size_bytes),
        meta_tone: if job.state == .failed {
            .danger
        } else if job.state == .printing { .info } else { .neutral },
        icon: .file,
    ) {
        trailing: {
            row(gap: .tight) {
                if job_active(job) {
                    ui.Button(
                        "Cancel",
                        icon: .x,
                        icon_only: true,
                        role: .destructive,
                        size: .small,
                    ) {
                        on press: set(cancel, job.id)
                    }
                } else if job_failed(job) {
                    ui.Button("Retry", size: .small) {
                        on press: device.retry_job(device.printers, job: job.id)
                    }
                } else {
                    ui.Button("Print again", icon: .refresh, size: .small) {
                        on press: device.retry_job(device.printers, job: job.id)
                    }
                }
            }
        }
    }
    if let progress = job.progress {
        ui.ProgressBar(value: progress.to_float() / 100.0, label: "How far it is")
    }
}

/// The jobs that wait or print, queue by queue.
view queue_pane(printers: device.Printers, cancel: State<Text>) {
    let waiting = printers.jobs.filter(job -> job_active(job))
    if waiting.is_empty() {
        ui.EmptyState("Nothing waits to print", icon: .printer)
    }
    for queue in printers.queues {
        let jobs = queue_jobs(printers.jobs, queue.name, true)
        if not jobs.is_empty() {
            ui.Section(queue.name) {
                for job in jobs {
                    job_row(job, printers, cancel)
                }
            }
        }
    }
}

/// What this computer printed, and what did not come out.
view history_pane(printers: device.Printers, cancel: State<Text>) {
    let finished = printers.jobs.filter(job -> not job_active(job))
    if finished.is_empty() {
        ui.EmptyState("Nothing printed yet", icon: .clock)
    }
    stack(gap: .none) {
        for job in finished {
            job_row(job, printers, cancel)
        }
    }
}

/// How many of this computer's printers answered the latest check.
fn responding(printers: device.Printers) -> Int {
    printers.queues.count(queue -> queue.reachable)
}

/// What each printer says about itself: whether it answers, why it stands as it does, its paper
/// sizes and its supplies.
view diagnostics_pane(printers: device.Printers, scanners: device.Scanners) {
    ui.StatGroup {
        ui.StatTile("Printers responding", "{responding(printers)} of {printers.queues.len()}")
    }
    for queue in printers.queues {
        ui.Section(queue.name) {
            ui.PropertyList {
                ui.Property("State", queue_words(queue.state))
                ui.Property("Accepting jobs", if queue.accepting { "Yes" } else { "No" })
                ui.Property("Reachable", if queue.reachable { "Yes" } else { "No" })
                if let code = queue.reason_code {
                    ui.Property("Reason", reason_words(code))
                }
                if let reason = queue.reason {
                    ui.Property("Reported", reason)
                }
                if let media = queue.default_media {
                    ui.Property("Paper", media)
                }
                ui.Property("Paper sizes", queue.media.join(", "))
            }
            if not queue.accepting {
                ui.Text("Not accepting new documents", role: .caption, secondary: true)
            }
            if queue.reachable {
                ui.Text(
                    "The printer answered the latest status check.",
                    role: .caption,
                    secondary: true,
                )
            } else {
                ui.Text(
                    "Live details are unavailable. The printer may be asleep or disconnected.",
                    role: .caption,
                    secondary: true,
                )
            }
            if queue.supplies.is_empty() {
                ui.Text(
                    "This printer does not report supply levels.",
                    role: .caption,
                    secondary: true,
                )
            }
            if not queue.supplies.is_empty() {
                ui.PropertyList {
                    for supply in queue.supplies {
                        if let level = supply.level {
                            ui.Property(supply.name, "{level}% left")
                        } else {
                            ui.Property(supply.name, "Its level is not reported")
                        }
                    }
                }
            }
        }
    }
    ui.Section("Scanners") {
        if scanners.found.is_empty() {
            ui.Text("No scanner answers on this computer.", role: .caption, secondary: true)
        }
        for scanner in scanners.found {
            ui.ListRow(scanner.description, secondary: scanner.device, icon: .scan_line)
        }
        ui.Button("Look for scanners", icon: .search) {
            on press: device.find_scanners(device.scanners)
        }
    }
}

/// The printers found on the network, each one press from being added.
view discover_pane(printers: device.Printers) {
    if printers.nearby.is_empty() {
        ui.EmptyState(
            "No printers found",
            message: "Look for the printers this computer can reach on its network.",
            icon: .search,
            action: "Look for printers",
        ) {
            on act: device.find(device.printers)
        }
    }
    stack(gap: .none) {
        for found in printers.nearby {
            ui.ListRow(
                found.name,
                secondary: if let location = found.location { location } else { found.uri },
                icon: .printer,
            ) {
                trailing: {
                    ui.Button("Add", size: .small, role: .primary) {
                        on press: device.add(device.printers, uri: found.uri, name: found.name)
                    }
                }
            }
        }
    }
}

/// The print form before anything is chosen: one press to a document, and the printers this
/// computer prints to beside it.
view print_form(p: PrintCentre, printers: device.Printers) {
    ui.EmptyState(
        "Print a document",
        message: "Choose what to print, then the printer and how it prints.",
        icon: .printer,
        action: "Choose a document",
    ) {
        on act: file.choose(
            absorb_document(p),
            accept: [
                "application/pdf",
                "image/png",
                "image/jpeg",
                "text/plain",
                ".pdf",
                ".png",
                ".jpg",
                ".jpeg",
                ".txt",
            ],
        )
    }
    if printers.queues.is_empty() {
        ui.Text("No printer is added on this computer.", role: .caption, secondary: true)
    } else {
        ui.Section("Printer") {
            for queue in printers.queues {
                ui.ListRow(queue.name, secondary: queue_words(queue.state), icon: .printer) {
                    on activate: choose_printer(p, queue.name)
                }
            }
        }
    }
}

/// The print form: the document, the printer, the copies and the printer's own options.
view print_pane(p: PrintCentre, doc: PrintDocument, printers: device.Printers) {
    ui.ObjectHeader(
        if doc.name != "" { doc.name } else { std.say("Print a document") },
        caption: if let file = doc.file { file.media } else { "" },
        icon: .printer,
    )
    row(gap: .tight, wrap: true) {
        ui.Button("Choose a document", icon: .file_plus) {
            on press: file.choose(
                absorb_document(p),
                accept: [
                    "application/pdf",
                    "image/png",
                    "image/jpeg",
                    "text/plain",
                    ".pdf",
                    ".png",
                    ".jpg",
                    ".jpeg",
                    ".txt",
                ],
            )
        }
    }
    if doc.preview_state == "preparing" {
        ui.StatusLine("Preparing document preview", status: .active)
        every 1s: prepare_preview(p)
    } else if let preview = doc.preview {
        ui.Image(src: preview.url, alt: doc.name, fit: .contain, aspect: 0.7727)
        ui.Text("The original document is unchanged.", role: .caption, secondary: true)
        if doc.preview_pages > 1 {
            row(gap: .tight, align: .center) {
                ui.Button("Previous", icon: .chevron_left, size: .small) {
                    on press: previous_page(p)
                }
                ui.Button("Next", icon: .chevron_right, size: .small) {
                    on press: next_page(p)
                }
                ui.Text(
                    "Page {doc.preview_page} of {doc.preview_pages}",
                    role: .caption,
                    secondary: true,
                )
            }
        }
    } else if doc.preview_state == "unavailable" {
        ui.Text("Preview unavailable", role: .caption, secondary: true)
    }
    let failed = doc.failed
    if failed != "" {
        ui.StatusLine(failed, status: .error)
    }
    if doc.job != "" {
        ui.StatusLine("The document is going to the printer", status: .active)
    }
    ui.Section("Printer") {
        if printers.queues.is_empty() {
            ui.Text("No printer is added on this computer.", role: .caption, secondary: true)
        }
        for queue in printers.queues {
            ui.ListRow(
                queue.name,
                secondary: queue_words(queue.state),
                selected: doc.queue == queue.name,
                icon: .printer,
            ) {
                on activate: choose_printer(p, queue.name)
            }
        }
    }
    form {
        field("Copies") {
            ui.NumberInput(bind: doc.copies, min: 1.0, max: 99.0, precision: 0)
        }
        field("Pages", help: "Empty for all of them; `1-3,5` for a range.") {
            ui.TextInput(bind: doc.pages, placeholder: "1-3")
        }
        if page_range_error(doc.pages) != "" {
            ui.StatusLine(page_range_error(doc.pages), status: .error)
        }
        field("Both sides") {
            ui.Toggle(bind: doc.two_sided, label: "Print on both sides") {
                on check: set_two_sided(p, doc.two_sided)
            }
        }
    }
    if not p.options.page_sizes.is_empty() {
        ui.Section("Paper") {
            choice_row(
                p.options.page_sizes,
                doc.page_size,
                option -> set_print_option(p, "PageSize", option),
            )
        }
    }
    if not p.options.colour_modes.is_empty() {
        ui.Section("Colour") {
            choice_row(
                p.options.colour_modes,
                doc.colour,
                option -> set_print_option(p, "ColorModel", option),
            )
        }
    }
    if not p.options.duplex_modes.is_empty() {
        ui.Section("Two sides") {
            choice_row(
                p.options.duplex_modes,
                doc.duplex,
                option -> set_print_option(p, "Duplex", option),
            )
        }
    }
    if not p.options.orientations.is_empty() {
        ui.Section("Orientation") {
            choice_row(
                p.options.orientations,
                doc.orientation,
                option -> set_print_option(p, "Orientation", option),
            )
        }
    }
}

/// One row of the printer's own choices, the chosen one marked.
view choice_row(choices: List<Choice>, chosen: Text, pick: (Text) -> Action) {
    row(gap: .tight, wrap: true) {
        for choice in choices {
            ui.Chip(choice.label, selected: chosen == choice.key) {
                on press: pick(choice.key)
            }
        }
    }
}

/// The scan form before a page is scanned: how to scan, and the scanner.
view scan_form(p: PrintCentre, scanners: device.Scanners) {
    if scanners.found.is_empty() {
        ui.EmptyState(
            "No scanner",
            message: "This computer has no scanner, or none answered.",
            icon: .scan_line,
            action: "Look for scanners",
        ) {
            on act: device.find_scanners(device.scanners)
        }
    } else {
        ui.EmptyState(
            "Scan a page",
            message: "Put a page on the scanner and scan it; more pages follow.",
            icon: .scan_line,
            action: "Scan a page",
        ) {
            on act: scan_page(p)
        }
    }
}

/// The scan form: the scanner, how to scan, and the pages as they come.
view scan_pane(
    p: PrintCentre,
    scan: ScanSession,
    scanners: device.Scanners,
    cropping: State<Text>,
    crop_edges: State<List<Float>>,
) {
    ui.ObjectHeader("Scan", caption: "Pages scanned on this computer", icon: .scan_line)
    if scanners.found.is_empty() {
        ui.EmptyState(
            "No scanner",
            message: "This computer has no scanner, or none answered.",
            icon: .scan_line,
            action: "Look for scanners",
        ) {
            on act: device.find_scanners(device.scanners)
        }
    } else {
        ui.Section("Scanner") {
            for scanner in scanners.found {
                ui.ListRow(
                    scanner.description,
                    secondary: scanner.device,
                    selected: scan.scanner == scanner.device
                        or scan.scanner == ""
                            and scanner.device == (scanners.found.first()?.device ?? ""),
                    icon: .scan_line,
                ) {
                    on activate: set_scanner(p, scanner.device)
                }
            }
        }
    }
    form {
        field("Resolution") {
            row(gap: .tight, wrap: true) {
                ui.Chip("150 dpi", selected: p.defaults.scan_resolution == 150) {
                    on press: set_resolution(p, 150)
                }
                ui.Chip("300 dpi", selected: p.defaults.scan_resolution == 300) {
                    on press: set_resolution(p, 300)
                }
                ui.Chip("600 dpi", selected: p.defaults.scan_resolution == 600) {
                    on press: set_resolution(p, 600)
                }
            }
        }
        field("Colour") {
            row(gap: .tight, wrap: true) {
                ui.Chip("Colour", selected: p.defaults.scan_colour == "color") {
                    on press: set_scan_colour(p, "color")
                }
                ui.Chip("Grayscale", selected: p.defaults.scan_colour == "grayscale") {
                    on press: set_scan_colour(p, "grayscale")
                }
                ui.Chip("Line art", selected: p.defaults.scan_colour == "line-art") {
                    on press: set_scan_colour(p, "line-art")
                }
            }
        }
        field("Source") {
            row(gap: .tight, wrap: true) {
                ui.Chip("Flatbed", selected: p.defaults.scan_source == "flatbed") {
                    on press: set_scan_source(p, "flatbed")
                }
                ui.Chip("Feeder", selected: p.defaults.scan_source == "feeder") {
                    on press: set_scan_source(p, "feeder")
                }
                ui.Chip("Feeder, both sides", selected: p.defaults.scan_source == "feeder-duplex") {
                    on press: set_scan_source(p, "feeder-duplex")
                }
            }
        }
        field("Text language") {
            row(gap: .tight, wrap: true) {
                for language in ocr_languages() {
                    ui.Chip(
                        language,
                        selected: p.defaults.ocr_language == language_code(language),
                    ) {
                        on press: set_ocr_language(p, language_code(language))
                    }
                }
            }
        }
    }
    if scan.job != "" {
        ui.StatusLine("The scanner is working", status: .active)
    }
    let failed = scan.failed
    if failed != "" {
        ui.StatusLine(failed, status: .error)
    }
    let pages = ordered(scan.pages.map(page -> page))
    if not pages.is_empty() {
        ui.Section("Pages") {
            ui.Text(
                if pages.len() == 1 {
                    "1 page captured. New scans are added to this document."
                } else {
                    "{pages.len()} pages captured. New scans are added to this document."
                },
                role: .caption,
                secondary: true,
            )
            ui.Text(
                "Press a page to turn it, and use its row's controls to order it.",
                role: .caption,
                secondary: true,
            )
            for page in pages {
                scanned_page(p, page, cropping, crop_edges)
            }
        }
        row(gap: .tight, wrap: true) {
            ui.Button("Scan another page", icon: .scan_line) {
                on press: scan_page(p)
            }
            ui.Button("New scanned document", icon: .file_plus, role: .destructive) {
                on press: new_scanned_document(p)
            }
            ui.Button("Save the document as a PDF", icon: .download, role: .primary) {
                on press: file.save(document_to_save(p), name: "scanned document.pdf")
            }
        }
    }
}

action set_scanner(p: PrintCentre, device_name: Text) {
    let scan = scan_of(p)
    scan.scanner = device_name
}

fn ocr_languages() -> List<Text> {
    ["English", "German", "French", "Spanish"]
}

fn language_code(name: Text) -> Text {
    if name == "German" {
        "deu"
    } else if name == "French" { "fra" } else if name == "Spanish" { "spa" } else { "eng" }
}

/// One scanned page: its picture, the text read off it, and what can be done with it.
view scanned_page(
    p: PrintCentre,
    page: ScannedPage,
    cropping: State<Text>,
    crop_edges: State<List<Float>>,
) {
    ui.Card(title: if page.text != "" { first_line(page.text) } else { std.say("Page") }) {
        ui.Image(src: page.file.url, alt: "Scanned page", fit: .contain, aspect: 0.7727)
        if cropping == page.id() {
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
                            p,
                            page,
                            crop_edges.at(0) ?? 0.0,
                            crop_edges.at(1) ?? 0.0,
                            crop_edges.at(2) ?? 1.0,
                            crop_edges.at(3) ?? 1.0,
                        )
                        .then(set(cropping, ""))
                }
                ui.Button("Cancel crop", icon: .x) {
                    on press: set(cropping, "")
                }
            }
        } else {
            ui.Button("Crop", icon: .scissors, size: .small) {
                on press: set(crop_edges, [0.0, 0.0, 1.0, 1.0]).then(set(cropping, page.id()))
            }
        }
        row(gap: .tight, wrap: true) {
            ui.Button("Turn", icon: .rotate_cw, size: .small) {
                on press: turn_page(page)
            }
            ui.Button("Earlier", icon: .arrow_up, icon_only: true, size: .small) {
                on press: move_page(p, page, page.order - 1)
            }
            ui.Button("Later", icon: .arrow_down, icon_only: true, size: .small) {
                on press: move_page(p, page, page.order + 1)
            }
            ui.Button("Read its text", icon: .file_text) {
                on press: start_read(page)
            }
            if page.reading {
                ui.StatusLine("Updating recognized text…", status: .active)
                every 1s: recognize(page)
            }
            if page.text != "" {
                ui.Button("Copy the text", icon: .copy) {
                    on press: copy_text(page)
                }
            }
            ui.Button("Save this page", icon: .download) {
                on press: file.save(page_to_save(page), name: page.file.name)
            }
            ui.Button("Remove", icon: .trash, icon_only: true, role: .destructive, size: .small) {
                on press: remove_page(p, page)
            }
        }
        if page.text != "" {
            ui.Text(page.text, role: .caption, secondary: true, lines: 4)
        }
    }
}

fn first_line(text: Text) -> Text {
    text.split("\n").first() ?? text
}
