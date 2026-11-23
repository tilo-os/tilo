//! The person's shell, as this device's daemon answers it on its broker
//! (`tilo_workspace_service::apps::shell`): the desktop session of the person signed in on this
//! computer, which reaches their workspaces through the daemon and never on its own.
//!
//! - Who a shell is: a process of the account this daemon runs as, which the launcher did not
//!   start as an app ([`Accounts`]).
//! - Its documents: the daemon's own reads and writes of the workspaces it holds, each write
//!   sealed with the keys of the document's group this device reads and signed as the device.
//! - Its asks: a workspace's governance action is answered where this device keeps the
//!   workspace, and asked of the device keeping it over tilo-actions/1 otherwise; the host's own
//!   operations are this device's workspace service's ([`Actions`]).
//! - How its workspaces stand: [`Standing`], read again whenever a workspace changes.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use runtime_collab::wire::{Event, Receipt};
use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvocationReceipt, InvokeArgument, InvokeRequest, ReceiptStatus,
};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::workspace_view::{LIST, Listed, resource};
use tilo_workspace_service::apps::services::{PublishedResources, Reading, ResourceWatch};
use tilo_workspace_service::apps::shell::{
    ShellAccounts, ShellActions, ShellCaller, ShellInvoked, ShellPeer, ShellServices,
};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::standing;
use tokio::sync::{Notify, mpsc, oneshot};

use super::workspaces::ServiceWorkspaces;
use crate::apps::Apps;

/// What this device's broker answers its person's shell with, read again as `workspaces`
/// changes.
pub(crate) fn services(workspaces: &Arc<ServiceWorkspaces>, apps: &Arc<Apps>) -> ShellServices {
    let standing = Standing::new(Arc::clone(workspaces));
    standing.follow();
    let copies = workspaces.copies().clone();
    ShellServices {
        accounts: Arc::new(Accounts {
            uid: own_uid(),
            person: workspaces.device_cert().user_public_key,
            apps: Arc::clone(apps),
        }),
        documents: Arc::new(ServiceDocuments::of(copies.clone())),
        writes: Arc::new(ServiceWrites::of(copies.clone())),
        document_writes: Arc::new(move || copies.document_writes()),
        actions: Arc::new(Actions {
            workspaces: Arc::clone(workspaces),
            changed: Arc::clone(&standing.changed),
        }),
        standing,
    }
}

/// The account this daemon runs as.
fn own_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Who is this device's person's shell: a process of the account the daemon runs as, which the
/// launcher did not start as an app. An app runs under its person's account too, and would
/// otherwise reach the device's own writes.
pub(crate) struct Accounts {
    uid: u32,
    person: [u8; 32],
    apps: Arc<Apps>,
}

impl ShellAccounts for Accounts {
    fn admit(&self, peer: ShellPeer) -> Result<ShellCaller, String> {
        if peer.uid != self.uid {
            return Err(format!(
                "this device's Tilo serves the account it runs as ({}), not account {}",
                self.uid, peer.uid
            ));
        }
        if self.apps.started_as_app(peer.pid) {
            return Err(format!(
                "process {} is an app this device started, and an app is never the shell",
                peer.pid
            ));
        }
        Ok(ShellCaller {
            person: self.person,
            uid: peer.uid,
        })
    }
}

/// What a shell's asks reach: this device's workspace service, and the devices keeping the
/// workspaces it holds as a member, over tilo-actions/1.
struct Actions {
    workspaces: Arc<ServiceWorkspaces>,
    /// Told once an ask settled, so the workspaces' standing is read again.
    changed: Arc<Notify>,
}

impl ShellActions for Actions {
    fn invoke(&self, caller: &ShellCaller, request: InvokeRequest) -> ShellInvoked {
        let workspaces = Arc::clone(&self.workspaces);
        let changed = Arc::clone(&self.changed);
        let caller = *caller;
        Box::pin(async move {
            let answer = ask(&workspaces, caller, request).await;
            changed.notify_one();
            answer
        })
    }
}

