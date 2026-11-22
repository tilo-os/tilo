//! An agent on one of the person's devices works in a workspace another of
//! their devices keeps, over the one action frame between devices: the first
//! device asks the second over its action service (`tilo-actions/1`), one
//! signed request per stream, and the agent's MCP is only the projection on the
//! asking device ([`McpServer`] over a [`RemoteDevice`]). The device that keeps
//! the workspace decides each request from its member list as it stands: a
//! device of the person's in it lists and acts there, and a device of
//! somebody else's, one of the person's that the list does not hold, or one
//! the person revoked is refused with the reason, before its request reaches
//! the actions there.

use std::sync::{Arc, Mutex};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueProvider, CatalogueQuery,
    CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState, InvokeRequest, Pending,
    ReceiptStatus,
};
use tilo_mcp::McpServer;
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, NoActions, RemoteDevice};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::{SeedState, StatePeer, seed_workspace};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};

/// The workspace B keeps, which the agent on A works in.
const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

/// The app B runs, which an agent lists and acts on.
const NOTES: &str = "so.tilo.notes";

/// A device key `n` certified by `root` as one of its person's devices,
/// called `label`.
fn device(root: &SigningKey, n: u8, label: &str) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
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

/// The workspace, made by `maker` with `members` in it, as the device that
/// keeps it holds it.
fn workspace(maker: &(SigningKey, DeviceCert), members: &[DeviceCert]) -> StatePeer {
    let now = tilo_clock::wall();
    let mut peer = StatePeer::new(
        WORKSPACE,
        maker.1.clone(),
        members.to_vec(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_exact_admissions().unwrap();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &maker.1, members).unwrap(),
            documents: Vec::new(),
        },
        &maker.0,
        now,
    )
    .unwrap();
    peer
}

/// B's action host: Notes runs there, its notebook offering one action, and
/// every caller that asked anything is kept.
#[derive(Clone, Default)]
struct Host {
    callers: Arc<Mutex<Vec<Caller>>>,
    invoked: Arc<Mutex<Vec<InvokeRequest>>>,
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

impl ActionProvider for Host {
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
                    workspace: WORKSPACE.into(),
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

/// B: the device that keeps the workspace, answering its actions on its
/// action service from its host.
struct B {
    key: (SigningKey, DeviceCert),
    workspace: Replica,
    host: Host,
    endpoint: Endpoint,
    _router: Router,
}

impl B {
    async fn keeping(key: (SigningKey, DeviceCert), members: &[DeviceCert]) -> Self {
        let kept = Replica::new(workspace(&key, members));
        let host = Host::default();
        let endpoint = endpoint(&key.0).await;
        let service = ActionService::new(key.0.clone(), Arc::new(host.clone()));
        service.hold(kept.clone());
        let router = Router::builder(endpoint.clone())
            .accept(ACTIONS_ALPN, service)
            .spawn();
        Self {
            key,
            workspace: kept,
            host,
            endpoint,
            _router: router,
        }
    }

    /// The person revokes `device`, and B's copy of the workspace holds it.
    fn revoke(&self, root: &SigningKey, device: &DeviceCert) {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device.device_public_key,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        let now = tilo_clock::wall();
        assert!(
            self.workspace
                .write(|peer| peer.governance_mut().publish_governed_revocations(
                    &[entry],
                    &self.key.0,
                    now
                ))
                .unwrap()
        );
    }

