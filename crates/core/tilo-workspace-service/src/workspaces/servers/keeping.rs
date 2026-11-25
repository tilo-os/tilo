//! What each device of the person holds of their list of workspaces.
//!
//! The person's list of workspaces is a resource of their own workspace
//! (`identity.workspaces`, a [`WorkspaceList`]). Every device of theirs enters
//! each workspace the list shows on it, as a member copy at the address the
//! list keeps for it ([`crate::workspaces::Workspaces::hold_as_members`]): the
//! device's daemon does, and the desktop session shows what it holds. A server
//! with an entry among their servers ([`super::SERVERS`]) keeps each workspace
//! the list names instead, unless its entry turned keeping off for it
//! ([`Server::keeps_not`]). One its entry no longer keeps it lets go of
//! ([`Workspaces::release`]); its files stay, and keeping it again takes it up
//! from them.
//!
//! On every device of the person, a workspace held as the device the machine
//! was copied from is entered again as this device at the device serving it
//! ([`Workspaces::held_as_another_device`]): the daemon does that, never the
//! desktop session.
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

/// Enter every workspace of this device's person's list that shows on this
/// device, or, on a server of theirs, that its entry among their servers keeps,
/// letting go of each that entry keeps no longer: one pass. Every device first
/// enters as itself what it holds as the device it was copied from.
///
/// # Errors
/// The servers' document or the list could not be read, or a workspace could
/// not be let go of or taken up again from its files.
pub async fn keep_listed(workspaces: &Workspaces) -> Result<Kept, String> {
    let own = workspaces.device_cert().clone();
    let person = person_workspace_id(&own.user_public_key);
    // What this device holds as the device it was copied from it enters as itself, at the device
    // serving it: every device of the person's, listed or not, and never its desktop session.
    let mut kept = enter_as_this_device(workspaces).await;
    if !holds_person_workspace(workspaces).await {
        tracing::debug!("this device holds no copy of its person's own workspace: no list to keep");
        return Ok(kept);
    }
    // A server keeps what its entry keeps; every other device what the list shows on it.
    let entry = read(workspaces)
        .await?
        .active(&own.device_public_key)
        .cloned();
    for workspace in entry.iter().flat_map(|entry| &entry.keeps_not) {
        if *workspace != person && workspaces.state(workspace).await.is_some() {
            workspaces.release(workspace).await?;
            kept.released.push(workspace.clone());
        }
    }
    let mut entering = Vec::new();
    for (workspace, hint, shown) in
        listed(workspaces, &own.user_public_key, &own.device_public_key).await?
    {
        let id = workspace.to_string();
        let keeps = entry.as_ref().map_or(shown, |entry| entry.keeps(&id));
        if id == person || !keeps || workspaces.state(&id).await.is_some() {
            continue;
        }
        if keep_again(workspaces, &id).await? {
            kept.entered.push(id);
            continue;
        }
        match hint {
            Some(hint) => entering.push((workspace, hint)),
            // The device that made it says where it is reached once it holds it: that write
            // to the list brings another pass.
            None => {
                tracing::debug!(workspace = %id, "the person's list says nowhere yet this device reaches a workspace of theirs");
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

/// Enter as this device each workspace held here as the device this machine was copied from
/// ([`Workspaces::held_as_another_device`]), and say which it holds as itself now.
async fn enter_as_this_device(workspaces: &Workspaces) -> Kept {
    let entering = workspaces.held_as_another_device().await;
    let ids: Vec<String> = entering.iter().map(|(id, _)| id.to_string()).collect();
    workspaces.hold_as_members(entering).await;
    let mut kept = Kept::default();
    for id in ids {
        if workspaces.holds_as_this_device(&id).await {
            kept.entered.push(id);
        }
    }
    kept
}

/// Each workspace of `person`'s list, with the address the list keeps for it
/// (`None` for one no device of theirs said where it is kept) and whether it
/// shows on `device`.
async fn listed(
    workspaces: &Workspaces,
    person: &[u8; 32],
    device: &[u8; 32],
) -> Result<Vec<(WorkspaceId, Option<TransportHint>, bool)>, String> {
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
            Some((workspace, hint, listed.shows_on(device)))
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
                    match Box::pin(keep_listed(&Self { inner })).await {
                        Ok(kept) if !kept.none() => {
                            tracing::info!(?kept, "this device keeps the person's list");
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
