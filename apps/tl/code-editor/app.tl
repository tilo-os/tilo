app so.tilo.code-editor "Code Editor" (
    icon: .file_code,
    category: .development,
    tagline: "Reads and changes a project's code, with its language's colours",
    keywords: ["code editor", "code", "editor", "source", "ide", "programming editor", "code editor online", "notepad", "text editor", "quelltext", "code-editor", "editor", "programmieren", "entwickeln"],
    document: project,
)

/// What an entry of the project is: code that is written, or a place that holds entries.
enum EntryKind {
    file
    folder
}

/// How much a problem matters, as the editor shows it.
enum Severity {
    error
    warning
    info
    hint
}

/// What a line of a command's output is: a problem a tool found, a place it named, a test, or
/// a change.
enum ItemKind {
    error
    warning
    location
    test_passed
    test_failed
    test_ignored
    change
}

/// One file or folder of the project: its name, where it stands, the language it is written
/// in, and — a file's — its text.
///
/// The text is the entry's own field, so two people writing in one file splice each other's
/// words rather than replacing the file; a value is the file's whole text, and the store keeps
/// the change as the difference.
///
/// A file the project was opened from disk keeps where it stands on this device (`disk`) and
/// the stamp of the bytes it last agreed with, which is what a save checks before it writes;
/// a file made in the project alone has neither until it is saved somewhere.
type Entry (untitled: "untitled") {
    kind: EntryKind = .file
    name: Text
    /// The folder it stands in; none at the top of the project.
    parent: Option<Entry>
    /// The language it is coloured, checked and kept as, by the language's own name (`Rust`,
    /// `Plain Text`); empty for a folder.
    language: Text = ""
    /// The file's text. A folder holds none.
    text: Text
    /// Where the file stands on this device; empty for a file saved nowhere yet.
    disk: Text = ""
    /// The modified time of the bytes this project last read or wrote, in nanoseconds; 0 when
    /// the file is nowhere on this device.
    stamp: Int = 0
    /// The length of the bytes the project last read or wrote.
    stamp_len: Int = 0
    /// The text as it stands on the device, so a reader sees what changed here and what is
    /// written already.
    saved: Text
    /// The file changed on the device since it was read here: saving over it asks first.
    stale: Bool = false
    /// What the last check of this file found.
    problems: List<Problem>
    /// What that check read: a file whose text no longer hashes to this has changed since.
    checked: Text = ""
    /// The checker that answered: the server's name, or `lint` while only the editor's own
    /// checks have run; empty before the file was checked.
    source: Text = ""
    /// The line and column a person asked to see, counted from 0, and how often they asked:
    /// the editor shows that place, and asks the same place again when the count changes.
    reveal_line: Int = 0
    reveal_column: Int = 0
    reveal_ask: Int merge counter = 0
}

/// One problem a checker found in a file: where it stands and what it says.
type Problem {
    severity: Severity = .error
    /// The line, counted from 0.
    line: Int = 0
    /// The column, counted from 0.
    column: Int = 0
    message: Text
}

/// One place in a file the words a person looked for stand: its line, counted from 0, the
/// column, and the words of the line there.
type Found {
    line: Int = 0
    column: Int = 0
    words: Text
}

/// One line of a command's output that names something a person can go to.
type Item {
    kind: ItemKind = .location
    text: Text
    /// The file it names, when it names one.
    path: Text = ""
    line: Int = 0
    column: Int = 0
}

/// One command run in the project.
type Command (title: command) {
    command: Text
    /// The folder it ran in.
    cwd: Text
    started: Time = Time("1970-01-01T00:00:00Z")
    ended: Time = Time("1970-01-01T00:00:00Z")
    /// What the command answered: 0 while it runs, its exit status once it is done.
    exit: Int = 0
    /// Its output, both streams as one, as the terminal shows it.
    output: Text
    /// The output grew past what a run keeps: what stands here is the start of it.
    truncated: Bool = false
    /// The engine's job while it runs; empty once it has stopped.
    job: Text = ""
    /// `running`, `done`, `stopped` or `failed`.
    state: Text = ""
    /// The lines of its output that name a problem, a place or a test.
    items: List<Item>
}

/// One place a project search found the words it looked for.
type Hit {
    /// The file's path on this device.
    path: Text
    /// Its path in the project, as the results show it.
    rel: Text
    /// The line, counted from 0.
    line: Int = 0
    column: Int = 0
    len: Int = 0
    /// The line's words, trimmed to the place the match stands.
    preview: Text
}

/// One suggestion a language server offers at the caret.
type Completion {
    label: Text
    /// What the suggestion stands for, as the server says it (`fn(u64) -> Vec<u64>`).
    detail: Text = ""
    /// What is written where it is accepted.
    insert: Text = ""
}

/// One place a declaration stands: what go-to-definition answers.
type Location {
    path: Text = ""
    line: Int = 0
    column: Int = 0
}

/// The device's answers, as this app reads them.
type Loaded {
    text: Text
    mtime: Int = 0
    len: Int = 0
}

type Stamped {
    mtime: Int = 0
    len: Int = 0
}

type Exists {
    exists: Bool = false
    mtime: Int = 0
    len: Int = 0
}

type DiskAsk {
    path: Text
}

type DiskSave {
    path: Text
    text: Text
    mtime: Int = 0
    len: Int = 0
    force: Bool = false
}

type DiskEntry {
    name: Text
    path: Text
    folder: Bool = false
    hidden: Bool = false
}

type Listing {
    entries: List<DiskEntry>
}

type SearchAsk {
    root: Text
    needle: Text
    case_sensitive: Bool = false
    whole_word: Bool = false
}

type RunAsk {
    command: Text
    cwd: Text
}

type SearchState {
    state: Text = ""
    hits: List<Hit>
    files: Int = 0
    truncated: Bool = false
}

type RunState {
    state: Text = ""
    exit: Int = 0
    output: Text
    truncated: Bool = false
    failure: Text = ""
}

type ServerAsk {
    language: Text
    root: Text
}

type DocumentAsk {
    job: Text
    path: Text
    language: Text = ""
    text: Text = ""
    version: Int = 0
}

type AskAsk {
    job: Text
    path: Text
    line: Int = 0
    column: Int = 0
    what: Text
}

type Progress {
    title: Text = ""
    detail: Text = ""
    percent: Int = 0
}

type Diag {
    severity: Text = "error"
    line: Int = 0
    column: Int = 0
    message: Text
}

type CompletionAnswer {
    what: Text = ""
    items: List<Completion>
}

type HoverAnswer {
    what: Text = ""
    text: Text = ""
}

type DefinitionAnswer {
    what: Text = ""
    locations: List<Location>
}

type FormatEdit {
    start: Int = 0
    start_column: Int = 0
    end: Int = 0
    end_column: Int = 0
    text: Text = ""
}

type FormatAnswer {
    what: Text = ""
    edits: List<FormatEdit>
    error: Text = ""
}

/// The work the server is doing, as the engine reports it (`Indexing`, 42).
type ServerProgress {
    title: Text = ""
    detail: Text = ""
    percent: Option<Int>
}

type ServerState {
    state: Text = ""
    server: Text = ""
    failure: Text = ""
    /// The work the server is doing (`Indexing 42%`), when it is doing any.
    progress: Option<ServerProgress>
    /// Each file's problems, by the path it stands at.
    diagnostics: Option<Map<List<Diag>>>
    answer: Text = ""
}

/// A project: its files and folders, the commands run in it, and what the device answers
/// about it.
type Project (title: folder, untitled: "Untitled project") {
    files: Collection<Entry>
    /// The commands run in the project, newest last.
    commands: Collection<Command>
    /// The folder on this device the project was opened from; empty for a project that lives
    /// in the workspace alone.
    folder: Text = ""
    /// The engine's job for the language server, while one runs for the open file's language.
    server_job: Text = ""
    /// The language the running server answers for.
    server_language: Text = ""
    /// The server's name (`rust-analyzer`), as its state line says it.
    server_name: Text = ""
    /// `starting`, `ready`, `busy`, `failed` or `stopped`; empty before a server was wanted.
    server_state: Text = ""
    /// What is wrong with the server, in a person's words.
    server_trouble: Text = ""
    /// The progress of the work the server is doing (`Indexing 42%`).
    server_progress: Text = ""
    /// Language servers answer for this project (the personal preference, kept with it).
    language_servers: Bool = true
    /// A file is formatted by its server each time it is saved.
    format_on_save: Bool = false
    /// The line a person reads code help about, and the words it answers: what a completion
    /// list, a hover or a definition put on the page.
    answer_what: Text = ""
    answer_text: Text = ""
    answer_line: Int = 0
    answer_column: Int = 0
    answer_items: List<Completion>
    answer_locations: List<Location>
    /// The project search the person started, as it stands.
    search_job: Text = ""
    search_query: Text = ""
    search_text: Text = ""
    search_case: Bool = false
    search_word: Bool = false
    search_state: Text = ""
    search_hits: List<Hit>
    search_files: Int = 0
    search_truncated: Bool = false
    /// What the find row of the open file last found.
    found: List<Found>
    /// What the last thing the app did came to, where a person reads it.
    trouble: Text = ""
}

root project: Project

default view project for Project

default view file_view for Entry

place view files for Project

