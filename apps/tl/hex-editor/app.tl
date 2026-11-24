app so.tilo.hex-editor "Hex Editor" (
    icon: .file_code,
    category: .development,
    tagline: "Reads and changes a file's bytes",
    keywords: ["hex editor", "hex", "bytes", "binary", "binary editor", "editor", "disk", "byte", "hexadezimal", "binär"],
    document: library.documents,
)

/// One edit of the bytes: at `at`, `remove` bytes stand as `insert`'s hexadecimal ones.
// ---------------------------------------------------------------- what a document holds
type Change {
    at: Int = 0
    remove: Int = 0
    insert: Text
}

/// One row of the grid as the device read it: its bytes as hexadecimal pairs, and for each a
/// `0` or `1` saying whether it differs from what the file holds.
type Row {
    hex: Text
    changed: Text = ""
}

/// One way the bytes at the cursor read: `u16` is 4660 there.
type Finding {
    label: Text
    value: Text = ""
    width: Int = 0
}

/// A span of bytes, half open.
type Span {
    start: Int = 0
    end: Int = 0
}

/// One person's place in a document: their cursor, how they read the bytes, and what the device
/// last read for their window. It is the reader's own, so two people at one file never turn
/// each other's pages.
type Desk (readers: author) {
    cursor: Int = 0
    anchor: Option<Int> = .none
    /// `hex` or `ascii`: which column the keys type into.
    column: Text = "hex"
    /// `overwrite` replaces the byte under the cursor; `insert` splices a new one in.
    mode: Text = "overwrite"
    /// The high nibble typed in overwrite mode, which the next digit completes; none between
    /// bytes.
    high: Option<Int> = .none
    /// `little` or `big`: the order the inspector reads numbers in.
    endian: Text = "little"
    /// The first row the window shows.
    first: Int = 0
    /// The bytes a row holds.
    columns: Int = 16
    /// The rows the window shows, as the device read them: each its bytes as hexadecimal
    /// pairs.
    rows: List<Text>
    /// For each row shown, a `0` or `1` for each of its bytes: whether it differs from what
    /// the file holds.
    changed: List<Text>
    /// What the bytes at the cursor read as: a label and a value, a tab between them.
    findings: List<Text>
    /// What the window's search found, each as `start:end`, and which of them the cursor is on.
    hits: List<Text>
    at_match: Int = 0
    truncated: Bool = false
    /// The query last searched, and how it was read.
    query: Text = ""
    kind: Text = "hex"
    match_case: Bool = false
    whole_word: Bool = false
    /// The strip open under the bar: `search`, `replace`, `goto` or `inspector`; empty for none.
    panel: Text = ""
    /// What the window last said: a result, or what went wrong.
    notice: Text = ""
    notice_kind: Text = "info"
}

/// One file's bytes: where they stand in the workspace's content store, and the edits made since
/// they were read. The edits are the document's own records, appended in order, so two people
/// changing different bytes both keep what they changed.
type Document (title: name, untitled: "Untitled.bin") {
    name: Text = ""
    /// How many times the bytes were read afresh.
    opened: Int merge counter
    /// The address of the bytes as they were read or last saved; empty for an empty buffer.
    base: Text = ""
    /// Where the file stands on this device; empty for one handed over or saved nowhere.
    path: Text = ""
    /// The length of the bytes with the edits applied.
    length: Int = 0
    /// How many bytes differ from what the file holds.
    edited: Int = 0
    /// The edits made since the base was read, in order: `at:remove:insert`, the bytes to
    /// splice as hexadecimal pairs.
    changes: List<Text>
    /// Each person's own place in the document.
    desks: Collection<Desk>
}

/// The workspace's Hex Editor: the buffers it has open.
type Library {
    documents: Collection<Document>
}

root library: Library

default view library for Library

default view document for Document

/// A document is opened by whoever opens it: they get their own place in it, and the window
/// reads the bytes it shows.
opened action arrive(d: Document) {
    if d.desks.is_empty() { d.desks.insert(Desk()) }
    show(d)
}

// ---------------------------------------------------------------- the device's answers
type OpenAsk {
    path: Text
}

type OpenedAnswer {
    name: Text = ""
    address: Text = ""
    length: Int = 0
}

type RowsAsk {
    address: Text
    changes: List<Text>
    first: Int = 0
    count: Int = 0
    columns: Int = 16
    endian: Text = "little"
    cursor: Int = 0
}

type RowsAnswer {
    length: Int = 0
    edited: Int = 0
    read: Bool = false
    error: Text = ""
    rows: List<Text>
    changed: List<Text>
    findings: List<Text>
}

type FindAsk {
    address: Text
    changes: List<Text>
    query: Text
    kind: Text = "hex"
    case: Bool = false
    word: Bool = false
    limit: Int = 10000
}

type FoundAnswer {
    hits: List<Text>
    truncated: Bool = false
}

type ApplyAsk {
    address: Text
    changes: List<Text>
    op: Text
    at: Int = 0
    len: Int = 0
    bytes: Text = ""
}

type AppliedAnswer {
    change: Text = ""
    length: Int = 0
}

type ReplaceAsk {
    address: Text
    changes: List<Text>
    spans: List<Text>
    bytes: Text = ""
    mode: Text = "overwrite"
}

type ReplacedAnswer {
    changes: List<Text>
    count: Int = 0
    length: Int = 0
}

type RangeAsk {
    address: Text
    changes: List<Text>
    start: Int = 0
    end: Int = 0
    kind: Text = "hex"
}

type RangeAnswer {
    text: Text = ""
}

type BytesAsk {
    address: Text
    changes: List<Text>
}

type BytesAnswer {
    bytes: Text = ""
}

type SaveAsk {
    address: Text
    changes: List<Text>
    path: Text
}

type SavedAnswer {
    address: Text = ""
    length: Int = 0
}

