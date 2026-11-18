//! The `git_client.repo` resource: the repository a client opens and what
//! members see of it, as the app that draws it and an agent that reads it both
//! see it.
//!
//! Working-tree status and an in-flight fetch or push are the app's live rows;
//! only what members share travels in the workspace.

use tilo_resource::{self, Atom, Model, resource};

use crate::shared::SharedRepo;

/// The record: the repository it opens and what members see of it.
/// Working-tree rows and in-flight fetch/push are live.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct GitClient {
    /// Repository
    ///
    /// The path of the repository the resource opens; empty until one was
    /// opened, when the client looks around its working directory.
    pub repository: Atom,
    /// Shared history
    ///
    /// Branches, history, remotes and identity as the repository on disk
    /// last held them, for members and past versions.
    #[model(readonly)]
    pub shared: SharedRepo,
}

impl GitClient {
    pub fn opened(&self) -> Option<&str> {
        Some(self.repository.0.as_str()).filter(|path| !path.is_empty())
    }
}

impl GitClient {
    /// What the resource holds, answered through [`fields`](tilo_resource::read::fields): a caller
    /// with no window open reads it as it reads any app's resource, and a named item is read from the
    /// resource's table or by the read its own type gives it.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "git_client.repo",
    read_content = GitClient::read_document
)]
impl GitClient {
    /// Keep what the repository on disk holds now
    ///
    /// Its branches, history, remotes and identity, for members and
    /// past versions. Read from the repository, nobody's edit.
    #[action(external)]
    pub fn store_repository(
        &mut self,
        #[param("The repository's branches, history, remotes and identity")] shared: SharedRepo,
    ) {
        self.shared = shared;
    }
}