/// The language a file is written in, by its name's extension, as the editor names it:
/// `Plain Text` where no extension says one.
// ---------------------------------------------------------------- names, languages, paths
fn language_of(name: Text) -> Text {
    let ext = name.split(".").last() ?? ""
    let lower = ext.lower()
    if lower == "rs" {
        "Rust"
    } else if lower == "toml" {
        "TOML"
    } else if lower == "md" or lower == "markdown" {
        "Markdown"
    } else if lower == "json" {
        "JSON"
    } else if lower == "yaml" or lower == "yml" {
        "YAML"
    } else if lower == "py" {
        "Python"
    } else if lower == "js" or lower == "mjs" or lower == "cjs" {
        "JavaScript"
    } else if lower == "ts" or lower == "tsx" {
        "TypeScript"
    } else if lower == "html" or lower == "htm" {
        "HTML"
    } else if lower == "css" {
        "CSS"
    } else if lower == "sh" or lower == "bash" or lower == "zsh" {
        "Shell"
    } else if lower == "c" or lower == "h" {
        "C"
    } else if lower == "cpp" or lower == "cc" or lower == "cxx" or lower == "hpp" {
        "C++"
    } else if lower == "go" {
        "Go"
    } else if lower == "v" or lower == "vh" {
        "Verilog"
    } else if lower == "sv" or lower == "svh" { "SystemVerilog" } else { "Plain Text" }
}

/// Every language a file can be kept as, as the editor names it.
fn language_labels() -> List<Text> {
    [
        "Rust",
        "TOML",
        "Markdown",
        "JSON",
        "YAML",
        "Python",
        "JavaScript",
        "TypeScript",
        "HTML",
        "CSS",
        "Shell",
        "C",
        "C++",
        "Go",
        "Verilog",
        "SystemVerilog",
        "Plain Text",
    ]
}

/// The language `text` names, however its letters are typed; none when it names no language
/// the editor knows.
fn language_named(text: Text) -> Option<Text> {
    language_labels().find(one -> one.lower() == text.trim().lower())
}

/// How many spaces one step of indentation is in `language`.
fn indent_of(language: Text) -> Int {
    if language == "JavaScript"
        or language == "TypeScript"
        or language == "HTML"
        or language == "CSS"
        or language == "JSON"
        or language == "YAML" {
        2
    } else { 4 }
}

/// The line comment marker `language` writes, and nothing for a language whose comments are
/// not lines (Markdown, HTML, CSS, JSON, plain text).
fn comment_of(language: Text) -> Text {
    if language == "Rust"
        or language == "JavaScript"
        or language == "TypeScript"
        or language == "C"
        or language == "C++"
        or language == "Go"
        or language == "Verilog"
        or language == "SystemVerilog" {
        "//"
    } else if language == "Python"
        or language == "Shell"
        or language == "TOML"
        or language == "YAML" {
        "#"
    } else { "" }
}

/// Where `e` stands in the project, its folders' names then its own, joined by `/`.
fn path_of(e: Entry) -> Text {
    if let p = e.parent { "{path_of(p)}/{e.name}" } else { e.name }
}

/// How deep `e` stands: no folder above it, 0.
fn depth_of(e: Entry) -> Int {
    if let p = e.parent { 1 + depth_of(p) } else { 0 }
}

/// Whether the file name `name` is one the editor does not accept: empty, a path, or the
/// names a folder keeps for itself.
fn named(name: Text) -> Bool {
    let trimmed = name.trim()
    trimmed != ""
        and trimmed.len() <= 255
        and not trimmed.contains("/")
        and not trimmed.contains("\\")
        and trimmed != "."
        and trimmed != ".."
}

/// The folder named `name` inside `folder`, and nothing when that name stands for a file
/// already.
fn free_in(p: Project, folder: Option<Entry>, name: Text, except: Option<Entry>) -> Bool {
    p.files.find(
        e -> e.parent == folder and e.name.lower() == name.lower() and not is_entry(e, except),
    )
        == .none
}

/// Whether `e` is the entry `other` names, when it names one.
fn is_entry(e: Entry, other: Option<Entry>) -> Bool {
    if let x = other { e == x } else { false }
}

/// The entries standing under `parent`, in the order the collection keeps them.
fn under(p: Project, parent: Option<Entry>) -> List<Entry> {
    if let f = parent {
        p.files.filter(e -> e.parent == parent and e != f)
    } else { p.files.filter(e -> e.parent == parent) }
}

/// The name a new entry takes: what a person typed, else `untitled`.
fn untitled_name(name: Text) -> Text {
    if name.trim() == "" { "untitled" } else { name.trim() }
}

/// The file the project has at `path` on this device, and nothing when none stands there.
fn file_at(p: Project, path: Text) -> Option<Entry> {
    p.files.find(e -> e.kind == .file and e.disk == path)
}

/// The entry a folder of the project is, by the path it stands at on this device.
fn folder_at(p: Project, path: Text) -> Option<Entry> {
    p.files.find(e -> e.kind == .folder and e.disk == path)
}

/// The text of `text`'s line `line`, counted from 0, and nothing past its last line.
fn line_at(text: Text, line: Int) -> Option<Text> {
    text.split("\n").at(line)
}

/// The lines of `text`, always at least one.
fn lines_of(text: Text) -> List<Text> {
    text.split("\n")
}

/// `text` with its lines `lines` joined again.
/// The lines `lines` from the one at `at` on.
fn rest_from(lines: List<Text>, at: Int) -> List<Text> {
    if at >= lines.len() {
        []
    } else {
        let one = lines.at(at) ?? ""
        [[one], rest_from(lines, at + 1)].flat_map(x -> x)
    }
}

fn joined(lines: List<Text>) -> Text {
    lines.join("\n")
}

/// The character index of the start of line `line` in `text`, counted from 0.
fn offset_of(text: Text, line: Int, column: Int) -> Int {
    if line <= 0 {
        return column
    }
    let lines = lines_of(text)
    let before = lines.take(line)
    let start = before.map(l -> l.len() + 1).sum()
    start + column
}

/// The `(line, column)` a character index `at` falls on in `text`.
fn place_of(text: Text, at: Int) -> List<Int> {
    let left = text.slice(0, at)
    let lines = left.split("\n")
    let line = lines.len() - 1
    let column = (lines.last() ?? "").len()
    [line, column]
}

/// A digest of `text`'s characters, so a check knows whether the file changed under it.
fn fingerprint(text: Text) -> Text {
    total(text, 0, 0)
}

fn total(text: Text, at: Int, sum: Int) -> Text {
    if at >= text.len() {
        return "{text.len()}:{sum % 2147483647}"
    } else {
        total(text, at + 1, (sum * 31 + code_of(text.slice(at, at + 1))) % 2147483647)
    }
}

fn code_of(one: Text) -> Int {
    one.len()
}

/// `text`'s words on one line, for a row or a result.
fn one_line(text: Text) -> Text {
    text.replace("\r", " ").replace("\n", " ").trim()
}

/// Starts a file called `name` in `folder`, holding `text`: what a person types in the project,
/// what a drop makes, and where an import lands.
// ---------------------------------------------------------------- the project's files
primary action new_file(
    p: Project,
    name: Text = "",
    folder: Option<Entry> = .none,
    text: Text = "",
) -> Entry (icon: .file_plus) {
    require named(untitled_name(name)) "A file's name cannot hold a slash and cannot be empty"
    require free_in(p, folder, untitled_name(name), .none) "An entry of that name is in the folder already"
    let named = untitled_name(name)
    let made = p.files.insert(
        Entry(
            kind: .file,
            name: named,
            parent: folder,
            language: language_of(named),
            text: text,
            saved: text,
        ),
    )
    made
}

/// Starts a folder called `name` in `folder`.
action new_folder(p: Project, name: Text = "", folder: Option<Entry> = .none) -> Entry (
    icon: .folder_plus,
) {
    require named(untitled_name(name)) "A folder's name cannot hold a slash and cannot be empty"
    require free_in(p, folder, untitled_name(name), .none) "An entry of that name is in the folder already"
    let named = untitled_name(name)
    let made = p.files.insert(
        Entry(kind: .folder, name: named, parent: folder, language: "", text: "", saved: ""),
    )
    made
}

/// Renames `entry` to `name`, keeping the names in its folder apart.
action rename(p: Project, entry: Entry, name: Text) (icon: .pencil, label: "Rename") {
    require named(name) "A name cannot hold a slash and cannot be empty"
    require free_in(p, entry.parent, name.trim(), .some(entry)) "An entry of that name is in the folder already"
    entry.name = name.trim()
    if entry.kind == .file {
        entry.language = language_of(entry.name)
    }
}

/// Moves `entry` into `folder` (the top of the project when none), just before `before` — or
/// after every entry there when none names one.
action move_entry(
    p: Project,
    entry: Entry,
    folder: Option<Entry> = .none,
    before: Option<Entry> = .none,
) (icon: .arrow_right, label: "Move") {
    require folder == .none or option_kind(folder) == "folder" "An entry moves into a folder"
    require folder != .some(entry) and not inside_it(folder, entry) "A folder cannot hold itself"
    require free_in(p, folder, entry.name, .some(entry)) "An entry of that name is in the folder already"
    entry.parent = folder
    if let target = before {
        p.files.move(entry, before: target)
    } else if let last = under(p, folder).filter(e -> e != entry).last() {
        p.files.move(entry, after: last)
    }
}

/// Moves `entry` just after `after`, as a drop on the row below asks.
action move_after(p: Project, entry: Entry, after: Entry) (label: "Move") {
    require after != entry "An entry cannot stand after itself"
    entry.parent = after.parent
    p.files.move(entry, after: after)
}

/// The kind of the entry `folder` names, as a word.
fn option_kind(folder: Option<Entry>) -> Text {
    if let f = folder {
        if f.kind == .folder { "folder" } else { "file" }
    } else { "folder" }
}

/// Whether `folder` is `inner` or holds it, at any depth.
fn inside_it(folder: Option<Entry>, inner: Entry) -> Bool {
    if let f = folder { holds(f, inner) } else { false }
}

/// Whether `outer` is `inner` or holds it, at any depth.
fn holds(outer: Entry, inner: Entry) -> Bool {
    if outer == inner { true } else if let p = inner.parent { holds(outer, p) } else { false }
}

