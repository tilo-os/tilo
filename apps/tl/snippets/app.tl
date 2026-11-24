app so.tilo.snippets "Snippets" (
    icon: .square_pen,
    category: .development,
    tagline: "Text and code to paste again",
    keywords: ["snippets", "snippet", "textbausteine", "vorlagen", "templates", "code", "clipboard", "zwischenablage", "boilerplate", "schnipsel"],
    splash_budget: 250,
)

/// The kind of text a snippet is written in: its badge in the list and the colours its body is
/// shown with.
enum Language {
    /// Anything that is only words.
    text
    shell
    rust
    sql
    git
    python
    javascript
    json
    yaml
    markdown
    html
    css
    toml
    dockerfile
}

/// One reusable piece of text or code.
type Snippet (title: title, untitled: "Untitled snippet") {
    title: Text
    /// What is copied. A `$` and its braces around a name are a variable the person fills in
    /// when they copy it.
    body: Text
    /// What the snippet is for, shown above its body.
    description: Text = ""
    /// Starred: starred snippets stand at the top of every place.
    favorite: Bool = false
    /// The folder it is filed under; empty for none, which every place but its own lists.
    folder: Text = ""
    language: Language = .text
    /// What it is about, lower case and without `#`.
    tags: Set<Text>
    /// How often it was copied: what "Most used" reads. Only copying it changes it.
    uses: Int merge counter = 0

    search {
        filter "Favorites" = favorite (icon: .star)
        filter tags (icon: .tag)
        filter language (icon: .file_code)
        sort title a_z
    }
}

/// One variable a person is filling in, and what they typed for it.
type FillValue (title: name) {
    name: Text
    value: Text = ""
}

/// A person's filling of one snippet's variables. It is theirs alone, so two people filling the
/// same snippet never see each other's values.
type Fill (readers: author) {
    /// The snippet being filled; none while nothing is being filled.
    snippet: Option<Snippet> = .none
    values: Collection<FillValue>
}

/// The workspace's library: every snippet it keeps.
type Library {
    snippets: Collection<Snippet>
    /// Each person's filling here.
    fills: Collection<Fill>
}

root library: Library

default view snippets_window for Library

default view snippet_page for Snippet

/// Makes a snippet, filed under `folder` when it names one.
// ---------------------------------------------------------------- the snippets
primary action new_snippet(lib: Library, title: Text, folder: Text = "") -> Snippet (
    icon: .plus,
    label: "New snippet",
    did: "made a snippet",
) {
    require title.trim() != "" "A snippet needs a title"
    lib.snippets.insert(Snippet(title: title.trim(), folder: folder.trim()))
}

/// Makes a second copy of a snippet, right after it; its word count starts again from nothing.
action duplicate(lib: Library, snippet: Snippet, title: Text = "") -> Snippet (
    icon: .copy,
    label: "Duplicate",
    did: "duplicated a snippet",
) {
    let copy = lib.snippets.insert(
        Snippet(
            title: if title.trim() == "" { snippet.title } else { title.trim() },
            body: snippet.body,
            description: snippet.description,
            folder: snippet.folder,
            language: snippet.language,
        ),
    )
    for tag in snippet.tags {
        copy.tags.add(tag)
    }
    lib.snippets.move(copy, after: snippet)
    copy
}

/// Deletes a snippet from the library. Undo brings it back.
destructive action delete(lib: Library, snippet: Snippet) (
    icon: .trash_2,
    label: "Delete",
    did: "deleted a snippet",
) {
    lib.snippets.remove(snippet)
}

/// Stars a snippet, or takes the star back.
action star(snippet: Snippet, starred: Bool = not snippet.favorite) (
    icon: .star,
    label: if starred { "Remove star" } else { "Star it" },
    on: snippet.favorite,
) {
    snippet.favorite = starred
}

