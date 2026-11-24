app so.tilo.diffmerge "Diff & Merge" (
    icon: .columns_2,
    category: .development,
    tagline: "Compares two texts, and merges three",
    keywords: ["diff", "merge", "compare", "comparison", "three-way", "three-way merge", "conflict", "patch", "diff tool", "vergleich", "zusammenführen", "konflikt"],
    splash_budget: 250,
    document: library.sessions,
)

/// One change of a comparison: the aligned rows it spans, and the first line they touch.
type Block {
    first: Int = 0
    last: Int = 0
    /// `added` (only the right has these lines), `removed` (only the left) or `modified`.
    kind: Text = "modified"
    preview: Text = ""
}

/// One change of a merge: the lines each side gives it, and how the person took it.
type Hunk {
    /// Its place in the merge's hunks, which is what `take_ours` and its siblings take.
    index: Int = 0
    /// `conflict` while the person has not said, `auto-ours` and `auto-theirs` for a side only
    /// one of them moved, and `ours`, `theirs`, `both` or `written` for what they said.
    resolution: Text = "conflict"
    base: Text = ""
    ours: Text = ""
    theirs: Text = ""
}

/// One file that differs between two folder trees.
type FolderEntry {
    path: Text = ""
    /// `added` (only the left tree has it), `removed` (only the right) or `modified`.
    kind: Text = "modified"
    left_len: Int = 0
    right_len: Int = 0
    /// The files were too large to compare by content, so only their lengths were compared.
    by_length: Bool = false
}

/// One comparison or merge: the texts a person opened, what the device made of them, and the
/// merged result everyone edits.
///
/// The sides, the merged result and the ways each change was taken are the workspace's: every
/// member sees the same comparison and can take the next side. Which of what is open a window
/// shows, the change it is at and what it typed for a change are each window's own.
type Session (title: name, untitled: "Untitled comparison") {
    name: Text = ""
    /// The two sides of a comparison, and the alignment the device made of them: one line per
    /// aligned row, an empty line where a side has none.
    left_label: Text = ""
    left: Text = ""
    right_label: Text = ""
    right: Text = ""
    left_aligned: Text = ""
    right_aligned: Text = ""
    /// One character per aligned row: ` ` equal, `a` added, `r` removed, `m` modified.
    kinds: Text = ""
    blocks: Collection<Block>
    added: Int = 0
    removed: Int = 0
    modified: Int = 0
    identical: Bool = false
    compared: Bool = false
    /// The three sides of a merge, and the merged result everyone edits.
    base_label: Text = ""
    base: Text = ""
    ours_label: Text = ""
    ours: Text = ""
    theirs_label: Text = ""
    theirs: Text = ""
    merged: Text = ""
    /// What the merged result was when the sides last composed it: a hand edit leaves it behind.
    composed: Text = ""
    hunks: Collection<Hunk>
    changes: Int = 0
    conflicts: Int = 0
    merging: Bool = false
    /// The two folders the person chose to compare, and the last comparison made of them.
    folder_choice_left: Text = ""
    folder_choice_right: Text = ""
    folder_left_label: Text = ""
    folder_right_label: Text = ""
    folder_left_root: Text = ""
    folder_right_root: Text = ""
    folder_entries: Collection<FolderEntry>
    folder_truncated: Bool = false
    folding: Bool = false
    /// Where the merged result was last written, and what it was then.
    saved_path: Text = ""
    saved_name: Text = ""
    saved: Text = ""
    /// What the window last said: a result, or what went wrong.
    notice: Text = ""
    notice_kind: Text = "info"
}

/// The workspace's Diff & Merge: the comparisons and merges its windows opened.
type Library {
    sessions: Collection<Session>
}

root library: Library

default view library for Library

default view session_window for Session

// ---------------------------------------------------------------- the device's answers
type OpenAsk {
    path: Text = ""
    address: Text = ""
    name: Text = ""
}

type OpenedAnswer {
    label: Text = ""
    text: Text = ""
}

type DiffAsk {
    left: Text
    right: Text
}

type DiffAnswer {
    /// One list per fact of a change, so a change is a place among them.
    block_firsts: List<Int>
    block_lasts: List<Int>
    block_kinds: List<Text>
    block_previews: List<Text>
    left: Text = ""
    right: Text = ""
    rows: Text = ""
    added: Int = 0
    removed: Int = 0
    modified: Int = 0
    identical: Bool = false
}

type MergeAsk {
    base: Text
    ours: Text
    theirs: Text
}

type MergeAnswer {
    /// One list per fact of a change, so a change is a place among them.
    hunk_indices: List<Int>
    hunk_resolutions: List<Text>
    hunk_bases: List<Text>
    hunk_ours: List<Text>
    hunk_theirs: List<Text>
    merged: Text = ""
    changes: Int = 0
    conflicts: Int = 0
}

type ComposeAsk {
    base: Text
    ours: Text
    theirs: Text
    choices: List<Text>
    written: List<Text>
}

type ComposedAnswer {
    merged: Text = ""
    changes: Int = 0
    conflicts: Int = 0
}

type FoldersAsk {
    left: Text
    right: Text
}

type FoldersAnswer {
    left_label: Text = ""
    left_root: Text = ""
    right_label: Text = ""
    right_root: Text = ""
    /// One list per fact of a file, so a file is a place among them.
    entry_paths: List<Text>
    entry_kinds: List<Text>
    entry_left_lens: List<Int>
    entry_right_lens: List<Int>
    entry_by_lengths: List<Bool>
    truncated: Bool = false
}

type SaveAsk {
    path: Text
    text: Text
}

type SavedAnswer {
    name: Text = ""
}

type SampleAsk {
    kind: Text
}

type SampleDiff {
    left_label: Text = ""
    left: Text = ""
    right_label: Text = ""
    right: Text = ""
}

type SampleMerge {
    base_label: Text = ""
    base: Text = ""
    ours_label: Text = ""
    ours: Text = ""
    theirs_label: Text = ""
    theirs: Text = ""
}

/// Starts an empty comparison: a place to open two files in.
// ---------------------------------------------------------------- opening what a person compares
action new_comparison(lib: Library) -> Session (
    icon: .columns_2,
    label: "New comparison",
    did: "started a comparison",
) {
    let made = lib.sessions.insert(Session(name: "Diff & Merge"))
    made
}

/// Compares two texts: what a script, an agent or another app asks of Diff & Merge.
action open_comparison(
    s: Session,
    left_label: Text,
    left: Text,
    right_label: Text,
    right: Text,
) -> Session (icon: .columns_2, label: "Compare two texts", did: "compared two texts") {
    s.left_label = left_label
    s.left = left
    s.right_label = right_label
    s.right = right
    s.compared = true
    if refresh_comparison(s) {
        s.name = comparison_title(s)
    }
    s
}

