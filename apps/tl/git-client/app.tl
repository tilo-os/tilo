app so.tilo.git-client "Git" (
    icon: .git_branch,
    category: .development,
    tagline: "Change, commit and share a repository",
    keywords: ["git", "repository", "commit", "branch", "merge", "diff", "stage", "stash", "remote", "version control", "quellcode", "versionierung", "zweig"],
)

op "stage_file" changes device
op "unstage_file" changes device
op "discard_file" changes device
op "stage_all" changes device
op "unstage_all" changes device
op "apply_hunk" changes device
op "discard_hunk" changes device
op "commit" changes device
op "resolve_conflict" changes device
op "create_branch" changes device
op "checkout_branch" changes device
op "delete_branch" changes device
op "stash_save" changes device
op "stash_apply" changes device
op "create_worktree" changes device
op "fetch" changes device
op "pull" changes device
op "push" leaves
op "commit_detail"
op "load"

/// Where a change stands: in the next commit, still in the working tree, or not tracked yet.
enum ChangeArea {
    staged
    unstaged
    untracked
}

/// What became of a file.
enum ChangeKind {
    added
    modified
    deleted
    renamed
    untracked
    conflicted
}

/// What a ref on a commit is.
enum RefKind {
    head
    local_branch
    remote_branch
    tag
}

/// How a diff reads.
enum DiffLayout {
    unified
    split
}

/// One hunk of a diff, which can be staged or discarded on its own.
type Hunk {
    index: Int = 0
    header: Text = ""
    old_start: Int = 0
    new_start: Int = 0
}

/// One changed file: where it stands, what became of it and its diff. A change is a resource
/// because the actions a person takes on it act on the file it names.
type Change (title: path) {
    path: Text = ""
    old_path: Option<Text> = .none
    area: ChangeArea = .unstaged
    kind: ChangeKind = .modified
    additions: Int = 0
    deletions: Int = 0
    binary: Bool = false
    /// What keeping each side of a conflict would discard, in lines.
    ours: Int = 0
    theirs: Int = 0
    /// The file's language token (`rust`, `json`, ...), for the code view's colouring.
    language: Text = "plain"
    /// The change in one column: each line's old and new number, its marker and its words.
    unified: Text = ""
    /// The old side of a side-by-side reading, aligned row for row with `right`.
    left: Text = ""
    /// The new side of a side-by-side reading.
    right: Text = ""
    /// Its hunks, each as its own JSON text.
    hunks: Text = ""
}

/// One ref that points at a commit.
type RefLabel {
    name: Text = ""
    kind: RefKind = .local_branch
}

/// One commit of the repository.
type Commit (title: summary) {
    id: Text = ""
    summary: Text = ""
    body: Text = ""
    author_name: Text = ""
    author_email: Text = ""
    /// When it was made, in seconds since the epoch.
    time: Int = 0
    /// How long ago that was, in seconds.
    age: Int = 0
    /// When it was made, as a machine reads it.
    date: Text = ""
    parents: List<Text>
    refs: List<WireRef>
    column: Int = 0
}

/// One branch of the repository.
type Branch (title: name) {
    name: Text = ""
    is_head: Bool = false
    is_remote: Bool = false
    upstream: Option<Text> = .none
    ahead: Int = 0
    behind: Int = 0
    tip: Text = ""
}

/// One tag of the repository.
type Tag (title: name) {
    name: Text = ""
    target: Text = ""
    message: Option<Text> = .none
}

/// One stash of the working tree.
type Stash (title: message) {
    index: Int = 0
    message: Text = ""
    branch: Text = ""
}

/// One remote the repository fetches from and pushes to.
type Remote (title: name) {
    name: Text = ""
    url: Text = ""
}

/// One working directory linked to the repository.
type Worktree (title: path) {
    name: Text = ""
    path: Text = ""
    head: Text = ""
    current: Bool = false
}

/// One file a commit changed, with its diff.
type CommitFile {
    path: Text = ""
    old_path: Option<Text> = .none
    kind: ChangeKind = .modified
    additions: Int = 0
    deletions: Int = 0
    binary: Bool = false
    language: Text = "plain"
    unified: Text = ""
    left: Text = ""
    right: Text = ""
}

/// One folder the repository picker listed.
type BrowseEntry (title: name) {
    name: Text = ""
    path: Text = ""
    folder: Bool = false
}

/// One person's window on the repository: the repository they opened here, its working tree, and
/// how they read diffs. It is the reader's own, so two people's pages never turn under each other.
type Session (readers: author) {
    /// The path of the repository this person opened here; empty until they open one.
    path: Text = ""
    changes: Collection<Change>
    layout: DiffLayout = .unified
    highlight: Bool = true
    whitespace: Bool = false
    wrap: Bool = true
    /// The branch a switch waits on, while the working tree holds uncommitted work.
    pending_switch: Text = ""
    /// The picker's folder and entries, while the person chooses where a repository is.
    browsing: Bool = false
    folder: Text = ""
    entries: Collection<BrowseEntry>
    /// The commit whose changed files were read, and those files as a JSON list of texts.
    commit_id: Text = ""
    commit_files: Text = ""
    /// What the last action did, said once; what went wrong, said until it is put away.
    notice: Text = ""
    trouble: Text = ""
}

/// The workspace's Git: the repository it opens and what members see of it, and one window per
/// person on it.
type Repository {
    /// Where the repository is on this device.
    path: Text = ""
    name: Text = ""
    head: Text = ""
    workdir: Text = ""
    author_name: Text = ""
    author_email: Text = ""
    /// Everything the device answered about the repository, as the text it wrote: its commits,
    /// branches, tags, stashes, remotes and working directories. The window reads it as the
    /// records it draws.
    snapshot: Text = ""
    sessions: Collection<Session>
    /// The device's job while a remote operation runs, what it is, and the remote it names;
    /// empty otherwise.
    remote_job: Text = ""
    remote_op: Text = ""
    remote_name: Text = ""
    /// The last check the project's host ran for it, which every member sees.
    check: ApprovedCheck
}

/// Where the last approved check stands: none ran yet (or it was put away), the host runs it now,
/// the host observed how its commands ended, it was stopped, or the workspace or the host
/// refused it.
enum CheckState {
    idle
    running
    observed
    stopped
    refused
}

/// A check the project's host approved, as it last ran here: what the host reports while it runs,
/// and what it observed once it ended. The host picks the commands, the sources and the bounds;
/// the app names the check by its id and nothing else.
type ApprovedCheck {
    check_id: Text = ""
    /// The device's job while it runs; empty otherwise.
    job: Text = ""
    state: CheckState = .idle
    /// The newest phase the host reported, with how much of how much it has done.
    phase: Text = ""
    completed: Int = 0
    total: Int = 0
    /// How the commands ended, as the host observed it: `exited`, `cancelled`, `timed_out`,
    /// `output_limit` or `failed`.
    outcome: Text = ""
    exit_code: Option<Int> = .none
    elapsed_ms: Int = 0
    /// The start of what the commands printed, and of what they printed as errors.
    output: Text = ""
    errors: Text = ""
    /// Why it did not run, or why the host could not supervise it.
    refusal: Text = ""
}

root git: Repository

default view changes for Repository

views changes (icon: .git_commit_horizontal), history (icon: .history), branches (icon: .git_branch) for Repository

/// Opening the app gives the person their window on it, though they open no repository yet.
opened action arrive(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    if s.path == "" and g.path != "" {
        s.path = g.path
    }
}

// ---------------------------------------------------------------- what the device answers
type Loaded {
    /// What the op that answered it made, in a few words: a commit's or stash's short id.
    answer: Text = ""
    name: Text = ""
    head: Text = ""
    workdir: Text = ""
    author_name: Text = ""
    author_email: Text = ""
    /// Each collection as a list of JSON texts, one an item.
    changes: List<Text>
    commits: List<Text>
    branches: List<Text>
    tags: List<Text>
    stashes: List<Text>
    remotes: List<Text>
    worktrees: List<Text>
    worktree_error: Option<Text> = .none
}

