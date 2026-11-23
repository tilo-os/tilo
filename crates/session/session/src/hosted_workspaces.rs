//! The hosts of the workspaces this device takes part in but does not host.
//!
//! A workspace placed on a host (a home server, a cloud host) has its records
//! changed there: the host decides the person's actions on them with the
//! workspace's policy and runs them in its own workers, and the change reaches
//! this desktop's windows by sync. The broker asks here whether a workspace
//! is placed so, and sends the person's invocations to the host's actions.
//! This desktop reaches each host as one of the person's devices, on a
//! session opened with its own certificate and key, the way the person's
//! other devices enter a workspace on its host; the session is kept and
//! reused for every workspace on that host, and opened again once it ended.
//! It is a session beside the one this device's engine keeps with the same
//! host: it runs on this session's own endpoint and holds no keys, since the
//! device is one endpoint and one member of each workspace's key group.
//!
//! A workspace this device hosts itself, or one founded on its own shell
//! (whose host is a desktop, which runs no workers), is decided here.
//!
//! The same sessions carry the person's workspace list as each host keeps it
//! ([`HostedWorkspaces::follow_lists`]): the workspaces of theirs a host holds,
//! made in a browser, by the CLI or on another computer, which a device of
//! theirs restored from their identity alone learns of nowhere else. Each one
//! this device holds no copy of is handed to the session with its host's
//! signed address, and the session enters it there as their further device.
//! A workspace the host governs that its list stops naming (the person left it,
//! or was taken out of it) is handed to the session to let go of, the way it
//! lets go of every workspace gone from the person's list.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, pending,
};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_server_session::workspaces::WorkspaceLabels;
use tilo_session_client::device::{Device, DeviceActions};
use tilo_session_client::{Cmd, Outbox as _};
use tilo_session_client::{PeerConfig, PersonalWorkspaces};
use tilo_workspace_service::device_tickets::PersonHost;

use crate::engine::Engine;

/// The hosts this device reaches its hosted workspaces' actions on.
#[derive(Clone)]
pub(crate) struct HostedWorkspaces(Arc<Inner>);

struct Inner {
    engine: Arc<Engine>,
    /// Where the endpoint this device runs under its key lands once bound: every host is dialled
    /// from it. `None` for a session that binds none.
    endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
    /// The approver sessions this device keeps, one per host: each host of a workspace here puts
    /// the person's questions to them on one, beside the peer network's hosts. `None` for a session
    /// that answers none.
    answering: Option<crate::network::Answering>,
    runtime: tokio::runtime::Handle,
    /// Each host's session, by the host's device key.
    hosts: Mutex<HashMap<[u8; 32], Arc<ServerAddress>>>,
    /// What serves what another device addresses to this device through a
    /// host of its, once the session's broker can answer it
    /// ([`crate::device_inbox`]).
    inbound: Mutex<Option<Arc<dyn ActionProvider + Send + Sync>>>,
    /// The person's workspaces the hosts listed that this device holds no copy of, by
    /// workspace, until the session takes them ([`HostedWorkspaces::take_listed`]).
    listed: Mutex<BTreeMap<WorkspaceId, Listed>>,
    /// The workspaces held here that their host's list stopped naming, until the session takes
    /// them ([`HostedWorkspaces::take_dropped`]).
    dropped: Mutex<BTreeSet<WorkspaceId>>,
    /// The workspaces this device held as the session started: what a host's first list is
    /// measured against, since the person was in each of them before it was sent.
    held_at_start: BTreeSet<WorkspaceId>,
}

/// One of the person's workspaces a host of theirs lists that this device holds no copy of.
pub(crate) struct Listed {
    /// Its name, as the host lists it.
    pub(crate) name: String,
    /// Where its host is reached for it, as the host signed it: where this device asks to be taken
    /// in.
    pub(crate) host: tilo_peer_state::join::TransportHint,
}

/// This device's session with one host, and the workspaces it entered there.
struct ServerAddress {
    device: Device,
    /// The host's device key.
    key: [u8; 32],
    /// The workspaces the host's newest list named; `None` until it answered one.
    named: Mutex<Option<BTreeSet<WorkspaceId>>>,
    /// The person's workspace list as the host sends it on this session.
    personal: PersonalWorkspaces,
    /// Whether the person's list is followed on this session yet.
    followed: AtomicBool,
    /// Each entered workspace's actions on the host, by its id there.
    entered: Mutex<HashMap<WorkspaceId, DeviceActions>>,
    /// Held while a workspace is entered, so two invocations enter it once.
    entering: tokio::sync::Mutex<()>,
}