/// Opens the two files at `left` and `right` on this device and compares them. Two absolute
/// paths, or two empty ones to choose the files at the window.
action open_files(lib: Library, left: Text = "", right: Text = "") -> Session (
    icon: .file_plus,
    label: "Open files",
    did: "opened two files",
) {
    require not (left == "" and right != "") "the left and right paths are given together, or not at all"
    require not (right == "" and left != "") "the left and right paths are given together, or not at all"
    let made = lib.sessions.insert(Session(name: "Diff & Merge"))
    if left == "" {} else {
        match read_side(left, "", "") {
            .ok(l) => match read_side(right, "", "") {
                .ok(r) => {
                    made.left_label = l.label
                    made.left = l.text
                    made.right_label = r.label
                    made.right = r.text
                    made.compared = true
                    if refresh_comparison(made) {
                        made.name = comparison_title(made)
                    }
                }
                .err(why) => refuse(made, why),
            },
            .err(why) => refuse(made, why),
        }
    }
    made
}

/// Opens one file on this device against nothing: what a folder comparison shows for a file only
/// one tree holds. `side` is `left` or `right`.
action open_one(lib: Library, path: Text, side: Text = "left") -> Session (
    icon: .file_plus,
    label: "Open one file",
    did: "opened a file",
) {
    let made = lib.sessions.insert(Session(name: "Diff & Merge"))
    match read_side(path, "", "") {
        .ok(one) => {
            if side == "right" {
                made.left_label = "(no such file)"
                made.right_label = one.label
                made.right = one.text
            } else {
                made.left_label = one.label
                made.left = one.text
                made.right_label = "(no such file)"
            }
            made.compared = true
            if refresh_comparison(made) {
                made.name = comparison_title(made)
            }
        }
        .err(why) => refuse(made, why),
    }
    made
}

/// The action the person's file chooser fills for the left side: the left file, and then the
/// right one.
action chosen_left(s: Session, file: File) -> Session (
    icon: .file_plus,
    label: "Choose a file to compare",
) {
    match read_side("", file.url, file.name) {
        .ok(one) => {
            s.left_label = one.label
            s.left = one.text
            if meaningful(s.right_label) {
                s.compared = true
                refresh_comparison(s)
            } else {
                s.notice = "Now choose the right file."
                s.notice_kind = "info"
            }
        }
        .err(why) => refuse(s, why),
    }
    s
}

/// The action the person's file chooser fills for the right side.
action chosen_right(s: Session, file: File) -> Session (
    icon: .file_plus,
    label: "Choose a file to compare",
) {
    match read_side("", file.url, file.name) {
        .ok(one) => {
            s.right_label = one.label
            s.right = one.text
            s.compared = true
            if refresh_comparison(s) {
                s.name = comparison_title(s)
            }
        }
        .err(why) => refuse(s, why),
    }
    s
}

fn meaningful(label: Text) -> Bool {
    label != "" and label != "(no such file)"
}

/// Reads the file at `path`, or the bytes at `address` when a chooser handed them over: its name
/// and its text.
fn read_side(path: Text, address: Text, name: Text) -> Result<OpenedAnswer, Text> {
    match engine.call("open", json.encode(OpenAsk(path: path, address: address, name: name))) {
        .ok(answer) => match json.decode<OpenedAnswer>(answer) {
            .ok(opened) => .ok(opened),
            .err(why) => .err(why),
        },
        .err(why) => .err(why),
    }
}

/// Asks the device to align the two sides, and holds what it made: the aligned columns, the rows
/// that changed and how many of each.
action refresh_comparison(s: Session) -> Bool {
    match engine.call("diff", json.encode(DiffAsk(left: s.left, right: s.right))) {
        .ok(answer) => match json.decode<DiffAnswer>(answer) {
            .ok(diff) => {
                s.left_aligned = diff.left
                s.right_aligned = diff.right
                s.kinds = diff.rows
                s.added = diff.added
                s.removed = diff.removed
                s.modified = diff.modified
                s.identical = diff.identical
                for held in s.blocks {
                    s.blocks.remove(held)
                }
                let found = diff.block_firsts.map(
                    (first, i) -> Block(
                        first: first,
                        last: diff.block_lasts.at(i) ?? first,
                        kind: diff.block_kinds.at(i) ?? "modified",
                        preview: diff.block_previews.at(i) ?? "",
                    ),
                )
                for block in found {
                    s.blocks.insert(block)
                }
                s.notice = ""
                s.notice_kind = "info"
                true
            }
            .err(why) => {
                refuse(s, why)
                false
            }
        },
        .err(why) => {
            refuse(s, why)
            false
        }
    }
}

/// Merges three texts: a common base and the two versions of it a person has.
// ---------------------------------------------------------------- merging three
action open_merge(
    s: Session,
    base_label: Text,
    base: Text,
    ours_label: Text,
    ours: Text,
    theirs_label: Text,
    theirs: Text,
) -> Session (icon: .arrow_up_down, label: "Merge three texts", did: "opened a merge") {
    s.base_label = base_label
    s.base = base
    s.ours_label = ours_label
    s.ours = ours
    s.theirs_label = theirs_label
    s.theirs = theirs
    s.merging = true
    if refresh_merge(s) {
        s.name = merge_title(s)
    }
    s
}

/// Asks the device to hunk the merge and compose the result with what the person took so far.
action refresh_merge(s: Session) -> Bool {
    match engine.call("merge", json.encode(MergeAsk(base: s.base, ours: s.ours, theirs: s.theirs))) {
        .ok(answer) => match json.decode<MergeAnswer>(answer) {
            .ok(made) => {
                for held in s.hunks {
                    s.hunks.remove(held)
                }
                let found = made.hunk_indices.map(
                    (i, at) -> Hunk(
                        index: i,
                        resolution: made.hunk_resolutions.at(at) ?? "conflict",
                        base: made.hunk_bases.at(at) ?? "",
                        ours: made.hunk_ours.at(at) ?? "",
                        theirs: made.hunk_theirs.at(at) ?? "",
                    ),
                )
                for hunk in found {
                    s.hunks.insert(hunk)
                }
                s.merged = made.merged
                s.composed = made.merged
                s.changes = made.changes
                s.conflicts = made.conflicts
                s.saved = ""
                s.notice = ""
                s.notice_kind = "info"
                true
            }
            .err(why) => {
                refuse(s, why)
                false
            }
        },
        .err(why) => {
            refuse(s, why)
            false
        }
    }
}

/// The choices and the written texts the person made, as the device composes them.
fn composed_with(s: Session) -> Result<ComposedAnswer, Text> {
    let choices = s.hunks
        .filter(
            h -> h.resolution == "ours" or h.resolution == "theirs" or h.resolution == "both",
        )
        .map(h -> "{h.index}:{h.resolution}")
    let written = s.hunks.filter(h -> h.resolution == "written").map(h -> "{h.index}:{h.base}")
    let ask = ComposeAsk(
        base: s.base,
        ours: s.ours,
        theirs: s.theirs,
        choices: choices,
        written: written,
    )
    match engine.call("compose", json.encode(ask)) {
        .ok(answer) => match json.decode<ComposedAnswer>(answer) {
            .ok(composed) => .ok(composed),
            .err(why) => .err(why),
        },
        .err(why) => .err(why),
    }
}

/// Takes our side for a change.
action take_ours(s: Session, hunk: Int) (
    icon: .arrow_left,
    label: "Take ours",
    did: "took our side of a change",
) {
    take(s, hunk, "ours")
}

/// Takes their side for a change.
action take_theirs(s: Session, hunk: Int) (
    icon: .arrow_right,
    label: "Take theirs",
    did: "took their side of a change",
) {
    take(s, hunk, "theirs")
}

