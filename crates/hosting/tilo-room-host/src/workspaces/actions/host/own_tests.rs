//! Workspaces of people's own on a host that keeps them: anyone's device
//! asks, the ones they have answer every time, as many as their plan keeps
//! are made and one more is refused typed, the person administers each from
//! their first device on, and their workspaces share what the host gives them
//! and keep no more of their writes when it is full.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use tilo_actions::remote::host_account::{self, HostAccount, Reason, Refusal};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
};
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_relay_protocol::workspaces::{JoinReply, JoinStep, join_proof_digest};
use tilo_relay_session::workspaces::{
    CREATE_OWN_ROOM, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE,
};
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationList, prepare_minted_grant,
    sign_device_cert,
};

use super::super::HostWorkspaceActions;
use super::super::tests::{caller, settled};
use crate::workspaces::tests::{governor, guest_device, test_host};
use crate::workspaces::{OwnWorkspaceOffer, Workspaces};

fn own_request(argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request("", CREATE_OWN_ROOM, argument);
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, CREATE_OWN_ROOM),
    };
    request
}

async fn invoke(
    host: &HostWorkspaceActions,
    device: &DeviceCert,
    request: InvokeRequest,
) -> Result<Value, ActionHostError> {
    let receipt = settled(host.as_host(device).invoke(&caller(device), request)).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

async fn create_own(
    host: &HostWorkspaceActions,
    device: &DeviceCert,
    argument: Value,
) -> Result<CreatedWorkspace, ActionHostError> {
    invoke(host, device, own_request(argument))
        .await
        .map(|value| serde_json::from_value(value).unwrap())
}

async fn offered(host: &HostWorkspaceActions, device: &DeviceCert, caller: &Caller) -> Vec<String> {
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

/// `device` redeems `created`'s invitation, as a page joining does.
async fn join(
    workspaces: &Workspaces,
    created: &CreatedWorkspace,
    key: &SigningKey,
    device: &DeviceCert,
) {
    let issued = created.invitation.as_ref().unwrap().decode().unwrap();
    let JoinReply::Challenge { challenge } = workspaces
        .page_join(JoinStep::Ask {
            invitation: issued.signed_invitation,
            device_cert: device.clone(),
            request_id: rand::random(),
        })
        .await
    else {
        panic!("no challenge");
    };
    let signature = key.sign(&join_proof_digest(&challenge)).to_bytes();
    let admitted = workspaces
        .page_join(JoinStep::Prove {
            challenge,
            signature,
            name: "Ada".into(),
        })
        .await;
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
}

/// How `person` stands in `workspace`: their role and whether they
/// administer it.
async fn standing(
    workspaces: &Workspaces,
    workspace: &str,
    person: [u8; 32],
) -> Option<(MemberRole, bool)> {
    workspaces
        .list()
        .await
        .into_iter()
        .find(|summary| summary.workspace == workspace)?
        .members
        .into_iter()
        .find(|member| member.user_public_key == person)
        .map(|member| (member.role, member.administrator))
}

#[tokio::test]
async fn anyone_gets_one_workspace_of_their_own_and_administers_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let (guest_key, guest) = guest_device();

    // A host that keeps none offers a stranger nothing.
    assert!(offered(&host, &guest, &caller(&guest)).await.is_empty());
    assert!(matches!(
        create_own(&host, &guest, json!({})).await,
        Err(ActionHostError::UnknownAction)
    ));

    assert!(
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap()
    );
    let mut offers = offered(&host, &guest, &caller(&guest)).await;
    offers.sort();
    assert_eq!(
        offers,
        [CREATE_OWN_ROOM, tilo_relay_session::workspaces::LIST_ROOMS],
        "a stranger makes and lists the workspace of their own, and nothing else"
    );
    let created = create_own(&host, &guest, json!({"name": "Ada's workspace"}))
        .await
        .unwrap();
    assert_eq!(created.workspace.name, "Ada's workspace");
    assert_eq!(
        workspaces.own_workspace_of(&guest.user_public_key),
        Some(created.workspace.workspace.clone())
    );
    // The invitation is the person's alone, an Editor's.
    let issued = created.invitation.as_ref().unwrap().decode().unwrap();
    let invitation = decode_and_verify_invitation(
        &issued.signed_invitation,
        Some(guest.user_public_key),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(invitation.grant().role, MemberRole::Editor);
    assert!(
        decode_and_verify_invitation(
            &issued.signed_invitation,
            Some([9; 32]),
            &RevocationList::new(),
            tilo_clock::unix_now_ns()
        )
        .is_err()
    );

    // Joining makes them its administrator.
    join(workspaces, &created, &guest_key, &guest).await;
    assert_eq!(
        standing(
            workspaces,
            &created.workspace.workspace,
            guest.user_public_key
        )
        .await,
        Some((MemberRole::Editor, true))
    );

    // Asking again without a name answers the same workspace, with a new
    // invitation.
    let again = create_own(&host, &guest, json!({})).await.unwrap();
    assert_eq!(again.workspace.workspace, created.workspace.workspace);
    assert_eq!(again.workspace.name, "Ada's workspace");
    assert_ne!(again.invitation, created.invitation);
    // Entering it by its id answers the same one.
    let entered = create_own(
        &host,
        &guest,
        json!({"room": created.workspace.workspace.clone()}),
    )
    .await
    .unwrap();
    assert_eq!(entered.workspace.workspace, created.workspace.workspace);
    assert_eq!(
        workspaces
            .list()
            .await
            .iter()
            .filter(|summary| summary.workspace == created.workspace.workspace)
            .count(),
        1
    );

    // A workspace that is no one of theirs is refused, not made one of
    // theirs.
    let other = workspaces.create("Not yours").await.unwrap();
    let refused = create_own(&host, &guest, json!({"room": other.workspace.clone()}))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ActionHostError::Refused { .. }),
        "{refused:?}"
    );

    // Someone else gets their own; the relay's own workspaces are no one's.
    let (_, stranger) = guest_device();
    let theirs = create_own(&host, &stranger, json!({})).await.unwrap();
    assert_eq!(theirs.workspace.name, "My workspace");
    assert_ne!(theirs.workspace.workspace, created.workspace.workspace);
    relay.shutdown().await;
}

