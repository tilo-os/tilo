//! The Git app's engine, with no UI: it reads a repository on disk into what the window shows —
//! its changes and their diffs, its history, branches, tags, stashes, remotes and working
//! directories — and performs the changes a person asks of it.
//!
//! The app runs sandboxed and holds no repository, so it asks this engine by name and keeps the
//! JSON answers in its own records. [`run`] is the one entry point a host answers an `engine.call`
//! with; fetch, pull and push answer through [`fetch`], [`pull`] and [`push`], which a host runs as
//! a job.

mod diff;
mod graph;
mod http_transport;
pub mod model;
mod repo;
mod ssh_transport;
mod transport;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use model::{ChangeArea, ChangeKind, DiffLineKind, FileChange, FileDiff, RepoModel};

/// What one change of the working tree is, and how its diff reads in each layout.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChangeView {
    pub path: String,
    pub old_path: Option<String>,
    pub area: ChangeArea,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    /// What keeping each side of a conflict would discard, in lines.
    pub ours: u32,
    pub theirs: u32,
    /// The file's language token, for the view's colouring.
    pub language: String,
    /// The change in one column: each line's old and new number, its marker and its words.
    pub unified: String,
    /// The old side of a side-by-side reading, aligned row for row with `right`.
    pub left: String,
    /// The new side of a side-by-side reading.
    pub right: String,
    pub hunks: Vec<String>,
}

/// One hunk of a diff, which can be staged or discarded on its own.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HunkView {
    pub index: usize,
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
}

/// One commit as the app reads it: its refs written as texts, since a list of records is a list
/// of resources to the app's decoder.
#[derive(Serialize)]
struct CommitOut<'a> {
    id: &'a str,
    summary: &'a str,
    body: &'a str,
    author_name: &'a str,
    author_email: &'a str,
    time: i64,
    age: i64,
    date: &'a str,
    parents: &'a [String],
    refs: Vec<String>,
    column: usize,
}

fn commit_out(commit: &model::Commit) -> CommitOut<'_> {
    CommitOut {
        id: &commit.id,
        summary: &commit.summary,
        body: &commit.body,
        author_name: &commit.author_name,
        author_email: &commit.author_email,
        time: commit.time,
        age: commit.age,
        date: &commit.date,
        parents: &commit.parents,
        refs: commit.refs.iter().map(write).collect(),
        column: commit.column,
    }
}

/// Everything a repository on disk holds, as the window reads it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Loaded {
    /// What the op that answered it made, in a few words: a commit's or stash's short id; empty
    /// for a plain read.
    pub answer: String,
    pub name: String,
    pub head: String,
    pub workdir: String,
    pub author_name: String,
    pub author_email: String,
    pub graph_width: usize,
    /// Each item written as its own JSON text: a list of records is a list of resources to the
    /// app's decoder, and only text crosses as a value.
    pub changes: Vec<String>,
    pub commits: Vec<String>,
    pub branches: Vec<String>,
    pub tags: Vec<String>,
    pub stashes: Vec<String>,
    pub remotes: Vec<String>,
    pub worktrees: Vec<String>,
    pub worktree_error: Option<String>,
}

/// The repository read as an answer: what a change replies with, so the app reads the state the
/// change left instead of asking again.
fn loaded_json(path: &Path, whitespace: bool, made: &str) -> Result<String, String> {
    let (model, diffs) = repo::load(path)
        .ok_or_else(|| format!("no git repository at or above {}", path.display()))?;
    let changes = model
        .all_changes()
        .into_iter()
        .map(|change| {
            let diff = diffs.get(&model::DiffKey::for_area(change.area, &change.path));
            write(&change_view(path, change, diff, whitespace))
        })
        .collect();
    answer(Loaded {
        answer: made.to_owned(),
        name: model.name,
        head: model.head,
        workdir: model.workdir,
        author_name: model.author_name,
        author_email: model.author_email,
        graph_width: model.graph_width,
        changes,
        commits: model
            .commits
            .iter()
            .map(|commit| write(&commit_out(commit)))
            .collect(),
        branches: model.branches.iter().map(write).collect(),
        tags: model.tags.iter().map(write).collect(),
        stashes: model.stashes.iter().map(write).collect(),
        remotes: model.remotes.iter().map(write).collect(),
        worktrees: model.worktrees.iter().map(write).collect(),
        worktree_error: model.worktree_error,
    })
}

