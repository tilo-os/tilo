//! Tenant isolation on one hosted relay.
//!
//! A hosted relay governs rooms for people who share nothing but the host:
//! two tenants, each its own governed room with its own members. These
//! tests stand the real relay up with two such rooms and drive every door a
//! member, an agent or a compute client has, asserting that one tenant
//! cannot read, write, enumerate, administer or invoke anything in the
//! other's room, that an agent holds only what its delegation names and
//! never governs, and that a removed member loses access at once. Each
//! refusal is checked against the other room's state afterwards, so a door
//! that answered "no" but acted anyway still fails.
//!
//! The suite is the `tenant_isolation::` module of the relay's `it` target;
//! t.toml's `security-isolation` check runs it nightly and on
//! demand alongside the room-access and agent-confinement suites.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "one end-to-end scenario whose relay and sessions live until it ends"
)]

use std::path::Path;
use std::time::Duration;

use tilo_action_policy::{Answer, Grant, KindSet, Subject, roles, roles::governance};
use tilo_actions::remote::{
    CatalogueQuery, InvokeArgument, InvokeRequest, ROOM_GOVERNANCE_PROVIDER, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, RemoteActionTarget, room_governance_resource,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_relay::rooms::{
    CLAIM_TOPOLOGY_NAMESPACE, CREATE_ROOM, ENTER_ROOM, GRANT_APP_AUTHORSHIP, HOST_ACTIONS_PROVIDER,
    HOST_ROOMS_RESOURCE, IssueInvitationRequest, IssuedInvitation, LIST_ROOMS,
    MEMBER_ACTIONS_PROVIDER, RECORD_EFFECT_RECEIPT, RemoveMemberRequest, RoomAdminClient,
    RoomAdminError, RoomCommand, RoomReply, RoomSummary, Rooms, member_resource,
};
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{ComputeClient, ContentFetchError, Envelope, Relay, RelayClient, RelayConfig};
use tilo_relay_protocol::delegation::{Delegate, DelegateScope, Delegation};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser};

/// How long a refused write is given to surface anywhere before its absence
/// counts as evidence.
const QUIET: Duration = Duration::from_millis(500);

/// An endpoint whose key is the device's own, as a device dials with.
async fn device_endpoint(device: &TestDevice) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.sk.to_bytes()))
        .bind()
        .await
        .unwrap()
}

async fn connect(relay: &Relay, device: &TestDevice) -> RelayClient {
    try_connect(relay, device).await.unwrap()
}

