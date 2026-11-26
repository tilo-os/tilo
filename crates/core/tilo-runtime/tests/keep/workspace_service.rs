//! A person's device hosts the workspaces placed on it with the server's own workspace
//! host, opened on an endpoint the device owns: another person it invited
//! joins through the workspace's join exchange, the person's other devices list
//! and enter its workspaces on a link of their own, and a member in the same
//! process reaches it over the same handshake and link as any other.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::sync::Arc;

use iroh::endpoint::Endpoint;
use iroh::protocol::Router;
use serde::de::DeserializeOwned;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_delivery_protocol::workspace_join::ServedWorkspace;
use tilo_peer_state::actions::{ActionService, NoActions, RemoteDevice};
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_peer_state::join_transport::WorkspaceJoinClient;
use tilo_server_client::LinkError;
use tilo_server_client::ServerAddress;
use tilo_server_client::link::{Dialer, Link, Purpose};
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use tilo_workspace_service::service::ServiceConfig;
use tilo_workspace_service::workspaces::{
    ENTER_WORKSPACE, EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACE,
    HOST_WORKSPACES_RESOURCE, IssueInvitationRequest, LIST_WORKSPACES, WorkspaceListing,
    WorkspaceSummary,
};

use crate::keep::common::{
    Hosted, Linked, TestDevice, TestUser, copy_says, device_endpoint, host_writes, reaches_host,
    receives, serve_configured, serve_on, write,
};

/// The host's own operations, asked as `device` over an endpoint of its own: the link a device
/// opens before it holds any workspace.
async fn host_operations(device: &TestDevice, hosted: &Hosted) -> (Endpoint, RemoteDevice) {
    let endpoint = device_endpoint(device).await;
    let remote = ActionService::new(device.sk.clone(), Arc::new(NoActions)).remote(
        &endpoint,
        &hosted.host.node_addr(),
        "",
    );
    (endpoint, remote)
}

/// Prove `device` to `host` over a new records link: the link the host serves the workspace on
/// once it admits the device.
async fn dial_records(
    dialer: &Dialer,
    device: &TestDevice,
    host: &DeviceCert,
) -> Result<Link, LinkError> {
    let link = dialer.link(Purpose::Records).await?;
    link.prove_member(&device.cert, &device.sk, host).await?;
    Ok(link)
}

/// What the host answers `message` on the link `remote` holds to it.
async fn ask(remote: &RemoteDevice, message: RemoteActionMessage) -> RemoteActionMessage {
    tilo_test_env::within("the host answers", remote.ask_message(message))
        .await
        .expect("the host answers")
        .expect("the host answers the request")
}

/// The host operation `action` with `argument`, asked as `device` over `remote`, and what it
/// produced.
async fn host_action<T: DeserializeOwned>(
    remote: &RemoteDevice,
    device: &TestDevice,
    action: &str,
    argument: serde_json::Value,
) -> T {
    let RemoteActionMessage::Receipt(receipt) = host_answer(remote, device, action, argument).await
    else {
        panic!("the host answers its action with a receipt");
    };
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the host's own actions complete: {:?}", receipt.status);
    };
    serde_json::from_value(output.value.unwrap_or_default()).unwrap()
}

/// What the host answers the host operation `action` with `argument`, asked as `device` over
/// `remote`.
async fn host_answer(
    remote: &RemoteDevice,
    device: &TestDevice,
    action: &str,
    argument: serde_json::Value,
) -> RemoteActionMessage {
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    let mut invoke = InvokeRequest {
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
    // A request naming no workspace proves its device by the invocation it carries: no
    // workspace's roster names who asks there.
    invoke.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(
            "",
            &invoke,
            &device.cert,
            &device.sk,
            tilo_clock::unix_now_ns(),
        )
        .unwrap(),
    ));
    ask(remote, RemoteActionMessage::Invoke(invoke)).await
}

/// Whether `answer` tells its asker the host does not know their person.
fn not_known(answer: &RemoteActionMessage) -> bool {
    format!("{answer:?}").contains(tilo_actions::remote::NOT_ON_THIS_SERVER)
}

