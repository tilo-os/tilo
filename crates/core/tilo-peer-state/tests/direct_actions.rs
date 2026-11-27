//! One device asks another device of its workspace for an action directly
//! over iroh, with no workspace host, relay or network actor in between: the
//! request reaches the device it names, is answered there as the caller the
//! engine authorized, and is refused from the moment the asking device may
//! no longer act.

use crate::common::{addr, device, device_with, localhost_endpoint, workspace};
use serde_json::Value;
use tilo_peer_state::join::WorkspaceId;

use ed25519_dalek::SigningKey;
use iroh::endpoint::RecvStream;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr};
use neighbour_wire::actions::{ActionServiceFrame, DeviceAction, SignedDeviceAction};
use std::sync::Arc;
use std::time::Duration;
use tilo_actions::remote::TestProvider;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget, ResourceReads,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, Bounds, RemoteDevice};
use tilo_peer_state::direct::Replica;

use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationEntry, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};
use tilo_workspace_join::delegation::{Delegate, DelegateScope, Delegation};
use web_time::SystemTime;

const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const ELSEWHERE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

/// How long a test waits for an answer before it counts as lost.
const PATIENCE: Duration = Duration::from_secs(20);

/// A device `root` delegates to `agent`: its own key, and a certificate that
/// holds none of the person's capabilities.
fn agent_device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    let (key, mut cert) = device(root, n);
    cert.device_label = format!("Agent: Scribe {n}");
    cert.capabilities = DeviceCapabilities::nothing();
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

use tilo_peer_state::fixture::ns;

/// The moment this test reads, the clock the engines read with it: a test
/// that holds the wall clock holds both sides of every exchange.
fn now_ns() -> u64 {
    ns(tilo_clock::wall())
}

