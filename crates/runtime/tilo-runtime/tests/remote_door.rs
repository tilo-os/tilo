//! An agent on one of the person's devices works on another of them over iroh:
//! its client asks its own host to carry it to that device's agent door, the
//! door decides from the transport and the person's own store whether the
//! dialing device is one of theirs, and an admitted agent is served there as an
//! agent delegated by the person, named after the device it came from. Every
//! other device — another person's, a revoked one, a store node, one showing a
//! certificate that is not its own — is refused with the reason before
//! anything it sends is read.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueProvider,
    CatalogueQuery, CatalogueResource, CatalogueRow, InvocationReceipt, InvocationState,
    InvokeRequest, Pending, ReceiptStatus,
};
use tilo_peer_state::{PersonalStore, PersonalSync};
use tilo_runtime::remote_door::{
    ALPN, Arrival, Arrivals, Doors, Forward, Origin, PersonalDevices, RemoteDoor, agent_id,
    ask_forward, forward_request, open, read_list,
};
use tilo_runtime::session_door::{Beside, Consent, read_first_line, serve_with};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason, sign_device_cert,
    sign_revocation_entry,
};

/// How long a step may take before the test calls it lost.
const PATIENCE: Duration = Duration::from_secs(30);

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

/// The person's own store on the device `own`, holding `others` and the
/// revocations `revoked`.
fn store(
    root: &SigningKey,
    own: &(SigningKey, DeviceCert),
    others: &[&DeviceCert],
    revoked: &[&DeviceCert],
) -> PersonalSync {
    let store = PersonalStore::open(
        root.verifying_key().to_bytes(),
        own.1.clone(),
        SystemTime::now(),
    )
    .unwrap();
    let sync = PersonalSync::new(store, own.0.clone());
    sync.admit(|store, now| {
        for other in others {
            store.admit_device(other, now)?;
        }
        for device in revoked {
            let mut entry = RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: device.device_public_key,
                revoked_at_ns: 1,
                reason: RevocationReason::Lost,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, root).unwrap();
            store.revoke(entry)?;
        }
        Ok(true)
    })
    .unwrap();
    sync
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

impl ActionHost for Host {
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
                    workspace: "workspace".into(),
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

/// B's agent door: each connection its endpoint opens is served as the
/// desktop's door serves one, admitted as the agent the device it came from
/// names, for the person.
#[derive(Clone, Default)]
struct Door {
    host: Host,
    arrived: Arc<Mutex<Vec<Origin>>>,
    /// Each connection's serving thread, which ends when it does.
    served: Arc<Mutex<Vec<std::thread::JoinHandle<std::io::Result<()>>>>>,
    /// The person's workspace list, as B holds it.
    list: Arc<Mutex<Option<tilo_relay_session::workspaces::PersonalList>>>,
}

impl Arrivals for Door {
    fn arrive(&self, arrival: Arrival) -> Result<(), String> {
        let Arrival { origin, stream } = arrival;
        self.arrived.lock().unwrap().push(origin.clone());
        let host = self.host.clone();
        let serving = std::thread::spawn(move || {
            serve_with(
                stream,
                None,
                || Ok(host),
                Beside::default(),
                |hello, _connected| Consent::Admit {
                    workspace: "workspace".into(),
                    agent: agent_id("claude-code", &origin.device),
                    person: if hello.client_name == "Claude Code" {
                        "ada".into()
                    } else {
                        String::new()
                    },
                },
            )
        });
        self.served.lock().unwrap().push(serving);
        Ok(())
    }

