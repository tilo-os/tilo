//! A person's device hosts a workspace with the same host a server runs, and a
//! key of theirs uses it with the same calls it uses on a server: it opens a
//! session with the device, lists their workspaces there, enters the
//! workspace (the host invites it on the session and it redeems there), and
//! runs a record action addressed to the host's own device, which the
//! device's own action broker answers in a run of the key's
//! ([`tilo_runtime::key_sessions`]).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives until it ends"
)]

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use tilo_action_policy::{Answer, Grantee};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage,
};
use tilo_desktop_model::WorkspaceId;
use tilo_runtime::key_sessions::{KeySessions, SessionHost, Sessions};
use tilo_runtime::other_devices::Whereabouts;
use tilo_runtime::personal_devices::PersonalDevices;
use tilo_session_client::PeerConfig;
use tilo_session_client::device::Device;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_service::service::{ServiceConfig, ServiceIdentity, WorkspaceService};

/// The app the device runs, which a key lists and acts on.
const NOTES: &str = "so.tilo.notes";

fn certified(
    root: &SigningKey,
    n: u8,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// A localhost endpoint with no relay whose identity is the device `key`.
async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

fn addr(endpoint: &Endpoint) -> EndpointAddr {
    EndpointAddr::new(endpoint.id()).with_ip_addr(std::net::SocketAddr::from((
        std::net::Ipv4Addr::LOCALHOST,
        endpoint.bound_sockets().first().unwrap().port(),
    )))
}

fn notebook(action: Option<&str>) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: "1".into(),
        app_id: NOTES.into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: "notes.notebook".into(),
            kind: "notes.notebook".into(),
            label: "Notebook".into(),
            presentation: None,
        },
        action: action.map(|action| tilo_actions::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new("notes.notebook", action),
            resource_kind: "notes.notebook".into(),
            resource_label: "Notebook".into(),
            action_label: "Pin".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract::default(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// The device's broker as a key's run reaches it: Notes runs there, and every
/// caller and invocation is kept.
#[derive(Clone, Default)]
struct Broker {
    /// The workspace its apps run in.
    workspace: String,
    callers: Arc<Mutex<Vec<Caller>>>,
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
}

impl ActionProvider for Broker {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        self.callers.lock().unwrap().push(caller.clone());
        let page = match query.provider.as_deref() {
            None => CataloguePage {
                revision: "1".into(),
                providers: vec![CatalogueProvider {
                    provider: "1".into(),
                    app_id: NOTES.into(),
                    workspace: self.workspace.clone(),
                    document_id: String::new(),
                    browsable: true,
                }],
                ..CataloguePage::default()
            },
            Some("1") => CataloguePage::from_rows(
                "1".into(),
                vec![notebook(None), notebook(Some("pin"))],
                &query,
            ),
            Some(_) => CataloguePage::from_rows("1".into(), Vec::new(), &query),
        };
        Pending::ready(Ok(page))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.callers.lock().unwrap().push(caller.clone());
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request);
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        }))
    }

    fn observe(
        &self,
        _caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<InvocationState> {
        let ran = self
            .invoked
            .lock()
            .unwrap()
            .iter()
            .any(|request| request.invocation == *id);
        ran.then(|| {
            InvocationState::new(
                *id,
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: tilo_actions::ActionOutput::default(),
                },
            )
        })
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// The device's runs: one workspace, and the keys a run was opened for.
struct DeviceRuns {
    workspace: String,
    broker: Broker,
    opened: Mutex<Vec<DeviceCert>>,
}

impl Sessions for DeviceRuns {
    fn workspace(&self, workspace_id: WorkspaceId) -> Option<String> {
        (tilo_desktop_model::WorkspaceId::parse(&self.workspace) == Some(workspace_id))
            .then(|| self.workspace.clone())
    }

    fn open(&self, workspace: &str, key: &DeviceCert) -> Result<(SessionHost, Caller), String> {
        if workspace != self.workspace {
            return Err(format!("no workspace {workspace} here"));
        }
        self.opened.lock().unwrap().push(key.clone());
        let caller =
            Caller::local_person("ada").delegated_to(tilo_mcp::devices::agent_id("agent", key));
        Ok((Arc::new(self.broker.clone()), caller))
    }
}

/// The person's device, hosting a workspace for them on its endpoint.
struct Box_ {
    host: Arc<WorkspaceService>,
    cert: DeviceCert,
    endpoint: Endpoint,
    workspace: String,
    runs: Arc<DeviceRuns>,
    _router: Router,
    _state: tempfile::TempDir,
}

async fn device_host(root: &SigningKey) -> Box_ {
    let (key, cert) = certified(root, 2, "Build box", DeviceCapabilities::primary());
    let endpoint = endpoint(&key).await;
    let state = tempfile::tempdir().unwrap();
    let key_sessions = KeySessions::new();
    let host = Arc::new(
        WorkspaceService::open(
            ServiceConfig {
                apps: Some(Arc::clone(&key_sessions)
                    as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>),
                // Reached on this machine's own network: nothing will name this
                // endpoint's address, so opening does not wait for one.
                reaching: tilo_workspace_service::Reaching::OnItsOwn,
                ..ServiceConfig::new(
                    state.path().join("workspace-host"),
                    tilo_server_protocol::WorkSetting::none(),
                )
            },
            ServiceIdentity {
                user_public_key: cert.user_public_key,
                device_signing_key: key.clone(),
                device_cert: cert.clone(),
                revocations: tilo_user_identity::RevocationList::new(),
            },
            endpoint.clone(),
            None,
        )
        .await
        .unwrap(),
    );
    let made = host.workspaces().create("Shell").await.unwrap();
    let runs = Arc::new(DeviceRuns {
        workspace: made.workspace.clone(),
        broker: Broker {
            workspace: made.workspace.clone(),
            ..Broker::default()
        },
        opened: Mutex::default(),
    });
    key_sessions.serve(
        Arc::new(PersonalDevices::new(host.workspaces().clone(), &cert)),
        Arc::clone(&runs) as Arc<dyn Sessions>,
    );
    let router = host
        .protocols()
        .into_iter()
        .fold(
            Router::builder(endpoint.clone()),
            |router, (alpn, handler)| router.accept(alpn, handler),
        )
        .spawn();
    Box_ {
        host,
        cert,
        endpoint,
        workspace: made.workspace,
        runs,
        _router: router,
        _state: state,
    }
}

/// `key`'s session with the device, as it opens one with any host.
async fn session(device: &Box_, key: &(SigningKey, DeviceCert)) -> Device {
    let session = Device::over(
        PeerConfig {
            host: tilo_server_client::ServerAddress::at(addr(&device.endpoint)),
            cert: key.1.clone(),
            server_cert: device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        Arc::new(key.0.clone()),
        &key.1.device_label,
    );
    session.connected().await.unwrap();
    session
}

fn tool(id: u64, name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call",
        "params":{"name":name,"arguments":arguments}})
}