impl ServerAddress {
    /// Serve what another device addresses to this device through this host, on
    /// this device's own session with it, and answer on that same session.
    fn serve_inbound(&self, actions: &Arc<dyn ActionProvider + Send + Sync>) {
        let session = self.device.session();
        let outbox = Arc::clone(session.outbox());
        session.serve_inbound(Arc::new(crate::device_inbox::Inbox::new(
            Arc::clone(actions),
            Arc::new(move |workspace, to, frame| {
                let _ = outbox.send(Cmd::RemoteAction(workspace, to, frame));
            }),
        )));
    }
}

/// A hosted workspace's host, as a Terminal window's shell is opened there: its actions, where
/// its terminal streams are dialled, and this device's endpoint they are dialled from (`None`
/// until it bound).
pub(crate) struct TerminalHost {
    pub(crate) actions: Arc<dyn ActionProvider + Send + Sync>,
    pub(crate) address: iroh::EndpointAddr,
    pub(crate) endpoint: Option<iroh::Endpoint>,
    pub(crate) runtime: tokio::runtime::Handle,
}

/// Where a workspace is placed: its host, as its signed hint names it.
struct Placed {
    workspace: String,
    workspace_id: WorkspaceId,
    host: WorkspaceDeviceHintV1,
}

impl HostedWorkspaces {
    /// The hosts of `engine`'s workspaces, reached from this device's `endpoint` on `runtime`, each
    /// one's questions for the person answered on `answering` ; `None` for a session that holds no
    /// workspaces. The session's own runtime reaches them: this
    /// is built where the session starts, which is no task of any runtime's.
    ///
    /// `endpoint` is where this session's own endpoint lands once it bound, and `None` for a
    /// session that binds none: this device's key names one endpoint, and one bound beside it for
    /// a host's session took this device's place at its server, so the person's other devices
    /// reached nothing of it there once they started, or came back, after it.
    pub(crate) fn service(
        engine: Option<&Arc<Engine>>,
        endpoint: Option<Arc<std::sync::OnceLock<iroh::Endpoint>>>,
        answering: Option<crate::network::Answering>,
        runtime: tokio::runtime::Handle,
    ) -> Option<Self> {
        let engine = Arc::clone(engine?);
        Some(Self(Arc::new(Inner {
            held_at_start: engine.workspaces().into_iter().collect(),
            engine,
            endpoint,
            answering,
            runtime,
            hosts: Mutex::default(),
            inbound: Mutex::default(),
            listed: Mutex::default(),
            dropped: Mutex::default(),
        })))
    }

    /// Follow the person's workspace list on every host of a workspace this device holds: what
    /// the host lists now, and each list it sends as it changes. A workspace of theirs this device
    /// holds no copy of is kept for [`Self::take_listed`], with the host's signed address for it,
    /// and `wake` runs the session's pass to take it. A host whose session ended is followed again
    /// on the next.
    ///
    /// Each such host's questions for the person are answered here from then on, on the
    /// endpoint it is reached from: the host asks the person's devices before it runs an action
    /// their access leaves to them, such as opening a Terminal there.
    pub(crate) fn follow_lists(&self, wake: &tilo_service::Wake) {
        for OnHosts { workspace, host } in hosts_of(&self.0.engine) {
            self.answer(&host);
            let placed = Placed {
                workspace,
                workspace_id: host.workspace_id,
                host,
            };
            let host = self.host(&placed);
            if host.followed.swap(true, Ordering::Relaxed) {
                continue;
            }
            let reading = Reading {
                hosted: Arc::downgrade(&self.0),
                host: Arc::downgrade(&host),
                on: (placed.workspace.clone(), placed.workspace_id),
                wake: Arc::clone(wake),
            };
            let again = reading.clone();
            host.device
                .session()
                .on_event(host.personal.sent(), Arc::new(move || again.read()));
            reading.read();
        }
    }