#[tokio::test]
async fn a_person_keeps_as_many_workspaces_of_their_own_as_they_may() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces().clone();
    let offer = OwnWorkspaceOffer {
        max_workspaces: 2,
        ..OwnWorkspaceOffer::free("tilo.test")
    };
    workspaces.offer_own_workspaces(offer.clone()).unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let (_, guest) = guest_device();
    let first = create_own(&host, &guest, json!({"name": "Work"}))
        .await
        .unwrap();
    let second = create_own(&host, &guest, json!({"name": "Side project"}))
        .await
        .unwrap();
    assert_ne!(first.workspace.workspace, second.workspace.workspace);
    assert_eq!(
        workspaces.own_workspaces_of(&guest.user_public_key),
        [
            first.workspace.workspace.clone(),
            second.workspace.workspace.clone()
        ]
    );
    // One more than the host keeps for them is refused, typed, naming both
    // of theirs and the count.
    let refused = create_own(&host, &guest, json!({"name": "Another"}))
        .await
        .unwrap_err();
    let ActionHostError::Refused { reason } = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(
        Refusal::from_reason(&reason).unwrap().reason,
        Reason::TooManyWorkspaces {
            limit: 2,
            existing: vec![
                first.workspace.workspace.clone(),
                second.workspace.workspace.clone()
            ],
        }
    );
    // A plain ask still answers the first they made.
    let again = create_own(&host, &guest, json!({})).await.unwrap();
    assert_eq!(again.workspace.workspace, first.workspace.workspace);

    // After a restart the host answers the same ones, and the same count.
    drop(host);
    drop(workspaces);
    relay.shutdown().await;
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces().clone();
    workspaces.offer_own_workspaces(offer).unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    assert_eq!(
        workspaces.own_workspaces_of(&guest.user_public_key),
        [
            first.workspace.workspace.clone(),
            second.workspace.workspace.clone()
        ]
    );
    let again = create_own(&host, &guest, json!({})).await.unwrap();
    assert_eq!(again.workspace.workspace, first.workspace.workspace);
    let refused = create_own(&host, &guest, json!({"name": "Another"}))
        .await
        .unwrap_err();
    let ActionHostError::Refused { reason } = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(
        Refusal::from_reason(&reason).unwrap().reason,
        Reason::TooManyWorkspaces {
            limit: 2,
            existing: vec![
                first.workspace.workspace.clone(),
                second.workspace.workspace.clone()
            ],
        }
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn the_account_read_lists_every_workspace_of_their_own_and_how_many_they_may_keep() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces().clone();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer {
            max_workspaces: 2,
            ..OwnWorkspaceOffer::free("tilo.test")
        })
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let (_, guest) = guest_device();
    let first = create_own(&host, &guest, json!({"name": "Work"}))
        .await
        .unwrap();
    let second = create_own(&host, &guest, json!({"name": "Side project"}))
        .await
        .unwrap();
    let mut read = super::super::tests::request("", host_account::ACCOUNT, json!({}));
    read.target = host_account::target(host_account::ACCOUNT);
    let account: HostAccount =
        serde_json::from_value(invoke(&host, &guest, read).await.unwrap()).unwrap();
    let standing = account.standing.unwrap();
    assert_eq!(account.host, "tilo.test");
    assert_eq!(standing.account, hex::encode(guest.user_public_key));
    assert_eq!(standing.limits.unwrap().max_workspaces, Some(2));
    assert_eq!(account.plans[0].limits.max_workspaces, Some(2));
    let listed: Vec<&str> = standing
        .usage
        .workspaces
        .iter()
        .map(|usage| usage.workspace.as_str())
        .collect();
    assert_eq!(
        listed,
        [
            first.workspace.workspace.as_str(),
            second.workspace.workspace.as_str()
        ]
    );
    assert_eq!(
        standing.usage.stored_bytes(),
        standing
            .usage
            .workspaces
            .iter()
            .map(|usage| usage.stored_bytes)
            .sum::<u64>()
    );
    assert!(standing.usage.stored_bytes() > 0);
    relay.shutdown().await;
}

