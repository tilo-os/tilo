//! An agent of this login working on another of the person's devices: a `--connect --device`
//! client is served MCP here, and every request it makes goes to that device over the one action
//! frame between devices, signed by the agent's own key this device made for it
//! ([`tilo_workspace_service::agents::agent_keys`]). The device that answers checks the signature
//! as that agent's, for the person, under the agent's grants there. The desktop session and the
//! device's daemon serve such clients alike.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use collab_types::signer::DeviceSigner;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, ConfirmedInvocation,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, RemoteActionFrame,
    RemoteActionMessage,
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};
use tilo_server_session::client::ServerClient;
use tilo_user_identity::{DeviceCert, RevocationList};
use tilo_workspace_service::agents::agent_keys::{AgentKeys, AgentSigner};

use crate::i18n::{t, t_args};

/// How long one request of a forwarded client waits for the device's answer.
const REPLY_DEADLINE: Duration = Duration::from_secs(60);
/// How long a forwarded client's connection to the device may take to open.
const CONNECT_DEADLINE: Duration = Duration::from_secs(20);

/// What a host knows of where the person's other devices are.
pub trait Whereabouts: Send + Sync {
    /// The workspaces this device holds, whose rosters name the person's devices.
    fn workspaces(&self) -> Vec<String>;

    /// Where the device `id` is reached.
    fn addr(&self, id: EndpointId) -> EndpointAddr {
        EndpointAddr::new(id)
    }

    /// The certificates `workspace`'s roster keeps here: its members as this device holds them.
    fn devices(&self, workspace: &str) -> Vec<DeviceCert>;
}

/// The person's other devices, as this device asks them over the one action frame between
/// devices.
#[derive(Clone)]
pub struct OtherDevices(Arc<Inner>);

struct Inner {
    /// The endpoint this device asks from.
    endpoint: Endpoint,
    /// The runtime the connection to the other device opens on.
    runtime: tokio::runtime::Handle,
    /// The person this device is, and its own key.
    whose: ([u8; 32], [u8; 32]),
    /// This device's certificate and the key that signs as it: the connection the agent's
    /// requests travel on, which the other device admits as this device.
    cert: DeviceCert,
    signer: Arc<dyn DeviceSigner>,
    /// The person's revocations, as this device reads them.
    revocations: RevocationList,
    /// The keys this device makes for its person's agents.
    keys: Arc<dyn AgentKeys>,
    places: Arc<dyn Whereabouts>,
}

impl OtherDevices {
    /// Ask through `actions` from `endpoint` on `runtime`, as the device `this` of `person`,
    /// whose certificate `cert` and key `signer` are, reaching devices where `places` says.
    #[must_use]
    pub fn new(
        endpoint: Endpoint,
        runtime: tokio::runtime::Handle,
        (person, this): ([u8; 32], [u8; 32]),
        (cert, signer): (DeviceCert, Arc<dyn DeviceSigner>),
        revocations: RevocationList,
        keys: Arc<dyn AgentKeys>,
        places: Arc<dyn Whereabouts>,
    ) -> Self {
        Self(Arc::new(Inner {
            endpoint,
            runtime,
            whose: (person, this),
            cert,
            signer,
            revocations,
            keys,
            places,
        }))
    }