/// A host answers its own operations, asked with no workspace, for the people it knows: its own
/// person, one its operator lists, and a member of a workspace it holds. Anyone else may ask it
/// only to hold a workspace another host serves, which its work setting decides, and is told
/// for the rest that the host does not know them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_answers_its_own_operations_for_the_people_it_knows() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let bob = TestUser::fresh("Bob");
    let bobs_phone = bob.enrol("phone", DeviceCapabilities::member());
    let cy = TestUser::fresh("Cy");
    let cys_phone = cy.enrol("phone", DeviceCapabilities::member());
    let list = serde_json::json!({});

    // The operator trusts their own person alone.
    let hosted = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let (endpoint, remote) = host_operations(&phone, &hosted).await;
    let _: Vec<WorkspaceListing> =
        host_action(&remote, &phone, LIST_WORKSPACES, list.clone()).await;
    drop((endpoint, remote));
    let (endpoint, remote) = host_operations(&bobs_phone, &hosted).await;
    let refused = host_answer(&remote, &bobs_phone, LIST_WORKSPACES, list.clone()).await;
    assert!(not_known(&refused), "a stranger lists nothing: {refused:?}");
    // Asked to hold a workspace, the host's work setting answers, not that it does not know them.
    let hold = host_answer(&remote, &bobs_phone, HOST_WORKSPACE, list.clone()).await;
    assert!(
        !not_known(&hold),
        "a stranger may ask the host to hold: {hold:?}"
    );
    drop((endpoint, remote));
    hosted.stop().await;

    // The operator lists Bob too.
    let state = tempfile::tempdir().unwrap();
    let config = ServiceConfig {
        trust_policy: tilo_server_protocol::policy::TrustPolicy::AllowList {
            users: vec![hex::encode(bob.user_pk)],
        },
        ..ServiceConfig::new(state.path().to_path_buf(), WorkSetting::none())
    };
    let hosted = serve_configured(&alice, &laptop, (config, state), None).await;
    let (endpoint, remote) = host_operations(&bobs_phone, &hosted).await;
    let _: Vec<WorkspaceListing> =
        host_action(&remote, &bobs_phone, LIST_WORKSPACES, list.clone()).await;
    drop((endpoint, remote));
    let (endpoint, remote) = host_operations(&cys_phone, &hosted).await;
    let refused = host_answer(&remote, &cys_phone, LIST_WORKSPACES, list).await;
    assert!(
        not_known(&refused),
        "the operator lists Bob alone: {refused:?}"
    );
    drop((endpoint, remote));
    hosted.stop().await;
}

