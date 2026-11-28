//! The names a workspace and its people go by.
//!
//! A workspace's own name is its governance's, which its owners change ([`Workspaces::rename`]).
//!
//! A person's is theirs, not one workspace's. They give it once, from any device of theirs (the
//! `tilo` command, a page, their desktop), and that device names them by it in every workspace
//! kept here that it is a member of, each a change it signs; this server keeps the name with
//! their profile ([`PersonNames`]). What a workspace's governance keeps of the name, which its
//! lists, its invitations and its history read, is what their device signed there.

use std::sync::Arc;

use tilo_lock::RwLockRecover as _;

use tilo_peer_state::workspace_membership::valid_workspace_name;

use super::{CollaborationError, HostedWorkspace, WorkspaceSummary, Workspaces};

/// Where this server keeps the name each person gave themselves: their profile, which every device
/// of theirs reads it from.
pub trait PersonNames: Send + Sync {
    /// The name `person` (their root) gave themselves, `None` before they gave one.
    fn name_of(&self, person: &[u8; 32]) -> Option<String>;

    /// Keep `name` as the one `person` goes by from now on.
    ///
    /// # Errors
    ///
    /// It could not be kept, named with why.
    fn keep_name(&self, person: &[u8; 32], name: &str) -> Result<(), String>;

    /// The picture `person` set in their profile, as the image's own bytes: what every surface
    /// that shows them as an author draws. `None` before they set one, or where this host keeps
    /// their profile without its picture.
    fn photo_of(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
        let _ = person;
        None
    }
}

impl Workspaces {
    /// Keep the name each person gives themselves in `names`.
    pub fn person_names(&self, names: Arc<dyn PersonNames>) {
        *self.inner.person_names.write_recover() = Some(names);
    }

    /// The picture `person` set in their profile, as its image bytes: what a surface showing
    /// them as an author draws. `None` before they set one.
    #[must_use]
    pub fn person_profile_photo(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
        self.inner
            .person_names
            .read_recover()
            .as_ref()?
            .photo_of(person)
            .filter(|photo| !photo.is_empty())
    }

    /// The name `person` gave themselves, as their profile keeps it here.
    pub(super) fn kept_person_name(&self, person: [u8; 32]) -> Option<String> {
        let names = self.inner.person_names.read_recover().clone();
        names?.name_of(&person)
    }

    /// Keep `name` as `person`'s, with their profile.
    ///
    /// # Errors
    ///
    /// The name could not be kept.
    pub(super) fn keep_person_name(
        &self,
        person: [u8; 32],
        name: &str,
    ) -> Result<(), CollaborationError> {
        let names = self.inner.person_names.read_recover().clone();
        names.map_or(Ok(()), |names| {
            names
                .keep_name(&person, name)
                .map_err(CollaborationError::new)
        })
    }

    /// The workspaces this server's person administers.
    pub(super) async fn administered_ids(&self) -> Vec<String> {
        let held: Vec<_> = self
            .inner
            .workspaces
            .lock()
            .await
            .iter()
            .map(|(id, workspace)| (id.clone(), workspace.clone()))
            .collect();
        let mut hosted = Vec::with_capacity(held.len());
        for (id, workspace) in held {
            if self.administers(&*workspace.lock().await) {
                hosted.push(id);
            }
        }
        hosted
    }

    /// Name `workspace` `name`, in its governance (what members show) and in
    /// what this server keeps of it. Naming it what it is called changes
    /// nothing.
    ///
    /// # Errors
    ///
    /// The workspace is not hosted here, the name is not 1–80 characters, or
    /// the change could not be committed.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from the rename until it is settled"
    )]
    pub async fn rename(&self, workspace: &str, name: &str) -> Result<WorkspaceSummary, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut hosted = state.lock().await;
        let summary = self.rename_hosted(&mut hosted, name)?;
        self.settle(&mut hosted).await?;
        Ok(summary)
    }

    /// [`Self::rename`] of `hosted`, which the caller keeps, without
    /// settling it: the entry answers with what the rename made.
    ///
    /// # Errors
    ///
    /// The name is not 1–80 characters, or the change could not be committed.
    pub(super) fn rename_hosted(
        &self,
        hosted: &mut HostedWorkspace,
        name: &str,
    ) -> Result<WorkspaceSummary, String> {
        let name = valid_workspace_name(name)?;
        if hosted.peer().governance().workspace_name().as_deref() == Some(name) {
            return Ok(self.people(hosted));
        }
        let now = tilo_clock::wall();
        let key = &self.inner.host.key;
        let mut candidate = hosted.peer().clone();
        candidate
            .governance_mut()
            .author_workspace_name(name, key, now)?;
        let collaboration = hosted.collaboration().clone();
        hosted.keep(candidate, key, now, |_, _| Ok(collaboration))?;
        Ok(self.people(hosted))
    }
}