/// How many rows the bytes take at `columns` bytes a row.
// ---------------------------------------------------------------- bytes and numbers
fn rows_of(length: Int, columns: Int) -> Int {
    if columns <= 0 or length <= 0 { 0 } else { (length + columns - 1) / columns }
}

/// How many bytes a person reads a count as: `812 B`, `4.2 MB`.
fn size_text(n: Int) -> Text {
    if n < 1000 {
        "{n} B"
    } else if n < 1000000 {
        "{n / 1000} KB"
    } else if n < 1000000000 { "{n / 1000000} MB" } else { "{n / 1000000000} GB" }
}

fn byte_count(n: Int) -> Text {
    if n == 1 { "1 byte" } else { "{n} bytes" }
}

/// The value of one hexadecimal digit written as text.
fn digit_value(one: Text) -> Option<Int> {
    if one == "0" {
        .some(0)
    } else if one == "1" {
        .some(1)
    } else if one == "2" {
        .some(2)
    } else if one == "3" {
        .some(3)
    } else if one == "4" {
        .some(4)
    } else if one == "5" {
        .some(5)
    } else if one == "6" {
        .some(6)
    } else if one == "7" {
        .some(7)
    } else if one == "8" {
        .some(8)
    } else if one == "9" {
        .some(9)
    } else if one.lower() == "a" {
        .some(10)
    } else if one.lower() == "b" {
        .some(11)
    } else if one.lower() == "c" {
        .some(12)
    } else if one.lower() == "d" {
        .some(13)
    } else if one.lower() == "e" {
        .some(14)
    } else if one.lower() == "f" { .some(15) } else { .none }
}

/// The printable characters a key can type, in code order, beside the control codes that have
/// no glyph. The backslash stands at no place, so the table keeps two runs.
fn ascii_table() -> Text {
    " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[]^_`abcdefghijklmnopqrstuvwxyz\{|\}~"
}

/// The byte a typed character is, and nothing for one this app cannot type.
fn code_of(one: Text) -> Option<Int> {
    if one.len() != 1 {
        return .none
    }
    if let at = ascii_table().find(one) {
        if at < 60 { .some(32 + at) } else if at < 61 { .none } else { .some(33 + at) }
    } else { .none }
}

/// The digits of `text`, separators left out; nothing when a character is neither.
fn digits_of(text: Text, at: Int, kept: Text) -> Option<Text> {
    if at >= text.len() {
        return .some(kept)
    }
    let one = text.slice(at, at + 1)
    if digit_value(one) != .none {
        digits_of(text, at + 1, kept + one)
    } else if one == " " or one == "-" or one == ":" or one == "," {
        digits_of(text, at + 1, kept)
    } else { .none }
}

/// The bytes `text` writes as hexadecimal pairs, and nothing when it is no such text.
fn hex_bytes(text: Text) -> Option<List<Int>> {
    let trimmed = text.trim()
    let body = if trimmed.starts_with("0x") or trimmed.starts_with("0X") {
        trimmed.slice(2, trimmed.len())
    } else { trimmed }
    let kept = digits_of(body, 0, "")
    if let digits = kept {
        if digits.len() % 2 == 0 {
            bytes_of(digits, 0, digits.len() / 2, [])
        } else { .none }
    } else { .none }
}

fn bytes_of(kept: Text, at: Int, pairs: Int, out: List<Int>) -> Option<List<Int>> {
    if at >= pairs {
        return .some(out)
    }
    if let high = digit_value(kept.slice(at * 2, at * 2 + 1)) {
        if let low = digit_value(kept.slice(at * 2 + 1, at * 2 + 2)) {
            return bytes_of(kept, at + 1, pairs, out + [high * 16 + low])
        } else {
            return .none
        }
    } else { .none }
}

/// `bytes` written as hexadecimal pairs, upper case.
fn hex_text(bytes: List<Int>) -> Text {
    bytes.map(b -> "{digit(b / 16 % 16)}{digit(b % 16)}").join("")
}

fn digit(n: Int) -> Text {
    if n == 0 {
        "0"
    } else if n == 1 {
        "1"
    } else if n == 2 {
        "2"
    } else if n == 3 {
        "3"
    } else if n == 4 {
        "4"
    } else if n == 5 {
        "5"
    } else if n == 6 {
        "6"
    } else if n == 7 {
        "7"
    } else if n == 8 {
        "8"
    } else if n == 9 {
        "9"
    } else if n == 10 {
        "A"
    } else if n == 11 {
        "B"
    } else if n == 12 { "C" } else if n == 13 { "D" } else if n == 14 { "E" } else { "F" }
}

/// The byte `desk` shows at `offset`, read from the rows the window holds.
fn shown_byte(d: Document, desk: Desk, offset: Int) -> Option<Int> {
    if desk.columns <= 0 or offset < 0 or offset >= d.length {
        return .none
    }
    let row = offset / desk.columns - desk.first
    if row < 0 {
        return .none
    }
    if let r = desk.rows.at(row) {
        let at = offset % desk.columns
        if let xs = hex_bytes(r.slice(at * 2, at * 2 + 2)) { xs.first() } else { .none }
    } else { .none }
}

/// The offset `query` names: `0x1F` hex, bare hex digits with a letter, or decimal.
fn offset_of(query: Text) -> Option<Int> {
    let q = query.trim()
    if q.starts_with("0x") or q.starts_with("0X") {
        hex_number(q.slice(2, q.len()), 0, 0)
    } else if has_letter(q) and hex_number(q, 0, 0) != .none {
        hex_number(q, 0, 0)
    } else { q.to_int() }
}

fn hex_number(text: Text, at: Int, total: Int) -> Option<Int> {
    if at >= text.len() {
        return .some(total)
    }
    if let d = digit_value(text.slice(at, at + 1)) {
        hex_number(text, at + 1, total * 16 + d)
    } else { .none }
}

fn has_letter(text: Text) -> Bool {
    letter_at(text, 0)
}

