//! One device asks another device of its workspace for an action directly
//! over iroh, with no room host, relay or network actor in between: the
//! request reaches the device it names, is answered there as the caller the
//! engine authorized, and is refused from the moment the asking device may
//! no longer act.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use iroh::endpoint::RecvStream;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use neighbour_wire::actions::{DeviceAction, SignedDeviceAction};
use std::sync::Arc;
use std::time::Duration;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CatalogueQuery, InvokeArgument,
    InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget, TestHost,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ACTIONS_ALPN, ActionPlane, Bounds, RemoteDevice};
use tilo_peer_state::direct::Replica;
use tilo_peer_state::join::{
    InviteGrant, JoinChallenge, MemberRole, VerifiedRedemption, decode_and_verify_invitation,
    issue_invitation, sign_join_proof, verify_join_proof,
};
use tilo_peer_state::{SeedState, StatePeer, seed_room};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};
use tilo_workspace_join::delegation::{Delegate, DelegateScope, Delegation};
use web_time::SystemTime;

const ROOM: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const ELSEWHERE: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";

/// How long a test waits for an answer before it counts as lost.
const PATIENCE: Duration = Duration::from_secs(20);

/// A device key `n` certified by `root` as one of its person's devices.
fn device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[n; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{n}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// A device `root` delegates to `agent`: its own key, and a certificate that
/// holds none of the person's capabilities.
fn agent_device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
    let (key, mut cert) = device(root, n);
    cert.device_label = format!("Agent: Scribe {n}");
    cert.capabilities = DeviceCapabilities {
        can_own_workspaces: false,
        can_admit_devices: false,
        can_act_as_host: false,
        can_read_all_workspaces: false,
        foreign: false,
        can_administer_devices: false,
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

fn ns(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

fn now_ns() -> u64 {
    ns(SystemTime::now())
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
    }
}

fn catalogue() -> RemoteActionMessage {
    RemoteActionMessage::CatalogueRequest {
        correlation: 1,
        query: CatalogueQuery::default(),
    }
}

/// The device that answers requests: its workspace replica, the plane that
/// serves actions out of it, and the dispatcher the plane hands them to.
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
        dispatcher: Arc<TestHost>,
        bounds: Bounds,
    ) -> Self {
        let endpoint = endpoint(&key).await;
        let plane = ActionPlane::with_bounds(key.clone(), dispatcher.clone(), bounds);
        plane.hold(replica.clone());
        let router = Router::builder(endpoint.clone())
            .accept(ACTIONS_ALPN, plane)
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

/// The workspace `a.1` creates and seeds, with `roster` certified as the
/// room's devices.
fn workspace(a: &(SigningKey, DeviceCert), roster: &[DeviceCert]) -> StatePeer {
    let now = SystemTime::now();
    let mut peer = StatePeer::new(
        ROOM,
        a.1.clone(),
        roster.to_vec(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    peer.select_admission_v5().unwrap();
    peer.install_seed(
        SeedState {
            room_changes: seed_room(ROOM, &a.1, roster).unwrap(),
            documents: Vec::new(),
        },
        &a.0,
        now,
    )
    .unwrap();
    peer
}

/// A device of `key` that only asks: its own plane, with nothing to answer.
async fn asking(key: &SigningKey) -> (Endpoint, ActionPlane) {
    let endpoint = endpoint(key).await;
    let plane = ActionPlane::new(key.clone(), Arc::new(TestHost::new()));
    (endpoint, plane)
}

/// One signed request written to `addr` by hand, and the one answer that
/// comes back: what the plane itself never writes, a test sends itself.
async fn send(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    signed: &SignedDeviceAction,
) -> Option<RemoteActionMessage> {
    let connection = endpoint.connect(addr, ACTIONS_ALPN).await.unwrap();
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    let bytes = postcard::to_allocvec(signed).unwrap();
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
async fn queried(remote: &RemoteDevice, caller: &Caller) -> Result<(), ActionHostError> {
    let answer = tokio::time::timeout(
        PATIENCE,
        ActionHost::query(remote, caller, CatalogueQuery::default()),
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
) -> Result<ReceiptStatus, ActionHostError> {
    let answer = tokio::time::timeout(
        PATIENCE,
        ActionHost::invoke(remote, caller, invoke(invocation)),
    )
    .await
    .expect("the device answers");
    answer
        .expect("the asker is not dropped")
        .map(|receipt| receipt.status)
}

/// The reason a refusal carries, for a test to read.
fn refusal(error: &ActionHostError) -> &str {
    match error {
        ActionHostError::Refused { reason } | ActionHostError::Unavailable { reason } => reason,
        other => panic!("expected a refusal with a reason, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_reaches_the_device_it_names_and_is_answered() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestHost::new());
    let engine = Engine::serving(
        Replica::new(workspace(&a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_plane) = asking(&b.0).await;
    let remote = b_plane.remote(b_endpoint.clone(), engine.addr(), ROOM);
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
        answered_as.room(),
        Some(ROOM),
        "confined to the workspace it named"
    );
    assert!(matches!(
        answered_as.principal(),
        CallerPrincipal::Person(person) if *person == hex::encode(root.verifying_key().to_bytes())
    ));
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
async fn a_device_that_is_not_the_sender_is_refused() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let b = device(&root, 3);
    let roster = vec![a.1.clone(), b.1.clone()];
    let dispatcher = Arc::new(TestHost::new());
    let engine = Engine::serving(
        Replica::new(workspace(&a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, _b_plane) = asking(&b.0).await;

    // A device of another person the workspace never admitted.
    let stranger_root = SigningKey::from_bytes(&[9; 32]);
    let c = device(&stranger_root, 10);
    let (c_endpoint, c_plane) = asking(&c.0).await;
    let stranger = c_plane.remote(c_endpoint.clone(), engine.addr(), ROOM);
    let refused = queried(
        &stranger,
        &Caller::authenticated(
            stranger_root.verifying_key().to_bytes(),
            c.1.device_public_key,
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(refused, ActionHostError::NotPermitted);

    // The device that asked answers as itself and nobody else: the owner's
    // own signature over an action naming the owner's device, sent from
    // another device, is not that device asking.
    let forged = SignedDeviceAction::sign(
        &DeviceAction {
            workspace: ROOM.into(),
            request_id: uuid::Uuid::from_u128(1),
            source: engine.device(),
            target: engine.device(),
            message: catalogue(),
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
            result: Err(ActionHostError::NotPermitted),
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
            workspace: ROOM.into(),
            request_id: uuid::Uuid::from_u128(2),
            source: b.1.device_public_key,
            target: c.1.device_public_key,
            message: catalogue(),
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
    let dispatcher = Arc::new(TestHost::new());
    let engine = Engine::serving(
        Replica::new(workspace(&a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_plane) = asking(&b.0).await;
    let remote = b_plane.remote(b_endpoint.clone(), engine.addr(), ROOM);
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
    let now = SystemTime::now();
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

#[tokio::test(flavor = "multi_thread")]
async fn a_person_who_left_the_workspace_is_refused() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let guest_root = SigningKey::from_bytes(&[3; 32]);
    let g = device(&guest_root, 5);
    let roster = vec![a.1.clone()];
    let dispatcher = Arc::new(TestHost::new());
    let engine = Engine::serving(
        Replica::new(workspace(&a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;

    // The guest joins through an invitation the workspace's authority issues.
    let now = SystemTime::now();
    let redemption = guest(&a, &guest_root, &g, now);
    assert!(
        engine
            .replica
            .write(|peer| peer
                .governance_mut()
                .admit_invited_member(&redemption, &a.0, now))
            .unwrap()
    );

    let (g_endpoint, g_plane) = asking(&g.0).await;
    let remote = g_plane.remote(g_endpoint.clone(), engine.addr(), ROOM);
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
            workspace_id: 7,
            room: ROOM.into(),
            issuer: a.1.clone(),
            role: MemberRole::Editor,
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

#[tokio::test(flavor = "multi_thread")]
async fn a_delegated_agent_acts_as_itself_and_not_once_its_window_closed() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let a = device(&root, 2);
    let helper = agent_device(&root, 4);
    let roster = vec![a.1.clone()];
    let dispatcher = Arc::new(TestHost::new());
    let engine = Engine::serving(
        Replica::new(workspace(&a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;

    let now = SystemTime::now();
    let delegation = Delegation::sign(
        &root,
        helper.1.device_public_key,
        Delegate::Agent {
            agent_id: "scribe".into(),
        },
        "Scribe",
        [10, 20, 30],
        DelegateScope {
            rooms: vec![ROOM.into()],
            apps: Vec::new(),
        },
        ns(now - Duration::from_secs(1)),
        ns(now + Duration::from_millis(300)),
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

    let (helper_endpoint, helper_plane) = asking(&helper.0).await;
    let remote = helper_plane.remote(helper_endpoint.clone(), engine.addr(), ROOM);
    let caller = Caller::delegated_agent(
        "scribe",
        root.verifying_key().to_bytes(),
        helper.1.device_public_key,
    );
    assert_eq!(queried(&remote, &caller).await, Ok(()));
    let answered_as = dispatcher.callers()[0].clone();
    assert_eq!(
        answered_as.principal(),
        &CallerPrincipal::Agent("scribe".into()),
        "the agent is the agent, never its person"
    );
    assert_eq!(
        answered_as.user_public_key(),
        Some(&root.verifying_key().to_bytes()),
        "acting for the person whose root delegated it"
    );

    // Past its window the same device is refused, though its identity stays
    // on the roster to keep the history it wrote attributed.
    tokio::time::sleep(Duration::from_millis(600)).await;
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
    let dispatcher = Arc::new(TestHost::new());
    let engine = Engine::serving(
        Replica::new(workspace(&a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds {
            changes_per_window: 1,
            window_secs: 3600,
            burst: 2,
        },
    )
    .await;
    let (b_endpoint, b_plane) = asking(&b.0).await;
    let remote = b_plane.remote(b_endpoint.clone(), engine.addr(), ROOM);
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
    let dispatcher = Arc::new(TestHost::new());
    let engine = Engine::serving(
        Replica::new(workspace(&a, &roster)),
        a.0.clone(),
        dispatcher.clone(),
        Bounds::default(),
    )
    .await;
    let (b_endpoint, b_plane) = asking(&b.0).await;

    // A workspace this device does not hold.
    let elsewhere = b_plane.remote(b_endpoint.clone(), engine.addr(), ELSEWHERE);
    let caller = Caller::authenticated(root.verifying_key().to_bytes(), b.1.device_public_key);
    let refused = queried(&elsewhere, &caller).await.unwrap_err();
    assert_eq!(
        refusal(&refused),
        "this device does not hold that workspace"
    );

    // A request the action plane does not serve is answered with a reason,
    // never left for the asker to wait on.
    let wanted = SignedDeviceAction::sign(
        &DeviceAction {
            workspace: ROOM.into(),
            request_id: uuid::Uuid::from_u128(3),
            source: b.1.device_public_key,
            target: engine.device(),
            message: RemoteActionMessage::RecordRequest {
                correlation: 5,
                query: tilo_actions::remote::RecordQuery {
                    room: ROOM.into(),
                    app: "test.app".into(),
                    record: "tab:1".into(),
                    offset: 0,
                },
            },
        },
        now_ns(),
        &b.0,
    )
    .unwrap();
    let answer = send(&b_endpoint, engine.addr(), &wanted)
        .await
        .expect("a request nothing serves is answered");
    let RemoteActionMessage::RecordResponse {
        correlation: 5,
        result: Err(error),
    } = answer
    else {
        panic!("an unserved request is refused, got {answer:?}");
    };
    assert!(
        matches!(error, ActionHostError::Unavailable { .. }),
        "{error:?}"
    );
    assert_eq!(dispatcher.callers().len(), 0);

    engine.close().await;
    b_endpoint.close().await;
}