    /// Answer the person's questions `host` puts to this device, once this device's endpoint is
    /// bound: on the one approver session per host the peer network's hosts are answered on too.
    fn answer(&self, host: &WorkspaceDeviceHintV1) {
        let (Some(answering), Some(endpoint)) = (
            self.0.answering.as_ref(),
            self.0.endpoint.as_ref().and_then(|endpoint| endpoint.get()),
        ) else {
            return;
        };
        let _inside = self.0.runtime.enter();
        answering.answer(
            endpoint,
            host.endpoint_addr.clone(),
            self.0.engine.identity(),
        );
    }

    /// The person's workspaces their hosts listed since the last call that this device holds no
    /// copy of, by workspace.
    pub(crate) fn take_listed(&self) -> BTreeMap<WorkspaceId, Listed> {
        std::mem::take(&mut *self.0.listed.lock_recover())
    }

    /// The workspaces held here that their host's list stopped naming since the last call: the
    /// person is no longer in them, so this device lets them go.
    pub(crate) fn take_dropped(&self) -> BTreeSet<WorkspaceId> {
        std::mem::take(&mut *self.0.dropped.lock_recover())
    }

    /// Every workspace the hosts of what this device holds list for the person, once each of them
    /// answered a list; `None` while one has not, when what the person has is not known yet.
    pub(crate) fn listed_everywhere(&self) -> Option<BTreeSet<WorkspaceId>> {
        let hosts = self.0.hosts.lock_recover();
        let mut named = BTreeSet::new();
        for on in hosts_of(&self.0.engine) {
            let host = hosts.get(&on.host.device_certificate.device_public_key)?;
            named.extend(host.named.lock_recover().clone()?);
        }
        drop(hosts);
        Some(named)
    }

    /// Serve what another device addresses to this device through a host of its:
    /// a listing of a window's resources, a page action on it. Installed once the
    /// session's broker can answer, on every session this device holds with a host
    /// and on each it opens from now on.
    pub(crate) fn serve_inbound(&self, actions: Arc<dyn ActionProvider + Send + Sync>) {
        for host in self.0.hosts.lock_recover().values() {
            host.serve_inbound(&actions);
        }
        *self.0.inbound.lock_recover() = Some(actions);
    }

    /// Where `workspace` is placed, when a holder other than this device takes its work: any
    /// holder that does, the workspace's holders being equal.
    fn placed(&self, workspace: &str) -> Option<Placed> {
        let id = WorkspaceId::parse(workspace)?;
        let hints = self
            .0
            .engine
            .owner()
            .with_current(id, |view| Ok(view.keeping_devices()))
            .ok()?;
        let own = self.0.engine.identity().device_cert.device_public_key;
        hints.iter().find_map(|hint| {
            let host = placed_on_host(hint, own)?;
            Some(Placed {
                workspace: workspace.to_owned(),
                workspace_id: host.workspace_id,
                host,
            })
        })
    }

    /// This device's session with `placed`'s host: the one kept, or a new one once it ended.
    fn host(&self, placed: &Placed) -> Arc<ServerAddress> {
        // The session a host is reached on is a task of this session's runtime, and a caller may
        // be a thread of the broker's: it opens here, inside the runtime that reaches hosts.
        let _inside = self.0.runtime.enter();
        let key = placed.host.device_certificate.device_public_key;
        let mut hosts = self.0.hosts.lock_recover();
        if let Some(host) = hosts.get(&key).filter(|host| host.device.is_live()) {
            return Arc::clone(host);
        }
        let identity = self.0.engine.identity();
        // It reads and asks with this device's own keys, and dials from this device's own
        // endpoint: the device holds one place in each workspace's key group, and one connection
        // per server, which a second key service or endpoint of its own would take.
        let device = Device::over_lent(
            PeerConfig {
                host: tilo_server_client::ServerAddress::at(placed.host.endpoint_addr.clone()),
                cert: identity.device_cert.clone(),
                server_cert: placed.host.device_certificate.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering: None,
            },
            Arc::clone(&identity.signer),
            &identity.device_cert.device_label,
            tilo_session_client::Lent {
                keys: self.0.engine.keys().get(),
                endpoint: self.0.endpoint.clone(),
                own: false,
            },
        );
        let host = Arc::new(ServerAddress {
            key,
            named: Mutex::new(None),
            personal: PersonalWorkspaces::follow(device.session()),
            followed: AtomicBool::new(false),
            device,
            entered: Mutex::default(),
            entering: tokio::sync::Mutex::new(()),
        });
        let installed = self.0.inbound.lock_recover().clone();
        if let Some(actions) = installed {
            host.serve_inbound(&actions);
        }
        let ended = hosts.insert(key, Arc::clone(&host));
        drop(hosts);
        if let Some(ended) = ended {
            ended.device.close();
        }
        host
    }

