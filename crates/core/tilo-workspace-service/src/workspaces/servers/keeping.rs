//! What an always-on server of the person keeps: every workspace in their list.
//!
//! The person's list of workspaces is a resource of their own workspace
//! (`identity.workspaces`, a [`WorkspaceList`]). A server with an entry among their servers
//! ([`super::SERVERS`]) keeps each workspace the list names, unless its entry
//! turned keeping off for it ([`Server::keeps_not`]). It enters one it does
//! not keep yet the way every device of the person enters a workspace of
//! theirs: as a member copy, at the address the list keeps for it
//! ([`crate::workspaces::Workspaces::hold_as_members`]). One its entry no longer keeps it
//! lets go of ([`Workspaces::release`]); its files stay, and keeping it again
//! takes it up from them.
//!
//! It learns of a change from the change itself: a write to the list or to
//! the servers' document, or the person's workspace reaching this device
//! ([`Workspaces::follow_person_list`]). Nothing reads either on a timer.
//!
//! What it keeps is each workspace's to say: `copies.read` (`…/servers#keeps`)
//! names the devices keeping it.

use std::sync::Arc;

use tilo_peer_state::join::TransportHint;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::IDENTITY_LIST;
use tilo_user_identity::WorkspaceList;
use tokio::sync::Mutex as AsyncMutex;

use super::{SERVERS, person_workspace_id, read};
use crate::workspaces::person_workspace::{self, holds_person_workspace};
use crate::workspaces::{GovernedWorkspace, RELEASED, Workspaces};

/// What one pass over the person's list did, by workspace id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// The workspaces this server keeps from now on.
    pub entered: Vec<String>,
    /// The workspaces it let go of.
    pub released: Vec<String>,
}

impl Kept {
    /// Whether the pass changed nothing.
    #[must_use]
    pub fn none(&self) -> bool {
        self.entered.is_empty() && self.released.is_empty()
    }
}

/// Keep every workspace of this device's person's list its entry among their
/// servers keeps, and let go of each it keeps no longer: one pass. A device
/// with no entry there, or a retired one, keeps what it keeps.
///
/// # Errors
/// The servers' document or the list could not be read, or a workspace could
/// not be let go of or taken up again from its files.
pub async fn keep_listed(workspaces: &Workspaces) -> Result<Kept, String> {
    let own = workspaces.device_cert().clone();
    let person = person_workspace_id(&own.user_public_key);
    if !holds_person_workspace(workspaces).await {
        return Ok(Kept::default());
    }
    let Some(entry) = read(workspaces)
        .await?
        .active(&own.device_public_key)
        .cloned()
    else {
        return Ok(Kept::default());
    };
    let mut kept = Kept::default();
    for workspace in &entry.keeps_not {
        if *workspace != person && workspaces.state(workspace).await.is_some() {
            workspaces.release(workspace).await?;
            kept.released.push(workspace.clone());
        }
    }
    let mut entering = Vec::new();
    for (workspace, hint) in listed(workspaces, &own.user_public_key).await? {
        let id = workspace.to_string();
        if id == person || !entry.keeps(&id) || workspaces.state(&id).await.is_some() {
            continue;
        }
        if keep_again(workspaces, &id).await? {
            kept.entered.push(id);
            continue;
        }
        match hint {
            Some(hint) => entering.push((workspace, hint)),
            None => {
                tracing::warn!(workspace = %id, "the person's list says nowhere this server reaches a workspace it keeps");
            }
        }
    }
    workspaces.hold_as_members(entering.iter().cloned()).await;
    for (workspace, _) in entering {
        let id = workspace.to_string();
        if workspaces.state(&id).await.is_some() {
            kept.entered.push(id);
        }
    }
    Ok(kept)
}

