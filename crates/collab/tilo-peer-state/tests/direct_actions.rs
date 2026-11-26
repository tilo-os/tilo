//! One device asks another device of its workspace for an action directly
//! over iroh, with no workspace host, relay or network actor in between: the
//! request reaches the device it names, is answered there as the caller the
//! engine authorized, and is refused from the moment the asking device may
//! no longer act.

use crate::common::{addr, device, localhost_endpoint, workspace};
use tilo_peer_state::join::WorkspaceId;

use ed25519_dalek::SigningKey;
use iroh::endpoint::RecvStream;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr};
use neighbour_wire::actions::{ActionServiceFrame, DeviceAction, HandedAgent, SignedDeviceAction};
use std::sync::Arc;
use std::time::Duration;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget, TestProvider,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionService, Bounds, RemoteDevice};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::join::{
    InviteGrant, JoinChallenge, Role, VerifiedRedemption, decode_and_verify_invitation,
    issue_invitation, sign_join_proof, verify_join_proof,
};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
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

fn ns(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

/// The moment this test reads, the clock the engines read with it: a test
/// that holds the wall clock holds both sides of every exchange.
fn now_ns() -> u64 {
    ns(tilo_clock::wall())
}

/// One invocation of an action, as a caller on another device sends it.
fn invoke(invocation: InvocationId) -> InvokeRequest {
    InvokeRequest {
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
}

impl Engine {
    async fn serving(
        replica: Replica,
        key: SigningKey,
        dispatcher: Arc<TestProvider>,
        bounds: Bounds,
    ) -> Self {
        let endpoint = localhost_endpoint(&key).await;
        let action_service = ActionService::with_bounds(key.clone(), dispatcher.clone(), bounds);
        action_service.hold(replica.clone());
        let router = Router::builder(endpoint.clone())
            .accept(ACTIONS_ALPN, action_service)
            .spawn();
        Self {
            key,
            replica,
            endpoint,
            router,
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
            agent: None,
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

    // A request naming a third device is not this one's to answer.
    let elsewhere = SignedDeviceAction::sign(
        &DeviceAction {
            workspace: WORKSPACE.into(),
            request_id: uuid::Uuid::from_u128(2),
            source: b.1.device_public_key,
            target: c.1.device_public_key,
            message: catalogue(),
            placement: None,
            agent: None,
        },
        now_ns(),
        &b.0,
    )
    .unwrap();
    let answer = send(&b_endpoint, engine.addr(), &elsewhere)
        .await
        .expect("a refused request is answered");
    let RemoteActionMessage::CatalogueResponse {
        result: Err(error), ..
    } = answer
    else {
        panic!("the request naming another device is refused, got {answer:?}");
    };
    assert_eq!(refusal(&error), "the request names another device");

    engine.close().await;
    b_endpoint.close().await;
    c_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_device_is_refused_from_that_moment_on() {
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
    assert_eq!(queried(&remote, &caller).await, Ok(()));

    let mut entry = RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: b.1.device_public_key,
        revoked_at_ns: 1,
        reason: RevocationReason::Lost,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &root).unwrap();
    let now = tilo_clock::wall();
    assert!(
        engine
            .replica
            .write(|peer| peer
                .governance_mut()
                .publish_governed_revocations(&[entry], &a.0, now))
            .unwrap()
    );

    let refused = queried(&remote, &caller).await.unwrap_err();
    assert_eq!(refusal(&refused), "the device is revoked");
    assert_eq!(
        dispatcher.callers().len(),
        1,
        "the revoked device's next request never reaches the dispatcher"
    );

    engine.close().await;
    b_endpoint.close().await;
}

/// A device of the person's that names the agent it handed a request to is answered as that
/// agent, for the person, in the session it named: never as the person themselves. A device that
/// is already not its person, an agent's own, hands nothing on and is refused before anything
/// runs.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_handed_to_an_agent_is_the_agents_and_never_more_than_its_person() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let person = root.verifying_key().to_bytes();
    let a = device(&root, 2);
    let b = device(&root, 3);
    let helper = agent_device(&root, 4);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let claude = || HandedAgent {
        client: "Claude Code".into(),
        session: "laptop-session-1".into(),
    };

    let (b_endpoint, b_action_service) = asking(&b.0).await;
    let remote =
        b_action_service.remote_for_agent(&b_endpoint, &engine.addr(), WORKSPACE, claude());
    let caller = Caller::authenticated(person, b.1.device_public_key);
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    let answered_as = dispatcher.callers()[0].clone();
    let agent = tilo_actions::remote::device_agent_id(&person, "Claude Code");
    assert_eq!(answered_as.acting().agent_id(), Some(agent.as_str()));
    assert!(!answered_as.acting().is_person(), "never the person");
    assert!(
        answered_as.is_session_delegated(),
        "handed by their own device"
    );
    assert_eq!(
        answered_as.acting_for_person(),
        Some(hex::encode(person).as_str()),
        "for the person"
    );
    assert_eq!(answered_as.agent_name(), Some("Claude Code"));
    assert_eq!(
        answered_as
            .acting()
            .agent_session
            .as_ref()
            .map(tilo_action_policy::AgentSessionId::as_str),
        Some("laptop-session-1")
    );
    assert_eq!(
        answered_as.device_public_key(),
        Some(&b.1.device_public_key)
    );

    let now = tilo_clock::wall();
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
        ns(now.checked_sub(Duration::from_secs(1)).unwrap()),
        ns(now.checked_add(Duration::from_secs(3600)).unwrap()),
    )
    .unwrap();
    assert!(
        engine
            .replica
            .write(|peer| peer.governance_mut().admit_agent(
                helper.1.clone(),
                delegation,
                &a.0,
                now
            ))
            .unwrap()
    );
    let (helper_endpoint, helper_action_service) = asking(&helper.0).await;
    let remote = helper_action_service.remote_for_agent(
        &helper_endpoint,
        &engine.addr(),
        WORKSPACE,
        claude(),
    );
    let caller = Caller::delegated_agent("scribe", person, helper.1.device_public_key);
    assert_eq!(
        queried(&remote, &caller).await,
        Err(ActionCallError::NotPermitted)
    );
    assert_eq!(dispatcher.callers().len(), 1, "nothing ran for it");

    engine.close().await;
    b_endpoint.close().await;
    helper_endpoint.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_who_left_the_workspace_is_refused() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let guest_root = SigningKey::from_bytes(&[3; 32]);
    let g = device(&guest_root, 5);
    let roster = vec![a.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let engine = Engine::serving(
        Replica::new(workspace(WORKSPACE, &a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;

    // The guest joins through an invitation the workspace's authority issues.
    let now = tilo_clock::wall();
    let redemption = guest(&a, &guest_root, &g, now);
    assert!(
        engine
            .replica
            .write(|peer| peer
                .governance_mut()
                .admit_invited_member(&redemption, &a.0, now))
            .unwrap()
    );

    let (g_endpoint, g_action_service) = asking(&g.0).await;
    let remote = g_action_service.remote(&g_endpoint, &engine.addr(), WORKSPACE);
    let caller =
        Caller::authenticated(guest_root.verifying_key().to_bytes(), g.1.device_public_key);
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    assert_eq!(
        dispatcher.callers()[0].user_public_key(),
        Some(&guest_root.verifying_key().to_bytes()),
        "an admitted guest acts as their own person"
    );

    // Once they are no longer a member, their device is nobody here.
    assert!(
        engine
            .replica
            .write(|peer| peer.governance_mut().remove_member(
                guest_root.verifying_key().to_bytes(),
                &a.0,
                now
            ))
            .unwrap()
    );
    let refused = queried(&remote, &caller).await.unwrap_err();
    assert_eq!(
        refusal(&refused),
        "the person is not a member of this workspace"
    );
    assert_eq!(dispatcher.callers().len(), 1);

    engine.close().await;
    g_endpoint.close().await;
}

/// The redemption of an invitation the workspace's authority issued to
/// `guest`, as its device proves it is the invited one.
fn guest(
    a: &(SigningKey, DeviceCert),
    guest_root: &SigningKey,
    guest: &(SigningKey, DeviceCert),
    now: SystemTime,
) -> VerifiedRedemption {
    let issued = issue_invitation(
        InviteGrant {
            workspace_id: WorkspaceId::parse(WORKSPACE).unwrap(),
            issuer: a.1.clone(),
            role: Role::Editor,
            nonce: [43; 16],
            recipient_user_public_key: Some(guest_root.verifying_key().to_bytes()),
            invitee_name: Some("Guest".into()),
            expires_at_ns: ns(now).saturating_add(10_000_000_000),
            transport: Vec::new(),
        },
        &a.0,
    )
    .unwrap();
    let join = decode_and_verify_invitation(
        &issued,
        Some(guest_root.verifying_key().to_bytes()),
        &RevocationList::new(),
        ns(now),
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &guest.1,
        [44; 32],
        [45; 32],
        ns(now).saturating_add(1_000_000_000),
        ns(now),
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &guest.0).unwrap();
    verify_join_proof(
        &proof,
        &challenge,
        &guest.1,
        &RevocationList::new(),
        ns(now),
    )
    .unwrap()
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
                &a.0,
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
        Some(&tilo_action_policy::MemberId::agent("scribe")),
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
        "this device does not hold that workspace"
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
                    app: "test.app".into(),
                    record: "tab:1".into(),
                    joined: false,
                    after: None,
                    wait_ms: None,
                },
            },
            placement: None,
            agent: None,
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

#[tokio::test(flavor = "multi_thread")]
async fn a_device_asking_from_an_endpoint_keyed_apart_is_answered_once_named() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestProvider::new());
    let endpoint = localhost_endpoint(&a.0).await;
    let action_service = ActionService::new(a.0.clone(), dispatcher.clone());
    action_service.hold(Replica::new(workspace(WORKSPACE, &a, &roster)));
    let serving = Router::builder(endpoint.clone())
        .accept(ACTIONS_ALPN, action_service.clone())
        .spawn();

    // B signs as its device but asks from an endpoint keyed by another key,
    // as a Tilo server does.
    let apart = SigningKey::from_bytes(&[30; 32]);
    let b_endpoint = localhost_endpoint(&apart).await;
    let b_action_service = ActionService::new(b.0.clone(), Arc::new(TestProvider::new()));
    let remote = b_action_service.remote(&b_endpoint, &addr(&endpoint), WORKSPACE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);
    assert_eq!(
        queried(&remote, &caller).await,
        Err(ActionCallError::NotPermitted),
        "an endpoint nobody named asks as the device it is keyed by"
    );
    assert!(dispatcher.callers().is_empty());

    action_service.reach_device_at(b.1.device_public_key, apart.verifying_key().to_bytes());
    let remote = b_action_service.remote(&b_endpoint, &addr(&endpoint), WORKSPACE);
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    assert_eq!(
        dispatcher.callers()[0].device_public_key(),
        Some(&b.1.device_public_key),
        "answered as the device it named"
    );

    serving.shutdown().await.unwrap();
    endpoint.close().await;
    b_endpoint.close().await;
}

/// A device whose endpoint is keyed apart from its device key, as a Tilo server's is, answers a
/// request addressed to that endpoint once it says the endpoint is its own: what a computer
/// logging in with a ticket names, since the ticket carries no device key.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_answers_a_request_named_at_the_endpoint_it_is_reached_at() {
    use tilo_device_identity::login::SCOPE;

    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let apart = SigningKey::from_bytes(&[32; 32]);
    let endpoint = localhost_endpoint(&apart).await;
    let action_service = ActionService::new(a.0.clone(), Arc::new(TestProvider::new()));
    action_service.admit_uncertified(SCOPE, Arc::new(TestProvider::new()));
    let serving = Router::builder(endpoint.clone())
        .accept(ACTIONS_ALPN, action_service.clone())
        .spawn();

    // A computer that knows no device key, only the endpoint: the ticket it holds names one.
    let desk = device(&root, 4);
    let (desk_endpoint, desk_service) = asking(&desk.0).await;
    let remote = desk_service.remote(&desk_endpoint, &addr(&endpoint), SCOPE);
    let who = Caller::visitor(desk.0.verifying_key().to_bytes());
    let refused = invoked(&remote, &who, InvocationId::new())
        .await
        .expect_err("an endpoint nobody claimed asks as the device it is keyed by");
    assert!(refusal(&refused).contains("another device"), "{refused:?}");

    action_service.reach_device_at(
        a.0.verifying_key().to_bytes(),
        apart.verifying_key().to_bytes(),
    );
    assert_eq!(
        invoked(&remote, &who, InvocationId::new()).await,
        Ok(ReceiptStatus::Completed {
            output: ActionOutput::default(),
        }),
        "the device answers a request that names the endpoint it is reached at"
    );

    serving.shutdown().await.unwrap();
    endpoint.close().await;
    desk_endpoint.close().await;
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

#[tokio::test(flavor = "multi_thread")]
async fn a_visitor_does_what_the_entry_opens_and_no_more_and_not_too_often() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let mut peer = workspace(WORKSPACE, &a, std::slice::from_ref(&a.1));
    // The workspace takes people by invitation and keeps their requests.
    peer.governance_mut()
        .put_governed_entry(
            &tilo_action_policy::EntryPolicy {
                listing: None,
                invitation_only: true,
                wait_list_max: 100,
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
    let ask = |request: InvokeRequest| {
        let remote = &remote;
        let nobody = &nobody;
        async move {
            tokio::time::timeout(PATIENCE, ActionProvider::invoke(remote, nobody, request))
                .await
                .expect("the device answers")
                .expect("the asker is not dropped")
                .map(|receipt| receipt.status)
        }
    };

    // Anything but what the entry opens is refused, as to a stranger, before any dispatcher
    // meets the visitor.
    assert_eq!(
        ask(invoke(InvocationId::new())).await,
        Err(ActionCallError::NotPermitted),
        "a visitor may not change a note"
    );
    assert_eq!(
        ask(at_the_entry("invite")).await,
        Err(ActionCallError::NotPermitted),
        "nor invite anyone from the wait list"
    );
    // Leaving an address, confirming it and taking it off are open to any key.
    for action in ["join", "confirm", "leave"] {
        assert!(
            matches!(
                ask(at_the_entry(action)).await,
                Ok(ReceiptStatus::Completed { .. })
            ),
            "a visitor may {action}"
        );
    }
    assert_eq!(
        queried(&remote, &nobody).await,
        Err(ActionCallError::NotPermitted),
        "nor browse the workspace's actions"
    );
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
            ask(at_the_entry("join")).await,
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