    /// `workspace`'s actions on its host, when a host other than this device runs its records.
    fn actions_of(&self, workspace: &str) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        let placed = self.placed(workspace)?;
        let host = self.host(&placed);
        Some(Arc::new(OnHost {
            host,
            workspace: placed.workspace,
            workspace_id: placed.workspace_id,
            runtime: self.0.runtime.clone(),
        }))
    }

    /// What a Terminal window's shell in `workspace` opens through when a host other than this
    /// device runs the workspace's work ([`crate::workspace_terminals`]); `None` when none does.
    pub(crate) fn terminal_host(&self, workspace: &str) -> Option<TerminalHost> {
        let placed = self.placed(workspace)?;
        let host = self.host(&placed);
        Some(TerminalHost {
            address: placed.host.endpoint_addr.clone(),
            endpoint: self
                .0
                .endpoint
                .as_ref()
                .and_then(|endpoint| endpoint.get().cloned()),
            runtime: self.0.runtime.clone(),
            actions: Arc::new(OnHost {
                host,
                workspace: placed.workspace,
                workspace_id: placed.workspace_id,
                runtime: self.0.runtime.clone(),
            }),
        })
    }

    /// The person's hosts as this device reaches them, each once, in one of the workspaces of
    /// theirs it runs: where their list of devices is read, named in and told of a revocation.
    pub(crate) fn person_hosts(&self) -> Vec<Arc<dyn PersonHost>> {
        hosts_of(&self.0.engine)
            .into_iter()
            .filter_map(|on| {
                let placed = self.placed(&on.workspace)?;
                let host = self.host(&placed);
                Some(Arc::new(OnHost {
                    host,
                    workspace: placed.workspace,
                    workspace_id: placed.workspace_id,
                    runtime: self.0.runtime.clone(),
                }) as Arc<dyn PersonHost>)
            })
            .collect()
    }
}

/// One read of the person's list on one host's session: what the host lists now, each workspace
/// of theirs this device holds no copy of kept with the host's signed address for it.
#[derive(Clone)]
struct Reading {
    hosted: Weak<Inner>,
    host: Weak<ServerAddress>,
    /// A workspace this device holds on the host, by its id and its id there: where the host is
    /// asked, as everything of the person's is asked there.
    on: (String, WorkspaceId),
    wake: tilo_service::Wake,
}

impl Reading {
    /// Read the list now, off the caller's thread.
    fn read(&self) {
        let Some(hosted) = self.hosted.upgrade() else {
            return;
        };
        let reading = self.clone();
        hosted.runtime.spawn(async move {
            let (Some(hosted), Some(host)) = (reading.hosted.upgrade(), reading.host.upgrade())
            else {
                return;
            };
            if read_list(&hosted, &host, &reading.on).await {
                (reading.wake)();
            }
        });
    }
}

