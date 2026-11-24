//! The person's shell, as this device's daemon answers it on its dispatch
//! (`tilo_workspace_service::apps::shell`): the desktop session of the person signed in on this
//! computer, which reaches their workspaces through the daemon and never on its own.
//!
//! - Who a shell is: the process the daemon started as the shell, holding the link it made for it
//!   (`Daemon::start_shell`); no other process, whatever account it runs as.
//! - Its documents: the daemon's own reads and writes of the workspaces it holds, each write
//!   sealed with the keys of the document's group this device reads and signed as the device.
//! - Its calls: every action it invokes reaches this device's one dispatch, as the person
//!   ([`PersonDispatch`]), the dispatch the person's own runs reach. Founding a workspace here and
//!   joining one by an invitation are the person's workspace list's actions; a change the person
//!   makes to a workspace's people and settings is written as this device's own, into the
//!   workspace this device keeps or its member copy; any other key access and membership action is
//!   answered where this device keeps the workspace, and asked of the device keeping it over
//!   tilo-actions/1 otherwise; the host's own operations are this device's workspace service's
//!   ([`Kept`]).
//! - How its workspaces stand: [`Standing`], read again whenever a workspace changes.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use runtime_collab::wire::{Event, Receipt};
use serde_json::{Map, Value};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::workspace_view::{CREATE, JOIN, LIST, Listed, MEMBERSHIP, resource};
use tilo_workspace_service::apps::services::{PublishedResources, Reading, ResourceWatch};
use tilo_workspace_service::apps::shell::{
    ShellActions, ShellCaller, ShellDispatch, ShellLinks, ShellServices,
};
use tilo_workspace_service::device_documents::{ServiceDocuments, ServiceWrites};
use tilo_workspace_service::workspaces::standing;
use tokio::sync::{Notify, mpsc, oneshot};

use super::services::PersonDispatch;
use super::workspaces::ServiceWorkspaces;

/// What this device's dispatch answers its person's shell with, read again as `workspaces`
/// changes. Its calls reach `dispatching` once the daemon serves it.
pub(crate) fn services(
    workspaces: &Arc<ServiceWorkspaces>,
    links: &Arc<ShellLinks>,
    socket: std::path::PathBuf,
    dispatching: Arc<Dispatching>,
    published: Arc<dyn PublishedResources>,
    processes: Option<tilo_workspace_service::apps::shell::ShellProcesses>,
    identity: Arc<dyn Fn(tilo_socket::Stream) + Send + Sync>,
) -> ShellServices {
    let standing = Standing::new(Arc::clone(workspaces));
    standing.follow();
    let copies = workspaces.copies().clone();
    ShellServices {
        links: Arc::clone(links),
        socket,
        processes,
        documents: Arc::new(ServiceDocuments::of(copies.clone())),
        writes: Arc::new(ServiceWrites::of(copies.clone())),
        document_writes: Arc::new({
            let copies = copies.clone();
            move || copies.document_writes()
        }),
        workspaces: copies,
        actions: Arc::new(Actions {
            kept: Arc::new(Kept {
                workspaces: Arc::clone(workspaces),
                changed: Arc::clone(&standing.changed),
            }),
            dispatching,
            by_workspace: Mutex::default(),
        }),
        standing,
        published: Some(published),
        identity: Some(identity),
    }
}

/// The one dispatch the person's calls reach, once the daemon's broker serves it.
#[derive(Default)]
pub(crate) struct Dispatching(OnceLock<PersonDispatch>);

impl Dispatching {
    /// The person's calls reach `person` from now on.
    pub(crate) fn serve(&self, person: PersonDispatch) {
        let _ = self.0.set(person);
    }
}

/// What a shell's calls reach.
struct Actions {
    kept: Arc<Kept>,
    dispatching: Arc<Dispatching>,
    /// The dispatch of each workspace a shell called in, admitted once.
    by_workspace: Mutex<HashMap<String, Arc<dyn ActionProvider + Send + Sync>>>,
}

