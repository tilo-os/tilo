//! What a repository is, read out of it: its working tree's changes and their diffs, the history
//! with its graph columns, and the branches, tags, stashes and remotes. The app reads these as
//! JSON and keeps them in its own records; nothing here performs IO.

use serde::{Deserialize, Serialize};

/// Where a change stands: in the next commit, still in the working tree, or not tracked yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeArea {
    Staged,
    Unstaged,
    Untracked,
}

/// What became of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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

/// A node in the commit graph: `column` is the column its dot sits in, and `parents` name the
/// commits its edges connect.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub summary: String,
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    /// When it was made, in seconds since the epoch.
    pub time: i64,
    /// How long ago that was, in seconds, as the view says it.
    pub age: i64,
    /// When it was made, as a machine reads it in UTC.
    pub date: String,
    pub parents: Vec<String>,
    pub refs: Vec<RefLabel>,
    pub column: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub tip: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub target: String,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stash {
    pub index: usize,
    pub message: String,
    pub branch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    /// The history's graph edges, read from its columns.
    #[serde(skip)]
    pub graph_edges: Vec<crate::graph::GraphEdge>,

    pub author_name: String,
    pub author_email: String,
}

impl RepoModel {
    pub fn change_count(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len()
    }

    pub fn all_changes(&self) -> Vec<&FileChange> {
        self.staged
            .iter()
            .chain(self.unstaged.iter())
            .chain(self.untracked.iter())
            .collect()
    }
}