fn letter_at(text: Text, at: Int) -> Bool {
    if at >= text.len() {
        return false
    }
    if (digit_value(text.slice(at, at + 1)) ?? 0) >= 10 {
        return true
    }
    letter_at(text, at + 1)
}

/// The cursor moved to `at`, kept inside the bytes; `extend` keeps where the selection started,
/// and the window's first row follows it when it leaves the rows shown.
action move_cursor(d: Document, desk: Desk, at: Int, extend: Bool) -> Bool {
    let limit = if desk.mode == "insert" {
        d.length
    } else if d.length > 0 { d.length - 1 } else { 0 }
    let target = if at < 0 { 0 } else if at > limit { limit } else { at }
    if extend and desk.anchor == .none {
        desk.anchor = .some(desk.cursor)
    } else if not extend {
        desk.anchor = .none
    }
    desk.cursor = target
    let row = target / desk.columns
    if row < desk.first {
        desk.first = row
        desk.high = .none
    } else if row >= desk.first + 16 {
        desk.first = row - 16
        desk.high = .none
    }
    desk.cursor < d.length or desk.cursor == 0
}

/// The selection as an inclusive span, when one is made.
fn selection(desk: Desk) -> Option<Span> {
    if let anchor = desk.anchor {
        let lo = if anchor < desk.cursor { anchor } else { desk.cursor }
        let hi = if anchor > desk.cursor { anchor } else { desk.cursor }
        chosen_span(lo, hi)
    } else { .none }
}

fn chosen_span(lo: Int, hi: Int) -> Option<Span> {
    .some(Span(start: lo, end: hi))
}

/// A span as the grid and the device write it: `"start:end"`.
fn span_text(start: Int, end: Int) -> Text {
    "{start}:{end}"
}

/// The start of the span `span` writes.
fn span_start(span: Text) -> Int {
    (span.split(":").at(0) ?? "0").to_int() ?? 0
}

/// The offset a change `at:remove:insert` stands at.
fn change_at(change: Text) -> Int {
    (change.split(":").at(0) ?? "0").to_int() ?? 0
}

/// Say something in the window: a result, or what went wrong.
action say(desk: Desk, words: Text, kind: Text) -> Bool {
    desk.notice = words
    desk.notice_kind = kind
    true
}

/// Read the window's rows and the bytes at its cursor from the device again.
// ---------------------------------------------------------------- reading and editing
action show(d: Document) {
    if let desk = d.desks.first() {
        let ask = RowsAsk(
            address: d.base,
            changes: d.changes,
            first: desk.first,
            count: 96,
            columns: desk.columns,
            endian: desk.endian,
            cursor: desk.cursor,
        )
        match engine.call("rows", json.encode(ask)) {
            .ok(answer) => match json.decode<RowsAnswer>(answer) {
                .ok(read) => {
                    d.length = read.length
                    d.edited = read.edited
                    desk.rows = read.rows
                    desk.changed = read.changed
                    desk.findings = read.findings
                    if not read.read { say(desk, read.error, "warning") }
                }
                .err(why) => { say(desk, why, "warning") }
            },
            .err(why) => { say(desk, why, "warning") }
        }
    }
}

/// Apply one edit of the bytes: the device checks it and answers the change to keep.
action edit(d: Document, op: Text, at: Int, len: Int, bytes: Text) -> Bool {
    let ask = ApplyAsk(address: d.base, changes: d.changes, op: op, at: at, len: len, bytes: bytes)
    match engine.call("apply", json.encode(ask)) {
        .ok(answer) => match json.decode<AppliedAnswer>(answer) {
            .ok(applied) => {
                d.changes = d.changes + [applied.change]
                d.length = applied.length
                true
            }
            .err(why) => {
                if let desk = d.desks.first() { say(desk, why, "warning") }
                false
            }
        },
        .err(why) => {
            if let desk = d.desks.first() { say(desk, why, "warning") }
            false
        }
    }
}

/// Opens the file at `path` on this device as its own buffer.
// ---------------------------------------------------------------- opening and saving
action open_path(lib: Library, path: Text) -> Document uses files {
    match engine.call("open", json.encode(OpenAsk(path: path))) {
        .ok(answer) => match json.decode<OpenedAnswer>(answer) {
            .ok(opened) => opened_buffer(lib, opened.name, opened.address, path, opened.length),
            .err(why) => refused_buffer(lib, path, why),
        },
        .err(why) => refused_buffer(lib, path, why),
    }
}

action refused_buffer(lib: Library, path: Text, why: Text) -> Document {
    let made = empty_buffer(lib, file_name(path))
    if let desk = made.desks.first() {
        say(desk, "Couldn’t open “{path}”: {why}", "warning")
    }
    made
}

/// Opens a file a person chose, or one another app handed over, as its own buffer. Its bytes
/// are the address the content store gave them, so every device reads the same file.
action open_file(lib: Library, file: File) -> Document (
    icon: .file_plus,
    label: "Open a file",
    did: "opened a file",
    opens: ["application/octet-stream", "application/x-binary", .bin, .dat, .dmp, .raw, .rom],
) {
    opened_buffer(lib, file.name, file.url, "", file.size)
}

/// The buffer of the bytes at `address`: the one already open on them, read afresh, or a new one.
action opened_buffer(lib: Library, name: Text, address: Text, path: Text, length: Int) -> Document {
    let held = lib.documents.find(d -> d.base == address and address != "")
    if let found = held {
        found.opened += 1
        found.changes = []
        found.path = path
        found.length = length
        found.edited = 0
        if found.name != name {
            found.name = name
        }
        if let desk = found.desks.first() {
            desk.rows = []
            desk.findings = []
            desk.hits = []
            desk.at_match = 0
            desk.first = 0
            desk.cursor = 0
            desk.anchor = .none
            desk.high = .none
        }
        show(found)
        found
    } else {
        let made = lib.documents.insert(
            Document(name: name, opened: 1, base: address, path: path, length: length),
        )
        made.desks.insert(Desk())
        show(made)
        made
    }
}

