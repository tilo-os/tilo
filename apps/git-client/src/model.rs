//! Pure, render-friendly data model for the git client.
//!
//! This is the single source of truth the UI draws from. It is a flat,
//! cloneable description of a repository's working state — staged /
//! unstaged / untracked changes, a selected file's diff, the commit
//! history with branch/merge lanes, and the side panels (branches,
//! tags, stashes, remotes).
//!
//! The model is deliberately decoupled from `git2`: the [`crate::repo`]
//! backend fills it from an on-disk repository, and
//! `RepoModel::sample` fills it from a bundled fixture so the headless
//! screenshot renders a populated state with no repository
//! present. Nothing in this file performs IO.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};

/// Where a changed file sits relative to the index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChangeArea {
    /// Difference between HEAD and the index — `git add`ed, ready to commit.
    Staged,
    /// Difference between the index and the working tree — edited but not added.
    Unstaged,
    /// Present in the working tree, unknown to git.
    Untracked,
}

/// The kind of change a file underwent, used for the leading status glyph
/// and its colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    /// Single-letter status badge (`M`, `A`, `D`, …) as shown in the list.
    pub fn badge(self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Untracked => "U",
            ChangeKind::Conflicted => "C",
        }
    }

    /// What the badge stands for, in words (for assistive technology).
    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::Added => "added",
            ChangeKind::Modified => "modified",
            ChangeKind::Deleted => "deleted",
            ChangeKind::Renamed => "renamed",
            ChangeKind::Untracked => "untracked",
            ChangeKind::Conflicted => "conflicted",
        }
    }
}

/// One entry in the changes lists.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct FileChange {
    pub path: String,
    /// For renames: the previous path. `None` otherwise.
    pub old_path: Option<String>,
    pub area: ChangeArea,
    pub kind: ChangeKind,
    /// Lines added / removed across all hunks (for the per-row +/- counts).
    pub additions: u32,
    pub deletions: u32,
}

impl FileChange {
    /// The base name shown in bold in the list (the trailing path segment).
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// The directory portion shown dimmed after the name.
    pub fn dir(&self) -> &str {
        match self.path.rfind('/') {
            // `rfind` on a `char` always lands on a byte boundary.
            Some(i) => self.path.get(..i).unwrap_or(""),
            None => "",
        }
    }
}

/// The role a diff line plays, driving its gutter glyph and row tint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    /// Unchanged context line, present on both sides.
    Context,
    /// Line added (`+`), only on the new side.
    Addition,
    /// Line removed (`-`), only on the old side.
    Deletion,
    /// `@@ … @@` hunk header.
    Hunk,
    /// `diff --git`, `index`, `--- a/…`, `+++ b/…` metadata line.
    Meta,
}

/// One line of a unified diff, already split into old/new line numbers so
/// both the unified and side-by-side renderers can address it cheaply.
#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// 1-based line number on the old (pre-image) side, if applicable.
    pub old_lineno: Option<u32>,
    /// 1-based line number on the new (post-image) side, if applicable.
    pub new_lineno: Option<u32>,
    /// The line text WITHOUT the leading +/-/space marker.
    pub content: String,
    /// Index of the enclosing hunk (for hunk-level staging).
    pub hunk: usize,
}

/// A contiguous block of changes within a file diff.
#[derive(Clone, Debug)]
pub struct DiffHunk {
    /// `@@ -old,+new @@ section` header text, sans the leading `@@`.
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    /// Inclusive range of [`DiffLine`] indices belonging to this hunk.
    pub line_range: (usize, usize),
}

/// A full file diff: the lines plus the hunk boundaries.
#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    /// Grammar token the diff pane highlights with (e.g. "rs", "toml", "md").
    pub language: String,
    pub lines: Vec<DiffLine>,
    pub hunks: Vec<DiffHunk>,
    /// True when this is a brand-new file (no old side to show).
    pub is_new: bool,
    /// True when the file is being deleted.
    pub is_deleted: bool,
    /// True when git detected the content as binary (no textual diff).
    pub binary: bool,
}

/// Key into the per-file diff map. A file can carry two *distinct* diffs at
/// once: the staged side (HEAD → index) and the worktree side (index →
/// working tree). Keying only by path collapses them, so selecting the
/// staged entry of a partially-staged file would show its worktree diff.
/// The `staged` flag keeps the two sides separate.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub staged: bool,
    pub path: String,
}