/// One invocation of an action, as a caller on another device sends it.
fn invoke(invocation: InvocationId) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: "test.provider".into(),
            address: ActionAddress::new("note:1", "insert"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn catalogue() -> RemoteActionMessage {
    RemoteActionMessage::CatalogueRequest {
        correlation: 1,
        query: CatalogueQuery::default(),
    }
}

/// The device that answers requests: its workspace replica, the action service that
/// serves actions out of it, and the dispatcher the action service hands them to.
struct Engine {
    key: SigningKey,
    replica: Replica,
    endpoint: Endpoint,
    router: Router,
    /// The action service the router answers with, which the test reaches
    /// devices through ([`ActionService::reach_device_at`]).
    service: ActionService,
}

impl Engine {
    async fn serving(
        replica: Replica,
        key: SigningKey,
        dispatcher: Arc<TestProvider>,
        bounds: Bounds,
    ) -> Self {
        Self::serving_provider(replica, key, dispatcher, bounds).await
    }

    /// [`Self::serving`] with any provider as the dispatcher.
    async fn serving_provider(
        replica: Replica,
        key: SigningKey,
        dispatcher: Arc<dyn ActionProvider + Send + Sync>,
        bounds: Bounds,
    ) -> Self {
        let endpoint = localhost_endpoint(&key).await;
        let action_service = ActionService::with_bounds(key.clone(), dispatcher.clone(), bounds);
        action_service.hold(replica.clone());
        let router = Router::builder(endpoint.clone())
            .accept(ACTIONS_ALPN, action_service.clone())
            .spawn();
        Self {
            key,
            replica,
            endpoint,
            router,
            service: action_service,
        }
    }

    fn addr(&self) -> EndpointAddr {
        addr(&self.endpoint)
    }

    fn device(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    async fn close(self) {
        self.router.shutdown().await.unwrap();
        self.endpoint.close().await;
    }
}

/// A device of `key` that only asks: its own action service, with nothing to answer.
async fn asking(key: &SigningKey) -> (Endpoint, ActionService) {
    let endpoint = localhost_endpoint(key).await;
    let action_service = ActionService::new(key.clone(), Arc::new(TestProvider::new()));
    (endpoint, action_service)
}

/// One signed request written to `addr` by hand, and the one answer that
/// comes back: what the action service itself never writes, a test sends itself.
async fn send(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    signed: &SignedDeviceAction,
) -> Option<RemoteActionMessage> {
    let connection = endpoint.connect(addr, ACTIONS_ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let bytes =
        postcard::to_allocvec(&ActionServiceFrame::Action(Box::new(signed.clone()))).unwrap();
    send.write_all(&u32::try_from(bytes.len()).unwrap().to_le_bytes())
        .await
        .unwrap();
    send.write_all(&bytes).await.unwrap();
    send.finish().unwrap();
    let answer = read_message(&mut recv).await;
    connection.close(0_u32.into(), b"done");
    answer
}

async fn read_message(recv: &mut RecvStream) -> Option<RemoteActionMessage> {
    let mut len = [0_u8; 4];
    if recv.read_exact(&mut len).await.is_err() {
        return None;
    }
    let mut bytes = vec![0_u8; usize::try_from(u32::from_le_bytes(len)).unwrap()];
    recv.read_exact(&mut bytes).await.unwrap();
    Some(
        tilo_actions::remote::RemoteActionFrame::decode(&bytes)
            .unwrap()
            .message,
    )
}

/// What `remote` answered a catalogue query as `caller`.
async fn queried(remote: &RemoteDevice, caller: &Caller) -> Result<(), ActionCallError> {
    let answer = tokio::time::timeout(
        PATIENCE,
        ActionProvider::query(remote, caller, CatalogueQuery::default()),
    )
    .await
    .expect("the device answers");
    answer.expect("the asker is not dropped").map(|_page| ())
}

/// What `remote` answered an invocation as `caller`.
async fn invoked(
    remote: &RemoteDevice,
    caller: &Caller,
    invocation: InvocationId,
) -> Result<ReceiptStatus, ActionCallError> {
    let answer = tokio::time::timeout(
        PATIENCE,
        ActionProvider::invoke(remote, caller, invoke(invocation)),
    )
    .await
    .expect("the device answers");
    answer
        .expect("the asker is not dropped")
        .map(|receipt| receipt.status)
}

/// The reason a refusal carries, for a test to read.
fn refusal(error: &ActionCallError) -> &str {
    match error {
        ActionCallError::Refused { reason } | ActionCallError::Unavailable { reason } => reason,
        other => panic!("expected a refusal with a reason, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_reaches_the_device_it_names_and_is_answered() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_action_service) = asking(&b.0).await;
    let remote = b_action_service.remote(&b_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);

    // The request reaches the addressed device, which answers it from its
    // own dispatcher.
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    let seen = dispatcher.callers();
    assert_eq!(seen.len(), 1, "one request, one caller at the dispatcher");
    let answered_as = &seen[0];
    assert_eq!(
        answered_as.user_public_key(),
        Some(&root.verifying_key().to_bytes()),
        "the caller is the asking device's person"
    );
    assert_eq!(
        answered_as.device_public_key(),
        Some(&b.1.device_public_key),
        "and the device that asked"
    );
    assert_eq!(
        answered_as.workspace(),
        Some(WORKSPACE),
        "confined to the workspace it named"
    );
    assert_eq!(
        answered_as.acting().person_id(),
        Some(hex::encode(root.verifying_key().to_bytes()).as_str())
    );
    assert!(!answered_as.is_foreign_device());

    // An invocation is run where the device holds it, and its receipt comes
    // back on the same stream.
    let first = InvocationId::new();
    assert!(matches!(
        invoked(&remote, &caller, first).await,
        Ok(ReceiptStatus::Completed { .. })
    ));
    assert_eq!(dispatcher.asked()[0].invocation, first);

    // Following an invocation that settles at once costs one round trip.
    let followed = Arc::new(remote)
        .invoke_until_settled(invoke(InvocationId::new()), PATIENCE)
        .await
        .expect("the asker is not dropped");
    assert!(matches!(followed, Ok(InvocationUpdate::Succeeded { .. })));
    assert_eq!(dispatcher.asked().len(), 2);

    engine.close().await;
    b_endpoint.close().await;
}

/// A device asked through its dispatch says whose device answered: the certificate it answers
/// under, which the asker checks names the device it reached and verifies. One that names no
/// certificate answers nothing, and one answering under another device's is not believed.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_says_whose_device_answers_its_dispatch() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        Arc::new(TestProvider::new()),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_action_service) = asking(&b.0).await;
    let nowhere = tilo_server_protocol::WorkspaceId::nil().to_string();
    let remote = b_action_service.remote(&b_endpoint, &engine.addr(), &nowhere);

    assert!(
        remote.certificate().await.is_err(),
        "a device that names no certificate says nothing of who it is"
    );
    engine.service.answer_under(a.1.clone());
    assert_eq!(
        remote.certificate().await,
        Ok(a.1.clone()),
        "the device answers under its own"
    );
    engine.service.answer_under(b.1.clone());
    assert!(
        remote.certificate().await.is_err(),
        "a certificate of another device than the one reached is not believed"
    );

    engine.close().await;
    b_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn where_the_asker_serves_rides_the_request_and_is_not_what_it_asks() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_action_service) = asking(&b.0).await;
    let placement = tilo_workspace_join::sign_transport_hint(
        "iroh",
        "where b serves the workspace",
        b.1.clone(),
        [4; 32],
        &b.0,
    )
    .unwrap();
    let remote = b_action_service.placing(&b_endpoint, &engine.addr(), WORKSPACE, &placement);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);

    // A request that says where its device serves the workspace is answered
    // as any other, and what the answering device runs and records is the
    // request exactly as asked: the placement is not part of it.
    let first = InvocationId::new();
    assert!(matches!(
        invoked(&remote, &caller, first).await,
        Ok(ReceiptStatus::Completed { .. })
    ));
    let asked = dispatcher.asked();
    assert_eq!(
        asked,
        vec![invoke(first)],
        "the request as asked, and only it"
    );
    let recorded = serde_json::to_string(&asked[0]).unwrap();
    assert!(
        !recorded.contains("where b serves the workspace"),
        "nothing of the placement is in what the action records: {recorded}"
    );

    engine.close().await;
    b_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_is_not_the_sender_is_refused() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, _b_action_service) = asking(&b.0).await;

    // A device of another person the workspace never admitted is only the key it signs with:
    // a visitor, which browses nothing.
    let stranger_root = SigningKey::from_bytes(&[9; 32]);
    let c = device(&stranger_root, 10);
    let (c_endpoint, c_action_service) = asking(&c.0).await;
    let stranger = c_action_service.remote(&c_endpoint, &engine.addr(), WORKSPACE);
    let refused = queried(
        &stranger,
        &Caller::authenticated(
            stranger_root.verifying_key().to_bytes(),
            c.1.device_public_key,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(refused, ActionCallError::NotPermitted);

    // The device that asked answers as itself and nobody else: the owner's
    // own signature over an action naming the owner's device, sent from
    // another device, is not that device asking.
    let forged = SignedDeviceAction::sign(
        &DeviceAction {
            workspace: WORKSPACE.into(),
            request_id: uuid::Uuid::from_u128(1),
            source: engine.device(),
            target: engine.device(),
            message: catalogue(),
            placement: None,
        },
        now_ns(),
        &a.0,
    )
    .unwrap();
    let answer = send(&b_endpoint, engine.addr(), &forged)
        .await
        .expect("a refused request is answered");
    assert_eq!(
        answer,
        RemoteActionMessage::CatalogueResponse {
            correlation: 1,
            result: Err(ActionCallError::NotPermitted),
        }
    );
    assert_eq!(
        dispatcher.callers().len(),
        0,
        "nothing reached the dispatcher"
    );

    // A request naming a third device is not this one's to answer, and is refused at once: one
    // it has no link to, and the stranger above, whose link proves only its key and which the
    // workspace never certified.
    let unreached = device(&stranger_root, 11);
    for (n, third) in [unreached.1.device_public_key, c.1.device_public_key]
        .into_iter()
        .enumerate()
    {
        let elsewhere = SignedDeviceAction::sign(
            &DeviceAction {
                workspace: WORKSPACE.into(),
                request_id: uuid::Uuid::from_u128(2 + n as u128),
                source: b.1.device_public_key,
                target: third,
                message: catalogue(),
                placement: None,
            },
            now_ns(),
            &b.0,
        )
        .unwrap();
        let asked = std::time::Instant::now();
        let answer = send(&b_endpoint, engine.addr(), &elsewhere)
            .await
            .expect("a refused request is answered");
        let RemoteActionMessage::CatalogueResponse {
            result: Err(error), ..
        } = answer
        else {
            panic!("the request naming another device is refused, got {answer:?}");
        };
        let why = if third == c.1.device_public_key {
            "takes only this device's own requests over the link it dialled"
        } else {
            "has no link open to this one to pass the request on over"
        };
        let device = hex::encode(&third[..8]);
        assert_eq!(refusal(&error), format!("device {device} {why}"));
        assert!(
            asked.elapsed() < Duration::from_secs(2),
            "refused at once, not after waiting on the device"
        );
    }

    engine.close().await;
    b_endpoint.close().await;
    c_endpoint.close().await;
}

/// A device asks only while the workspace admits it: one its roster never
/// named asks nothing, and a device of the owner's revoked before it asks or
/// while it is served, or a guest's once the guest is removed, is refused from
/// that moment on with the reason, before its request reaches the dispatcher.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_is_refused_once_the_workspace_no_longer_admits_it() {
    use tilo_peer_state::fixture::{invitation, redeem};
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let outsider = device(&root, 9);
    let lost = device(&root, 6);
    let guest_root = SigningKey::from_bytes(&[4; 32]);
    let g = device(&guest_root, 5);
    let roster = vec![a.1.clone(), b.1.clone(), lost.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let now = tilo_clock::wall();
    let mut peer = workspace(WORKSPACE, &a, &roster);
    let signed = invitation(&peer, &a.0, now, |grant| {
        grant.nonce = [43; 16];
        grant.recipient_user_public_key = Some(g.1.user_public_key);
    });
    assert!(
        peer.governance_mut()
            .admit_invited_member(&redeem(&signed, (&g.0, &g.1), 44, now), &g.0, now)
            .unwrap()
    );
    let entry = revocation(&lost.1);
    peer.governance_mut()
        .publish_member_revocations(std::slice::from_ref(&entry), &a.0, now)
        .unwrap();
    let engine = Engine::serving(
        Replica::new(peer),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let mut endpoints = Vec::new();
    let mut asker = async |(key, cert): &(SigningKey, DeviceCert)| {
        let (endpoint, service) = asking(key).await;
        let remote = service.remote(&endpoint, &engine.addr(), WORKSPACE);
        endpoints.push(endpoint);
        (
            remote,
            Caller::authenticated(cert.user_public_key, cert.device_public_key),
        )
    };
    let (outsider, outsider_caller) = asker(&outsider).await;
    let (lost, lost_caller) = asker(&lost).await;
    let (own, own_caller) = asker(&b).await;
    let (guest, guest_caller) = asker(&g).await;

    assert!(
        queried(&outsider, &outsider_caller).await.is_err(),
        "a device the roster does not name asks nothing"
    );
    // A device revoked before anything asked: its signature verifies, and the workspace refuses it.
    let refused = query_error(&lost, &lost_caller).await;
    assert_eq!(refusal(&refused), "the device is revoked");
    assert_eq!(queried(&own, &own_caller).await, Ok(()));
    assert_eq!(queried(&guest, &guest_caller).await, Ok(()));
    assert_eq!(
        dispatcher.callers()[1].user_public_key(),
        Some(&guest_root.verifying_key().to_bytes()),
        "an admitted guest acts as their own person"
    );

    // A device revoked while it is served is refused from the moment the revocation is known.
    let entry = revocation(&b.1);
    assert!(
        engine
            .replica
            .write(|peer| peer
                .governance_mut()
                .publish_member_revocations(&[entry], &a.0, now))
            .unwrap()
    );
    let refused = query_error(&own, &own_caller).await;
    assert_eq!(refusal(&refused), "the device is revoked");

    assert!(
        engine
            .replica
            .write(|peer| peer
                .governance_mut()
                .remove_member(g.1.user_public_key, &a.0, now))
            .unwrap()
    );
    let refused = query_error(&guest, &guest_caller).await;
    assert_eq!(
        refusal(&refused),
        "the person is not a member of this workspace"
    );
    assert_eq!(
        dispatcher.callers().len(),
        2,
        "no refused request reaches the dispatcher"
    );

    engine.close().await;
    for endpoint in endpoints {
        endpoint.close().await;
    }
}

/// How long the root delegates the agent for: it acts inside this window and
/// not one instant after it.
const DELEGATION_WINDOW: Duration = Duration::from_millis(300);

#[test]
fn a_delegated_agent_acts_as_itself_and_not_once_its_window_closed() {
    // The answering engine reads the wall clock on its own threads, so the
    // test takes the process to itself and moves the clock past the window
    // instead of waiting for it: the window is exact at any machine speed.
    tilo_test_env::in_own_process(
        module_path!(),
        "a_delegated_agent_acts_as_itself_and_not_once_its_window_closed",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("a runtime for this test alone")
                .block_on(the_agent_acts_inside_its_window_and_not_after(process));
        },
    );
}

async fn the_agent_acts_inside_its_window_and_not_after(process: tilo_clock::testing::OwnProcess) {
    // The clock as the engine reads it, at its own resolution: the held time
    // is whole milliseconds, so every instant below is one the engine can be
    // held at exactly.
    let held = tilo_clock::testing::hold_wall_everywhere(process, tilo_clock::wall());
    let started = tilo_clock::wall();
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let helper = agent_device(&root, 4);
    let owner_device = a.1.device_public_key;
    let roster = vec![a.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;

    let issued = started
        .checked_sub(Duration::from_secs(1))
        .expect("a time one second before the window opens");
    let expires = started
        .checked_add(DELEGATION_WINDOW)
        .expect("a time the window ends");
    let delegation = Delegation::sign(
        &root,
        helper.1.device_public_key,
        Delegate::Agent {
            agent_id: "scribe".into(),
        },
        "Scribe",
        [10, 20, 30],
        DelegateScope {
            workspaces: vec![WorkspaceId::parse(WORKSPACE).unwrap()],
            apps: Vec::new(),
        },
        ns(issued),
        ns(expires),
    )
    .unwrap();
    assert!(
        engine
            .replica
            .write(|peer| peer.governance_mut().admit_agent(
                helper.1.clone(),
                delegation,
                &helper.0,
                started
            ))
            .unwrap()
    );

    let (helper_endpoint, helper_action_service) = asking(&helper.0).await;
    let remote = helper_action_service.remote(&helper_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::delegated_agent(
        "scribe",
        root.verifying_key().to_bytes(),
        helper.1.device_public_key,
    );
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    let answered_as = dispatcher
        .callers()
        .first()
        .expect("the agent's request reached the dispatcher")
        .clone();
    assert_eq!(
        answered_as.acting().member_id(),
        Some(&tilo_member_types::MemberId::agent(
            "scribe",
            hex::encode(root.verifying_key().to_bytes())
        )),
        "the agent is the agent, never its person"
    );
    assert_eq!(
        answered_as.user_public_key(),
        Some(&root.verifying_key().to_bytes()),
        "acting for the person whose root delegated it"
    );

    // Once its window is over the same device is refused, though its identity
    // stays on the roster to keep the history it wrote attributed. The clock is
    // moved to the instant the window ends, not waited for.
    drop(held);
    let _lapsed = tilo_clock::testing::hold_wall_everywhere(process, expires);
    let refused = queried(&remote, &caller).await.unwrap_err();
    assert_eq!(
        refusal(&refused),
        "the device's delegation is expired or withdrawn"
    );
    assert_eq!(dispatcher.callers().len(), 1);

    engine.close().await;
    helper_endpoint.close().await;
}

/// A root-signed delegation of `device` to the agent `id` in `WORKSPACE`, current from `at`.
fn agent_delegation(root: &SigningKey, device: &[u8; 32], id: &str, at: SystemTime) -> Delegation {
    Delegation::sign(
        root,
        *device,
        Delegate::Agent {
            agent_id: id.into(),
        },
        "Scribe",
        [10, 20, 30],
        DelegateScope {
            workspaces: vec![WorkspaceId::parse(WORKSPACE).unwrap()],
            apps: Vec::new(),
        },
        ns(at),
        ns(at).saturating_add(600_000_000_000),
    )
    .unwrap()
}

/// An agent's own key, on the workspace's roster, is bound to that entry where the server builds
/// its caller: the person's root-signed delegation for this workspace then reaches it.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_key_the_workspace_lists_is_bound_to_it() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let helper = agent_device(&root, 4);
    let roster = vec![a.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let now = tilo_clock::wall();
    let delegation = agent_delegation(&root, &helper.1.device_public_key, "scribe", now);
    assert!(
        engine
            .replica
            .write(|peer| peer.governance_mut().admit_agent(
                helper.1.clone(),
                delegation,
                &helper.0,
                now
            ))
            .unwrap()
    );

    let (helper_endpoint, helper_service) = asking(&helper.0).await;
    let remote = helper_service.remote(&helper_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::delegated_agent(
        "scribe",
        root.verifying_key().to_bytes(),
        helper.1.device_public_key,
    );
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    let answered_as = dispatcher
        .callers()
        .pop()
        .expect("the agent's request reached the dispatcher");
    assert_eq!(
        answered_as.acting().member_id(),
        Some(&tilo_member_types::MemberId::agent(
            "scribe",
            hex::encode(root.verifying_key().to_bytes())
        )),
        "the agent acts as itself"
    );
    assert!(
        answered_as.roster_created_at().is_some(),
        "the roster lists the key: {answered_as:?}"
    );
    assert!(
        !answered_as.granted().is_empty(),
        "the certificate's delegation reaches the agent"
    );

    engine.close().await;
    helper_endpoint.close().await;
}

/// A key `n` certified by `root` for an agent's grant, expiring an hour on: an agent's grant
/// that is no server key always does (ninety days at most).
fn agent_key(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let now = tilo_clock::unix_now_ns().saturating_sub(60_000_000_000);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// The device that hosts `root`'s workspace, with two keys of theirs admitted to it: `agent`,
/// which their root approved for an agent, and `server`, their server key.
struct HostWithKeys {
    engine: Engine,
    dispatcher: Arc<TestProvider>,
    /// The owner's own device: the person's access, where an agent's key is the agent's.
    owner_device: [u8; 32],
    agent: (SigningKey, DeviceCert),
    server: (SigningKey, DeviceCert),
    now: SystemTime,
    person: String,
}

async fn host_with_keys() -> HostWithKeys {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let agent = agent_key(&root, 7);
    let server = device_with(
        &root,
        8,
        DeviceCapabilities::own_key_grant(AgentScope::AllWorkspaces),
    );
    let roster = vec![a.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let now = tilo_clock::wall();
    for (key, cert, name) in [
        (&agent.0, &agent.1, "Bea's agent"),
        (&server.0, &server.1, "Bea's backend"),
    ] {
        assert!(
            engine
                .replica
                .write(|peer| {
                    let listed = peer.governance_mut().admit_member_device(cert, key, now)?;
                    let granted =
                        peer.governance_mut()
                            .author_own_grant(cert, Some(name), key, now)?;
                    Ok::<_, String>(listed || granted)
                })
                .unwrap(),
            "the workspace takes {name} in"
        );
    }
    HostWithKeys {
        engine,
        dispatcher,
        owner_device,
        agent,
        server,
        now,
        person: hex::encode(root.verifying_key().to_bytes()),
    }
}

/// A key the person approved for an agent, dialing the device that hosts their workspace, acts as
/// that agent: the workspace's grants naming it decide its calls, never its person's rights.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_the_workspace_lists_acts_as_its_agent() {
    let host = host_with_keys().await;
    let agent = host.agent;
    let (agent_endpoint, agent_service) = asking(&agent.0).await;
    let remote = agent_service.remote(&agent_endpoint, &host.engine.addr(), WORKSPACE);
    let agent_id = tilo_member_types::key_agent_id(&agent.1.device_public_key);
    let caller = Caller::delegated_agent(
        agent_id.clone(),
        agent.1.user_public_key,
        agent.1.device_public_key,
    );
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    let answered_as = host
        .dispatcher
        .callers()
        .pop()
        .expect("the agent's request reached the dispatcher");
    assert_eq!(
        answered_as.acting().member_id(),
        Some(&tilo_member_types::MemberId::agent(
            agent_id.clone(),
            host.person.clone()
        )),
        "a key acts as the agent its certificate names"
    );
    assert_eq!(
        answered_as.acting().person_id(),
        None,
        "the agent never stands for its person"
    );
    assert_eq!(
        answered_as.acting().acted_for_person(),
        Some(host.person.as_str()),
        "it acts for the person whose root approved it"
    );
    assert_eq!(
        answered_as.device_public_key(),
        Some(&agent.1.device_public_key)
    );
    assert_eq!(
        answered_as.user_public_key(),
        Some(&agent.1.user_public_key)
    );
    assert!(
        answered_as.granted().iter().any(|grant| grant
            .grantee
            .as_agent()
            .is_some_and(|(id, of)| id == agent_id && of.as_str() == host.person)),
        "the workspace's grant naming the agent reaches it: {:?}",
        answered_as.granted()
    );
    assert!(
        answered_as
            .granted()
            .iter()
            .all(|grant| grant.grantee.as_agent().is_some()),
        "no person's own grant reaches the agent: {:?}",
        answered_as.granted()
    );

    // What the workspace leaves to its owner, its agent's grant does not allow: the workspace's
    // own settings. The person's access allows it; the agent's, decided by the grants it carries,
    // does not.
    let settings = tilo_action_policy::roles::workspace_resource(WORKSPACE);
    let write = tilo_action_types::ActionKind::Undoable;
    // The person's own device holds the person's access: the agent's key holds its agent's.
    let access = host
        .engine
        .replica
        .read(|peer| peer.permissions(host.owner_device, host.now))
        .expect("the workspace knows the owner's device");
    assert!(
        access.allows(&settings, write, host.now),
        "the person may change their workspace's settings"
    );
    let agent_access = access
        .as_acting(answered_as.acting().clone())
        .expect("the agent acts for its person")
        .granted(answered_as.granted());
    assert!(
        !agent_access.allows(&settings, write, host.now),
        "the agent's own grants leave its person's settings alone"
    );

    host.engine.close().await;
    agent_endpoint.close().await;
}

/// A server key of the person's is them at the key: what its person may do there is theirs.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_key_acts_as_its_person() {
    let host = host_with_keys().await;
    let server = host.server;
    let (server_endpoint, server_service) = asking(&server.0).await;
    let remote = server_service.remote(&server_endpoint, &host.engine.addr(), WORKSPACE);
    let caller = Caller::authenticated(server.1.user_public_key, server.1.device_public_key);
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    let answered_as = host
        .dispatcher
        .callers()
        .pop()
        .expect("the server key's request reached the dispatcher");
    assert_eq!(
        answered_as.acting().person_id(),
        Some(host.person.as_str()),
        "a server key is the person at the key"
    );
    assert_eq!(
        answered_as.acting().member_id(),
        Some(&tilo_member_types::MemberId::person(host.person.clone()))
    );
    let server_access = host
        .engine
        .replica
        .read(|peer| peer.permissions(server.1.device_public_key, host.now))
        .expect("the workspace knows the key");
    assert!(
        server_access.allows(
            &tilo_action_policy::roles::workspace_resource(WORKSPACE),
            tilo_action_types::ActionKind::Undoable,
            host.now
        ),
        "a server key holds its person's own"
    );

    host.engine.close().await;
    server_endpoint.close().await;
}

/// A key the workspace's roster never listed, or has retired, is refused at the server and
/// reaches no dispatcher.
#[tokio::test(flavor = "multi_thread")]
async fn a_key_the_workspace_lists_not_or_retired_is_refused() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let retired = agent_device(&root, 5);
    let unknown = agent_device(&root, 6);
    let roster = vec![a.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let now = tilo_clock::wall();
    let delegation = agent_delegation(&root, &retired.1.device_public_key, "quill", now);
    assert!(
        engine
            .replica
            .write(|peer| peer.governance_mut().admit_agent(
                retired.1.clone(),
                delegation,
                &retired.0,
                now
            ))
            .unwrap()
    );
    assert!(
        engine
            .replica
            .write(|peer| peer.governance_mut().withdraw_agent(
                retired.1.device_public_key,
                &a.0,
                now
            ))
            .unwrap()
    );

    // Retired: refused, though its identity stays to keep its history attributed.
    let (retired_endpoint, retired_service) = asking(&retired.0).await;
    let remote = retired_service.remote(&retired_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::delegated_agent(
        "quill",
        root.verifying_key().to_bytes(),
        retired.1.device_public_key,
    );
    let refused = queried(&remote, &caller).await.unwrap_err();
    assert_eq!(
        refusal(&refused),
        "the device's delegation is expired or withdrawn"
    );

    // Never listed: refused too.
    let (unknown_endpoint, unknown_service) = asking(&unknown.0).await;
    let remote = unknown_service.remote(&unknown_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::delegated_agent(
        "ghost",
        root.verifying_key().to_bytes(),
        unknown.1.device_public_key,
    );
    assert!(
        matches!(
            queried(&remote, &caller).await,
            Err(ActionCallError::NotPermitted)
        ),
        "a key the roster never listed is refused"
    );
    assert_eq!(
        dispatcher.callers().len(),
        0,
        "no refused key reached the dispatcher"
    );

    engine.close().await;
    retired_endpoint.close().await;
    unknown_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changes_spend_their_budget_while_reading_spends_none() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds {
            changes_per_window: 1,
            window_secs: 3600,
            burst: 2,
        },
    )
    .await;
    let (b_endpoint, b_action_service) = asking(&b.0).await;
    let remote = b_action_service.remote(&b_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);

    for _ in 0..2 {
        assert!(matches!(
            invoked(&remote, &caller, InvocationId::new()).await,
            Ok(ReceiptStatus::Completed { .. })
        ));
    }
    let refused = invoked(&remote, &caller, InvocationId::new())
        .await
        .unwrap_err();
    assert_eq!(refusal(&refused), "too many changes; slow down");
    assert_eq!(
        dispatcher.asked().len(),
        2,
        "a device over its budget runs nothing"
    );

    // A caller that is following work must keep being able to look at it.
    assert_eq!(queried(&remote, &caller).await, Ok(()));

    engine.close().await;
    b_endpoint.close().await;
}

/// The action a [`Described`] provider describes as a read.
const READ: &str = "read";

/// `test.provider` as a provider that describes its own actions: [`READ`] reads, anything else
/// changes. What it answers is the test provider's.
struct Described(Arc<TestProvider>);

impl ActionProvider for Described {
    fn serves(&self, provider: &str) -> bool {
        provider == "test.provider"
    }

    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> tilo_actions::remote::Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>>
    {
        self.0.query(caller, query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, ActionCallError>,
    > {
        self.0.invoke(caller, request)
    }

    fn observe(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        self.0.observe(caller, id)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        self.0.cancel(caller, id)
    }

    fn invocation(
        &self,
        address: &ActionAddress,
        _argument: Option<&str>,
    ) -> Option<tilo_actions::api::ActionDescriptor> {
        let kind = if address.action == READ {
            tilo_actions::contract::ActionKind::Read
        } else {
            tilo_actions::contract::ActionKind::Outside
        };
        Some(tilo_actions::api::ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: address.clone(),
            resource_kind: "note".into(),
            resource_label: "Note".into(),
            action_label: address.action.clone(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::contract::ActionContract {
                kind,
                ..tilo_actions::contract::ActionContract::default()
            },
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        })
    }
}

/// `invocation` of the note's `action`, its argument saying it only reads.
fn claiming_a_read(invocation: InvocationId, action: &str) -> InvokeRequest {
    let mut request = invoke(invocation);
    request.target.address = ActionAddress::new("note:1", action);
    request.argument = InvokeArgument::Text("read".into());
    request
}

/// A read its provider describes as one spends none of the caller's changes; a change spends
/// one whatever the caller says of it, so the bound holds against a write passed off as a read.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_its_provider_describes_spends_nothing_and_a_write_called_a_read_still_spends() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        Arc::new(TestProvider::new()),
        Bounds {
            changes_per_window: 1,
            window_secs: 3600,
            burst: 2,
        },
    )
    .await;
    engine
        .service
        .add_provider(Arc::new(Described(Arc::clone(&dispatcher))));
    let (b_endpoint, b_action_service) = asking(&b.0).await;
    let remote = b_action_service.remote(&b_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);
    let sent = |request: InvokeRequest| {
        let remote = &remote;
        let caller = &caller;
        async move {
            tokio::time::timeout(PATIENCE, ActionProvider::invoke(remote, caller, request))
                .await
                .expect("the device answers")
                .expect("the asker is not dropped")
                .map(|receipt| receipt.status)
        }
    };

    // Far more reads than the bound allows changes.
    for _ in 0..5 {
        assert!(matches!(
            sent(claiming_a_read(InvocationId::new(), READ)).await,
            Ok(ReceiptStatus::Completed { .. })
        ));
    }
    // A write that says everywhere it can that it reads spends a change, up to the bound.
    for _ in 0..2 {
        assert!(matches!(
            sent(claiming_a_read(InvocationId::new(), "insert")).await,
            Ok(ReceiptStatus::Completed { .. })
        ));
    }
    let refused = sent(claiming_a_read(InvocationId::new(), "insert"))
        .await
        .unwrap_err();
    assert_eq!(refusal(&refused), "too many changes; slow down");
    // A plain write past the bound is refused too, and nothing past it ran.
    let refused = sent(invoke(InvocationId::new())).await.unwrap_err();
    assert_eq!(refusal(&refused), "too many changes; slow down");
    assert_eq!(
        dispatcher.asked().len(),
        7,
        "the reads and the writes within the bound ran, nothing past it"
    );

    engine.close().await;
    b_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_a_device_does_not_answer_are_refused_at_once() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_action_service) = asking(&b.0).await;

    // A workspace this device does not hold.
    let elsewhere = b_action_service.remote(&b_endpoint, &engine.addr(), ELSEWHERE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);
    let refused = queried(&elsewhere, &caller).await.unwrap_err();
    assert_eq!(
        refusal(&refused),
        "this device does not have that workspace"
    );

    // A request the action service does not serve is answered with a reason,
    // never left for the asker to wait on.
    let wanted = SignedDeviceAction::sign(
        &DeviceAction {
            workspace: WORKSPACE.into(),
            request_id: uuid::Uuid::from_u128(3),
            source: b.1.device_public_key,
            target: engine.device(),
            message: RemoteActionMessage::ResourceRead {
                correlation: 5,
                on_behalf_of: None,
                read: tilo_actions::remote::ResourceReadRequest {
                    workspace: WORKSPACE.into(),
                    app: "test.app".into(),
                    record: "tab:1".into(),
                    joined: false,
                    after: None,
                    wait_ms: None,
                    answer: tilo_actions::remote::ReadAnswer::Reading,
                },
            },
            placement: None,
        },
        now_ns(),
        &b.0,
    )
    .unwrap();
    let answer = send(&b_endpoint, engine.addr(), &wanted)
        .await
        .expect("a request nothing serves is answered");
    let RemoteActionMessage::ResourceReading {
        correlation: 5,
        result: Err(error),
    } = answer
    else {
        panic!("an unserved request is refused, got {answer:?}");
    };
    assert!(
        matches!(error, ActionCallError::Unavailable { .. }),
        "{error:?}"
    );
    assert_eq!(dispatcher.callers().len(), 0);

    engine.close().await;
    b_endpoint.close().await;
}

/// `action` on the address `entry-1` left at the workspace's entry, as a visitor's page asks it.
fn at_the_entry(action: &str) -> InvokeRequest {
    InvokeRequest {
        target: RemoteActionTarget {
            provider: tilo_action_policy::VISITOR_ENTRY_APP.into(),
            address: ActionAddress::new("entry-1", action),
        },
        ..invoke(InvocationId::new())
    }
}

/// `request` as `caller` sends it to the device at `remote`, and the status the device answered it
/// with.
async fn asking_as(
    remote: &RemoteDevice,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<ReceiptStatus, ActionCallError> {
    tokio::time::timeout(PATIENCE, ActionProvider::invoke(remote, caller, request))
        .await
        .expect("the device answers")
        .expect("the asker is not dropped")
        .map(|receipt| receipt.status)
}

/// What the entry lets a visitor's page do: the calls the entry opens complete, and every other
/// write or read is refused as to a stranger, before any dispatcher meets the visitor.
async fn the_entry_decides_a_visitors_calls(remote: &RemoteDevice, nobody: &Caller) {
    assert_eq!(
        asking_as(remote, nobody, invoke(InvocationId::new())).await,
        Err(ActionCallError::NotPermitted),
        "a visitor may not change a note"
    );
    assert_eq!(
        asking_as(remote, nobody, at_the_entry("invite")).await,
        Err(ActionCallError::NotPermitted),
        "nor invite anyone from the wait list"
    );
    // Leaving an address, confirming it and taking it off are open to any key.
    for action in ["join", "confirm", "leave"] {
        assert!(
            matches!(
                asking_as(remote, nobody, at_the_entry(action)).await,
                Ok(ReceiptStatus::Completed { .. })
            ),
            "a visitor may {action}"
        );
    }
    assert_eq!(
        queried(remote, nobody).await,
        Err(ActionCallError::NotPermitted),
        "nor browse the workspace's actions"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_visitor_does_what_the_entry_opens_and_no_more_and_not_too_often() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let mut peer = workspace(WORKSPACE, &a, std::slice::from_ref(&a.1));
    // The workspace takes people by invitation and keeps their requests.
    peer.governance_mut()
        .put_workspace_entry(
            &tilo_action_policy::EntryPolicy {
                listing: None,
                invitation_only: true,
                wait_list_max: 100,
                contact: None,
                mail: None,
            },
            &a.0,
            tilo_clock::wall(),
        )
        .unwrap();
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(peer),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;

    // A page with a key of its own, which no certificate in the workspace names.
    let visitor = SigningKey::from_bytes(&[9; 32]);
    let (visitor_endpoint, visitor_service) = asking(&visitor).await;
    let remote = visitor_service.remote(&visitor_endpoint, &engine.addr(), WORKSPACE);
    let nobody = Caller::visitor(visitor.verifying_key().to_bytes());

    the_entry_decides_a_visitors_calls(&remote, &nobody).await;
    let seen = dispatcher.callers();
    assert_eq!(seen.len(), 3, "only what the grants allow runs");
    assert_eq!(
        seen[0].acting().visitor_key(),
        Some(visitor.verifying_key().to_bytes()),
        "the dispatcher meets the visitor as the key that signed"
    );
    assert_eq!(seen[0].user_public_key(), None, "acting for nobody");
    assert_eq!(seen[0].workspace(), Some(WORKSPACE));

    // One network gets so many calls in ten minutes, whatever key it signs with.
    let calls_so_far = 3;
    for _ in calls_so_far..tilo_peer_state::actions::VISITOR_CALLS_PER_TEN_MINUTES {
        assert!(matches!(
            asking_as(&remote, &nobody, at_the_entry("join")).await,
            Ok(ReceiptStatus::Completed { .. })
        ));
    }
    let another = SigningKey::from_bytes(&[10; 32]);
    let (another_endpoint, another_service) = asking(&another).await;
    let fresh_key = another_service.remote(&another_endpoint, &engine.addr(), WORKSPACE);
    let refused = tokio::time::timeout(
        PATIENCE,
        ActionProvider::invoke(&fresh_key, &nobody, at_the_entry("join")),
    )
    .await
    .expect("the device answers")
    .expect("the asker is not dropped")
    .unwrap_err();
    assert!(
        refusal(&refused).starts_with("too many tries from your network"),
        "{refused:?}"
    );
    assert_eq!(
        dispatcher.callers().len(),
        tilo_peer_state::actions::VISITOR_CALLS_PER_TEN_MINUTES,
        "a call over the count runs nothing"
    );

    engine.close().await;
    visitor_endpoint.close().await;
    another_endpoint.close().await;
}

/// A request one device signed for another, hand-written: what a device sends
/// when it names a device that is not the one it is connected to.
fn addressed_to(
    key: &SigningKey,
    source: [u8; 32],
    target: [u8; 32],
    message: RemoteActionMessage,
) -> SignedDeviceAction {
    SignedDeviceAction::sign(
        &DeviceAction {
            workspace: WORKSPACE.into(),
            request_id: uuid::Uuid::new_v4(),
            source,
            target,
            message,
            placement: None,
        },
        now_ns(),
        key,
    )
    .unwrap()
}

/// A link carries the requests of the device at its other end: the service
/// serving one end answers what the other end's device asks, and answers it as
/// that device.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_carries_the_requests_of_the_device_at_its_other_end() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    // A serves its action service on the link it holds; B asks over the other end.
    let (a_end, b_end) =
        tilo_peer_state::actions::testing::duplex(Some(b.1.device_public_key), None);
    let service = ActionService::new(a.0.clone(), dispatcher.clone());
    service.hold(Replica::new(workspace(WORKSPACE, &a, &roster)));
    service.serve_link(a_end);
    let asking_service = ActionService::new(b.0.clone(), Arc::new(TestProvider::new()));
    let remote = asking_service.remote_over(b_end, a.1.device_public_key, WORKSPACE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);

    assert_eq!(queried(&remote, &caller).await, Ok(()));
    assert_eq!(
        dispatcher.callers()[0].device_public_key(),
        Some(&b.1.device_public_key),
        "the device at the other end of the link asks as itself"
    );
    assert!(matches!(
        invoked(&remote, &caller, InvocationId::new()).await,
        Ok(ReceiptStatus::Completed { .. })
    ));
    assert_eq!(dispatcher.asked().len(), 1);
}

/// A request addressed to another device this service holds a link to is passed
/// on there as its signer wrote it: the device it names runs it as the device
/// that signed it, and its answer is the asker's.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_addressed_to_another_device_is_passed_on() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let c = device(&root, 4);
    let roster = vec![a.1.clone(), b.1.clone(), c.1.clone()];
    // A answers the requests its devices address to it; C is a page that holds
    // a link to A and answers what reaches it there.
    let a_dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        a_dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let c_dispatcher = Arc::new(TestProvider::new());
    let c_service = ActionService::new(c.0.clone(), c_dispatcher.clone());
    c_service.hold(Replica::new(workspace(WORKSPACE, &c, &roster)));
    let (host_end, page_end) =
        tilo_peer_state::actions::testing::duplex(Some(c.1.device_public_key), None);
    c_service.serve_link(page_end);
    engine
        .service
        .reach_device_at(c.1.device_public_key, host_end);

    // B asks A for C's actions; A passes the request on to C.
    let (b_endpoint, b_service) = asking(&b.0).await;
    let asked = addressed_to(
        &b.0,
        b.1.device_public_key,
        c.1.device_public_key,
        catalogue(),
    );
    let answer = send(&b_endpoint, engine.addr(), &asked)
        .await
        .expect("a request a device this one reaches is answered");
    assert!(
        matches!(
            answer,
            RemoteActionMessage::CatalogueResponse { result: Ok(_), .. }
        ),
        "{answer:?}"
    );
    assert_eq!(
        c_dispatcher.callers()[0].device_public_key(),
        Some(&b.1.device_public_key),
        "the device it names runs the request as the device that signed it"
    );
    assert!(
        a_dispatcher.callers().is_empty(),
        "the device it was passed through answers nothing itself"
    );

    // A device no link reaches is refused at once, never left waiting.
    let unfound = device(&root, 5).1.device_public_key;
    let refused = send(
        &b_endpoint,
        engine.addr(),
        &addressed_to(&b.0, b.1.device_public_key, unfound, catalogue()),
    )
    .await
    .expect("a request no link reaches is refused");
    let RemoteActionMessage::CatalogueResponse {
        result: Err(error), ..
    } = refused
    else {
        panic!("a request no link reaches is refused, got {refused:?}");
    };
    assert_eq!(
        refusal(&error),
        format!(
            "device {} has no link open to this one to pass the request on over",
            hex::encode(&unfound[..8])
        ),
        "the refusal names the device and why: {error:?}"
    );
    drop(b_service);
    engine.close().await;
    b_endpoint.close().await;
}

/// What `remote` answered an invocation of `request` as `caller`.
async fn invoke_as(
    remote: &RemoteDevice,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<ReceiptStatus, ActionCallError> {
    let answer = tokio::time::timeout(PATIENCE, ActionProvider::invoke(remote, caller, request))
        .await
        .expect("the device answers")
        .expect("the asker is not dropped");
    answer.map(|receipt| receipt.status)
}

/// `request` signed by `cert`'s key, as the caller sends it.
fn signed_request(request: &InvokeRequest, cert: &DeviceCert, key: &SigningKey) -> InvokeRequest {
    let mut request = request.clone();
    request.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(
            WORKSPACE,
            &request,
            cert,
            key,
            tilo_clock::unix_now_ns(),
        )
        .unwrap(),
    ));
    request
}

/// A device signs a call as its person only when the person makes it on their own connection to
/// it: what an app, an agent, a shortcut's run or a caller the device merely admitted asks goes
/// unsigned, whatever person it names, and a device with no certificate to sign under says so
/// rather than sending the person's call unsigned.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_signs_only_its_persons_own_calls_as_theirs() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_service) = asking(&b.0).await;
    b_service.answer_under(b.1.clone());
    let remote = b_service.remote(&b_endpoint, &engine.addr(), WORKSPACE);
    let person = hex::encode(root.verifying_key().to_bytes());
    let own = Caller::own_person(person.clone()).in_workspace(WORKSPACE);

    // The person's own call goes signed by their device, for the workspace it is sent to.
    match invoke_as(&remote, &own, invoke(InvocationId::new())).await {
        Ok(ReceiptStatus::Completed { .. }) => {}
        other => panic!("the person's own call runs, got {other:?}"),
    }
    let ran = dispatcher.asked().pop().unwrap();
    let signed = ran.signed.expect("the person's own call is signed");
    assert_eq!(signed.statement().signer, b.1.device_public_key);
    assert_eq!(signed.statement().workspace, WORKSPACE);

    // Nobody else's call does, whoever it names.
    let others = [
        (
            "a caller the device admitted",
            Caller::local_person(person.clone()),
        ),
        ("an app", Caller::local_app("so.tilo.chat")),
        (
            "the person's call through an app",
            own.clone().through_app("so.tilo.chat"),
        ),
        ("a shortcut's run", own.clone().running_shortcut("weekly")),
        (
            "an agent of the person's",
            Caller::local_agent("scribe", person.clone()),
        ),
        (
            "an agent the person handed their session",
            own.clone().delegated_to("scribe"),
        ),
    ];
    for (who, caller) in others {
        let caller = caller.in_workspace(WORKSPACE);
        match invoke_as(&remote, &caller, invoke(InvocationId::new())).await {
            Ok(ReceiptStatus::Completed { .. }) => {}
            other => panic!("{who}'s call runs as the device, got {other:?}"),
        }
        let ran = dispatcher.asked().pop().unwrap();
        assert!(
            ran.signed.is_none(),
            "{who}'s call is not signed as the person"
        );
    }

    // A device answering under no certificate cannot sign as its person: it says so, and sends
    // nothing.
    let (c_endpoint, c_service) = asking(&b.0).await;
    let uncertified = c_service.remote(&c_endpoint, &engine.addr(), WORKSPACE);
    let asked_before = dispatcher.asked().len();
    let refused = invoke_as(&uncertified, &own, invoke(InvocationId::new()))
        .await
        .expect_err("an uncertified device does not send its person's call unsigned");
    assert!(
        refusal(&refused).contains("cannot sign as its person"),
        "{refused:?}"
    );
    assert_eq!(dispatcher.asked().len(), asked_before, "nothing ran");
}

/// An invocation runs as the key that signed it: one whose invocation was changed after signing
/// is refused, the same id for another request is a conflict, and an invocation
/// signed under a revoked or expired certificate runs nothing.
#[tokio::test(flavor = "multi_thread")]
async fn an_invocation_runs_as_the_key_that_signed_it() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_service) = asking(&b.0).await;
    let remote = b_service.remote(&b_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);

    // A signed invocation runs as its signer, and what it signed travels with it.
    let asked = signed_request(&invoke(InvocationId::new()), &b.1, &b.0);
    let ran = match invoke_as(&remote, &caller, asked.clone()).await {
        Ok(ReceiptStatus::Completed { .. }) => dispatcher.asked().pop().unwrap(),
        other => panic!("a signed invocation runs, got {other:?}"),
    };
    assert_eq!(ran.signed, asked.signed);
    // The caller is the key that signed, in the run its link carries.
    let signer = &dispatcher.callers()[0];
    let expected = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);
    assert_eq!(
        signer.acting(),
        expected.acting(),
        "the caller is the key that signed"
    );
    assert_eq!(signer.device_public_key(), Some(&b.1.device_public_key));
    assert_eq!(
        signer.user_public_key(),
        Some(&root.verifying_key().to_bytes())
    );
    assert_eq!(signer.workspace(), Some(WORKSPACE));
    assert_eq!(signer.forwarder(), None);

    // A request whose invocation was changed after signing is refused, for its target, its
    // argument, or the workspace it is sent to.
    let mut retargeted = signed_request(&invoke(InvocationId::new()), &b.1, &b.0);
    retargeted.target.address.action = "delete".into();
    let mut reargued = signed_request(&invoke(InvocationId::new()), &b.1, &b.0);
    reargued.argument = InvokeArgument::Text("Other".into());
    for changed in [retargeted, reargued] {
        let refused = invoke_as(&remote, &caller, changed)
            .await
            .expect_err("a changed invocation is refused");
        assert!(
            refusal(&refused).starts_with("invocation_signature_invalid"),
            "{refused:?}"
        );
    }
    assert_eq!(dispatcher.asked().len(), 1, "nothing changed ran");

    // Signed under a certificate that has expired since.
    let mut expired = b.1.clone();
    expired.expires_at_ns = Some(2);
    tilo_user_identity::sign_device_cert(&mut expired, &root).unwrap();
    let late = signed_request(&invoke(InvocationId::new()), &expired, &b.0);
    let refused = invoke_as(&remote, &caller, late)
        .await
        .expect_err("an invocation under an expired certificate is refused");
    assert!(
        refusal(&refused).starts_with("invocation_signature_invalid"),
        "{refused:?}"
    );

    // One invocation id names one request: signed again for the same request it is a retry,
    // for another action it is a conflict, as the invocation log answers one.
    assert!(matches!(
        invoke_as(&remote, &caller, asked.clone()).await,
        Ok(ReceiptStatus::Completed { .. })
    ));
    let mut again = asked.clone();
    again.target.address.action = "delete".into();
    let again = signed_request(&again, &b.1, &b.0);
    assert_eq!(
        invoke_as(&remote, &caller, again)
            .await
            .expect_err("another request under the same id is refused"),
        ActionCallError::InvocationConflict
    );

    // Cancelling names an invocation the signer started, and reaches the provider.
    assert_eq!(
        tokio::time::timeout(
            PATIENCE,
            ActionProvider::cancel(&remote, &caller, &asked.invocation),
        )
        .await
        .expect("the device answers")
        .expect("the asker is not dropped"),
        Ok(())
    );

    engine.close().await;
    b_endpoint.close().await;
}

/// `cert`'s revocation by the root of its person, `[1; 32]`, for a lost device.
fn revocation(cert: &DeviceCert) -> RevocationEntry {
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    entry
}

/// What `remote` refused a catalogue query with, as `caller`.
async fn query_error(remote: &RemoteDevice, caller: &Caller) -> ActionCallError {
    tokio::time::timeout(
        PATIENCE,
        ActionProvider::query(remote, caller, CatalogueQuery::default()),
    )
    .await
    .expect("the device answers")
    .expect("the asker is not dropped")
    .expect_err("the device refuses")
}

/// A request the provider abandons is refused with why, never left for its asker
/// to wait on.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_the_provider_abandons_is_refused() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    // A provider that drops the completer of every answer it is asked for.
    let silent = Arc::new(TestProvider::new().answering(|_, _| {
        tilo_actions::remote::pending::<
            Result<tilo_actions::remote::InvocationReceipt, ActionCallError>,
        >()
        .1
    }));
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        silent,
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_service) = asking(&b.0).await;
    let remote = b_service.remote(&b_endpoint, &engine.addr(), WORKSPACE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);
    let refused = invoked(&remote, &caller, InvocationId::new())
        .await
        .expect_err("an abandoned request is refused");
    assert!(
        matches!(refused, ActionCallError::Unavailable { .. }),
        "{refused:?}"
    );
    engine.close().await;
    b_endpoint.close().await;
}
/// The host's entry, and nothing else, as a device with no certificate from it may ask: a
/// provider that answers `workspace.entry` and refuses every other action.
#[derive(Clone)]
struct EntryOnly;