/// Deletes `entry`, and every entry under it: a folder empties with its entries, as one change
/// a person takes back at once.
destructive action delete(p: Project, entry: Entry) (icon: .trash, key: "Delete") {
    for e in p.files.descendants(entry, .parent) {
        p.files.remove(e)
    }
    p.files.remove(entry)
}

/// Sets the language `entry` is coloured and checked as, by the name of a language the editor
/// knows.
action set_language(entry: Entry, language: Text) (icon: .code, label: "Language") {
    require language_named(language) != .none "`{language}` is not a language the editor knows"
    if let named = language_named(language) {
        entry.language = named
    }
}

/// Writes `text` as the whole file, for an agent or a script that holds a file's new text.
action replace_text(entry: Entry, text: Text) (
    icon: .pencil,
    label: "Replace the text",
    did: "wrote a file",
) {
    require entry.kind == .file "A folder has no text"
    entry.text = text
}

/// Replaces `count` characters from `at` with `text`: a range or a caret in one file, counted in
/// characters from its start.
action replace_range(entry: Entry, at: Int, count: Int = 0, text: Text) (
    icon: .pencil,
    label: "Replace a range",
    did: "wrote a file",
) {
    require entry.kind == .file "A folder has no text"
    require at >= 0 and count >= 0 and at + count <= entry.text.len() "The range lies outside the file"
    entry.text = "{entry.text.slice(0, at)}{text}{entry.text.slice(at + count, entry.text.len())}"
}

/// Copies a line of the file to the line under it.
action duplicate_line(entry: Entry, line: Int = 0) (icon: .copy, label: "Duplicate line") {
    require line >= 0 and line < lines_of(entry.text).len() "The line is outside the file"
    let lines = lines_of(entry.text)
    let one = lines.at(line) ?? ""
    entry.text = joined([lines.take(line + 1), [one], rest_from(lines, line + 1)].flat_map(x -> x))
}

/// Moves a line over the one above it (`up`) or under the one below it.
action move_line(entry: Entry, line: Int = 0, up: Bool = true) (
    icon: .arrow_up,
    label: "Move line",
) {
    require line >= 0 and line < lines_of(entry.text).len() "The line is outside the file"
    require if up { line > 0 } else { line + 1 < lines_of(entry.text).len() } "The line is at the file's edge"
    let lines = lines_of(entry.text)
    let to = if up { line - 1 } else { line + 1 }
    let moved = lines.at(line) ?? ""
    let other = lines.at(to) ?? ""
    entry.text = joined(
        [
            lines.take(to.min(line)),
            if up { [moved, other] } else { [other, moved] },
            rest_from(lines, to.max(line) + 1),
        ]
            .flat_map(x -> x),
    )
}

/// Deletes a line of the file.
action delete_line(entry: Entry, line: Int = 0) (icon: .delete, label: "Delete line") {
    require line >= 0 and line < lines_of(entry.text).len() "The line is outside the file"
    let lines = lines_of(entry.text)
    let kept = [lines.take(line), rest_from(lines, line + 1)].flat_map(x -> x)
    entry.text = if kept.is_empty() { "" } else { joined(kept) }
}

/// Comments out a line, or takes its comment away when the line stands commented already: the
/// marker is the language's own, at the line's own indentation.
action toggle_comment(entry: Entry, line: Int = 0) (icon: .slash, label: "Toggle comment") {
    require comment_of(entry.language) != "" "A {entry.language} file has no line comment"
    require line >= 0 and line < lines_of(entry.text).len() "The line is outside the file"
    let marker = comment_of(entry.language)
    let lines = lines_of(entry.text)
    let one = lines.at(line) ?? ""
    let indent = leading_spaces(one)
    let body = one.slice(indent, one.len())
    let changed = if body.starts_with("{marker} ") {
        "{one.slice(0, indent)}{body.slice(marker.len() + 1, body.len())}"
    } else { "{one.slice(0, indent)}{marker} {body}" }
    entry.text = joined([lines.take(line), [changed], rest_from(lines, line + 1)].flat_map(x -> x))
}

/// Adds one indentation step to a line.
action indent_line(entry: Entry, line: Int = 0) (icon: .arrow_right, label: "Indent") {
    require line >= 0 and line < lines_of(entry.text).len() "The line is outside the file"
    let step = spaces(indent_of(entry.language))
    entry.text = shifted(entry, line, step, true)
}

/// Takes one indentation step off a line.
action outdent_line(entry: Entry, line: Int = 0) (icon: .arrow_left, label: "Outdent") {
    require line >= 0 and line < lines_of(entry.text).len() "The line is outside the file"
    entry.text = shifted(entry, line, "", false)
}

fn shifted(entry: Entry, line: Int, step: Text, inward: Bool) -> Text {
    let lines = lines_of(entry.text)
    let one = lines.at(line) ?? ""
    let width = indent_of(entry.language)
    let spaces = leading_spaces(one)
    let changed = if inward {
        "{step}{one}"
    } else if spaces == 0 { one } else { one.slice(spaces.min(width), one.len()) }
    joined([lines.take(line), [changed], rest_from(lines, line + 1)].flat_map(x -> x))
}

/// `count` spaces: one indentation step's room.
fn spaces(count: Int) -> Text {
    if count <= 0 { "" } else { " {spaces(count - 1)}" }
}

/// How many spaces and tabs `one` starts with.
fn leading_spaces(one: Text) -> Int {
    count_leading(one, 0)
}

fn count_leading(one: Text, at: Int) -> Int {
    if at >= one.len() {
        at
    } else if one.slice(at, at + 1) == " " or one.slice(at, at + 1) == "\t" {
        count_leading(one, at + 1)
    } else { at }
}

// ---------------------------------------------------------------- find and replace

/// The window's search finds the words in the open file and across the project's files: the
/// first place in the file is shown, so a hit is in sight as the words are typed.
action find_in_window(p: Project, entry: Option<Entry>, text: Text) uses time (
    icon: .search,
    label: "Find",
) {
    let words = text.trim()
    p.search_text = words
    p.found = []
    if words == "" {
        stop_search(p)
    } else {
        if let e = entry {
            p.found = find_text(e, words)
            if let first = p.found.at(0) {
                reveal_line(e, first.line, first.column)
            }
        }
        if words.len() >= 2 {
            search_project(p, words)
        }
    }
}

/// Finds the words `text` in a file, the case of their letters aside unless `case` says
/// otherwise; `word` matches only where they stand alone as a word.
action find_text(entry: Entry, text: Text, case: Bool = false, word: Bool = false) -> List<Found> (
    icon: .search,
    label: "Find in file",
) {
    applies text != "" "Type something to find"
    let wanted = if case { text } else { text.lower() }
    let found: List<Found> = []
    let at = 0
    for line in lines_of(entry.text) {
        let hay = if case { line } else { line.lower() }
        let column = where_in(hay, wanted, word, 0)
        if let column = column {
            found = found + [Found(line: at, column: column, words: line.trim())]
        }
        at = at + 1
    }
    found
}

/// The first place `needle` stands in `hay` from `from` on, or nothing.
fn where_in(hay: Text, needle: Text, word: Bool, from: Int) -> Option<Int> {
    if from >= hay.len() {
        return .none
    }
    let found = hay.slice(from, hay.len()).find(needle)
    if let at = found {
        let whole = at + from
        if not word or alone_in(hay, whole, needle.len()) {
            return .some(whole)
        }
        return where_in(hay, needle, word, whole + 1)
    }
    .none
}

fn alone_in(hay: Text, at: Int, len: Int) -> Bool {
    let before = if at == 0 { " " } else { hay.slice(at - 1, at) }
    let after = if at + len >= hay.len() { " " } else { hay.slice(at + len, at + len + 1) }
    not word_char(before) and not word_char(after)
}

fn word_char(one: Text) -> Bool {
    let lower = one.lower()
    lower >= "a" and lower <= "z" or one >= "0" and one <= "9" or one == "_"
}

/// Replaces every place the words `text` stand in a file, answering how many changed.
action replace_in_file(
    entry: Entry,
    text: Text,
    with: Text,
    case: Bool = false,
    word: Bool = false,
) -> Int (icon: .pencil, label: "Replace in file", did: "replaced words") {
    applies text != "" "Type something to replace"
    let before = entry.text
    let after = replaced(before, text, with, case, word)
    entry.text = after
    if before == after { 0 } else { count_in(before, text, case, word) }
}

fn replaced(text: Text, needle: Text, with: Text, case: Bool, word: Bool) -> Text {
    if case {
        if word { replace_whole(text, needle, with) } else { text.replace(needle, with) }
    } else { replace_folded(text, needle, with, 0) }
}

fn replace_folded(text: Text, needle: Text, with: Text, from: Int) -> Text {
    let at = where_in(text.lower(), needle.lower(), false, from)
    if let at = at {
        let next = "{text.slice(0, at)}{with}{text.slice(at + needle.len(), text.len())}"
        replace_folded(next, needle, with, at + with.len())
    } else { text }
}

fn replace_whole(text: Text, needle: Text, with: Text) -> Text {
    let at = where_in(text, needle, true, 0)
    if let at = at {
        let next = "{text.slice(0, at)}{with}{text.slice(at + needle.len(), text.len())}"
        replace_whole(next, needle, with)
    } else { text }
}

fn count_in(text: Text, needle: Text, case: Bool, word: Bool) -> Int {
    count_from(
        if case { text } else { text.lower() },
        if case { needle } else { needle.lower() },
        word,
        0,
        0,
    )
}

fn count_from(hay: Text, needle: Text, word: Bool, from: Int, count: Int) -> Int {
    if let at = where_in(hay, needle, word, from) {
        count_from(hay, needle, word, at + needle.len().max(1), count + 1)
    } else { count }
}