#[tokio::test]
async fn a_plan_sets_how_many_workspaces_of_their_own_a_person_keeps() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces().clone();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let (_, guest) = guest_device();
    let plan = tilo_actions::remote::host_account::PlanOffer {
        id: "pro".into(),
        name: "Pro".into(),
        price: None,
        limits: tilo_actions::remote::host_account::Limits {
            max_workspaces: Some(1),
            ..tilo_actions::remote::host_account::Limits::default()
        },
    };
    assert!(workspaces.set_own_plan(&guest.user_public_key, plan.clone()));
    assert!(
        create_own(&host, &guest, json!({"name": "Solo"}))
            .await
            .is_ok()
    );
    // Their plan keeps one, not the ten the host gives everyone.
    let refused = create_own(&host, &guest, json!({"name": "One more"}))
        .await
        .unwrap_err();
    let ActionHostError::Refused { reason } = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(
        Refusal::from_reason(&reason).unwrap().reason,
        Reason::TooManyWorkspaces {
            limit: 1,
            existing: vec![workspaces.own_workspaces_of(&guest.user_public_key)[0].clone()],
        }
    );
    // The account read is the plan they are on, and a stranger is on the
    // free one.
    let mut read = super::super::tests::request("", host_account::ACCOUNT, json!({}));
    read.target = host_account::target(host_account::ACCOUNT);
    let account: HostAccount =
        serde_json::from_value(invoke(&host, &guest, read).await.unwrap()).unwrap();
    assert_eq!(account.plans, [plan]);
    assert_eq!(
        account.standing.unwrap().limits.unwrap().max_workspaces,
        Some(1)
    );
    let (_, stranger) = guest_device();
    let account = workspaces.own_account(&stranger.user_public_key).unwrap();
    assert_eq!(
        account.standing.unwrap().limits.unwrap().max_workspaces,
        Some(OwnWorkspaceOffer::DEFAULT_MAX_WORKSPACES)
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn their_workspaces_of_their_own_share_the_storage_the_host_gives_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces().clone();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let (_, guest) = guest_device();
    let first = create_own(&host, &guest, json!({"name": "Work"}))
        .await
        .unwrap();
    let second = create_own(&host, &guest, json!({"name": "Side project"}))
        .await
        .unwrap();
    let account = workspaces.own_account(&guest.user_public_key).unwrap();
    let usage = account.standing.unwrap().usage;
    let kept = |workspace: &str| {
        usage
            .workspaces
            .iter()
            .find(|usage| usage.workspace == workspace)
            .unwrap()
            .stored_bytes
    };
    let [one, two] = [
        kept(&first.workspace.workspace),
        kept(&second.workspace.workspace),
    ];
    assert!(one > 0 && two > 0, "{one} {two}");
    // Room for all but the last byte of what the two take between them: what
    // they share is full while neither alone would be.
    let plan = tilo_actions::remote::host_account::PlanOffer {
        limits: tilo_actions::remote::host_account::Limits {
            storage_bytes: Some(one + two - 1),
            ..tilo_actions::remote::host_account::Limits::default()
        },
        ..tilo_actions::remote::host_account::PlanOffer::default()
    };
    assert!(workspaces.set_own_plan(&guest.user_public_key, plan));
    let full = workspaces
        .storage_refusal(&first.workspace.workspace)
        .expect("the two together have no room left");
    let Reason::StorageFull {
        limit_bytes,
        used_bytes,
    } = full.reason
    else {
        panic!("{full:?}");
    };
    assert_eq!((limit_bytes, used_bytes), (one + two - 1, one + two));
    let theirs = workspaces
        .storage_refusal(&second.workspace.workspace)
        .expect("either of them is refused while they share a full budget");
    assert!(matches!(theirs.reason, Reason::StorageFull { .. }));
    // Another person's own workspace keeps its own budget whole.
    let (_, stranger) = guest_device();
    let other = create_own(&host, &stranger, json!({})).await.unwrap();
    assert_eq!(workspaces.storage_refusal(&other.workspace.workspace), None);
    relay.shutdown().await;
}