/// An empty buffer to type bytes into, saved wherever the person says.
action new_buffer(lib: Library) -> Document (
    icon: .plus,
    label: "New empty file",
    key: "Ctrl+N",
    did: "started an empty buffer",
) {
    let made = empty_buffer(lib, "Untitled.bin")
    show(made)
    made
}

action empty_buffer(lib: Library, name: Text) -> Document {
    let made = lib.documents.insert(Document(name: name, opened: 1, base: "", path: "", length: 0))
    made.desks.insert(Desk())
    made
}

/// The action the person's file chooser fills.
action open_chosen(lib: Library, file: File) -> Document (icon: .file_plus, label: "Open a file") {
    open_file(lib, file)
}

/// Saves the buffer where it came from, or asks where when it stands nowhere yet.
action save(d: Document) uses files (
    icon: .save,
    label: "Save",
    key: "Ctrl+S",
    did: "saved the file",
) {
    require d.path != "" "This file stands nowhere on this device yet: use Save as"
    if true {
        let ask = SaveAsk(address: d.base, changes: d.changes, path: d.path)
        match engine.call("save", json.encode(ask)) {
            .ok(answer) => match json.decode<SavedAnswer>(answer) {
                .ok(saved) => {
                    rebased(d, saved.address, saved.length)
                    if let desk = d.desks.first() {
                        say(desk, "Saved {size_text(saved.length)}", "success")
                    }
                }
                .err(why) => {
                    if let desk = d.desks.first() {
                        say(desk, "Couldn’t save the file: {why}", "warning")
                    }
                }
            },
            .err(why) => {
                if let desk = d.desks.first() {
                    say(desk, "Couldn’t save the file: {why}", "warning")
                }
            }
        }
    }
}

/// The bytes as a file the person places with the system's save chooser, which the window
/// shows.
action save_bytes(d: Document) -> File {
    let ask = BytesAsk(address: d.base, changes: d.changes)
    match engine.call("bytes", json.encode(ask)) {
        .ok(answer) => match json.decode<BytesAnswer>(answer) {
            .ok(read) => File.of_bytes(d.name, Bytes.from_base64(read.bytes)),
            .err(_) => File.of_bytes(d.name, Bytes.from_base64("")),
        },
        .err(_) => File.of_bytes(d.name, Bytes.from_base64("")),
    }
}

/// What was saved is the base: no edit stands against it any more.
action rebased(d: Document, address: Text, length: Int) {
    d.base = address
    d.changes = []
    d.length = length
    d.edited = 0
    show(d)
}

/// Forgets a buffer, its edits with it.
destructive action forget(lib: Library, d: Document) (icon: .x, label: "Close the file") {
    lib.documents.remove(d)
}

/// The name `path` ends in.
fn file_name(path: Text) -> Text {
    let last = path.split("/").last() ?? path
    if last == "" { "Untitled.bin" } else { last }
}

/// The keys pressed on the grid, as the person's keyboard names them.
// ---------------------------------------------------------------- the window's keys
action press(d: Document, key: Text) (icon: .keyboard, label: "Press a key") {
    if let desk = d.desks.first() {
        let shifted = key.starts_with("Shift+")
        let bare = if shifted { key.slice(6, key.len()) } else { key }
        if key.starts_with("Ctrl+") {
            chord(d, desk, key.slice(5, key.len()))
        } else if bare == "Tab" {
            desk.column = if desk.column == "hex" { "ascii" } else { "hex" }
            show(d)
        } else if bare == "Insert" {
            desk.mode = if desk.mode == "insert" { "overwrite" } else { "insert" }
            show(d)
        } else if bare == "Escape" {
            if desk.panel != "" {
                desk.panel = ""
            } else {
                desk.anchor = .none
            }
            show(d)
        } else if bare == "ArrowLeft" {
            moved(d, desk, desk.cursor - 1, shifted)
        } else if bare == "ArrowRight" {
            moved(d, desk, desk.cursor + 1, shifted)
        } else if bare == "ArrowUp" {
            moved(d, desk, desk.cursor - desk.columns, shifted)
        } else if bare == "ArrowDown" {
            moved(d, desk, desk.cursor + desk.columns, shifted)
        } else if bare == "Home" {
            moved(d, desk, desk.cursor / desk.columns * desk.columns, shifted)
        } else if bare == "End" {
            moved(d, desk, desk.cursor / desk.columns * desk.columns + desk.columns - 1, shifted)
        } else if bare == "PageUp" {
            moved(d, desk, desk.cursor - desk.columns * 16, shifted)
        } else if bare == "PageDown" {
            moved(d, desk, desk.cursor + desk.columns * 16, shifted)
        } else if bare == "Backspace" {
            backspace(d, desk)
        } else if bare == "Delete" { delete_at_cursor(d, desk) } else { type_key(d, desk, bare) }
    }
}

/// The Ctrl chords typed while the bytes have the keyboard.
action chord(d: Document, desk: Desk, key: Text) {
    let lower = key.lower()
    if lower == "a" {
        select_all(d, desk)
    } else if lower == "c" {
        copy(d)
    } else if lower == "x" {
        cut(d)
    } else if lower == "v" {
        paste(d)
    } else if lower == "f" {
        desk.panel = "search"
    } else if lower == "h" {
        desk.panel = "replace"
    } else if lower == "g" {
        desk.panel = "goto"
    } else if lower == "i" {
        desk.panel = if desk.panel == "inspector" { "" } else { "inspector" }
    }
}

/// The cursor moved to `at`: the rows and the inspector read there.
action moved(d: Document, desk: Desk, at: Int, extend: Bool) {
    move_cursor(d, desk, at, extend)
    desk.high = .none
    show(d)
}

action type_key(d: Document, desk: Desk, one: Text) {
    if one.len() != 1 {
        return
    }
    if desk.column == "hex" {
        if let value = digit_value(one) {
            type_hex(d, desk, value)
        } else if one.lower() == "e" {
            desk.endian = if desk.endian == "big" { "little" } else { "big" }
            show(d)
        }
    } else { type_ascii(d, desk, one) }
}