/// The declarations a file's text writes — `fn`, `struct`, `enum`, `trait`, `class`, `def`,
/// `interface`, `type`, `const` — each with the line and column it stands at: what the
/// project's symbols are and what go-to-symbol offers.
action symbols(entry: Entry) -> List<Found> (icon: .code, label: "Go to symbol") {
    symbols_in(entry)
}

fn symbols_in(entry: Entry) -> List<Found> {
    let found: List<Found> = []
    let at = 0
    for line in lines_of(entry.text) {
        if let name = declared(line) {
            let column = line.find(name) ?? 0
            found = found + [Found(line: at, column: column, words: name)]
        }
        at = at + 1
    }
    found
}

fn declared(line: Text) -> Option<Text> {
    let trimmed = line.trim()
    let all = [
        "pub fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "class ",
        "def ",
        "function ",
        "interface ",
        "type ",
        "const ",
    ]
    let prefix = all.find(one -> trimmed.starts_with(one))
    if let prefix = prefix {
        let after = trimmed.slice(prefix.len(), trimmed.len())
        return .some(name_start(after))
    }
    .none
}

fn name_start(rest: Text) -> Text {
    let taken = ""
    let at = 0
    for one in rest.split("") {
        if one != "" and word_char(one) {
            taken = "{taken}{one}"
            at = at + 1
        } else if taken == "" {
            at = at + 1
        } else {
            return taken
        }
    }
    taken
}

/// The problems a file's text has, found without a language server: brackets that do not
/// balance, indentation that mixes its steps, lines longer than a reader reads, trailing
/// whitespace and the notes a person left in the code.
///
/// These are facts about the text, so they stand until a server answers for the file; once one
/// has, its problems are the file's, since a server knows what the code means and this does
/// not.
// ---------------------------------------------------------------- the editor's own checks
fn lint(entry: Entry) -> List<Problem> {
    let found: List<Problem> = []
    let at = 0
    let depth = 0
    let limit = if entry.language == "Rust" { 100 } else { 120 }
    for line in lines_of(entry.text) {
        let width = line.len()
        if (line.ends_with(" ") or line.ends_with("\t")) and line.trim() != "" {
            found = found
                + [Problem(severity: .warning, line: at, column: 0, message: "Trailing whitespace")]
        }
        if width > limit {
            found = found
                + [
                    Problem(
                        severity: .warning,
                        line: at,
                        column: limit,
                        message: "The line is {width} characters long",
                    ),
                ]
        }
        if line.contains("TODO") or line.contains("FIXME") {
            found = found
                + [
                    Problem(
                        severity: .info,
                        line: at,
                        column: 0,
                        message: "A note left in the code",
                    ),
                ]
        }
        if leading_spaces(line) > 0 and line.slice(0, 1) == "\t" and line.contains("  ") {
            found = found
                + [
                    Problem(
                        severity: .info,
                        line: at,
                        column: 0,
                        message: "Indentation mixes tabs and spaces",
                    ),
                ]
        }
        let opened = count_of(line, "\{") + count_of(line, "(") + count_of(line, "[")
        let closed = count_of(line, "\}") + count_of(line, ")") + count_of(line, "]")
        depth = depth + opened - closed
        at = at + 1
    }
    if depth > 0 {
        found = found
            + [
                Problem(
                    severity: .error,
                    line: lines_of(entry.text).len() - 1,
                    column: 0,
                    message: "A bracket stands open at the end of the file",
                ),
            ]
    } else if depth < 0 {
        found = found
            + [
                Problem(
                    severity: .error,
                    line: 0,
                    column: 0,
                    message: "A bracket closes with none open",
                ),
            ]
    }
    found
}

fn count_of(line: Text, piece: Text) -> Int {
    line.split(piece).len() - 1
}

/// Checks a file and keeps what it found: the device's server's problems when one answers for
/// the file, the editor's own otherwise.
action check(p: Project, entry: Entry) (icon: .check, label: "Check the file") {
    require entry.kind == .file "A folder holds no text"
    ensure_server(p, entry)
    let digest = fingerprint(entry.text)
    entry.checked = digest
    if has_server(p, entry) {
        entry.problems = []
        entry.source = p.server_name
    } else {
        entry.problems = lint(entry)
        entry.source = "lint"
    }
}

/// Describes what a file's check found, for the page's line about it.
fn problems_said(e: Entry) -> Text {
    if e.checked == "" {
        return ""
    }
    if e.problems.is_empty() {
        if e.source == "lint" { "No problems in this file" } else { "{e.source}: no problems" }
    } else if e.problems.len() == 1 { "1 problem" } else { "{e.problems.len()} problems" }
}

fn problem_severity(one: Problem) -> Text {
    if one.severity == .error {
        "error"
    } else if one.severity == .warning {
        "warning"
    } else if one.severity == .info { "info" } else { "hint" }
}

/// Opens the folder `folder` as the project: each file and folder it holds, read as entries,
/// and each file's bytes as its text.
// ---------------------------------------------------------------- the device's files
action open_folder(p: Project, folder: Text) uses files (
    icon: .folder_open,
    label: "Open a project folder",
) {
    let root = if folder == "" { "~" } else { folder }
    match engine.call("list", json.encode(DiskAsk(path: root))) {
        .err(why) => {
            p.trouble = "Couldn’t read {root}: {why}"
        }
        .ok(answer) => {
            let olds: List<Entry> = []
            for e in p.files {
                olds = olds + [e]
            }
            for e in olds {
                p.files.remove(e)
            }
            p.folder = root
            p.trouble = ""
            read_into(p, .none, answer)
        }
    }
}

/// Records the entries `answer` lists, standing in `folder`, and reads a file's own bytes:
/// the tree is the project's, so every file the project shows is one a save can check. The
/// listing crosses an action call as the text the device answered, which it already is.
action read_into(p: Project, parent: Option<Entry>, answer: Text) {
    match json.decode<Listing>(answer) {
        .ok(listing) => {
            for found in listing.entries {
                if not found.hidden {
                    let made = p.files.insert(
                        Entry(
                            kind: if found.folder { .folder } else { .file },
                            name: found.name,
                            parent: parent,
                            language: if found.folder { "" } else { language_of(found.name) },
                            text: "",
                            saved: "",
                            disk: found.path,
                        ),
                    )
                    if found.folder {
                        read_folder_into(p, made, found.path)
                    } else { read_text(p, made) }
                }
            }
        }
        .err(why) => {
            p.trouble = why
        }
    }
}

action read_folder_into(p: Project, folder: Entry, path: Text) {
    match engine.call("list", json.encode(DiskAsk(path: path))) {
        .ok(answer) => read_into(p, .some(folder), answer),
        .err(_) => {}
    }
}

action read_text(p: Project, entry: Entry) {
    match engine.call("load", json.encode(DiskAsk(path: entry.disk))) {
        .ok(answer) => match json.decode<Loaded>(answer) {
            .ok(loaded) => {
                entry.text = loaded.text
                entry.saved = loaded.text
                entry.stamp = loaded.mtime
                entry.stamp_len = loaded.len
                entry.stale = false
            }
            .err(why) => {
                p.trouble = "Couldn’t read {entry.name}: {why}"
            }
        },
        .err(why) => {
            p.trouble = "Couldn’t read {entry.name}: {why}"
        }
    }
}

/// Opens a file a person chose, or one Files or another app handed over: `file` is the file it
/// is, and its words become an entry of the project.
action open_file(p: Project, file: File) uses files (
    icon: .file_plus,
    label: "Open a file",
    did: "opened a file",
    opens: [
    "text/rust",
    "text/x-rust",
    "text/x-python",
    "text/x-python3",
    "text/javascript",
    "application/javascript",
    "text/x-typescript",
    "application/typescript",
    "text/css",
    "application/toml",
    "application/x-toml",
    "application/yaml",
    "application/x-yaml",
    "text/x-yaml",
    "text/yaml",
    "application/x-sh",
    "text/x-shellscript",
    "text/x-csrc",
    "text/x-chdr",
    "text/x-c",
    "text/x-c++src",
    "text/x-c++hdr",
    "text/x-c++",
    "text/x-go",
    "text/x-verilog",
    "text/x-systemverilog",
    .rs,
    .toml,
    .py,
    .js,
    .mjs,
    .cjs,
    .ts,
    .tsx,
    .css,
    .yaml,
    .yml,
    .sh,
    .bash,
    .zsh,
    .c,
    .h,
    .cpp,
    .cc,
    .cxx,
    .hpp,
    .go,
    .v,
    .vh,
    .sv,
    .svh,
],
) {
    match file.text() {
        .ok(text) => {
            let made = p.files.insert(
                Entry(
                    kind: .file,
                    name: file.name,
                    language: language_of(file.name),
                    text: text,
                    saved: text,
                ),
            )
            made
            p.trouble = ""
        }
        .err(why) => {
            p.trouble = "Couldn’t open {file.name}: {why}"
        }
    }
}

/// Writes a file's text to the device, checking first that the bytes there are the ones this
/// project read: a file something else changed is not written over without the person saying
/// so.
action save(p: Project, entry: Entry) uses files (icon: .hard_drive, label: "Save") {
    require entry.kind == .file "A folder has no text"
    require entry.disk != "" "This file is nowhere on the device yet: save it as one"
    write(p, entry, false)
}