/// `request` asked as this device for `caller`, answered where it is decided.
async fn ask(
    workspaces: &ServiceWorkspaces,
    caller: ShellCaller,
    request: InvokeRequest,
) -> Result<Value, String> {
    let cert = workspaces.device_cert().clone();
    if caller.person != cert.user_public_key {
        return Err("this device acts for its own person only".to_owned());
    }
    // The host's own operations are this device's workspace service's.
    if request.target.provider == tilo_server_session::workspaces::HOST_ACTIONS_PROVIDER {
        let arguments = match request.argument {
            InvokeArgument::Json(arguments) => arguments,
            InvokeArgument::None => Map::new(),
            InvokeArgument::Text(_) => {
                return Err("the host's operations take named arguments".to_owned());
            }
        };
        return workspaces
            .service()
            .ask_own(&cert, &request.target.address.action, arguments)
            .await;
    }
    let workspace = tilo_actions::remote::workspace_governance_operation(&request.target.address)
        .map(|(workspace, _)| workspace.to_owned())
        .ok_or("the shell asks a workspace's governance or the host, and nothing else")?;
    let id = WorkspaceId::parse(&workspace).ok_or("that is not a workspace")?;
    // A workspace this device keeps answers here, as it answers every member's device.
    if workspaces.copies().governs_workspace(&workspace).await {
        let as_device = Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace);
        let provider = workspaces.copies().device_actions();
        let receipt: InvocationReceipt = provider
            .invoke(&as_device, request)
            .await
            .ok_or("the workspace dropped the request")?
            .map_err(|error| error.to_string())?;
        return match receipt.status {
            ReceiptStatus::Accepted => {
                follow(provider.as_ref(), &as_device, &receipt.invocation).await
            }
            status => settled(status),
        };
    }
    // Kept elsewhere: asked of the device keeping it, at the address that device signed.
    let peer = workspaces
        .peer(id)
        .ok_or("this device does not hold the workspace")?;
    let (device, addr) = standing::keeping_device_elsewhere(&peer, &cert.device_public_key)
        .ok_or("this device reaches no device keeping the workspace")?;
    let (actions, endpoint, _) = workspaces
        .turn_action_service()
        .ok_or("this device runs no action service")?;
    actions.reach_device_at(device, *addr.id.as_bytes());
    let followed = Arc::new(actions.remote(&endpoint, &addr, &workspace))
        .invoke_until_settled(request, tilo_peer_state::actions::FOLLOW_DEADLINE)
        .await;
    match followed {
        Some(Ok(update)) => finished(update),
        Some(Err(error)) => Err(error.to_string()),
        None => Err("the workspace's keeping device dropped the request".to_owned()),
    }
}

/// Follow `invocation` on `provider` until it finishes, as long as a request is followed.
async fn follow(
    provider: &(dyn tilo_actions::remote::ActionProvider + Send + Sync),
    caller: &Caller,
    invocation: &tilo_actions::InvocationId,
) -> Result<Value, String> {
    let followed = tokio::time::timeout(tilo_peer_state::actions::FOLLOW_DEADLINE, async {
        loop {
            let state = provider
                .observe(caller, invocation)
                .ok_or("the workspace lost the request")?;
            if !matches!(
                state.update,
                InvocationUpdate::Accepted { .. }
                    | InvocationUpdate::Progress { .. }
                    | InvocationUpdate::Item { .. }
            ) {
                return finished(state.update);
            }
            provider
                .next_change(caller, invocation)
                .ok_or("the workspace does not say when the request moves")?
                .await;
        }
    })
    .await;
    followed.unwrap_or_else(|_| Err("the workspace did not finish the request in time".to_owned()))
}

/// What a finished invocation comes to.
fn finished(update: InvocationUpdate) -> Result<Value, String> {
    match update {
        InvocationUpdate::Succeeded { output, .. } => Ok(output.value.unwrap_or_default()),
        InvocationUpdate::Failed { error, .. } => Err(error.message),
        InvocationUpdate::Cancelled { message, .. } => {
            Err(message.unwrap_or_else(|| "the request was cancelled".to_owned()))
        }
        InvocationUpdate::Accepted { .. }
        | InvocationUpdate::Progress { .. }
        | InvocationUpdate::Item { .. } => Err("the request did not finish".to_owned()),
    }
}

/// What a receipt the workspace answered at once comes to.
fn settled(status: ReceiptStatus) -> Result<Value, String> {
    match status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error.to_string()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Cancelled { message } => {
            Err(message.unwrap_or_else(|| "the request was cancelled".to_owned()))
        }
        ReceiptStatus::Accepted => Err("the request did not finish".to_owned()),
    }
}

/// How the person's workspaces stand, as the shell watches them: [`LIST`], and each workspace's
/// reading at [`resource`], read again whenever something changes there and published to every
/// watch whose resource reads differently.
pub(crate) struct Standing {
    workspaces: Arc<ServiceWorkspaces>,
    /// Told whenever a workspace may read differently; many tellings before a read are one.
    changed: Arc<Notify>,
    state: Mutex<Published>,
}

#[derive(Default)]
struct Published {
    revision: u64,
    /// The readings as last published, by resource.
    readings: BTreeMap<String, Vec<u8>>,
    watches: BTreeMap<u64, (String, mpsc::UnboundedSender<Event>)>,
    next_watch: u64,
}

impl Standing {
    fn new(workspaces: Arc<ServiceWorkspaces>) -> Arc<Self> {
        let standing = Arc::new(Self {
            workspaces,
            changed: Arc::new(Notify::new()),
            state: Mutex::default(),
        });
        standing.read_again();
        standing
    }