/// Type one hexadecimal digit: in overwrite mode the first sets the high nibble and the second
/// completes the byte; in insert mode each completed byte is spliced in.
action type_hex(d: Document, desk: Desk, value: Int) {
    let current = shown_byte(d, desk, desk.cursor) ?? 0
    if desk.mode == "overwrite" and desk.cursor < d.length {
        if desk.high == .none {
            let byte = value * 16 + current % 16
            if edit(d, "overwrite", desk.cursor, 1, hex_text([byte])) {
                desk.high = .some(value)
                show(d)
            }
        } else {
            let byte = current / 16 * 16 + value
            if edit(d, "overwrite", desk.cursor, 1, hex_text([byte])) {
                desk.high = .none
                if move_cursor(d, desk, desk.cursor + 1, false) { show(d) }
            }
        }
    } else if desk.high == .none {
        let byte = value * 16
        if edit(d, "insert", desk.cursor, 0, hex_text([byte])) {
            desk.high = .some(value)
            show(d)
        }
    } else {
        let byte = current / 16 * 16 + value
        if edit(d, "overwrite", desk.cursor, 1, hex_text([byte])) {
            desk.high = .none
            if move_cursor(d, desk, desk.cursor + 1, false) { show(d) }
        }
    }
}

/// Type a character into the ASCII column, then advance.
action type_ascii(d: Document, desk: Desk, one: Text) {
    if let byte = code_of(one) {
        if desk.mode == "overwrite" and desk.cursor < d.length {
            if edit(d, "overwrite", desk.cursor, 1, hex_text([byte])) {
                if move_cursor(d, desk, desk.cursor + 1, false) { show(d) }
            }
        } else if edit(d, "insert", desk.cursor, 0, hex_text([byte])) {
            if move_cursor(d, desk, desk.cursor + 1, false) { show(d) }
        }
    }
}

/// In insert mode the byte before the cursor goes; in overwrite mode the cursor only steps back.
action backspace(d: Document, desk: Desk) {
    if desk.mode == "insert" and desk.cursor > 0 {
        if edit(d, "delete", desk.cursor - 1, 1, "") {
            if move_cursor(d, desk, desk.cursor - 1, false) { show(d) }
        }
    } else if move_cursor(d, desk, desk.cursor - 1, false) { show(d) }
}

/// The selection goes, or else the byte under the cursor.
action delete_at_cursor(d: Document, desk: Desk) {
    if let span = selection(desk) {
        let len = span.end - span.start + 1
        if edit(d, "delete", span.start, len, "") {
            desk.anchor = .none
            if move_cursor(d, desk, span.start, false) { show(d) }
        }
    } else if desk.cursor < d.length {
        if edit(d, "delete", desk.cursor, 1, "") {
            if move_cursor(d, desk, desk.cursor, false) { show(d) }
        }
    }
}

action select_all(d: Document, desk: Desk) {
    if d.length == 0 {
        return
    }
    desk.anchor = .some(0)
    desk.cursor = d.length - 1
    show(d)
}

/// Copies the selected bytes in the column's form: `50 4e 47 44`, or `PNGD`.
// ---------------------------------------------------------------- the clipboard
action copy(d: Document) uses clipboard (icon: .copy, label: "Copy", key: "Ctrl+C") {
    if let desk = d.desks.first() {
        if let span = selection(desk) {
            let kind = if desk.column == "hex" { "hex" } else { "text" }
            let ask = RangeAsk(
                address: d.base,
                changes: d.changes,
                start: span.start,
                end: span.end + 1,
                kind: kind,
            )
            match engine.call("range", json.encode(ask)) {
                .ok(answer) => match json.decode<RangeAnswer>(answer) {
                    .ok(read) => {
                        clipboard.copy(read.text)
                        say(desk, "Copied {byte_count(span.end - span.start + 1)}", "success")
                    }
                    .err(why) => { say(desk, why, "warning") }
                },
                .err(why) => { say(desk, why, "warning") }
            }
        } else {
            say(desk, "Nothing selected: hold Shift while moving to select bytes", "info")
        }
    }
}

/// Copies the selection and deletes it.
action cut(d: Document) uses clipboard (icon: .scissors, label: "Cut", key: "Ctrl+X") {
    copy(d)
    if let desk = d.desks.first() {
        if let span = selection(desk) {
            let count = span.end - span.start + 1
            if edit(d, "delete", span.start, count, "") {
                desk.anchor = .none
                if move_cursor(d, desk, span.start, false) { show(d) }
                say(desk, "Cut {byte_count(count)}", "success")
            }
        }
    }
}

/// Pastes the clipboard at the selection, or at the cursor, in the focus mode.
action paste(d: Document) uses clipboard (icon: .clipboard_paste, label: "Paste", key: "Ctrl+V") {
    if let desk = d.desks.first() {
        if let text = clipboard.paste() {
            let bytes = if desk.column == "hex" { hex_bytes(text) } else { text_bytes(text) }
            if let xs = bytes {
                if xs.is_empty() {
                    say(desk, "The clipboard holds no bytes", "info")
                } else if let span = selection(desk) {
                    replaced(d, desk, [span_text(span.start, span.end + 1)], hex_text(xs))
                } else if desk.mode == "insert" {
                    if edit(d, "insert", desk.cursor, 0, hex_text(xs)) {
                        if move_cursor(d, desk, desk.cursor + xs.len(), false) { show(d) }
                        say(desk, "Pasted {byte_count(xs.len())}", "success")
                    }
                } else if desk.cursor + xs.len() > d.length {
                    say(
                        desk,
                        "What you pasted is longer than the file has left. Switch to Insert to make the file longer.",
                        "warning",
                    )
                } else {
                    replaced(
                        d,
                        desk,
                        [span_text(desk.cursor, desk.cursor + xs.len())],
                        hex_text(xs),
                    )
                }
            } else {
                say(desk, "Couldn’t paste here as bytes", "warning")
            }
        } else {
            say(desk, "The clipboard holds no text", "info")
        }
    }
}