/// Keep each workspace `host` lists for the person that `hosted`'s device holds no copy of, with
/// the host's signed address for it: one a host keeps itself is asked for alone, which is when the
/// host signs where it answers for it. A workspace held here on `host` that its list named before
/// (or that was held as the session started) and names no longer is kept for the session to let
/// go of. Whether anything moved: something kept, something dropped, or the host's first answer.
async fn read_list(hosted: &Inner, host: &ServerAddress, on: &(String, WorkspaceId)) -> bool {
    let entered = match enter(host, &on.0, on.1).await {
        Ok(_) => on.1,
        Err(error) => {
            tracing::info!(host = host.device.label(), %error, "a host's list of the person's workspaces was not read");
            return false;
        }
    };
    let list = match host.personal.read_in(&host.device, entered).await {
        Ok(list) => list,
        Err(refusal) => {
            tracing::info!(host = host.device.label(), %refusal, "a host's list of the person's workspaces was not read");
            return false;
        }
    };
    let held: BTreeSet<WorkspaceId> = hosted.engine.workspaces().into_iter().collect();
    let named: BTreeSet<WorkspaceId> = list
        .workspaces
        .iter()
        .filter_map(|entry| WorkspaceId::parse(&entry.workspace))
        .collect();
    let before = host.named.lock_recover().replace(named.clone());
    let mut kept = before.is_none();
    // The host governs what it places here: a workspace it named and names no longer is one the
    // person is not in now. A list that never named one says nothing of it: the person may have
    // come into it after the list was sent.
    let own = hosted.engine.identity().device_cert.device_public_key;
    let gone: Vec<WorkspaceId> = hosted
        .engine
        .host_hints()
        .into_iter()
        .filter_map(|(workspace, hint)| {
            let hint = postcard::from_bytes::<tilo_peer_state::join::TransportHint>(&hint).ok()?;
            let placed = placed_on_host(&hint, own)?;
            let workspace = WorkspaceId::parse(&workspace)?;
            (placed.device_certificate.device_public_key == host.key
                && held.contains(&workspace)
                && !named.contains(&workspace)
                && before
                    .as_ref()
                    .unwrap_or(&hosted.held_at_start)
                    .contains(&workspace))
            .then_some(workspace)
        })
        .collect();
    if !gone.is_empty() {
        tracing::info!(
            host = host.device.label(),
            ?gone,
            "a host no longer lists workspaces of the person's held here"
        );
        hosted.dropped.lock_recover().extend(gone);
        kept = true;
    }
    for entry in list.workspaces {
        let Some(workspace) = WorkspaceId::parse(&entry.workspace) else {
            continue;
        };
        // A workspace no device of the person's said the host of yet waits until one does.
        let Some(address) = entry.host else {
            continue;
        };
        if held.contains(&workspace) {
            continue;
        }
        // A hint the list carries is the device that keeps it; else the host that answered keeps
        // it, and signs where it does when asked for that workspace alone.
        let signed = match address.hint {
            Some(hint) => Some(hint),
            None => host
                .device
                .personal_list_in(entered, &WorkspaceLabels::new(), Some(&entry.workspace))
                .await
                .ok()
                .and_then(|one| one.workspaces.into_iter().next())
                .and_then(|one| one.enter_at),
        };
        let Some(hint) = signed.and_then(|hint| signed_hint(&hint, workspace)) else {
            tracing::debug!(%workspace, "a listed workspace's host signed no address for it");
            continue;
        };
        hosted.listed.lock_recover().insert(
            workspace,
            Listed {
                name: entry.name,
                host: hint,
            },
        );
        kept = true;
    }
    kept
}

/// The signed address `hint` (postcard in hex) is for `workspace`, when it is one: signed by the
/// host it names, for that workspace.
fn signed_hint(hint: &str, workspace: WorkspaceId) -> Option<tilo_peer_state::join::TransportHint> {
    let bytes = hex::decode(hint).ok()?;
    let hint: tilo_peer_state::join::TransportHint = postcard::from_bytes(&bytes).ok()?;
    let (host, _) = WorkspaceDeviceHintV1::authenticate_transport_hint(&hint).ok()?;
    (host.workspace_id == workspace).then_some(hint)
}

/// The device `hint` places a workspace on, when that is another device than `own` and it runs
/// the workspace's work in its own workers. A workspace a desktop serves runs its records'
/// actions where its windows are.
fn placed_on_host(
    hint: &tilo_peer_state::join::TransportHint,
    own: [u8; 32],
) -> Option<WorkspaceDeviceHintV1> {
    let (device, _) = WorkspaceDeviceHintV1::authenticate_transport_hint(hint).ok()?;
    (device.runs_work && device.device_certificate.device_public_key != own).then_some(device)
}

/// One of the person's workspaces on a host that runs its records: the workspace as the host knows
/// it, and where it is reached.
struct OnHosts {
    /// A workspace of theirs this device is in there, which is where its host is asked anything
    /// about the person.
    workspace: String,
    host: WorkspaceDeviceHintV1,
}