#[tokio::test]
async fn an_agents_grant_makes_its_persons_own_workspace_when_it_makes_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let root = SigningKey::from_bytes(&rand::random());
    let now = tilo_clock::unix_now_ns();
    let grant_key = SigningKey::from_bytes(&rand::random());
    let grant = |scope| {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: grant_key.verifying_key().to_bytes(),
            device_label: "agent".into(),
            capabilities: DeviceCapabilities::agent_grant(scope),
            issued_at_ns: now,
            expires_at_ns: Some(now + 3_600_000_000_000),
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    };
    let narrow = grant(AgentScope::Workspace([7; 16]));
    assert!(matches!(
        create_own(&host, &narrow, json!({})).await,
        Err(ActionHostError::UnknownAction)
    ));
    let all = grant(AgentScope::AllWorkspaces);
    // The grant asks as its session makes it: its person's agent.
    let agent =
        Caller::authenticated(all.user_public_key, all.device_public_key).delegated_to("grant");
    let receipt = settled(host.as_host(&all).invoke(&agent, own_request(json!({}))))
        .await
        .unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the grant's person's own workspace is made: {receipt:?}");
    };
    let created: CreatedWorkspace = serde_json::from_value(output.value.unwrap()).unwrap();
    // The invitation is the person's, which the grant redeems as any new
    // device of theirs does.
    let issued = created.invitation.as_ref().unwrap().decode().unwrap();
    assert!(
        decode_and_verify_invitation(
            &issued.signed_invitation,
            Some(root.verifying_key().to_bytes()),
            &RevocationList::new(),
            tilo_clock::unix_now_ns()
        )
        .is_ok()
    );
    join(workspaces, &created, &grant_key, &all).await;
    assert_eq!(
        standing(
            workspaces,
            &created.workspace.workspace,
            root.verifying_key().to_bytes()
        )
        .await,
        Some((MemberRole::Editor, true)),
        "its person is a member, who administers their own workspace"
    );
    // A key the grant mints for that workspace alone redeems the person's
    // invitation the grant asks for, as the grant did.
    let again = settled(host.as_host(&all).invoke(&agent, own_request(json!({}))))
        .await
        .unwrap();
    let ReceiptStatus::Completed { output } = again.status else {
        panic!("the grant asks again: {again:?}");
    };
    let again: CreatedWorkspace = serde_json::from_value(output.value.unwrap()).unwrap();
    let minted_key = SigningKey::from_bytes(&rand::random());
    let mut minted = prepare_minted_grant(
        &all,
        minted_key.verifying_key().to_bytes(),
        hex::decode(created.workspace.workspace.replace('-', ""))
            .unwrap()
            .try_into()
            .unwrap(),
        "minted",
        now,
        now + 60_000_000_000,
    );
    sign_device_cert(&mut minted, &grant_key).unwrap();
    join(workspaces, &again, &minted_key, &minted).await;
    // Listing answers the grant's person only their own workspace, not the
    // host's others.
    workspaces.create("Someone else's").await.unwrap();
    let mut list = own_request(json!({}));
    list.target.address.action = tilo_relay_session::workspaces::LIST_ROOMS.into();
    let listed = settled(host.as_host(&all).invoke(&agent, list))
        .await
        .unwrap();
    let ReceiptStatus::Completed { output } = listed.status else {
        panic!("the grant lists: {listed:?}");
    };
    let listed: Vec<Value> = serde_json::from_value(output.value.unwrap()).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    relay.shutdown().await;
}

