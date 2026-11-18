//! Handing the workspaces this device governs to the servers its person's
//! document names as their holders.
//!
//! The Servers workspace's document ([`super::SERVERS`]) says, per server, what
//! that server should be. One action on it thus both chooses a holder and names
//! it in the workspace: the workspace names the server among its keeping
//! devices, a server whose role changed moves to the new one, and a server the
//! document stops naming lets the workspace go, each at the address it
//! enrolled.
//!
//! Every holder of the Servers workspace applies this part of the document, not
//! only a server: a person whose only device is a desktop approves their first
//! server there, and it holds the workspace from then on. [`Workspaces::follow_servers`]
//! applies it now and again after each change, and [`attach_named`] is the one
//! pass it makes, so a caller that wants to know what it did reads it too.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use iroh_base::EndpointAddr;
use tilo_peer_state::CopyRole;

use super::{STORE, Server, admin_workspace_id, read};
use crate::workspaces::Workspaces;

/// How often a holder looks for a server the document names besides the
/// changes its watches hear: a change another holder wrote reaches this copy
/// without its write watch, and a machine's own enrollment arrives the same
/// way.
const NAMED_TICK: Duration = Duration::from_secs(2);

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

/// Hand each server the document names the workspaces this device governs, in
/// the role its entry says, and let go of the ones it stops naming: one pass.
///
/// A workspace an entry names but this device does not govern is applied by
/// its governor, so it is skipped here.
///
/// # Errors
/// The document could not be read, or a workspace's keeping devices could not
/// be changed.
pub async fn attach_named(workspaces: &Workspaces) -> Result<Attached, String> {
    // A device that holds no Servers workspace yet names nobody in it, and
    // asks a holder to take it in rather than read it.
    if !super::holds_admin(workspaces).await {
        return Ok(Attached::default());
    }
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    let servers = read(workspaces).await?;
    let cert = workspaces.serving_certificate().clone();
    let mut attached = Attached::default();
    // Each workspace this device governs, which is where a holder it names is
    // named and from where one its entries stop naming is let go; the admin
    // workspace counts even before it is listed.
    let mut named_workspaces = workspaces
        .list()
        .await
        .into_iter()
        .map(|summary| summary.workspace)
        .collect::<BTreeSet<_>>();
    named_workspaces.insert(admin.clone());
    for (_, server) in servers.servers.iter() {
        named_workspaces.extend(server.holds.iter().map(|(id, _)| id.as_str().to_owned()));
    }
    for workspace in named_workspaces {
        let Ok(keeping) = workspaces.keeping_devices(&workspace).await else {
            // Not a workspace this device governs: its governor applies this
            // part of the document.
            continue;
        };
        // What each keeping device of the workspace reads: a key holder reads
        // its contents, a store only keeps and forwards them.
        let named = keeping
            .iter()
            .map(|keeper| (keeper.device.clone(), keeper.reads))
            .collect::<BTreeMap<_, _>>();
        for (device, server) in servers.servers.iter() {
            let Some(key) = tilo_failure::ignored!(
                "a server is named by its device key in hex",
                <[u8; 32]>::try_from(hex::decode(device.as_str()).unwrap_or_default()).ok()
            ) else {
                continue;
            };
            if key == cert.device_public_key {
                continue;
            }
            // Every server takes part in the admin workspace; elsewhere it
            // holds the workspaces its entry names, and a retired server
            // holds none.
            let wants =
                !server.retired && (workspace == admin || server.holding(&workspace).is_some());
            // The admin workspace is read by every server of the person.
            let role = if workspace != admin && server.holding(&workspace) == Some(STORE) {
                CopyRole::Store
            } else {
                CopyRole::KeyHolder
            };
            let reads = role == CopyRole::KeyHolder;
            let named = named.get(device.as_str());
            if wants && named.is_some_and(|named| *named != reads) {
                workspaces.set_holder_role(&workspace, key, role).await?;
                attached.held.push(workspace.clone());
            } else if wants && named.is_none() {
                let Some(endpoint) = endpoint(server) else {
                    tracing::info!(
                        %workspace,
                        server = %device.as_str(),
                        "a server the document names as a holder enrolled no address"
                    );
                    continue;
                };
                workspaces
                    .attach_server(&workspace, key, endpoint, role)
                    .await?;
                attached.held.push(workspace.clone());
            } else if !wants && named.is_some() {
                workspaces
                    .remove_server(&workspace, device.as_str())
                    .await?;
                attached.released.push(workspace.clone());
            }
        }
    }
    Ok(attached)
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
        let admin = admin_workspace_id(&self.serving_certificate().user_public_key);
        runtime.spawn(async move {
            let mut again = tokio::time::interval(NAMED_TICK);
            again.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let Some(inner) = workspaces.upgrade() else {
                    return;
                };
                let this = Self { inner };
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
                    _ = again.tick() => {}
                }
            }
        });
    }
}
