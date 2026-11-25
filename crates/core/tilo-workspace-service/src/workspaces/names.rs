//! The names a workspace and its people go by.
//!
//! A workspace's own name is its governance's, which its owners change ([`Workspaces::rename`]).
//!
//! A person's is theirs, not one workspace's. They give it once, from any device of theirs (the
//! `tilo` command, a page, their desktop); this server keeps it with their profile
//! ([`PersonNames`]) and names them by it in every workspace it hosts, the ones they join later
//! included. What a workspace's governance keeps of the name, which its lists, its invitations
//! and its history read, follows the person's.

use std::sync::Arc;

use tilo_lock::RwLockRecover as _;

use tilo_peer_state::workspace_membership::valid_workspace_name;

use super::{CollaborationError, HostedWorkspace, WorkspaceSummary, Workspaces, system_time};

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
    /// Keep the name each person gives themselves in `names`, and name every person in each
    /// workspace hosted here by the one `names` keeps for them, as soon as its governance moves
    /// (someone joins, someone renames themselves on another device).
    pub fn person_names(&self, names: Arc<dyn PersonNames>) {
        *self.inner.person_names.write_recover() = Some(names);
        let this = self.clone();
        let mut moved = self.governance_changes();
        tokio::spawn(async move {
            loop {
                match moved.recv().await {
                    Ok(workspace) => this.names_follow_people(&workspace).await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Moves were missed: every workspace is read again.
                        for workspace in this.administered_ids().await {
                            this.names_follow_people(&workspace).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// The name `person` (their root) gave themselves, as this host keeps it in their profile:
    /// the one every workspace names them by, and what someone outside a member list they hold a
    /// row in is still called. `None` before they gave one.
    #[must_use]
    pub fn person_profile_name(&self, person: &[u8; 32]) -> Option<String> {
        let names = self.inner.person_names.read_recover();
        names
            .as_ref()?
            .name_of(person)
            .filter(|name| !name.trim().is_empty())
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

    /// Keep `name` as `person`'s, then name them by it in every workspace hosted here that they
    /// hold a part in. Whether any workspace changed.
    ///
    /// # Errors
    ///
    /// The name could not be kept.
    pub(crate) async fn name_person(
        &self,
        person: [u8; 32],
        name: &str,
        now: u64,
    ) -> Result<bool, CollaborationError> {
        let names = self.inner.person_names.read_recover().clone();
        if let Some(names) = names {
            names
                .keep_name(&person, name)
                .map_err(CollaborationError::new)?;
        }
        Ok(self.name_person_everywhere(person, name, now).await)
    }

    /// Name `person` `name` in every workspace hosted here that they hold a part in, as the
    /// profile their root signed says: whether any workspace changed. A workspace whose
    /// governance could not be written says so in the log and keeps the name it had; the others
    /// take it all the same.
    pub async fn name_person_everywhere(&self, person: [u8; 32], name: &str, now: u64) -> bool {
        let mut changed = false;
        for workspace in self.administered_ids().await {
            match self.name_in(&workspace, person, name, now).await {
                Ok(named) => changed |= named,
                Err(error) => {
                    tracing::warn!(%error, workspace, "a person was not named by their own name");
                }
            }
        }
        changed
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

    /// Name each person in `workspace` by the name they gave themselves, where it calls them
    /// something else.
    async fn names_follow_people(&self, workspace: &str) {
        let Some(names) = self.inner.person_names.read_recover().clone() else {
            return;
        };
        let Ok(state) = self.workspace(workspace).await else {
            return;
        };
        let now = tilo_clock::unix_now_ns();
        let read = {
            let hosted = state.lock().await;
            if !self.administers(&hosted) {
                return;
            }
            hosted.peer().governance().member_list(system_time(now))
        };
        let members = match read {
            Ok(list) => list.members,
            Err(error) => {
                tracing::warn!(%error, workspace, "the workspace's people were not read");
                return;
            }
        };
        for member in members.iter().filter(|member| !member.removed) {
            let Some(person) = member.root() else {
                continue;
            };
            let Some(name) = names.name_of(&person) else {
                continue;
            };
            if member.name == name {
                continue;
            }
            if let Err(error) = self.name_in(workspace, person, &name, now).await {
                tracing::warn!(%error, workspace, "a person was not named by their own name");
            }
        }
    }

    /// Name `person` `name` in `workspace`, where they hold a part in it: whether it changed.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked from the naming until it is settled"
    )]
    async fn name_in(
        &self,
        workspace: &str,
        person: [u8; 32],
        name: &str,
        now: u64,
    ) -> Result<bool, CollaborationError> {
        let state = self.workspace(workspace).await?;
        let mut hosted = state.lock().await;
        let at = system_time(now);
        let holds_a_part = hosted
            .peer()
            .governance()
            .member_list(at)
            .map_err(CollaborationError::new)?
            .members
            .iter()
            .any(|member| member.root() == Some(person) && !member.removed);
        if !holds_a_part {
            return Ok(false);
        }
        let key = self.inner.host.key.clone();
        let mut candidate = hosted.peer().clone();
        if !candidate
            .governance_mut()
            .name_person(person, name, &key, at)
            .map_err(CollaborationError::new)?
        {
            return Ok(false);
        }
        let log = hosted.collaboration().clone();
        hosted
            .keep(candidate, &key, at, |_, _| Ok(log))
            .map_err(CollaborationError::new)?;
        self.settle(&mut hosted).await?;
        Ok(true)
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
