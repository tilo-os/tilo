//! What a server does as its entry in the admin workspace's document says.
//!
//! The document ([`super::SERVERS`]) is the one place a server's configuration
//! lives. Every device of the person partakes in the admin workspace, so each
//! one applies the document it reads:
//!
//! - A device that governs a workspace attaches the servers the document says
//!   hold it ([`Workspaces::attach_server`]), each in the role its entry says,
//!   moves one whose role changed ([`Workspaces::set_holder_role`]) and
//!   detaches the ones it no longer names, each at the address that server
//!   enrolled. One action on the document thus both chooses a holder and names
//!   it in the workspace, and a server never has to be told anything by hand.
//! - Each server sets what it answers devices offering it work from its own
//!   entry ([`Workspaces::set_offer`]), writes the update policy it follows
//!   ([`super::policy`]), and, once retired, lets go of every workspace it
//!   holds.
//!
//! [`follow`] applies the document as it stands, and again after each change to
//! it. Nothing here remembers a setting of its own: what a server is is what
//! its entry says.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use iroh_base::EndpointAddr;
use tilo_peer_state::CopyRole;
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
            .map(|hint| {
                (
                    hint.host.device_public_key,
                    tilo_peer_state::holder_role(hint),
                )
            })
            .collect::<BTreeMap<_, _>>();
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
            // The admin workspace is read by every server of the person.
            let role = if workspace != admin && server.holding(&workspace) == Some(super::STORE) {
                CopyRole::Store
            } else {
                CopyRole::KeyHolder
            };
            if wants && named.get(&key).is_some_and(|named| *named != role) {
                workspaces.set_holder_role(&workspace, key, role).await?;
                applied.held.push(workspace.clone());
            } else if wants && !named.contains_key(&key) {
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
                applied.held.push(workspace.clone());
            } else if !wants && named.contains_key(&key) {
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

/// Apply the document now, and again after each change to it and to the
/// admin workspace's holders, for as long as `workspaces` lives, writing the
/// update policy to `policy_request`. A change another holder wrote reaches
/// this one as its copy takes it in; one this device wrote, as it is
/// committed. A server handed the workspace after it started applies it as
/// the hand-over lands.
pub fn follow(workspaces: &Workspaces, policy_request: PathBuf) -> tokio::task::JoinHandle<()> {
    let workspaces = workspaces.clone();
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    let mut writes = workspaces.document_writes();
    let mut governance = workspaces.governance_changes();
    tokio::spawn(async move {
        apply_now(&workspaces, &policy_request).await;
        loop {
            let ours = tokio::select! {
                write = writes.recv() => match write {
                    // The app keeps one document, so any write to it is this one.
                    Ok(ResourceWrite { workspace, app, .. }) => {
                        Ok(workspace == admin && app == SERVERS.0)
                    }
                    Err(error) => Err(error),
                },
                change = governance.recv() => change.map(|workspace| workspace == admin),
            };
            match ours {
                Ok(true) => apply_now(&workspaces, &policy_request).await,
                Ok(false) => {}
                // A device that fell behind applies the document as it
                // stands, which is what every change asked for anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    apply_now(&workspaces, &policy_request).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// [`reconcile_into`], with what it did in the log rather than in an answer.
async fn apply_now(workspaces: &Workspaces, policy_request: &Path) {
    match reconcile_into(workspaces, policy_request).await {
        Ok(applied) if applied != Applied::default() => {
            tracing::info!(?applied, "the servers' document applied");
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "the servers' document was not applied");
        }
    }
}
