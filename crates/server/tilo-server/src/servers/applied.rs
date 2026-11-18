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
//!   ([`super::policy`]) and the device configuration it applies
//!   ([`super::device`]), reports what the root helper said it applied in
//!   the entry's own status, and, once retired, lets go of every workspace it
//!   holds.
//! - Each server carries out what the person asked of it: every operation its
//!   own entry carries out without an answer ([`super::Asked`]) goes through
//!   [`crate::operations`], and its result is written back to that entry, where
//!   every device reads it.
//!
//! [`follow`] applies the document as it stands, and again after each change to
//! it. Nothing here remembers a setting of its own: what a server is is what
//! its entry says.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iroh_base::EndpointAddr;
use tilo_peer_state::CopyRole;
use tilo_workspace_service::workspaces::{Offer, ResourceWrite, Workspaces};

use super::{SERVERS, admin_workspace_id, device, device_id, policy, read};
use tilo_lock::MutexRecover as _;

/// How often this device looks for an operation asked of it, besides the
/// changes its write watch hears.
const PENDING_TICK: std::time::Duration = std::time::Duration::from_secs(2);

/// Where a server writes what its entry asks of the machine, for the root
/// helpers to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requests {
    /// The update policy it follows.
    pub policy: PathBuf,
    /// The configuration of the machine itself.
    pub device: device::Paths,
}

impl Requests {
    /// The requests beside the update policy at `policy`, so a test or an
    /// embedder keeps them all in one directory.
    #[must_use]
    pub fn at(policy: impl Into<PathBuf>) -> Self {
        let policy = policy.into();
        let dir = policy
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Self {
            policy,
            device: device::Paths::under(&dir),
        }
    }
}

impl Default for Requests {
    fn default() -> Self {
        Self::at(policy::POLICY_REQUEST)
    }
}

/// What applying the servers' document did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each says whether one part of the document applied; they are read one at a time"
)]
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
    /// The revision of the device configuration written, when it changed.
    pub device_config: Option<String>,
    /// Whether the machine's last report became the entry's status.
    pub device_status: bool,
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
    reconcile_into(workspaces, &Requests::default()).await
}