/// Gives a snippet the tags a person typed, comma-separated: each trimmed, lower case, without
/// `#`, at most 24 of them.
action set_tags(snippet: Snippet, tags: Text) (
    icon: .tag,
    label: "Edit tags…",
    did: "tagged a snippet",
) {
    let olds = snippet.tags.map(t -> t)
    for old in olds {
        snippet.tags.remove(old)
    }
    if tags.trim() == "" {
        return
    }
    let kept: List<Text> = tags
        .split(",")
        .map(t -> t.trim().replace("#", "").lower())
        .filter(t -> t != "")
        .take(24)
    for tag in kept {
        snippet.tags.add(tag)
    }
}

/// Takes one tag off one snippet.
action untag(snippet: Snippet, tag: Text) (label: "Remove the tag") {
    snippet.tags.remove(tag)
}

/// Renames a folder everywhere it is used; an empty new name files its snippets under nothing.
action rename_folder(lib: Library, folder: Text, to: Text) (
    icon: .pencil,
    label: "Rename folder",
    did: "renamed a folder",
) {
    require lib.snippets.any(s -> s.folder == folder) "There is no such folder"
    for snippet in lib.snippets.filter(s -> s.folder == folder) {
        snippet.folder = to.trim()
    }
}

/// Renames a tag everywhere it is used.
action rename_tag(lib: Library, tag: Text, to: Text) (
    icon: .pencil,
    label: "Rename tag",
    did: "renamed a tag",
) {
    require lib.snippets.any(s -> s.tags.contains(tag)) "There is no such tag"
    require to.trim() != "" "A tag needs a name"
    let renamed = to.trim().replace("#", "").lower()
    for snippet in lib.snippets.filter(s -> s.tags.contains(tag)) {
        snippet.tags.remove(tag)
        snippet.tags.add(renamed)
    }
}

/// Takes a tag off every snippet that carries it.
destructive action remove_tag(lib: Library, tag: Text) (
    icon: .trash_2,
    label: "Remove tag",
    did: "removed a tag",
) {
    require lib.snippets.any(s -> s.tags.contains(tag)) "There is no such tag"
    for snippet in lib.snippets.filter(s -> s.tags.contains(tag)) {
        snippet.tags.remove(tag)
    }
}

/// Copies a snippet's body to the clipboard, and counts the use. A snippet with variables is
/// filled in first (`begin_fill`), so what is copied is what the person meant.
action copy(snippet: Snippet) uses clipboard (icon: .copy, label: "Copy", did: "copied a snippet") {
    clipboard.copy(snippet.body)
    snippet.uses += 1
}

/// Starts filling in a snippet's variables: one field for each variable its body holds, emptied.
// ---------------------------------------------------------------- filling in variables
action begin_fill(lib: Library, snippet: Snippet) -> Fill (icon: .copy, label: "Fill in and copy") {
    let fill = lib.fills.first() ?? lib.fills.insert(Fill())
    fill.snippet = .some(snippet)
    for old in fill.values.map(v -> v) {
        fill.values.remove(old)
    }
    for name in placeholders(snippet.body) {
        fill.values.insert(FillValue(name: name))
    }
    fill
}

/// Copies the filled body, counts the use, and puts the filling away.
action copy_filled(lib: Library, fill: Fill) uses clipboard (
    icon: .copy,
    label: "Copy",
    did: "copied a snippet",
) {
    let snippet = fill.snippet ?? Snippet(title: "")
    clipboard.copy(expanded(snippet.body, fill))
    snippet.uses += 1
    clear_fill(fill)
}

/// Puts the filling away without copying.
action cancel_fill(fill: Fill) (label: "Cancel") {
    clear_fill(fill)
}

/// Empties a filling, so the sheet closes.
action clear_fill(fill: Fill) {
    fill.snippet = .none
    for old in fill.values.map(v -> v) {
        fill.values.remove(old)
    }
}

/// The variables a body holds (a `$` and its braces around a name), in the order they first
/// appear, each once.
fn placeholders(body: Text) -> List<Text> {
    names_in(body, 0, [])
}

