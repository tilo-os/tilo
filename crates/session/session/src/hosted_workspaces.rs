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
//!
//! A workspace this device hosts itself, or one founded on its own desk
//! (whose host is a desktop, which runs no workers), is decided here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, pending,
};
use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
use tilo_desktop_model::WorkspaceId;
use tilo_lock::MutexRecover as _;
use tilo_relay_session::workspaces::PersonalList;
use tilo_session_client::PeerConfig;
use tilo_session_client::device::Device;
use tilo_session_client::{Cmd, Outbox as _};
use tilo_session_core::remote::SessionActions;

use crate::engine::Engine;

/// The hosts this device reaches its hosted workspaces' actions on.
#[derive(Clone)]
pub(crate) struct HostedWorkspaces(Arc<Inner>);

struct Inner {
    engine: Arc<Engine>,
    runtime: tokio::runtime::Handle,
    /// Each host's session, by the host's device key.
    hosts: Mutex<HashMap<[u8; 32], Arc<Host>>>,
}

/// This device's session with one host, and the workspaces it entered there.
struct Host {
    device: Device,
    /// Each entered workspace's actions on the host, by its id there.
    entered: Mutex<HashMap<u64, SessionActions>>,
    /// Held while a workspace is entered, so two invocations enter it once.
    entering: tokio::sync::Mutex<()>,
}

/// Where a workspace is placed: its host, as its signed hint names it.
struct Placed {
    workspace: String,
    workspace_id: u64,
    host: IrohWorkspaceHostHintV1,
}

impl HostedWorkspaces {
    /// The hosts of `engine`'s workspaces, reached on `runtime`; `None` for a session that holds
    /// no workspaces. The session's own runtime reaches them: this is built where the session
    /// starts, which is no task of any runtime's.
    pub(crate) fn service(
        engine: Option<&Arc<Engine>>,
        runtime: tokio::runtime::Handle,
    ) -> Option<Self> {
        Some(Self(Arc::new(Inner {
            engine: Arc::clone(engine?),
            runtime,
            hosts: Mutex::default(),
        })))
    }

    /// Where `workspace` is placed, when a host other than this device runs its records.
    fn placed(&self, workspace: &str) -> Option<Placed> {
        let id = WorkspaceId::parse(workspace)?;
        let hint = self
            .0
            .engine
            .owner()
            .with_current(id, |view| Ok(view.transport_hint()?.clone()))
            .ok()?;
        let own = self.0.engine.identity().device_cert.device_public_key;
        placed_on_host(&hint, own).map(|host| Placed {
            workspace: workspace.to_owned(),
            workspace_id: host.workspace_id,
            host,
        })
    }

    /// This device's session with `placed`'s host: the one kept, or a new one once it ended.
    fn host(&self, placed: &Placed) -> Arc<Host> {
        // The session a host is reached on is a task of this session's runtime, and a caller may
        // be a thread of the broker's: it opens here, inside the runtime that reaches hosts.
        let _inside = self.0.runtime.enter();
        let key = placed.host.host_device_certificate.device_public_key;
        let mut hosts = self.0.hosts.lock_recover();
        if let Some(host) = hosts.get(&key).filter(|host| host.device.is_live()) {
            return Arc::clone(host);
        }
        let identity = self.0.engine.identity();
        let device = Device::over(
            PeerConfig {
                host: tilo_relay_client::Host::at(placed.host.endpoint_addr.clone()),
                cert: identity.device_cert.clone(),
                host_cert: placed.host.host_device_certificate.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
                entering: None,
            },
            Arc::clone(&identity.signer),
            &identity.device_cert.device_label,
        );
        let host = Arc::new(Host {
            device,
            entered: Mutex::default(),
            entering: tokio::sync::Mutex::new(()),
        });
        let ended = hosts.insert(key, Arc::clone(&host));
        drop(hosts);
        if let Some(ended) = ended {
            ended.device.close();
        }
        host
    }

    /// `workspace`'s actions on its host, when a host other than this device runs its records.
    fn actions_of(&self, workspace: &str) -> Option<Arc<dyn ActionHost + Send + Sync>> {
        let placed = self.placed(workspace)?;
        let host = self.host(&placed);
        Some(Arc::new(OnHost {
            host,
            workspace: placed.workspace,
            workspace_id: placed.workspace_id,
            runtime: self.0.runtime.clone(),
        }))
    }