/// `phone`, a device of `alice`'s own, lists the workspaces `laptop` hosts and enters `workspace`
/// through its join, over `remote` and the endpoint its link runs from.
async fn enter_on(
    alice: &TestUser,
    phone: &TestDevice,
    endpoint: &Endpoint,
    remote: &RemoteDevice,
    workspace: &WorkspaceSummary,
) {
    let listed: Vec<WorkspaceListing> =
        host_action(remote, phone, LIST_WORKSPACES, serde_json::json!({})).await;
    assert!(
        listed
            .iter()
            .any(|listed| listed.workspace == workspace.workspace),
        "the host lists the workspace its person made: {listed:?}"
    );
    let entered: EnteredWorkspace = host_action(
        remote,
        phone,
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
    WorkspaceJoinClient::new(endpoint.clone())
        .redeem_hint(
            &verified.grant().transport[0],
            own.signed_invitation,
            phone.cert.clone(),
            &phone.sk,
            [5; 32],
        )
        .await
        .unwrap();
}

/// `phone`, a device of `alice`'s own, lists the workspaces `laptop` hosts and enters `workspace`
/// on a link of its own.
async fn own_device_enters(
    alice: &TestUser,
    phone: &TestDevice,
    hosted: &Hosted,
    workspace: &WorkspaceSummary,
) -> Linked {
    let (endpoint, remote) = host_operations(phone, hosted).await;
    enter_on(alice, phone, &endpoint, &remote, workspace).await;
    drop(remote);
    drop(endpoint);
    Linked::open_hosted(hosted, phone, workspace.workspace_id)
        .await
        .expect("the workspace admits the device that entered it")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_host_admits_an_invited_person_and_its_own_devices_enter_through_it() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let bob = TestUser::fresh("Bob");
    let bobs_phone = bob.enrol("phone", DeviceCapabilities::member());
    let hosted = serve_on(&alice, &laptop, WorkSetting::none()).await;
    let workspace = hosted.host.workspaces().create("Plans").await.unwrap();

    // The laptop serves the workspace itself: its join answers name
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
    drop(bobs_endpoint);

    // Bob's phone reaches the workspace on the laptop, by the trust the join
    // gave it, and nothing else of the laptop's.
    let bobs = Linked::open_hosted(&hosted, &bobs_phone, workspace.workspace_id)
        .await
        .expect("the join's trust reaches the workspace");
    let private = hosted.host.workspaces().create("Private").await.unwrap();
    let refused = bobs.reopen_hosted(&hosted, private.workspace_id).await;
    assert!(
        refused.is_err(),
        "another person's device is never the person's own"
    );

    // Alice's phone, her own device, lists the laptop's workspaces and enters
    // one on a link of its own: neither device is a certified host.
    let phone_link = own_device_enters(&alice, &phone, &hosted, &workspace).await;
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
    drop((bobs, phone_link));
    hosted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_in_process_member_shares_deltas_with_a_remote_one() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let hosted = serve_on(&alice, &laptop, WorkSetting::none()).await;
    // The laptop makes the workspace, so its server holds it and the laptop
    // administers it; the phone enters it as any other device of hers does.
    let home = hosted.host.workspaces().create("Home").await.unwrap();
    assert_eq!(
        hosted.host.device_cert(),
        &laptop.cert,
        "the host is this device"
    );
    let remote = own_device_enters(&alice, &phone, &hosted, &home).await;

    // What the device serving the workspace writes in process reaches the
    // remote member's link.
    host_writes(&hosted, home.workspace_id, "note:laptop", "from the laptop").await;
    receives(&remote, |peer| {
        copy_says(peer, "note:laptop", "from the laptop")
    })
    .await;
    // And what the remote member writes is the host's own copy's.
    write(&remote, &phone, "note:phone", "from the phone").unwrap();
    reaches_host(&hosted, home.workspace_id, "note:phone", "from the phone").await;
    assert!(
        hosted
            .host
            .workspaces()
            .state(&home.workspace)
            .await
            .is_some(),
        "the host's own copy is the workspace's first member"
    );
    assert!(
        hosted
            .host
            .is_subscribed(home.workspace_id, &phone.device_pubkey())
            .await,
        "the remote member holds its link"
    );
    drop(remote);
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
    let (slot, protocols) = tilo_workspace_service::service::deferred_protocols();
    let mut router = Router::builder(endpoint.clone());
    for (alpn, handler) in protocols {
        router = router.accept(alpn, handler);
    }
    let router = router.spawn();
    let phone_endpoint = device_endpoint(&phone).await;
    let dialer = Dialer::from_endpoint(ServerAddress::at(endpoint.addr()), phone_endpoint.clone());
    // A dial the host answers only once a workspace is served here: the link is
    // admitted, and its first stream proves the device.
    assert!(
        dial_records(&dialer, &phone, &laptop.cert).await.is_err(),
        "no workspace is hosted here yet"
    );

    // The laptop's own service, which founds and seals as the laptop's device, answers on the
    // router's endpoint once it is installed.
    let state = tempfile::tempdir().unwrap();
    let (service, _keys) =
        crate::keep::common::laptop_service_at(state.path(), &laptop, endpoint.clone()).await;
    let service = Arc::new(service);
    // A workspace the laptop made is served here before any device asks for it.
    let home = service.workspaces().create("Home").await.unwrap();
    slot.install(Arc::clone(&service));

    // The phone asks the host's own operations over a link from its endpoint, and enters Home.
    let actions = ActionService::new(phone.sk.clone(), Arc::new(NoActions));
    let remote = actions.remote_over(
        Arc::new(dialer.link(Purpose::Actions).await.unwrap()),
        laptop.device_pubkey(),
        "",
    );
    enter_on(&alice, &phone, &phone_endpoint, &remote, &home).await;
    drop(remote);

    // The installed host serves the workspace to the device that entered it.
    let peer = service
        .workspaces()
        .state(&home.workspace)
        .await
        .expect("the installed host serves the workspace");
    let sync = DirectSync::new(peer, phone.sk.clone());
    let link = dial_records(&dialer, &phone, &laptop.cert)
        .await
        .expect("the installed host admits the device that entered it");
    let joined = sync.dial_over(Arc::new(link), laptop.device_pubkey()).await;
    assert!(
        joined.is_ok(),
        "the installed host serves the workspace to the device that entered it"
    );
    router.shutdown().await.unwrap();
}