fn names_in(body: Text, from: Int, out: List<Text>) -> List<Text> {
    if from >= body.len() {
        return out
    }
    match body.slice(from, body.len()).find("$\{") {
        .none => out,
        .some(offset) => {
            let start = from + offset + 2
            match body.slice(start, body.len()).find("\}") {
                .none => out,
                .some(length) => {
                    let name = body.slice(start, start + length).trim()
                    let next = start + length + 1
                    if name == "" or out.contains(name) {
                        names_in(body, next, out)
                    } else {
                        names_in(body, next, [out, [name]].flat_map(x -> x))
                    }
                }
            }
        }
    }
}

/// The body with every variable a value was typed for replaced; one left empty stays as it is,
/// so a person can copy the body and fill it in later.
fn expanded(body: Text, fill: Fill) -> Text {
    filled(body, 0, fill, "")
}

fn filled(body: Text, from: Int, fill: Fill, out: Text) -> Text {
    if from >= body.len() {
        return out
    }
    match body.slice(from, body.len()).find("$\{") {
        .none => out + body.slice(from, body.len()),
        .some(offset) => {
            let at = from + offset
            let start = at + 2
            let before = out + body.slice(from, at)
            match body.slice(start, body.len()).find("\}") {
                .none => before + body.slice(at, body.len()),
                .some(length) => {
                    let name = body.slice(start, start + length).trim()
                    let value = fill.values.find(v -> v.name == name)?.value ?? ""
                    let kept = if value == "" { body.slice(at, start + length + 1) } else { value }
                    filled(body, start + length + 1, fill, before + kept)
                }
            }
        }
    }
}

/// How a language is called in the list's badge.
// ---------------------------------------------------------------- words the list and page use
fn badge(language: Language) -> Text {
    if language == .text {
        "Text"
    } else if language == .shell {
        "Shell"
    } else if language == .rust {
        "Rust"
    } else if language == .sql {
        "SQL"
    } else if language == .git {
        "Git"
    } else if language == .python {
        "Python"
    } else if language == .javascript {
        "JS"
    } else if language == .json {
        "JSON"
    } else if language == .yaml {
        "YAML"
    } else if language == .markdown {
        "MD"
    } else if language == .html {
        "HTML"
    } else if language == .css { "CSS" } else if language == .toml { "TOML" } else { "Docker" }
}

/// The line under a snippet's title in the list: what it is for, or the first line of its body.
fn secondary(snippet: Snippet) -> Text {
    let said = snippet.description.trim()
    if said != "" {
        said
    } else {
        snippet.body.split("\n").first()?.trim() ?? ""
    }
}

/// How often a snippet was copied, as a sentence.
fn uses_words(n: Int) -> Text {
    if n == 1 { "Copied once" } else if n == 0 { "Never copied" } else { "Copied {n} times" }
}

/// The snippets the place `place` lists, starred first and then by title, or by how often they
/// were copied.
fn place_snippets(lib: Library, place: Text) -> List<Snippet> {
    let matched: List<Snippet> = if place == "favorites" {
        lib.snippets.filter(s -> s.favorite)
    } else if place == "most-used" {
        lib.snippets.filter(s -> s.uses > 0)
    } else if place.starts_with("folder:") {
        let name = place.slice(7, place.len())
        lib.snippets.filter(s -> s.folder == name)
    } else if place.starts_with("tag:") {
        let tag = place.slice(4, place.len())
        lib.snippets.filter(s -> s.tags.contains(tag))
    } else { lib.snippets.map(s -> s) }
    if place == "most-used" {
        matched.sort_by(s -> s.uses, desc)
    } else {
        let by_title = matched.sort_by(s -> s.title.lower())
        let starred = by_title.filter(s -> s.favorite)
        let rest = by_title.filter(s -> not s.favorite)
        [starred, rest].flat_map(s -> s)
    }
}

/// What the place is called.
fn place_label(place: Text) -> Text {
    if place == "favorites" {
        "Favorites"
    } else if place == "most-used" {
        "Most used"
    } else if place.starts_with("folder:") {
        place.slice(7, place.len())
    } else if place.starts_with("tag:") { place.slice(4, place.len()) } else { "All snippets" }
}