/// A connection the relay may refuse at its handshake.
async fn try_connect(
    relay: &Relay,
    device: &TestDevice,
) -> Result<RelayClient, tilo_relay::ClientError> {
    RelayClient::connect_to_pinned_host(
        device_endpoint(device).await,
        relay.node_addr(),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
}

/// The relay's first answer to `client` subscribing to `workspace_id`.
async fn subscribe(client: &RelayClient, workspace_id: u64) -> Option<Envelope> {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the relay answers the subscription", async {
        loop {
            match client.recv().await {
                Some(answer @ (Envelope::Ready { .. } | Envelope::NotAuthorised { .. })) => {
                    break Some(answer);
                }
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await
}

/// A session of `device` that the relay admitted to `workspace_id`.
async fn subscribed(relay: &Relay, device: &TestDevice, workspace_id: u64) -> RelayClient {
    let client = connect(relay, device).await;
    assert!(
        matches!(
            subscribe(&client, workspace_id).await,
            Some(Envelope::Ready { .. })
        ),
        "the room admits its own member"
    );
    client
}

/// Admit `member`'s person to `room` as `role`, through an invitation the
/// relay's own device issues and `member`'s device redeems.
async fn admit(
    relay: &Relay,
    host: &TestDevice,
    member: &TestDevice,
    room: &str,
    role: MemberRole,
) {
    let issued = invitation(relay.rooms().unwrap(), host, room, role, member).await;
    redeem(member, &issued).await.unwrap();
}

/// An invitation to `room` for `recipient`'s person, issued by `host`.
async fn invitation(
    rooms: &Rooms,
    host: &TestDevice,
    room: &str,
    role: MemberRole,
    recipient: &TestDevice,
) -> IssuedInvitation {
    let mut request_id = [0; 32];
    getrandom::fill(&mut request_id).unwrap();
    rooms
        .issue(
            room,
            &host.cert,
            IssueInvitationRequest {
                request_id,
                role,
                recipient_user_public_key: Some(recipient.cert.user_public_key),
                invitee_name: None,
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
            },
        )
        .await
        .unwrap()
}

/// `device` redeems `issued` through the address the invitation carries.
async fn redeem(device: &TestDevice, issued: &IssuedInvitation) -> Result<(), String> {
    let verified = decode_and_verify_invitation(
        &issued.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )?;
    let mut request_id = [0; 32];
    getrandom::fill(&mut request_id).unwrap();
    WorkspaceJoinClient::new(device_endpoint(device).await)
        .redeem_hint(
            &verified.grant().transport[0],
            issued.signed_invitation.clone(),
            device.cert.clone(),
            &device.sk,
            request_id,
        )
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// What a room's governance holds, as far as any door could change it.
async fn governance(rooms: &Rooms, host: &TestDevice, room: &str) -> String {
    let (generation, members) = rooms.membership(room).await.unwrap();
    let pending = rooms.pending(room, &host.cert).await.unwrap();
    let summary = rooms.summary(room).await.unwrap();
    format!("{generation} {members:?} {pending:?} {:?}", summary.members)
}

/// The relay's answer to `command` from `admin`. A transport failure is
/// neither a grant nor a refusal, so the exchange is tried again, a few
/// times, before its error is the answer; a refusal or a grant is final.
async fn administer(
    admin: &RoomAdminClient,
    relay: &Relay,
    command: RoomCommand,
) -> Result<RoomReply, RoomAdminError> {
    for _ in 1..3 {
        match admin.send(relay.node_addr(), command.clone()).await {
            Err(RoomAdminError::Transport(_) | RoomAdminError::Timeout) => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            answer => return answer,
        }
    }
    admin.send(relay.node_addr(), command).await
}

/// `admin` asks the administration door, whose one remaining command is
/// removing a member, to remove `victim` from `room`: it must be refused.
async fn removal_refused(
    who: &str,
    admin: &RoomAdminClient,
    relay: &Relay,
    room: &RoomSummary,
    victim: [u8; 32],
) {
    let answer = administer(
        admin,
        relay,
        RoomCommand::Remove {
            room: room.room.clone(),
            request: RemoveMemberRequest {
                request_id: [0xA4; 32],
                user_public_key: victim,
            },
        },
    )
    .await;
    assert!(
        matches!(answer, Err(RoomAdminError::Refused(_))),
        "{who} must be refused removing a member of {}: {answer:?}",
        room.name
    );
}

/// An invocation of `action` on `resource` of `provider` with `argument`.
fn invocation(
    provider: &str,
    resource: String,
    action: &str,
    argument: serde_json::Value,
) -> RemoteActionMessage {
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.into(),
            address: ActionAddress::new(resource, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    })
}

/// Whether `answer` is a receipt of an action that ran.
fn completed(answer: &RemoteActionMessage) -> bool {
    matches!(
        answer,
        RemoteActionMessage::Receipt(receipt)
            if matches!(receipt.status, ReceiptStatus::Completed { .. })
    )
}

/// Whether `answer` is a receipt of an action the relay refused.
fn rejected(answer: &RemoteActionMessage) -> bool {
    matches!(
        answer,
        RemoteActionMessage::Receipt(receipt)
            if matches!(receipt.status, ReceiptStatus::Rejected { .. })
    )
}

/// A retained delta carrying `marker`, as a device publishes one.
fn retained_delta(marker: &[u8]) -> Vec<u8> {
    tilo_relay::coedit::CoeditPayload::RetainedDelta {
        channel: [9; 32],
        body: marker.to_vec(),
    }
    .encode()
    .unwrap()
}

fn persisted(root: &Path, marker: &[u8]) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            persisted(&path, marker)
        } else {
            std::fs::read(&path).is_ok_and(|bytes| carries(&bytes, marker))
        }
    })
}

/// Everything `client` receives until it has been quiet for [`QUIET`], or
/// its session ends.
async fn received_until_quiet(client: &RelayClient) -> Vec<Envelope> {
    let mut received = Vec::new();
    while let Ok(Some(envelope)) = tokio::time::timeout(QUIET, client.recv()).await {
        received.push(envelope);
        assert!(received.len() < 10_000, "an unbounded stream of envelopes");
    }
    received
}