action write(p: Project, entry: Entry, force: Bool) -> Bool {
    let ask = DiskSave(
        path: entry.disk,
        text: entry.text,
        mtime: entry.stamp,
        len: entry.stamp_len,
        force: force,
    )
    match engine.call("save", json.encode(ask)) {
        .ok(answer) => match json.decode<Stamped>(answer) {
            .ok(stamped) => {
                entry.saved = entry.text
                entry.stamp = stamped.mtime
                entry.stamp_len = stamped.len
                entry.stale = false
                p.trouble = ""
                if has_server(p, entry) { announce(p, entry) }
                if p.format_on_save { format_file(p, entry) }
                true
            }
            .err(why) => {
                p.trouble = "Couldn’t save {entry.name}: {why}"
                false
            }
        },
        .err(why) => {
            if why == "conflict" {
                entry.stale = true
                p.trouble = "{entry.name} changed on disk. Save over it, or reload it, to go on."
            } else {
                p.trouble = "Couldn’t save {entry.name}: {why}"
            }
            false
        }
    }
}

/// Saves a file over the device's change, as a person chose after reading what changed.
action overwrite(p: Project, entry: Entry) uses files (icon: .hard_drive, label: "Save over it") {
    write(p, entry, true)
}

/// Reads the file again from the device, dropping what changed here.
action reload(p: Project, entry: Entry) uses files (icon: .rotate_ccw, label: "Reload from disk") {
    read_text(p, entry)
    p.trouble = ""
}

/// Writes every file that changed here to the device.
action save_all(p: Project) uses files (icon: .hard_drive, label: "Save all") {
    for e in p.files {
        if e.kind == .file and e.disk != "" and e.text != e.saved and not e.stale {
            write(p, e, false)
        }
    }
}

/// Writes `entry` into the folder `files` names, under its own name.
action save_into(p: Project, entry: Entry, files: List<File>) {
    require not files.is_empty() "Choose a folder to save the file in"
    let held = files.first()
    if let one = held {
        let folder = one.name
        entry.disk = folder
        write(p, entry, true)
    }
}

/// Moves a file into the folder `to` of the project, on the device as in the tree.
action move_on_disk(p: Project, entry: Entry, to: Text) uses files (label: "Move on disk") {
    require entry.disk != "" "This file is nowhere on the device"
    let name = entry.name
    let destination = "{to}/{name}"
    match File.move(entry.disk, to) {
        .ok(_) => {
            entry.disk = destination
            p.trouble = ""
        }
        .err(why) => {
            p.trouble = "Couldn’t move {name}: {why}"
        }
    }
}

/// Imports the files a person dropped on the window: a folder becomes a folder of the project,
/// a text file an entry with its words.
action import_dropped(p: Project, files: List<File>) (icon: .file_plus, label: "Import files") {
    for file in files {
        open_file(p, file)
    }
}

/// Runs `command` in the project's folder, as the terminal does: its output and its exit status
/// are kept with the command, and a person reads them beside the code.
// ---------------------------------------------------------------- the terminal
action run(p: Project, command: Text, cwd: Text = "") uses time (icon: .play, label: "Run") {
    require command.trim() != "" "Type a command to run"
    let here = if cwd != "" { cwd } else if p.folder != "" { p.folder } else { "~" }
    match engine.start(
        "run",
        json.encode(RunAsk(command: command, cwd: here)),
        done: j -> ran(p, j),
        progress: j -> ran(p, j),
    ) {
        .ok(job) => {
            let made = p.commands.insert(
                Command(
                    command: command,
                    cwd: here,
                    started: now,
                    output: "",
                    job: job,
                    state: "running",
                ),
            )
            made
            p.trouble = ""
        }
        .err(why) => {
            p.trouble = "Couldn’t run the command: {why}"
        }
    }
}

/// Stops the command that runs in the project now.
action stop(p: Project) uses time (icon: .square, label: "Stop") {
    require not p.commands.filter(c -> c.state == "running").is_empty() "Nothing runs in the project now"
    let running = p.commands.filter(c -> c.state == "running")
    for c in running {
        engine.cancel(c.job)
        c.state = "stopped"
        c.ended = now
        c.job = ""
    }
}

/// Runs a command of the project's terminal again, as a person does from its history.
action rerun(p: Project, run: Command) uses time (icon: .repeat, label: "Run again") {
    run_(p, run.cwd, run.command)
}

/// Runs one test of a run again: the command with the test's name after it, run as that run's
/// tool ran it.
action rerun_test(p: Project, run: Command, test: Text) uses time (
    icon: .repeat_1,
    label: "Run the test again",
) {
    run_(p, run.cwd, "{run.command} {test}")
}

action run_(p: Project, cwd: Text, command: Text) uses time {
    match engine.start(
        "run",
        json.encode(RunAsk(command: command, cwd: cwd)),
        done: j -> ran(p, j),
        progress: j -> ran(p, j),
    ) {
        .ok(job) => {
            let made = p.commands.insert(
                Command(
                    command: command,
                    cwd: cwd,
                    started: now,
                    output: "",
                    job: job,
                    state: "running",
                ),
            )
            made
            p.trouble = ""
        }
        .err(why) => {
            p.trouble = why
        }
    }
}

/// Erases the terminal's history.
destructive action clear_commands(p: Project) (icon: .trash, label: "Clear the terminal") {
    for c in p.commands {
        p.commands.remove(c)
    }
}

/// Opens the project's folder in the system's terminal: the shell a person runs by hand.
action open_terminal(p: Project) uses files (icon: .terminal, label: "Open a terminal") {
    let here = if p.folder != "" { p.folder } else { "~" }
    match tool.terminal(here) {
        .ok => {
            p.trouble = ""
        }
        .err(why) => {
            p.trouble = "Couldn’t open a terminal: {why}"
        }
    }
}

/// The command a job runs ended, or moved: its output, exit status and truncated flag are kept
/// with the command, its output's places become items, and a failure is the window's trouble.
action ran(p: Project, j: host.Job) uses time {
    let running = p.commands.filter(x -> x.state == "running" and x.job == j.id)
    for c in running {
        match json.decode<RunState>(j.detail) {
            .ok(state) => {
                c.output = state.output
                c.truncated = state.truncated
                c.exit = state.exit
                if state.state != "running" {
                    c.state = if state.failure != "" { "failed" } else { state.state }
                    c.ended = now
                    c.job = ""
                    c.items = items_of(state.output)
                    if state.failure != "" {
                        p.trouble = state.failure
                    }
                }
            }
            .err(_) => {}
        }
    }
}

/// The project search ended, or moved: each hit it found is added as it comes, and the files it
/// searched are counted.
action searched(p: Project, j: host.Job) {
    if p.search_job != j.id {
        return
    }
    match json.decode<SearchState>(j.detail) {
        .ok(state) => {
            p.search_hits = p.search_hits + state.hits
            p.search_files = state.files
            p.search_truncated = state.truncated
            p.search_state = state.state
            if state.state != "running" {
                p.search_job = ""
            }
        }
        .err(_) => {}
    }
}

/// Asks how far the language server has got, so its state, problems and answers reach the window.
action poll(p: Project) {
    poll_server(p)
}

/// Reads a command's output as the items a person can go to: a problem a compiler found, a
/// place a tool named, a test that passed or failed, and a change.
fn items_of(output: Text) -> List<Item> {
    let found: List<Item> = []
    for one in output.split("\n") {
        if let item = item_of(one) {
            found = found + [item]
        }
    }
    found
}

fn item_of(one: Text) -> Option<Item> {
    let trimmed = one.trim()
    if trimmed == "" {
        return .none
    }
    if trimmed.contains(": error") or trimmed.contains(": error:") {
        return .some(named_item(trimmed, .error, "error"))
    }
    if trimmed.contains(": warning") {
        return .some(named_item(trimmed, .warning, "warning"))
    }
    if trimmed.starts_with("test ") and trimmed.ends_with("... ok") {
        return .some(Item(kind: .test_passed, text: trimmed))
    }
    if trimmed.starts_with("test ") and trimmed.ends_with("FAILED") {
        return .some(Item(kind: .test_failed, text: trimmed))
    }
    if trimmed.starts_with("test ") and trimmed.contains("ignored") {
        return .some(Item(kind: .test_ignored, text: trimmed))
    }
    if trimmed.starts_with("+++") or trimmed.starts_with("---") or trimmed.starts_with("@@") {
        return .some(Item(kind: .change, text: trimmed))
    }
    if let place = place_item(trimmed) {
        return .some(place)
    }
    .none
}

/// A line shaped `path:line:column: words`, as compilers and linters write one.
fn named_item(one: Text, kind: ItemKind, _says: Text) -> Item {
    let head = if let place = place_item(one) { place } else { Item(text: one) }
    Item(kind: kind, text: one, path: head.path, line: head.line, column: head.column)
}

fn place_item(one: Text) -> Option<Item> {
    let parts = one.split(":")
    if parts.len() < 3 {
        return .none
    }
    let path = parts.at(0) ?? ""
    let line = parts.at(1) ?? ""
    let column = parts.at(2) ?? ""
    if path == "" or line.trim() == "" or column.trim() == "" {
        return .none
    }
    let number = line.trim()
    let column_number = column.trim()
    if not digits(number) or not digits(column_number) {
        return .none
    }
    .some(
        Item(
            kind: .location,
            text: one,
            path: path,
            line: number.to_int() ?? 1,
            column: column_number.to_int() ?? 1,
        ),
    )
}

fn digits(one: Text) -> Bool {
    one != "" and one.split("").filter(c -> c != "").all(c -> c >= "0" and c <= "9")
}

/// Brings the caret to the place an item of a command's output names: the file is read if the
/// project does not hold it yet, and the page shows it.
action reveal_item(p: Project, path: Text, line: Int = 0) uses files (
    icon: .locate_fixed,
    label: "Go to it",
) {
    require path != "" "That line names no file"
    let at = canonical(path)
    if let known = file_at(p, at) {
        p.trouble = ""
        reveal_line(known, line, 0)
    } else if let found = find_file_named(p, path) {
        p.trouble = ""
        reveal_line(found, line, 0)
    } else {
        p.trouble = "{at} is not a file of this project"
    }
}

