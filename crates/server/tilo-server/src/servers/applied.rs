//! What a server does as its entry in the admin workspace's document says.
//!
//! The document ([`super::SERVERS`]) is the one place a server's configuration
//! lives. Every device of the person partakes in the admin workspace, so each
//! one applies the document it reads:
//!
//! - A device that governs a workspace attaches the servers the document says
//!   hold it ([`Workspaces::attach_host`]) and detaches the ones it no longer
//!   names, each at the address that server enrolled. One action on the
//!   document thus both chooses a holder and names it in the workspace, and a
//!   server never has to be told anything by hand.
//! - Each server sets what it answers devices offering it work from its own
//!   entry ([`Workspaces::set_offer`]), writes the update policy it follows
//!   ([`super::policy`]), and, once retired, lets go of every workspace it
//!   holds.
//!
//! [`follow`] applies the document as it stands, and again after each change to
//! it. Nothing here remembers a setting of its own: what a server is is what
//! its entry says.

use std::collections::BTreeSet;
use std::path::Path;

use iroh_base::EndpointAddr;
use tilo_workspace_service::workspaces::{Offer, ResourceWrite, Workspaces};

use super::{SERVERS, admin_workspace_id, device_id, policy, read};

/// What applying the servers' document did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// The workspaces this device attached a server to.
    pub held: Vec<String>,
    /// The workspaces this device detached a server from, and the ones a
    /// retired server let go of.
    pub released: Vec<String>,
    /// Whether this device's own entry was found.
    pub offered: bool,
    /// Whether the update policy a server follows was written.
    pub policy: bool,
    /// Whether this device's own entry says it was retired.
    pub retired: bool,
}

/// Apply the admin workspace's [`SERVERS`] document as it stands: attach and
/// detach the servers of the workspaces this device governs, and apply this
/// device's own entry.
///
/// # Errors
/// The document could not be read, or a workspace's hosts could not be changed.
pub async fn reconcile(workspaces: &Workspaces) -> Result<Applied, String> {
    reconcile_into(workspaces, Path::new(policy::POLICY_REQUEST)).await
}

/// [`reconcile`] with the update policy written to `policy_request`, where
/// the root-run helper reads it
/// ([`policy::POLICY_REQUEST`](super::policy::POLICY_REQUEST)).
///
/// # Errors
/// As [`reconcile`].
pub async fn reconcile_into(
    workspaces: &Workspaces,
    policy_request: &Path,
) -> Result<Applied, String> {
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    let servers = read(workspaces).await?;
    let cert = workspaces.serving_certificate().clone();
    let own_entry = servers.servers.get(&device_id(&cert.device_public_key));
    let mut applied = Applied {
        offered: own_entry.is_some(),
        retired: own_entry.is_some_and(|server| server.retired),
        ..Applied::default()
    };
    // What this device answers devices offering it work: the entry says what
    // it offers, and a retired server takes nothing.
    workspaces.set_offer(
        own_entry
            .filter(|server| !server.retired)
            .map_or_else(Offer::none, |server| {
                Offer::of(server.storage_gb, server.compute, server.machines)
            }),
    );
    if let Some(entry) = own_entry {
        if entry.retired {
            // A retired server serves nothing: it lets go of everything it
            // holds, and its files stay for a later entry that wants them.
            for summary in workspaces.list().await {
                workspaces.release(&summary.workspace).await?;
                applied.released.push(summary.workspace);
            }
        } else {
            applied.policy = policy::write(entry, policy_request)?;
        }
    }
    // Each workspace this device governs, which is where a holder it names is
    // named and from where one its entries stop naming is let go; a workspace
    // an entry names but this device does not govern is applied by its
    // governor. The admin workspace counts even before it is listed.
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
        let Ok(hosts) = workspaces.hosts(&workspace).await else {
            // Not a workspace this device governs: its governor applies this
            // part of the document.
            continue;
        };
        let named = hosts
            .iter()
            .map(|hint| hint.host.device_public_key)
            .collect::<BTreeSet<_>>();
        for (device, server) in servers.servers.iter() {
            let Ok(key) = <[u8; 32]>::try_from(hex::decode(device.as_str()).unwrap_or_default())
            else {
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
            if wants && !named.contains(&key) {
                let Some(endpoint) = endpoint(server) else {
                    tracing::info!(
                        %workspace,
                        server = %device.as_str(),
                        "a server the document names as a holder enrolled no address"
                    );
                    continue;
                };
                workspaces.attach_host(&workspace, key, endpoint).await?;
                applied.held.push(workspace.clone());
            } else if !wants && named.contains(&key) {
                workspaces.detach_host(&workspace, key, None).await?;
                applied.released.push(workspace.clone());
            }
        }
    }
    Ok(applied)
}

/// The address `server` enrolled, as it is dialled: none when it enrolled
/// none.
fn endpoint(server: &super::Server) -> Option<EndpointAddr> {
    serde_json::from_str(&server.endpoint).ok()
}

/// Apply the document now, and again after each change to it,
/// for as long as `workspaces` lives.
pub fn follow(workspaces: &Workspaces) -> tokio::task::JoinHandle<()> {
    let workspaces = workspaces.clone();
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    let mut writes = workspaces.document_writes();
    tokio::spawn(async move {
        apply_now(&workspaces).await;
        loop {
            match writes.recv().await {
                Ok(ResourceWrite { workspace, app, .. })
                    if workspace == admin && app == SERVERS.0 =>
                {
                    // The app keeps one document, so any write to it is this one.
                    apply_now(&workspaces).await;
                }
                Ok(_) => {}
                // A device that fell behind the document's writes applies it as
                // it stands, which is what every write asked for anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    apply_now(&workspaces).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// [`reconcile`], with what it did in the log rather than in an answer.
async fn apply_now(workspaces: &Workspaces) {
    match reconcile(workspaces).await {
        Ok(applied) if applied != Applied::default() => {
            tracing::info!(?applied, "the servers' document applied");
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "the servers' document was not applied");
        }
    }
}