    fn listed(&self) -> Option<tilo_relay_session::workspaces::PersonalList> {
        self.list.lock().unwrap().clone()
    }
}

/// The person (root) and their devices: A, where the agent runs, and B, the
/// box it works on.
struct Person {
    root: SigningKey,
    a: (SigningKey, DeviceCert),
    b: (SigningKey, DeviceCert),
}

impl Person {
    fn new() -> Self {
        let root = SigningKey::from_bytes(&[0x11; 32]);
        let a = device(&root, 1, "Studio laptop");
        let b = device(&root, 2, "Build box");
        Self { root, a, b }
    }
}

/// B, serving its door over its person's store, which holds the
/// revocations `revoked`.
struct B {
    endpoint: Endpoint,
    door: Door,
    _router: Router,
}

async fn b(person: &Person, revoked: &[&DeviceCert]) -> B {
    let endpoint = endpoint(&person.b.0).await;
    let door = Door::default();
    let devices = Arc::new(PersonalDevices::new(store(
        &person.root,
        &person.b,
        &[&person.a.1],
        revoked,
    )));
    let router = Router::builder(endpoint.clone())
        .accept(ALPN, RemoteDoor::new(devices, Arc::new(door.clone())))
        .spawn();
    B {
        endpoint,
        door,
        _router: router,
    }
}

/// A's host, reaching B's door from A's endpoint as A, and the person's
/// devices as A's store holds them: B, and `others`.
async fn a(person: &Person, b: &B, others: &[&DeviceCert]) -> Arc<Doors> {
    let endpoint = endpoint(&person.a.0).await;
    let mut known = vec![&person.b.1];
    known.extend_from_slice(others);
    let doors = Doors::new(
        endpoint,
        Arc::new(PersonalDevices::new(store(
            &person.root,
            &person.a,
            &known,
            &[],
        ))),
        tokio::runtime::Handle::current(),
    );
    doors.learn(addr(&b.endpoint));
    Arc::new(doors)
}

/// A client of A's host asking to be carried to `device`, as
/// `tilo-runtime-mcp --connect --device` does: A's host reads the line it
/// opens with and carries it on, as its agent door does.
fn connect(
    doors: &Arc<Doors>,
    device: &str,
) -> (std::io::Result<UnixStream>, std::thread::JoinHandle<()>) {
    let (mut client, host_end) = UnixStream::pair().unwrap();
    let doors = Arc::clone(doors);
    let carrying = std::thread::spawn(move || {
        let mut host_end = host_end;
        let first = read_first_line(&mut host_end).unwrap();
        let forward = forward_request(&first).expect("the client asks for a forward");
        doors.serve_forward(host_end, &forward);
    });
    client.set_read_timeout(Some(PATIENCE)).unwrap();
    let asked = ask_forward(
        &mut client,
        &Forward {
            device: device.into(),
            workspace: None,
        },
    )
    .map(|()| client);
    (asked, carrying)
}

/// Send `request` and read its answer.
fn call(client: &mut BufReader<UnixStream>, request: &Value) -> Value {
    client
        .get_mut()
        .write_all(format!("{request}\n").as_bytes())
        .unwrap();
    let mut line = String::new();
    client.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|_| panic!("an answer, not {line:?}"))
}

fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":"2025-06-18","capabilities":{},
        "clientInfo":{"name":"Claude Code","version":"2"}}})
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