    /// Read again whenever a workspace changes here: its governance decided something, another
    /// device's write was taken in, this device took a workspace in, or a shell's ask settled.
    fn follow(self: &Arc<Self>) {
        let copies = self.workspaces.copies().clone();
        let tell = |changed: &Arc<Notify>| {
            let changed = Arc::clone(changed);
            move || changed.notify_one()
        };
        let mut governance = copies.governance_changes();
        let governed = tell(&self.changed);
        tilo_rt::app().spawn(async move {
            while !matches!(
                governance.recv().await,
                Err(tokio::sync::broadcast::error::RecvError::Closed)
            ) {
                governed();
            }
        });
        let (written, lagged) = (tell(&self.changed), tell(&self.changed));
        tilo_rt::app().spawn(
            tilo_workspace_service::apps::collab_inbound::follow_admitted_writes(
                &copies,
                move |_| written(),
                lagged,
            ),
        );
        for taken in [copies.members_held(), self.workspaces.entered()] {
            let took = tell(&self.changed);
            tilo_rt::app().spawn(async move {
                loop {
                    taken.notified().await;
                    took();
                }
            });
        }
        let standing = Arc::downgrade(self);
        let changed = Arc::clone(&self.changed);
        tilo_rt::app().spawn(async move {
            loop {
                changed.notified().await;
                let Some(standing) = standing.upgrade() else {
                    return;
                };
                // The read waits on the workspaces' own locks: off the runtime's workers.
                if tokio::task::spawn_blocking(move || standing.read_again())
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
    }

    /// Read every resource again, and publish those that read differently now.
    fn read_again(&self) {
        let readings = self.read_all();
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changed: BTreeSet<String> = readings
            .keys()
            .chain(state.readings.keys())
            .filter(|resource| readings.get(*resource) != state.readings.get(*resource))
            .cloned()
            .collect();
        if changed.is_empty() {
            return;
        }
        state.revision = state.revision.wrapping_add(1);
        state.readings = readings;
        let revision = state.revision;
        let Published {
            readings, watches, ..
        } = &mut *state;
        watches.retain(|_, (resource, sink)| {
            !changed.contains(resource)
                || sink
                    .send(Event::Reading {
                        resource: resource.clone(),
                        revision,
                        reading: readings.get(resource).cloned(),
                    })
                    .is_ok()
        });
        drop(state);
    }

    fn read_all(&self) -> BTreeMap<String, Vec<u8>> {
        let cert = self.workspaces.device_cert().clone();
        let signer = self.workspaces.signer();
        let reader = standing::Reader {
            person: cert.user_public_key,
            device: cert.device_public_key,
            signer: signer.as_ref(),
        };
        let mut readings = BTreeMap::new();
        let mut listed = Vec::new();
        for (workspace, name, peer) in self.workspaces.held() {
            let holder = standing::keeping_device_elsewhere(&peer, &cert.device_public_key)
                .map_or(cert.device_public_key, |(device, _)| device);
            let reading = standing::reading(workspace, &peer, &reader, Some(holder), Vec::new());
            listed.push(Listed {
                id: workspace.to_string(),
                name,
                labels: Vec::new(),
                own: reading.owner.as_deref() == Some(hex::encode(cert.user_public_key).as_str()),
                kept_here: holder == cert.device_public_key,
            });
            match serde_json::to_vec(&reading) {
                Ok(bytes) => {
                    readings.insert(resource(&workspace.to_string()), bytes);
                }
                Err(error) => {
                    tracing::error!(%workspace, %error, "a workspace's standing does not encode");
                }
            }
        }
        match serde_json::to_vec(&listed) {
            Ok(bytes) => {
                readings.insert(LIST.to_owned(), bytes);
            }
            Err(error) => tracing::error!(%error, "the workspace list does not encode"),
        }
        readings
    }
}

impl PublishedResources for Standing {
    fn snapshot(
        &self,
        _caller: &tilo_workspace_service::apps::services::Caller,
        resource: &str,
    ) -> oneshot::Receiver<Result<Reading, String>> {
        let (answer, answered) = oneshot::channel();
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = answer.send(Ok((state.revision, state.readings.get(resource).cloned())));
        answered
    }

    fn watch(
        &self,
        _caller: &tilo_workspace_service::apps::services::Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> Result<ResourceWatch, String> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let id = state.next_watch;
        state.next_watch = id.wrapping_add(1);
        let _ = sink.send(Event::Reading {
            resource: resource.to_owned(),
            revision: state.revision,
            reading: state.readings.get(resource).cloned(),
        });
        state.watches.insert(id, (resource.to_owned(), sink));
        drop(state);
        Ok(ResourceWatch::new(id))
    }

    fn unwatch(&self, watch: ResourceWatch) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .watches
            .remove(&watch.id());
    }

    fn invoke(
        &self,
        _caller: &tilo_workspace_service::apps::services::Caller,
        resource: &str,
        action: &str,
        _arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt> {
        let (answer, answered) = oneshot::channel();
        let _ = answer.send(Receipt::Refused {
            code: "unknown_action".to_owned(),
            message: format!(
                "{resource} is only read; {action} is one of the daemon's account actions"
            ),
        });
        answered
    }
}
