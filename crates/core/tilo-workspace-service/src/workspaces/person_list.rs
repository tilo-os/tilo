//! The person's list of their workspaces, written by a device of theirs that administers their own
//! workspace.
//!
//! A workspace is listed when the person's own daemon makes one for them, redeems the invitation
//! into one, or lets one go, and when their device hands their own workspace to a server of
//! theirs: the server answers the other workspaces it keeps for them, and the device, which holds
//! their root, lists them under its own key before the hand-over is done, so a login that reads
//! the list from that server finds their workspaces, not an empty list. A server, on Tilo OS or
//! not, never writes the list itself; a host that merely admits someone holds no copy of their own
//! workspace and writes nothing either.
//!
//! One writer runs for the person's list: the device that keeps their root, calling these changes
//! as part of the action that made the workspace, took the device into one that is theirs or
//! admitted the person, and carrying them to wherever that workspace is hosted. So the list is
//! complete without a desktop session, and a workspace removed is remembered as gone so a device
//! that was away lets it go too.

use tilo_peer_state::join::TransportHint;
use tilo_resource::Invoke;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_server_protocol::workspaces::{IDENTITY_LIST, KeptWorkspace};
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
        changes.push(hosting(workspace, kept.device));
        if let Some(hint) = &kept.hint
            && let Some(reach) = reaching(workspace, kept.device, hint)
        {
            changes.push(reach);
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
    Some(reaching_at(workspace, device, &hex::encode(bytes)))
}

/// [`reaching`] at the address `hint` already encoded: postcard, in hex, as a workspace's host
/// signed it.
#[must_use]
pub fn reaching_at(workspace: &str, device: [u8; 32], hint: &str) -> Invoke<WorkspaceList> {
    WorkspaceList::REACH_WORKSPACE.with((
        workspace.to_owned(),
        hex::encode(device),
        hint.to_owned(),
    ))
}

/// The change that shows `workspace` on `device`, one of the person's, or keeps it off there.
#[must_use]
pub fn showing(workspace: &str, device: [u8; 32], shown: bool) -> Invoke<WorkspaceList> {
    WorkspaceList::SHOW_WORKSPACE.with((workspace.to_owned(), hex::encode(device), shown))
}

/// The change that puts `workspace` right after `after` in the person's order.
#[must_use]
pub fn placing(workspace: &str, after: &str) -> Invoke<WorkspaceList> {
    WorkspaceList::PLACE_WORKSPACE.with((workspace.to_owned(), after.to_owned()))
}

impl Workspaces {
    /// Where `workspace` is kept, as this device reads its governance: the device keeping it, and
    /// the address it signed.
    pub async fn kept_at(&self, workspace: &str) -> Option<KeptAt> {
        let hosted = self.workspace(workspace).await.ok()?;
        let hosted = hosted.lock().await;
        let own = self.device_cert().device_public_key;
        let hosts = hosted.peer().governance().keeping_devices();
        drop(hosted);
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

    /// List `workspace` as `name` in `person`'s own list, where this device hosts
    /// their own workspace: whether it wrote. A device that keeps no copy of it
    /// writes nothing, and neither does one keeping a copy another device hosts.
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
    /// into, in their own list: what a device that founds their own workspace writes, so the list
    /// is as complete as this host knows it.
    pub async fn list_known_for(&self, person: &[u8; 32]) -> bool {
        for (workspace, name) in self.known_for(person).await {
            self.list_for(person, &workspace, &name).await;
        }
        true
    }

    /// The workspaces of `person`'s this server keeps, each with its signed address for it: what
    /// it answers the device of theirs that hands it their own workspace, which lists them.
    pub(super) async fn keeps_for(&self, person: &[u8; 32]) -> Vec<KeptWorkspace> {
        let own = self.device_cert().device_public_key;
        let mut kept = Vec::new();
        for (workspace, name) in self.known_for(person).await {
            let Some(KeptAt {
                device,
                hint: Some(hint),
            }) = self.kept_at(&workspace).await
            else {
                continue;
            };
            let Ok(bytes) = postcard::to_allocvec(&hint) else {
                continue;
            };
            if device == own {
                kept.push(KeptWorkspace {
                    workspace,
                    name,
                    host_hint: hex::encode(bytes),
                });
            }
        }
        kept
    }

    /// List in `person`'s own list, under this device's key, each workspace `kept` names that the
    /// server `server` says it keeps for them, as it answered the hand-over of their own workspace.
    /// Only an address `server` signed for that workspace is taken: it names no workspace kept
    /// elsewhere. Whether every one landed.
    pub async fn list_kept_by(
        &self,
        person: &[u8; 32],
        server: [u8; 32],
        kept: &[KeptWorkspace],
    ) -> bool {
        let mut written = true;
        for entry in kept {
            let Ok(hint) = super::decode_hex::<TransportHint>(&entry.host_hint) else {
                written = false;
                continue;
            };
            let signed =
                tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::from_transport_hint(
                    &hint,
                );
            let theirs = signed.is_ok_and(|signed| {
                signed.device_certificate.device_public_key == server
                    && signed.workspace_id.to_string() == entry.workspace
            });
            if !theirs || entry.workspace == person_workspace_id(person) {
                written = false;
                continue;
            }
            let at = KeptAt {
                device: server,
                hint: Some(hint),
            };
            written &= self
                .write_person_list(person, listing(&entry.workspace, &entry.name, Some(&at)))
                .await;
        }
        written
    }

    /// Every workspace this host records `person` in, but their own, by name.
    async fn known_for(&self, person: &[u8; 32]) -> Vec<(String, String)> {
        let their_own = person_workspace_id(person);
        let mut known: Vec<(String, String)> = Vec::new();
        if *person == self.device_cert().user_public_key {
            // What this host keeps that is theirs: the workspaces their member
            // list names them in. A host that runs other people's workspaces
            // beside them hosts those as well, and none of them is listed here.
            let now = tilo_clock::wall();
            for workspace in self.ids().await {
                let Ok(state) = self.workspace(&workspace.to_string()).await else {
                    continue;
                };
                let hosted = state.lock().await;
                let theirs = hosted
                    .peer()
                    .governance()
                    .member_list(now)
                    .is_ok_and(|list| {
                        list.members
                            .iter()
                            .any(|member| member.root() == Some(*person) && !member.removed)
                    });
                let listed = (hosted.workspace().to_owned(), hosted.name());
                drop(hosted);
                if theirs {
                    known.push(listed);
                }
            }
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
        known.retain(|(workspace, _)| *workspace != their_own);
        known
    }

    /// Apply `changes` to `person`'s own workspace as the device that keeps their root: whether
    /// every one landed. The list is `person`'s own act, signed by a device that keeps their root:
    /// a server handed their own workspace holds it without that root, and writes nothing here.
    /// The device keeping the root writes it where it hosts that workspace, and where another
    /// device keeps it, through its copy carried there, as any record of theirs is written.
    async fn write_person_list(
        &self,
        person: &[u8; 32],
        changes: Vec<Invoke<WorkspaceList>>,
    ) -> bool {
        let own = person_workspace_id(person);
        let cert = self.device_cert();
        if cert.user_public_key != *person || !cert.capabilities.can_admit_devices {
            return false;
        }
        let holds_copy =
            self.state(&own).await.is_some() || self.is_member(&own).await.unwrap_or(false);
        if !holds_copy {
            tracing::warn!(
                %own,
                "the person's list of workspaces is not written here: this device holds no copy \
                 of their own workspace"
            );
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
