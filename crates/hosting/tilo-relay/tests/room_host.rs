//! A person's device hosts the rooms placed on it with the relay's own room
//! host, opened on an endpoint the device owns: another person it invited
//! joins through the room's join exchange, the person's other devices list
//! and enter its rooms on a session of their own, and a member in the same
//! process connects through the same handshake and session as any other.

use iroh::protocol::Router;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::WorkspaceHostTrust;
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_relay::host::{HostConfig, HostIdentity, HostMode, RoomHost};
use tilo_relay::rooms::{
    ENTER_ROOM, EnteredRoom, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE, IssueInvitationRequest,
    LIST_ROOMS, RoomListing, RoomSummary, ask_host,
};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::{Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser};

/// A workspace the person's devices hold on the host before any room.
const HOME: u64 = 0x686f_6d65;

/// An endpoint whose key is the device's own, as a device dials with.
async fn device_endpoint(device: &TestDevice) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&device.sk.to_bytes()))
        .bind()
        .await
        .unwrap()
}

/// A room host on `device`'s own endpoint, routed as its embedder routes it.
struct Hosted {
    host: RoomHost,
    router: Router,
    _state: tempfile::TempDir,
}

async fn host_on(user: &TestUser, device: &TestDevice) -> Hosted {
    let state = tempfile::tempdir().unwrap();
    let endpoint = device_endpoint(device).await;
    let host = RoomHost::open(
        HostConfig::new(state.path().to_path_buf(), HostMode::Device),
        HostIdentity {
            user_public_key: user.user_pk,
            device_signing_key: device.sk.clone(),
            device_cert: device.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        None,
    )
    .await
    .unwrap();
    let mut router = Router::builder(endpoint);
    for (alpn, handler) in host.protocols() {
        router = router.accept(alpn, handler);
    }
    Hosted {
        host,
        router: router.spawn(),
        _state: state,
    }
}

impl Hosted {
    async fn stop(self) {
        self.router.shutdown().await.unwrap();
        self.host.shutdown().await;
    }
}

/// Subscribe `client` to `workspace_id` and wait until the host lets it in.
async fn subscribe(client: &RelayClient, workspace_id: u64) {
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the host admits the subscription", async {
        loop {
            match client.recv().await.expect("the session stays open") {
                Envelope::Ready {
                    workspace_id: ready,
                    ..
                } if ready == workspace_id => break,
                Envelope::NotAuthorised {
                    workspace_id: refused,
                    ..
                } if refused == workspace_id => {
                    panic!("the host refused workspace {workspace_id}")
                }
                _ => {}
            }
        }
    })
    .await;
}