/// Each host of a workspace `engine` holds, with one of its workspaces, in the order the workspaces
/// came: one ask per host reaches it once, however many workspaces it runs.
fn hosts_of(engine: &Engine) -> Vec<OnHosts> {
    let own = engine.identity().device_cert.device_public_key;
    let mut seen = std::collections::BTreeSet::new();
    let mut hosts = Vec::new();
    for (workspace, hint) in engine.host_hints() {
        let Ok(hint) = postcard::from_bytes::<tilo_peer_state::join::TransportHint>(&hint) else {
            continue;
        };
        let Some(host) = placed_on_host(&hint, own) else {
            continue;
        };
        if !seen.insert(host.device_certificate.device_public_key) {
            continue;
        }
        hosts.push(OnHosts { workspace, host });
    }
    hosts
}

/// The request that asks a host the person's record `step` (`workspace.personal.keep` with the
/// list, `workspace.personal` alone).
fn record_request(
    step: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_server_session::workspaces::HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_server_session::workspaces::HOST_WORKSPACES_RESOURCE,
                step,
            ),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: Some(20_000),
        appearance: None,
        signed: None,
    }
}

/// Keep `list`, the person's workspace list, on every dedicated host of a workspace `engine` holds,
/// through the sessions `hosted` keeps with them: what the person's keys that run on no device of
/// theirs read it back from. One ask per host, in one of its workspaces this device is in; a host
/// that does not answer is told the next time the list moves.
pub(crate) fn keep_on_hosts(
    hosted: &dyn tilo_workspace_service::apps::services::HostedWorkspaces,
    engine: &Engine,
    list: &tilo_server_session::workspaces::PersonalList,
) {
    let Ok(value) = serde_json::to_value(list) else {
        return;
    };
    for OnHosts { workspace, host } in hosts_of(engine) {
        let Some(actions) = hosted.host_actions(&workspace) else {
            continue;
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert("list".to_owned(), value.clone());
        let request = record_request(tilo_server_session::workspaces::KEEP_PERSONAL, arguments);
        let label = host.device_certificate.device_label.clone();
        actions.invoke(&Caller::local_person("tilo"), request).then(
            move |answered| match answered {
                Some(Ok(receipt))
                    if matches!(
                        receipt.status,
                        tilo_actions::remote::ReceiptStatus::Completed { .. }
                    ) =>
                {
                    tracing::debug!(host = %label, "the person's workspace list is kept on a host");
                }
                other => tracing::info!(
                    host = %label,
                    answer = ?other.map(|answer| answer.map(|receipt| receipt.status)),
                    "a host did not keep the person's workspace list"
                ),
            },
        );
    }
}

impl tilo_workspace_service::apps::services::HostedWorkspaces for HostedWorkspaces {
    fn host_actions(&self, workspace: &str) -> Option<Arc<dyn ActionProvider + Send + Sync>> {
        self.actions_of(workspace)
    }
}

/// One workspace's actions on its host, entering it there on first use.
struct OnHost {
    host: Arc<ServerAddress>,
    workspace: String,
    workspace_id: WorkspaceId,
    runtime: tokio::runtime::Handle,
}

impl OnHost {
    /// The workspace's actions on the host, once this device entered it there.
    fn entered(&self) -> Option<DeviceActions> {
        self.host
            .entered
            .lock_recover()
            .get(&self.workspace_id)
            .cloned()
    }

    /// Ask `ask` of the workspace's actions on the host once this device is in it there.
    fn asking<T: Send + 'static>(
        &self,
        ask: impl FnOnce(DeviceActions) -> Pending<Result<T, ActionCallError>> + Send + 'static,
    ) -> Pending<Result<T, ActionCallError>> {
        let (answer, answered) = pending();
        let host = Arc::clone(&self.host);
        let workspace = self.workspace.clone();
        let workspace_id = self.workspace_id;
        self.runtime.spawn(async move {
            let actions = match enter(&host, &workspace, workspace_id).await {
                Ok(actions) => actions,
                Err(error) => {
                    answer.complete(Err(error));
                    return;
                }
            };
            answer.complete(ask(actions).await.unwrap_or_else(|| {
                Err(ActionCallError::Unavailable {
                    reason: "the workspace's host did not answer".into(),
                })
            }));
        });
        answered
    }
}

