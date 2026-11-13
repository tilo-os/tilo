//! A person's device hosts a workspace with the same host a relay runs, and a
//! key of theirs uses it with the same calls it uses on a relay: it opens a
//! session with the device, lists their workspaces there, enters the
//! workspace (the host invites it on the session and it redeems there), and
//! runs a record action addressed to the host's own device, which the
//! device's own action broker answers in a run of the key's
//! ([`tilo_runtime::key_runs`]).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the host lives until it ends"
)]
tilo_automerge_isolate::test_worker!();

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ReceiptStatus,
};
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::{PersonalStore, PersonalSync};
use tilo_room_host::host::{HostConfig, HostIdentity, WorkspaceHost};
use tilo_runtime::enter::Entered;
use tilo_runtime::key_runs::{KeyRuns, RunHost, Runs};
use tilo_runtime::remote_door::PersonalDevices;
use tilo_session_client::PeerConfig;
use tilo_session_client::device::Device;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

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

impl ActionHost for Broker {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.callers.lock().unwrap().push(caller.clone());
        let page = match query.provider.as_deref() {
            None => CataloguePage {
                revision: "1".into(),
                providers: vec![CatalogueProvider {
                    provider: "1".into(),
                    app_id: NOTES.into(),
                    workspace: self.workspace.clone(),
                    record_id: String::new(),
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
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
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
    ) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Ok(()))
    }
}

/// The device's runs: one workspace, and the keys a run was opened for.
struct DeviceRuns {
    workspace: String,
    broker: Broker,
    opened: Mutex<Vec<DeviceCert>>,
}

impl Runs for DeviceRuns {
    fn workspace(&self, workspace_id: WorkspaceId) -> Option<String> {
        (tilo_desktop_model::WorkspaceId::parse(&self.workspace) == Some(workspace_id))
            .then(|| self.workspace.clone())
    }

    fn open(&self, workspace: &str, key: &DeviceCert) -> Result<(RunHost, Caller), String> {
        if workspace != self.workspace {
            return Err(format!("no workspace {workspace} here"));
        }
        self.opened.lock().unwrap().push(key.clone());
        let caller = Caller::local_person("ada")
            .delegated_to(tilo_runtime::remote_door::agent_id("agent", key));
        Ok((Arc::new(self.broker.clone()), caller))
    }
}

/// The person's device, hosting a workspace for them on its endpoint.
struct Box_ {
    host: Arc<WorkspaceHost>,
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
    let key_runs = KeyRuns::new();
    let host = Arc::new(
        WorkspaceHost::open(
            HostConfig {
                apps: Some(
                    Arc::clone(&key_runs) as Arc<dyn tilo_room_host::app_serving::AppServingPlane>
                ),
                ..HostConfig::new(
                    state.path().join("room-host"),
                    tilo_relay_protocol::WorkSetting::none(),
                )
            },
            HostIdentity {
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
    let made = host.workspaces().create("Desk").await.unwrap();
    let store = PersonalStore::open(
        root.verifying_key().to_bytes(),
        cert.clone(),
        SystemTime::now(),
    )
    .unwrap();
    let runs = Arc::new(DeviceRuns {
        workspace: made.workspace.clone(),
        broker: Broker {
            workspace: made.workspace.clone(),
            ..Broker::default()
        },
        opened: Mutex::default(),
    });
    key_runs.serve(
        Arc::new(PersonalDevices::new(PersonalSync::new(store, key))),
        Arc::clone(&runs) as Arc<dyn Runs>,
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
            host: tilo_relay_client::Host::at(addr(&device.endpoint)),
            cert: key.1.clone(),
            host_cert: device.cert.clone(),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
    let listed = key.personal_list().await.unwrap();
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
    let entered = Entered::on_host(key, workspace_id, &device.workspace, "Visa backend #1");
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
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
    assert!(
        key.enter_workspace(&device.workspace, "elsewhere")
            .await
            .is_err()
    );
    assert!(device.runs.opened.lock().unwrap().is_empty());
}