    /// How many requests reached B's actions.
    fn reached(&self) -> usize {
        self.host.callers.lock().unwrap().len()
    }
}

/// The agent's MCP on the device `key`: the projection of B's actions in the
/// workspace, asked of B over its action service as `key`.
async fn agent_on(key: &(SigningKey, DeviceCert), b: &B) -> (McpServer<RemoteDevice>, Endpoint) {
    let endpoint = endpoint(&key.0).await;
    let remote = ActionService::new(key.0.clone(), Arc::new(NoActions)).remote(
        &endpoint,
        &addr(&b.endpoint),
        WORKSPACE,
    );
    let person = hex::encode(key.1.user_public_key);
    (
        McpServer::for_agent(remote, WORKSPACE.to_owned(), "agent", &person),
        endpoint,
    )
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

/// The person (root) and their devices: A, where the agent runs, B, the box
/// that keeps the workspace, and C, a device of theirs the workspace does not
/// hold. And a device of somebody else's.
struct Person {
    root: SigningKey,
    a: (SigningKey, DeviceCert),
    b: (SigningKey, DeviceCert),
    c: (SigningKey, DeviceCert),
    stranger: (SigningKey, DeviceCert),
}

impl Person {
    fn new() -> Self {
        let root = SigningKey::from_bytes(&[0x11; 32]);
        Self {
            a: device(&root, 1, "Studio laptop"),
            b: device(&root, 2, "Build box"),
            c: device(&root, 3, "Kitchen tablet"),
            stranger: device(&SigningKey::from_bytes(&[0x22; 32]), 4, "Their laptop"),
            root,
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_one_device_lists_and_acts_in_a_workspace_another_device_keeps() {
    let person = Person::new();
    let b = B::keeping(person.b.clone(), &[person.a.1.clone(), person.b.1.clone()]).await;
    let (agent, a_endpoint) = agent_on(&person.a, &b).await;

    // It lists B's apps through its own MCP, from B's actions.
    let apps = agent.handle(tool(2, "tilo_ls", &json!({}))).await.unwrap();
    assert_eq!(apps["result"]["isError"], false, "{apps}");
    assert!(text(&apps).contains("notes"), "{apps}");

    // It acts there: B runs the action, for the person at A.
    let done = agent
        .handle(tool(3, "tilo_do", &json!({"path": "notes/Notebook#pin"})))
        .await
        .unwrap();
    assert_eq!(done["result"]["isError"], false, "{done}");
    let invoked = b.host.invoked.lock().unwrap().clone();
    assert_eq!(invoked.len(), 1, "it ran on B: {invoked:?}");
    assert_eq!(invoked[0].target.address.action, "pin");
    let callers = b.host.callers.lock().unwrap().clone();
    assert!(
        callers.iter().all(|caller| {
            caller.user_public_key() == Some(&person.root.verifying_key().to_bytes())
                && caller.device_public_key() == Some(&person.a.1.device_public_key)
                && caller.workspace() == Some(WORKSPACE)
        }),
        "B answered the person at A, in the workspace it named: {callers:?}"
    );
    a_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_the_workspace_does_not_hold_is_refused_before_anything_runs() {
    let person = Person::new();
    let b = B::keeping(person.b.clone(), &[person.a.1.clone(), person.b.1.clone()]).await;

    // A device of somebody else's, and one of the person's the workspace does not hold: each is
    // told it may not act there, and nothing it asked reaches B's actions.
    for outside in [&person.stranger, &person.c] {
        let (agent, endpoint) = agent_on(outside, &b).await;
        let refused = agent
            .handle(tool(2, "tilo_do", &json!({"path": "notes/Notebook#pin"})))
            .await
            .unwrap();
        assert_ne!(refused["result"]["isError"], false, "{refused}");
        let remote = ActionService::new(outside.0.clone(), Arc::new(NoActions)).remote(
            &endpoint,
            &addr(&b.endpoint),
            WORKSPACE,
        );
        let caller = Caller::authenticated(outside.1.user_public_key, outside.1.device_public_key);
        let answer = ActionProvider::query(&remote, &caller, CatalogueQuery::default())
            .await
            .expect("B answers");
        assert!(
            matches!(answer, Err(ActionCallError::NotPermitted)),
            "{answer:?}"
        );
        endpoint.close().await;
    }
    assert_eq!(b.reached(), 0, "nothing from outside reached B's actions");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_the_person_revoked_is_refused_from_then_on() {
    let person = Person::new();
    let b = B::keeping(person.b.clone(), &[person.a.1.clone(), person.b.1.clone()]).await;
    let (agent, a_endpoint) = agent_on(&person.a, &b).await;
    let apps = agent.handle(tool(2, "tilo_ls", &json!({}))).await.unwrap();
    assert_eq!(apps["result"]["isError"], false, "{apps}");
    let before = b.reached();

    b.revoke(&person.root, &person.a.1);
    let refused = agent
        .handle(tool(3, "tilo_do", &json!({"path": "notes/Notebook#pin"})))
        .await
        .unwrap();
    assert_ne!(refused["result"]["isError"], false, "{refused}");
    assert!(
        refused.to_string().contains("revoked"),
        "it is told why: {refused}"
    );
    assert_eq!(
        b.reached(),
        before,
        "nothing it asked since reached B's actions"
    );
    assert!(b.host.invoked.lock().unwrap().is_empty());
    a_endpoint.close().await;
}
