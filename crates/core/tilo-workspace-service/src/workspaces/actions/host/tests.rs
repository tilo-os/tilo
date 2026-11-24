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
use tilo_server_protocol::workspaces::{
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
const HOST_OPERATIONS: [&str; 22] = [
    CREATE_WORKSPACE,
    tilo_server_protocol::workspaces::CREATE_OWN_WORKSPACE,
    DELETE_WORKSPACE,
    tilo_server_protocol::workspaces::BRANCH_OWN_WORKSPACE,
    LIST_WORKSPACES,
    LIST_REVOCATIONS,
    super::BLOCK,
    super::UNBLOCK,
    ENTER_WORKSPACE,
    HOST_WORKSPACE,
    UNHOST_WORKSPACE,
    tilo_server_protocol::workspaces::CATCH_UP_WORKSPACE,
    tilo_server_protocol::workspaces::CATCH_UP_ABSENT,
    tilo_server_protocol::workspaces::CATCH_UP_STANDING,
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
        let refused = invoke(&host, &stranger, host_request(action, argument)).await;
        assert!(
            matches!(refused, Err(ActionCallError::Refused { .. })),
            "{action}: {refused:?}"
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
    assert!(
        matches!(&unasked, Err(ActionCallError::Refused { reason })
            if reason.contains("could not be kept for them to answer")),
        "with nothing of the person's here to keep the question in, it is refused for that: \
         {unasked:?}"
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

/// A block is the operator's own `tilo.host` action: it names one key, a device
/// or a person's root, writes it to the record of their own workspace, and the
/// host refuses the key from then on. Lifting it writes the record again.
#[tokio::test]
async fn the_operator_blocks_and_unblocks_a_device_and_a_person_as_actions() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let laptop = owners_laptop();
    // The record the blocks are written to lives in the operator's own
    // workspace, which their server holds.
    crate::workspaces::person_workspace::found(workspaces)
        .await
        .expect("the operator's own workspace is founded here");

    let (_, guest) = guest_device();
    let device = hex::encode(guest.device_public_key);
    let user = hex::encode(guest.user_public_key);
    let someone_else = [0x77; 32];

    // One key is named: a device's or a person's, never both and never neither.
    for argument in [
        json!({}),
        json!({ "device": device, "user": user }),
        json!({ "device": "not a key" }),
    ] {
        assert!(
            invoke(&host, &laptop, host_request(super::BLOCK, argument))
                .await
                .is_err(),
            "a block names one key of 32 bytes in lowercase hex"
        );
    }

    let blocked = invoke(
        &host,
        &laptop,
        host_request(super::BLOCK, json!({ "device": device })),
    )
    .await
    .unwrap();
    assert_eq!(blocked["blocked"], json!(true));
    assert_eq!(blocked["key"], json!(device));
    assert!(
        workspaces
            .blocked_keys()
            .blocks(&guest.device_public_key, &someone_else),
        "the host refuses the blocked device"
    );
    assert!(
        !workspaces
            .blocked_keys()
            .blocks(&someone_else, &someone_else),
        "another device is not blocked"
    );

    // A person's root key refuses every device of theirs.
    let blocked = invoke(
        &host,
        &laptop,
        host_request(super::BLOCK, json!({ "user": user })),
    )
    .await
    .unwrap();
    assert_eq!(blocked["blocked"], json!(true));
    assert!(
        workspaces
            .blocked_keys()
            .blocks(&someone_else, &guest.user_public_key),
        "every device of the blocked person is refused"
    );

    // Lifting either refuses nothing of the key any more.
    let lifted = invoke(
        &host,
        &laptop,
        host_request(super::UNBLOCK, json!({ "user": user })),
    )
    .await
    .unwrap();
    assert_eq!(lifted["blocked"], json!(false));
    assert!(
        !workspaces
            .blocked_keys()
            .blocks(&someone_else, &guest.user_public_key),
        "the person is admitted again"
    );
    invoke(
        &host,
        &laptop,
        host_request(super::UNBLOCK, json!({ "device": device })),
    )
    .await
    .unwrap();
    assert!(
        !workspaces
            .blocked_keys()
            .blocks(&guest.device_public_key, &someone_else),
        "the device is admitted again"
    );
}

/// Only the host's person, on a device of their own, blocks a key: another
/// person is offered no such operation and invoking it is no operation.
#[tokio::test]
async fn a_stranger_cannot_block_a_key_on_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = ServiceWorkspaceActions::new();
    host.attach(workspaces.clone());
    let (_, guest) = guest_device();
    assert!(
        !offered(&host, &guest, &caller(&guest))
            .await
            .iter()
            .any(|action| action == super::BLOCK || action == super::UNBLOCK),
        "a stranger is offered no block"
    );
    assert!(
        invoke(
            &host,
            &guest,
            host_request(
                super::BLOCK,
                json!({ "device": hex::encode(guest.device_public_key) }),
            ),
        )
        .await
        .is_err(),
        "and invoking one is no operation"
    );
}