type WireChange {
    path: Text = ""
    old_path: Option<Text> = .none
    area: ChangeArea = .unstaged
    kind: ChangeKind = .modified
    additions: Int = 0
    deletions: Int = 0
    binary: Bool = false
    ours: Int = 0
    theirs: Int = 0
    language: Text = "plain"
    unified: Text = ""
    left: Text = ""
    right: Text = ""
    hunks: List<Text>
}

type WireCommit {
    sha: Text = ""
    summary: Text = ""
    body: Text = ""
    author_name: Text = ""
    author_email: Text = ""
    time: Int = 0
    age: Int = 0
    date: Text = ""
    parents: List<Text>
    refs: List<Text>
    column: Int = 0
}

type WireRef {
    name: Text = ""
    kind: RefKind = .local_branch
}

type WireBranch {
    name: Text = ""
    is_head: Bool = false
    is_remote: Bool = false
    upstream: Option<Text> = .none
    ahead: Int = 0
    behind: Int = 0
    tip: Text = ""
}

type WireTag {
    name: Text = ""
    target: Text = ""
    message: Option<Text> = .none
}

type WireStash {
    index: Int = 0
    message: Text = ""
    branch: Text = ""
}

type WireRemote {
    name: Text = ""
    url: Text = ""
}

type WireWorktree {
    name: Text = ""
    path: Text = ""
    head: Text = ""
    current: Bool = false
}

type WireFile {
    path: Text = ""
    old_path: Option<Text> = .none
    kind: ChangeKind = .modified
    additions: Int = 0
    deletions: Int = 0
    binary: Bool = false
    language: Text = "plain"
    unified: Text = ""
    left: Text = ""
    right: Text = ""
}

type PathAsk {
    path: Text
    /// Whether a diff shows the whitespace at the end of its lines.
    whitespace: Bool = false
}

type FileAsk {
    path: Text
    file: Text
}

type HunkAsk {
    path: Text
    file: Text
    hunk: Int
    unstage: Bool = false
}

type CommitAsk {
    path: Text
    message: Text
}

type BranchAsk {
    path: Text
    name: Text
    checkout: Bool = false
}

type CheckoutAsk {
    path: Text
    name: Text
    stash: Bool = false
}

type CommitFilesAsk {
    path: Text
    commit: Text
}

type WorktreeAsk {
    path: Text
    destination: Text
    branch: Text
    start: Text
}

type StashSaveAsk {
    path: Text
    message: Text = ""
}

type StashApplyAsk {
    path: Text
    index: Int
}

type ResolveAsk {
    path: Text
    file: Text
    ours: Bool
}

/// How a check stands, as its job's record says (`engine.job`): once it is done, what the host
/// observed of its commands.
type CheckStatus {
    state: Text = ""
    phase: Text = ""
    completed: Option<Int> = .none
    total: Option<Int> = .none
    why: Text = ""
    outcome: Text = ""
    exit_code: Option<Int> = .none
    elapsed_ms: Int = 0
    output: Text = ""
    errors: Text = ""
    failure: Text = ""
}

type RemoteAsk {
    path: Text
    remote: Text = ""
}

fn texts(text: Text) -> List<Text> {
    if text == "" {
        []
    } else {
        match json.decode<List<Text>>(text) {
            .ok(items) => items,
            .err(_) => [],
        }
    }
}

fn changes_of(items: List<Text>) -> List<WireChange> {
    items.flat_map(
        t -> match json.decode<WireChange>(t) {
            .ok(w) => [w],
            .err(_) => [],
        },
    )
}

fn hunks_of(items: List<Text>) -> List<Hunk> {
    items.flat_map(
        t -> match json.decode<Hunk>(t) {
            .ok(h) => [h],
            .err(_) => [],
        },
    )
}

fn refs_of(items: List<Text>) -> List<RefLabel> {
    items
        .flat_map(
            t -> match json.decode<WireRef>(t) {
                .ok(r) => [r],
                .err(_) => [],
            },
        )
        .map(r -> RefLabel(name: r.name, kind: r.kind))
}

/// The commits the device's engine answered with; `id` reads from the engine's own text, since a
/// record's `.id` is the resource it is, which a commit is not.
fn commits_of(items: List<Text>) -> List<WireCommit> {
    items.flat_map(
        t -> match json.decode<WireCommit>(t) {
            .ok(w) => [with_sha(w, t)],
            .err(_) => [],
        },
    )
}

/// The commit `w`, with the identity the engine wrote in its text: a record's `.id` names the
/// resource it is, and a commit is a value of the text, no resource.
fn with_sha(w: WireCommit, raw: Text) -> WireCommit {
    let written = match json.parse(raw) {
        .ok(value) => std.json_text(value, "id"),
        .err(_) => "",
    }
    let sha = if written == "" { w.sha } else { written }
    WireCommit(
        sha: sha,
        summary: w.summary,
        body: w.body,
        author_name: w.author_name,
        author_email: w.author_email,
        time: w.time,
        age: w.age,
        date: w.date,
        parents: w.parents,
        refs: w.refs,
        column: w.column,
    )
}

fn branches_of(items: List<Text>) -> List<WireBranch> {
    items.flat_map(
        t -> match json.decode<WireBranch>(t) {
            .ok(w) => [w],
            .err(_) => [],
        },
    )
}

fn tags_of(items: List<Text>) -> List<WireTag> {
    items.flat_map(
        t -> match json.decode<WireTag>(t) {
            .ok(w) => [w],
            .err(_) => [],
        },
    )
}

fn stashes_of(items: List<Text>) -> List<WireStash> {
    items.flat_map(
        t -> match json.decode<WireStash>(t) {
            .ok(w) => [w],
            .err(_) => [],
        },
    )
}

fn remotes_of(items: List<Text>) -> List<WireRemote> {
    items.flat_map(
        t -> match json.decode<WireRemote>(t) {
            .ok(w) => [w],
            .err(_) => [],
        },
    )
}

fn worktrees_of(items: List<Text>) -> List<WireWorktree> {
    items.flat_map(
        t -> match json.decode<WireWorktree>(t) {
            .ok(w) => [w],
            .err(_) => [],
        },
    )
}

fn files_of(items: List<Text>) -> List<CommitFile> {
    items.flat_map(
        t -> match json.decode<WireFile>(t) {
            .ok(w) => [
                CommitFile(path: w.path, kind: w.kind, additions: w.additions, binary: w.binary),
            ],
            .err(_) => [],
        },
    )
}

/// The repository the device last answered, read as the window draws it.
fn shown(g: Repository) -> Option<Loaded> {
    if g.snapshot == "" {
        .none
    } else {
        match json.decode<Loaded>(g.snapshot) {
            .ok(loaded) => .some(loaded),
            .err(_) => .none,
        }
    }
}

fn committed(g: Repository) -> List<WireCommit> {
    if let loaded = shown(g) { commits_of(loaded.commits) } else { [] }
}

fn branched(g: Repository) -> List<WireBranch> {
    if let loaded = shown(g) { branches_of(loaded.branches) } else { [] }
}

fn tagged(g: Repository) -> List<WireTag> {
    if let loaded = shown(g) { tags_of(loaded.tags) } else { [] }
}

fn stashed(g: Repository) -> List<WireStash> {
    if let loaded = shown(g) { stashes_of(loaded.stashes) } else { [] }
}

fn remoted(g: Repository) -> List<WireRemote> {
    if let loaded = shown(g) { remotes_of(loaded.remotes) } else { [] }
}