fn carries(bytes: &[u8], marker: &[u8]) -> bool {
    bytes.windows(marker.len()).any(|window| window == marker)
}

/// Whatever `client` receives within [`QUIET`], or `None`.
async fn next_within_quiet(client: &RelayClient) -> Option<Envelope> {
    tokio::time::timeout(QUIET, client.recv())
        .await
        .ok()
        .flatten()
}

/// A remote action from `client` to the relay's own device `host`, in
/// `workspace_id`.
fn to_host(
    client: &RelayClient,
    host: &TestDevice,
    workspace_id: u64,
    message: RemoteActionMessage,
) {
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: host.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(message).encode().unwrap(),
        })
        .unwrap();
}

/// The relay's answer to the remote action `client` sent last.
async fn action_answer(client: &RelayClient) -> RemoteActionMessage {
    tilo_test_env::within("the relay answers the action", async {
        loop {
            match client.recv().await.expect("the session stays open") {
                Envelope::RemoteAction { frame, .. } => {
                    break RemoteActionFrame::decode(&frame).unwrap().message;
                }
                Envelope::RemoteActionUndeliverable { reason, .. } => {
                    panic!("the relay did not answer: {reason}")
                }
                _ => {}
            }
        }
    })
    .await
}

fn invite_action(room: &str) -> RemoteActionMessage {
    let mut argument = serde_json::Map::new();
    argument.insert("role".into(), "viewer".into());
    RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget::room_governance(room, "invite"),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    })
}

/// The governance resources `client`'s catalogue lists in `workspace_id`.
async fn governance_resources(
    client: &RelayClient,
    host: &TestDevice,
    workspace_id: u64,
) -> Vec<String> {
    to_host(
        client,
        host,
        workspace_id,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                provider: Some(ROOM_GOVERNANCE_PROVIDER.into()),
                ..CatalogueQuery::default()
            },
        },
    );
    let RemoteActionMessage::CatalogueResponse { result, .. } = action_answer(client).await else {
        panic!("a catalogue request is answered with a page");
    };
    result
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.resource.id)
        .collect()
}

/// A hosted relay with two tenants' rooms: Acme, where Ann invites, and
/// Globex, where Bob edits and an invitation for Carol waits.
struct TwoTenants {
    state: tempfile::TempDir,
    provider: TestUser,
    host: TestDevice,
    relay: Relay,
    acme: RoomSummary,
    globex: RoomSummary,
    ann: TestUser,
    ann_laptop: TestDevice,
    bob: TestUser,
    bob_phone: TestDevice,
    carol_phone: TestDevice,
    carol_invitation: IssuedInvitation,
}

impl TwoTenants {
    async fn start() -> Self {
        let provider = TestUser::fresh("Provider");
        let host = provider.enrol("hosted-relay", DeviceCapabilities::host());
        let ann = TestUser::fresh("Ann of Acme");
        let ann_laptop = ann.enrol("ann-laptop", DeviceCapabilities::primary());
        let bob = TestUser::fresh("Bob of Globex");
        let bob_phone = bob.enrol("bob-phone", DeviceCapabilities::member());
        let carol = TestUser::fresh("Carol, invited to Globex");
        let carol_phone = carol.enrol("carol-phone", DeviceCapabilities::member());
        let state = tempfile::tempdir().unwrap();
        let relay = Relay::start(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: provider.user_pk,
                device_signing_key: host.sk.clone(),
                device_cert: host.cert.clone(),
                revocations: RevocationList::new(),
            },
        )
        .await
        .unwrap();
        let rooms = relay.rooms().unwrap();
        let acme = rooms.create("Acme").await.unwrap();
        let globex = rooms.create("Globex").await.unwrap();
        assert_ne!(acme.workspace_id, globex.workspace_id);