fn canonical(path: Text) -> Text {
    path
}

fn find_file_named(p: Project, path: Text) -> Option<Entry> {
    p.files.find(
        e -> e.kind == .file and (e.disk == path or e.name == path or e.disk.ends_with(path)),
    )
}

/// Shows `line` of `entry`, counted from 0: the editor scrolls it into view with the caret on it.
action reveal_line(entry: Entry, line: Int = 0, column: Int = 0) (
    icon: .locate_fixed,
    label: "Go to line",
) {
    entry.reveal_line = line
    entry.reveal_column = column
    entry.reveal_ask += 1
}

/// Shows `line` of the file at `path`, counted from 0.
action go_to_line(p: Project, path: Text, line: Int = 0) {
    if let file = file_at(p, path) {
        reveal_line(file, line, 0)
    } else {
        p.trouble = "{path} is not a file of this project"
    }
}

/// Searches every file of the project for `needle`, the case of its letters aside unless
/// `case` says otherwise.
// ---------------------------------------------------------------- project search
action search_project(p: Project, needle: Text, case: Bool = false, word: Bool = false) uses time (
    icon: .search,
    label: "Search the project",
) {
    applies needle != "" "Type something to find"
    if p.search_job != "" { engine.cancel(p.search_job) }
    let root = if p.folder != "" { p.folder } else { "~" }
    match engine.start(
        "search",
        json.encode(SearchAsk(root: root, needle: needle, case_sensitive: case, whole_word: word)),
        done: j -> searched(p, j),
        progress: j -> searched(p, j),
    ) {
        .ok(job) => {
            p.search_job = job
            p.search_query = needle
            p.search_text = needle
            p.search_case = case
            p.search_word = word
            p.search_hits = []
            p.search_files = 0
            p.search_truncated = false
            p.search_state = "running"
            p.trouble = ""
        }
        .err(why) => {
            p.trouble = "Couldn’t search: {why}"
        }
    }
}

/// Stops the project search.
action stop_search(p: Project) (icon: .square, label: "Stop searching") {
    if p.search_job != "" {
        engine.cancel(p.search_job)
        p.search_job = ""
        p.search_state = "stopped"
    }
}

/// Shows the place a search found: the file is read from the device when the project does not
/// hold it yet, and the page opens it.
action open_hit(p: Project, path: Text, line: Int = 0) uses files (
    icon: .locate_fixed,
    label: "Open the place",
) {
    if let known = file_at(p, path) {
        p.trouble = ""
        reveal_line(known, line, 0)
        let shown: Option<Entry> = .some(known)
        window_state("opened", shown)
    } else {
        match engine.call("load", json.encode(DiskAsk(path: path))) {
            .ok(answer) => match json.decode<Loaded>(answer) {
                .ok(loaded) => {
                    let made = p.files.insert(
                        Entry(
                            kind: .file,
                            name: file_name(path),
                            language: language_of(path),
                            text: loaded.text,
                            saved: loaded.text,
                            disk: path,
                            stamp: loaded.mtime,
                            stamp_len: loaded.len,
                        ),
                    )
                    reveal_line(made, line, 0)
                    let shown: Option<Entry> = .some(made)
                    window_state("opened", shown)
                    p.trouble = ""
                }
                .err(why) => {
                    p.trouble = why
                }
            },
            .err(why) => {
                p.trouble = "Couldn’t read {path}: {why}"
            }
        }
    }
}

fn file_name(path: Text) -> Text {
    path.split("/").last() ?? path
}

/// What the project search says of itself: how many results, in how many files, and how far it
/// scanned.
fn distinct_files(hits: List<Hit>) -> Int {
    let seen: List<Text> = []
    for hit in hits {
        if not seen.contains(hit.rel) {
            seen = seen + [hit.rel]
        }
    }
    seen.len()
}

fn search_said(p: Project) -> Text {
    if p.search_text == "" {
        return "Type words to find in every file of the project"
    }
    if p.search_state == "running" and p.search_hits.is_empty() {
        return "Searching…"
    }
    if p.search_hits.is_empty() {
        return "No file holds “{p.search_text}”"
    }
    let files = distinct_files(p.search_hits)
    let head = if p.search_hits.len() == 1 {
        "1 result"
    } else if files == 1 {
        "{p.search_hits.len()} results in 1 file"
    } else {
        "{p.search_hits.len()} results in {files} files"
    }
    if p.search_state == "running" {
        "{head}, still searching"
    } else if p.search_truncated {
        "{head}; the search stopped after {p.search_files} files"
    } else if p.search_files > 0 {
        "{head}, {p.search_files} files searched"
    } else { head }
}

/// Whether a server answers for `entry`: one runs for its language, and the person wants them.
// ---------------------------------------------------------------- language servers
fn has_server(p: Project, entry: Entry) -> Bool {
    p.language_servers
        and p.server_job != ""
        and p.server_language == entry.language
        and entry.disk != ""
}

/// Starts the language server for a file's language, if none runs for it yet, and tells it
/// about the file.
action ensure_server(p: Project, entry: Entry) {
    if not p.language_servers or entry.disk == "" {
        return
    }
    if p.server_job != "" and p.server_language == entry.language {
        announce(p, entry)
        return
    }
    if p.server_job != "" {
        engine.cancel(p.server_job)
        p.server_job = ""
    }
    let root = if p.folder != "" { p.folder } else { "~" }
    match engine.start("server", json.encode(ServerAsk(language: entry.language, root: root))) {
        .ok(job) => {
            p.server_job = job
            p.server_language = entry.language
            p.server_state = "starting"
            p.server_trouble = ""
            p.server_name = ""
            announce(p, entry)
        }
        .err(why) => {
            p.server_trouble = if why.contains("not found") or why.contains("No such file") {
                "The {entry.language} language server is not installed on this computer. {install_hint(entry.language)}"
            } else {
                "Code help is unavailable for {entry.language}: {why}"
            }
            p.server_state = "failed"
        }
    }
}

/// How to install the language server a language's code help needs, in that server's own way.
fn install_hint(language: Text) -> Text {
    if language == "Rust" {
        "Install it with “rustup component add rust-analyzer”, then try again."
    } else if language == "Python" {
        "Install it with “pip install pyright”, then try again."
    } else if language == "JavaScript" or language == "TypeScript" {
        "Install it with “npm install -g typescript-language-server typescript”, then try again."
    } else if language == "Go" {
        "Install it with “go install golang.org/x/tools/gopls@latest”, then try again."
    } else {
        "Install it with your system's package manager, then try again."
    }
}

/// Tells the server that `entry` is open, with the text it holds now, so its answers are about
/// what a person reads.
action announce(p: Project, entry: Entry) {
    if p.server_job == "" {
        return
    }
    let ask = DocumentAsk(
        job: p.server_job,
        path: entry.disk,
        language: entry.language,
        text: entry.text,
        version: 1,
    )
    engine.call("change", json.encode(ask))
}

/// Reads what the server answered: its state and progress, its problems for each file, and any
/// answer to a code-help ask the page made.
action poll_server(p: Project) {
    if p.server_job == "" {
        return
    }
    if let j = engine.job(p.server_job) {
        match json.decode<ServerState>(j.detail) {
            .ok(state) => {
                p.server_state = state.state
                if state.server != "" {
                    p.server_name = state.server
                }
                p.server_trouble = state.failure
                if let progress = state.progress {
                    let percent = progress.percent ?? 0
                    p.server_progress = if percent > 0 {
                        "{progress.title} {percent}%"
                    } else { progress.title }
                } else {
                    p.server_progress = ""
                }
                if let found = state.diagnostics {
                    for path in found.keys() {
                        if let held = found.get(path) {
                            if let file = file_at(p, path) {
                                file.problems = held.map(d -> problem_of(d))
                                file.source = p.server_name
                            }
                        }
                    }
                }
                let said = state.answer
                if said != "" { read_answer(p, said) }
                if state.state == "failed" or state.state == "stopped" {
                    p.server_job = ""
                }
            }
            .err(why) => {
                p.server_trouble = why
            }
        }
    }
}

fn problem_of(one: Diag) -> Problem {
    let severity: Severity = if one.severity == "warning" {
        .warning
    } else if one.severity == "info" {
        .info
    } else if one.severity == "hint" { .hint } else { .error }
    Problem(severity: severity, line: one.line, column: one.column, message: one.message)
}

/// Reads the answer the server gave to the page's last ask: a completion list, the words a
/// hover shows, the places a declaration stands at, or the edits a format makes.
action read_answer(p: Project, answer: Text) {
    if answer == "" {
        return
    }
    if p.answer_what == "hover" {
        match json.decode<HoverAnswer>(answer) {
            .ok(hover) => {
                p.answer_text = hover.text
            }
            .err(_) => {}
        }
    } else if p.answer_what == "complete" {
        match json.decode<CompletionAnswer>(answer) {
            .ok(found) => {
                p.answer_items = found.items
            }
            .err(_) => {}
        }
    } else if p.answer_what == "definition" {
        match json.decode<DefinitionAnswer>(answer) {
            .ok(found) => {
                p.answer_locations = found.locations
            }
            .err(_) => {}
        }
    }
}

/// Asks the server for completions at a place in a file.
action complete(p: Project, entry: Entry, line: Int = 0, column: Int = 0) (
    icon: .code,
    label: "Suggest",
) {
    ensure_server(p, entry)
    if p.server_job == "" {
        return
    }
    p.answer_what = "complete"
    p.answer_line = line
    p.answer_column = column
    p.answer_items = []
    let ask = AskAsk(
        job: p.server_job,
        path: entry.disk,
        line: line,
        column: column,
        what: "complete",
    )
    engine.call("ask", json.encode(ask))
}