/// The bytes `text` writes, a character each; nothing when one has no printable code.
fn text_bytes(text: Text) -> Option<List<Int>> {
    if text.len() == 0 {
        return .none
    }
    chars_bytes(text, 0, [])
}

fn chars_bytes(text: Text, at: Int, out: List<Int>) -> Option<List<Int>> {
    if at >= text.len() {
        return .some(out)
    }
    if let byte = code_of(text.slice(at, at + 1)) {
        chars_bytes(text, at + 1, out + [byte])
    } else { .none }
}

/// Replaces each span with the same bytes, as one step.
action replaced(d: Document, desk: Desk, spans: List<Text>, bytes: Text) {
    let ask = ReplaceAsk(
        address: d.base,
        changes: d.changes,
        spans: spans,
        bytes: bytes,
        mode: desk.mode,
    )
    match engine.call("replace", json.encode(ask)) {
        .ok(answer) => match json.decode<ReplacedAnswer>(answer) {
            .ok(done) => {
                if done.count == 0 {
                    say(desk, "What you pasted is already there", "info")
                } else {
                    for change in done.changes {
                        d.changes = d.changes + [change]
                    }
                    d.length = done.length
                    desk.anchor = .none
                    if let first = done.changes.last() {
                        desk.cursor = change_at(first)
                    }
                    show(d)
                    say(desk, "Pasted {byte_count(bytes.len() / 2)}", "success")
                }
            }
            .err(why) => { say(desk, why, "warning") }
        },
        .err(why) => { say(desk, why, "warning") }
    }
}

/// Opens the search strip.
// ---------------------------------------------------------------- finding bytes
action start_search(d: Document) (icon: .search, label: "Find bytes", key: "Ctrl+F") {
    if let desk = d.desks.first() {
        desk.panel = "search"
    }
}

/// Opens the replace strip.
action start_replace(d: Document) (icon: .arrow_up_down, label: "Find and replace", key: "Ctrl+H") {
    if let desk = d.desks.first() {
        desk.panel = "replace"
    }
}

/// Opens the go-to-offset strip.
action start_goto(d: Document) (icon: .arrow_right, label: "Go to offset", key: "Ctrl+G") {
    if let desk = d.desks.first() {
        desk.panel = "goto"
    }
}

/// Shows or hides the inspector.
action toggle_inspector(d: Document) (
    icon: .panel_right,
    label: "Show or hide the inspector",
    key: "Ctrl+I",
) {
    if let desk = d.desks.first() {
        desk.panel = if desk.panel == "inspector" { "" } else { "inspector" }
    }
}

/// Closes the strip under the bar.
action close_panel(d: Document) {
    if let desk = d.desks.first() {
        desk.panel = ""
    }
}

/// Finds `query` in the bytes: a hexadecimal pattern, or text under its rules.
action search(d: Document, query: Text) (icon: .search, label: "Search the bytes") {
    if let desk = d.desks.first() {
        let ask = FindAsk(
            address: d.base,
            changes: d.changes,
            query: query,
            kind: desk.kind,
            case: desk.match_case,
            word: desk.whole_word,
            limit: 10000,
        )
        match engine.call("find", json.encode(ask)) {
            .ok(answer) => match json.decode<FoundAnswer>(answer) {
                .ok(found) => {
                    desk.query = query
                    desk.hits = found.hits
                    desk.truncated = found.truncated
                    desk.at_match = 0
                    if found.hits.len() == 0 {
                        say(desk, "Nothing was found", "info")
                    } else { reveal(d, 0) }
                }
                .err(why) => { say(desk, why, "warning") }
            },
            .err(why) => { say(desk, why, "warning") }
        }
    }
}

/// Puts the cursor on the match at `index`.
action reveal(d: Document, index: Int) {
    if let desk = d.desks.first() {
        if let hit = desk.hits.at(index) {
            desk.at_match = index
            desk.anchor = .none
            move_cursor(d, desk, span_start(hit), false)
            show(d)
        }
    }
}

/// Replaces every match the search found with `with`.
action replace_all(d: Document, with: Text) {
    if let desk = d.desks.first() {
        if desk.hits.len() == 0 {
            say(desk, "Search for something first", "info")
        } else { replace_spans(d, desk, desk.hits, with) }
    }
}

action replace_spans(d: Document, desk: Desk, spans: List<Text>, with: Text) {
    let ask = ReplaceAsk(
        address: d.base,
        changes: d.changes,
        spans: spans,
        bytes: with,
        mode: desk.mode,
    )
    match engine.call("replace", json.encode(ask)) {
        .ok(answer) => match json.decode<ReplacedAnswer>(answer) {
            .ok(done) => {
                if done.count == 0 {
                    say(desk, "Nothing was replaced", "info")
                } else {
                    for change in done.changes {
                        d.changes = d.changes + [change]
                    }
                    d.length = done.length
                    desk.anchor = .none
                    show(d)
                    say(desk, "Replaced {done.count} matches", "success")
                }
            }
            .err(why) => { say(desk, why, "warning") }
        },
        .err(why) => { say(desk, why, "warning") }
    }
}

/// Reads the search strip's choices: text or hexadecimal, letter case and whole words.
action search_kind(d: Document, i: Int) {
    if let desk = d.desks.first() {
        desk.kind = if i == 1 { "text" } else { "hex" }
    }
}

action search_case(d: Document, on: Bool) {
    if let desk = d.desks.first() {
        desk.match_case = on
    }
}

action search_word(d: Document, on: Bool) {
    if let desk = d.desks.first() {
        desk.whole_word = on
    }
}

/// Moves the cursor to the offset `query` names.
action go_to(d: Document, query: Text) {
    if let desk = d.desks.first() {
        if let at = offset_of(query) {
            move_cursor(d, desk, at, false)
            desk.panel = ""
            show(d)
        } else {
            say(desk, "Write the offset as `0x1F` or a number", "info")
        }
    }
}