    /// Serve `stream`, a client of this login that asked to work on the device `forward` names
    /// as the person's agent: MCP here, every request it makes sent to that device as an ordinary
    /// action frame signed by the agent's own key this device made, and decided there. Refused,
    /// with why, where no device of the person's goes by that name. Blocks the calling thread,
    /// which must not be one of the runtime's.
    pub fn serve_forward(&self, mut stream: UnixStream, forward: &tilo_mcp::devices::Forward) {
        let (device, workspace) = match self.target(forward) {
            Ok(target) => target,
            Err(reason) => {
                tracing::info!(device = %forward.device, %reason, "MCP server: a client is not carried to another device");
                tilo_mcp::devices::refuse_forward(stream, reason);
                return;
            }
        };
        if let Err(error) = tilo_mcp::devices::open_forward(&mut stream) {
            tilo_failure::ignored!(
                "a client that left before it was carried needs no answer",
                tracing::debug!(%error, "MCP server: the client left before it was carried")
            );
            return;
        }
        let person = self.0.whose.0;
        let served = tilo_mcp::session_api::serve_as(stream, None, |hello, _| {
            let client = hello.client_name.clone();
            let agent = tilo_actions::remote::device_agent_id(&person, &client);
            let signer = AgentSigner::grant(
                Arc::clone(&self.0.keys),
                &workspace,
                (&agent, &client),
                &hex::encode(person),
            )
            .map_err(|reason| format!("this computer cannot make the agent a key: {reason}"))?;
            let (signer, _caller) = signer;
            let (agent_cert, agent_signer) = signer.signing()?;
            let session = self.0.runtime.block_on(self.open(
                device,
                &workspace,
                &client,
                agent_cert,
                agent_signer,
            ))?;
            let server = tilo_mcp::McpServer::for_agent(
                Forwarded(Arc::clone(&session)),
                workspace.clone(),
                &agent,
                &hex::encode(person),
            )
            .agent_named(&client);
            Ok(server)
        });
        if let Err(error) = served {
            tilo_failure::ignored!(
                "a client that disconnected needs no answer",
                tracing::debug!(%error, "MCP server: the carried client disconnected")
            );
        }
    }

    /// Open the connection to `device` and make it the run of the agent `client`, in
    /// `workspace`: invocations on it are signed by `agent_cert`/`agent_signer`, and the device
    /// admits them as that agent's.
    async fn open(
        &self,
        device: [u8; 32],
        workspace: &str,
        client: &str,
        agent_cert: DeviceCert,
        agent_signer: Arc<dyn DeviceSigner>,
    ) -> Result<Arc<ForwardSession>, String> {
        let id =
            EndpointId::from_bytes(&device).map_err(|_| "that is not a device key".to_owned())?;
        let workspace_id =
            WorkspaceId::parse(workspace).ok_or_else(|| "that is not a workspace id".to_owned())?;
        let whose = self.0.whose;
        let connect = ServerClient::connect_v2_to_own_device(
            self.0.endpoint.clone(),
            self.0.places.addr(id),
            whose.0,
            device,
            self.0.cert.clone(),
            Arc::clone(&self.0.signer),
            self.0.revocations.clone(),
        );
        let client_session = tokio::time::timeout(CONNECT_DEADLINE, connect)
            .await
            .map_err(|_| "the device did not answer".to_owned())
            .and_then(|connected| connected.map_err(|error| error.to_string()))?
            .signing_invocations_as(agent_cert, agent_signer);
        let client_session = Arc::new(client_session);
        client_session
            .send(Envelope::Subscribe { workspace_id })
            .map_err(|_| "the device's connection closed".to_owned())?;
        client_session
            .send(Envelope::AgentSession {
                request_id: 1,
                label: client.to_owned(),
            })
            .map_err(|_| "the device's connection closed".to_owned())?;
        let mut ready = false;
        let mut run = false;
        while !ready || !run {
            let envelope = tokio::time::timeout(REPLY_DEADLINE, client_session.recv())
                .await
                .map_err(|_| "the device did not answer".to_owned())?
                .ok_or_else(|| "the device closed the connection".to_owned())?;
            match envelope {
                Envelope::Ready {
                    workspace_id: ready_id,
                    ..
                } if ready_id == workspace_id => {
                    ready = true;
                }
                Envelope::NotAuthorised { refusal, .. } => {
                    return Err(format!(
                        "the workspace did not admit this device: {refusal}"
                    ));
                }
                Envelope::AgentSessionAnswer {
                    refusal: Some(reason),
                    ..
                } => {
                    return Err(format!("the device did not take the agent's run: {reason}"));
                }
                Envelope::AgentSessionAnswer { refusal: None, .. } => run = true,
                Envelope::AgentSessionStopped => {
                    return Err("the device ended the agent's run".to_owned());
                }
                _ => {}
            }
        }
        let session = Arc::new(ForwardSession {
            client: Arc::clone(&client_session),
            workspace: workspace_id,
            host: device,
            own: whose.1,
            user: whose.0,
            next_correlation: AtomicU64::new(1),
            waits: std::sync::Mutex::default(),
        });
        session.clone().start();
        Ok(session)
    }