/// What the list says when the place holds nothing.
fn empty_words(place: Text) -> Text {
    if place == "favorites" {
        "No starred snippets"
    } else if place == "most-used" {
        "Nothing copied yet"
    } else if place.starts_with("tag:") {
        "No snippets with this tag"
    } else if place.starts_with("folder:") { "This folder is empty" } else { "No snippets yet" }
}

/// Every folder the library files snippets under, alphabetized.
fn folders(lib: Library) -> List<Text> {
    let seen: Map<Bool> = Map<Bool>()
    let out: List<Text> = []
    for snippet in lib.snippets {
        if snippet.folder != "" and seen.get(snippet.folder) == .none {
            seen.put(snippet.folder, true)
            out = [out, [snippet.folder]].flat_map(f -> f)
        }
    }
    out.sort_by(f -> f.lower())
}

/// Every tag the library's snippets carry, alphabetized.
fn tags(lib: Library) -> List<Text> {
    let seen: Map<Bool> = Map<Bool>()
    let out: List<Text> = []
    for snippet in lib.snippets {
        for tag in snippet.tags.sort_by(t -> t) {
            if seen.get(tag) == .none {
                seen.put(tag, true)
                out = [out, [tag]].flat_map(t -> t)
            }
        }
    }
    out
}

/// How many of a folder's or a tag's snippets stand.
fn in_folder(lib: Library, folder: Text) -> Int {
    lib.snippets.filter(s -> s.folder == folder).len()
}

fn in_tag(lib: Library, tag: Text) -> Int {
    lib.snippets.filter(s -> s.tags.contains(tag)).len()
}

/// The fill a window has open, as its snippet.
fn filling(fill: Option<Fill>) -> Option<Snippet> {
    fill?.snippet
}

/// One snippet's row: its language badge, its star, what it is for, and its variables.
// ---------------------------------------------------------------- the window
view snippet_row(snippet: Snippet) {
    let variables = placeholders(snippet.body).len()
    row(
        snippet.title,
        badge: badge(snippet.language),
        secondary: secondary(snippet),
        mark: if snippet.favorite { .star },
        meta: if variables > 0 {
            if variables == 1 { "1 variable" } else { "{variables} variables" }
        },
    )
}

/// The sidebar's places: the whole library, the starred snippets and the most copied, then the
/// folders and the tags, each with how many snippets it holds.
view places(lib: Library, place: Text, chosen: State<Text>) {
    ui.NavList(selected: place, title: "Snippets") {
        ui.NavItem("all", "All snippets", icon: .layers, count: lib.snippets.len()) {
            on select: set(chosen, "all")
        }
        ui.NavItem(
            "favorites",
            "Favorites",
            icon: .star,
            count: lib.snippets.count(s -> s.favorite),
        ) {
            on select: set(chosen, "favorites")
        }
        ui.NavItem(
            "most-used",
            "Most used",
            icon: .trending_up,
            count: lib.snippets.count(s -> s.uses > 0),
        ) {
            on select: set(chosen, "most-used")
        }
        if not folders(lib).is_empty() {
            ui.NavSection("Folders", add: new_snippet(lib)) {
                for folder in folders(lib) {
                    ui.NavItem(
                        "folder:{folder}",
                        folder,
                        icon: .folder,
                        count: in_folder(lib, folder),
                    ) {
                        on select: set(chosen, "folder:{folder}")
                    }
                }
            }
        }
        if not tags(lib).is_empty() {
            ui.NavSection("Tags") {
                for tag in tags(lib) {
                    ui.NavItem("tag:{tag}", tag, icon: .tag, count: in_tag(lib, tag)) {
                        on select: set(chosen, "tag:{tag}")
                    }
                }
            }
        }
    }
}

/// The snippets of the chosen place, each a row that shows its snippet beside the list.
view snippet_list(lib: Library, place: Text, shown: List<Snippet>, opened: State<Option<Snippet>>) {
    if shown.is_empty() {
        ui.EmptyState(
            empty_words(place),
            message: if lib.snippets.is_empty() {
                "Keep the text and code you paste often, with its own folders and tags."
            } else { "Nothing here yet." },
            icon: .square_pen,
            action: if lib.snippets.is_empty() { "New snippet" },
        ) {
            on act: new_snippet(lib)
        }
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true, narrow: space.compact) {
            on select(x: Snippet): set(opened, x)
            on activate(x: Snippet): open(x)
            for snippet in shown {
                ui.ListItem(item: snippet) {
                    snippet_row(snippet)
                }
            }
        }
    }
}