action set_endian(d: Document, i: Int) {
    if let desk = d.desks.first() {
        desk.endian = if i == 1 { "big" } else { "little" }
        show(d)
    }
}

action set_mode(d: Document, i: Int) {
    if let desk = d.desks.first() {
        desk.mode = if i == 1 { "insert" } else { "overwrite" }
        desk.high = .none
    }
}

action set_column(d: Document, i: Int) {
    if let desk = d.desks.first() {
        desk.column = if i == 1 { "ascii" } else { "hex" }
    }
}

action dismiss(d: Document) {
    if let desk = d.desks.first() {
        desk.notice = ""
    }
}

/// A press on the grid: the byte at `at` gets the cursor, the column pressed the focus, and
/// `"shift"` keeps where the selection started.
action pressed(d: Document, at: List<Text>) {
    if let desk = d.desks.first() {
        let offset = (at.at(0) ?? "0").to_int() ?? 0
        let extend = (at.at(2) ?? "") == "shift"
        desk.column = at.at(1) ?? "hex"
        move_cursor(d, desk, offset, extend)
        desk.high = .none
        show(d)
    }
}

/// The wheel or the bar moved the window by `rows`.
action scrolled(d: Document, rows: Int) {
    if let desk = d.desks.first() {
        let most = if d.length == 0 { 0 } else { rows_of(d.length, desk.columns) - 24 }
        let wanted = desk.first + rows
        desk.first = if wanted < 0 { 0 } else if wanted > most { most } else { wanted }
        if desk.first < 0 {
            desk.first = 0
        }
        show(d)
    }
}

/// Inserts `bytes` (hexadecimal, like `de ad be ef`) at `offset`.
// ---------------------------------------------------------------- the published verbs
action insert_bytes(d: Document, offset: Int, bytes: Text) (icon: .plus, label: "Insert bytes") {
    if edit(d, "insert", offset, 0, bytes) { show(d) }
}

/// Replaces the bytes at `offset` with `bytes` (hexadecimal), never growing the file.
action overwrite_bytes(d: Document, offset: Int, bytes: Text) (
    icon: .pencil,
    label: "Replace bytes",
) {
    if edit(d, "overwrite", offset, 0, bytes) { show(d) }
}

/// Removes `len` bytes at `offset`.
destructive action delete_bytes(d: Document, offset: Int, len: Int) (
    icon: .trash,
    label: "Delete bytes",
) {
    if edit(d, "delete", offset, len, "") { show(d) }
}