/// Wait on a blocking step from the test's runtime.
async fn blocking<T: Send + 'static>(step: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::time::timeout(PATIENCE, tokio::task::spawn_blocking(step))
        .await
        .expect("the step finishes")
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_on_one_device_lists_and_acts_on_another_as_an_agent_via_the_first() {
    let person = Person::new();
    let b = b(&person, &[]).await;
    let doors = a(&person, &b, &[]).await;
    let expected = agent_id("claude-code", &person.a.1);
    assert!(
        expected.starts_with("claude-code@studio-laptop-"),
        "the agent is named after the device it came from: {expected}"
    );
    let (answers, carried) = {
        let doors = Arc::clone(&doors);
        blocking(move || {
            let (client, carrying) = connect(&doors, "build box");
            let mut client = BufReader::new(client.expect("B's door opens for A"));
            let introduced = call(&mut client, &initialize());
            let apps = call(&mut client, &tool(2, "tilo_ls", &json!({})));
            let done = call(
                &mut client,
                &tool(3, "tilo_do", &json!({"path": "notes/Notebook#pin"})),
            );
            (vec![introduced, apps, done], (client, carrying))
        })
        .await
    };
    assert!(
        answers[0].pointer("/result/serverInfo").is_some(),
        "B answers the agent's initialize: {}",
        answers[0]
    );
    assert_eq!(answers[1]["result"]["isError"], false, "{}", answers[1]);
    assert!(
        text(&answers[1]).contains("notes"),
        "the agent lists the apps B runs: {}",
        answers[1]
    );
    assert_eq!(answers[2]["result"]["isError"], false, "{}", answers[2]);
    let invoked = b.door.host.invoked.lock().unwrap().clone();
    assert_eq!(invoked.len(), 1, "the action ran on B once");
    assert_eq!(invoked[0].target.address.action, "pin");
    let arrived = b.door.arrived.lock().unwrap().clone();
    assert_eq!(arrived.len(), 1);
    assert_eq!(
        arrived[0].device, person.a.1,
        "B knows the connection came from A"
    );
    let callers = b.door.host.callers.lock().unwrap().clone();
    assert!(!callers.is_empty());
    for caller in &callers {
        assert_eq!(
            caller.principal(),
            &CallerPrincipal::Agent(expected.clone()),
            "everything the agent asked of B is the agent's, via A"
        );
        assert_eq!(caller.workspace(), Some("workspace"));
    }

    // The agent leaving ends its connection on B, and on A.
    let (client, carrying) = carried;
    drop(client);
    blocking(move || carrying.join().unwrap()).await;
    let served = b.door.served.lock().unwrap().pop().unwrap();
    blocking(move || served.join().unwrap().unwrap()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_b_refuses_reaches_the_agent_and_ends_its_connection_on_a() {
    let person = Person::new();
    let b = b(&person, &[]).await;
    let doors = a(&person, &b, &[]).await;
    // B named by its endpoint id rather than its name.
    let b_id = hex::encode(person.b.1.device_public_key);
    let (said, carrying) = {
        let doors = Arc::clone(&doors);
        blocking(move || {
            let (client, carrying) = connect(&doors, &b_id);
            let mut client = client.expect("B's door opens for A");
            // A tool call before `initialize`: B's door answers it with a
            // refusal and closes, and the agent on A reads to the end.
            client
                .write_all(format!("{}\n", tool(9, "tilo_ls", &json!({}))).as_bytes())
                .unwrap();
            let mut said = String::new();
            client.read_to_string(&mut said).unwrap();
            (said, carrying)
        })
        .await
    };
    assert!(said.contains("initialize"), "{said}");
    blocking(move || carrying.join().unwrap()).await;
    assert!(
        b.door.host.callers.lock().unwrap().is_empty(),
        "nothing the agent asked before it was admitted reached B's host"
    );
}

/// Dial B's door from `key`'s endpoint showing `certificate`, and read what
/// it says.
async fn dial(b: &B, key: &SigningKey, certificate: &DeviceCert) -> Result<(), String> {
    let endpoint = endpoint(key).await;
    let dialed = open(&endpoint, addr(&b.endpoint), certificate, None, false).await;
    dialed.map(tilo_runtime::remote_door::Dialed::close)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_of_another_person_is_refused_before_anyone_is_asked() {
    let person = Person::new();
    let b = b(&person, &[]).await;
    let stranger = SigningKey::from_bytes(&[0x22; 32]);
    let theirs = device(&stranger, 3, "Their laptop");
    let refused = dial(&b, &theirs.0, &theirs.1).await.unwrap_err();
    assert!(refused.contains("someone else"), "{refused}");
    assert!(b.door.arrived.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_device_is_refused() {
    let person = Person::new();
    let lost = device(&person.root, 4, "Lost phone");
    let b = b(&person, &[&lost.1]).await;
    let refused = dial(&b, &lost.0, &lost.1).await.unwrap_err();
    assert!(refused.contains("removed"), "{refused}");
    assert!(b.door.arrived.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_showing_another_devices_certificate_is_refused() {
    let person = Person::new();
    let b = b(&person, &[]).await;
    // The person's own certificate for A, shown by a device that is not A.
    let impostor = SigningKey::from_bytes(&[0x33; 32]);
    let refused = dial(&b, &impostor, &person.a.1).await.unwrap_err();
    assert!(refused.contains("another device"), "{refused}");
    assert!(b.door.arrived.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_store_node_is_refused() {
    let person = Person::new();
    let b = b(&person, &[]).await;
    let (key, mut node) = device(&person.root, 5, "Home server");
    node.capabilities.can_act_as_host = true;
    sign_device_cert(&mut node, &person.root).unwrap();
    let refused = dial(&b, &key, &node).await.unwrap_err();
    assert!(refused.contains("store node"), "{refused}");
    assert!(b.door.arrived.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_persons_devices_are_listed_with_whether_each_answers() {
    let person = Person::new();
    let b = b(&person, &[]).await;
    // A third device of the person's, which A knows of and which is away.
    let away = device(&person.root, 6, "Phone");
    let doors = a(&person, &b, &[&away.1]).await;
    let reach = doors.reach_now().await;
    let state = |label: &str| {
        reach
            .iter()
            .find(|reach| reach.device.device_label == label)
            .unwrap_or_else(|| panic!("{label} is listed: {reach:?}"))
            .clone()
    };
    assert!(state("Studio laptop").this);
    assert_eq!(state("Build box").answer, Ok(()));
    assert!(state("Phone").answer.is_err());
    assert!(
        b.door.arrived.lock().unwrap().is_empty(),
        "asking whether a device answers admits nothing there"
    );
    let listing = tilo_runtime::remote_door::listing(&reach);
    assert!(
        listing.contains("Build box") && listing.contains("reachable"),
        "{listing}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_the_person_has_none_of_is_named_back_with_theirs() {
    let person = Person::new();
    let b = b(&person, &[]).await;
    let doors = a(&person, &b, &[]).await;
    let refused = {
        let doors = Arc::clone(&doors);
        blocking(move || {
            let (asked, carrying) = connect(&doors, "kitchen");
            carrying.join().unwrap();
            asked.map(|_| ()).unwrap_err().to_string()
        })
        .await
    };
    assert!(
        refused.contains("kitchen") && refused.contains("Build box"),
        "{refused}"
    );
    let this = {
        let doors = Arc::clone(&doors);
        blocking(move || {
            let (asked, carrying) = connect(&doors, "Studio laptop");
            carrying.join().unwrap();
            asked.map(|_| ()).unwrap_err().to_string()
        })
        .await
    };
    assert!(this.contains("this device"), "{this}");
    assert!(b.door.arrived.lock().unwrap().is_empty());
}

/// A grant of the person `root`'s for an agent, reaching `scope`, keyed `n`.
fn grant(
    root: &SigningKey,
    n: u8,
    scope: tilo_user_identity::AgentScope,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "an agent".into(),
        capabilities: DeviceCapabilities::agent_grant(scope),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

const DESK: &str = "11111111-1111-4111-8111-111111111111";
const ON_RELAY: &str = "22222222-2222-4222-8222-222222222222";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_of_the_persons_reads_their_workspace_list_from_a_device_and_no_one_else_does() {
    use tilo_relay_session::workspaces::{PersonalList, PersonalWorkspace, WorkspaceHostAddress};
    let person = Person::new();
    let b = b(&person, &[]).await;
    let all = grant(
        &person.root,
        5,
        tilo_user_identity::AgentScope::AllWorkspaces,
    );
    // Before B holds a list, it says so.
    let dialing = endpoint(&all.0).await;
    let refused = read_list(&dialing, addr(&b.endpoint), &all.1)
        .await
        .unwrap_err();
    assert!(refused.contains("no workspace list"), "{refused}");
    let entry = |workspace: &str, host: &str| PersonalWorkspace {
        workspace: workspace.into(),
        name: workspace.into(),
        host: Some(WorkspaceHostAddress {
            name: host.into(),
            hint: Some("00".into()),
        }),
        role: Some("owner".into()),
    };
    let held = PersonalList {
        workspaces: vec![entry(DESK, "Build box"), entry(ON_RELAY, "eu1.tilo.sh")],
        ..PersonalList::default()
    };
    *b.door.list.lock().unwrap() = Some(held.clone());
    // A key reaching all their workspaces reads all of them, each with its
    // host; nobody is asked, and nothing is admitted.
    assert_eq!(
        read_list(&dialing, addr(&b.endpoint), &all.1)
            .await
            .unwrap(),
        held
    );
    assert!(b.door.arrived.lock().unwrap().is_empty());
    // A key for one workspace reads that one.
    let one = grant(
        &person.root,
        6,
        tilo_user_identity::AgentScope::parse(DESK).unwrap(),
    );
    let listed = read_list(&endpoint(&one.0).await, addr(&b.endpoint), &one.1)
        .await
        .unwrap();
    assert_eq!(
        listed
            .workspaces
            .iter()
            .map(|one| one.workspace.as_str())
            .collect::<Vec<_>>(),
        [DESK]
    );
    // Someone else's key reads nothing.
    let stranger = SigningKey::from_bytes(&[0x33; 32]);
    let theirs = grant(&stranger, 7, tilo_user_identity::AgentScope::AllWorkspaces);
    let refused = read_list(&endpoint(&theirs.0).await, addr(&b.endpoint), &theirs.1)
        .await
        .unwrap_err();
    assert!(refused.contains("someone else"), "{refused}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_key_reads_no_workspace_list() {
    let person = Person::new();
    let revoked = grant(
        &person.root,
        8,
        tilo_user_identity::AgentScope::AllWorkspaces,
    );
    let b = b(&person, &[&revoked.1]).await;
    *b.door.list.lock().unwrap() = Some(tilo_relay_session::workspaces::PersonalList::default());
    let refused = read_list(&endpoint(&revoked.0).await, addr(&b.endpoint), &revoked.1)
        .await
        .unwrap_err();
    assert!(refused.contains("removed"), "{refused}");
}

/// B's own signed address for `workspace`, as the person's list carries it.
fn address_on_b(
    person: &Person,
    b: &B,
    workspace: &str,
) -> tilo_relay_session::workspaces::WorkspaceHostAddress {
    let hint = tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1 {
        version: 1,
        mode: tilo_delivery_protocol::workspace_join::WorkspaceHostMode::NativeInteractiveWorkspaceOwner,
        workspace_id: tilo_peer_state::join::room_workspace_id(workspace).unwrap(),
        room: workspace.to_owned(),
        endpoint_addr: addr(&b.endpoint),
        host_user_public_key: person.b.1.user_public_key,
        host_device_certificate: person.b.1.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(&person.b.0)
    .unwrap();
    tilo_relay_session::workspaces::WorkspaceHostAddress::of_hint(
        &postcard::to_stdvec(&hint).unwrap(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_enters_a_workspace_its_persons_device_hosts_at_the_address_their_list_names() {
    use tilo_runtime::enter::{Key, enter};
    let person = Person::new();
    let b = b(&person, &[]).await;
    let all = grant(
        &person.root,
        9,
        tilo_user_identity::AgentScope::AllWorkspaces,
    );
    let key = Key {
        key: all.0.clone(),
        cert: all.1.clone(),
    };
    let address = address_on_b(&person, &b, DESK);
    assert_eq!(
        address.name, "Build box",
        "the list names the device that hosts it"
    );
    let dialing = endpoint(&all.0).await;
    let entered = enter(&dialing, &key, DESK, &address, "Visa backend #1")
        .await
        .expect("the person's device lets their key in");
    let introduced = entered.handle(initialize()).await.unwrap();
    assert!(
        introduced.pointer("/result/serverInfo").is_some(),
        "{introduced}"
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
    assert_eq!(b.door.host.invoked.lock().unwrap().len(), 1, "it ran on B");
    let arrived = b.door.arrived.lock().unwrap().clone();
    assert_eq!(arrived.len(), 1);
    assert_eq!(arrived[0].device, all.1, "B knows which key came in");
    assert_eq!(arrived[0].workspace.as_deref(), Some(DESK));

    // An address names one workspace: another's is not entered with it.
    assert!(
        enter(&dialing, &key, ON_RELAY, &address, "Visa backend #1")
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_for_another_workspace_or_a_revoked_key_is_refused_at_the_device() {
    use tilo_runtime::enter::{Key, enter};
    let person = Person::new();
    let revoked = grant(
        &person.root,
        10,
        tilo_user_identity::AgentScope::AllWorkspaces,
    );
    let b = b(&person, &[&revoked.1]).await;
    // A key for one workspace does not reach another the same device hosts.
    let elsewhere = grant(
        &person.root,
        11,
        tilo_user_identity::AgentScope::parse(ON_RELAY).unwrap(),
    );
    let refused = enter(
        &endpoint(&elsewhere.0).await,
        &Key {
            key: elsewhere.0.clone(),
            cert: elsewhere.1.clone(),
        },
        DESK,
        &address_on_b(&person, &b, DESK),
        "elsewhere",
    )
    .await
    .err()
    .expect("refused");
    assert!(refused.contains("reaches only"), "{refused}");
    // A key the person revoked is refused.
    let refused = enter(
        &endpoint(&revoked.0).await,
        &Key {
            key: revoked.0.clone(),
            cert: revoked.1.clone(),
        },
        DESK,
        &address_on_b(&person, &b, DESK),
        "revoked",
    )
    .await
    .err()
    .expect("refused");
    assert!(refused.contains("removed"), "{refused}");
    assert!(
        b.door.arrived.lock().unwrap().is_empty(),
        "nobody was asked"
    );
}