fn text(answer: &Value) -> &str {
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
}

#[tokio::test(flavor = "current_thread")]
async fn a_key_lists_enters_and_acts_in_a_workspace_its_persons_device_hosts() {
    let root = SigningKey::from_bytes(&[0x11; 32]);
    let device = device_host(&root).await;
    let agent = certified(
        &root,
        9,
        "Visa backend #1",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let key = session(&device, &agent).await;

    // The person's workspaces, as the host lists them to a key of theirs.
    let listed = key
        .personal_list(
            &tilo_server_protocol::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .unwrap();
    assert!(
        listed
            .workspaces
            .iter()
            .any(|listed| listed.workspace == device.workspace),
        "the device lists the workspace it hosts: {listed:?}"
    );

    // It enters the workspace as every key enters one on any host.
    let workspace_id = key
        .enter_workspace(&device.workspace, "Visa backend #1")
        .await
        .unwrap();
    assert!(
        device
            .host
            .is_subscribed(workspace_id, &agent.1.device_public_key)
            .await
    );

    // Its agent surface acts there, on the device's broker, in a run of its own.
    let entered = tilo_mcp::McpServer::for_agent(
        key.actions(workspace_id),
        device.workspace.clone(),
        &tilo_mcp::devices::agent_id("agent", &agent.1),
        &hex::encode(agent.1.user_public_key),
    );
    let apps = entered
        .handle(tool(2, "tilo_ls", &json!({})))
        .await
        .unwrap();
    assert_eq!(apps["result"]["isError"], false, "{apps}");
    assert!(text(&apps).contains("notes"), "{apps}");
    let done = entered
        .handle(tool(3, "tilo_do", &json!({"path": "notes/Notebook#pin"})))
        .await
        .unwrap();
    assert_eq!(done["result"]["isError"], false, "{done}");
    assert_eq!(
        device.runs.broker.invoked.lock().unwrap().len(),
        1,
        "it ran on the device"
    );
    let opened = device.runs.opened.lock().unwrap().clone();
    assert_eq!(
        opened,
        vec![agent.1.clone()],
        "one run, for the key that came in"
    );
    key.close();
}

#[tokio::test(flavor = "current_thread")]
async fn a_key_reaching_another_workspace_enters_nothing_and_runs_nothing_here() {
    let root = SigningKey::from_bytes(&[0x12; 32]);
    let device = device_host(&root).await;
    let elsewhere = certified(
        &root,
        10,
        "elsewhere",
        DeviceCapabilities::agent_grant(
            AgentScope::parse("22222222-2222-4222-8222-222222222222").unwrap(),
        ),
    );
    let key = session(&device, &elsewhere).await;
    let refused = key
        .enter_workspace(&device.workspace, "elsewhere")
        .await
        .unwrap_err();
    // The device's host said so itself, and why: a session that never reached
    // it, or an answer that never came, is no refusal of this key.
    assert_eq!(
        tilo_server_protocol::workspaces::AdminRefusal::from_reason(&refused.message),
        Some(tilo_server_protocol::workspaces::AdminRefusal::NotYourOwn),
        "{refused:?}"
    );
    assert!(
        !device
            .host
            .is_subscribed(
                device.workspace.parse().unwrap(),
                &elsewhere.1.device_public_key
            )
            .await,
        "the key entered nothing"
    );
    assert!(device.runs.opened.lock().unwrap().is_empty());
}

/// The agent keys a test makes from the person's root it holds: the same key for the same agent
/// in the same workspace, certified as an agent's grant, exactly as the device does in production.
#[derive(Default)]
struct TestKeys {
    root: Option<SigningKey>,
    made: Mutex<Vec<([u8; 32], DeviceCert, SigningKey)>>,
}

impl tilo_workspace_service::agents::agent_keys::AgentKeys for TestKeys {
    fn grant(
        &self,
        workspace: tilo_server_protocol::WorkspaceId,
        agent: &str,
        _name: &str,
        person: [u8; 32],
    ) -> Result<[u8; 32], String> {
        let root = self.root.as_ref().ok_or("this device holds no root")?;
        let mut seed = [0x5a; 32];
        seed[0] = u8::try_from(agent.len()).unwrap_or(0);
        let key = SigningKey::from_bytes(&seed);
        let public = key.verifying_key().to_bytes();
        let now = tilo_clock::unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: public,
            device_label: agent.into(),
            capabilities: DeviceCapabilities::agent_grant(AgentScope::agent_in(
                *workspace.as_uuid().as_bytes(),
                agent,
            )),
            issued_at_ns: now.saturating_sub(1_000_000_000),
            expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).map_err(|error| error.to_string())?;
        self.made.lock().unwrap().push((public, cert, key));
        Ok(public)
    }

    fn sign(
        &self,
        workspace: tilo_server_protocol::WorkspaceId,
        key: &[u8; 32],
        request: &mut InvokeRequest,
    ) -> Result<(), String> {
        let made = self.made.lock().unwrap();
        let (_, cert, signing) = made
            .iter()
            .find(|(public, ..)| public == key)
            .ok_or("the agent's key was not made here")?;
        request.signed = None;
        let invocation = tilo_server_protocol::invocation::sign_invocation_now(
            &workspace.to_string(),
            request,
            cert,
            signing,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| error.to_string())?;
        request.signed = Some(Box::new(invocation));
        Ok(())
    }

    fn made(&self, key: &[u8; 32]) -> Option<(DeviceCert, SigningKey)> {
        self.made
            .lock()
            .unwrap()
            .iter()
            .find(|(public, ..)| public == key)
            .map(|(_, cert, signing)| (cert.clone(), signing.clone()))
    }
}

/// What the device that keeps the workspace answers a forwarded client with: Notes runs there,
/// and the caller of each request is kept, as the connection admitted it.
#[derive(Default)]
struct Answers {
    broker: Mutex<Broker>,
    /// Every request that reached the host, and the callers the invocations came as.
    invokers: Arc<Mutex<Vec<Caller>>>,
}

impl Answers {
    fn answer(&self, caller: &Caller, message: RemoteActionMessage) -> RemoteActionMessage {
        match message {
            RemoteActionMessage::CatalogueRequest { correlation, query } => {
                let result = self
                    .broker
                    .lock()
                    .unwrap()
                    .query(caller, query)
                    .wait_timeout(std::time::Duration::from_secs(5))
                    .ok()
                    .flatten()
                    .unwrap_or(Err(ActionCallError::Unanswered));
                RemoteActionMessage::CatalogueResponse {
                    correlation,
                    result,
                }
            }
            RemoteActionMessage::Invoke(request) => {
                self.invokers.lock().unwrap().push(caller.clone());
                let invocation = request.invocation;
                let receipt = {
                    let broker = self.broker.lock().unwrap();
                    broker
                        .invoke(caller, request)
                        .wait_timeout(std::time::Duration::from_secs(5))
                        .ok()
                        .flatten()
                };
                let status = match receipt {
                    Some(Ok(receipt)) => receipt.status,
                    Some(Err(error)) => ReceiptStatus::Rejected { error },
                    None => ReceiptStatus::Rejected {
                        error: ActionCallError::Unanswered,
                    },
                };
                RemoteActionMessage::Receipt(InvocationReceipt { invocation, status })
            }
            RemoteActionMessage::Observe { invocation, .. } => RemoteActionMessage::Update {
                invocation,
                state: self.broker.lock().unwrap().observe(caller, &invocation),
                correlation: None,
            },
            RemoteActionMessage::Cancel { invocation } => RemoteActionMessage::CancelResponse {
                invocation,
                result: Ok(()),
            },
            other => tilo_actions::remote::refuse(
                &other,
                ActionCallError::Unavailable {
                    reason: "not here".into(),
                },
            )
            .unwrap_or(other),
        }
    }
}

impl tilo_workspace_service::app_serving::AppActionServer for Answers {
    fn serve(
        &self,
        _workspace: tilo_server_protocol::WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        Some(Pending::ready(self.answer(caller, message)))
    }

    fn serve_certified(
        &self,
        _workspace: tilo_server_protocol::WorkspaceId,
        caller: &Caller,
        _certificate: &DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        Some(Pending::ready(self.answer(caller, message)))
    }
}

/// The person's devices where this laptop finds them: the workspace one device keeps, its roster,
/// and where that device is.
struct TestPlaces {
    workspace: String,
    host: EndpointAddr,
    devices: Vec<DeviceCert>,
}

impl Whereabouts for TestPlaces {
    fn workspaces(&self) -> Vec<String> {
        vec![self.workspace.clone()]
    }

    fn reachable(&self, _workspace: &str, _device: [u8; 32]) -> bool {
        true
    }

    fn addr(&self, _id: EndpointId) -> EndpointAddr {
        self.host.clone()
    }

    fn devices(&self, _workspace: &str) -> Vec<DeviceCert> {
        self.devices.clone()
    }
}

/// A forwarded client, speaking MCP on its own line each.
struct ForwardedClient {
    stream: std::os::unix::net::UnixStream,
    reader: std::io::BufReader<std::os::unix::net::UnixStream>,
    next: u64,
}

impl ForwardedClient {
    fn over(stream: std::os::unix::net::UnixStream) -> Self {
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        stream
            .set_write_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let reader = std::io::BufReader::new(stream.try_clone().unwrap());
        let mut client = Self {
            stream,
            reader,
            next: 1,
        };
        // The client's hello, as every MCP client opens.
        let hello = client.send(&json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "Claude Code", "version": "1"}
            }
        }));
        assert!(hello.get("result").is_some(), "{hello}");
        client
    }

    fn send(&mut self, request: &Value) -> Value {
        use std::io::{BufRead as _, Write as _};
        writeln!(self.stream, "{request}").unwrap();
        let id = request["id"].clone();
        loop {
            let mut line = String::new();
            let read = self.reader.read_line(&mut line).unwrap();
            assert_ne!(read, 0, "the forward closed without answering {request}");
            let answer: Value = match serde_json::from_str(&line) {
                Ok(answer) => answer,
                Err(_) => continue,
            };
            if answer.get("id") == Some(&id) && answer.get("method").is_none() {
                return answer;
            }
        }
    }

    fn call(&mut self, name: &str, arguments: &Value) -> Value {
        let id = self.next;
        self.next = id.saturating_add(1);
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
    }
}

