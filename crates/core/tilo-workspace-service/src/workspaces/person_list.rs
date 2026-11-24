//! The person's list of their workspaces, written by the device that holds their own workspace.
//!
//! A workspace is listed when the person's own daemon makes one for them, redeems the invitation
//! into one, or lets one go: the daemon holds the person's own workspace, so its write is decided
//! there like any other record's. A host that merely admits someone holds no copy of their own
//! workspace and writes nothing; a server that makes a workspace for them holds no copy of it
//! either, because founding it is their own device's, so nothing of theirs is written on the
//! server.
//!
//! One writer runs wherever the person's own workspace is held: the daemon calls these changes as
//! part of the action that made the workspace or admitted the person, so the list is complete
//! without a desktop session, and a workspace removed is remembered as gone so a device that was
//! away lets it go too.

use tilo_peer_state::join::TransportHint;
use tilo_resource::Invoke;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::IDENTITY_LIST;
use tilo_user_identity::WorkspaceList;

use super::{Workspaces, person_workspace};

/// Where a workspace is kept: the device keeping it, and the address it signed for it when one of
/// the person's devices knows it.
#[derive(Clone, Debug)]
pub struct KeptAt {
    /// The device that keeps the workspace, by its key.
    pub device: [u8; 32],
    /// Its signed address for the workspace, where it offered one.
    pub hint: Option<TransportHint>,
}

/// The changes that list `workspace` as `name` in the person's list, saying where it is kept when
/// `kept` names it.
#[must_use]
pub fn listing(workspace: &str, name: &str, kept: Option<&KeptAt>) -> Vec<Invoke<WorkspaceList>> {
    let mut changes =
        vec![WorkspaceList::LIST_WORKSPACE.with((workspace.to_owned(), name.to_owned()))];
    if let Some(kept) = kept {
        let device = hex::encode(kept.device);
        changes.push(WorkspaceList::HOST_WORKSPACE.with((workspace.to_owned(), device.clone())));
        if let Some(hint) = &kept.hint
            && let Ok(bytes) = postcard::to_allocvec(hint)
        {
            changes.push(WorkspaceList::REACH_WORKSPACE.with((
                workspace.to_owned(),
                device,
                hex::encode(bytes),
            )));
        }
    }
    changes
}

/// The change that says `device`, one of the person's, keeps `workspace`.
#[must_use]
pub fn hosting(workspace: &str, device: [u8; 32]) -> Invoke<WorkspaceList> {
    WorkspaceList::HOST_WORKSPACE.with((workspace.to_owned(), hex::encode(device)))
}

/// The change that takes `workspace` off the person's list, remembered as gone.
#[must_use]
pub fn unlisting(workspace: &str) -> Invoke<WorkspaceList> {
    WorkspaceList::UNLIST_WORKSPACE.with(workspace.to_owned())
}

/// The change that says `device`, one of the person's that holds `workspace`, reaches its host at
/// `hint`.
#[must_use]
pub fn reaching(
    workspace: &str,
    device: [u8; 32],
    hint: &TransportHint,
) -> Option<Invoke<WorkspaceList>> {
    let bytes = postcard::to_allocvec(hint).ok()?;
    Some(WorkspaceList::REACH_WORKSPACE.with((
        workspace.to_owned(),
        hex::encode(device),
        hex::encode(bytes),
    )))
}

impl Workspaces {
    /// Where `workspace` is kept, as this device reads its governance: the device keeping it, and
    /// the address it signed.
    pub async fn kept_at(&self, workspace: &str) -> Option<KeptAt> {
        let governed = self.workspace(workspace).await.ok()?;
        let governed = governed.lock().await;
        let own = self.serving_certificate().device_public_key;
        let hosts = governed.peer().governance().keeping_devices();
        drop(governed);
        // The device keeping it: this one when it does, else the host of this member copy.
        let hint = hosts
            .iter()
            .find(|hint| hint.host.device_public_key == own)
            .cloned()
            .or_else(|| hosts.into_iter().next())?;
        let device = hint.host.device_public_key;
        Some(KeptAt {
            device,
            hint: Some(hint),
        })
    }

    /// List `workspace` as `name` in `person`'s own list, where this device holds their own
    /// workspace: whether it wrote. A host that keeps no copy of it writes nothing.
    pub async fn list_for(&self, person: &[u8; 32], workspace: &str, name: &str) -> bool {
        // Their own workspace holds the list; the list never names it.
        if workspace == person_workspace_id(person) {
            return false;
        }
        let kept = self.kept_at(workspace).await;
        let changes = listing(workspace, name, kept.as_ref());
        self.write_person_list(person, changes).await
    }

    /// Take `workspace` off `person`'s own list, remembered as gone, where this device holds it:
    /// whether it wrote.
    pub async fn unlist_for(&self, person: &[u8; 32], workspace: &str) -> bool {
        self.write_person_list(person, vec![unlisting(workspace)])
            .await
    }

    /// List every workspace this host records `person` in, one they made and one they were invited
    /// into, in their own list: what a device that starts holding their own workspace writes, so
    /// the list is as complete as this host knows it.
    pub async fn list_known_for(&self, person: &[u8; 32]) -> bool {
        let their_own = person_workspace_id(person);
        let mut known: Vec<(String, String)> = Vec::new();
        if *person == self.serving_certificate().user_public_key {
            // What this host governs is all theirs.
            known.extend(
                self.list()
                    .await
                    .into_iter()
                    .map(|summary| (summary.workspace, summary.name)),
            );
        }
        let own = self.own().map(|own| own.of(person)).unwrap_or_default();
        let mut invited = self.inner.memberships.of(person);
        invited.retain(|workspace| !own.contains(workspace));
        for workspace in own.into_iter().chain(invited) {
            if known.iter().any(|(held, _)| *held == workspace) {
                continue;
            }
            if let Ok(summary) = self.summary(&workspace).await {
                known.push((summary.workspace, summary.name));
            }
        }
        for (workspace, name) in known {
            if workspace == their_own {
                continue;
            }
            self.list_for(person, &workspace, &name).await;
        }
        true
    }

    /// Apply `changes` to `person`'s own workspace where this device holds it: whether every one
    /// landed.
    async fn write_person_list(
        &self,
        person: &[u8; 32],
        changes: Vec<Invoke<WorkspaceList>>,
    ) -> bool {
        let own = person_workspace_id(person);
        if self.state(&own).await.is_none() {
            return false;
        }
        for change in changes {
            if let Err(error) = person_workspace::apply_on(self, &own, IDENTITY_LIST, change).await
            {
                tracing::warn!(%error, %own, "the person's list of workspaces was not written");
                return false;
            }
        }
        true
    }
}