impl ActionProvider for EntryOnly {
    fn serves(&self, provider: &str) -> bool {
        provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> tilo_actions::remote::Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>>
    {
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::CataloguePage::from_rows(
            "0".into(),
            Vec::new(),
            &query,
        )))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, ActionCallError>,
    > {
        if request.target.address.action != tilo_server_protocol::workspaces::ENTER_ENTRY {
            // What a host without a certificate answers: nothing reached here is served, however
            // it names one.
            return tilo_actions::remote::Pending::ready(Err(ActionCallError::UnknownAction));
        }
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({"entered": "admitted"})),
                    resources: Vec::new(),
                },
            },
        }))
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// The invocation a device with no certificate asks `action` of `resource` under, in the scope a
/// host admits it in.
fn scoped(invocation: InvocationId, resource: &str, action: &str) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER.into(),
            address: ActionAddress::new(resource.to_owned(), action.to_owned()),
        },
        argument: InvokeArgument::Json(serde_json::Map::new()),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_with_no_certificate_reaches_only_the_entry() {
    let root = SigningKey::from_bytes(&[5; 32]);
    let host = device(&root, 6);
    let asker = device(&root, 7);
    let host_endpoint = localhost_endpoint(&host.0).await;
    let service = ActionService::new(
        host.0.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    service.admit_uncertified(
        tilo_server_protocol::workspaces::ENTRY_SCOPE,
        Arc::new(EntryOnly),
    );
    let router = Router::builder(host_endpoint.clone())
        .accept(ACTIONS_ALPN, service)
        .spawn();
    let host_addr = addr(&host_endpoint);
    let asker_endpoint = localhost_endpoint(&asker.0).await;
    let host_device = host.1.device_public_key;
    let source = asker.1.device_public_key;
    let signed = |invoke: InvokeRequest| {
        SignedDeviceAction::sign(
            &DeviceAction {
                workspace: tilo_server_protocol::workspaces::ENTRY_SCOPE.to_owned(),
                request_id: uuid::Uuid::new_v4(),
                source,
                target: host_device,
                message: RemoteActionMessage::Invoke(invoke),
                placement: None,
            },
            now_ns(),
            &asker.0,
        )
        .unwrap()
    };

    // The entry is answered.
    let entry = signed(scoped(
        InvocationId::new(),
        tilo_server_protocol::workspaces::HOST_WORKSPACES_RESOURCE,
        tilo_server_protocol::workspaces::ENTER_ENTRY,
    ));
    let answer = send(&asker_endpoint, host_addr.clone(), &entry).await;
    assert!(
        matches!(
            answer,
            Some(RemoteActionMessage::Receipt(
                tilo_actions::remote::InvocationReceipt {
                    status: ReceiptStatus::Completed { .. },
                    ..
                }
            ))
        ),
        "the entry is answered: {answer:?}"
    );

    // Nothing else is reachable in that scope, however it is named.
    let other = signed(scoped(
        InvocationId::new(),
        tilo_server_protocol::workspaces::HOST_WORKSPACES_RESOURCE,
        tilo_server_protocol::workspaces::ADMINISTER_WORKSPACES,
    ));
    let answer = send(&asker_endpoint, host_addr, &other).await;
    assert!(
        matches!(
            answer,
            Some(RemoteActionMessage::Receipt(
                tilo_actions::remote::InvocationReceipt {
                    status: ReceiptStatus::Rejected {
                        error: ActionCallError::UnknownAction
                    },
                    ..
                }
            ))
        ),
        "a device with no certificate reaches no other action: {answer:?}"
    );
    router.shutdown().await.unwrap();
    host_endpoint.close().await;
    asker_endpoint.close().await;
}

/// The call `request` names `acting` as its caller, in what its sender says of it.
fn naming(mut request: InvokeRequest, acting: &tilo_member_types::Acting) -> InvokeRequest {
    request.context = Some(tilo_actions::ActionContext {
        invoker: Some(tilo_actions::ActionInvoker::admitted(acting)),
        ..tilo_actions::ActionContext::default()
    });
    request
}

/// A device forwards the calls of the people, agents and visitors it serves to another device
/// of the workspace as each of them signed it: the device that answers runs the call as its
/// signer, with that key's grants, and records which device brought it. A call naming another
/// caller without their signature runs as nobody, and a device outside the workspace forwards
/// nobody's call.
#[tokio::test(flavor = "multi_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one forwarded call, from its signer to its answer"
)]
async fn a_forwarded_call_runs_as_the_one_who_signed_it() {
    let started = tilo_clock::wall();
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let c = device(&root, 5);
    let helper = agent_device(&root, 4);
    let roster = vec![a.1.clone(), b.1.clone(), c.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let delegation = Delegation::sign(
        &root,
        helper.1.device_public_key,
        Delegate::Agent {
            agent_id: "scribe".into(),
        },
        "Scribe",
        [10, 20, 30],
        DelegateScope {
            workspaces: vec![WorkspaceId::parse(WORKSPACE).unwrap()],
            apps: Vec::new(),
        },
        ns(started.checked_sub(Duration::from_secs(1)).unwrap()),
        ns(started.checked_add(Duration::from_secs(3600)).unwrap()),
    )
    .unwrap();
    assert!(
        engine
            .replica
            .write(|peer| peer.governance_mut().admit_agent(
                helper.1.clone(),
                delegation,
                &helper.0,
                started
            ))
            .unwrap()
    );
    let person = root.verifying_key().to_bytes();
    let (b_endpoint, b_service) = asking(&b.0).await;
    let forwarding = b_service.remote(&b_endpoint, &engine.addr(), WORKSPACE);
    let b_caller = Caller::authenticated(person, b.1.device_public_key);

    // The agent signed with its own key; the device that does not run the action brings it.
    agent_call_runs_as_the_agent(&forwarding, &dispatcher, &b_caller, &helper, &b).await;
    // A person's call from another of their devices travels the same way.
    a_persons_call_runs_as_its_device(&forwarding, &dispatcher, &b_caller, &c, &b, person).await;
    // The device's own unsigned call runs as that device, forwarded by nobody.
    the_devices_own_call_runs_as_itself(&forwarding, &dispatcher, &b_caller, &b).await;
    // What does not prove itself never runs as the device that sent it, or at all.
    unproven_forwards_are_refused(
        &forwarding,
        &dispatcher,
        &b_caller,
        &helper,
        person,
        &engine,
    )
    .await;

    engine.close().await;
    b_endpoint.close().await;
}

/// The agent's signed call, brought by another device, runs as the agent and asks under its
/// signature, never as the device that brought it.
async fn agent_call_runs_as_the_agent(
    forwarding: &RemoteDevice,
    dispatcher: &TestProvider,
    b_caller: &Caller,
    helper: &(SigningKey, DeviceCert),
    b: &(SigningKey, DeviceCert),
) {
    let agent_call = signed_request(&invoke(InvocationId::new()), &helper.1, &helper.0);
    assert!(
        matches!(
            invoke_as(forwarding, b_caller, agent_call.clone()).await,
            Ok(ReceiptStatus::Completed { .. })
        ),
        "the agent's forwarded call runs"
    );
    let ran_as = dispatcher.callers().pop().unwrap();
    assert_eq!(
        ran_as.acting().member_id(),
        Some(&tilo_member_types::MemberId::agent(
            "scribe",
            hex::encode(helper.1.user_public_key)
        )),
        "the call runs as the agent, never as the device that brought it"
    );
    assert_eq!(
        ran_as.device_public_key(),
        Some(&helper.1.device_public_key)
    );
    assert_eq!(ran_as.forwarder(), Some(&b.1.device_public_key));
    assert_eq!(dispatcher.asked().pop().unwrap().signed, agent_call.signed);
}

/// A person's signed call from another of their devices runs as that device, forwarded by the one
/// that brought it.
async fn a_persons_call_runs_as_its_device(
    forwarding: &RemoteDevice,
    dispatcher: &TestProvider,
    b_caller: &Caller,
    c: &(SigningKey, DeviceCert),
    b: &(SigningKey, DeviceCert),
    person: [u8; 32],
) {
    let person_call = signed_request(&invoke(InvocationId::new()), &c.1, &c.0);
    assert!(matches!(
        invoke_as(forwarding, b_caller, person_call).await,
        Ok(ReceiptStatus::Completed { .. })
    ));
    let ran_as = dispatcher.callers().pop().unwrap();
    assert_eq!(ran_as.user_public_key(), Some(&person));
    assert_eq!(ran_as.device_public_key(), Some(&c.1.device_public_key));
    assert_eq!(ran_as.forwarder(), Some(&b.1.device_public_key));
}

/// The bringing device's own unsigned call runs as that device, forwarded by nobody.
async fn the_devices_own_call_runs_as_itself(
    forwarding: &RemoteDevice,
    dispatcher: &TestProvider,
    b_caller: &Caller,
    b: &(SigningKey, DeviceCert),
) {
    let own = naming(invoke(InvocationId::new()), b_caller.acting());
    assert!(matches!(
        invoke_as(forwarding, b_caller, own).await,
        Ok(ReceiptStatus::Completed { .. })
    ));
    let ran_as = dispatcher.callers().pop().unwrap();
    assert_eq!(ran_as.device_public_key(), Some(&b.1.device_public_key));
    assert_eq!(ran_as.forwarder(), None);
}

/// The agent's call forwarded without its signature names the agent and proves nothing: it is
/// refused, and never runs as the device that sent it. A device the workspace does not know
/// brings the agent's signed call: it is refused too, and nothing runs either way.
async fn unproven_forwards_are_refused(
    forwarding: &RemoteDevice,
    dispatcher: &TestProvider,
    b_caller: &Caller,
    helper: &(SigningKey, DeviceCert),
    person: [u8; 32],
    engine: &Engine,
) {
    let agent = Caller::delegated_agent("scribe", person, helper.1.device_public_key);
    let before = dispatcher.asked().len();
    let unsigned = naming(invoke(InvocationId::new()), agent.acting());
    let refused = invoke_as(forwarding, b_caller, unsigned)
        .await
        .expect_err("an unsigned forward of another caller's call is refused");
    assert!(
        refusal(&refused).contains("carries no signature of theirs"),
        "{refused:?}"
    );
    assert_eq!(dispatcher.asked().len(), before, "nothing ran");

    let stranger = device(&SigningKey::from_bytes(&[9; 32]), 6);
    let (stranger_endpoint, stranger_service) = asking(&stranger.0).await;
    let from_outside = stranger_service.remote(&stranger_endpoint, &engine.addr(), WORKSPACE);
    let carried = signed_request(&invoke(InvocationId::new()), &helper.1, &helper.0);
    let refused = invoke_as(&from_outside, b_caller, carried)
        .await
        .expect_err("a device outside the workspace forwards nobody's call");
    assert!(
        refusal(&refused).contains("outside the workspace"),
        "{refused:?}"
    );
    assert_eq!(dispatcher.asked().len(), before, "nothing ran");
    stranger_endpoint.close().await;
}

/// A device's engine jobs, as the device that runs them answers them: `running` until the job
/// moves, `done` after.
#[derive(Clone)]
struct JobReads {
    moved: Arc<tokio::sync::watch::Sender<u64>>,
}

impl JobReads {
    /// The reading of `record` in `state`.
    fn reading(record: &str, state: &str) -> String {
        serde_json::json!({
            "revision": state,
            "records": [ { "record": record, "value": { "id": record, "state": state } } ]
        })
        .to_string()
    }
}

impl ActionProvider for JobReads {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> tilo_actions::remote::Pending<Result<tilo_actions::remote::CataloguePage, ActionCallError>>
    {
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::CataloguePage::default()))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, ActionCallError>,
    > {
        tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Rejected {
                error: ActionCallError::UnknownAction,
            },
        }))
    }

    fn observe(
        &self,
        _: &Caller,
        _: &InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &Caller,
        _: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn resource_reads(&self) -> Option<Arc<dyn tilo_actions::remote::ResourceReads>> {
        Some(Arc::new(self.clone()))
    }
}