/// The open snippet's page: what it is, its language and folder, its tags and its body in the
/// colours of the language it is written in, with copying at the top.
view snippet_page(snippet: Snippet) {
    let lib = root Library
    let variables = placeholders(snippet.body).len()
    let has_variables = variables > 0
    page {
        stack(gap: .group) {
            row(gap: .tight, wrap: true) {
                ui.Button(
                    if snippet.favorite { "Remove star" } else { "Star it" },
                    icon: .star,
                    role: .plain,
                    toggled: snippet.favorite,
                ) {
                    on press: star(snippet)
                }
                ui.Button("Duplicate", icon: .copy, role: .plain) {
                    on press: duplicate(lib, snippet)
                }
                ui.Button("Delete", icon: .trash_2, role: .destructive) {
                    on press: delete(lib, snippet)
                }
                spacer()
                ui.Text(uses_words(snippet.uses), role: .caption, secondary: true)
            }
            ui.EditableLabel(
                bind: snippet.title,
                role: .large_title,
                title: true,
                placeholder: "Untitled snippet",
            )
            row(gap: .related, wrap: true, align: .center) {
                ui.EnumField(bind: snippet.language, label: "Language")
                ui.TextInput(bind: snippet.folder, placeholder: "No folder", min: 9rem)
            }
            row(gap: .tight, wrap: true, align: .center) {
                for tag in snippet.tags.sort_by(t -> t) {
                    ui.Chip(tag, icon: .tag) {
                        on remove: untag(snippet, tag)
                    }
                }
                ui.Button("Edit tags…", icon: .tag, role: .plain) {
                    on press: set_tags(snippet)
                }
            }
            ui.TextInput(bind: snippet.description, placeholder: "What it is for")
            row {
                ui.Button(
                    if has_variables { "Fill in and copy" } else { "Copy" },
                    icon: .copy,
                    role: .primary,
                ) {
                    on press: if has_variables { begin_fill(lib, snippet) } else { copy(snippet) }
                }
            }
            ui.CodeEditor(
                bind: snippet.body,
                gutter: true,
                min_lines: 8,
                max_lines: 24,
                language: if snippet.language == .shell {
                    // Each language a snippet is written in is coloured as itself: the editor knows
                    // every one of them, so no body reads as plain text.
                    .shell
                } else if snippet.language == .rust {
                    .rust
                } else if snippet.language == .sql {
                    .sql
                } else if snippet.language == .git {
                    .git
                } else if snippet.language == .python {
                    .python
                } else if snippet.language == .javascript {
                    .javascript
                } else if snippet.language == .json {
                    .json
                } else if snippet.language == .yaml {
                    .yaml
                } else if snippet.language == .markdown {
                    .markdown
                } else if snippet.language == .html {
                    .html
                } else if snippet.language == .css {
                    .css
                } else if snippet.language == .toml {
                    .toml
                } else if snippet.language == .dockerfile { .dockerfile } else { .plain_text },
            )
        }
    }
}

/// The sheet a person fills a snippet's variables in, with the body it will copy under it.
view fill_sheet(lib: Library, fill: Fill) {
    ui.ModalDialog(
        title: "Fill in and copy",
        description: "What each variable stands for",
        open: true,
        primary: "Copy",
        secondary: "Cancel",
    ) {
        on act_primary: copy_filled(lib, fill)
        on act_secondary: cancel_fill(fill)
        on dismiss: cancel_fill(fill)
        stack(gap: .related) {
            for value in fill.values {
                field(value.name) {
                    ui.TextInput(bind: value.value, placeholder: "")
                }
            }
            if let snippet = fill.snippet {
                ui.Text(expanded(snippet.body, fill), role: .code, lines: 8)
            }
        }
    }
}