/// Takes both sides for a change, ours first.
action take_both(s: Session, hunk: Int) (
    icon: .arrow_up_down,
    label: "Take both",
    did: "took both sides of a change",
) {
    take(s, hunk, "both")
}

/// Resolves a change with the person's own text.
action resolve(s: Session, hunk: Int, text: Text) (
    icon: .pencil,
    label: "Resolve with my own text",
    did: "resolved a change by hand",
) {
    require s.merged == s.composed "the merged result was edited by hand; resolve the rest in it"
    match s.hunks.find(h -> h.index == hunk) {
        .some(found) => {
            found.resolution = "written"
            found.base = text
            compose_into(s)
        }
        .none => refuse(s, "the merge has no change {hunk + 1}"),
    }
}

action take(s: Session, hunk: Int, resolution: Text) -> Bool {
    if not composed_so_far(s) {
        return refuse(s, "the merged result was edited by hand; resolve the rest in it")
    }
    match s.hunks.find(h -> h.index == hunk) {
        .some(found) => {
            found.resolution = resolution
            compose_into(s)
        }
        .none => refuse(s, "the merge has no change {hunk + 1}"),
    }
    true
}

/// Whether the merged result is still what the sides compose: once someone edits it by hand,
/// taking a side would throw the edit away, so the rest is resolved in the text.
fn composed_so_far(s: Session) -> Bool {
    s.merging and s.merged == s.composed
}

action compose_into(s: Session) -> Bool {
    match composed_with(s) {
        .ok(composed) => {
            s.merged = composed.merged
            s.composed = composed.merged
            s.changes = composed.changes
            s.conflicts = composed.conflicts
            s.notice = ""
            s.notice_kind = "info"
        }
        .err(why) => refuse(s, why),
    }
    true
}

/// Compares the two folders at `left` and `right` on this device: what only one holds, and what
/// both hold but differ in. Two absolute paths, or two empty ones to choose the folders at the
/// window.
// ---------------------------------------------------------------- comparing folders
action compare_folders(s: Session, left: Text = "", right: Text = "") (
    icon: .folder,
    label: "Compare folders",
    did: "compared two folders",
) {
    require not (left == "" and right != "") "the left and right folder paths are given together, or not at all"
    require not (right == "" and left != "") "the left and right folder paths are given together, or not at all"
    require left != "" "choose the two folders at the window"
    match engine.call("compare-folders", json.encode(FoldersAsk(left: left, right: right))) {
        .ok(answer) => { hold_folders(s, answer) }
        .err(why) => refuse(s, why),
    }
}

action hold_folders(s: Session, answer: Text) -> Bool {
    match json.decode<FoldersAnswer>(answer) {
        .ok(folders) => {
            s.folder_left_label = folders.left_label
            s.folder_right_label = folders.right_label
            s.folder_left_root = folders.left_root
            s.folder_right_root = folders.right_root
            s.folder_truncated = folders.truncated
            for held in s.folder_entries {
                s.folder_entries.remove(held)
            }
            let found = folders.entry_paths.map(
                (path, i) -> FolderEntry(
                    path: path,
                    kind: folders.entry_kinds.at(i) ?? "modified",
                    left_len: folders.entry_left_lens.at(i) ?? 0,
                    right_len: folders.entry_right_lens.at(i) ?? 0,
                    by_length: folders.entry_by_lengths.at(i) ?? false,
                ),
            )
            for entry in found {
                s.folder_entries.insert(entry)
            }
            s.folding = true
            s.notice = ""
            s.notice_kind = "info"
            true
        }
        .err(why) => refuse(s, why),
    }
}

/// The action the person's folder chooser fills for the left tree.
action chosen_folder_left(s: Session, folder: Text) {
    s.folder_choice_left = folder
    s.notice = "Now choose the right folder."
    s.notice_kind = "info"
}

/// The action the person's folder chooser fills for the right tree: both chosen, the folders are
/// compared.
action chosen_folder_right(s: Session, folder: Text) {
    s.folder_choice_right = folder
    if s.folder_choice_left == "" {
        s.notice = "Choose the left folder first."
        s.notice_kind = "info"
    } else {
        compare_folders(s, s.folder_choice_left, folder)
    }
}

/// Opens one file of the folder comparison: both its sides when both trees hold it, else the one
/// that does, against nothing.
action open_folder_entry(s: Session, entry: FolderEntry) -> Session (
    icon: .file_plus,
    label: "Open the file",
    did: "opened a file a folder comparison found",
) {
    if entry.kind == "added" {
        open_one(library, join(s.folder_left_root, entry.path), "left")
    } else if entry.kind == "removed" {
        open_one(library, join(s.folder_right_root, entry.path), "right")
    } else {
        open_files(
            library,
            join(s.folder_left_root, entry.path),
            join(s.folder_right_root, entry.path),
        )
    }
}

/// Opens the file of the row the person pressed, for the key that asked: Enter opens it.
action open_on_key(s: Session, entry: FolderEntry, key: Text) -> Session {
    if key == "Enter" { open_folder_entry(s, entry) } else { s }
}

fn join(root: Text, path: Text) -> Text {
    if root.ends_with("/") { "{root}{path}" } else { "{root}/{path}" }
}

/// Replaces the merged result: what a person types into it, and what an agent or a script writes
/// when the result is theirs to change. Once it is not what the sides compose, taking a side
/// would throw the edit away, so the rest is resolved in the text.
// ---------------------------------------------------------------- the merged result
action edit_merged(s: Session, text: Text) (
    icon: .pencil,
    label: "Write the merged result",
    did: "wrote the merged result",
) {
    require s.merging "no merge is open, so there is no merged result to write"
    s.merged = text
}

/// Writes the merged result to `path` on this device.
action save_merged(s: Session, path: Text) (
    icon: .save,
    label: "Save merged result",
    did: "saved the merged result",
    key: "Ctrl+S",
) {
    require s.merging "no merge is open, so there is no merged result to save"
    require path != "" "the merged result is saved to a path, or through the file chooser"
    match engine.call("save", json.encode(SaveAsk(path: path, text: s.merged))) {
        .ok(answer) => match json.decode<SavedAnswer>(answer) {
            .ok(saved) => {
                s.saved_path = path
                s.saved_name = saved.name
                s.saved = s.merged
                s.notice = "Saved to {saved.name}"
                s.notice_kind = "success"
            }
            .err(why) => refuse(s, why),
        },
        .err(why) => refuse(s, why),
    }
}

/// The merged result as a file the person places with the system's save chooser, which the window
/// shows.
action save_bytes(s: Session) -> File {
    File.of_text(save_name(s), s.merged)
}

fn save_name(s: Session) -> Text {
    let named = file_name(s.ours_label)
    if named == "" { "merged.txt" } else { named }
}

/// Opens the sample comparison and the sample merge, so a person sees how each reads before they
/// have two files at hand.
// ---------------------------------------------------------------- samples
action load_sample(lib: Library) -> Session (
    icon: .sparkles,
    label: "Load sample",
    did: "opened the sample",
) {
    let made = lib.sessions.insert(Session(name: "Diff & Merge"))
    fill_sample(made)
    made
}