/// [`reconcile`] with the requests written where the root helpers read
/// them ([`Requests`]).
///
/// # Errors
/// As [`reconcile`].
pub async fn reconcile_into(
    workspaces: &Workspaces,
    requests: &Requests,
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
            let settings = apply_settings(workspaces, &cert, entry, requests).await?;
            applied.policy = settings.policy;
            applied.device_config = settings.device_config;
            applied.device_status = settings.device_status;
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

/// Apply this device's own entry to itself: the update policy it follows, the
/// configuration of its machine, and the report the machine's root helper
/// left, which becomes the entry's status so the person reads what it applied
/// or why not.
async fn apply_settings(
    workspaces: &Workspaces,
    cert: &tilo_user_identity::DeviceCert,
    entry: &super::Server,
    requests: &Requests,
) -> Result<Applied, String> {
    let mut applied = Applied {
        policy: policy::write(entry, &requests.policy)?,
        device_config: device::write(entry, &requests.device)?,
        ..Applied::default()
    };
    if let Some(status) = device::report(&requests.device)
        && entry.device.status != status
    {
        super::apply(
            workspaces,
            super::Servers::REPORT_DEVICE_CONFIG.with((
                device_id(&cert.device_public_key),
                status.revision,
                status.state,
                status.detail,
            )),
        )
        .await?;
        applied.device_status = true;
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
/// requests to what `requests` names, and running this server's own
/// operations with its state at `state_root`. A change another holder wrote
/// reaches this one as its copy takes it in; one this device wrote, as it is
/// committed. A server handed the workspace after it started applies it as
/// the hand-over lands.
///
/// When the configuration of the machine itself changed and `endpoint` is the
/// one this device is reached at, the network change is confirmed once this
/// device is reached again ([`device::confirm`]); without a
/// confirmation the root helper puts the previous network back.
#[must_use]
pub fn follow(
    workspaces: &Workspaces,
    requests: Requests,
    endpoint: Option<iroh::Endpoint>,
    state_root: PathBuf,
) -> tokio::task::JoinHandle<()> {
    let workspaces = workspaces.clone();
    let admin = admin_workspace_id(&workspaces.serving_certificate().user_public_key);
    let mut writes = workspaces.document_writes();
    let mut governance = workspaces.governance_changes();
    // The requests one task here is running at a time: a write while it runs
    // applies the document again, and must not run the same one twice.
    let running = Arc::new(Mutex::new(HashSet::new()));
    tokio::spawn(async move {
        apply_now(&workspaces, &requests, endpoint.as_ref()).await;
        answer_now(&workspaces, &state_root, &running).await;
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
                // What is asked of this server may land in a copy the write
                // watch does not hear, so it is looked for now and then too.
                () = tokio::time::sleep(PENDING_TICK) => Ok(false),
            };
            match ours {
                Ok(true) => apply_now(&workspaces, &requests, endpoint.as_ref()).await,
                Ok(false) => {}
                // A device that fell behind applies the document as it
                // stands, which is what every change asked for anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    apply_now(&workspaces, &requests, endpoint.as_ref()).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
            // An ask reaches this copy through a holder's, which the write
            // watch does not always hear: what is asked of this server is
            // carried out as it is found.
            answer_now(&workspaces, &state_root, &running).await;
        }
    })
}

/// Carry out the operations this device's own entry holds without an
/// answer, each in a task of its own so a slow one holds up no change to the
/// document.
async fn answer_now(
    workspaces: &Workspaces,
    state_root: &Path,
    running: &Arc<Mutex<HashSet<String>>>,
) {
    let cert = workspaces.serving_certificate().clone();
    let Ok(servers) = read(workspaces).await else {
        return;
    };
    let Some(entry) = servers.servers.get(&device_id(&cert.device_public_key)) else {
        return;
    };
    if entry.retired {
        return;
    }
    let pending: Vec<(String, super::Asked)> = entry
        .pending()
        .map(|(id, asked)| (id.as_str().to_owned(), asked.clone()))
        .collect();
    for (request, asked) in pending {
        if !running.lock_recover().insert(request.clone()) {
            continue;
        }
        let workspaces = workspaces.clone();
        let cert = cert.clone();
        let state_root = state_root.to_path_buf();
        let running = Arc::clone(running);
        tokio::spawn(async move {
            let result = match asked.operation() {
                Some(operation) => {
                    crate::operations::operate(&state_root, Some(&workspaces), &cert, operation)
                        .await
                }
                None => Err("no such operation".to_owned()),
            };
            let answer = match result {
                Ok(result) => serde_json::to_string(&result),
                // What it could not do is its answer too: the person reads
                // why in the entry, as they read what it did.
                Err(error) => {
                    serde_json::to_string(&tilo_server_protocol::transport::ServerOperationResult {
                        operation: asked.operation.clone(),
                        done: false,
                        summary: error,
                        answer: tilo_server_protocol::transport::ServerAnswer::Nothing,
                    })
                }
            };
            let action = match answer {
                Ok(answer) => super::Servers::ANSWER_OPERATION.with((
                    device_id(&cert.device_public_key),
                    request.clone(),
                    answer,
                )),
                Err(error) => {
                    tracing::warn!(%error, "an operation's answer does not write");
                    running.lock_recover().remove(&request);
                    return;
                }
            };
            if let Err(error) = super::apply(&workspaces, action).await {
                tracing::warn!(%error, %request, "an operation's answer was not recorded");
            }
            running.lock_recover().remove(&request);
        });
    }
}

/// [`reconcile_into`], with what it did in the log rather than in an answer,
/// and a changed machine configuration confirmed once the device is reached
/// again.
async fn apply_now(
    workspaces: &Workspaces,
    requests: &Requests,
    endpoint: Option<&iroh::Endpoint>,
) {
    match reconcile_into(workspaces, requests).await {
        Ok(applied) if applied != Applied::default() => {
            tracing::info!(?applied, "the servers' document applied");
            if let (Some(revision), Some(endpoint)) = (applied.device_config, endpoint)
                && device::awaits_confirmation(&requests.device)
            {
                tokio::spawn(device::confirm(
                    requests.device.clone(),
                    endpoint.clone(),
                    revision,
                ));
            }
        }
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(%error, "the servers' document was not applied");
        }
    }
}