impl DiffKey {
    /// The staged (HEAD → index) diff for `path`.
    pub fn staged(path: impl Into<String>) -> Self {
        Self {
            staged: true,
            path: path.into(),
        }
    }
    /// The worktree (index → working tree) diff for `path`; also used for
    /// untracked files (which have no index side).
    pub fn worktree(path: impl Into<String>) -> Self {
        Self {
            staged: false,
            path: path.into(),
        }
    }
    /// The key for a file selected in `area`.
    pub fn for_area(area: ChangeArea, path: impl Into<String>) -> Self {
        Self {
            staged: area == ChangeArea::Staged,
            path: path.into(),
        }
    }
}

/// The per-file diff map: distinct staged / worktree diffs keyed by
/// [`DiffKey`].
pub type DiffMap = std::collections::HashMap<DiffKey, FileDiff>;

impl FileDiff {
    pub fn additions(&self) -> u32 {
        self.lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Addition)
            .count() as u32
    }
    pub fn deletions(&self) -> u32 {
        self.lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Deletion)
            .count() as u32
    }
}

/// A node in the commit graph. `lane` is the horizontal column the node's
/// dot sits in; `parents` index into the commit list (by id) for drawing
/// the connecting edges.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Commit {
    pub id: String,
    pub summary: String,
    /// Full multi-paragraph message body (everything after the summary line).
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    /// Pretty, already-formatted relative or absolute timestamp.
    pub when: String,
    /// Seconds since the epoch — for sorting and absolute-date tooltips.
    pub time: i64,
    /// Parent commit ids. Two or more parents => a merge commit.
    pub parents: Vec<String>,
    /// Refs that point directly at this commit (local branches, tags,
    /// remote-tracking branches, HEAD).
    pub refs: Vec<RefLabel>,
    /// Graph lane (column) assigned by the layout pass.
    pub lane: usize,
    /// The per-file diff summary for this commit (populated lazily / for
    /// the sample). Empty when not yet loaded.
    pub files: Vec<FileChange>,
}

/// A decorating ref drawn as a coloured pill next to a commit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
}

/// One local or remote branch in the branches panel.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Branch {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    /// Upstream tracking branch, if any (e.g. "origin/main").
    pub upstream: Option<String>,
    /// Commits ahead / behind the upstream.
    pub ahead: u32,
    pub behind: u32,
    /// Short id of the commit the branch points at.
    pub tip: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Tag {
    pub name: String,
    pub target: String,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Stash {
    pub index: usize,
    pub message: String,
    pub branch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

/// One working directory belonging to this repository, read from disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub name: String,
    pub path: String,
    pub head: String,
    pub current: bool,
}

/// The complete render state. Cloneable and self-contained.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RepoModel {
    /// Friendly repository name (last path segment of the workdir).
    pub name: String,
    /// Currently checked-out branch (or detached-HEAD description).
    pub head: String,
    /// Absolute path of the repository working directory.
    pub workdir: String,

    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<FileChange>,

    pub commits: Vec<Commit>,
    pub branches: Vec<Branch>,
    pub tags: Vec<Tag>,
    pub stashes: Vec<Stash>,
    pub remotes: Vec<Remote>,
    /// Device-local linked working directories, never part of the CRDT projection.
    #[serde(skip)]
    pub worktrees: Vec<Worktree>,
    /// Failure to observe registered working directories, shown rather than treated as empty.
    #[serde(skip)]
    pub worktree_error: Option<String>,

    /// Number of graph lanes in use (for sizing the graph gutter).
    pub graph_width: usize,

    /// Default commit author identity (from git config).
    pub author_name: String,
    pub author_email: String,
}

impl RepoModel {
    /// Total pending changes across all three areas — drives the badge in
    /// the changes-view header.
    pub fn change_count(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len()
    }

    /// Look up a commit by id.
    pub fn commit(&self, id: &str) -> Option<&Commit> {
        self.commits.iter().find(|c| c.id == id)
    }

    /// Find the changed file (across all areas) at a given absolute index,
    /// where the order is staged, then unstaged, then untracked.
    pub fn all_changes(&self) -> Vec<&FileChange> {
        self.staged
            .iter()
            .chain(self.unstaged.iter())
            .chain(self.untracked.iter())
            .collect()
    }
}