/// One file's changes in a commit, as the same view the working tree uses.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffView {
    pub path: String,
    pub old_path: Option<String>,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    pub language: String,
    pub unified: String,
    pub left: String,
    pub right: String,
}

/// The network runtime the SSH and HTTP transports run on, made once.
fn net_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("tilo-git-net")
            .enable_all()
            .build()
            .expect("the git engine's network runtime starts")
    })
}

fn absolute(path: &str) -> Result<PathBuf, String> {
    // `~` is this device's home folder, as a folder the person chose reads it; anything else
    // must be absolute, so a repository is never opened relative to wherever a request ran.
    let path = if let Some(rest) = path.strip_prefix('~') {
        let home =
            std::env::var_os("HOME").ok_or_else(|| "this device has no home folder".to_owned())?;
        PathBuf::from(home).join(rest.trim_start_matches('/'))
    } else {
        PathBuf::from(path)
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("the path must be absolute".to_owned())
    }
}

/// Open `path` (walking up to its `.git`) and read it whole; `None` when no repository is there.
#[must_use]
pub fn load(path: &Path) -> Option<(RepoModel, model::DiffMap)> {
    repo::load(path)
}

fn change_view(
    repo_path: &Path,
    change: &FileChange,
    diff: Option<&FileDiff>,
    whitespace: bool,
) -> ChangeView {
    let (ours, theirs) = if change.kind == ChangeKind::Conflicted {
        repo::conflict_cost(repo_path, &change.path)
            .map_or((0, 0), |cost| (cost.ours as u32, cost.theirs as u32))
    } else {
        (0, 0)
    };
    let (binary, language, unified, left, right, hunks) = match diff {
        Some(diff) => (
            diff.binary,
            language_token(&diff.language),
            render_unified(diff, whitespace),
            render_side(diff, true, whitespace),
            render_side(diff, false, whitespace),
            diff.hunks
                .iter()
                .enumerate()
                .map(|(index, hunk)| {
                    write(&HunkView {
                        index,
                        header: hunk.header.clone(),
                        old_start: hunk.old_start,
                        new_start: hunk.new_start,
                    })
                })
                .collect(),
        ),
        None => (
            false,
            language_token(""),
            String::new(),
            String::new(),
            String::new(),
            Vec::new(),
        ),
    };
    ChangeView {
        path: change.path.clone(),
        old_path: change.old_path.clone(),
        area: change.area,
        kind: change.kind,
        additions: change.additions,
        deletions: change.deletions,
        binary,
        ours,
        theirs,
        language,
        unified,
        left,
        right,
        hunks,
    }
}

/// The language token the app's editor colours: a file's extension where the toolkit knows it,
/// plain otherwise.
fn language_token(token: &str) -> String {
    match token {
        "rs" | "rust" => "rust",
        "sql" => "sql",
        "json" => "json",
        "tl" => "tilolang",
        "typ" | "typst" => "typst",
        _ => "plain",
    }
    .to_owned()
}

/// One line in a unified reading: its old and new numbers, its marker, and its words.
fn unified_line(kind: DiffLineKind, old: Option<u32>, new: Option<u32>, content: &str) -> String {
    let old = old.map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
    let new = new.map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
    let marker = match kind {
        DiffLineKind::Addition => '+',
        DiffLineKind::Deletion => '-',
        _ => ' ',
    };
    format!("{old} {new} {marker}{content}")
}

fn render_unified(diff: &FileDiff, whitespace: bool) -> String {
    let mut text = String::new();
    let mut first = true;
    for line in &diff.lines {
        match line.kind {
            DiffLineKind::Meta => continue,
            DiffLineKind::Hunk => {
                if !first {
                    text.push('\n');
                }
                text.push_str(&line.content);
                text.push('\n');
            }
            kind => {
                text.push_str(&unified_line(
                    kind,
                    line.old_lineno,
                    line.new_lineno,
                    &shown(&line.content, whitespace),
                ));
                text.push('\n');
            }
        }
        first = false;
    }
    text
}