/// The window: the places a person's snippets stand in, the snippets of one, and the snippet they
/// have open beside them.
view snippets_window(lib: Library) {
    state place: Text = "all"
    state opened: Option<Snippet> = .none
    // What the window's search holds: it narrows the chosen place's snippets.
    state found: Text = ""
    on show(x: Snippet): set(opened, x)
    let listed = place_snippets(lib, place)
    let shown = if found.is_empty() { listed } else { listed.matching(found) }
    let open_now = opened ?? shown.first()
    let fill = lib.fills.first()
    ui.NavShell(
        content_kind: .page,
        sidebar_title: "Snippets",
        title: place_label(place),
        overview_title: place_label(place),
        overview_count: shown.len(),
        nothing_listed: lib.snippets.is_empty(),
        actions: [new_snippet],
    ) {
        on search(q: Text): set(found, q)
        sidebar: {
            places(lib, place, place)
        }
        overview: {
            snippet_list(lib, place, shown, opened)
        }
        content: {
            if lib.snippets.is_empty() {
                ui.EmptyState(
                    "No snippets yet",
                    message: "Keep the text and code you paste often, with its own folders and tags.",
                    icon: .square_pen,
                    action: "New snippet",
                ) {
                    on act: new_snippet(lib)
                }
            } else if let snippet = open_now {
                snippet_page(snippet)
            } else {
                ui.EmptyState("Choose a snippet", icon: .square_pen)
            }
        }
    }
    if let sheet = fill {
        if filling(.some(sheet)) != .none {
            fill_sheet(lib, sheet)
        }
    }
}

/// The starter library: a folder of ops commands, a folder of writing, tags and stars, so a
/// capture shows the manager doing real work.
// ---------------------------------------------------------------- what a person starts with
scenario "a small library" -> Library {
    let lib = root Library
    let restart = lib.snippets.insert(
        Snippet(
            title: "Restart deployment",
            body: "kubectl -n $\{namespace\} rollout restart deployment/$\{deployment\}\nkubectl -n $\{namespace\} rollout status deployment/$\{deployment\} --timeout=120s",
            description: "Roll a Kubernetes deployment in a namespace.",
            favorite: true,
            folder: "Ops",
            language: .shell,
        ),
    )
    restart.uses += 12
    restart.tags.add("k8s")
    restart.tags.add("deploy")
    lib.snippets.insert(
        Snippet(
            title: "Tail container logs",
            body: "kubectl -n $\{namespace\} logs -f --tail=200 $\{pod\}",
            description: "Follow the last 200 log lines from a pod.",
            folder: "Ops",
            language: .shell,
        ),
    )
    lib.snippets.insert(
        Snippet(
            title: "Rebase onto main",
            body: "git fetch origin\ngit rebase origin/main",
            description: "Bring a branch up to date with main.",
            folder: "Git",
            language: .git,
        ),
    )
    lib.snippets.insert(
        Snippet(
            title: "Recent commits",
            body: "git log --graph --oneline --decorate -n $\{count\}",
            description: "Read the last commits as a graph.",
            folder: "Git",
            language: .git,
        ),
    )
    let sql = lib.snippets.insert(
        Snippet(
            title: "Table size",
            body: "SELECT table_name, pg_size_pretty(pg_total_relation_size(table_name)) AS size\nFROM information_schema.tables\nWHERE table_schema = 'public'\nORDER BY pg_total_relation_size(table_name) DESC;",
            description: "How much each table of a database takes.",
            folder: "SQL",
            language: .sql,
        ),
    )
    sql.uses += 3
    sql.tags.add("postgres")
    lib.snippets.insert(
        Snippet(
            title: "Meeting note",
            body: "# $\{topic\}\n\n**Date:** $\{date\}  \n**Attendees:** $\{attendees\}\n\n## Decisions\n- \n\n## Action items\n- [ ] $\{owner\}: ",
            description: "A reusable meeting-notes scaffold.",
            folder: "Writing",
            language: .markdown,
            favorite: true,
        ),
    )
    lib
}