/// Asks the server what stands at a place in a file.
action hover(p: Project, entry: Entry, line: Int = 0, column: Int = 0) (
    icon: .info,
    label: "What stands here",
) {
    ensure_server(p, entry)
    if p.server_job == "" {
        return
    }
    p.answer_what = "hover"
    p.answer_text = ""
    let ask = AskAsk(job: p.server_job, path: entry.disk, line: line, column: column, what: "hover")
    engine.call("ask", json.encode(ask))
}

/// Asks the server where the declaration at a place is written.
action definition(p: Project, entry: Entry, line: Int = 0, column: Int = 0) (
    icon: .locate_fixed,
    label: "Go to definition",
) {
    ensure_server(p, entry)
    if p.server_job == "" {
        return
    }
    p.answer_what = "definition"
    p.answer_locations = []
    let ask = AskAsk(
        job: p.server_job,
        path: entry.disk,
        line: line,
        column: column,
        what: "definition",
    )
    engine.call("ask", json.encode(ask))
}

/// Writes the completion a person chose where the caret stands.
action accept_completion(p: Project, entry: Entry, label: Text, insert: Text) (
    icon: .check,
    label: "Write the suggestion",
) {
    let at = offset_of(entry.text, p.answer_line, p.answer_column)
    let written = if insert != "" { insert } else { label }
    entry.text = "{entry.text.slice(0, at)}{written}{entry.text.slice(at, entry.text.len())}"
    p.answer_items = []
    p.answer_what = ""
}

/// Reformats a file as its language's formatter does, as one change.
action format_file(p: Project, entry: Entry) (icon: .wand_sparkles, label: "Format the file") {
    require entry.kind == .file "A folder holds no text"
    ensure_server(p, entry)
    if p.server_job == "" {
        return
    }
    p.answer_what = "format"
    p.answer_text = entry.disk
    let ask = AskAsk(job: p.server_job, path: entry.disk, line: 0, column: 0, what: "format")
    engine.call("ask", json.encode(ask))
}

/// Turns language servers on or off, for the project.
action toggle_language_servers(p: Project, servers: Bool = not p.language_servers) (
    icon: .code,
    label: "Language servers",
) {
    p.language_servers = servers
    if not p.language_servers and p.server_job != "" {
        engine.cancel(p.server_job)
        p.server_job = ""
        p.server_state = ""
    }
}

/// Turns formatting a file on save on or off.
action toggle_format_on_save(p: Project, format: Bool = not p.format_on_save) (
    icon: .wand_sparkles,
    label: "Format on save",
) {
    p.format_on_save = format
}

/// Stops the language server, as a person pressing "Try again" after a failure does.
action restart_server(p: Project) (icon: .rotate_cw, label: "Try again") {
    if p.server_job != "" { engine.cancel(p.server_job) }
    p.server_job = ""
    p.server_state = ""
    p.server_trouble = ""
}

/// One file or folder in the tree: its icon says what it is.
// ---------------------------------------------------------------- views
view file_row(e: Entry) {
    ui.TreeViewRow(icon: if e.kind == .folder { .folder } else { .file_text }, tone: .secondary)
}

/// The languages a file can be kept as, each choosing it for `e`.
view language_menu(e: Entry, current: Text) {
    ui.MenuButton("Language", icon: .code, icon_only: true) {
        for name in language_labels() {
            ui.MenuItem(name, checked: name == current) {
                on choose: set_language(e, name)
            }
        }
    }
}

/// The control a row shows while it is hovered: a file or folder inside it.
view add_inside(e: Entry) {
    ui.Button("New file", icon: .file_plus, icon_only: true) {
        on press: add_child(e)
    }
}

/// Starts a file inside the folder `e`: a row's "+", pressed.
action add_child(e: Entry) {
    if e.kind == .folder {
        project.files.insert(
            Entry(
                kind: .file,
                name: "untitled",
                parent: .some(e),
                language: "Plain Text",
                text: "",
                saved: "",
            ),
        )
    }
}

/// The window's title and its controls, given the file open beside the tree.
view project_head(open: Option<Entry>) {
    let p = project
    ui.MenuButton("Project", icon: .ellipsis, icon_only: true) {
        ui.MenuItem("Open a folder…", icon: .folder_open) {
            on choose: file.choose_folder(open_folder(p), accept: ["text/plain"])
        }
        ui.MenuItem("New file", icon: .file_plus) {
            on choose: new_file(p)
        }
        ui.MenuItem("New folder", icon: .folder_plus) {
            on choose: new_folder(p)
        }
        ui.MenuItem("Save all", icon: .hard_drive) {
            on choose: save_all(p)
        }
        ui.MenuItem("Search the project…", icon: .search) {
            on choose: search_project(p, p.search_text)
        }
        ui.MenuItem("Open a terminal", icon: .terminal) {
            on choose: open_terminal(p)
        }
    }
}

/// The window: the project's tree in the sidebar, the open file beside it, and under both the
/// problems the file has, the commands the project ran and the places its search found.
view project(p: Project) {
    // The file the window shows: the first of the tree until a person chooses another, and the
    // one a search result or a problem opens.
    state opened: Option<Entry> = p.files.filter(e -> e.kind == .file).first()
    // What stands under the code: the problems of the open file, the terminal, or the search's
    // places.
    state panel: Text = "problems"
    // The line the page's go-to row holds, counted from 1 as the status line shows it.
    state goto: Text = ""
    std.tree(
        p,
        p.files,
        opened: opened,
        parent: e -> e.parent,
        depth: e -> .some(depth_of(e)),
        branch: e -> e.kind == .folder,
        label: e -> e.name,
        place: (moved, folder, before) -> move_entry(p, moved, folder, before),
        place_after: (moved, after) -> move_after(p, moved, after),
        add: add_inside,
        path: e -> path_of(e),
        row: file_row,
        head: project_head,
        finds: words -> find_in_window(p, opened, words),
        drop_files: files -> import_dropped(p, files),
        empty: "No files yet: a project starts with its first file",
    ) {
        below: {
            explorer_footer(p)
        }
        doc: {
            if let e = opened {
                file_page(p, e, panel, goto)
            } else {
                ui.EmptyState("Choose a file to read it", icon: .file_text)
            }
        }
    }
}

/// What stands under the tree: the project's folder on this device and what it came to.
view explorer_footer(p: Project) {
    stack(gap: .related) {
        if p.trouble != "" {
            text(p.trouble, role: .caption, secondary: true)
        }
        text(
            if p.folder != "" { p.folder } else { "A project in the workspace alone" },
            role: .caption,
            secondary: true,
        )
    }
}

/// The open file's page: its name and state, its code, and the panel under it.
template view file_page(
    p: Project,
    e: Entry,
    panel: State<Text>,
    goto: State<Text>,
) {
    stack(gap: .related) {
        file_head(p, e, panel, goto)
        file_code(e)
        if not p.found.is_empty() {
            found_list(p, e)
        }
        if panel != "" {
            file_panel(p, e, panel)
        }
        status_line(p, e)
    }
}

/// The open file's code, coloured as the language it is written in: every language the editor
/// names in a file's extension has its own colours.
template view file_code(e: Entry) {
    ui.CodeEditor(
        bind: e.text,
        reveal: "{e.reveal_line + 1}:{e.reveal_column + 1}:{e.reveal_ask}",
        language: if e.language == "Rust" {
            .rust
        } else if e.language == "TOML" {
            .toml
        } else if e.language == "Markdown" {
            .markdown
        } else if e.language == "JSON" {
            .json
        } else if e.language == "YAML" {
            .yaml
        } else if e.language == "Python" {
            .python
        } else if e.language == "JavaScript" {
            .javascript
        } else if e.language == "TypeScript" {
            .typescript
        } else if e.language == "HTML" {
            .html
        } else if e.language == "CSS" {
            .css
        } else if e.language == "Shell" {
            .shell
        } else if e.language == "C" {
            .c
        } else if e.language == "C++" {
            .cpp
        } else if e.language == "Go" {
            .go
        } else if e.language == "Verilog" {
            .verilog
        } else if e.language == "SystemVerilog" { .system_verilog } else { .plain_text },
        min_lines: 18,
        placeholder: "Write the file",
    )
}