/// One side of a side-by-side reading, a row for each paired line: a deletion pairs with the
/// addition after it, a context line spans both sides and a hunk heads both. Rows missing on one
/// side stay blank there, so the two readings line up row for row.
fn render_side(diff: &FileDiff, old: bool, whitespace: bool) -> String {
    let mut text = String::new();
    let lines = &diff.lines;
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            DiffLineKind::Meta => i += 1,
            DiffLineKind::Hunk => {
                let line = &lines[i];
                text.push_str(&side_line(
                    old,
                    line.old_lineno,
                    line.new_lineno,
                    ' ',
                    &shown(&line.content, whitespace),
                ));
                text.push('\n');
                i += 1;
            }
            DiffLineKind::Context => {
                let line = &lines[i];
                text.push_str(&side_line(
                    old,
                    line.old_lineno,
                    line.new_lineno,
                    ' ',
                    &shown(&line.content, whitespace),
                ));
                text.push('\n');
                i += 1;
            }
            DiffLineKind::Deletion => {
                let del_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Deletion {
                    i += 1;
                }
                let add_start = i;
                while i < lines.len() && lines[i].kind == DiffLineKind::Addition {
                    i += 1;
                }
                let dels = add_start - del_start;
                let adds = i - add_start;
                for k in 0..dels.max(adds) {
                    let row = if old {
                        (k < dels).then(|| &lines[del_start + k])
                    } else {
                        (k < adds).then(|| &lines[add_start + k])
                    };
                    match row {
                        Some(line) => {
                            let marker = if old { '-' } else { '+' };
                            text.push_str(&side_line(
                                old,
                                line.old_lineno,
                                line.new_lineno,
                                marker,
                                &shown(&line.content, whitespace),
                            ));
                        }
                        None => text.push_str("                         "),
                    }
                    text.push('\n');
                }
            }
            DiffLineKind::Addition => {
                let line = &lines[i];
                if old {
                    text.push_str("                         ");
                } else {
                    text.push_str(&side_line(
                        old,
                        line.old_lineno,
                        line.new_lineno,
                        '+',
                        &shown(&line.content, whitespace),
                    ));
                }
                text.push('\n');
                i += 1;
            }
        }
    }
    text
}

/// A line's words, with the whitespace at the end of them shown as a middot each when the person
/// asked for it.
fn shown(content: &str, whitespace: bool) -> String {
    if !whitespace {
        return content.to_owned();
    }
    let trimmed = content.trim_end_matches([' ', '\t']);
    let mut out = trimmed.to_owned();
    for tail in content[trimmed.len()..].chars() {
        out.push(if tail == '\t' { '\u{2192}' } else { '\u{00b7}' });
    }
    out
}

fn side_line(
    old: bool,
    old_no: Option<u32>,
    new_no: Option<u32>,
    marker: char,
    content: &str,
) -> String {
    let number = if old { old_no } else { new_no };
    let number = number.map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
    format!("{number} {marker}{content}")
}

// ---------------------------------------------------------------- the asks
//
// Each is one JSON input an app sends with `engine.call`; the answers are the JSON above.

#[derive(Deserialize)]
struct LoadAsk {
    path: String,
    #[serde(default)]
    whitespace: bool,
}

#[derive(Deserialize)]
struct ChangeAsk {
    path: String,
    area: ChangeArea,
    file: String,
    #[serde(default)]
    whitespace: bool,
}

#[derive(Deserialize)]
struct FileAsk {
    path: String,
    file: String,
}

#[derive(Deserialize)]
struct HunkAsk {
    path: String,
    file: String,
    hunk: usize,
    #[serde(default)]
    unstage: bool,
}

#[derive(Deserialize)]
struct CommitAsk {
    path: String,
    message: String,
}

#[derive(Deserialize)]
struct BranchAsk {
    path: String,
    name: String,
    #[serde(default)]
    checkout: bool,
}

#[derive(Deserialize)]
struct CheckoutAsk {
    path: String,
    name: String,
    #[serde(default)]
    stash: bool,
}

#[derive(Deserialize)]
struct CommitFilesAsk {
    path: String,
    commit: String,
}

#[derive(Deserialize)]
struct CommitDiffAsk {
    path: String,
    commit: String,
    file: String,
}

#[derive(Deserialize)]
struct WorktreeAsk {
    path: String,
    destination: String,
    branch: String,
    start: String,
}