#[tokio::test]
async fn a_full_workspace_of_ones_own_is_refused_and_the_account_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer {
            host: "tilo.test".into(),
            storage_bytes: 1,
            max_workspaces: 1,
        })
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    let other = workspaces.create("Lounge").await.unwrap();
    let (_, guest) = guest_device();
    let created = create_own(&host, &guest, json!({})).await.unwrap();
    let own = &created.workspace;

    let refusal = workspaces
        .storage_refusal(&own.workspace)
        .expect("a byte is full");
    let Reason::StorageFull {
        limit_bytes,
        used_bytes,
    } = refusal.reason
    else {
        panic!("{refusal:?}");
    };
    assert_eq!(limit_bytes, 1);
    assert!(used_bytes > 1, "its record alone takes more");
    // A workspace that is no one's own is never refused for its size.
    assert_eq!(workspaces.storage_refusal(&other.workspace), None);

    // The person's account read says what they keep and may keep.
    let mut read = super::super::tests::request("", host_account::ACCOUNT, json!({}));
    read.target = host_account::target(host_account::ACCOUNT);
    let account: HostAccount =
        serde_json::from_value(invoke(&host, &guest, read).await.unwrap()).unwrap();
    let standing = account.standing.unwrap();
    assert_eq!(account.host, "tilo.test");
    assert_eq!(standing.account, hex::encode(guest.user_public_key));
    assert_eq!(standing.limits.unwrap().storage_bytes, Some(1));
    assert_eq!(standing.usage.stored_bytes(), used_bytes);
    assert_eq!(standing.usage.workspaces[0].workspace, own.workspace);
    relay.shutdown().await;
}
