//! What a server does as its entry in the admin workspace's document says.
//!
//! The document ([`super::SERVERS`]) is the one place a server's configuration
//! lives. Every device of the person partakes in the admin workspace, so each
//! one applies the document it reads:
//!
//! - Which workspaces a server holds is each workspace's governance. A device
//!   that governs a workspace hands a planned machine the workspaces its plan
//!   names once, and takes a retired server out; every enrolled server takes
//!   part in the Servers workspace. Every holder of the Servers workspace does
//!   that, server or desktop alike ([`super::attach::attach_named`]).
//! - Each server writes the update policy it follows
//!   ([`super::policy`]) and the device configuration it applies
//!   ([`super::device`]), reports what the root helper said it applied in
//!   the entry's own status, goes by the name its entry gives it, and, once
//!   retired, lets go of every workspace it holds.
//! - Every holder refuses the key of each retired server: the revocation its
//!   person's root signed is in its entry ([`super::refuse_retired`]).
//! - Each server carries out what the person asked of it: every operation its
//!   own entry carries out without an answer ([`super::Asked`]) goes through
//!   [`crate::operations`], and its result is written back to that entry, where
//!   every device reads it.
//!
//! [`follow`] applies the document as it stands, and again after each change to
//! it. Nothing here remembers a setting of its own: what a server is is what
//! its entry says.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tilo_workspace_service::workspaces::{ResourceWrite, Workspaces};

use super::{SERVERS, device, device_id, person_workspace_id, policy, read};
use tilo_lock::MutexRecover as _;

/// The file in a server's state root that says it was retired, which its
/// machine reads to start its daemon no more.
const RETIRED: &str = "decommissioned";

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
    // A server whose key its person's root revoked is retired, whatever its
    // copy of the Servers workspace says, or whether it still keeps one.
    if revoked(workspaces) {
        return let_go(workspaces).await;
    }
    let mut servers = read(workspaces).await?;
    let cert = workspaces.serving_certificate().clone();
    let own = device_id(&cert.device_public_key);
    // Asked to retire, it retires its own entry while every holder still
    // reaches it, before it lets go of everything below.
    if servers
        .servers
        .get(&own)
        .is_some_and(|server| !server.retired && !server.revocation.is_empty())
    {
        super::apply(workspaces, super::Servers::RETIRE_SERVER.with(own.clone())).await?;
        servers = read(workspaces).await?;
    }
    // Every device of the person refuses the key of each retired server,
    // this one's own among them.
    super::refuse_retired(workspaces, &servers).await?;
    let own_entry = servers.servers.get(&own);
    if own_entry.is_some_and(|server| server.retired) {
        return let_go(workspaces).await;
    }
    let mut applied = Applied {
        offered: own_entry.is_some(),
        ..Applied::default()
    };
    // Every holder of the Servers workspace applies this part of the
    // document first: each enrolled server takes part in the workspace, a
    // planned machine gets the workspaces its plan names once, and a retired
    // server is taken out of every workspace. The same pass runs on every
    // device of the person, so it is applied where it is written rather than
    // only on a server.
    let attached = super::attach::attach_named(workspaces).await?;
    if let Some(entry) = own_entry {
        let settings = apply_settings(workspaces, &cert, entry, requests).await?;
        applied.policy = settings.policy;
        applied.device_config = settings.device_config;
        applied.device_status = settings.device_status;
    }
    applied.held = attached.held;
    applied.released.extend(attached.released);
    Ok(applied)
}

/// Whether this device's own person's root revoked its key: a verified entry
/// in the list it refuses keys by, however that entry reached it. A server
/// retired while it was away learns it so: every device of its person refuses
/// it, and its entry in their Servers workspace never reaches it.
fn revoked(workspaces: &Workspaces) -> bool {
    let cert = workspaces.serving_certificate();
    workspaces
        .revocations()
        .is_revoked(&cert.user_public_key, &cert.device_public_key)
}

/// What a retired server does: it serves nothing, so it lets go of everything
/// it holds, and its files stay for a later entry that wants them.
async fn let_go(workspaces: &Workspaces) -> Result<Applied, String> {
    let mut applied = Applied {
        retired: true,
        ..Applied::default()
    };
    for summary in workspaces.list().await {
        workspaces.release(&summary.workspace).await?;
        applied.released.push(summary.workspace);
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
    // The workspaces it holds show it by the name its entry gives it.
    if !entry.label.is_empty() {
        workspaces.name_holder(&entry.label).await;
    }
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

/// Apply the document now, and again after each change to it, to the admin
/// workspace's holders or to what this device refuses, for as long as
/// `workspaces` lives, writing the requests to what `requests` names, and
/// running this server's own
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
    let admin = person_workspace_id(&workspaces.serving_certificate().user_public_key);
    let mut writes = workspaces.document_writes();
    let mut governance = workspaces.governance_changes();
    // A revocation of this device's key changes no document: it reaches the
    // list every admission reads, which announces the change itself.
    let mut authority = crate::authority_epoch::subscribe();
    authority.mark_unchanged();
    // The requests one task here is running at a time: a write while it runs
    // applies the document again, and must not run the same one twice.
    let running = Arc::new(Mutex::new(HashSet::new()));
    tokio::spawn(async move {
        apply_now(&workspaces, &requests, endpoint.as_ref(), &state_root).await;
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
                // What this device refuses may have narrowed: apply the
                // document as it stands, which asks again whether its own key
                // is revoked.
                _ = authority.changed() => Ok(true),
            };
            match ours {
                Ok(true) => apply_now(&workspaces, &requests, endpoint.as_ref(), &state_root).await,
                Ok(false) => {}
                // A device that fell behind applies the document as it
                // stands, which is what every change asked for anyway.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    apply_now(&workspaces, &requests, endpoint.as_ref(), &state_root).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
            // What is asked of this server is carried out as it is found.
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
    state_root: &Path,
) {
    match reconcile_into(workspaces, requests).await {
        Ok(applied) if applied != Applied::default() => {
            tracing::info!(?applied, "the servers' document applied");
            // A retired server stays out of service: the machine does not
            // start its daemon again once it reads this.
            let retired = state_root.join(RETIRED);
            if applied.retired
                && !retired.exists()
                && let Err(error) = std::fs::write(&retired, b"retired\n")
            {
                tracing::warn!(%error, "a retired server did not note it is out of service");
            }
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
    apply_web(workspaces).await;
}

/// Apply this server's own entry's web UI setting: a server whose person turned it off
/// stops serving its browsers at once, and one they turned back on serves them again.
/// An entry that says nothing serves one, as its image comes.
async fn apply_web(workspaces: &Workspaces) {
    let cert = workspaces.serving_certificate().clone();
    let Ok(servers) = read(workspaces).await else {
        return;
    };
    let Some(entry) = servers.servers.get(&device_id(&cert.device_public_key)) else {
        return;
    };
    let serving = entry.web != super::WEB_SERVED[1];
    if let Err(error) = workspaces.serve_web(serving) {
        tracing::debug!(%error, "this server's web UI did not follow its entry");
    }
}