#[derive(Deserialize)]
struct StashSaveAsk {
    path: String,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct StashApplyAsk {
    path: String,
    index: usize,
}

#[derive(Deserialize)]
struct ConflictAsk {
    path: String,
    file: String,
}

#[derive(Deserialize)]
struct ResolveAsk {
    path: String,
    file: String,
    ours: bool,
}

#[derive(Deserialize)]
struct RemoteAsk {
    path: String,
    #[serde(default)]
    remote: String,
}

#[derive(Serialize)]
struct CommitAnswer {
    id: String,
}

#[derive(Serialize)]
struct CheckoutAnswer {
    stash: Option<String>,
}

#[derive(Serialize)]
struct CostAnswer {
    ours: usize,
    theirs: usize,
}

fn ask<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}

fn answer<T: Serialize>(value: T) -> Result<String, String> {
    serde_json::to_string(&value).map_err(|error| format!("the answer does not write: {error}"))
}

/// One item of an answer, as its own JSON text.
fn write<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn read_change(
    path: &Path,
    area: ChangeArea,
    file: &str,
    whitespace: bool,
) -> Result<ChangeView, String> {
    let (model, diffs) =
        repo::load(path).ok_or_else(|| format!("no git repository at {}", path.display()))?;
    let change = [&model.staged, &model.unstaged, &model.untracked]
        .into_iter()
        .flatten()
        .find(|change| change.area == area && change.path == file)
        .ok_or_else(|| format!("{file} has no {area:?} change"))?;
    let diff = diffs.get(&model::DiffKey::for_area(area, file));
    Ok(change_view(path, change, diff, whitespace))
}