/// Fills a comparison with the samples: the two-way comparison and the three-way merge, so a
/// person sees how each reads before they have two files at hand.
action fill_sample(s: Session) (
    icon: .sparkles,
    label: "Load the sample comparison",
    did: "opened the sample",
) {
    let made = s
    match engine.call("sample", json.encode(SampleAsk(kind: "diff"))) {
        .ok(answer) => match json.decode<SampleDiff>(answer) {
            .ok(sample) => {
                made.left_label = sample.left_label
                made.left = sample.left
                made.right_label = sample.right_label
                made.right = sample.right
                made.compared = true
                refresh_comparison(made)
            }
            .err(why) => refuse(made, why),
        },
        .err(why) => refuse(made, why),
    }
    match engine.call("sample", json.encode(SampleAsk(kind: "merge"))) {
        .ok(answer) => match json.decode<SampleMerge>(answer) {
            .ok(sample) => {
                made.base_label = sample.base_label
                made.base = sample.base
                made.ours_label = sample.ours_label
                made.ours = sample.ours
                made.theirs_label = sample.theirs_label
                made.theirs = sample.theirs
                made.merging = true
                refresh_merge(made)
            }
            .err(why) => refuse(made, why),
        },
        .err(why) => refuse(made, why),
    }
    made.name = merge_title(made)
    made
}

/// Shows the sample folder comparison: no disk is read for it.
action sample_folders(s: Session) (icon: .folder, label: "Show the sample folders") {
    match engine.call("sample", json.encode(SampleAsk(kind: "folders"))) {
        .ok(answer) => { hold_folders(s, answer) }
        .err(why) => refuse(s, why),
    }
}

// ---------------------------------------------------------------- titles and words
fn comparison_title(s: Session) -> Text {
    let left = file_name(s.left_label)
    let right = file_name(s.right_label)
    if left == "" and right == "" {
        "Comparison"
    } else if left == "" {
        "{right}, against nothing"
    } else if right == "" {
        "{left}, against nothing"
    } else if left == right { "{left}" } else { "{left} and {right}" }
}

fn merge_title(s: Session) -> Text {
    let ours = file_name(s.ours_label)
    let theirs = file_name(s.theirs_label)
    if ours == "" and theirs == "" {
        "Merged result"
    } else if ours == theirs { "Merge of {ours}" } else { "Merge of {ours} and {theirs}" }
}

/// The file name a side's label shows, without what it was chosen from.
fn file_name(label: Text) -> Text {
    let trimmed = label.trim()
    if let cut = trimmed.find(" (") { trimmed.slice(0, cut) } else { trimmed }
}

action refuse(s: Session, why: Text) -> Bool {
    s.notice = why
    s.notice_kind = "warning"
    false
}

fn count(n: Int) -> Text {
    "{n}"
}

fn size(bytes: Int) -> Text {
    if bytes >= 1048576 {
        "{bytes / 1048576} MB"
    } else if bytes >= 1024 { "{bytes / 1024} KB" } else { "{bytes} B" }
}

fn kind_word(kind: Text) -> Text {
    if kind == "added" { "Added" } else if kind == "removed" { "Removed" } else { "Modified" }
}

fn resolution_word(resolution: Text) -> Text {
    if resolution == "conflict" {
        "Conflict"
    } else if resolution == "auto-ours" {
        "Auto-resolved: ours"
    } else if resolution == "auto-theirs" {
        "Auto-resolved: theirs"
    } else if resolution == "ours" {
        "Resolved: ours"
    } else if resolution == "theirs" {
        "Resolved: theirs"
    } else if resolution == "both" {
        "Resolved: both"
    } else if resolution == "written" { "Resolved: my own text" } else { "Unchanged" }
}

fn change_for_row(s: Session, row: Int) -> Int {
    if let found = s.blocks.find(b -> row >= b.first and row <= b.last) {
        s.blocks.index_of(found) ?? 0
    } else { 0 }
}

/// The change of the merge `step` after the one at `at`, among the changes it lists: a change's
/// index counts the runs the sides already agree on, so the next place is read from the list.
fn hunk_step(s: Session, at: Int, step: Int) -> Int {
    let place = match s.hunks.find(h -> h.index == at) {
        .some(found) => s.hunks.index_of(found) ?? 0,
        .none => 0,
    }
    match s.hunks.at((place + step).clamp(0, s.hunks.len() - 1)) {
        .some(there) => there.index,
        .none => at,
    }
}

/// The language a side's file name gives it, as the toolkit names it: every language the shared
/// highlighter knows, by extension, and a `Dockerfile` by its name.
fn language_word(label: Text) -> Text {
    let lower = file_name(label).lower()
    let name = lower.split("/").last() ?? lower
    if name == "dockerfile" or name.starts_with("dockerfile.") {
        "dockerfile"
    } else if lower.ends_with(".rs") {
        "rust"
    } else if lower.ends_with(".typ") {
        "typst"
    } else if lower.ends_with(".sql") {
        "sql"
    } else if lower.ends_with(".json") {
        "json"
    } else if lower.ends_with(".sh")
        or lower.ends_with(".bash")
        or lower.ends_with(".zsh")
        or lower.ends_with(".fish") {
        "shell"
    } else if name == ".gitignore"
        or name == ".gitattributes"
        or name == ".gitmodules"
        or name == ".gitconfig" {
        "git"
    } else if lower.ends_with(".py") {
        "python"
    } else if lower.ends_with(".js")
        or lower.ends_with(".mjs")
        or lower.ends_with(".cjs")
        or lower.ends_with(".jsx") {
        "javascript"
    } else if lower.ends_with(".ts")
        or lower.ends_with(".tsx")
        or lower.ends_with(".mts")
        or lower.ends_with(".cts") {
        "typescript"
    } else if lower.ends_with(".yaml") or lower.ends_with(".yml") {
        "yaml"
    } else if lower.ends_with(".md") or lower.ends_with(".markdown") {
        "markdown"
    } else if lower.ends_with(".html") or lower.ends_with(".htm") {
        "html"
    } else if lower.ends_with(".css") {
        "css"
    } else if lower.ends_with(".toml") {
        "toml"
    } else if lower.ends_with(".c") or lower.ends_with(".h") {
        "c"
    } else if lower.ends_with(".cpp")
        or lower.ends_with(".cc")
        or lower.ends_with(".cxx")
        or lower.ends_with(".hpp")
        or lower.ends_with(".hxx")
        or lower.ends_with(".hh") {
        "cpp"
    } else if lower.ends_with(".go") {
        "go"
    } else if lower.ends_with(".v") or lower.ends_with(".vh") {
        "verilog"
    } else if lower.ends_with(".sv") or lower.ends_with(".svh") {
        "system_verilog"
    } else if lower.ends_with(".tl") { "tilolang" } else { "plain_text" }
}

fn block_first(s: Session, at: Int) -> Int {
    match s.blocks.at(at) {
        .some(b) => b.first,
        .none => 0,
    }
}