    /// The person's devices as the record on each host of a workspace `engine` holds lists them:
    /// every key `room.personal` answers there, once, each saying whether that host refuses it.
    /// A key their root certified on another of their devices — a browser in its own tab, a device
    /// paired there — is in that record and in no fleet of this one's, so this is what lists the
    /// person's devices and not the devices this machine happens to know.
    ///
    /// Empty when no workspace of theirs is placed on a host.
    pub(crate) fn record_devices(
        &self,
        engine: &Engine,
    ) -> Pending<Vec<(tilo_user_identity::DeviceCert, bool)>> {
        let asks: Vec<_> = hosts_of(engine)
            .into_iter()
            .filter_map(|on| {
                let actions = self.actions_of(&on.workspace)?;
                Some(actions.invoke(
                    &Caller::local_person("tilo"),
                    record_request(
                        tilo_relay_session::workspaces::LIST_PERSONAL,
                        serde_json::Map::new(),
                    ),
                ))
            })
            .collect();
        if asks.is_empty() {
            return Pending::ready(Vec::new());
        }
        let (answer, answered) = pending();
        self.0.runtime.spawn(async move {
            let mut devices: Vec<(tilo_user_identity::DeviceCert, bool)> = Vec::new();
            for ask in asks {
                let Some(Ok(receipt)) = ask.await else {
                    continue;
                };
                let tilo_actions::remote::ReceiptStatus::Completed { output } = receipt.status
                else {
                    continue;
                };
                let Ok(list) = serde_json::from_value::<PersonalList>(
                    output.value.unwrap_or(serde_json::Value::Null),
                ) else {
                    continue;
                };
                for device in list.devices {
                    match devices
                        .iter_mut()
                        .find(|(known, _)| known.device_public_key == device.cert.device_public_key)
                    {
                        Some((known, refused)) => {
                            *refused |= device.revoked;
                            if known.issued_at_ns < device.cert.issued_at_ns {
                                *known = device.cert;
                            }
                        }
                        None => devices.push((device.cert, device.revoked)),
                    }
                }
            }
            answer.complete(devices);
        });
        answered
    }

    /// Tell every host of a workspace `engine` holds that the person's root revoked `entry`: the
    /// host keeps it and refuses the key from then on, and answers it as refused in the person's
    /// record, which is how every device of theirs comes to list the key as taken back.
    pub(crate) fn announce_revocation(
        &self,
        engine: &Engine,
        entry: &tilo_user_identity::RevocationEntry,
    ) {
        for on in hosts_of(engine) {
            let Some(placed) = self.placed(&on.workspace) else {
                continue;
            };
            let host = self.host(&placed);
            if host
                .device
                .session()
                .send(Cmd::Revoke(entry.clone()))
                .is_err()
            {
                tracing::info!(
                    host = host.device.label(),
                    "a host did not hear that the person took a device back"
                );
            }
        }
    }
}

/// The host `hint` places a workspace on, when that is a dedicated host other than the device
/// `own`: one that runs its records' actions in its own workers. A workspace a desktop hosts
/// (its own desk's) runs them where its windows are.
fn placed_on_host(
    hint: &tilo_peer_state::join::TransportHint,
    own: [u8; 32],
) -> Option<IrohWorkspaceHostHintV1> {
    let (host, _) = IrohWorkspaceHostHintV1::authenticate_transport_hint(hint).ok()?;
    (host.mode == WorkspaceHostMode::CertifiedHost
        && host.host_device_certificate.device_public_key != own)
        .then_some(host)
}

/// One of the person's workspaces on a host that runs its records: the workspace as the host knows
/// it, and where it is reached.
struct OnHosts {
    /// A workspace of theirs this device is in there, which is where its host is asked anything
    /// about the person.
    workspace: String,
    host: IrohWorkspaceHostHintV1,
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
        if !seen.insert(host.host_device_certificate.device_public_key) {
            continue;
        }
        hosts.push(OnHosts { workspace, host });
    }
    hosts
}

/// The request that asks a host the person's record `step` (`room.personal.keep` with the list,
/// `room.personal` alone).
fn record_request(
    step: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_relay_session::workspaces::HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_relay_session::workspaces::HOST_ROOMS_RESOURCE,
                step,
            ),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: Some(20_000),
        appearance: None,
    }
}