    /// The device `forward` names and the workspace the agent works in there: the one it names,
    /// else the one of this device's workspaces that device is in. A device is named by its name
    /// or its key.
    fn target(&self, forward: &tilo_mcp::devices::Forward) -> Result<([u8; 32], String), String> {
        let asked = forward.device.trim();
        let key = tilo_failure::ignored!(
            "a device named by its name is not a key",
            hex::decode(asked).ok()
        )
        .and_then(|key| <[u8; 32]>::try_from(key).ok());
        if let (Some(key), Some(workspace)) = (key, &forward.workspace) {
            return Ok((key, workspace.clone()));
        }
        let workspaces = match &forward.workspace {
            Some(workspace) => vec![workspace.clone()],
            None => self.0.places.workspaces(),
        };
        let mut found = Vec::new();
        let mut known = std::collections::BTreeSet::new();
        for workspace in workspaces {
            for device in self.others_in(&workspace) {
                let name = tilo_mcp::devices::device_name(&device);
                if key == Some(device.device_public_key) || name.eq_ignore_ascii_case(asked) {
                    found.push((device.device_public_key, workspace.clone()));
                }
                known.insert(name);
            }
        }
        match found.as_slice() {
            [one] => Ok(one.clone()),
            [] if known.is_empty() => Err(t("other-devices-none")),
            [] => Err(t_args(
                "other-devices-unknown",
                &[
                    ("device", asked.to_owned().into()),
                    (
                        "devices",
                        known.into_iter().collect::<Vec<_>>().join(", ").into(),
                    ),
                ],
            )),
            _ => Err(t_args(
                "other-devices-name-workspace",
                &[("device", asked.to_owned().into())],
            )),
        }
    }

    /// The person's other devices in `workspace`'s roster as this device holds it.
    fn others_in(&self, workspace: &str) -> Vec<DeviceCert> {
        let (person, this) = self.0.whose;
        self.0
            .places
            .devices(workspace)
            .into_iter()
            .filter(|device| {
                device.user_public_key == person && device.device_public_key != this
            })
            .collect()
    }
}

/// One forwarded client's connection to the device that answers for it.
struct ForwardSession {
    client: Arc<ServerClient>,
    workspace: WorkspaceId,
    host: [u8; 32],
    own: [u8; 32],
    user: [u8; 32],
    next_correlation: AtomicU64,
    /// What each request waits for: an answer by its correlation, or an invocation's receipt or
    /// state.
    waits: std::sync::Mutex<Waits>,
}

/// The requests waiting for an answer, by what answers them.
#[derive(Default)]
struct Waits {
    by_correlation: BTreeMap<u64, tilo_actions::remote::Completer<Answered>>,
    by_invocation: std::collections::HashMap<
        tilo_actions::InvocationId,
        Vec<tilo_actions::remote::Completer<Answered>>,
    >,
}

type Answered = Result<RemoteActionMessage, ActionCallError>;

impl std::fmt::Debug for ForwardSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardSession")
            .field("host", &hex::encode(self.host))
            .finish_non_exhaustive()
    }
}

impl ForwardSession {
    /// Read everything the device answers, on the runtime, and hand each to the request it
    /// answers. Ends when the device's connection ends, failing what still waits.
    fn start(self: Arc<Self>) {
        let client = Arc::clone(&self.client);
        tokio::spawn(async move {
            while let Some(envelope) = client.recv().await {
                if !self.heard(envelope) {
                    break;
                }
            }
            self.failed(&ActionCallError::Unavailable {
                reason: "the device's connection ended".to_owned(),
            });
        });
    }