/// Each workspace of `person`'s list, with the address the list keeps for it:
/// `None` for one no device of theirs said where it is kept.
async fn listed(
    workspaces: &Workspaces,
    person: &[u8; 32],
) -> Result<Vec<(WorkspaceId, Option<TransportHint>)>, String> {
    let list = person_workspace::read_on::<WorkspaceList>(
        workspaces,
        &person_workspace_id(person),
        IDENTITY_LIST,
    )
    .await?;
    Ok(list
        .listed()
        .filter_map(|(id, listed)| {
            let workspace = WorkspaceId::parse(&id.to_string())?;
            let hint = listed.address().and_then(|bytes| {
                tilo_failure::ignored!(
                    "an address the list keeps that does not read names none",
                    postcard::from_bytes::<TransportHint>(&bytes).ok()
                )
            });
            Some((workspace, hint))
        })
        .collect())
}

/// Take up again the copy of `workspace` this device let go of, from the files
/// it kept, when the workspace still names this device among the ones keeping
/// it: what it had then is where it stands, and the other keepers bring what
/// it missed. A member copy is taken up through the device serving it instead,
/// and a workspace with no files here is entered.
async fn keep_again(workspaces: &Workspaces, workspace: &str) -> Result<bool, String> {
    let path = workspaces.inner.dir.join(format!("{workspace}.workspace"));
    let released = path.with_extension(RELEASED);
    if !released.exists() {
        return Ok(false);
    }
    let governed = GovernedWorkspace::open(path, workspaces.device_keys().as_ref())?;
    let own = workspaces.device_cert().device_public_key;
    if governed.member.is_some()
        || !governed
            .peer()
            .governance()
            .keeping_devices()
            .iter()
            .any(|hint| hint.host.device_public_key == own)
    {
        return Ok(false);
    }
    std::fs::remove_file(&released).map_err(|error| error.to_string())?;
    workspaces.serve_direct(&governed)?;
    let reading = governed.reading();
    let state = Arc::new(AsyncMutex::new(governed));
    workspaces.inner.readings.hold(workspace, &state, reading);
    workspaces
        .inner
        .workspaces
        .lock()
        .await
        .insert(workspace.to_owned(), Arc::clone(&state));
    workspaces.serve_kept(std::slice::from_ref(&state)).await?;
    workspaces.keep_reading(&state).await;
    let _ = workspaces.inner.changes.send(workspace.to_owned());
    tracing::info!(%workspace, "a workspace let go of here is kept again");
    Ok(true)
}

impl Workspaces {
    /// Keep the person's list as this device's entry among their servers
    /// says, now and again after each change to the list, to the servers'
    /// document, or to the person's workspace here, for as long as this
    /// device keeps its workspaces.
    pub(crate) fn follow_person_list(&self) {
        let mut writes = self.document_writes();
        let mut governance = self.governance_changes();
        let workspaces = Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let person = person_workspace_id(&self.device_cert().user_public_key);
        runtime.spawn(async move {
            loop {
                {
                    let Some(inner) = workspaces.upgrade() else {
                        return;
                    };
                    match keep_listed(&Self { inner }).await {
                        Ok(kept) if !kept.none() => {
                            tracing::info!(?kept, "this server keeps the person's list");
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(%error, "this server's keeping of the person's list was not applied");
                        }
                    }
                }
                loop {
                    use tokio::sync::broadcast::error::RecvError;
                    let changed = tokio::select! {
                        write = writes.recv() => match write {
                            Ok(write) => {
                                write.workspace == person
                                    && ((write.app == IDENTITY_LIST.0 && write.record == IDENTITY_LIST.1)
                                        || write.app == SERVERS.0)
                            }
                            Err(RecvError::Lagged(_)) => true,
                            Err(RecvError::Closed) => return,
                        },
                        change = governance.recv() => match change {
                            Ok(workspace) => workspace == person,
                            Err(RecvError::Lagged(_)) => true,
                            Err(RecvError::Closed) => return,
                        },
                    };
                    if changed {
                        break;
                    }
                }
            }
        });
    }
}
