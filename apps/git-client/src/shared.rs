//! What collaborators see of the repository: its branches, history,
//! remotes, tags, stashes and identity, read from the repository on disk
//! and stored whole in the record (see [`crate::resource::GitClient`]).
//! Working-tree status is never part of it; it stays a live row of the
//! window. The diff preferences are the person's (see [`crate::settings`]).

use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, JsonSchema};

use crate::model::{Branch, Commit, Remote, RepoModel, Stash, Tag};

/// Branches, history, remotes and identity collaborators share. No staged /
/// unstaged / untracked lists — those are live working-tree status.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SharedRepo {
    pub name: String,
    pub head: String,
    pub workdir: String,
    pub commits: Vec<Commit>,
    pub branches: Vec<Branch>,
    pub tags: Vec<Tag>,
    pub stashes: Vec<Stash>,
    pub remotes: Vec<Remote>,
    pub graph_width: usize,
    pub author_name: String,
    pub author_email: String,
}

impl SharedRepo {
    /// Drop working-tree lists from a live model for the collaborative projection.
    pub fn from_model(model: &RepoModel) -> Self {
        Self {
            name: model.name.clone(),
            head: model.head.clone(),
            workdir: model.workdir.clone(),
            commits: model.commits.clone(),
            branches: model.branches.clone(),
            tags: model.tags.clone(),
            stashes: model.stashes.clone(),
            remotes: model.remotes.clone(),
            graph_width: model.graph_width,
            author_name: model.author_name.clone(),
            author_email: model.author_email.clone(),
        }
    }

    /// Materialise a [`RepoModel`] with empty working-tree lists.
    pub fn into_model(self) -> RepoModel {
        RepoModel {
            name: self.name,
            head: self.head,
            workdir: self.workdir,
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
            commits: self.commits,
            branches: self.branches,
            tags: self.tags,
            stashes: self.stashes,
            remotes: self.remotes,
            worktrees: Vec::new(),
            worktree_error: None,
            graph_width: self.graph_width,
            author_name: self.author_name,
            author_email: self.author_email,
        }
    }
}

tilo_resource::json_model!(SharedRepo);

/// Stable fingerprint so we rewrite the projection only when shared state moves.
pub fn shared_fingerprint(model: &RepoModel) -> String {
    let shared = SharedRepo::from_model(model);
    serde_json::to_string(&shared).unwrap_or_default()
}