/// The library: the comparisons this workspace has open.
// ---------------------------------------------------------------- the window
view library(lib: Library) {
    ui.NavShell(content_kind: .list, title: "Diff & Merge") {
        actions: {
            row(gap: .related, align: .center) {
                ui.Button("Compare files", icon: .file_plus, role: .primary) {
                    on press: new_comparison(lib)
                }
                ui.MenuButton("More", icon: .ellipsis, icon_only: true) {
                    ui.MenuItem("Load sample", icon: .sparkles) {
                        on choose: load_sample(lib)
                    }
                }
            }
        }
        content: {
            if lib.sessions.is_empty() {
                ui.EmptyState(
                    "Open two files to compare them",
                    icon: .columns_2,
                    message: "Start a comparison, then choose the two files in its window; or load a sample to see how one reads.",
                    action: "Compare files",
                ) {
                    on act: new_comparison(lib)
                }
            } else {
                ui.VirtualList(name: "Comparisons", in_column: true) {
                    on activate(s: Session): open(s)
                    for s in lib.sessions {
                        ui.ListItem(item: s) {
                            menu: {
                                ui.MenuItem("Close", icon: .x) {
                                    on choose: forget(lib, s)
                                }
                            }
                            row: {
                                ui.ListRow(
                                    title: if s.name == "" {
                                        "Untitled comparison"
                                    } else { s.name },
                                    secondary: secondary_of(s),
                                    icon: .columns_2,
                                    value: if s.merging {
                                        "Merge"
                                    } else if s.folding {
                                        "Folders"
                                    } else if s.compared { "Comparison" } else { "" },
                                )
                            }
                        }
                    }
                }
            }
        }
    }
}

fn secondary_of(s: Session) -> Text {
    if s.merging {
        "{count(s.changes - s.conflicts)} of {count(s.changes)} changes resolved"
    } else if s.folding {
        "{count(s.folder_entries.len())} files differ"
    } else if s.compared {
        if s.identical {
            "The files are identical"
        } else {
            "{count(s.added)} added, {count(s.removed)} removed, {count(s.modified)} modified"
        }
    } else { "Nothing open" }
}

destructive action forget(lib: Library, s: Session) (icon: .x, label: "Close the comparison") {
    lib.sessions.remove(s)
}

/// One comparison's window: the sides, the merge or the folder list, and what is under them.
view session_window(s: Session) {
    // Which of what is open the window shows, the change it is at, and which side of a narrow
    // window it reads.
    state mode: Text = if s.merging { "merge" } else if s.folding { "folders" } else { "diff" }
    state at: Int = 0
    // The change of the merge the person is at: what taking a side decides.
    state at_hunk: Int = s.hunks.first()?.index ?? 0
    state side: Text = "ours"
    // The change whose own text is being written, and what was typed.
    state writing: Int = -1
    state typed: Text = ""
    ui.NavShell(
        content_kind: .page,
        title: if s.name == "" { "Untitled comparison" } else { s.name },
    ) {
        actions: {
            toolbar(s, mode, at, at_hunk)
        }
        content: {
            stack(gap: .related, fill: 1.0) {
                if s.notice != "" {
                    ui.Banner(
                        s.notice,
                        kind: if s.notice_kind == "warning" {
                            .warning
                        } else if s.notice_kind == "success" { .success } else { .neutral },
                        icon: if s.notice_kind == "warning" {
                            .triangle_alert
                        } else if s.notice_kind == "success" { .check } else { .info },
                        action: "Dismiss",
                    ) {
                        on act: clear_notice(s)
                    }
                }
                if mode == "folders" {
                    folders_page(s)
                } else if mode == "merge" {
                    merge_page(s, side, writing, typed, at_hunk)
                } else {
                    comparison_page(s, at, side)
                }
            }
        }
        band: {
            status_line(s, mode, at)
        }
    }
}

action clear_notice(s: Session) {
    s.notice = ""
}

view toolbar(s: Session, mode: State<Text>, at: State<Int>, at_hunk: State<Int>) {
    row(gap: .related, align: .center) {
        ui.ToggleGroup(
            selected: if mode == "merge" { 1 } else if mode == "folders" { 2 } else { 0 },
        ) {
            ui.Segment("Diff")
            ui.Segment("Merge")
            ui.Segment("Folders")
            on change(i): set(
                mode,
                if i == 1 { "merge" } else if i == 2 { "folders" } else { "diff" },
            )
        }
        if mode == "diff" {
            ui.Button("Previous change", icon: .arrow_up, icon_only: true, key: "F7") {
                on press: set(at, (at - 1).clamp(0, s.blocks.len() - 1))
            }
            ui.Button("Next change", icon: .arrow_down, icon_only: true, key: "F8") {
                on press: set(at, (at + 1).clamp(0, s.blocks.len() - 1))
            }
        }
        ui.MenuButton("File", icon: .folder_open) {
            ui.MenuItem("Compare files…", icon: .file_plus, key: "Ctrl+O") {
                on choose: file.choose(chosen_left(s))
            }
            ui.MenuItem("Choose the right file…", icon: .file_plus) {
                on choose: file.choose(chosen_right(s))
            }
            ui.MenuItem("Compare folders…", icon: .folder) {
                on choose: file.choose_folder(chosen_folder_left(s))
            }
            if mode == "merge" {
                ui.MenuSeparator()
                ui.MenuItem("Save as…", icon: .hard_drive, key: "Ctrl+Shift+S") {
                    on choose: file.save(save_bytes(s), name: save_name(s))
                }
            }
            ui.MenuSeparator()
            ui.MenuItem("Load sample", icon: .sparkles) {
                on choose: load_sample(library)
            }
        }
        ui.MenuButton("View", icon: .eye) {
            ui.MenuItem("Diff", icon: .columns_2, key: "1") {
                on choose: set(mode, "diff")
            }
            ui.MenuItem("Merge", icon: .arrow_up_down, key: "2") {
                on choose: set(mode, "merge")
            }
            ui.MenuItem("Folders", icon: .folder, key: "f") {
                on choose: set(mode, if mode == "folders" { "diff" } else { "folders" })
            }
            if mode == "merge" and not s.hunks.is_empty() {
                ui.MenuSeparator()
                ui.MenuItem("Previous change", icon: .arrow_up, key: "p") {
                    on choose: set(at_hunk, hunk_step(s, at_hunk, -1))
                }
                ui.MenuItem("Next change", icon: .arrow_down, key: "n") {
                    on choose: set(at_hunk, hunk_step(s, at_hunk, 1))
                }
            } else if not s.blocks.is_empty() {
                ui.MenuSeparator()
                ui.MenuItem("Previous change", icon: .arrow_up, key: "p") {
                    on choose: set(at, (at - 1).clamp(0, s.blocks.len() - 1))
                }
                ui.MenuItem("Next change", icon: .arrow_down, key: "n") {
                    on choose: set(at, (at + 1).clamp(0, s.blocks.len() - 1))
                }
            }
        }
        if mode == "merge" {
            ui.Button(
                if s.saved_path == "" { "Save…" } else { "Save to {s.saved_name}" },
                icon: .save,
                role: .primary,
                key: "Ctrl+S",
            ) {
                on press: if s.saved_path == "" {
                    file.save(save_bytes(s), name: save_name(s))
                } else { save_merged(s, s.saved_path) }
            }
        }
    }
}