impl tilo_actions::remote::ResourceReads for JobReads {
    fn read<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a tilo_actions::remote::ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, tilo_actions::remote::ResourceCopy> {
        Box::pin(std::future::ready(Err(ActionCallError::Unavailable {
            reason: "this test answers held reads only".into(),
        })))
    }

    /// A held read waits here until the job moves: the wait is the device's, not the asker's.
    fn answer_read(
        self: Arc<Self>,
        _: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> tilo_actions::remote::ReadingAnswer {
        let mut moved = self.moved.subscribe();
        let seen = *moved.borrow_and_update();
        let held = read.after.as_deref() == Some("running") && read.wait_ms.is_some();
        let record = read.record;
        Box::pin(async move {
            if held {
                let _ = moved.changed().await;
            }
            let state = if *moved.borrow() > seen {
                "done"
            } else {
                "running"
            };
            RemoteActionMessage::ResourceReading {
                correlation,
                result: Ok(JobReads::reading(&record, state)),
            }
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_jobs_read_from_another_device_is_answered_there_and_followed_to_its_end() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let moved = Arc::new(tokio::sync::watch::Sender::new(0_u64));
    let engine = Engine::serving_provider(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        Arc::new(JobReads {
            moved: Arc::clone(&moved),
        }),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_action_service) = asking(&b.0).await;
    let remote = Arc::new(b_action_service.remote(&b_endpoint, &engine.addr(), WORKSPACE));
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);
    let address = tilo_actions::remote::device_job_address(&engine.device(), "render-1");

    // A asks B for the job it runs: B answers with what the job says now.
    let request = tilo_actions::remote::ResourceReadRequest {
        workspace: address.workspace.clone(),
        app: address.app.clone(),
        record: address.record.clone(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: tilo_actions::remote::ReadAnswer::Reading,
    };
    let answer = tokio::time::timeout(
        PATIENCE,
        Arc::clone(&remote).answer_read(&caller, 0, request),
    )
    .await
    .expect("the device answers");
    let RemoteActionMessage::ResourceReading { result, .. } = answer else {
        panic!("a read is answered by a reading");
    };
    let reading: Value = serde_json::from_str(&result.expect("B reads its job")).unwrap();
    assert_eq!(reading["records"][0]["value"]["state"], "running");

    // A holds a read of it: when B's job moves to its end, B answers A with it.
    let request = tilo_actions::remote::ResourceReadRequest {
        workspace: address.workspace.clone(),
        app: address.app.clone(),
        record: address.record.clone(),
        joined: false,
        after: Some("running".to_owned()),
        wait_ms: Some(10_000),
        answer: tilo_actions::remote::ReadAnswer::Reading,
    };
    let held =
        tokio::spawn(async move { Arc::clone(&remote).answer_read(&caller, 0, request).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    moved.send_modify(|seen| *seen = 1);
    let answer = tokio::time::timeout(PATIENCE, held)
        .await
        .expect("the held read wakes")
        .expect("the read ran");
    let RemoteActionMessage::ResourceReading { result, .. } = answer else {
        panic!("a read is answered by a reading");
    };
    let reading: Value = serde_json::from_str(&result.expect("B reads its job")).unwrap();
    assert_eq!(
        reading["records"][0]["value"]["state"], "done",
        "B woke A's held read when its job ended"
    );

    engine.close().await;
    b_endpoint.close().await;
}