/// Keep `list`, the person's workspace list, on every dedicated host of a workspace `engine` holds,
/// through the sessions `hosted` keeps with them: what the person's keys that run on no device of
/// theirs read it back from. One ask per host, in one of its workspaces this device is in; a host
/// that does not answer is told the next time the list moves.
pub(crate) fn keep_on_hosts(
    hosted: &dyn tilo_action_broker::services::HostedWorkspaces,
    engine: &Engine,
    list: &tilo_relay_session::workspaces::PersonalList,
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
        let request = record_request(tilo_relay_session::workspaces::KEEP_PERSONAL, arguments);
        let label = host.host_device_certificate.device_label.clone();
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

impl tilo_action_broker::services::HostedWorkspaces for HostedWorkspaces {
    fn host_actions(&self, workspace: &str) -> Option<Arc<dyn ActionHost + Send + Sync>> {
        self.actions_of(workspace)
    }
}

/// One workspace's actions on its host, entering it there on first use.
struct OnHost {
    host: Arc<Host>,
    workspace: String,
    workspace_id: u64,
    runtime: tokio::runtime::Handle,
}

impl OnHost {
    /// The workspace's actions on the host, once this device entered it there.
    fn entered(&self) -> Option<SessionActions> {
        self.host
            .entered
            .lock_recover()
            .get(&self.workspace_id)
            .cloned()
    }

    /// Ask `ask` of the workspace's actions on the host once this device is in it there.
    fn asking<T: Send + 'static>(
        &self,
        ask: impl FnOnce(SessionActions) -> Pending<Result<T, ActionHostError>> + Send + 'static,
    ) -> Pending<Result<T, ActionHostError>> {
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
                Err(ActionHostError::Unavailable {
                    reason: "the workspace's host did not answer".into(),
                })
            }));
        });
        answered
    }
}

/// `host`'s actions of `workspace`, entering it there first when this device is not in it yet.
async fn enter(
    host: &Host,
    workspace: &str,
    workspace_id: u64,
) -> Result<SessionActions, ActionHostError> {
    let _entering = host.entering.lock().await;
    if let Some(actions) = host.entered.lock_recover().get(&workspace_id) {
        return Ok(actions.clone());
    }
    let entered =
        host.device
            .join(workspace)
            .await
            .map_err(|error| ActionHostError::Unavailable {
                reason: format!("the workspace's host did not let this device in: {error}"),
            })?;
    let actions = host.device.actions(entered);
    host.entered
        .lock_recover()
        .insert(workspace_id, actions.clone());
    Ok(actions)
}

impl ActionHost for OnHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.query(&caller, query))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let caller = caller.clone();
        self.asking(move |actions| actions.invoke(&caller, request))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.entered()?.observe(caller, id)
    }

    fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.entered()?.next_change(caller, id)
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        match self.entered() {
            Some(actions) => actions.cancel(caller, id),
            None => Pending::ready(Err(ActionHostError::UnknownInvocation)),
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
    ) -> Pending<Result<ConfirmedInvocation, ActionHostError>> {
        let caller = caller.clone();
        let request = request.clone();
        self.asking(move |actions| actions.confirm(&caller, &request))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a test's unwrap is its assertion")]

    use super::placed_on_host;
    use tilo_delivery_protocol::workspace_join::{IrohWorkspaceHostHintV1, WorkspaceHostMode};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    /// A hint placing a workspace on the device `capabilities` certify, in `mode`, and that
    /// device's key.
    fn hint(
        mode: WorkspaceHostMode,
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
        let hint = IrohWorkspaceHostHintV1 {
            version: 1,
            mode,
            workspace_id: tilo_peer_state::join::room_workspace_id(&workspace).unwrap(),
            room: workspace,
            endpoint_addr: iroh::EndpointAddr::new(
                iroh::EndpointId::from_bytes(&cert.device_public_key).unwrap(),
            ),
            host_user_public_key: cert.user_public_key,
            host_device_certificate: cert.clone(),
            packet_relay_url: "iroh:direct".into(),
        }
        .transport_hint(&key)
        .unwrap();
        (hint, cert.device_public_key)
    }

    #[test]
    fn only_a_dedicated_host_of_another_device_runs_the_records() {
        let (dedicated, host) = hint(WorkspaceHostMode::CertifiedHost, DeviceCapabilities::host());
        assert!(
            placed_on_host(&dedicated, [7; 32]).is_some(),
            "a dedicated host runs its workspaces' records"
        );
        assert!(
            placed_on_host(&dedicated, host).is_none(),
            "this device runs those it hosts itself"
        );
        let (desk, _) = hint(
            WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
            DeviceCapabilities::primary(),
        );
        assert!(
            placed_on_host(&desk, [7; 32]).is_none(),
            "a desktop's own workspace runs its records where its windows are"
        );
    }
}