    /// Hand `envelope` to the request it answers.
    fn heard(&self, envelope: Envelope) -> bool {
        match envelope {
            Envelope::RemoteAction { frame, .. } => {
                let Ok(frame) = RemoteActionFrame::decode(&frame) else {
                    return true;
                };
                self.answered(frame.message);
                true
            }
            Envelope::RemoteActionUndeliverable { refusal, .. } => {
                self.failed(&ActionCallError::Refused {
                    reason: format!("{refusal:?}"),
                });
                true
            }
            Envelope::AgentSessionStopped => false,
            _ => true,
        }
    }

    fn answered(&self, message: RemoteActionMessage) {
        let (correlation, invocation) = match &message {
            RemoteActionMessage::CatalogueResponse { correlation, .. }
            | RemoteActionMessage::ConfirmResponse { correlation, .. }
            | RemoteActionMessage::Update {
                correlation: Some(correlation),
                ..
            } => (Some(*correlation), None),
            RemoteActionMessage::Receipt(receipt) => (None, Some(receipt.invocation)),
            RemoteActionMessage::Update {
                invocation,
                correlation: None,
                ..
            }
            | RemoteActionMessage::CancelResponse { invocation, .. } => (None, Some(*invocation)),
            _ => (None, None),
        };
        let waiting = {
            let mut waits = self
                .waits
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match (correlation, invocation) {
                (Some(correlation), _) => waits.by_correlation.remove(&correlation),
                (None, Some(invocation)) => {
                    waits
                        .by_invocation
                        .get_mut(&invocation)
                        .and_then(|waiting| {
                            if waiting.is_empty() {
                                None
                            } else {
                                Some(waiting.remove(0))
                            }
                        })
                }
                (None, None) => None,
            }
        };
        if let Some(waiting) = waiting {
            waiting.complete(Ok(message));
        }
    }