/// One `engine.call`: the op's name and its ask, answered with JSON.
///
/// # Errors
///
/// The ask does not read, the path is not absolute, no repository is there, or the repository
/// refuses the change.
pub fn run(op: &str, input: &str) -> Result<String, String> {
    match op {
        "load" => {
            let ask: LoadAsk = ask(input)?;
            loaded_json(&absolute(&ask.path)?, ask.whitespace, "")
        }
        "diff" => {
            let ask: ChangeAsk = ask(input)?;
            answer(read_change(
                &absolute(&ask.path)?,
                ask.area,
                &ask.file,
                ask.whitespace,
            )?)
        }
        "commit_detail" => {
            let ask: CommitFilesAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            let files = repo::commit_files(&path, &ask.commit).map_err(|e| format!("{e:#}"))?;
            let mut detail = Vec::with_capacity(files.len());
            for file in &files {
                let diff = repo::commit_file_diff(&path, &ask.commit, &file.path);
                let view = diff
                    .map(|diff| {
                        serde_json::json!({
                            "path": diff.path,
                            "old_path": file.old_path,
                            "kind": file.kind,
                            "additions": diff.additions(),
                            "deletions": diff.deletions(),
                            "binary": diff.binary,
                            "language": language_token(&diff.language),
                            "unified": render_unified(&diff, false),
                            "left": render_side(&diff, true, false),
                            "right": render_side(&diff, false, false),
                        })
                    })
                    .unwrap_or_else(|_| {
                        serde_json::json!({
                            "path": file.path,
                            "old_path": file.old_path,
                            "kind": file.kind,
                            "additions": file.additions,
                            "deletions": file.deletions,
                            "binary": false,
                            "language": "plain",
                            "unified": "",
                            "left": "",
                            "right": "",
                        })
                    });
                detail.push(view.to_string());
            }
            answer(detail)
        }
        "commit_files" => {
            let ask: CommitFilesAsk = ask(input)?;
            let files = repo::commit_files(&absolute(&ask.path)?, &ask.commit)
                .map_err(|e| format!("{e:#}"))?;
            answer(files.iter().map(write).collect::<Vec<String>>())
        }
        "commit_diff" => {
            let ask: CommitDiffAsk = ask(input)?;
            let diff = repo::commit_file_diff(&absolute(&ask.path)?, &ask.commit, &ask.file)
                .map_err(|e| format!("{e:#}"))?;
            answer(DiffView {
                path: diff.path.clone(),
                old_path: diff.old_path.clone(),
                kind: if diff.is_new {
                    ChangeKind::Added
                } else if diff.is_deleted {
                    ChangeKind::Deleted
                } else {
                    ChangeKind::Modified
                },
                additions: diff.additions(),
                deletions: diff.deletions(),
                binary: diff.binary,
                language: language_token(&diff.language),
                unified: render_unified(&diff, false),
                left: render_side(&diff, true, false),
                right: render_side(&diff, false, false),
            })
        }
        "stage_file" => {
            let ask: FileAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::stage_file(&path, &ask.file).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "unstage_file" => {
            let ask: FileAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::unstage_file(&path, &ask.file).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "stage_all" => {
            let ask: LoadAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::stage_all(&path).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, ask.whitespace, "")
        }
        "unstage_all" => {
            let ask: LoadAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::unstage_all(&path).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, ask.whitespace, "")
        }
        "apply_hunk" => {
            let ask: HunkAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::apply_hunk(&path, &ask.file, ask.hunk, ask.unstage)
                .map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "discard_file" => {
            let ask: FileAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::discard_file(&path, &ask.file).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "discard_hunk" => {
            let ask: HunkAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::discard_hunk(&path, &ask.file, ask.hunk).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "commit" => {
            let ask: CommitAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            let id = repo::commit(&path, &ask.message).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, &id)
        }
        "create_branch" => {
            let ask: BranchAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::create_branch(&path, &ask.name, None, ask.checkout)
                .map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "delete_branch" => {
            let ask: BranchAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::delete_branch(&path, &ask.name).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "checkout_branch" => {
            let ask: CheckoutAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            let plan = if ask.stash {
                repo::CheckoutPlan::Stash
            } else {
                repo::CheckoutPlan::Carry
            };
            let stash = repo::checkout_branch_planned(&path, &ask.name, plan)
                .map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, stash.as_deref().unwrap_or(""))
        }
        "create_worktree" => {
            let ask: WorktreeAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            let destination = absolute(&ask.destination)?;
            repo::create_worktree(&path, &destination, &ask.branch, &ask.start)
                .map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "stash_save" => {
            let ask: StashSaveAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            let id = repo::stash_save(&path, &ask.message).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, &id)
        }
        "stash_apply" => {
            let ask: StashApplyAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            repo::stash_apply(&path, ask.index).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "conflict_cost" => {
            let ask: ConflictAsk = ask(input)?;
            let cost = repo::conflict_cost(&absolute(&ask.path)?, &ask.file)
                .map_err(|e| format!("{e:#}"))?;
            answer(CostAnswer {
                ours: cost.ours,
                theirs: cost.theirs,
            })
        }
        "resolve_conflict" => {
            let ask: ResolveAsk = ask(input)?;
            let path = absolute(&ask.path)?;
            let side = if ask.ours {
                repo::ConflictSide::Ours
            } else {
                repo::ConflictSide::Theirs
            };
            repo::resolve_conflict(&path, &ask.file, side).map_err(|e| format!("{e:#}"))?;
            loaded_json(&path, false, "")
        }
        "workdir" => {
            let ask: LoadAsk = ask(input)?;
            answer(
                repo::workdir(&absolute(&ask.path)?)
                    .map(|path| path.to_string_lossy().into_owned()),
            )
        }
        other => Err(format!("`{other}` is no op this engine answers")),
    }
}

fn remote_name(remote: &str) -> Option<&str> {
    (!remote.is_empty()).then_some(remote)
}

/// Fetch from `remote` (the default remote when empty).
///
/// # Errors
///
/// The repository cannot be opened, or the transfer fails.
pub fn fetch(path: &Path, remote: &str) -> Result<String, String> {
    repo::fetch(path, remote_name(remote)).map_err(|error| format!("{error:#}"))
}

/// Fetch, then fast-forward the current branch to its upstream.
///
/// # Errors
///
/// As [`fetch`], or the branch diverged.
pub fn pull(path: &Path, remote: &str) -> Result<(), String> {
    repo::pull(path, remote_name(remote)).map_err(|error| format!("{error:#}"))
}

/// Push the current branch to a like-named branch on `remote`.
///
/// # Errors
///
/// As [`fetch`], or the remote refuses it.
pub fn push(path: &Path, remote: &str) -> Result<(), String> {
    repo::push(path, remote_name(remote)).map_err(|error| format!("{error:#}"))
}

/// Whether a repository is at or above `path`.
#[must_use]
pub fn is_repo(path: &Path) -> bool {
    repo::is_repo(path)
}