impl ShellActions for Actions {
    fn dispatch(
        &self,
        caller: &ShellCaller,
        workspace: Option<String>,
        pressed: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> ShellDispatch {
        let kept = Arc::clone(&self.kept);
        let shell = *caller;
        let Some(workspace) = workspace else {
            let cert = kept.workspaces.device_cert();
            let person = Caller::authenticated(cert.user_public_key, cert.device_public_key);
            return Box::pin(std::future::ready(Ok((
                Arc::new(Shell {
                    kept,
                    shell,
                    workspace: None,
                }) as Arc<dyn ActionProvider + Send + Sync>,
                person,
            ))));
        };
        let Some(person) = self.dispatching.0.get().cloned() else {
            return Box::pin(std::future::ready(Err(
                "this device's dispatch is not up yet".to_owned(),
            )));
        };
        if shell.person != kept.workspaces.device_cert().user_public_key {
            return Box::pin(std::future::ready(Err(
                "this device acts for its own person only".to_owned(),
            )));
        }
        // A press names a window of its own, which the cached dispatch does not: only a call
        // nobody pressed shares the workspace's one.
        let keep = pressed.is_none();
        let cached = keep.then(|| {
            self.by_workspace
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&workspace)
                .cloned()
        });
        let caller = person.caller(&workspace);
        let admitted = match cached.flatten() {
            Some(dispatch) => Ok(dispatch),
            None => person
                .provider_pressed(&workspace, pressed)
                .map(|provider| Arc::new(provider) as Arc<dyn ActionProvider + Send + Sync>)
                .map_err(|error| error.to_string())
                .inspect(|dispatch| {
                    if keep {
                        self.by_workspace
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(workspace.clone(), Arc::clone(dispatch));
                    }
                }),
        };
        Box::pin(std::future::ready(admitted.map(|dispatch| {
            (
                Arc::new(Shell {
                    kept,
                    shell,
                    workspace: Some(dispatch),
                }) as Arc<dyn ActionProvider + Send + Sync>,
                caller,
            )
        })))
    }
}

/// A shell's calls: those of the person's workspace list, a workspace's key access and
/// membership and the host's own operations answered where they are decided ([`Kept`]), and every
/// other one by the person's dispatch in the workspace it names.
struct Shell {
    kept: Arc<Kept>,
    shell: ShellCaller,
    workspace: Option<Arc<dyn ActionProvider + Send + Sync>>,
}

impl Shell {
    fn in_workspace(&self) -> Result<&Arc<dyn ActionProvider + Send + Sync>, ActionCallError> {
        self.workspace.as_ref().ok_or(ActionCallError::Refused {
            reason: "the shell calls that in a workspace, and named none".into(),
        })
    }
}

/// Whether `request` is the person's workspace list's, a workspace's key access and membership,
/// or the host's own: what this device answers where it is decided ([`ask`]).
fn kept_here(request: &InvokeRequest) -> bool {
    request.target.provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
        || tilo_actions::remote::workspace_governance_operation(&request.target.address).is_some()
}

