//! Render-friendly data model for the git client, the single source the UI draws
//! from: a flat, cloneable description of the working state, a selected file's
//! diff, the history with its lanes, and the side panels. It is decoupled from
//! `git2` — [`crate::repo`] fills it from a repository and the sample fixture fills
//! it for screenshots — and nothing here performs IO.

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChangeArea {
    Staged,
    Unstaged,
    Untracked,
}

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub area: ChangeArea,
    pub kind: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
}

impl FileChange {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    pub fn dir(&self) -> &str {
        match self.path.rfind('/') {
            // `rfind` on a `char` always lands on a byte boundary.
            Some(i) => self.path.get(..i).unwrap_or(""),
            None => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Addition,
    Deletion,
    Hunk,
    Meta,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_lineno: Option<u32>,
    pub new_lineno: Option<u32>,
    pub content: String,
    pub hunk: usize,
}

#[derive(Clone, Debug)]
pub struct DiffHunk {
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub line_range: (usize, usize),
}

#[derive(Clone, Debug, Default)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub language: String,
    pub lines: Vec<DiffLine>,
    pub hunks: Vec<DiffHunk>,
    pub is_new: bool,
    pub is_deleted: bool,
    pub binary: bool,
}

/// Key into the per-file diff map. A file can carry two distinct diffs at once, staged (HEAD →
/// index) and worktree (index → working tree); keying by path alone would show a partially staged
/// file's worktree diff under its staged entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiffKey {
    pub staged: bool,
    pub path: String,
}

impl DiffKey {
    pub fn staged(path: impl Into<String>) -> Self {
        Self {
            staged: true,
            path: path.into(),
        }
    }
    pub fn worktree(path: impl Into<String>) -> Self {
        Self {
            staged: false,
            path: path.into(),
        }
    }
    pub fn for_area(area: ChangeArea, path: impl Into<String>) -> Self {
        Self {
            staged: area == ChangeArea::Staged,
            path: path.into(),
        }
    }
}

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

/// A node in the commit graph: `lane` is the column its dot sits in, and `parents` name the commits
/// its edges connect.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Commit {
    pub id: String,
    pub summary: String,
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    pub when: String,
    pub time: i64,
    pub parents: Vec<String>,
    pub refs: Vec<RefLabel>,
    pub lane: usize,
    pub files: Vec<FileChange>,
}

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, JsonSchema)]
pub struct Branch {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub name: String,
    pub path: String,
    pub head: String,
    pub current: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RepoModel {
    pub name: String,
    pub head: String,
    pub workdir: String,

    pub staged: Vec<FileChange>,
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<FileChange>,

    pub commits: Vec<Commit>,
    pub branches: Vec<Branch>,
    pub tags: Vec<Tag>,
    pub stashes: Vec<Stash>,
    pub remotes: Vec<Remote>,
    #[serde(skip)]
    pub worktrees: Vec<Worktree>,
    #[serde(skip)]
    pub worktree_error: Option<String>,

    pub graph_width: usize,

    pub author_name: String,
    pub author_email: String,
}

impl RepoModel {
    pub fn change_count(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len()
    }

    pub fn commit(&self, id: &str) -> Option<&Commit> {
        self.commits.iter().find(|c| c.id == id)
    }

    /// Find the changed file across all areas at a given absolute index, where the order is staged,
    /// then unstaged, then untracked.
    pub fn all_changes(&self) -> Vec<&FileChange> {
        self.staged
            .iter()
            .chain(self.unstaged.iter())
            .chain(self.untracked.iter())
            .collect()
    }
}