/// `host`'s actions of `workspace`, entering it there first when this device is not in it yet.
async fn enter(
    host: &ServerAddress,
    workspace: &str,
    workspace_id: WorkspaceId,
) -> Result<DeviceActions, ActionCallError> {
    let _entering = host.entering.lock().await;
    if let Some(actions) = host.entered.lock_recover().get(&workspace_id) {
        return Ok(actions.clone());
    }
    let entered =
        host.device
            .join(workspace)
            .await
            .map_err(|error| ActionCallError::Unavailable {
                reason: format!("the workspace's host did not let this device in: {error}"),
            })?;
    let actions = host.device.workspace_actions(entered);
    host.entered
        .lock_recover()
        .insert(workspace_id, actions.clone());
    Ok(actions)
}

/// A host as the person's devices are managed through it: its own actions asked in the workspace
/// this device is in there, and a revocation told on this device's session with it.
impl PersonHost for OnHost {
    fn ask(
        &self,
        action: &str,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> futures_util::future::BoxFuture<'_, Result<serde_json::Value, String>> {
        let asked = self.invoke(
            &Caller::local_person("tilo"),
            record_request(action, arguments),
        );
        Box::pin(async move {
            match asked.await {
                Some(Ok(receipt)) => match receipt.status {
                    tilo_actions::remote::ReceiptStatus::Completed { output } => {
                        Ok(output.value.unwrap_or(serde_json::Value::Null))
                    }
                    tilo_actions::remote::ReceiptStatus::Failed { error } => Err(error.message),
                    tilo_actions::remote::ReceiptStatus::Rejected { error } => {
                        Err(error.to_string())
                    }
                    _ => Err("the host did not finish in time".to_owned()),
                },
                Some(Err(error)) => Err(error.to_string()),
                None => Err("the host did not answer".to_owned()),
            }
        })
    }

    fn revoked(&self, entry: &tilo_user_identity::RevocationEntry) -> Result<(), String> {
        self.host
            .device
            .session()
            .send(Cmd::Revoke(entry.clone()))
            .map_err(|_| "the session with the host ended".to_owned())
    }

    fn label(&self) -> String {
        self.host.device.label().to_owned()
    }
}

impl ActionProvider for OnHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.query(&caller, query))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.invoke(&caller, request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.entered()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.entered()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        match self.entered() {
            Some(actions) => actions.cancel(caller, id),
            None => Pending::ready(Err(ActionCallError::UnknownInvocation)),
        }
    }

    /// The person's answer to `request`, asked of the workspace's host: the
    /// host is the one that reaches their approving devices, so a request a
    /// page needs their word for is put to them there and answered here with
    /// the confirmation it runs with.
    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        let caller = caller.clone();
        let request = request.clone();
        self.asking(move |actions| actions.confirm(&caller, &request))
    }
}

#[cfg(test)]
mod tests {

    use super::placed_on_host;
    use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    /// A hint placing a workspace on the device `capabilities` certify, which runs the
    /// workspace's work or not, and that device's key.
    fn hint(
        runs_work: bool,
        capabilities: DeviceCapabilities,
    ) -> (tilo_peer_state::join::TransportHint, [u8; 32]) {
        let root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "host".into(),
            capabilities,
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned();
        let hint = WorkspaceDeviceHintV1 {
            version: 1,
            holds_keys: true,
            runs_work,
            label: String::new(),
            cloud: false,
            origin: None,
            workspace_id: tilo_peer_state::join::WorkspaceId::parse(&workspace).unwrap(),
            endpoint_addr: iroh::EndpointAddr::new(
                iroh::EndpointId::from_bytes(&cert.device_public_key).unwrap(),
            ),
            user_public_key: cert.user_public_key,
            device_certificate: cert.clone(),
            packet_relay_url: "iroh:direct".into(),
        }
        .transport_hint(&key)
        .unwrap();
        (hint, cert.device_public_key)
    }

    #[test]
    fn only_another_device_that_takes_the_work_runs_the_records() {
        let (taker, device) = hint(true, DeviceCapabilities::member());
        assert!(
            placed_on_host(&taker, [7; 32]).is_some(),
            "a device that takes the work runs the workspace's records"
        );
        assert!(
            placed_on_host(&taker, device).is_none(),
            "a device runs the records of the workspaces it serves itself"
        );
        let (shell, _) = hint(false, DeviceCapabilities::primary());
        assert!(
            placed_on_host(&shell, [7; 32]).is_none(),
            "a device that takes no work has its records run where its windows are"
        );
    }
}