/// The host operation `action` with `argument`, asked by `client` of its
/// host directly, and what it produced.
async fn host_action<T: serde::de::DeserializeOwned>(
    client: &RelayClient,
    action: &str,
    argument: serde_json::Value,
) -> T {
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    let invoke = InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.into(),
            address: ActionAddress::new(HOST_ROOMS_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    let status = tilo_test_env::within("the host answers its action", ask_host(client, invoke))
        .await
        .expect("the host answers");
    let ReceiptStatus::Completed { output } = status else {
        panic!("the host's own actions complete: {status:?}");
    };
    serde_json::from_value(output.value.unwrap_or_default()).unwrap()
}

/// The next delta `client` receives.
async fn next_delta(client: &RelayClient) -> Vec<u8> {
    tilo_test_env::within("the delta arrives", async {
        loop {
            if let Envelope::Delta { delta, .. } =
                client.recv().await.expect("the session stays open")
            {
                break delta;
            }
        }
    })
    .await
}

/// A session of `device` to its own person's `host`, pinned to that device.
async fn own_session(user: &TestUser, device: &TestDevice, hosted: &Hosted) -> RelayClient {
    RelayClient::connect_v2_to_own_device(
        device_endpoint(device).await,
        hosted.host.node_addr(),
        user.user_pk,
        hosted.host.device_cert().device_public_key,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap()
}

/// `phone`, a device of `alice`'s own, lists the rooms `laptop` hosts and
/// enters `room` on a session of its own.
async fn own_device_enters(
    alice: &TestUser,
    phone: &TestDevice,
    hosted: &Hosted,
    room: &RoomSummary,
) -> RelayClient {
    let session = own_session(alice, phone, hosted).await;
    let listed: Vec<RoomListing> = host_action(&session, LIST_ROOMS, serde_json::json!({})).await;
    assert!(listed.iter().any(|listed| listed.room == room.room));
    let entered: EnteredRoom = host_action(
        &session,
        ENTER_ROOM,
        serde_json::json!({ "room": room.room }),
    )
    .await;
    let own = entered.invitation.decode().unwrap();
    let verified = decode_and_verify_invitation(
        &own.signed_invitation,
        Some(alice.user_pk),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    WorkspaceJoinClient::new(device_endpoint(phone).await)
        .redeem_hint(
            &verified.grant().transport[0],
            own.signed_invitation,
            phone.cert.clone(),
            &phone.sk,
            [5; 32],
        )
        .await
        .unwrap();
    subscribe(&session, room.workspace_id).await;
    session
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes the host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn a_device_host_admits_an_invited_person_and_its_own_devices_enter_through_it() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let bob = TestUser::fresh("Bob");
    let bobs_phone = bob.enrol("phone", DeviceCapabilities::member());
    let hosted = host_on(&alice, &laptop).await;
    let room = hosted.host.rooms().create("Plans").await.unwrap();

    // The laptop governs and serves the room itself: its join answers name
    // it so, with no host capability.
    let invitation = hosted
        .host
        .rooms()
        .issue(
            &room.room,
            &laptop.cert,
            IssueInvitationRequest {
                request_id: [3; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(bob.user_pk),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        Some(bob.user_pk),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(verified.grant().issuer, laptop.cert);
    let bobs_endpoint = device_endpoint(&bobs_phone).await;
    let admission = WorkspaceJoinClient::new(bobs_endpoint.clone())
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation.clone(),
            bobs_phone.cert.clone(),
            &bobs_phone.sk,
            [4; 32],
        )
        .await
        .unwrap();
    assert_eq!(admission.bootstrap.room, room.room);
    let WorkspaceHostTrust::InteractiveOwner(trust) = admission.trust else {
        panic!("a device host is the room's own governor, not a certified host");
    };
    assert_eq!(trust.host(), &laptop.cert);

    // Bob's phone reaches the room on the laptop, by the trust the join
    // gave it, and nothing else of the laptop's.
    let bobs_session = RelayClient::connect_v2_to_interactive_workspace_owner(
        bobs_endpoint,
        hosted.host.node_addr(),
        trust,
        bobs_phone.cert.clone(),
        bobs_phone.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    subscribe(&bobs_session, room.workspace_id).await;
    assert!(
        RelayClient::connect_v2_to_own_device(
            device_endpoint(&bobs_phone).await,
            hosted.host.node_addr(),
            alice.user_pk,
            laptop.device_pubkey(),
            bobs_phone.cert.clone(),
            bobs_phone.sk.clone(),
            RevocationList::new(),
        )
        .await
        .is_err(),
        "another person's device is never the person's own"
    );

    // Alice's phone, her own device, lists the laptop's rooms and enters
    // one on a session of its own: neither device is a certified host.
    let session = own_device_enters(&alice, &phone, &hosted, &room).await;
    let members = hosted
        .host
        .rooms()
        .summary(&room.room)
        .await
        .unwrap()
        .members;
    for device in [bobs_phone.device_pubkey(), phone.device_pubkey()] {
        assert!(
            members
                .iter()
                .any(|member| member.active_device_keys.contains(&device)),
            "{members:?}"
        );
    }
    drop((bobs_session, session));
    hosted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes the host after the assertions; the suggested earlier drop would use a moved value"
)]
async fn an_in_process_member_shares_deltas_with_a_remote_one() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let hosted = host_on(&alice, &laptop).await;
    let local = hosted
        .host
        .connect_local(
            laptop.cert.clone(),
            laptop.sk.clone(),
            RevocationList::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        local.relay_peer.cert, laptop.cert,
        "the host is this device"
    );
    subscribe(&local, HOME).await;
    let remote = own_session(&alice, &phone, &hosted).await;
    subscribe(&remote, HOME).await;

    let delta = |from: &TestDevice, at: u64, body: &[u8]| Envelope::Delta {
        workspace_id: HOME,
        author_device_public_key: from.device_pubkey(),
        authored_at_ns: at,
        delta: body.to_vec(),
        signature: [0; 64],
    };
    local.send(delta(&laptop, 1, b"from the laptop")).unwrap();
    assert_eq!(next_delta(&remote).await, b"from the laptop");
    remote.send(delta(&phone, 2, b"from the phone")).unwrap();
    assert_eq!(next_delta(&local).await, b"from the phone");
    assert!(
        hosted
            .host
            .is_subscribed(HOME, &laptop.device_pubkey())
            .await
    );
    drop((local, remote));
    hosted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_router_built_before_any_room_refuses_until_its_host_is_installed() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let endpoint = device_endpoint(&laptop).await;
    let (slot, protocols) = tilo_relay::host::deferred_protocols();
    let mut router = Router::builder(endpoint.clone());
    for (alpn, handler) in protocols {
        router = router.accept(alpn, handler);
    }
    let router = router.spawn();
    let dial = || async {
        RelayClient::connect_v2_to_own_device(
            device_endpoint(&phone).await,
            endpoint.addr(),
            alice.user_pk,
            laptop.device_pubkey(),
            phone.cert.clone(),
            phone.sk.clone(),
            RevocationList::new(),
        )
        .await
    };
    assert!(dial().await.is_err(), "no room is hosted here yet");

    let state = tempfile::tempdir().unwrap();
    let host = RoomHost::open(
        HostConfig::new(state.path().to_path_buf(), HostMode::Device),
        HostIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: laptop.sk.clone(),
            device_cert: laptop.cert.clone(),
            revocations: RevocationList::new(),
        },
        endpoint.clone(),
        None,
    )
    .await
    .unwrap();
    slot.install(std::sync::Arc::new(host));
    let session = dial().await.unwrap();
    subscribe(&session, HOME).await;
    drop(session);
    router.shutdown().await.unwrap();
}
