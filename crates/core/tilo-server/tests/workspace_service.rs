//! A person's device hosts the workspaces placed on it with the server's own workspace
//! host, opened on an endpoint the device owns: another person it invited
//! joins through the workspace's join exchange, the person's other devices list
//! and enter its workspaces on a session of their own, and a member in the same
//! process connects through the same handshake and session as any other.

use iroh::protocol::Router;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::ServedWorkspace;
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_server::service::{ServiceConfig, ServiceIdentity, WorkspaceService};
use tilo_server::workspace_join::WorkspaceJoinClient;
use tilo_server::workspaces::{
    ENTER_WORKSPACE, EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
    IssueInvitationRequest, LIST_WORKSPACES, WorkspaceListing, WorkspaceSummary, ask_host,
};
use tilo_server::{Envelope, ServerClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{Hosted, TestDevice, TestUser, device_endpoint, receives, serve_on};
use tilo_server::WorkspaceId;

/// Subscribe `client` to `workspace_id` and wait until the host lets it in.
async fn subscribe(client: &ServerClient, workspace_id: WorkspaceId) {
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
    client: &ServerClient,
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
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let status = tilo_test_env::within("the host answers its action", ask_host(client, invoke))
        .await
        .expect("the host answers");
    let ReceiptStatus::Completed { output } = status else {
        panic!("the host's own actions complete: {status:?}");
    };
    serde_json::from_value(output.value.unwrap_or_default()).unwrap()
}

/// A session of `device` to its own person's `host`, pinned to that device.
async fn own_session(user: &TestUser, device: &TestDevice, hosted: &Hosted) -> ServerClient {
    ServerClient::connect_v2_to_own_device(
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

/// `phone`, a device of `alice`'s own, lists the workspaces `laptop` hosts and
/// enters `workspace` on a session of its own.
async fn own_device_enters(
    alice: &TestUser,
    phone: &TestDevice,
    hosted: &Hosted,
    workspace: &WorkspaceSummary,
) -> ServerClient {
    let session = own_session(alice, phone, hosted).await;
    enter_on(alice, phone, &session, workspace).await;
    session
}

/// `phone`, a device of `alice`'s own, on its `session` to her server, lists
/// the workspaces it holds and enters `workspace` through its join.
async fn enter_on(
    alice: &TestUser,
    phone: &TestDevice,
    session: &ServerClient,
    workspace: &WorkspaceSummary,
) {
    let listed: Vec<WorkspaceListing> =
        host_action(session, LIST_WORKSPACES, serde_json::json!({})).await;
    assert!(
        listed
            .iter()
            .any(|listed| listed.workspace == workspace.workspace)
    );
    let entered: EnteredWorkspace = host_action(
        session,
        ENTER_WORKSPACE,
        serde_json::json!({ "workspace": workspace.workspace }),
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
    subscribe(session, workspace.workspace_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_host_admits_an_invited_person_and_its_own_devices_enter_through_it() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let bob = TestUser::fresh("Bob");
    let bobs_phone = bob.enrol("phone", DeviceCapabilities::member());
    let hosted = serve_on(&alice, &laptop, tilo_server_protocol::WorkSetting::none()).await;
    let workspace = hosted.host.workspaces().create("Plans").await.unwrap();

    // The laptop governs and serves the workspace itself: its join answers name
    // it so, with no host capability.
    let invitation = hosted
        .host
        .workspaces()
        .issue(
            &workspace.workspace,
            &laptop.cert,
            IssueInvitationRequest {
                request_id: [3; 32],
                role: Role::Editor,
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
    assert_eq!(admission.bootstrap.workspace_id, workspace.workspace_id);
    let served: ServedWorkspace = admission.served;
    assert_eq!(served.device(), &laptop.cert);

    // Bob's phone reaches the workspace on the laptop, by the trust the join
    // gave it, and nothing else of the laptop's.
    let bobs_session = ServerClient::connect_to_workspace_device(
        bobs_endpoint,
        hosted.host.node_addr(),
        served,
        bobs_phone.cert.clone(),
        bobs_phone.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    subscribe(&bobs_session, workspace.workspace_id).await;
    assert!(
        ServerClient::connect_v2_to_own_device(
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

    // Alice's phone, her own device, lists the laptop's workspaces and enters
    // one on a session of its own: neither device is a certified host.
    let session = own_device_enters(&alice, &phone, &hosted, &workspace).await;
    let members = hosted
        .host
        .workspaces()
        .summary(&workspace.workspace)
        .await
        .unwrap()
        .devices;
    for device in [bobs_phone.device_pubkey(), phone.device_pubkey()] {
        assert!(
            members.values().any(|devices| devices.contains(&device)),
            "{members:?}"
        );
    }
    drop((bobs_session, session));
    hosted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_process_member_shares_deltas_with_a_remote_one() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let hosted = serve_on(&alice, &laptop, tilo_server_protocol::WorkSetting::none()).await;
    // The laptop makes the workspace, so its server holds it and the laptop
    // governs it; the phone enters it as any other device of hers does.
    let home = hosted.host.workspaces().create("Home").await.unwrap();
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
        local.server_peer.cert, laptop.cert,
        "the host is this device"
    );
    subscribe(&local, home.workspace_id).await;
    let remote = own_device_enters(&alice, &phone, &hosted, &home).await;

    let delta = |from: &TestDevice, at: u64, body: &[u8]| Envelope::Delta {
        workspace_id: home.workspace_id,
        author_device_public_key: from.device_pubkey(),
        authored_at_ns: at,
        delta: body.to_vec(),
        signature: [0; 64],
    };
    local.send(delta(&laptop, 1, b"from the laptop")).unwrap();
    receives(&remote, b"from the laptop").await;
    remote.send(delta(&phone, 2, b"from the phone")).unwrap();
    receives(&local, b"from the phone").await;
    assert!(
        hosted
            .host
            .is_subscribed(home.workspace_id, &laptop.device_pubkey())
            .await
    );
    drop((local, remote));
    hosted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the service moves into the router once it holds its workspace; nothing of it is left to drop"
)]
async fn a_router_built_before_any_workspace_refuses_until_its_host_is_installed() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let endpoint = device_endpoint(&laptop).await;
    let (slot, protocols) = tilo_server::service::deferred_protocols();
    let mut router = Router::builder(endpoint.clone());
    for (alpn, handler) in protocols {
        router = router.accept(alpn, handler);
    }
    let router = router.spawn();
    let dial = || async {
        ServerClient::connect_v2_to_own_device(
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
    assert!(dial().await.is_err(), "no workspace is hosted here yet");

    let state = tempfile::tempdir().unwrap();
    let service = WorkspaceService::open(
        ServiceConfig::new(
            state.path().to_path_buf(),
            tilo_server_protocol::WorkSetting::none(),
        ),
        ServiceIdentity {
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
    // A workspace the laptop made is held here before any device asks for it.
    let home = service.workspaces().create("Home").await.unwrap();
    slot.install(std::sync::Arc::new(service));
    let session = dial().await.unwrap();
    enter_on(&alice, &phone, &session, &home).await;
    drop(session);
    router.shutdown().await.unwrap();
}
