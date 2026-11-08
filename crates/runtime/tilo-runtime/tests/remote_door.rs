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
    ask_forward, forward_request, open,
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