/// The two-way comparison: the aligned grid, and the list of changes beside it.
view comparison_page(s: Session, at: State<Int>, side: State<Text>) {
    if not s.compared {
        stack(gap: .related) {
            ui.EmptyState(
                "Open two files to compare them",
                icon: .columns_2,
                message: "Choose the left file, then the right one, or load a sample to see how a comparison reads.",
                action: "Choose the left file",
            ) {
                on act: file.choose(chosen_left(s))
            }
            row(gap: .related, align: .center) {
                ui.Button("Choose the right file", icon: .file_plus) {
                    on press: file.choose(chosen_right(s))
                }
                ui.Button("Load sample", icon: .sparkles) {
                    on press: fill_sample(s)
                }
            }
        }
    } else if s.identical {
        stack(gap: .related) {
            ui.EmptyState(
                "The files are identical",
                icon: .check,
                message: "Every line of {s.left_label} and {s.right_label} agrees.",
            )
            ui.Button("Compare other files", icon: .file_plus) {
                on press: file.choose(chosen_left(s))
            }
        }
    } else {
        row(gap: .related, fill) {
            if space.compact {
                stack(gap: .tight, fill: 1.0) {
                    ui.ToggleGroup(selected: if side == "left" { 0 } else { 1 }, fill: true) {
                        ui.Segment("Left")
                        ui.Segment("Right")
                        on change(i): set(side, if i == 1 { "right" } else { "left" })
                    }
                    comparison_grid(
                        s,
                        at,
                        if side == "left" { "left" } else { "right" },
                        language_word(s.left_label),
                    )
                }
            } else {
                stack(gap: .tight, fill: 1.0) {
                    row(gap: .section, fill) {
                        ui.Text(s.left_label, role: .label, truncate: true, fill: 1.0)
                        ui.Text(s.right_label, role: .label, truncate: true, fill: 1.0)
                    }
                    comparison_grid(s, at, "both", language_word(s.left_label))
                }
            }
            if not space.compact and not s.blocks.is_empty() {
                changes_rail(s, at)
            }
        }
    }
}

view comparison_grid(s: Session, at: State<Int>, side: Text, language: Text) {
    ui.DiffGrid(
        left: s.left_aligned.split("\n"),
        right: s.right_aligned.split("\n"),
        kinds: s.kinds,
        at: if s.blocks.is_empty() { .none } else { .some(block_first(s, at)) },
        side: if side == "left" { .left } else if side == "right" { .right } else { .both },
        language: if language == "rust" {
            .rust
        } else if language == "typst" {
            .typst
        } else if language == "sql" {
            .sql
        } else if language == "json" {
            .json
        } else if language == "shell" {
            .shell
        } else if language == "git" {
            .git
        } else if language == "python" {
            .python
        } else if language == "javascript" {
            .javascript
        } else if language == "typescript" {
            .typescript
        } else if language == "yaml" {
            .yaml
        } else if language == "markdown" {
            .markdown
        } else if language == "html" {
            .html
        } else if language == "css" {
            .css
        } else if language == "toml" {
            .toml
        } else if language == "dockerfile" {
            .dockerfile
        } else if language == "c" {
            .c
        } else if language == "cpp" {
            .cpp
        } else if language == "go" {
            .go
        } else if language == "verilog" {
            .verilog
        } else if language == "system_verilog" {
            .system_verilog
        } else if language == "tilolang" { .tilolang } else { .plain_text },
        label: "{s.left_label} and {s.right_label}",
        fill,
    ) {
        on press(row: Int): set(at, change_for_row(s, row))
    }
}

/// The changes as a list beside the comparison: what each one is, the lines it touches and the
/// start of what it says.
view changes_rail(s: Session, at: State<Int>) {
    ui.Section("Changes") {
        ui.VirtualList(name: "Changes", selection_follows_focus: true) {
            on activate(b: Block): set(at, s.blocks.index_of(b) ?? 0)
            for b in s.blocks {
                ui.ListItem(item: b) {
                    row: {
                        ui.ListRow(
                            title: if b.preview == "" { kind_word(b.kind) } else { b.preview },
                            prefix: kind_word(b.kind),
                            secondary: "lines {b.first + 1}–{b.last + 1}",
                            tone: if b.kind == "added" {
                                .success
                            } else if b.kind == "removed" { .danger } else { .warning },
                            mark: if b.kind == "added" {
                                .plus
                            } else if b.kind == "removed" { .minus } else { .pencil },
                        )
                    }
                }
            }
        }
    }
}

/// The three-way merge: the sides, the changes to decide about, and the merged result.
view merge_page(
    s: Session,
    side: State<Text>,
    writing: State<Int>,
    typed: State<Text>,
    at_hunk: State<Int>,
) {
    if not s.merging {
        ui.EmptyState(
            "No merge open",
            icon: .arrow_up_down,
            message: "A three-way merge starts from a common base and two edited versions of it. Load a sample to try one.",
            action: "Load sample",
        ) {
            on act: load_sample(library)
        }
    } else {
        stack(gap: .related, fill: 1.0) {
            if space.compact {
                ui.ToggleGroup(
                    selected: if side == "base" { 0 } else if side == "theirs" { 2 } else { 1 },
                    fill: true,
                ) {
                    ui.Segment("Base")
                    ui.Segment("Ours")
                    ui.Segment("Theirs")
                    on change(i): set(
                        side,
                        if i == 0 { "base" } else if i == 2 { "theirs" } else { "ours" },
                    )
                }
                if side == "base" {
                    merge_source(s, "base", "Base", s.base_label)
                } else if side == "theirs" {
                    merge_source(s, "theirs", "Theirs", s.theirs_label)
                } else {
                    merge_source(s, "ours", "Ours", s.ours_label)
                }
            } else {
                row(gap: .related, fill) {
                    merge_source(s, "base", "Base", s.base_label)
                    merge_source(s, "ours", "Ours", s.ours_label)
                    merge_source(s, "theirs", "Theirs", s.theirs_label)
                }
            }
            ui.Section("Changes", collapsible: false) {
                if s.changes == 0 {
                    ui.Text("The three sides agree: the merged result needs nothing.", role: .body)
                } else {
                    stack(gap: .sibling) {
                        for h in s.hunks {
                            hunk_card(s, h, writing, typed, at_hunk)
                        }
                    }
                }
            }
            merged_section(s)
        }
    }
}