fn refused_by(answer: &Value) -> String {
    assert_eq!(answer["result"]["isError"], true, "{answer}");
    answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Two of a person's devices: one keeps a workspace, the other's daemon carries an agent's MCP
/// client to it under the agent's own key. Returns the forward along with what the test flips.
struct Forwarded {
    client: ForwardedClient,
    host: Arc<WorkspaceService>,
    answers: Arc<Answers>,
    workspace: String,
    host_cert: DeviceCert,
    person: [u8; 32],
    host_endpoint: Endpoint,
    laptop_endpoint: Endpoint,
    router: Router,
    served: Option<std::thread::JoinHandle<()>>,
    _state: tempfile::TempDir,
}

impl Forwarded {
    async fn start() -> Self {
        let root = SigningKey::from_bytes(&[0x21; 32]);
        let person = root.verifying_key().to_bytes();
        let (host_key, host_cert) = certified(&root, 2, "Build box", DeviceCapabilities::primary());
        let host_endpoint = endpoint(&host_key).await;
        let answers = Arc::new(Answers::default());
        let state = tempfile::tempdir().unwrap();
        let host = Arc::new(
            WorkspaceService::open(
                ServiceConfig {
                    apps: Some(Arc::clone(&answers)
                        as Arc<dyn tilo_workspace_service::app_serving::AppActionServer>),
                    reaching: tilo_workspace_service::Reaching::OnItsOwn,
                    ..ServiceConfig::new(
                        state.path().join("workspace-host"),
                        tilo_server_protocol::WorkSetting::none(),
                    )
                },
                ServiceIdentity {
                    user_public_key: person,
                    device_signing_key: host_key.clone(),
                    device_cert: host_cert.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                },
                host_endpoint.clone(),
                None,
            )
            .await
            .unwrap(),
        );
        let made = host.workspaces().create("Shell").await.unwrap();
        let workspace = made.workspace.clone();
        *answers.broker.lock().unwrap() = Broker {
            workspace: workspace.clone(),
            ..Broker::default()
        };
        let router = host.protocols().into_iter().fold(
            Router::builder(host_endpoint.clone()),
            |router, (alpn, handler)| router.accept(alpn, handler),
        );
        let router = router.spawn();

        // The laptop: another device of the person's, holding their root, which makes the
        // agent's key as the person's device does.
        let (laptop_key, laptop_cert) =
            certified(&root, 3, "Studio laptop", DeviceCapabilities::primary());
        let laptop_endpoint = endpoint(&laptop_key).await;
        let keys = Arc::new(TestKeys {
            root: Some(root.clone()),
            made: Mutex::default(),
        });
        let places = Arc::new(TestPlaces {
            workspace: workspace.clone(),
            host: addr(&host_endpoint),
            devices: vec![host_cert.clone(), laptop_cert.clone()],
        });
        let other = tilo_runtime::other_devices::OtherDevices::new(
            laptop_endpoint.clone(),
            tokio::runtime::Handle::current(),
            (person, laptop_cert.device_public_key),
            (laptop_cert.clone(), Arc::new(laptop_key.clone())),
            tilo_user_identity::RevocationList::new(),
            keys,
            places,
        );
        let (local, remote) = std::os::unix::net::UnixStream::pair().unwrap();
        let forward = tilo_mcp::devices::Forward {
            device: "Build box".into(),
            workspace: Some(workspace.clone()),
        };
        let served = std::thread::spawn(move || other.serve_forward(remote, &forward));
        Self {
            client: ForwardedClient::over(local),
            host,
            answers,
            workspace,
            host_cert,
            person,
            host_endpoint,
            laptop_endpoint,
            router,
            served: Some(served),
            _state: state,
        }
    }

    /// Let the person's agents from their other devices in on this computer, or take it back.
    async fn let_in(&self, answer: Answer) {
        let grant = tilo_action_policy::device_agents::grant(
            &self.workspace,
            &self.host_cert.device_public_key,
            answer,
            Grantee::person(hex::encode(self.person)),
        );
        tilo_workspace_service::workspaces::fixtures::record_grants(
            self.host.workspaces(),
            &self.workspace,
            &[grant],
        )
        .await;
    }

    /// Take the answer back.
    async fn take_back(&self) {
        tilo_workspace_service::workspaces::fixtures::revoke_grants(
            self.host.workspaces(),
            &self.workspace,
            &[tilo_action_policy::device_agents::grant_id(
                &self.host_cert.device_public_key,
            )],
        )
        .await;
    }

    /// The caller the host last ran an invocation as.
    fn last_invoker(&self) -> Caller {
        self.answers
            .invokers
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("an invocation reached the host")
    }

    async fn finish(mut self) {
        drop(self.client);
        if let Some(served) = self.served.take() {
            served.join().unwrap();
        }
        self.router.shutdown().await.unwrap();
        self.host_endpoint.close().await;
        self.laptop_endpoint.close().await;
        self.host.shutdown().await;
    }
}

tilo_test_env::scenario! {
    fn an_agent_on_the_persons_laptop_works_on_another_device_through_the_forward;

    phase the_forward_lists_the_persons_devices_and_a_call_there_is_refused_until_let_in;
    {
        runtime().block_on(async {
            let mut forwarded = Forwarded::start().await;
            let listed = forwarded.client.call("tilo_devices", &json!({}));
            let text = text(&listed);
            assert!(text.contains("Build box"), "{listed}");
            assert!(
                text.contains(&hex::encode(forwarded.host_cert.device_public_key)),
                "{listed}"
            );
            let refused = forwarded
                .client
                .call("tilo_do", &json!({"path": "notes/Notebook#pin"}));
            let why = refused_by(&refused);
            assert!(
                why.contains("Accept agents from my other devices"),
                "the refusal names the setting: {why}"
            );
            assert!(forwarded.answers.invokers.lock().unwrap().is_empty());
            forwarded.finish().await;
        });
    }

    phase let_in_a_step_its_grants_ask_first_is_put_to_the_person_here;
    {
        runtime().block_on(async {
            let mut forwarded = Forwarded::start().await;
            forwarded.let_in(Answer::Ask).await;
            let done = forwarded
                .client
                .call("tilo_do", &json!({"path": "notes/Notebook#pin"}));
            assert_eq!(done["result"]["isError"], false, "{done}");
            let caller = forwarded.last_invoker();
            assert!(caller.acting().is_agent(), "it acts as the agent: {caller:?}");
            assert!(
                !caller.lets_steps_go_unasked(),
                "the step its grants ask first is put to the person here"
            );
            forwarded.finish().await;
        });
    }

    phase letting_them_act_without_asking_answers_the_ask_on_the_device;
    {
        runtime().block_on(async {
            let mut forwarded = Forwarded::start().await;
            forwarded.let_in(Answer::Ask).await;
            forwarded.let_in(Answer::Allow).await;
            let done = forwarded
                .client
                .call("tilo_do", &json!({"path": "notes/Notebook#pin"}));
            assert_eq!(done["result"]["isError"], false, "{done}");
            assert!(
                forwarded.last_invoker().lets_steps_go_unasked(),
                "the person let the step go unasked here"
            );
            forwarded.finish().await;
        });
    }

    phase taking_the_answer_back_refuses_the_next_call;
    {
        runtime().block_on(async {
            let mut forwarded = Forwarded::start().await;
            forwarded.let_in(Answer::Ask).await;
            let done = forwarded
                .client
                .call("tilo_do", &json!({"path": "notes/Notebook#pin"}));
            assert_eq!(done["result"]["isError"], false, "{done}");
            forwarded.take_back().await;
            let before = forwarded.answers.invokers.lock().unwrap().len();
            let refused = forwarded
                .client
                .call("tilo_do", &json!({"path": "notes/Notebook#pin"}));
            let why = refused_by(&refused);
            assert!(
                why.contains("Accept agents from my other devices"),
                "the refusal names the setting: {why}"
            );
            assert_eq!(
                forwarded.answers.invokers.lock().unwrap().len(),
                before,
                "nothing it asked since reached the host"
            );
            forwarded.finish().await;
        });
    }
}

/// A runtime for one phase: the forward's session and the host's peer session live on it.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}