        // Ann invites in Acme: the owner grants her the room's `invite`
        // governance action, as an owner hands a member a part of running
        // their room.
        admit(&relay, &host, &ann_laptop, &acme.room, MemberRole::Editor).await;
        let who = hex::encode(ann.user_pk);
        let grant = Grant::new(
            format!("tenant-isolation:invite:{who}"),
            Subject::Person(who),
            roles::governance_action(&acme.room, governance::INVITE),
            KindSet::ALL,
            Answer::Allow,
            Subject::Person(hex::encode(provider.user_pk)),
        );
        rooms
            .put_room_grants(&acme.room, &host.cert, [0x11; 32], vec![grant])
            .await
            .unwrap();
        admit(&relay, &host, &bob_phone, &globex.room, MemberRole::Editor).await;
        let carol_invitation =
            invitation(rooms, &host, &globex.room, MemberRole::Editor, &carol_phone).await;
        Self {
            state,
            provider,
            host,
            relay,
            acme,
            globex,
            ann,
            ann_laptop,
            bob,
            bob_phone,
            carol_phone,
            carol_invitation,
        }
    }

    fn rooms(&self) -> Rooms {
        self.relay.rooms().unwrap().clone()
    }

    async fn globex_governance(&self) -> String {
        governance(&self.rooms(), &self.host, &self.globex.room).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_tenants_inviter_reaches_nothing_in_another_tenants_room() {
    let t = TwoTenants::start().await;
    let rooms = t.rooms();
    let before = t.globex_governance().await;
    let ann = subscribed(&t.relay, &t.ann_laptop, t.acme.workspace_id).await;

    // Positive control: Ann governs her own room through the same actions.
    let own = invocation(
        ROOM_GOVERNANCE_PROVIDER,
        room_governance_resource(&t.acme.room),
        "invitation.list",
        serde_json::json!({}),
    );
    to_host(&ann, &t.host, t.acme.workspace_id, own);
    let own = action_answer(&ann).await;
    assert!(
        completed(&own),
        "Ann lists her own room's invitations: {own:?}"
    );

    // Globex's governance, the host's own operations on Globex and a
    // member's own business there: every one rejected, from Ann's room.
    let globex = t.globex.room.as_str();
    let probes = [
        (
            ROOM_GOVERNANCE_PROVIDER,
            room_governance_resource(globex),
            "invite",
            serde_json::json!({ "role": "viewer" }),
        ),
        (
            ROOM_GOVERNANCE_PROVIDER,
            room_governance_resource(globex),
            "invitation.list",
            serde_json::json!({}),
        ),
        (
            ROOM_GOVERNANCE_PROVIDER,
            room_governance_resource(globex),
            "invitation.cancel",
            serde_json::json!({ "invite": hex::encode(t.carol_invitation.invite_id) }),
        ),
        (
            ROOM_GOVERNANCE_PROVIDER,
            room_governance_resource(globex),
            governance::SET_ROLE,
            serde_json::json!({ "person": hex::encode(t.bob.user_pk), "role": "viewer" }),
        ),
        (
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            LIST_ROOMS,
            serde_json::json!({}),
        ),
        (
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            CREATE_ROOM,
            serde_json::json!({ "name": "Squatter" }),
        ),
        (
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            ENTER_ROOM,
            serde_json::json!({ "room": globex }),
        ),
        (
            HOST_ACTIONS_PROVIDER,
            HOST_ROOMS_RESOURCE.to_owned(),
            GRANT_APP_AUTHORSHIP,
            serde_json::json!({ "room": globex, "app": "so.tilo.notes" }),
        ),
        (
            MEMBER_ACTIONS_PROVIDER,
            member_resource(globex),
            CLAIM_TOPOLOGY_NAMESPACE,
            serde_json::json!({ "salt": 165 }),
        ),
        (
            MEMBER_ACTIONS_PROVIDER,
            member_resource(globex),
            RECORD_EFFECT_RECEIPT,
            serde_json::json!({
                "invocation": uuid::Uuid::new_v4().to_string(),
                "redacted": r#"{"status":"completed"}"#,
            }),
        ),
    ];
    for (provider, resource, action, argument) in probes {
        to_host(
            &ann,
            &t.host,
            t.acme.workspace_id,
            invocation(provider, resource, action, argument),
        );
        let answer = action_answer(&ann).await;
        assert!(
            rejected(&answer),
            "Ann ran {provider} {action} on Globex: {answer:?}"
        );
    }

    // The administration door's one command, removing a member, is refused.
    let ann_admin = RoomAdminClient::new(
        device_endpoint(&t.ann_laptop).await,
        t.ann_laptop.cert.clone(),
    );
    removal_refused(
        "Acme's inviter",
        &ann_admin,
        &t.relay,
        &t.globex,
        t.bob.user_pk,
    )
    .await;

    // Nothing of that changed Globex.
    assert_eq!(t.globex_governance().await, before);
    assert_eq!(rooms.list().await.len(), 2, "no room was created");

    // An invitation Globex issued to someone else admits nobody else.
    assert!(
        redeem(&t.ann_laptop, &t.carol_invitation).await.is_err(),
        "an invitation bound to Carol's person admits Ann"
    );
    assert_eq!(t.globex_governance().await, before);
    // Carol herself still redeems it: the refusal was about Ann, not the
    // invitation.
    redeem(&t.carol_phone, &t.carol_invitation).await.unwrap();
    t.relay.shutdown().await;
    drop(t.state);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_tenants_member_cannot_read_write_or_invoke_in_another_tenants_room() {
    let t = TwoTenants::start().await;
    let before = t.globex_governance().await;
    let bob_in_globex = subscribed(&t.relay, &t.bob_phone, t.globex.workspace_id).await;
    received_until_quiet(&bob_in_globex).await;

    // Reading: Ann's subscription to Globex is refused outright.
    let ann_probe = connect(&t.relay, &t.ann_laptop).await;
    assert!(
        matches!(
            subscribe(&ann_probe, t.globex.workspace_id).await,
            Some(Envelope::NotAuthorised { .. }) | None
        ),
        "Ann reads Globex"
    );
    drop(ann_probe);

    // Writing: a delta Ann addresses to Globex from her own room's session
    // reaches no Globex member and is persisted nowhere on the relay. (The
    // relay may end the session that sent it; that is a refusal too.)
    let marker = b"acme-writes-into-globex-7f3c";
    let ann_writer = subscribed(&t.relay, &t.ann_laptop, t.acme.workspace_id).await;
    ann_writer
        .send(Envelope::Delta {
            workspace_id: t.globex.workspace_id,
            author_device_public_key: t.ann_laptop.device_pubkey(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: retained_delta(marker),
            signature: [0; 64],
        })
        .unwrap();
    let leaked = received_until_quiet(&bob_in_globex).await;
    assert!(
        !leaked.iter().any(|envelope| matches!(
            envelope,
            Envelope::Delta { delta, .. } if carries(delta, marker)
        )),
        "Bob received Ann's write: {leaked:?}"
    );
    assert!(
        !persisted(t.state.path(), marker),
        "Ann's write was persisted"
    );
    drop(ann_writer);

    // Invoking: Ann's catalogue in her own room lists Acme's governance and
    // nothing of Globex's, and invoking Globex's invite from her room does
    // nothing.
    let ann_in_acme = subscribed(&t.relay, &t.ann_laptop, t.acme.workspace_id).await;
    let resources = governance_resources(&ann_in_acme, &t.host, t.acme.workspace_id).await;
    assert!(
        resources.contains(&room_governance_resource(&t.acme.room)),
        "Ann's own room offers her its governance: {resources:?}"
    );
    assert!(
        !resources.contains(&room_governance_resource(&t.globex.room)),
        "Ann's catalogue enumerates Globex: {resources:?}"
    );
    to_host(
        &ann_in_acme,
        &t.host,
        t.acme.workspace_id,
        invite_action(&t.globex.room),
    );
    let RemoteActionMessage::Receipt(receipt) = action_answer(&ann_in_acme).await else {
        panic!("an invocation is answered with a receipt");
    };
    assert!(
        matches!(receipt.status, ReceiptStatus::Rejected { .. }),
        "Ann invited into Globex: {:?}",
        receipt.status
    );
    drop(ann_in_acme);

    // Addressed to Globex's workspace, the same action is not run.
    let ann_foreign = subscribed(&t.relay, &t.ann_laptop, t.acme.workspace_id).await;
    to_host(
        &ann_foreign,
        &t.host,
        t.globex.workspace_id,
        invite_action(&t.globex.room),
    );
    let answer = tilo_test_env::within("the relay answers Ann's foreign action", async {
        loop {
            match ann_foreign.recv().await {
                Some(
                    answer @ (Envelope::RemoteAction { .. }
                    | Envelope::RemoteActionUndeliverable { .. }
                    | Envelope::NotAuthorised { .. }),
                ) => break Some(answer),
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await;
    if let Some(Envelope::RemoteAction { frame, .. }) = answer {
        let message = RemoteActionFrame::decode(&frame).unwrap().message;
        assert!(
            !matches!(
                &message,
                RemoteActionMessage::Receipt(receipt)
                    if matches!(receipt.status, ReceiptStatus::Completed { .. })
            ),
            "Ann invoked an action in Globex's workspace: {message:?}"
        );
    }
    drop(ann_foreign);

    // Nor may Ann address a Globex member's device directly.
    let ann_direct = subscribed(&t.relay, &t.ann_laptop, t.acme.workspace_id).await;
    ann_direct
        .send(Envelope::RemoteAction {
            workspace_id: t.globex.workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: t.bob_phone.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(invite_action(&t.globex.room))
                .encode()
                .unwrap(),
        })
        .unwrap();
    let leaked = received_until_quiet(&bob_in_globex).await;
    assert!(
        !leaked
            .iter()
            .any(|envelope| matches!(envelope, Envelope::RemoteAction { .. })),
        "Bob received Ann's action: {leaked:?}"
    );
    assert_eq!(t.globex_governance().await, before);
    t.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removed_member_loses_access_at_once_and_other_tenants_keep_theirs() {
    let t = TwoTenants::start().await;
    let ann_in_acme = subscribed(&t.relay, &t.ann_laptop, t.acme.workspace_id).await;
    let bob_in_globex = subscribed(&t.relay, &t.bob_phone, t.globex.workspace_id).await;
    crate::common::wait_until_subscribed(
        &t.relay,
        t.globex.workspace_id,
        &t.bob_phone.device_pubkey(),
    )
    .await;

    t.rooms()
        .remove_member(
            &t.globex.room,
            &t.host.cert,
            RemoveMemberRequest {
                request_id: [0x21; 32],
                user_public_key: t.bob.user_pk,
            },
        )
        .await
        .unwrap();

    // `remove_member` returns once the relay stops admitting Bob: his very
    // next write is refused, never fanned out or kept.
    let marker = b"removed-member-writes-9d21";
    bob_in_globex
        .send(Envelope::Delta {
            workspace_id: t.globex.workspace_id,
            author_device_public_key: t.bob_phone.device_pubkey(),
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: retained_delta(marker),
            signature: [0; 64],
        })
        .ok();
    let answer = tilo_test_env::within("the relay answers the removed member", async {
        loop {
            match bob_in_globex.recv().await {
                Some(answer @ Envelope::NotAuthorised { .. }) => break Some(answer),
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await;
    assert!(
        matches!(answer, Some(Envelope::NotAuthorised { .. }) | None),
        "{answer:?}"
    );
    assert!(
        !persisted(t.state.path(), marker),
        "a removed member's write was kept"
    );
    tilo_test_env::within("the removed member's subscription ends", async {
        while t
            .relay
            .is_subscribed(t.globex.workspace_id, &t.bob_phone.device_pubkey())
            .await
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    // A fresh connection is not admitted either: the handshake or the
    // subscription refuses it.
    if let Ok(again) = try_connect(&t.relay, &t.bob_phone).await {
        let answer = subscribe(&again, t.globex.workspace_id).await;
        assert!(
            matches!(answer, Some(Envelope::NotAuthorised { .. }) | None),
            "a removed member subscribed again: {answer:?}"
        );
    }

    // Acme is untouched by Globex's decision.
    assert!(
        t.relay
            .is_subscribed(t.acme.workspace_id, &t.ann_laptop.device_pubkey())
            .await
    );
    assert!(
        !matches!(
            next_within_quiet(&ann_in_acme).await,
            Some(Envelope::NotAuthorised { .. })
        ),
        "Globex's removal cut Acme's member"
    );
    t.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_never_governs_and_reaches_no_room_outside_its_delegation() {
    let t = TwoTenants::start().await;
    let before = t.globex_governance().await;
    let acme_before = governance(&t.rooms(), &t.host, &t.acme.room).await;
    // The host's own agent: a device of the relay's person with no
    // capabilities, delegated Acme alone.
    let agent = t.provider.enrol(
        "acme-agent",
        DeviceCapabilities {
            can_own_workspaces: false,
            can_admit_devices: false,
            can_act_as_host: false,
            can_read_all_workspaces: false,
            foreign: false,
            can_administer_devices: false,
        },
    );
    let issued = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &t.provider.user_sk,
        agent.device_pubkey(),
        Delegate::Agent {
            agent_id: "acme-helper".into(),
        },
        "Acme helper",
        [0x7c, 0x3a, 0xed],
        DelegateScope {
            rooms: vec![t.acme.room.clone()],
            apps: Vec::new(),
        },
        issued,
        issued.checked_add(600_000_000_000).unwrap(),
    )
    .unwrap();
    t.relay.register_agent(delegation).unwrap();

    // An agent governs nothing, not even the room it is delegated, and it
    // is no host person: the administration door refuses it removing anyone,
    // and the one decision every door asks refuses it the host's
    // operations and either room's governance.
    let admin = RoomAdminClient::new(device_endpoint(&agent).await, agent.cert.clone());
    removal_refused("the agent", &admin, &t.relay, &t.acme, t.ann.user_pk).await;
    removal_refused("the agent", &admin, &t.relay, &t.globex, t.bob.user_pk).await;
    let rooms = t.rooms();
    for room in [&t.acme.room, &t.globex.room] {
        let expires_at_ns = tilo_clock::unix_now_ns()
            .checked_add(600_000_000_000)
            .unwrap();
        let issued = rooms
            .issue(
                room,
                &agent.cert,
                IssueInvitationRequest {
                    request_id: [0xB1; 32],
                    role: MemberRole::Viewer,
                    recipient_user_public_key: None,
                    invitee_name: None,
                    inviter_name: None,
                    expires_at_ns,
                },
            )
            .await;
        assert!(issued.is_err(), "the agent invited into {room}");
        let administrator = rooms
            .set_member_administrator(room, &agent.cert, [0xB2; 32], t.ann.user_pk)
            .await;
        assert!(
            administrator.is_err(),
            "the agent made an administrator in {room}"
        );
        let removed = rooms
            .remove_member(
                room,
                &agent.cert,
                RemoveMemberRequest {
                    request_id: [0xB3; 32],
                    user_public_key: t.bob.user_pk,
                },
            )
            .await;
        assert!(removed.is_err(), "the agent removed a member of {room}");
    }
    assert_eq!(t.globex_governance().await, before);
    assert_eq!(
        governance(&t.rooms(), &t.host, &t.acme.room).await,
        acme_before
    );
    assert_eq!(t.rooms().list().await.len(), 2, "the agent created a room");

    // The host's compute is its person's, never an agent's.
    compute_control_refused("the agent", &t, &agent).await;

    // Outside its delegation it is not admitted to a room at all.
    let probe = connect(&t.relay, &agent).await;
    assert!(
        matches!(
            subscribe(&probe, t.globex.workspace_id).await,
            Some(Envelope::NotAuthorised { .. }) | None
        ),
        "the agent reads a room its delegation does not name"
    );
    t.relay.shutdown().await;
}

/// `device`, which completes the compute handshake, drives none of the
/// host's compute: job content in either tenant's room is refused before any
/// device holding bytes is asked, and its first control message ends the
/// control stream, so it attaches to no room's compute plane and receives
/// no one's queued job.
async fn compute_control_refused(who: &str, t: &TwoTenants, device: &TestDevice) {
    let mut client = ComputeClient::connect(
        device_endpoint(device).await,
        t.relay.node_addr(),
        t.provider.user_pk,
        Some(t.host.device_pubkey()),
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("the device completes the compute handshake");
    for workspace_id in [t.acme.workspace_id, t.globex.workspace_id] {
        assert_eq!(
            client
                .fetch_content(workspace_id, [0x31; 16], [0x32; 32], u64::MAX)
                .await,
            Err(ContentFetchError::Refused("not_authorised".into())),
            "{who} fetched compute content in workspace {workspace_id}"
        );
    }
    client.attach(t.acme.workspace_id).await.ok();
    let answer = tokio::time::timeout(Duration::from_secs(5), client.receive())
        .await
        .expect("the relay answers a compute control message");
    let answer = answer.map(|delivery| delivery.envelope);
    assert!(
        answer.is_err(),
        "{who} holds compute control: the relay answered {answer:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tenants_member_gets_no_compute_control_and_no_job_content() {
    let t = TwoTenants::start().await;
    compute_control_refused("Acme's inviter", &t, &t.ann_laptop).await;
    compute_control_refused("Globex's member", &t, &t.bob_phone).await;
    t.relay.shutdown().await;
}