impl ActionProvider for Shell {
    /// The device's one search index: the person's search spans every workspace they see.
    fn resource_search(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceSearch>> {
        self.kept
            .workspaces
            .search()
            .map(|search| search as Arc<dyn tilo_actions::remote::ResourceSearch>)
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        match self.in_workspace() {
            Ok(dispatch) => dispatch.query(caller, query),
            Err(refused) => Pending::ready(Err(refused)),
        }
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if kept_here(&request) {
            return self.kept.invoke(self.shell, request);
        }
        match self.in_workspace() {
            Ok(dispatch) => dispatch.invoke(caller, request),
            Err(refused) => Pending::ready(Err(refused)),
        }
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.workspace.as_ref()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.workspace.as_ref()?.next_change(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        match self.in_workspace() {
            Ok(dispatch) => dispatch.cancel(caller, id),
            Err(refused) => Pending::ready(Err(refused)),
        }
    }
}

/// What this device answers of a shell's calls itself: its workspace service, and the devices
/// keeping the workspaces it holds as a member, over tilo-actions/1.
struct Kept {
    workspaces: Arc<ServiceWorkspaces>,
    /// Told once an ask settled, so the workspaces' standing is read again.
    changed: Arc<Notify>,
}

impl Kept {
    /// `request` asked for `shell`, answered once it settled.
    fn invoke(
        &self,
        shell: ShellCaller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let (done, receipt) = tilo_actions::remote::pending();
        let workspaces = Arc::clone(&self.workspaces);
        let changed = Arc::clone(&self.changed);
        let invocation = request.invocation;
        tilo_rt::app().spawn(async move {
            let answer = ask(&workspaces, shell, request).await;
            changed.notify_one();
            let status = match answer {
                Ok(value) => ReceiptStatus::Completed {
                    output: tilo_actions::ActionOutput {
                        value: Some(value),
                        ..tilo_actions::ActionOutput::default()
                    },
                },
                Err(message) => ReceiptStatus::Failed {
                    error: tilo_actions::ActionFailure {
                        code: "refused".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            };
            done.complete(Ok(InvocationReceipt { invocation, status }));
        });
        receipt
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
    let address = &request.target.address;
    if request.target.provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER {
        let arguments = named(request.argument)?;
        // The person's workspace list: founding one here, and joining one.
        if address.resource == LIST {
            return listed(workspaces, &address.action, &arguments).await;
        }
        // The host's own operations are this device's workspace service's.
        return workspaces
            .service()
            .ask_own(&cert, &address.action, arguments)
            .await;
    }
    let workspace = tilo_actions::remote::workspace_governance_operation(address)
        .map(|(workspace, _)| workspace.to_owned())
        .ok_or(
            "the shell asks a workspace's key access and membership or the host, and nothing else",
        )?;
    let id = WorkspaceId::parse(&workspace).ok_or("that is not a workspace")?;
    // A change to the workspace's people and settings is this device's own, signed with its key:
    // the workspace's admission decides it as any member's.
    if address.action == MEMBERSHIP {
        let command = named(request.argument)?
            .remove("command")
            .ok_or("a change to a workspace's people names its `command`")?;
        let command = serde_json::from_value(command)
            .map_err(|error| format!("the change does not read: {error}"))?;
        workspaces
            .copies()
            .commit_own_change(&workspace, command, None)
            .await?;
        return Ok(Value::Null);
    }
    // A workspace this device keeps answers here, as it answers every member's device.
    if workspaces.copies().administers_workspace(&workspace).await {
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
    actions.keyed_apart(device, *addr.id.as_bytes());
    let followed = Arc::new(actions.remote(&endpoint, &addr, &workspace))
        .invoke_until_settled(request, tilo_peer_state::actions::FOLLOW_DEADLINE)
        .await;
    match followed {
        Some(Ok(update)) => finished(update),
        Some(Err(error)) => Err(error.to_string()),
        None => Err("the workspace's keeping device dropped the request".to_owned()),
    }
}

/// The named arguments `argument` carries.
fn named(argument: InvokeArgument) -> Result<Map<String, Value>, String> {
    match argument {
        InvokeArgument::Json(arguments) => Ok(arguments),
        InvokeArgument::None => Ok(Map::new()),
        InvokeArgument::Text(_) => Err("the device's actions take named arguments".to_owned()),
    }
}

/// The person's workspace list's `action` with `arguments`: [`CREATE`] founds a workspace this
/// device keeps, [`JOIN`] joins the workspace an invitation is to. Answers the workspace's id.
async fn listed(
    workspaces: &ServiceWorkspaces,
    action: &str,
    arguments: &Map<String, Value>,
) -> Result<Value, String> {
    let text = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("`{action}` names its `{name}`"))
    };
    let workspace = match action {
        CREATE => {
            let workspace = WorkspaceId::parse(text("id")?).ok_or("that is not a workspace")?;
            workspaces.found(workspace, text("name")?).await?;
            workspace.to_string()
        }
        JOIN => workspaces
            .copies()
            .join_member(text("invite")?)
            .await?
            .to_string(),
        _ => return Err(format!("the person's workspaces have no action `{action}`")),
    };
    Ok(Value::String(workspace))
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

    /// Read again whenever a workspace changes here: its key access or membership changed, another
    /// device's write was taken in, this device took a workspace in, or a shell's ask settled.
    fn follow(self: &Arc<Self>) {
        let copies = self.workspaces.copies().clone();
        let tell = |changed: &Arc<Notify>| {
            let changed = Arc::clone(changed);
            move || changed.notify_one()
        };
        let mut decided = copies.governance_changes();
        let governed = tell(&self.changed);
        tilo_rt::app().spawn(async move {
            while !matches!(
                decided.recv().await,
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
                host: standing::host_elsewhere(&peer, &cert.device_public_key)
                    .and_then(|host| host.written()),
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

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
