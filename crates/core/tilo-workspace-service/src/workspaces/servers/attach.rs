//! Handing the workspaces this device governs to the person's servers.
//!
//! Which workspaces a server holds is each workspace's governance: its holders
//! list names the server, and the workspace's actions add, move and remove it.
//! The Servers workspace's document ([`super::SERVERS`]) adds only what
//! governance cannot say yet or must follow:
//!
//! - every server of the person takes part in the Servers workspace itself, as
//!   a key holder, once it enrolled an address;
//! - a planned machine holds the workspaces its plan names: a device governing
//!   one names the machine there once it enrolled, then marks it handed in the
//!   plan, so a holder taken out of a workspace later stays out;
//! - a retired server holds nothing: a device governing a workspace that names
//!   it takes it out.
//!
//! Every holder of the Servers workspace applies this, not only a server: a
//! person whose only device is a desktop approves their first server there, and
//! it holds the workspace from then on. [`Workspaces::follow_servers`] applies it
//! now and again after each change, and [`attach_named`] is the one pass it
//! makes, so a caller that wants to know what it did reads it too.

use std::collections::BTreeSet;

use iroh_base::EndpointAddr;
use tilo_peer_state::CopyRole;

use super::{Server, Servers, person_workspace_id, read};
use crate::workspaces::Workspaces;

/// The workspaces this device handed to a server, and the ones it detached it
/// from, by workspace id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attached {
    /// The workspaces a server now holds here.
    pub held: Vec<String>,
    /// The workspaces a server no longer holds from here.
    pub released: Vec<String>,
}

impl Attached {
    /// Whether this device did nothing.
    #[must_use]
    pub fn none(&self) -> bool {
        self.held.is_empty() && self.released.is_empty()
    }
}

/// Name each enrolled server in the Servers workspace, hand a planned machine
/// the workspaces its plan names that this device governs, and take a retired
/// server out of every workspace this device governs: one pass.
///
/// A workspace this device does not govern is its governor's to change, so it
/// is skipped here.
///
/// # Errors
/// The document could not be read or written, or a workspace's keeping
/// devices could not be changed.
pub async fn attach_named(workspaces: &Workspaces) -> Result<Attached, String> {
    // A device that has no Servers workspace yet names nobody in it.
    if !super::holds_person_workspace(workspaces).await {
        return Ok(Attached::default());
    }
    let admin = person_workspace_id(&workspaces.serving_certificate().user_public_key);
    // A copy founded in the person's name waits to be founded again
    // ([`super::refound`]): what this pass wrote there would not be carried.
    if workspaces
        .state(&admin)
        .await
        .is_some_and(|state| state.governance().founder() == Some(state.owner()))
    {
        return Ok(Attached::default());
    }
    let servers = read(workspaces).await?;
    let own = workspaces.serving_certificate().device_public_key;
    let mut governed = workspaces
        .list()
        .await
        .into_iter()
        .map(|summary| summary.workspace)
        .collect::<BTreeSet<_>>();
    governed.insert(admin.clone());
    let mut attached = Attached::default();
    for (device, server) in servers.servers.iter() {
        let Some(key) = tilo_failure::ignored!(
            "a server is named by its device key in hex",
            <[u8; 32]>::try_from(hex::decode(device.as_str()).unwrap_or_default()).ok()
        ) else {
            continue;
        };
        if key == own {
            continue;
        }
        if server.retired {
            for workspace in &governed {
                if names(workspaces, workspace, device.as_str()).await == Some(true) {
                    workspaces.remove_server(workspace, device.as_str()).await?;
                    attached.released.push(workspace.clone());
                }
            }
            continue;
        }
        let Some(endpoint) = endpoint(server) else {
            // Not enrolled yet: there is no address to hand it anything at.
            continue;
        };
        if names(workspaces, &admin, device.as_str()).await == Some(false) {
            workspaces
                .attach_server(&admin, key, endpoint.clone(), CopyRole::KeyHolder)
                .await?;
            attached.held.push(admin.clone());
        }
        for (workspace, planned) in server.plan.hosting.iter() {
            let workspace = workspace.as_str();
            if planned.handed || workspace == admin {
                continue;
            }
            // Only the workspace's governor names a holder in it.
            let Some(named) = names(workspaces, workspace, device.as_str()).await else {
                continue;
            };
            if !named {
                let role = if planned.mode == "store" {
                    CopyRole::Store
                } else {
                    CopyRole::KeyHolder
                };
                workspaces
                    .attach_server(workspace, key, endpoint.clone(), role)
                    .await?;
                attached.held.push(workspace.to_owned());
            }
            super::apply(
                workspaces,
                Servers::HAND_PLANNED_WORKSPACE.with((device.clone(), workspace.to_owned())),
            )
            .await?;
        }
    }
    Ok(attached)
}

/// Whether `workspace` names the device `device` (hex) among its keeping
/// devices: `None` when this device does not govern the workspace.
async fn names(workspaces: &Workspaces, workspace: &str, device: &str) -> Option<bool> {
    let keeping = workspaces.keeping_devices(workspace).await.ok()?;
    Some(keeping.iter().any(|keeper| keeper.device == device))
}

/// The address `server` enrolled, as it is dialled: none when it enrolled
/// none.
fn endpoint(server: &Server) -> Option<EndpointAddr> {
    tilo_failure::ignored!(
        "a server's address is the one it enrolled, and one that does not read names none",
        serde_json::from_str(&server.endpoint).ok()
    )
}

impl Workspaces {
    /// Apply the servers' document to the workspaces this device governs now
    /// and again after each change to it or to a workspace's keeping devices,
    /// for as long as this device keeps the workspace. A server handed a
    /// workspace after it started is attached as the document says, whoever
    /// governs the workspace: a desktop that approved it and a server of the
    /// person alike.
    pub(crate) fn follow_servers(&self) {
        let mut writes = self.document_writes();
        let mut governance = self.governance_changes();
        let workspaces = std::sync::Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let admin = person_workspace_id(&self.serving_certificate().user_public_key);
        runtime.spawn(async move {
            loop {
                let Some(inner) = workspaces.upgrade() else {
                    return;
                };
                let this = Self { inner };
                if let Err(error) = super::refound::refound(&this).await {
                    tracing::warn!(%error, "the Servers workspace was not founded again under this device");
                }
                match attach_named(&this).await {
                    Ok(attached) if !attached.none() => {
                        tracing::info!(?attached, "the servers' document applied");
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(%error, "the servers' document was not applied");
                    }
                }
                drop(this);
                tokio::select! {
                    write = writes.recv() => match write {
                        // The document names the servers; a change to any
                        // other document says nothing about a holder.
                        Ok(write) if write.workspace == admin && write.app != super::SERVERS.0 => {}
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                    change = governance.recv() => match change {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
        });
    }
}