fn worked(g: Repository) -> List<WireWorktree> {
    if let loaded = shown(g) { worktrees_of(loaded.worktrees) } else { [] }
}

fn worktree_error(g: Repository) -> Text {
    if let loaded = shown(g) {
        if let why = loaded.worktree_error { why } else { "" }
    } else { "" }
}

fn full_id(c: Option<WireCommit>) -> Text {
    if let one = c { one.sha } else { "" }
}

fn made(g: Repository) -> Text {
    if let loaded = shown(g) { loaded.answer } else { "" }
}

// ---------------------------------------------------------------- the repository on disk
fn area_key(a: ChangeArea) -> Text {
    if a == .staged { "staged" } else if a == .unstaged { "unstaged" } else { "untracked" }
}

fn change_key(c: Change) -> Text {
    "{area_key(c.area)}:{c.path}"
}

fn change_key_of(w: WireChange) -> Text {
    "{area_key(w.area)}:{w.path}"
}

/// Reads the repository on disk into the app's records, for this person's window.
action refresh(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    if s.path == "" {
        return
    }
    loads(g, s, json.encode(PathAsk(path: s.path, whitespace: s.whitespace)))
}

/// Opens the repository at `path` on this device.
// ---------------------------------------------------------------- opening a repository
action open_repository(g: Repository, path: Text) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    let typed = path.trim()
    if typed == "" {
        s.trouble = "Type the path of a repository"
        return
    }
    s.path = typed
    loads(g, s, json.encode(PathAsk(path: typed, whitespace: s.whitespace)))
    if s.trouble == "" {
        s.notice = "Opened the repository {typed}"
    } else {
        s.path = g.path
    }
}

/// Starts choosing a repository's folder, listing this device's home folder.
action choose_repository(g: Repository) uses files {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    s.browsing = true
    s.trouble = ""
    read_folder(s, "~")
}

/// Lists `folder` in the repository picker.
action read_folder(s: Session, folder: Text) uses files {
    match File.list(folder) {
        .err(why) => {
            s.trouble = why
        }
        .ok(listed) => {
            let old = s.entries
            for e in old {
                s.entries.remove(e)
            }
            for e in listed.filter(x -> x.folder) {
                let entry = s.entries.insert(BrowseEntry())
                entry.name = e.name
                entry.path = e.path
                entry.folder = true
            }
            s.folder = folder
            s.trouble = ""
        }
    }
}

/// Opens the folder the picker shows as a repository.
action open_browsed(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    s.browsing = false
    open_repository(g, s.folder)
}

/// Puts the picker away.
action close_picker(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    s.browsing = false
    s.trouble = ""
}

