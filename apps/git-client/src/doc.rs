//! Automerge document layer for the git client.
//!
//! Durable view preferences live under `root.settings` (see [`crate::settings`]).
//! The shared repository projection — branches, commits, remotes, tags, stashes,
//! identity — is a JSON scalar at `root.shared_repo`, the same shape Disks uses
//! for topology: machine-derived, replaced wholesale, never character-edited.
//! Working-tree status is intentionally omitted from the projection; it stays
//! a live widget extra when hosted.

use serde::{Deserialize, Serialize};
use ui::automerge::transaction::Transactable;
use ui::automerge::{AutoCommit, ROOT};
use ui::doc::read_str;

use crate::model::{Branch, Commit, Remote, RepoModel, Stash, Tag};
use crate::settings::Settings;

/// Document keys beside [`crate::settings::keys`].
pub mod keys {
    pub const SHARED_REPO: &str = "shared_repo";
    /// The path of the repository the record opens.
    pub const REPOSITORY: &str = "repository";
}

/// Framework resource state: preferences, the repository the record opens
/// and the shared repository projection.
#[derive(Clone, Debug, Default)]
pub struct GitClientState {
    pub settings: Settings,
    /// The path of the repository the record opens; `None` until one was
    /// opened, when the client looks around its working directory.
    pub repository: Option<String>,
    pub repo: RepoModel,
}

impl GitClientState {
    pub fn from_doc(doc: &AutoCommit) -> Self {
        Self {
            settings: Settings::read(doc),
            repository: read_str(doc, &ROOT, keys::REPOSITORY).filter(|path| !path.is_empty()),
            repo: SharedRepo::read(doc)
                .map(SharedRepo::into_model)
                .unwrap_or_default(),
        }
    }
}

/// Branches, history, remotes and identity collaborators share. No staged /
/// unstaged / untracked lists — those are live working-tree status.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
            graph_width: self.graph_width,
            author_name: self.author_name,
            author_email: self.author_email,
        }
    }

    #[cfg(test)]
    pub fn write(doc: &mut AutoCommit, shared: &Self) {
        let json = serde_json::to_string(shared).unwrap_or_else(|_| "{}".into());
        Self::write_json(doc, &json);
    }

    pub fn write_json(doc: &mut AutoCommit, json: &str) {
        let _ = doc.put(&ROOT, keys::SHARED_REPO, json);
    }

    pub fn read(doc: &AutoCommit) -> Option<Self> {
        let json = read_str(doc, &ROOT, keys::SHARED_REPO)?;
        serde_json::from_str(&json).ok()
    }
}

/// Stable fingerprint so we rewrite the projection only when shared state moves.
pub fn shared_fingerprint(model: &RepoModel) -> String {
    let shared = SharedRepo::from_model(model);
    serde_json::to_string(&shared).unwrap_or_default()
}