/// The file's own line: its name, whether it changed here, what it is written in, and what can
/// be done with it.
template view file_head(
    p: Project,
    e: Entry,
    panel: State<Text>,
    goto: State<Text>,
) {
    let dirty = e.text != e.saved
    row(gap: .related, align: .center, fill: 0.0, wrap: true) {
        ui.EditableLabel(bind: e.name)
        if dirty {
            ui.Badge("Unsaved", tone: .warning)
        }
        if e.stale {
            ui.Badge("Changed on disk", tone: .danger)
        }
        language_menu(e, e.language)
        ui.MenuButton("File", icon: .ellipsis, icon_only: true) {
            ui.MenuItem("Save", icon: .hard_drive, key: "Ctrl+S") {
                on choose: save(p, e)
            }
            ui.MenuItem("Save as…", icon: .hard_drive) {
                on choose: file.choose_folder(save_into(p, e), accept: ["text/plain"])
            }
            ui.MenuItem("Reload from disk", icon: .rotate_ccw) {
                on choose: reload(p, e)
            }
            ui.MenuItem("Format", icon: .wand_sparkles) {
                on choose: format_file(p, e)
            }
            ui.MenuItem("Duplicate line", icon: .copy) {
                on choose: duplicate_line(e, p.answer_line)
            }
            ui.MenuItem("Delete line", icon: .delete) {
                on choose: delete_line(e, p.answer_line)
            }
            ui.MenuItem("Toggle comment", icon: .slash) {
                on choose: toggle_comment(e, p.answer_line)
            }
            ui.MenuItem("Check the file", icon: .check) {
                on choose: check(p, e)
            }
            ui.MenuItem("Suggest", icon: .code) {
                on choose: complete(p, e, p.answer_line, p.answer_column)
            }
            ui.MenuItem("What stands here", icon: .info) {
                on choose: hover(p, e, p.answer_line, p.answer_column)
            }
            ui.MenuItem("Go to definition", icon: .locate_fixed) {
                on choose: definition(p, e, p.answer_line, p.answer_column)
            }
            for place in symbols_in(e) {
                ui.MenuItem(place.words, icon: .code) {
                    on choose: reveal_line(e, place.line, 0)
                }
            }
        }
    }
    row(gap: .related, align: .center) {
        // The window's search is the file's find: what it finds is replaced from here.
        ui.Button("Replace all", icon: .pencil) {
            on press: replace_in_file(e, p.search_text, "")
        }
        ui.TextInput(bind: goto, placeholder: "Line", min: 6rem)
        ui.Button("Go to line", icon: .arrow_right) {
            on press: reveal_line(e, (goto.to_int() ?? 1) - 1, 0)
        }
        ui.MenuButton("Panel", icon: .panel_bottom, icon_only: true) {
            ui.MenuItem("Problems", icon: .triangle_alert, checked: panel == "problems") {
                on choose: set(panel, "problems")
            }
            ui.MenuItem("Terminal", icon: .terminal, checked: panel == "terminal") {
                on choose: set(panel, "terminal")
            }
            ui.MenuItem("Search results", icon: .search, checked: panel == "search") {
                on choose: set(panel, "search")
            }
            if panel != "" {
                ui.MenuItem("Hide panel", icon: .panel_bottom) {
                    on choose: set(panel, "")
                }
            }
        }
    }
}

/// What the window's search found in the open file: each place, with the line it stands on.
view found_list(p: Project, e: Entry) {
    ui.Section("Found in this file") {
        for place in p.found {
            ui.ListRow(title: place.words, meta: "line {place.line + 1}", icon: .search) {
                on activate: reveal_line(e, place.line, place.column)
            }
        }
    }
}

/// The panel under the code: the file's problems, the project's commands, or what its search
/// found.
template view file_panel(p: Project, e: Entry, panel: State<Text>) {
    let named = if panel == "problems" {
        "Problems"
    } else if panel == "terminal" { "Terminal" } else { "Search results" }
    ui.Section(named) {
        if panel == "problems" {
            problems_panel(p, e)
        } else if panel == "terminal" {
            terminal_panel(p)
        } else {
            search_panel(p)
        }
    }
}

view problems_panel(p: Project, e: Entry) {
    if e.problems.is_empty() {
        ui.EmptyState(
            if e.checked == "" { "This file has not been checked yet" } else { problems_said(e) },
            icon: .check,
            inline: true,
        )
        ui.Button("Check the file", icon: .check) {
            on press: check(p, e)
        }
    } else {
        text("{problems_said(e)}, by {e.source}", role: .caption, secondary: true)
        for one in e.problems {
            ui.ListRow(
                title: one.message,
                meta: "line {one.line + 1}, column {one.column + 1}",
                icon: if one.severity == .error {
                    .ban
                } else if one.severity == .warning { .triangle_alert } else { .info },
                tone: if one.severity == .error {
                    .danger
                } else if one.severity == .warning { .warning } else { .neutral },
            )
        }
        if p.server_progress != "" {
            ui.StatusLine(p.server_progress, status: .active)
        }
    }
}

/// The terminal: a command line, the commands run and their output, and what a command can be
/// done with again.
view terminal_panel(p: Project) {
    state typed: Text = ""
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            ui.TextInput(bind: typed, placeholder: "Run a command in the project")
            ui.Button("Run", icon: .play) {
                on press: run(p, typed)
            }
            ui.Button("Stop", icon: .square) {
                on press: stop(p)
            }
            ui.Button("Clear", icon: .trash) {
                on press: clear_commands(p)
            }
        }
        if p.commands.is_empty() {
            ui.EmptyState("No command has run in the project", icon: .terminal, inline: true)
        } else {
            for c in p.commands {
                ui.DisclosureRow(title: "$ {c.command}", side: command_said(c)) {
                    ui.CodeEditor(
                        bind: c.output,
                        language: .plain_text,
                        read_only: true,
                        max_lines: 12,
                    )
                    row(gap: .related) {
                        ui.Button("Run again", icon: .repeat) {
                            on press: rerun(p, c)
                        }
                        for item in c.items.take(8) {
                            ui.Button(item.text, icon: .locate_fixed) {
                                on press: reveal_item(p, item.path, item.line)
                            }
                        }
                    }
                }
            }
        }
        ui.MenuButton("Terminal", icon: .ellipsis, icon_only: true) {
            ui.MenuItem("Open a terminal", icon: .terminal) {
                on choose: open_terminal(p)
            }
            ui.MenuItem("Clear the terminal", icon: .trash) {
                on choose: clear_commands(p)
            }
        }
    }
}

fn command_said(c: Command) -> Text {
    if c.state == "running" {
        "running"
    } else if c.state == "stopped" {
        "stopped"
    } else if c.exit == 0 { "done" } else { "exit {c.exit}" }
}

/// The project search: what the window's search found across the project's files, and how far
/// it got.
view search_panel(p: Project) {
    stack(gap: .related) {
        row(gap: .related, align: .center) {
            if p.search_text != "" {
                ui.Button("Stop", icon: .square) {
                    on press: stop_search(p)
                }
            }
        }
        text(search_said(p), role: .caption, secondary: true)
        for hit in p.search_hits.take(200) {
            ui.ListRow(title: hit.preview, meta: "{hit.rel}:{hit.line + 1}", icon: .search) {
                on activate: open_hit(p, hit.path, hit.line)
            }
        }
    }
}

/// The line under the code: what the file is, where the caret stands, and what is wrong.
template view status_line(p: Project, e: Entry) {
    row(gap: .related, align: .center, fill: 0.0, wrap: true) {
        if e.language != "" {
            text(e.language, role: .caption, secondary: true)
        }
        if e.text != e.saved {
            text("Unsaved changes", role: .caption, secondary: true)
        }
        if not e.problems.is_empty() {
            let count = if e.problems.len() == 1 {
                "1 problem"
            } else { "{e.problems.len()} problems" }
            text(count, role: .caption, secondary: true)
        }
        if p.server_state == "busy" and p.server_progress != "" {
            text(p.server_progress, role: .caption, secondary: true)
        }
        if p.server_trouble != "" {
            row(gap: .tight, align: .center) {
                text(p.server_trouble, role: .caption, secondary: true)
                ui.Button("Try again", icon: .rotate_cw) {
                    on press: restart_server(p)
                }
            }
        }
        text("Ln {p.answer_line + 1}, Col {p.answer_column + 1}", role: .caption, secondary: true)
    }
    if p.answer_what == "complete" and not p.answer_items.is_empty() {
        ui.Section("Suggestions") {
            for item in p.answer_items.take(8) {
                ui.ListRow(title: item.label, secondary: item.detail, icon: .code) {
                    on activate: accept_completion(p, e, item.label, item.insert)
                }
            }
        }
    }
    if p.answer_what == "hover" and p.answer_text != "" {
        ui.Section("What stands here") {
            text(p.answer_text)
        }
    }
    if p.answer_what == "definition" and not p.answer_locations.is_empty() {
        ui.Section("Definitions") {
            for place in p.answer_locations {
                ui.ListRow(title: place.path, meta: "line {place.line + 1}", icon: .locate_fixed)
            }
        }
    }
}

/// A file of the project on its own, as a place shows it: the code at the window's measure.
view file_view(e: Entry) {
    ui.NavShell(content_kind: .page, title: e.name) {
        content: {
            ui.Scroll {
                file_code(e)
            }
        }
    }
}

/// The project's files on their own, as a place shows them: each file a row, opening beside the
/// list.
view files(p: Project) {
    if p.files.is_empty() {
        ui.EmptyState("No files yet", icon: .file_code)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            on select(x: Entry): open(x, front: false)
            on activate(x: Entry): open(x)
            for e in p.files {
                ui.ListItem(item: e) {
                    ui.ListRow(
                        title: e.name,
                        meta: path_of(e),
                        secondary: if e.kind == .file { excerpt(e) } else { "" },
                        icon: if e.kind == .folder { .folder } else { .file_text },
                    )
                }
            }
        }
    }
}

fn excerpt(e: Entry) -> Text {
    if e.kind == .folder {
        ""
    } else {
        one_line(e.text).slice(0, one_line(e.text).len().min(80))
    }
}

/// Every command the project ran, as a place shows them: what each ran, where, and what it came
/// to.
view commands(p: Project) {
    if p.commands.is_empty() {
        ui.EmptyState("No command has run in the project", icon: .terminal)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true) {
            for c in p.commands {
                ui.ListItem(item: c) {
                    ui.ListRow(
                        title: "$ {c.command}",
                        secondary: one_line(c.output),
                        meta: command_said(c),
                        icon: .terminal,
                    )
                }
            }
        }
    }
}

/// A run of the project's terminal on its own: its command, where it ran, and its output.
view command_view(c: Command) {
    ui.NavShell(content_kind: .document, title: c.command) {
        content: {
            ui.Scroll {
                page {
                    stack(gap: .group) {
                        ui.Facts {
                            ui.Fact(c.cwd, label: "Ran in")
                            ui.Fact(command_said(c), label: "State")
                        }
                        ui.CodeEditor(
                            bind: c.output,
                            language: .plain_text,
                            read_only: true,
                            min_lines: 8,
                        )
                        for item in c.items {
                            ui.ListRow(title: item.text, meta: item.path, icon: .locate_fixed)
                        }
                    }
                }
            }
        }
    }
}