/// Stages one changed file.
// ---------------------------------------------------------------- the working tree
action stage_file(g: Repository, c: Change) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("stage_file", json.encode(FileAsk(path: s.path, file: c.path))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Takes one staged file back out of the next commit.
action unstage_file(g: Repository, c: Change) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("unstage_file", json.encode(FileAsk(path: s.path, file: c.path))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// The key pressed on a file's row: `s` stages it or takes it back out of the next commit, and on
/// a conflicted file `o` or `t` keeps our side or theirs.
action change_on_key(g: Repository, c: Change, key: Text) {
    if key == "s" {
        if c.area == .staged { unstage_file(g, c) } else { stage_file(g, c) }
    } else if key == "o" and c.kind == .conflicted {
        resolve_conflict(g, c, true)
    } else if key == "t" and c.kind == .conflicted { resolve_conflict(g, c, false) }
}

/// Discards the working tree's edits to one file, or deletes it when it was never tracked.
destructive action discard_file(g: Repository, c: Change) (
    icon: .undo_2,
    label: "Discard file changes",
) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    let file = c.path
    match engine.call("discard_file", json.encode(FileAsk(path: s.path, file: file))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        s.notice = "Discarded the changes to {file}"
    }
}

/// Stages every change.
action stage_all(g: Repository) (icon: .plus, key: "Shift+S") {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("stage_all", json.encode(PathAsk(path: s.path))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Takes every staged change back out of the next commit.
action unstage_all(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("unstage_all", json.encode(PathAsk(path: s.path))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Stages one hunk of a file.
action stage_hunk(g: Repository, c: Change, hunk: Int) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call(
        "apply_hunk",
        json.encode(HunkAsk(path: s.path, file: c.path, hunk: hunk, unstage: false)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Takes one staged hunk back out of the next commit.
action unstage_hunk(g: Repository, c: Change, hunk: Int) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call(
        "apply_hunk",
        json.encode(HunkAsk(path: s.path, file: c.path, hunk: hunk, unstage: true)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Discards one hunk of a file's working tree edits.
destructive action discard_hunk(g: Repository, c: Change, hunk: Int) (
    icon: .undo_2,
    label: "Discard this change",
) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("discard_hunk", json.encode(HunkAsk(path: s.path, file: c.path, hunk: hunk))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Creates a commit from what is staged.
action commit(g: Repository, message: Text) (
    icon: .git_commit_horizontal,
    label: "Commit staged changes",
    key: "Ctrl+Enter",
) {
    require message.trim() != "" "A commit has a message"
    require staged_ready(g) "There are no staged changes to commit"
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("commit", json.encode(CommitAsk(path: s.path, message: message))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        s.notice = "Committed {made(g)}"
    }
}

/// Keeps one side of a conflicted file and stages it as resolved.
action resolve_conflict(g: Repository, c: Change, ours: Bool) (label: "Keep a side") {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    let file = c.path
    match engine.call(
        "resolve_conflict",
        json.encode(ResolveAsk(path: s.path, file: file, ours: ours)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        s.notice = if ours { "Kept our side of {file}" } else { "Kept their side of {file}" }
    }
}

/// Creates a branch and switches to it.
// ---------------------------------------------------------------- branches, stashes and worktrees
action create_branch(g: Repository, name: Text) (icon: .git_branch, label: "Create branch") {
    require name.trim() != "" "A branch has a name"
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call(
        "create_branch",
        json.encode(BranchAsk(path: s.path, name: name.trim(), checkout: true)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Switches to a branch, asking what becomes of uncommitted work when there is any.
action checkout_branch(g: Repository, name: Text) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    if s.changes.len() > 0 {
        s.pending_switch = name
        return
    }
    switch_to(g, name, false)
}

/// Answers the switch question by parking the uncommitted work on the stash first.
action switch_stashing(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    let name = s.pending_switch
    s.pending_switch = ""
    switch_to(g, name, true)
}

/// Answers the switch question by carrying the uncommitted work along.
action switch_carrying(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    let name = s.pending_switch
    s.pending_switch = ""
    switch_to(g, name, false)
}

/// Puts the switch question away.
action cancel_switch(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    s.pending_switch = ""
}

/// Switches to `name`, parking the uncommitted work when `stash` says so.
action switch_to(g: Repository, name: Text, stash: Bool) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call(
        "checkout_branch",
        json.encode(CheckoutAsk(path: s.path, name: name, stash: stash)),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        let carried = made(g)
        s.notice = if carried == "" {
            "Switched to {name}"
        } else {
            "Switched to {name}; the changes are stashed as {carried}"
        }
    }
}

/// Deletes a local branch.
destructive action delete_branch(g: Repository, name: Text) (icon: .trash, label: "Delete branch") {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("delete_branch", json.encode(BranchAsk(path: s.path, name: name))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        s.notice = "Deleted the branch {name}"
    }
}

/// The key pressed on a branch's row: `Enter` switches to it, and `Delete` deletes it when it is a
/// local branch that is not the one checked out.
action branch_on_key(g: Repository, name: Text, is_remote: Bool, key: Text) {
    if key == "Enter" {
        checkout_branch(g, name)
    } else if key == "Delete" and not is_remote { delete_branch(g, name) }
}

/// Stashes the working tree's changes, with a message.
action save_stash(g: Repository, message: Text = "") (
    icon: .archive,
    label: "Stash working changes",
) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("stash_save", json.encode(StashSaveAsk(path: s.path, message: message))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        s.notice = "Stashed the changes as {made(g)}"
    }
}

/// Applies a stash, leaving it in the list.
action apply_stash(g: Repository, index: Int) (icon: .corner_down_left, label: "Apply stash") {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call("stash_apply", json.encode(StashApplyAsk(path: s.path, index: index))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        s.notice = "Applied the stash"
    }
}

/// Makes a new branch at `start` and a working directory of its own.
action create_worktree(g: Repository, path: Text, branch: Text, start: Text) (
    icon: .folder_plus,
    label: "Create worktree",
) {
    require branch.trim() != "" "A worktree branch has a name"
    require path.trim() != "" "A worktree has a destination path"
    require start.trim() != "" "A worktree starts somewhere"
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    match engine.call(
        "create_worktree",
        json.encode(
            WorktreeAsk(
                path: s.path,
                destination: path.trim(),
                branch: branch.trim(),
                start: start.trim(),
            ),
        ),
    ) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
    if s.trouble == "" {
        s.notice = "Created the worktree {path}"
    }
}

/// Fetches from a remote.
// ---------------------------------------------------------------- remotes
action fetch(g: Repository, remote: Text = "") (icon: .cloud_download, label: "Fetch from remote") {
    require g.remote_job == "" "Wait for the current remote operation to finish."
    start_remote(g, "fetch", remote)
}

/// Fetches, then fast-forwards the current branch to its upstream.
action pull(g: Repository, remote: Text = "") (icon: .arrow_down, label: "Pull from remote") {
    require g.remote_job == "" "Wait for the current remote operation to finish."
    start_remote(g, "pull", remote)
}

/// Pushes the current branch to a like-named branch on a remote. What it sends leaves the system
/// for good: everyone who reads the remote has it.
action push(g: Repository, remote: Text = "") (icon: .arrow_up, label: "Push to remote") {
    require g.remote_job == "" "Wait for the current remote operation to finish."
    start_remote(g, "push", remote)
}

/// Starts the remote operation as a job the person can stop: the window says what it is doing
/// while it runs, and the answer comes back to `remote_ended`.
action start_remote(g: Repository, op: Text, remote: Text) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    let ask = json.encode(RemoteAsk(path: s.path, remote: remote))
    let started = if op == "fetch" {
        engine.start("fetch", ask, done: j -> remote_ended(g, j))
    } else if op == "pull" {
        engine.start("pull", ask, done: j -> remote_ended(g, j))
    } else {
        engine.start("push", ask, done: j -> remote_ended(g, j))
    }
    match started {
        .err(why) => {
            s.trouble = why
        }
        .ok(job) => {
            g.remote_job = job
            g.remote_op = op
            g.remote_name = remote
            s.trouble = ""
            s.notice = ""
        }
    }
}

/// What the remote operation came to: the repository as it now stands, or why it stopped.
action remote_ended(g: Repository, j: host.Job) {
    if g.remote_job != j.id {
        return
    }
    let op = g.remote_op
    let remote = g.remote_name
    g.remote_job = ""
    g.remote_op = ""
    g.remote_name = ""
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    if j.state == .done {
        keep(g, s, j.detail)
        let word = if op == "fetch" {
            "Fetched"
        } else if op == "pull" { "Pulled" } else { "Pushed" }
        if op == "push" {
            s.notice = if remote == "" { word } else { "{word} to {remote}" }
        } else {
            s.notice = if remote == "" { word } else { "{word} from {remote}" }
        }
    } else if j.state == .failed and j.error != "" {
        s.trouble = "The remote operation stopped: {j.error}"
    } else if j.state == .cancelled {
        s.trouble = "The remote operation was stopped."
    } else {
        s.trouble = "The remote operation stopped unexpectedly. Check the repository before trying again."
    }
}

/// Stops the remote operation the person started, and says so.
action cancel_remote(g: Repository) (icon: .square, label: "Stop the remote operation") {
    require g.remote_job != "" "No remote operation is running"
    engine.cancel(g.remote_job)
    g.remote_job = ""
    g.remote_op = ""
    g.remote_name = ""
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    s.trouble = "The remote operation was stopped."
}

/// Runs a check this project's host approved, by its id: the host picks the commands, the sources
/// and the bounds, reports how far it has got, and says what it observed once it ends.
// ---------------------------------------------------------------- approved project checks
action run_approved_check(
    g: Repository,
    check_id: Text (label: "Check ID approved by this exact project host"),
) uses check (icon: .list_checks, label: "Run approved project check") {
    require g.check.state != .running "A check is already running"
    g.check.check_id = check_id
    g.check.phase = ""
    g.check.completed = 0
    g.check.total = 0
    g.check.outcome = ""
    g.check.exit_code = .none
    g.check.elapsed_ms = 0
    g.check.output = ""
    g.check.errors = ""
    g.check.refusal = ""
    match engine.check(check_id) {
        .err(why) => {
            g.check.job = ""
            g.check.state = .refused
            g.check.refusal = why
        }
        .ok(job) => {
            g.check.job = job
            g.check.state = .running
        }
    }
}

/// Asks the device how the running check is doing, and keeps what the host reported.
action poll_check(g: Repository) {
    if g.check.state != .running or g.check.job == "" {
        return
    }
    if let j = engine.job(g.check.job) {
        match json.decode<CheckStatus>(j.detail) {
            .err(why) => {
                g.check.job = ""
                g.check.state = .refused
                g.check.refusal = why
            }
            .ok(status) => {
                g.check.phase = status.phase
                g.check.completed = status.completed ?? 0
                g.check.total = status.total ?? 0
                if status.state == "done" {
                    g.check.job = ""
                    g.check.state = .observed
                    g.check.outcome = status.outcome
                    g.check.exit_code = status.exit_code
                    g.check.elapsed_ms = status.elapsed_ms
                    g.check.output = status.output
                    g.check.errors = status.errors
                    g.check.refusal = status.failure
                } else if status.state == "failed" {
                    g.check.job = ""
                    g.check.state = .refused
                    g.check.refusal = status.why
                } else if status.state == "cancelled" {
                    g.check.job = ""
                    g.check.state = .stopped
                }
            }
        }
    } else {
        g.check.job = ""
        g.check.state = .refused
        g.check.refusal = "This device no longer keeps the check"
    }
}

/// A running check is followed once a second, so what the host reports reaches the window
/// without anything polling by hand.
trigger watch_check(g: Repository) every 1s: poll_check(g)

/// Stops the running check; the host ends its commands.
action cancel_check(g: Repository) (icon: .square, label: "Stop check") {
    require g.check.state == .running "No check is running"
    if g.check.job != "" { engine.cancel(g.check.job) }
    g.check.job = ""
    g.check.state = .stopped
}

/// Puts the last check's result away.
action clear_check(g: Repository) {
    g.check.state = .idle
}

/// Reads a commit's changed files and their diffs when it is opened.
// ---------------------------------------------------------------- history
action read_commit(g: Repository, id: Text) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    if s.commit_id == id and s.commit_files != "" {
        return
    }
    match engine.call("commit_detail", json.encode(CommitFilesAsk(path: s.path, commit: id))) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => {
            s.trouble = ""
            s.commit_id = id
            s.commit_files = text
        }
    }
}

/// The key pressed on a commit's row: `Enter` opens its changed files, and `Backspace` or `Escape`
/// steps back out of them, leaving the list of commits as it was.
action commit_on_key(g: Repository, id: Text, key: Text) {
    if key == "Enter" {
        read_commit(g, id)
    } else if key == "Backspace" or key == "Escape" {
        let s = g.sessions.first() ?? g.sessions.insert(Session())
        s.commit_id = ""
        s.commit_files = ""
    }
}

/// Switches how this person reads diffs: one column or two.
// ---------------------------------------------------------------- diff preferences
action toggle_layout(
    g: Repository,
    s: Session,
    layout: DiffLayout = if s.layout == .unified { .split } else { .unified },
) (label: "Diff layout") {
    s.layout = layout
}

/// Turns syntax colouring on or off for this person.
action toggle_syntax(g: Repository, s: Session, on: Bool = not s.highlight) (
    label: "Syntax highlighting",
) {
    s.highlight = on
}

/// Shows or hides the whitespace at the end of diff lines for this person.
action toggle_whitespace(g: Repository, s: Session, on: Bool = not s.whitespace) (
    label: "Show whitespace",
) {
    s.whitespace = on
    refresh(g)
}

/// Wraps long diff lines or lets them run wider than the pane, for this person.
action toggle_wrap(g: Repository, s: Session, wrap: Bool = not s.wrap) (label: "Wrap long lines") {
    s.wrap = wrap
}

/// Puts the trouble banner away.
action clear_trouble(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    s.trouble = ""
}

/// Puts the notice banner away.
action clear_notice(g: Repository) {
    let s = g.sessions.first() ?? g.sessions.insert(Session())
    s.notice = ""
}

/// Reads the repository on the device with `ask`, changing nothing there, and keeps the answer.
// ---------------------------------------------------------------- the one way an answer is kept
action loads(g: Repository, s: Session, ask: Text) {
    match engine.call("load", ask) {
        .err(why) => {
            s.trouble = why
        }
        .ok(text) => keep(g, s, text),
    }
}

/// Keeps what the device answered: the repository as it now stands, and the working tree as the
/// device read it.
action keep(g: Repository, s: Session, text: Text) {
    match json.decode<Loaded>(text) {
        .err(why) => {
            s.trouble = why
        }
        .ok(loaded) => {
            s.trouble = ""
            g.path = s.path
            g.snapshot = text
            g.name = loaded.name
            g.head = loaded.head
            g.workdir = loaded.workdir
            g.author_name = loaded.author_name
            g.author_email = loaded.author_email
            let wire = changes_of(loaded.changes)
            let old = s.changes
            for c in old {
                if not wire.any(w -> change_key_of(w) == change_key(c)) { s.changes.remove(c) }
            }
            for w in wire {
                let c = s.changes.find(x -> change_key(x) == change_key_of(w))
                    ?? s.changes.insert(Change())
                c.path = w.path
                c.old_path = w.old_path
                c.area = w.area
                c.kind = w.kind
                c.additions = w.additions
                c.deletions = w.deletions
                c.binary = w.binary
                c.ours = w.ours
                c.theirs = w.theirs
                c.language = w.language
                c.unified = w.unified
                c.left = w.left
                c.right = w.right
                c.hunks = json.encode(w.hunks)
            }
        }
    }
}

/// A duration as a person reads it: `840 ms`, `12.40 s`.
fn took(ms: Int) -> Text {
    if ms < 1000 {
        return "{ms} ms"
    }
    let hundredths = ms / 10
    let part = hundredths % 100
    if part < 10 {
        return "{hundredths / 100}.0{part} s"
    }
    "{hundredths / 100}.{part} s"
}

/// What the host observed of the last check, in a few words.
fn check_title(c: ApprovedCheck) -> Text {
    if c.outcome == "exited" {
        if let code = c.exit_code {
            if code == 0 {
                return "{c.check_id} passed"
            }
            return "{c.check_id} failed with exit code {code}"
        }
        return "{c.check_id} ended on a signal"
    }
    if c.outcome == "timed_out" {
        return "{c.check_id} ran out of time"
    }
    if c.outcome == "output_limit" {
        return "{c.check_id} printed more than the host keeps"
    }
    if c.outcome == "cancelled" {
        return "{c.check_id} was stopped by the host"
    }
    "The host could not run {c.check_id}"
}

/// Whether the last check's commands ended well.
fn check_passed(c: ApprovedCheck) -> Bool {
    c.outcome == "exited" and c.exit_code == .some(0)
}

/// The share of the running check the host reported done, when it said how much there is.
fn check_share(c: ApprovedCheck) -> Option<Float> {
    if c.total <= 0 {
        return .none
    }
    .some(c.completed.to_float() / c.total.to_float())
}

/// Whether this person's working tree has something staged for the next commit.
fn staged_ready(g: Repository) -> Bool {
    if let s = g.sessions.first() { s.changes.any(c -> c.area == .staged) } else { false }
}

// ---------------------------------------------------------------- what the window reads
fn badge(k: ChangeKind) -> Text {
    if k == .added {
        "A"
    } else if k == .modified {
        "M"
    } else if k == .deleted {
        "D"
    } else if k == .renamed { "R" } else if k == .untracked { "U" } else { "C" }
}

fn area_word(a: ChangeArea) -> Text {
    if a == .staged { "Staged" } else if a == .unstaged { "Not staged" } else { "Untracked" }
}

fn file_name(path: Text) -> Text {
    path.split("/").last() ?? path
}

fn lines_changed(additions: Int, deletions: Int) -> Text {
    if additions == 0 and deletions == 0 {
        ""
    } else if deletions == 0 {
        "+{additions}"
    } else if additions == 0 { "−{deletions}" } else { "+{additions} −{deletions}" }
}

fn short(id: Text) -> Text {
    if id.len() > 7 { id.slice(0, 7) } else { id }
}

fn repo_title(g: Repository) -> Text {
    if g.name == "" { "Git" } else { g.name }
}

fn tracking(b: WireBranch) -> Text {
    if let up = b.upstream {
        if b.ahead == 0 and b.behind == 0 {
            "Up to date with {up}"
        } else if b.behind == 0 {
            "{b.ahead} ahead of {up}"
        } else if b.ahead == 0 {
            "{b.behind} behind {up}"
        } else {
            "{b.ahead} ahead, {b.behind} behind {up}"
        }
    } else { "No upstream branch" }
}

fn stash_title(st: WireStash) -> Text {
    let message = st.message
    for prefix in ["WIP on ", "On "] {
        if message.starts_with(prefix) {
            let rest = message.slice(prefix.len(), message.len())
            let at = rest.find(": ")
            if let colon = at {
                return rest.slice(colon + 2, rest.len())
            }
        }
    }
    message
}

fn reflow(body: Text) -> Text {
    body
        .split("\n\n")
        .map(p -> p.split("\n").map(l -> l.trim()).filter(l -> l != "").join(" "))
        .filter(p -> p != "")
        .join("\n\n")
}

fn said_ago(seconds: Int) -> Text {
    if seconds < 60 {
        "just now"
    } else if seconds < 3600 {
        "{seconds / 60} min ago"
    } else if seconds < 86400 { "{seconds / 3600} h ago" } else { "{seconds / 86400} d ago" }
}

fn hunks_of_change(c: Change) -> List<Hunk> {
    hunks_of(texts(c.hunks))
}

fn ref_names(c: WireCommit) -> Text {
    let shown = refs_of(c.refs).filter(r -> r.kind != .head)
    if shown.is_empty() { "" } else { shown.first()?.name ?? "" }
}

fn commit_files(g: Repository, s: Session) -> List<CommitFile> {
    if s.commit_files == "" { [] } else { files_of(texts(s.commit_files)) }
}

fn language_of(s: Session, token: Text) -> Text {
    if not s.highlight { "plain" } else { token }
}

// ---------------------------------------------------------------- the window
view changes(g: Repository) {
    state chosen: Option<Change> = .none
    state message: Text = ""
    if let s = g.sessions.first() {
        ui.NavShell(
            content_kind: .list,
            title: repo_title(g),
            overview_title: "Changes",
            overview_count: s.changes.len(),
            sidebar_title: "Git",
        ) {
            actions: {
                ui.Button("Refresh", icon: .refresh_ccw) {
                    on press: refresh(g)
                }
                ui.Button("Fetch", icon: .cloud_download) {
                    on press: fetch(g, "")
                }
                ui.Button("Pull", icon: .arrow_down) {
                    on press: pull(g, "")
                }
                ui.Button("Push", icon: .arrow_up) {
                    on press: push(g, "")
                }
                ui.Button("Diff layout", icon: .columns_2) {
                    on press: toggle_layout(g, s)
                }
                ui.Button("Syntax highlighting", icon: .code, toggled: s.highlight) {
                    on press: toggle_syntax(g, s)
                }
                ui.Button("Show whitespace", icon: .ruler, toggled: s.whitespace) {
                    on press: toggle_whitespace(g, s)
                }
                ui.Button("Wrap long lines", icon: .arrow_up_down, toggled: s.wrap) {
                    on press: toggle_wrap(g, s)
                }
                ui.Button("Stash changes", icon: .archive) {
                    on press: save_stash(g, "")
                }
                ui.Button("Run approved project check", icon: .list_checks) {
                    on press: run_approved_check(g)
                }
            }
            overview_actions: {
                if s.changes.any(c -> c.area != .staged) {
                    ui.Button("Stage all", icon: .plus) {
                        on press: stage_all(g)
                    }
                }
                if s.changes.any(c -> c.area == .staged) {
                    ui.Button("Unstage all", icon: .minus) {
                        on press: unstage_all(g)
                    }
                }
            }
            overview: {
                if s.path == "" {
                    open_page(g)
                } else if s.browsing {
                    picker(g, s)
                } else {
                    ui.Docked {
                        foot: {
                            commit_box(g, s, message)
                        }
                        stack {
                            if g.remote_job != "" {
                                ui.Banner(
                                    title: if g.remote_op == "fetch" {
                                        "Fetching from remote…"
                                    } else if g.remote_op == "pull" {
                                        "Pulling from remote…"
                                    } else { "Pushing to remote…" },
                                    kind: .info,
                                    icon: .cloud_download,
                                    action: "Stop",
                                ) {
                                    on act: cancel_remote(g)
                                }
                            }
                            if s.trouble != "" {
                                ui.Banner(title: s.trouble, kind: .error, icon: .circle_alert) {
                                    on dismiss: clear_trouble(g)
                                }
                            }
                            if s.notice != "" {
                                ui.Banner(title: s.notice, kind: .success) {
                                    on dismiss: clear_notice(g)
                                }
                            }
                            check_card(g)
                            changes_list(g, s, chosen)
                        }
                    }
                }
            }
            content: {
                if let c = chosen ?? s.changes.first() {
                    change_detail(g, s, c)
                } else if s.path != "" and not s.browsing {
                    ui.EmptyState("Select a file to see its changes", icon: .file_code)
                }
            }
        }
        if s.pending_switch != "" {
            ui.ModalDialog(
                title: "Switch to {s.pending_switch}?",
                icon: .git_branch,
                description: "You have uncommitted changes. Stash them and switch, or keep them; switching fails when a change would be overwritten.",
                open: true,
                primary: "Stash and switch",
                secondary: "Cancel",
                tertiary: "Keep my changes",
            ) {
                on act_primary: switch_stashing(g)
                on act_secondary: cancel_switch(g)
                on act_tertiary: switch_carrying(g)
                on dismiss: cancel_switch(g)
            }
        }
    } else {
        open_page(g)
    }
}

/// The last approved check: its phase and progress while the host runs it, with a way to stop
/// it, then what the host observed, or why it did not run.
view check_card(g: Repository) {
    let c = g.check
    if c.state == .running {
        stack(gap: .related) {
            ui.Banner(
                title: "Running {c.check_id}",
                message: if c.phase == "" { "Waiting for the project's host" } else { c.phase },
                kind: .info,
                icon: .list_checks,
                action: "Stop",
            ) {
                on act: cancel_check(g)
            }
            ui.ProgressBar(check_share(c), label: "Check under way")
        }
    } else if c.state == .observed {
        stack(gap: .related) {
            ui.Banner(
                title: check_title(c),
                message: "Took {took(c.elapsed_ms)}",
                kind: if check_passed(c) { .success } else { .warning },
                icon: if check_passed(c) { .circle_check } else { .list_checks },
            ) {
                on dismiss: clear_check(g)
            }
            if c.output != "" or c.errors != "" or c.refusal != "" {
                ui.Section("What the check printed", collapsible: true) {
                    for line in c.output.split("\n") {
                        ui.Text(line, role: .code)
                    }
                    for line in c.errors.split("\n") {
                        ui.Text(line, role: .code)
                    }
                    for line in c.refusal.split("\n") {
                        ui.Text(line, role: .code)
                    }
                }
            }
        }
    } else if c.state == .stopped {
        ui.Banner(title: "{c.check_id} was stopped", kind: .neutral, icon: .square) {
            on dismiss: clear_check(g)
        }
    } else if c.state == .refused {
        ui.Banner(
            title: "No project's approved check ran",
            message: c.refusal,
            kind: .error,
            icon: .circle_alert,
        ) {
            on dismiss: clear_check(g)
        }
    }
}

view changes_list(g: Repository, s: Session, chosen: State<Option<Change>>) {
    if s.changes.is_empty() {
        ui.EmptyState("Your working tree is clean", icon: .check)
    } else {
        ui.VirtualList(selection_follows_focus: true, in_column: true, narrow: space.compact) {
            on select(x: Change): set(chosen, .some(x))
            for c in s.changes {
                ui.ListItem(
                    item: c,
                    group: .some(area_word(c.area)),
                    keys: if c.kind == .conflicted { ["s", "o", "t"] } else { ["s"] },
                ) {
                    on shortcut(k): change_on_key(g, c, k)
                    change_row(g, c)
                    menu: {
                        if c.area == .staged {
                            ui.MenuItem("Unstage file", icon: .minus) {
                                on choose: unstage_file(g, c)
                            }
                        } else {
                            ui.MenuItem("Stage file", icon: .plus) {
                                on choose: stage_file(g, c)
                            }
                        }
                        if c.area != .staged and c.kind != .conflicted {
                            ui.MenuItem("Discard file changes", icon: .undo_2, destructive: true) {
                                on choose: discard_file(g, c)
                            }
                        }
                    }
                }
            }
        }
    }
}

view change_row(g: Repository, c: Change) {
    ui.ListRow(
        title: file_name(c.path),
        prefix: badge(c.kind),
        secondary: c.path,
        meta: lines_changed(c.additions, c.deletions),
        meta_tone: if c.kind == .conflicted { .danger } else { .neutral },
        icon: if c.kind == .conflicted { .circle_alert } else { .file_code },
        tone: if c.kind == .conflicted { .danger } else { .neutral },
    ) {
        trailing: {
            if c.area == .staged {
                ui.Button(
                    "Unstage",
                    icon: .minus,
                    icon_only: true,
                    size: .small,
                    description: "Unstage {c.path}",
                ) {
                    on press: unstage_file(g, c)
                }
            } else {
                ui.Button(
                    "Stage",
                    icon: .plus,
                    icon_only: true,
                    size: .small,
                    description: "Stage {c.path}",
                ) {
                    on press: stage_file(g, c)
                }
            }
        }
    }
}

view commit_box(g: Repository, s: Session, message: State<Text>) {
    stack(padding: .section, gap: .related) {
        ui.TextInput(bind: message, placeholder: "Commit message", entry: true, key: "c")
        ui.Button(
            if g.head == "" { "Commit" } else { "Commit to {g.head}" },
            icon: .git_commit_horizontal,
            role: .primary,
        ) {
            on press: commit(g, message)
        }
    }
}

view change_detail(g: Repository, s: Session, c: Change) {
    ui.Scroll {
        stack(padding: .section, gap: .related) {
            row(gap: .related, align: .center, wrap: true) {
                ui.Text(file_name(c.path), role: .label)
                if c.additions > 0 {
                    ui.Badge("+{c.additions}", tone: .success)
                }
                if c.deletions > 0 {
                    ui.Badge("−{c.deletions}", tone: .danger)
                }
                if let old = c.old_path {
                    ui.Text("Renamed from {old}", role: .caption, secondary: true)
                }
                spacer
                if c.kind != .conflicted {
                    if c.area == .staged {
                        ui.Button("Unstage file", icon: .minus, size: .small) {
                            on press: unstage_file(g, c)
                        }
                    } else {
                        ui.Button("Stage file", icon: .plus, size: .small) {
                            on press: stage_file(g, c)
                        }
                    }
                }
                if c.area != .staged and c.kind != .conflicted {
                    ui.Button("Discard file", icon: .undo_2, role: .destructive, size: .small) {
                        on press: discard_file(g, c)
                    }
                }
            }
            if c.kind == .conflicted {
                ui.Banner(
                    title: "Both sides changed this file",
                    message: "Keeping ours drops {c.theirs} lines of theirs; keeping theirs drops {c.ours} lines of yours.",
                    kind: .warning,
                    icon: .circle_alert,
                )
                row(gap: .related) {
                    ui.Button("Take ours") {
                        on press: resolve_conflict(g, c, true)
                    }
                    ui.Button("Take theirs") {
                        on press: resolve_conflict(g, c, false)
                    }
                }
            }
            change_code(s, c)
            if c.hunks != "" and c.kind != .conflicted {
                for h in hunks_of_change(c) {
                    row(gap: .related, align: .center) {
                        ui.Text(h.header, role: .code, secondary: true, truncate: true)
                        spacer
                        if c.area == .staged {
                            ui.Button("Unstage hunk", icon: .minus, size: .small) {
                                on press: unstage_hunk(g, c, h.index)
                            }
                        } else {
                            ui.Button("Stage hunk", icon: .plus, size: .small) {
                                on press: stage_hunk(g, c, h.index)
                            }
                            ui.Button(
                                "Discard hunk",
                                icon: .undo_2,
                                role: .destructive,
                                size: .small,
                            ) {
                                on press: discard_hunk(g, c, h.index)
                            }
                        }
                    }
                }
            }
        }
    }
}

view change_code(s: Session, c: Change) {
    if c.binary {
        ui.EmptyState("Binary file", message: "There is no text to compare.")
    } else if c.unified == "" {
        ui.EmptyState("No changes to show", message: "Only the file's permissions changed.")
    } else if s.layout == .split and not space.compact {
        row(gap: .related, fill) {
            code_column(s, c, true)
            code_column(s, c, false)
        }
    } else if s.wrap {
        ui.CodeEditor(
            bind: c.unified,
            read_only: true,
            gutter: false,
            language: if language_of(s, c.language) == "rust" {
                .rust
            } else if language_of(s, c.language) == "sql" {
                .sql
            } else if language_of(s, c.language) == "json" {
                .json
            } else if language_of(s, c.language) == "tilolang" {
                .tilolang
            } else if language_of(s, c.language) == "typst" { .typst } else { .plain_text },
        )
    } else {
        ui.Scroll(across: true) {
            stack(min: 160rem) {
                ui.CodeEditor(
                    bind: c.unified,
                    read_only: true,
                    gutter: false,
                    language: if language_of(s, c.language) == "rust" {
                        .rust
                    } else if language_of(s, c.language) == "sql" {
                        .sql
                    } else if language_of(s, c.language) == "json" {
                        .json
                    } else if language_of(s, c.language) == "tilolang" {
                        .tilolang
                    } else if language_of(s, c.language) == "typst" { .typst } else { .plain_text },
                )
            }
        }
    }
}

view code_column(s: Session, c: Change, old: Bool) {
    if s.wrap {
        if old {
            ui.CodeEditor(
                bind: c.left,
                read_only: true,
                gutter: false,
                language: if language_of(s, c.language) == "rust" {
                    .rust
                } else if language_of(s, c.language) == "sql" {
                    .sql
                } else if language_of(s, c.language) == "json" {
                    .json
                } else if language_of(s, c.language) == "tilolang" {
                    .tilolang
                } else if language_of(s, c.language) == "typst" { .typst } else { .plain_text },
            )
        } else {
            ui.CodeEditor(
                bind: c.right,
                read_only: true,
                gutter: false,
                language: if language_of(s, c.language) == "rust" {
                    .rust
                } else if language_of(s, c.language) == "sql" {
                    .sql
                } else if language_of(s, c.language) == "json" {
                    .json
                } else if language_of(s, c.language) == "tilolang" {
                    .tilolang
                } else if language_of(s, c.language) == "typst" { .typst } else { .plain_text },
            )
        }
    } else if old {
        ui.Scroll(across: true) {
            stack(min: 80rem) {
                ui.CodeEditor(bind: c.left, read_only: true, gutter: false, language: .plain_text)
            }
        }
    } else {
        ui.Scroll(across: true) {
            stack(min: 80rem) {
                ui.CodeEditor(bind: c.right, read_only: true, gutter: false, language: .plain_text)
            }
        }
    }
}

view open_page(g: Repository) {
    state typed: Text = ""
    stack(padding: .group, gap: .related, max: 40rem) {
        ui.Text("Open a repository", role: .title)
        ui.Text(
            "Enter the path of a git repository on this computer.",
            role: .body,
            secondary: true,
        )
        ui.TextInput(bind: typed, placeholder: "/home/you/project", entry: true) {
            on submit: open_repository(g, typed)
        }
        row(gap: .related) {
            ui.Button("Open", icon: .git_branch, role: .primary) {
                on press: open_repository(g, typed)
            }
            ui.Button("Choose…", icon: .folder_open) {
                on press: choose_repository(g)
            }
        }
    }
}

view picker(g: Repository, s: Session) {
    stack(padding: .section, gap: .related, fill) {
        row(gap: .related, align: .center) {
            ui.Text(s.folder, role: .label, truncate: true)
            spacer
            ui.Button("Open this folder", icon: .git_branch, role: .primary) {
                on press: open_browsed(g)
            }
            ui.Button("Cancel") {
                on press: close_picker(g)
            }
        }
        if s.trouble != "" {
            ui.Banner(title: s.trouble, kind: .error, icon: .circle_alert)
        }
        ui.VirtualList {
            for e in s.entries {
                ui.ListItem(item: e) {
                    ui.ListRow(title: e.name, icon: .folder) {
                        on activate: read_folder(s, e.path)
                    }
                }
            }
        }
    }
}

/// The window's History: the commits, and the page of the commit chosen.
view history(g: Repository) {
    state chosen: Text = ""
    let s = g.sessions.first()
    let commits = committed(g)
    let reading = commits.find(x -> x.sha == chosen) ?? commits.first()
    let read = if let w = s { w.commit_id == full_id(reading) } else { false }
    let files = if let w = s { commit_files(g, w) } else { [] }
    ui.NavShell(
        content_kind: .document,
        title: "History",
        overview_title: "History",
        overview_count: commits.len(),
        sidebar_title: "Git",
    ) {
        overview: {
            if commits.is_empty() {
                ui.EmptyState("No commits yet", icon: .git_commit_horizontal)
            } else {
                ui.VirtualList(
                    selection_follows_focus: true,
                    in_column: true,
                    narrow: space.compact,
                ) {
                    on select_value(id: Text): set(chosen, id).then(read_commit(g, id))
                    for c in commits {
                        ui.ListItem(value: c.sha, keys: ["Enter", "Backspace", "Escape"]) {
                            on shortcut(k): commit_on_key(g, c.sha, k)
                            commit_row(c)
                        }
                    }
                }
            }
        }
        content: {
            if let found = reading {
                commit_page(found, read, files)
            } else {
                ui.EmptyState("No commits yet", icon: .git_commit_horizontal)
            }
        }
    }
}

view commit_row(c: WireCommit) {
    ui.ListRow(
        title: c.summary,
        prefix: ref_names(c),
        secondary: "{c.author_name}, {said_ago(c.age)}",
        meta: short(c.sha),
        icon: .git_commit_horizontal,
    )
}

view commit_page(c: WireCommit, read: Bool, files: List<CommitFile>) {
    ui.Scroll {
        stack(padding: .group, gap: .section, max: 60rem) {
            ui.Text(c.summary, role: .title)
            row(gap: .related, align: .center) {
                ui.Avatar(name: c.author_name)
                stack(gap: .tight) {
                    ui.Text(c.author_name, role: .body)
                    ui.Text(c.author_email, role: .caption, secondary: true)
                }
            }
            stack(gap: .tight) {
                row(gap: .related) {
                    ui.Text("Commit", role: .caption, secondary: true)
                    ui.Text(c.sha, role: .code)
                }
                row(gap: .related) {
                    ui.Text("Date", role: .caption, secondary: true)
                    ui.Text(c.date, role: .body)
                }
                if not c.parents.is_empty() {
                    row(gap: .related) {
                        ui.Text("Parents", role: .caption, secondary: true)
                        ui.Text(c.parents.map(p -> short(p)).join(", "), role: .code)
                    }
                }
                let refs = refs_of(c.refs).filter(r -> r.kind != .head)
                if not refs.is_empty() {
                    row(gap: .related, wrap: true) {
                        ui.Text("Branches and tags", role: .caption, secondary: true)
                        for r in refs {
                            ui.Chip(r.name, icon: if r.kind == .tag { .tag } else { .git_branch })
                        }
                    }
                }
            }
            let body = reflow(c.body)
            if body != "" {
                ui.Text(body, role: .body)
            }
            if read and not files.is_empty() {
                ui.Text("Changed files", role: .subheading)
                for f in files {
                    commit_file(f)
                }
            }
        }
    }
}

view commit_file(f: CommitFile) {
    stack(gap: .related) {
        ui.ListRow(
            title: file_name(f.path),
            prefix: badge(f.kind),
            secondary: f.path,
            meta: lines_changed(f.additions, f.deletions),
            icon: .file_code,
        )
        if f.binary {
            ui.EmptyState("Binary file", message: "There is no text to compare.")
        } else if f.unified == "" {
            ui.EmptyState("No changes to show", message: "Only the file's permissions changed.")
        } else {
            ui.CodeEditor(
                bind: f.unified,
                read_only: true,
                gutter: false,
                language: if f.language == "rust" {
                    .rust
                } else if f.language == "sql" {
                    .sql
                } else if f.language == "json" {
                    .json
                } else if f.language == "tilolang" {
                    .tilolang
                } else if f.language == "typst" { .typst } else { .plain_text },
            )
        }
    }
}

/// The window's Branches: local and remote branches, working directories, tags, stashes and
/// remotes, each a section with its rows and the form that makes another.
view branches(g: Repository) {
    state new_branch: Text = ""
    state wt_branch: Text = ""
    state wt_path: Text = ""
    state wt_start: Text = "HEAD"
    ui.NavShell(content_kind: .page, title: "Branches", sidebar_title: "Git") {
        content: {
            ui.Scroll {
                stack(padding: .group, gap: .group, max: 64rem) {
                    ui.Section("Branches") {
                        ui.VirtualList(name: "Branches", selection_follows_focus: true) {
                            for b in branched(g).filter(x -> not x.is_remote) {
                                ui.ListItem(value: b.name, keys: ["Enter", "Delete"]) {
                                    on shortcut(k): branch_on_key(g, b.name, b.is_remote, k)
                                    branch_row(g, b)
                                }
                            }
                        }
                        row(gap: .related) {
                            ui.TextInput(
                                bind: new_branch,
                                placeholder: "New branch name",
                                entry: true,
                                key: "b",
                            ) {
                                on submit: create_branch(g, new_branch)
                            }
                            ui.Button("Create branch", icon: .git_branch) {
                                on press: create_branch(g, new_branch)
                            }
                        }
                    }
                    let remote_branches = branched(g).filter(x -> x.is_remote)
                    if not remote_branches.is_empty() {
                        ui.Section("Remote branches") {
                            ui.VirtualList(name: "Remote branches", selection_follows_focus: true) {
                                for b in remote_branches {
                                    ui.ListItem(value: b.name) {
                                        branch_row(g, b)
                                    }
                                }
                            }
                        }
                    }
                    ui.Section("Working directories") {
                        if worktree_error(g) != "" {
                            ui.Text(worktree_error(g), role: .caption, secondary: true)
                        }
                        for t in worked(g) {
                            ui.ListRow(
                                title: t.head,
                                secondary: t.path,
                                icon: .folder,
                                selected: t.current,
                            )
                        }
                        ui.TextInput(bind: wt_branch, placeholder: "New worktree branch")
                        ui.TextInput(bind: wt_path, placeholder: "Absolute destination path")
                        ui.TextInput(
                            bind: wt_start,
                            placeholder: "Start point (branch, tag or commit)",
                        )
                        ui.Button("Create worktree", icon: .folder_plus) {
                            on press: create_worktree(g, wt_path, wt_branch, wt_start)
                        }
                    }
                    let tags = tagged(g)
                    if not tags.is_empty() {
                        ui.Section("Tags") {
                            for t in tags {
                                ui.ListRow(
                                    title: t.name,
                                    secondary: t.message ?? "",
                                    meta: short(t.target),
                                    icon: .tag,
                                )
                            }
                        }
                    }
                    let stashes = stashed(g)
                    if not stashes.is_empty() {
                        ui.Section("Stashes") {
                            for st in stashes {
                                ui.ListRow(
                                    title: stash_title(st),
                                    secondary: st.branch,
                                    icon: .archive,
                                ) {
                                    on activate: apply_stash(g, st.index)
                                }
                            }
                        }
                    }
                    let remotes = remoted(g)
                    if not remotes.is_empty() {
                        ui.Section("Remotes") {
                            for r in remotes {
                                ui.ListRow(title: r.name, secondary: r.url, icon: .server) {
                                    trailing: {
                                        row(gap: .tight) {
                                            ui.Button(
                                                "Fetch",
                                                icon: .cloud_download,
                                                icon_only: true,
                                                size: .small,
                                            ) {
                                                on press: fetch(g, r.name)
                                            }
                                            ui.Button(
                                                "Pull",
                                                icon: .arrow_down,
                                                icon_only: true,
                                                size: .small,
                                            ) {
                                                on press: pull(g, r.name)
                                            }
                                            ui.Button(
                                                "Push",
                                                icon: .arrow_up,
                                                icon_only: true,
                                                size: .small,
                                            ) {
                                                on press: push(g, r.name)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

view branch_row(g: Repository, b: WireBranch) {
    ui.ListRow(
        title: b.name,
        secondary: if b.is_remote { short(b.tip) } else { tracking(b) },
        meta: short(b.tip),
        icon: .git_branch,
        selected: b.is_head,
    ) {
        on activate: checkout_branch(g, b.name)
        trailing: {
            if not b.is_head and not b.is_remote {
                ui.Button(
                    "Delete",
                    icon: .trash,
                    icon_only: true,
                    role: .destructive,
                    size: .small,
                    description: "Delete {b.name}",
                ) {
                    on press: delete_branch(g, b.name)
                }
            }
        }
    }
}