    /// Fail every request still waiting: the device closed or refused the frame.
    fn failed(&self, error: &ActionCallError) {
        let waits = std::mem::take(
            &mut *self
                .waits
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for waiting in waits.by_correlation.into_values() {
            waiting.complete(Err(error.clone()));
        }
        for waiting in waits.by_invocation.into_values().flatten() {
            waiting.complete(Err(error.clone()));
        }
    }

    /// Send `message` to the device and wait for the answer `key` names.
    fn ask(&self, message: &RemoteActionMessage) -> Pending<Answered> {
        let frame = match RemoteActionFrame::new(message.clone()).encode() {
            Ok(frame) => frame,
            Err(error) => {
                return Pending::ready(Err(ActionCallError::Refused {
                    reason: format!("the request does not encode: {error}"),
                }));
            }
        };
        let (completer, pending) = tilo_actions::remote::pending();
        {
            let mut waits = self
                .waits
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match message {
                RemoteActionMessage::CatalogueRequest { correlation, .. }
                | RemoteActionMessage::Confirm { correlation, .. } => {
                    waits.by_correlation.insert(*correlation, completer);
                }
                RemoteActionMessage::Invoke(request) => {
                    waits
                        .by_invocation
                        .entry(request.invocation)
                        .or_default()
                        .push(completer);
                }
                RemoteActionMessage::Observe {
                    correlation,
                    invocation,
                    ..
                } => {
                    if *correlation == 0 {
                        waits
                            .by_invocation
                            .entry(*invocation)
                            .or_default()
                            .push(completer);
                    } else {
                        waits.by_correlation.insert(*correlation, completer);
                    }
                }
                RemoteActionMessage::Cancel { invocation } => {
                    waits
                        .by_invocation
                        .entry(*invocation)
                        .or_default()
                        .push(completer);
                }
                _ => {}
            }
        }
        match self.client.send(Envelope::RemoteAction {
            workspace_id: self.workspace,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: self.host,
            from_device_public_key: self.own,
            from_user_public_key: self.user,
            frame,
        }) {
            Ok(()) => pending,
            Err(_) => Pending::ready(Err(ActionCallError::Refused {
                reason: "the device's connection closed".to_owned(),
            })),
        }
    }
}

/// One forwarded client's actions, each asked of the device that answers for it.
#[derive(Clone)]
struct Forwarded(Arc<ForwardSession>);

impl ActionProvider for Forwarded {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.0
            .ask(&RemoteActionMessage::CatalogueRequest { correlation, query })
            .map(|answer| {
                answer.and_then(|message| match message {
                    RemoteActionMessage::CatalogueResponse { result, .. } => result,
                    _ => Err(unexpected()),
                })
            })
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.0
            .ask(&RemoteActionMessage::Invoke(request))
            .map(|answer| {
                answer.and_then(|message| match message {
                    RemoteActionMessage::Receipt(receipt) => match receipt.status {
                        tilo_actions::remote::ReceiptStatus::Rejected { error } => Err(error),
                        _ => Ok(receipt),
                    },
                    _ => Err(unexpected()),
                })
            })
    }

    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.0
            .ask(&RemoteActionMessage::Observe {
                correlation,
                invocation: *id,
                wait_ms: None,
                seen: None,
            })
            .wait_timeout(REPLY_DEADLINE)
            .ok()
            .flatten()
            .and_then(|answer| match answer {
                Ok(RemoteActionMessage::Update { state, .. }) => state,
                _ => None,
            })
    }

    fn next_change(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<Pending<()>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        let asked = self.0.ask(&RemoteActionMessage::Observe {
            correlation,
            invocation: *id,
            wait_ms: Some(1_000),
            seen: None,
        });
        let (changed, pending) = tilo_actions::remote::pending();
        asked.then(move |_| changed.complete(()));
        Some(pending)
    }

    fn cancel(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        self.0
            .ask(&RemoteActionMessage::Cancel { invocation: *id })
            .map(|answer| {
                answer.and_then(|message| match message {
                    RemoteActionMessage::CancelResponse { result, .. } => result,
                    _ => Err(unexpected()),
                })
            })
    }

    fn confirm(
        &self,
        _caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
        let correlation = self.0.next_correlation.fetch_add(1, Ordering::Relaxed);
        self.0
            .ask(&RemoteActionMessage::Confirm {
                correlation,
                request: Box::new(request.clone()),
            })
            .map(|answer| {
                answer.and_then(|message| match message {
                    RemoteActionMessage::ConfirmResponse { result, .. } => {
                        result.map(|confirmed| *confirmed)
                    }
                    _ => Err(unexpected()),
                })
            })
    }
}

fn unexpected() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "the device answered with an unexpected message".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_mcp::devices::{Forward, agent_id, forward_request};

    fn certificate(label: &str, key: u8) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [key; 32],
            device_label: label.into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    tilo_test_env::scenario! {
        fn a_forward_names_its_agent_its_request_and_the_devices_that_answer;

        phase an_agent_is_named_by_the_device_it_came_from;
        {
            let laptop = certificate("Studio Laptop", 0x3f);
            assert_eq!(
                agent_id("claude-code", &laptop),
                "claude-code@studio-laptop-3f3f3f3f"
            );
            assert_eq!(
                agent_id("codex", &certificate("  ", 0xab)),
                "codex@abababab"
            );
            let id = agent_id(&"x".repeat(64), &certificate(&"Ü".repeat(200), 1));
            tilo_peer_state::workspace_membership::valid_agent_id(&id).unwrap();
        }

        phase only_a_forward_line_asks_for_a_forward;
        {
            assert_eq!(
                forward_request(br#"{"forward":{"device":"studio"}}"#),
                Some(Forward {
                    device: "studio".into(),
                    workspace: None
                })
            );
            assert_eq!(
                forward_request(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#),
                None
            );
            assert_eq!(forward_request(br#"{"op":"agent_place"}"#), None);
            assert_eq!(
                forward_request(br#"{"forward":{"device":"studio","as":"person"}}"#),
                None,
                "nothing beyond the device and the workspace is read"
            );
            assert!(!tilo_mcp::session_api::is_control_request(
                br#"{"forward":{"device":"studio"}}"#
            ));
        }
    }
}