/// One side of a merge's code, coloured as its file is written.
view merge_source(s: Session, which: Text, name: Text, label: Text) {
    let word = language_word(label)
    stack(gap: .tight, fill: 1.0, min: 16rem) {
        source_head(name, label)
        ui.Scroll(fill: 1.0) {
            if which == "base" {
                ui.CodeEditor(
                    bind: s.base,
                    read_only: true,
                    gutter: true,
                    max_lines: 16,
                    language: if word == "rust" {
                        .rust
                    } else if word == "typst" {
                        .typst
                    } else if word == "sql" {
                        .sql
                    } else if word == "json" {
                        .json
                    } else if word == "shell" {
                        .shell
                    } else if word == "git" {
                        .git
                    } else if word == "python" {
                        .python
                    } else if word == "javascript" {
                        .javascript
                    } else if word == "typescript" {
                        .typescript
                    } else if word == "yaml" {
                        .yaml
                    } else if word == "markdown" {
                        .markdown
                    } else if word == "html" {
                        .html
                    } else if word == "css" {
                        .css
                    } else if word == "toml" {
                        .toml
                    } else if word == "dockerfile" {
                        .dockerfile
                    } else if word == "c" {
                        .c
                    } else if word == "cpp" {
                        .cpp
                    } else if word == "go" {
                        .go
                    } else if word == "verilog" {
                        .verilog
                    } else if word == "system_verilog" {
                        .system_verilog
                    } else if word == "tilolang" { .tilolang } else { .plain_text },
                )
            } else if which == "theirs" {
                ui.CodeEditor(
                    bind: s.theirs,
                    read_only: true,
                    gutter: true,
                    max_lines: 16,
                    language: if word == "rust" {
                        .rust
                    } else if word == "typst" {
                        .typst
                    } else if word == "sql" {
                        .sql
                    } else if word == "json" {
                        .json
                    } else if word == "shell" {
                        .shell
                    } else if word == "git" {
                        .git
                    } else if word == "python" {
                        .python
                    } else if word == "javascript" {
                        .javascript
                    } else if word == "typescript" {
                        .typescript
                    } else if word == "yaml" {
                        .yaml
                    } else if word == "markdown" {
                        .markdown
                    } else if word == "html" {
                        .html
                    } else if word == "css" {
                        .css
                    } else if word == "toml" {
                        .toml
                    } else if word == "dockerfile" {
                        .dockerfile
                    } else if word == "c" {
                        .c
                    } else if word == "cpp" {
                        .cpp
                    } else if word == "go" {
                        .go
                    } else if word == "verilog" {
                        .verilog
                    } else if word == "system_verilog" {
                        .system_verilog
                    } else if word == "tilolang" { .tilolang } else { .plain_text },
                )
            } else {
                ui.CodeEditor(
                    bind: s.ours,
                    read_only: true,
                    gutter: true,
                    max_lines: 16,
                    language: if word == "rust" {
                        .rust
                    } else if word == "typst" {
                        .typst
                    } else if word == "sql" {
                        .sql
                    } else if word == "json" {
                        .json
                    } else if word == "shell" {
                        .shell
                    } else if word == "git" {
                        .git
                    } else if word == "python" {
                        .python
                    } else if word == "javascript" {
                        .javascript
                    } else if word == "typescript" {
                        .typescript
                    } else if word == "yaml" {
                        .yaml
                    } else if word == "markdown" {
                        .markdown
                    } else if word == "html" {
                        .html
                    } else if word == "css" {
                        .css
                    } else if word == "toml" {
                        .toml
                    } else if word == "dockerfile" {
                        .dockerfile
                    } else if word == "c" {
                        .c
                    } else if word == "cpp" {
                        .cpp
                    } else if word == "go" {
                        .go
                    } else if word == "verilog" {
                        .verilog
                    } else if word == "system_verilog" {
                        .system_verilog
                    } else if word == "tilolang" { .tilolang } else { .plain_text },
                )
            }
        }
    }
}

/// What one side of a merge is called: which side, and the file it was.
view source_head(name: Text, label: Text) {
    ui.Text(name, role: .label)
    ui.Text(
        if label == "" { "—" } else { label },
        role: .caption,
        secondary: true,
        truncate: true,
    )
}

/// One side of a change, coloured as the merged file is written.
view hunk_source(s: Session, h: Hunk, which: Text, name: Text) {
    let word = language_word(s.ours_label)
    ui.Text(name, role: .caption, secondary: true)
    if which == "base" {
        ui.CodeEditor(
            bind: h.base,
            read_only: true,
            gutter: false,
            max_lines: 6,
            language: if word == "rust" {
                .rust
            } else if word == "typst" {
                .typst
            } else if word == "sql" {
                .sql
            } else if word == "json" {
                .json
            } else if word == "shell" {
                .shell
            } else if word == "git" {
                .git
            } else if word == "python" {
                .python
            } else if word == "javascript" {
                .javascript
            } else if word == "typescript" {
                .typescript
            } else if word == "yaml" {
                .yaml
            } else if word == "markdown" {
                .markdown
            } else if word == "html" {
                .html
            } else if word == "css" {
                .css
            } else if word == "toml" {
                .toml
            } else if word == "dockerfile" {
                .dockerfile
            } else if word == "c" {
                .c
            } else if word == "cpp" {
                .cpp
            } else if word == "go" {
                .go
            } else if word == "verilog" {
                .verilog
            } else if word == "system_verilog" {
                .system_verilog
            } else if word == "tilolang" { .tilolang } else { .plain_text },
        )
    } else if which == "ours" {
        ui.CodeEditor(
            bind: h.ours,
            read_only: true,
            gutter: false,
            max_lines: 6,
            language: if word == "rust" {
                .rust
            } else if word == "typst" {
                .typst
            } else if word == "sql" {
                .sql
            } else if word == "json" {
                .json
            } else if word == "shell" {
                .shell
            } else if word == "git" {
                .git
            } else if word == "python" {
                .python
            } else if word == "javascript" {
                .javascript
            } else if word == "typescript" {
                .typescript
            } else if word == "yaml" {
                .yaml
            } else if word == "markdown" {
                .markdown
            } else if word == "html" {
                .html
            } else if word == "css" {
                .css
            } else if word == "toml" {
                .toml
            } else if word == "dockerfile" {
                .dockerfile
            } else if word == "c" {
                .c
            } else if word == "cpp" {
                .cpp
            } else if word == "go" {
                .go
            } else if word == "verilog" {
                .verilog
            } else if word == "system_verilog" {
                .system_verilog
            } else if word == "tilolang" { .tilolang } else { .plain_text },
        )
    } else {
        ui.CodeEditor(
            bind: h.theirs,
            read_only: true,
            gutter: false,
            max_lines: 6,
            language: if word == "rust" {
                .rust
            } else if word == "typst" {
                .typst
            } else if word == "sql" {
                .sql
            } else if word == "json" {
                .json
            } else if word == "shell" {
                .shell
            } else if word == "git" {
                .git
            } else if word == "python" {
                .python
            } else if word == "javascript" {
                .javascript
            } else if word == "typescript" {
                .typescript
            } else if word == "yaml" {
                .yaml
            } else if word == "markdown" {
                .markdown
            } else if word == "html" {
                .html
            } else if word == "css" {
                .css
            } else if word == "toml" {
                .toml
            } else if word == "dockerfile" {
                .dockerfile
            } else if word == "c" {
                .c
            } else if word == "cpp" {
                .cpp
            } else if word == "go" {
                .go
            } else if word == "verilog" {
                .verilog
            } else if word == "system_verilog" {
                .system_verilog
            } else if word == "tilolang" { .tilolang } else { .plain_text },
        )
    }
}