// ---------------------------------------------------------------- the views
view library(lib: Library) {
    ui.NavShell(content_kind: .list, title: "Hex Editor") {
        actions: {
            row(gap: .related, align: .center) {
                ui.Button("Open file", icon: .file_plus, role: .primary) {
                    on press: file.choose(open_chosen(lib))
                }
                ui.Button("New empty file", icon: .plus) {
                    on press: new_buffer(lib)
                }
            }
        }
        content: {
            if lib.documents.is_empty() {
                ui.EmptyState(
                    "No file open",
                    icon: .file_code,
                    message: "Open a file to read its bytes, or start an empty one and type bytes in.",
                    action: "Open file",
                ) {
                    on act: file.choose(open_chosen(lib))
                }
            } else {
                ui.VirtualList(name: "Open files", in_column: true) {
                    on activate(d: Document): open(d)
                    for doc in lib.documents {
                        ui.ListItem(item: doc) {
                            menu: {
                                ui.MenuItem("Close", icon: .x) {
                                    on choose: forget(lib, doc)
                                }
                            }
                            row: {
                                ui.ListRow(
                                    title: doc.name,
                                    secondary: if doc.edited > 0 {
                                        "{size_text(doc.length)} · {doc.edited} changed"
                                    } else { size_text(doc.length) },
                                    icon: .file_code,
                                    value: if doc.path == "" { "Not on disk" } else { "Saved" },
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One buffer's window: the grid, the inspector beside it and what the window says under them.
view document(d: Document) {
    state query: Text = ""
    state with: Text = ""
    if let desk = d.desks.first() {
        ui.NavShell(content_kind: .page, title: d.name) {
            actions: {
                toolbar(d, desk)
            }
            content: {
                stack(gap: .related, fill: 1.0) {
                    if desk.notice != "" {
                        ui.Banner(
                            desk.notice,
                            kind: if desk.notice_kind == "warning" { .warning } else { .neutral },
                            icon: if desk.notice_kind == "warning" {
                                .triangle_alert
                            } else { .info },
                            action: "Dismiss",
                        ) {
                            on act: dismiss(d)
                        }
                    }
                    if desk.panel == "search" or desk.panel == "replace" {
                        search_row(d, desk, query, with)
                    }
                    if desk.panel == "goto" {
                        goto_row(d, desk, query)
                    }
                    ui.HexGrid(
                        rows: desk.rows,
                        changed: desk.changed,
                        total: rows_of(d.length, desk.columns),
                        first: desk.first,
                        columns: desk.columns,
                        cursor: .some(desk.cursor),
                        anchor: desk.anchor,
                        column: if desk.column == "hex" { .hex } else { .ascii },
                        matches: desk.hits,
                        at_match: .some(desk.at_match),
                        label: "{d.name}, {byte_count(d.length)}",
                        fill,
                    ) {
                        on press(at: List<Text>): pressed(d, at)
                        on key(k: Text): press(d, k)
                        on wheel(rows: Int): scrolled(d, rows)
                    }
                }
            }
            inspector: {
                if desk.panel == "inspector" {
                    inspector(d, desk)
                }
            }
            band: {
                status(d, desk)
            }
        }
    } else {
        ui.EmptyState("Reading the file…", icon: .file_code)
    }
}

/// The window's own controls: saving, the edit mode, the strips and the inspector.
view toolbar(d: Document, desk: Desk) {
    row(gap: .related, align: .center) {
        ui.Button("Save", icon: .save, role: .primary) {
            on press: save(d)
        }
        ui.Button("Save as", icon: .download) {
            on press: file.save(save_bytes(d), name: d.name)
        }
        ui.ToggleGroup(selected: if desk.mode == "insert" { 1 } else { 0 }, caption: "Edit mode") {
            ui.Segment("Overwrite")
            ui.Segment("Insert")
            on change(i): set_mode(d, i)
        }
        ui.ToggleGroup(selected: if desk.column == "ascii" { 1 } else { 0 }, caption: "Keys type") {
            ui.Segment("Hex")
            ui.Segment("Text")
            on change(i): set_column(d, i)
        }
        ui.MenuButton("Edit", icon: .pencil) {
            ui.MenuItem("Find bytes", icon: .search, key: "Ctrl+F") {
                on choose: start_search(d)
            }
            ui.MenuItem("Find and replace", icon: .arrow_up_down, key: "Ctrl+H") {
                on choose: start_replace(d)
            }
            ui.MenuItem("Go to offset", icon: .arrow_right, key: "Ctrl+G") {
                on choose: start_goto(d)
            }
            ui.MenuSeparator()
            ui.MenuItem("Copy", icon: .copy, key: "Ctrl+C", disabled: desk.anchor == .none) {
                on choose: copy(d)
            }
            ui.MenuItem("Cut", icon: .scissors, key: "Ctrl+X", disabled: desk.anchor == .none) {
                on choose: cut(d)
            }
            ui.MenuItem("Paste", icon: .clipboard_paste, key: "Ctrl+V") {
                on choose: paste(d)
            }
            ui.MenuItem("Select all", icon: .list, key: "Ctrl+A") {
                on choose: select_all(d, desk)
            }
            ui.MenuSeparator()
            ui.MenuItem("Inspector", icon: .panel_right, key: "Ctrl+I") {
                on choose: toggle_inspector(d)
            }
        }
    }
}

/// The search strip: the pattern, how it reads, and what was found.
view search_row(d: Document, desk: Desk, query: State<Text>, with: State<Text>) {
    stack(gap: .tight) {
        row(gap: .related, align: .center) {
            ui.TextInput(
                bind: query,
                placeholder: if desk.kind == "hex" {
                    "de ad be ef"
                } else { "Words in the bytes" },
                entry: true,
                autofocus: true,
                fill,
            ) {
                on submit(q): search(d, q)
                on cancel: close_panel(d)
            }
            ui.ToggleGroup(selected: if desk.kind == "text" { 1 } else { 0 }, caption: "Read as") {
                ui.Segment("Hex")
                ui.Segment("Text")
                on change(i): search_kind(d, i)
            }
            if desk.kind == "text" {
                ui.Toggle(shown: desk.match_case, label: "Match case") {
                    on check(on): search_case(d, on)
                }
                ui.Toggle(shown: desk.whole_word, label: "Whole word") {
                    on check(on): search_word(d, on)
                }
            }
            ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
                on press: close_panel(d)
            }
        }
        if desk.panel == "replace" {
            row(gap: .related, align: .center) {
                ui.TextInput(bind: with, placeholder: "What to put there", entry: true, fill) {
                    on submit(w): replace_all(d, w)
                }
                ui.Button("Replace all", icon: .arrow_up_down) {
                    on press: replace_all(d, with)
                }
            }
        }
        if desk.hits.len() > 0 {
            ui.Button(
                if desk.truncated {
                    "First {desk.hits.len()} matches"
                } else { "{desk.hits.len()} matches" },
                icon: .arrow_right,
                role: .plain,
            ) {
                on press: reveal(d, desk.at_match)
            }
        }
    }
}

/// The go-to strip: an offset as hexadecimal or a number.
view goto_row(d: Document, desk: Desk, query: State<Text>) {
    row(gap: .related, align: .center) {
        ui.TextInput(bind: query, placeholder: "0x1F or 31", entry: true, autofocus: true, fill) {
            on submit(q): go_to(d, q)
            on cancel: close_panel(d)
        }
        ui.Button("Close", icon: .x, icon_only: true, role: .plain) {
            on press: close_panel(d)
        }
    }
}

/// The bytes at the cursor as the numbers they can be, in the order the person reads them.
view inspector(d: Document, desk: Desk) {
    stack(gap: .related) {
        text("Inspector", role: .heading)
        ui.ToggleGroup(selected: if desk.endian == "big" { 1 } else { 0 }, caption: "Byte order") {
            ui.Segment("Little")
            ui.Segment("Big")
            on change(i): set_endian(d, i)
        }
        ui.PropertyList {
            for finding in desk.findings {
                ui.Property(label_of(finding), value_of(finding), copyable: true)
            }
        }
    }
}

/// What the window says about the bytes and where the cursor is.
view status(d: Document, desk: Desk) {
    let chosen = if let span = selection(desk) { span.end - span.start + 1 } else { 0 }
    row(gap: .related, align: .center) {
        text("Offset 0x{cursor_hex(desk.cursor)}", role: .caption, secondary: true)
        if chosen > 0 {
            text("{byte_count(chosen)} selected", role: .caption, secondary: true)
        }
        if d.edited > 0 {
            text("{byte_count(d.edited)} edited", role: .caption, secondary: true)
        }
        text(byte_count(d.length), role: .caption, secondary: true)
        if d.path != "" {
            text(d.path, role: .caption, secondary: true)
        }
    }
}

/// The label of a finding, written "label<TAB>value".
fn label_of(finding: Text) -> Text {
    finding.split("\t").at(0) ?? finding
}

/// The value of a finding, written "label<TAB>value".
fn value_of(finding: Text) -> Text {
    finding.split("\t").at(1) ?? ""
}

fn cursor_hex(cursor: Int) -> Text {
    hex_text([cursor / 16777216 % 256, cursor / 65536 % 256, cursor / 256 % 256, cursor % 256])
}
