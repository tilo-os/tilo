//! The host's own operations as `tilo.host` actions: offered to and run for
//! the devices of the server's own person only, decided as every entry
//! decides a host operation, and asked with the certificate the transport
//! verified.

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget,
};
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_server_session::workspaces::{
    CREATE_WORKSPACE, CreatedWorkspace, DELETE_WORKSPACE, ENTER_WORKSPACE, EnteredWorkspace,
    HOST_ACTIONS_PROVIDER, HOST_WORKSPACE, HOST_WORKSPACES_RESOURCE, LIST_REVOCATIONS,
    LIST_WORKSPACES, UNHOST_WORKSPACE, WorkspaceListing,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

use super::super::tests::{caller, settled};
use super::super::{GovernedServer, ServiceWorkspaceActions};
use crate::app_serving::AppActionServer as _;
use crate::workspaces::tests::{governor, guest_device, test_host};
use tilo_server_protocol::WorkspaceId;

/// Another device of the server's own person: the one `governor` certifies
/// the server for.
fn owners_laptop() -> DeviceCert {
    let root = SigningKey::from_bytes(&[1; 32]);
    let key = SigningKey::from_bytes(&[5; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "laptop".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    cert
}

fn host_request(action: &str, argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request("", action, argument);
    request.target = RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: ActionAddress::new(HOST_WORKSPACES_RESOURCE, action),
    };
    request
}

/// What `device` is offered of the host's operations, asked of the host.
async fn offered(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
    caller: &Caller,
) -> Vec<String> {
    let query = CatalogueQuery {
        provider: Some(HOST_ACTIONS_PROVIDER.into()),
        max_results: 100,
        ..CatalogueQuery::default()
    };
    settled(host.as_host(device).query(caller, query))
        .await
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

/// `request` by `device`, its certificate as the transport verified it.
async fn invoke(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
    request: InvokeRequest,
) -> Result<Value, ActionCallError> {
    let receipt = settled(host.as_host(device).invoke(&caller(device), request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

/// What the host offers its person's devices, in the order it lists them.
const HOST_OPERATIONS: [&str; 23] = [
    CREATE_WORKSPACE,
    tilo_server_session::workspaces::CREATE_OWN_WORKSPACE,
    DELETE_WORKSPACE,
    tilo_server_session::workspaces::BRANCH_OWN_WORKSPACE,
    LIST_WORKSPACES,
    tilo_server_session::workspaces::LIST_PERSONAL,
    tilo_server_session::workspaces::KEEP_PERSONAL,
    tilo_server_session::workspaces::PERSONAL_DEVICE_NAME,
    LIST_REVOCATIONS,
    ENTER_WORKSPACE,
    HOST_WORKSPACE,
    UNHOST_WORKSPACE,
    tilo_server_session::workspaces::CATCH_UP_WORKSPACE,
    tilo_server_session::workspaces::CATCH_UP_ABSENT,
    tilo_server_session::workspaces::CATCH_UP_STANDING,
    super::PLAN_MACHINE,
    super::LIST_MACHINES,
    super::SHOW_MACHINE,
    super::LIST_SERVERS,
    super::ADMIT_MACHINE,
    super::ADMIT_SERVER,
    super::KEEP_RECOVERY_KEY,
    super::RECOVERY_KEYS,
];

#[tokio::test]
async fn the_servers_person_creates_and_lists_workspaces_as_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let laptop = owners_laptop();
    assert_eq!(
        offered(&host, &laptop, &caller(&laptop)).await,
        HOST_OPERATIONS.map(str::to_owned)
    );

    // Creating the workspace issues the invitation the share is for, with
    // exactly the terms asked.
    let (_, guest) = guest_device();
    let before = tilo_clock::unix_now_ns();
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &laptop,
            host_request(
                CREATE_WORKSPACE,
                json!({
                    "name": "Plans",
                    "invite_role": "commenter",
                    "invite_recipient": hex::encode(guest.user_public_key),
                    "invitee_name": "Grace",
                    "invite_expires_in_seconds": 120,
                }),
            ),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let after = tilo_clock::unix_now_ns();
    assert_eq!(created.workspace.name, "Plans");
    let first = created.invitation.expect("asked for").decode().unwrap();
    let pending = workspaces
        .pending(&created.workspace.workspace, &owner)
        .await
        .unwrap()
        .into_iter()
        .find(|pending| pending.invite_id == first.invite_id)
        .expect("the first invitation is pending");
    assert_eq!(pending.role, Role::Commenter);
    assert_eq!(
        pending.recipient_user_public_key,
        Some(guest.user_public_key)
    );
    assert_eq!(pending.invitee_name.as_deref(), Some("Grace"));
    let lifetime: u64 = 120_000_000_000;
    assert!(
        (before.saturating_add(lifetime)..=after.saturating_add(lifetime))
            .contains(&pending.expires_at_ns)
    );

    // Without terms, a workspace is created with no invitation.
    let bare: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &laptop,
            host_request(CREATE_WORKSPACE, json!({ "name": "Bare" })),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    assert!(bare.invitation.is_none());

    let listed: Vec<WorkspaceListing> = serde_json::from_value(
        invoke(&host, &laptop, host_request(LIST_WORKSPACES, json!({})))
            .await
            .unwrap(),
    )
    .unwrap();
    for workspace in [&created.workspace, &bare.workspace] {
        assert!(
            listed
                .iter()
                .any(|listed| listed.workspace == workspace.workspace
                    && listed.name == workspace.name),
            "{listed:?}"
        );
    }
    server.shutdown().await;
}

#[tokio::test]
async fn the_servers_person_creates_a_workspace_with_an_administrators_invitation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let host = ServiceWorkspaceActions::new();
    host.attach(server.workspaces().clone());
    let laptop = owners_laptop();

    // The first invitation can make the person it brings in an
    // administrator: they join as an Editor and the invitation says so.
    let created: CreatedWorkspace = serde_json::from_value(
        invoke(
            &host,
            &laptop,
            host_request(
                CREATE_WORKSPACE,
                json!({ "name": "Plans", "invite_role": "administrator" }),
            ),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let first = created.invitation.expect("asked for").decode().unwrap();
    assert!(
        tilo_peer_state::join::makes_administrator(&first.signed_invitation),
        "the first invitation makes its holder an administrator"
    );
    let verified = decode_and_verify_invitation(
        &first.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(verified.grant().role, Role::Editor);
    server.shutdown().await;
}

#[tokio::test]
async fn the_servers_person_enters_a_workspace_and_lets_an_app_author_it_as_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let laptop = owners_laptop();
    let created = workspaces.create("Plans").await.unwrap();

    // Entering admits the laptop and answers the invitation it redeems,
    // restricted to its own person.
    let enter = || host_request(ENTER_WORKSPACE, json!({ "workspace": created.workspace }));
    let entered: EnteredWorkspace =
        serde_json::from_value(invoke(&host, &laptop, enter()).await.unwrap()).unwrap();
    assert!(entered.changed);
    let own = entered.invitation.decode().unwrap();
    let verified = decode_and_verify_invitation(
        &own.signed_invitation,
        Some(laptop.user_public_key),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(
        verified.grant().recipient_user_public_key,
        Some(laptop.user_public_key)
    );
    let again: EnteredWorkspace =
        serde_json::from_value(invoke(&host, &laptop, enter()).await.unwrap()).unwrap();
    assert!(!again.changed, "already admitted");

    // A Viewer, who may invite no one, enters from a device of theirs the same way: the
    // invitation it answers is their own, at their role, and the device redeems it.
    let root = SigningKey::from_bytes(&[9; 32]);
    workspaces
        .add_member(
            &created.workspace,
            &laptop,
            crate::workspaces::AddMemberRequest {
                request_id: [9; 32],
                user_public_key: root.verifying_key().to_bytes(),
                place: crate::workspaces::MemberPlace::Role(Role::Viewer),
                name: Some("Cy".into()),
            },
        )
        .await
        .unwrap();
    let mut phone = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: SigningKey::from_bytes(&[10; 32]).verifying_key().to_bytes(),
        device_label: "phone".into(),
        ..laptop.clone()
    };
    sign_device_cert(&mut phone, &root).unwrap();
    let entered: EnteredWorkspace =
        serde_json::from_value(invoke(&host, &phone, enter()).await.unwrap()).unwrap();
    assert!(entered.changed, "the phone is enrolled under its person");
    let own = entered.invitation.decode().unwrap();
    let verified = decode_and_verify_invitation(
        &own.signed_invitation,
        Some(phone.user_public_key),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(verified.grant().role, Role::Viewer);
    let redeemed = workspaces
        .admit_proven(own.signed_invitation, phone, [11; 32], "Cy", None)
        .await;
    assert!(
        matches!(
            redeemed,
            tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
        ),
        "{redeemed:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn no_one_else_and_nothing_uncertified_runs_the_hosts_operations() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let workspace = workspaces.create("Mine").await.unwrap();
    let laptop = owners_laptop();

    // Another person's device is offered nothing, invoking is unknown, and a workspace that
    // does not have them lets them in by no entry.
    let (_, stranger) = guest_device();
    assert!(
        offered(&host, &stranger, &caller(&stranger))
            .await
            .is_empty()
    );
    let entered = invoke(
        &host,
        &stranger,
        host_request(ENTER_WORKSPACE, json!({ "workspace": workspace.workspace })),
    )
    .await;
    assert!(
        matches!(entered, Err(ActionCallError::Refused { .. })),
        "{entered:?}"
    );
    for (action, argument) in [
        (CREATE_WORKSPACE, json!({ "name": "Theirs" })),
        (LIST_WORKSPACES, json!({})),
    ] {
        assert_eq!(
            invoke(&host, &stranger, host_request(action, argument)).await,
            Err(ActionCallError::UnknownAction),
            "{action}"
        );
    }
    assert_eq!(workspaces.list().await.len(), 1, "nothing was created");

    // An agent no grant names, on the person's own key, is offered nothing
    // and refused; one the laptop handed its session reads, and asks its
    // person before it makes anything.
    let agent = Caller::delegated_agent("writer", laptop.user_public_key, laptop.device_public_key);
    assert!(offered(&host, &laptop, &agent).await.is_empty());
    let refused = settled(
        host.as_host(&laptop)
            .invoke(&agent, host_request(LIST_WORKSPACES, json!({}))),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "{refused:?}"
    );
    let handed = caller(&laptop).delegated_to("writer");
    let offered_handed = offered(&host, &laptop, &handed).await;
    assert!(
        offered_handed
            .iter()
            .any(|action| action == LIST_WORKSPACES)
            && offered_handed
                .iter()
                .any(|action| action == CREATE_WORKSPACE),
        "{offered_handed:?}"
    );
    let unasked = settled(host.as_host(&laptop).invoke(
        &handed,
        host_request(CREATE_WORKSPACE, json!({ "name": "Its own" })),
    ))
    .await;
    assert_eq!(
        unasked.unwrap_err(),
        ActionCallError::confirmation_required(),
        "with nobody to ask, the person's answer is still wanted"
    );

    // A certificate that is not the caller's speaks for nobody.
    let refused = settled(
        host.as_host(&laptop)
            .invoke(&caller(&stranger), host_request(LIST_WORKSPACES, json!({}))),
    )
    .await;
    assert_eq!(refused.unwrap_err(), ActionCallError::UnknownAction);

    // Without the certificate the transport verified, the host's operations
    // are not there at all.
    let unverified = settled(
        host.in_workspace(WorkspaceId::from_u128(7))
            .invoke(&caller(&laptop), host_request(LIST_WORKSPACES, json!({}))),
    )
    .await;
    assert_eq!(unverified.unwrap_err(), ActionCallError::UnknownAction);

    // The app action server hands the certificate on: through it, the laptop lists.
    let governed = GovernedServer {
        governance: host.clone(),
        apps: None,
    };
    let list = RemoteActionMessage::Invoke(host_request(LIST_WORKSPACES, json!({})));
    assert!(governed.answers(&caller(&laptop), &list));
    let answer = governed
        .serve_host(&caller(&laptop), &laptop, list)
        .expect("an invocation is answered");
    let RemoteActionMessage::Receipt(receipt) = settled(answer).await else {
        panic!("a receipt");
    };
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "{:?}",
        receipt.status
    );
    server.shutdown().await;
}