/// One change to decide about: what the sides give it, and the ways they can be taken. The change
/// the person is at answers `o`, `t` and `b`, one press each.
view hunk_card(s: Session, h: Hunk, writing: State<Int>, typed: State<Text>, at_hunk: State<Int>) {
    ui.Card(title: "Change {h.index + 1}", item: s, selected: at_hunk == h.index) {
        on activate: set(at_hunk, h.index)
        ui.Badge(
            resolution_word(h.resolution),
            tone: if h.resolution == "conflict" { .warning } else { .success },
        )
        stack(gap: .tight, fill) {
            if h.base != "" {
                hunk_source(s, h, "base", "Base")
            }
            hunk_source(s, h, "ours", "Ours")
            hunk_source(s, h, "theirs", "Theirs")
        }
        row(gap: .related, align: .center) {
            ui.Button(
                "Take ours",
                icon: .arrow_left,
                role: if h.resolution == "ours" { .primary } else { .secondary },
                key: if at_hunk == h.index { "o" } else { "" },
            ) {
                on press: take_ours(s, h.index)
            }
            ui.Button(
                "Take theirs",
                icon: .arrow_right,
                role: if h.resolution == "theirs" { .primary } else { .secondary },
                key: if at_hunk == h.index { "t" } else { "" },
            ) {
                on press: take_theirs(s, h.index)
            }
            ui.Button(
                "Take both",
                icon: .arrow_up_down,
                role: if h.resolution == "both" { .primary } else { .secondary },
                key: if at_hunk == h.index { "b" } else { "" },
            ) {
                on press: take_both(s, h.index)
            }
            if writing == h.index {
                ui.TextInput(
                    bind: typed,
                    placeholder: "My own lines for this change",
                    entry: true,
                    fill: 1.0,
                ) {
                    on submit(text): resolve(s, h.index, text)
                        .then(set(writing, -1))
                        .then(set(typed, ""))
                }
                ui.Button("Resolve", icon: .check) {
                    on press: resolve(s, h.index, typed).then(set(writing, -1)).then(set(typed, ""))
                }
            } else {
                ui.Button("Write my own", icon: .pencil, role: .plain) {
                    on press: set(writing, h.index).then(set(typed, h.base))
                }
            }
        }
    }
}

/// The merged result everyone edits, and how much of the merge still asks something.
view merged_section(s: Session) {
    let word = language_word(s.ours_label)
    ui.Section(
        if s.conflicts == 0 {
            "Merged result"
        } else {
            "Merged result · {count(s.conflicts)} conflicts left"
        },
        collapsible: false,
    ) {
        if s.merged != s.composed {
            ui.Banner(
                "The merged result was edited by hand: the rest is resolved in it, and taking a side would replace your edit.",
                kind: .info,
                icon: .info,
            )
        }
        ui.CodeEditor(
            bind: s.merged,
            gutter: true,
            min_lines: 8,
            max_lines: 24,
            language: if word == "rust" {
                .rust
            } else if word == "typst" {
                .typst
            } else if word == "sql" {
                .sql
            } else if word == "json" {
                .json
            } else if word == "shell" {
                .shell
            } else if word == "git" {
                .git
            } else if word == "python" {
                .python
            } else if word == "javascript" {
                .javascript
            } else if word == "typescript" {
                .typescript
            } else if word == "yaml" {
                .yaml
            } else if word == "markdown" {
                .markdown
            } else if word == "html" {
                .html
            } else if word == "css" {
                .css
            } else if word == "toml" {
                .toml
            } else if word == "dockerfile" {
                .dockerfile
            } else if word == "c" {
                .c
            } else if word == "cpp" {
                .cpp
            } else if word == "go" {
                .go
            } else if word == "verilog" {
                .verilog
            } else if word == "system_verilog" {
                .system_verilog
            } else if word == "tilolang" { .tilolang } else { .plain_text },
        )
        row(gap: .related, align: .center) {
            ui.Button("Save…", icon: .save, role: .primary) {
                on press: file.save(save_bytes(s), name: save_name(s))
            }
            if s.saved_path != "" {
                ui.Button("Save to {s.saved_name}", icon: .hard_drive) {
                    on press: save_merged(s, s.saved_path)
                }
            }
            if s.saved != "" and s.saved == s.merged {
                ui.Badge("Saved", tone: .success)
            } else {
                ui.Badge("Unsaved", tone: .warning)
            }
        }
    }
}

/// The files that differ between two folder trees.
view folders_page(s: Session) {
    if s.folder_entries.is_empty() {
        stack(gap: .related) {
            ui.EmptyState(
                "Compare two folders",
                icon: .folder,
                message: "Choose two folders to list the files only one holds and the files both hold but differ in.",
                action: "Choose the left folder",
            ) {
                on act: file.choose_folder(chosen_folder_left(s))
            }
            ui.Button("Show the sample folders", icon: .sparkles) {
                on press: sample_folders(s)
            }
        }
    } else {
        stack(gap: .related, fill: 1.0) {
            row(gap: .related, align: .center) {
                ui.Text(
                    "{s.folder_left_label} and {s.folder_right_label}: {count(s.folder_entries.len())} files differ",
                    role: .label,
                    truncate: true,
                    fill: 1.0,
                )
                ui.Button("Choose the folders again", icon: .folder) {
                    on press: file.choose_folder(chosen_folder_left(s))
                }
            }
            if s.folder_truncated {
                ui.Banner(
                    "Only the first part of the folders was compared",
                    kind: .warning,
                    icon: .triangle_alert,
                )
            }
            ui.VirtualList(name: "Files that differ", in_column: true) {
                on activate(e: FolderEntry): open_folder_entry(s, e)
                for e in s.folder_entries {
                    ui.ListItem(item: e, keys: ["Enter"]) {
                        on shortcut(k): open_on_key(s, e, k)
                        row: {
                            ui.ListRow(
                                title: e.path,
                                prefix: kind_word(e.kind),
                                secondary: folder_secondary(e),
                                tone: if e.kind == "added" {
                                    .success
                                } else if e.kind == "removed" { .danger } else { .warning },
                            )
                        }
                    }
                }
            }
        }
    }
}

fn folder_secondary(e: FolderEntry) -> Text {
    if e.kind == "added" {
        "only here, {size(e.left_len)}"
    } else if e.kind == "removed" {
        "only there, {size(e.right_len)}"
    } else if e.by_length {
        "{size(e.left_len)} became {size(e.right_len)}"
    } else { "different contents" }
}

/// The bar under the window: what the comparison found, and which change it is at.
view status_line(s: Session, mode: Text, at: State<Int>) {
    if mode == "folders" {
        let added = s.folder_entries.filter(e -> e.kind == "added").len()
        let removed = s.folder_entries.filter(e -> e.kind == "removed").len()
        let modified = s.folder_entries.filter(e -> e.kind == "modified").len()
        ui.StatusLine(
            "{count(added)} added, {count(removed)} removed, {count(modified)} modified",
            status: if s.folder_truncated { .warning } else { .idle },
        )
    } else if mode == "merge" {
        ui.StatusLine(
            if s.changes == 0 {
                "No changes to merge"
            } else {
                "{count(s.changes)} changes, {count(s.changes - s.conflicts)} resolved, {count(s.conflicts)} conflicts left"
            },
            status: if s.conflicts > 0 { .warning } else { .idle },
        )
    } else if not s.compared {
        ui.StatusLine("No files open")
    } else if s.identical {
        ui.StatusLine("The files are identical")
    } else {
        ui.StatusLine(
            "{count(s.added)} added, {count(s.removed)} removed, {count(s.modified)} modified. Change {count(at + 1)} of {count(s.blocks.len())}",
            status: .idle,
        )
    }
}
