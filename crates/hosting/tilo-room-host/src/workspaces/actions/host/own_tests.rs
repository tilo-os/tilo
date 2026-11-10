//! Workspaces of people's own on a host that keeps them: anyone's device
//! asks, the ones they have answer every time, another is made for each name
//! with no bound on how many, the person administers each from their first
//! device on, and their workspaces share what the host gives them and keep no
//! more of their writes when it is full.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};
use tilo_actions::remote::host_account::{self, HostAccount, Reason};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeRequest, ReceiptStatus,
};
use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
use tilo_relay_protocol::workspaces::{JoinReply, JoinStep, join_proof_digest};
use tilo_relay_session::workspaces::{
    CREATE_OWN_ROOM, CreatedWorkspace, DELETE_ROOM, HOST_ACTIONS_PROVIDER, HOST_ROOMS_RESOURCE,
    NOT_YOUR_OWN,
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
    join_with(workspaces, issued.signed_invitation, key, device).await;
}

/// `device` redeems `invitation`, the signed bytes of one.
async fn join_with(
    workspaces: &Workspaces,
    invitation: Vec<u8>,
    key: &SigningKey,
    device: &DeviceCert,
) {
    let JoinReply::Challenge { challenge } = workspaces
        .page_join(JoinStep::Ask {
            invitation,
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
            // As the page or desk joining says what its person is called.
            name: device.device_label.clone(),
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
        [
            CREATE_OWN_ROOM,
            DELETE_ROOM,
            tilo_relay_session::workspaces::LIST_ROOMS
        ],
        "a stranger makes, deletes and lists the workspaces of their own, and nothing else"
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

/// How many workspaces of their own one person is given in one test.
const HUNDREDS: usize = 300;
/// How long answering one more workspace of a person's own, listing the ones
/// they have, or reading their account may take once they have hundreds.
const QUICK: std::time::Duration = std::time::Duration::from_millis(200);

#[tokio::test]
async fn a_person_may_keep_hundreds_of_workspaces_of_their_own_and_they_stay_quick() {
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
    let mut made = Vec::new();
    let mut last = std::time::Duration::ZERO;
    for number in 0..HUNDREDS {
        let started = std::time::Instant::now();
        let created = create_own(
            &host,
            &guest,
            json!({"name": format!("Workspace {number}")}),
        )
        .await
        .unwrap();
        last = started.elapsed();
        made.push(created.workspace.workspace);
    }
    assert_eq!(
        workspaces.own_workspaces_of(&guest.user_public_key),
        made,
        "there is no bound on how many workspaces of their own a person keeps"
    );
    assert!(last < QUICK, "the {HUNDREDS}th workspace took {last:?}");
    // Their listing names every one, in the time their own take.
    let mut ask = own_request(json!({}));
    ask.target.address.action = tilo_relay_session::workspaces::LIST_ROOMS.into();
    let started = std::time::Instant::now();
    let listed: Vec<tilo_relay_session::workspaces::WorkspaceListing> =
        serde_json::from_value(invoke(&host, &guest, ask).await.unwrap()).unwrap();
    let listing = started.elapsed();
    assert_eq!(listed.len(), HUNDREDS, "{listed:?}");
    assert!(
        listed.iter().all(|one| made.contains(&one.workspace)),
        "a listing names the workspaces of their own"
    );
    assert!(listing < QUICK, "listing {HUNDREDS} took {listing:?}");
    // Their account read lists every one, measuring what they keep once for
    // all of them.
    let started = std::time::Instant::now();
    let account = workspaces.own_account(&guest.user_public_key).unwrap();
    let read = started.elapsed();
    let standing = account.standing.unwrap();
    assert_eq!(standing.usage.workspaces.len(), HUNDREDS);
    assert_eq!(
        standing.limits.unwrap().max_workspaces,
        None,
        "how many of their own a person may keep is unbounded"
    );
    assert!(read < QUICK, "their account read took {read:?}");
    // The file that says whose each of them is holds every one, one rewrite
    // per workspace made.
    let index = dir.path().join("rooms/own-workspaces.json");
    let kept: serde_json::Value = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    assert_eq!(
        kept[&hex::encode(guest.user_public_key)]
            .as_array()
            .map(Vec::len),
        Some(HUNDREDS)
    );
    eprintln!(
        "{HUNDREDS} workspaces of their own: the last made in {last:?}, listed in {listing:?}, read in {read:?}, whose-names file {} bytes",
        std::fs::metadata(&index).unwrap().len()
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn the_account_read_lists_every_workspace_of_their_own_with_no_bound_on_how_many() {
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
    let mut read = super::super::tests::request("", host_account::ACCOUNT, json!({}));
    read.target = host_account::target(host_account::ACCOUNT);
    let account: HostAccount =
        serde_json::from_value(invoke(&host, &guest, read).await.unwrap()).unwrap();
    let standing = account.standing.unwrap();
    assert_eq!(account.host, "tilo.test");
    assert_eq!(standing.account, hex::encode(guest.user_public_key));
    assert_eq!(
        standing.limits.unwrap().max_workspaces,
        None,
        "no bound on how many workspaces of their own a person keeps"
    );
    assert_eq!(account.plans[0].limits.max_workspaces, None);
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
async fn a_plan_sets_the_storage_a_persons_own_workspaces_keep() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces().clone();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let (_, guest) = guest_device();
    let plan = tilo_actions::remote::host_account::PlanOffer {
        id: "pro".into(),
        name: "Pro".into(),
        price: None,
        limits: tilo_actions::remote::host_account::Limits {
            storage_bytes: Some(2_000_000_000),
            ..tilo_actions::remote::host_account::Limits::default()
        },
    };
    assert!(workspaces.set_own_plan(&guest.user_public_key, plan));
    let account = workspaces.own_account(&guest.user_public_key).unwrap();
    assert_eq!(account.plans[0].id, "pro");
    assert_eq!(
        account.plans[0].limits.storage_bytes,
        Some(2_000_000_000),
        "their plan's storage, not the host's ten gigabytes"
    );
    let limits = account.standing.unwrap().limits.unwrap();
    assert_eq!(limits.storage_bytes, Some(2_000_000_000));
    assert_eq!(limits.max_workspaces, None);
    // A stranger is on the host's own storage.
    let (_, stranger) = guest_device();
    assert_eq!(
        workspaces
            .own_account(&stranger.user_public_key)
            .unwrap()
            .plans[0]
            .limits
            .storage_bytes,
        Some(OwnWorkspaceOffer::DEFAULT_STORAGE_BYTES)
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
    // What the two keep is measured on the disk, and the record a workspace
    // is made of is written as the host settles it: a write still on its way
    // when the account was read lands after it. The budget is set against the
    // size they come to once no write is on its way, which is the size every
    // read of the account finds from then on.
    let settled = |limit: Option<u64>| {
        let workspaces = workspaces.clone();
        let person = guest.user_public_key;
        let workspace = first.workspace.workspace.clone();
        async move {
            let plan = tilo_actions::remote::host_account::PlanOffer {
                limits: tilo_actions::remote::host_account::Limits {
                    storage_bytes: limit,
                    ..tilo_actions::remote::host_account::Limits::default()
                },
                ..tilo_actions::remote::host_account::PlanOffer::default()
            };
            assert!(workspaces.set_own_plan(&person, plan));
            // Measured afresh while they are over what the plan allows: the
            // latest write is in the number before it is taken.
            let mut last = None;
            for _ in 0..100 {
                let refused = workspaces
                    .storage_refusal(&workspace)
                    .expect("they are over the plan they were given");
                let Reason::StorageFull { used_bytes, .. } = refused.reason else {
                    panic!("{refused:?}");
                };
                if last == Some(used_bytes) {
                    return used_bytes;
                }
                last = Some(used_bytes);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("the workspaces' size never stood still: {last:?}");
        }
    };
    // A byte is under both: measuring afresh twice over, what the two keep is
    // what no further write changes.
    let total = settled(Some(1)).await;
    assert!(one < total && two < total, "{one} {two} {total}");
    // Room for all but the last byte of what the two take between them: what
    // they share is full while neither alone would be.
    let plan = tilo_actions::remote::host_account::PlanOffer {
        limits: tilo_actions::remote::host_account::Limits {
            storage_bytes: Some(total - 1),
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
    assert_eq!((limit_bytes, used_bytes), (total - 1, total));
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

#[tokio::test]
async fn a_server_key_reads_its_persons_account_and_an_agent_or_a_narrower_key_none() {
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
    let mut all = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: grant_key.verifying_key().to_bytes(),
        device_label: "backend".into(),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: now,
        expires_at_ns: Some(now + 3_600_000_000_000),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut all, &root).unwrap();
    let read = || {
        let mut read = super::super::tests::request("", host_account::ACCOUNT, json!({}));
        read.target = host_account::target(host_account::ACCOUNT);
        read
    };
    // A backend's key reads what its person's workspaces keep and ran.
    let agent =
        Caller::authenticated(all.user_public_key, all.device_public_key).delegated_to("grant");
    let receipt = settled(host.as_host(&all).invoke(&agent, read()))
        .await
        .unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the grant reads its person's account: {receipt:?}");
    };
    let account: HostAccount = serde_json::from_value(output.value.unwrap()).unwrap();
    assert_eq!(
        account.standing.unwrap().account,
        hex::encode(root.verifying_key().to_bytes())
    );
    // An agent the person connected through consent, reaching every
    // workspace too, reads no account.
    let consented_key = SigningKey::from_bytes(&rand::random());
    let mut consented = DeviceCert {
        device_public_key: consented_key.verifying_key().to_bytes(),
        device_label: "assistant".into(),
        capabilities: DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        ..all.clone()
    };
    sign_device_cert(&mut consented, &root).unwrap();
    let assistant = Caller::authenticated(consented.user_public_key, consented.device_public_key)
        .delegated_to("assistant");
    let refused = settled(host.as_host(&consented).invoke(&assistant, read())).await;
    assert!(
        matches!(refused, Err(ActionHostError::UnknownAction)),
        "{refused:?}"
    );
    // The key it mints for one workspace reads no account.
    let minted_key = SigningKey::from_bytes(&rand::random());
    let mut minted = prepare_minted_grant(
        &all,
        minted_key.verifying_key().to_bytes(),
        [7; 16],
        "minted",
        now,
        now + 60_000_000_000,
    );
    sign_device_cert(&mut minted, &grant_key).unwrap();
    let narrow = Caller::authenticated(minted.user_public_key, minted.device_public_key)
        .delegated_to("minted");
    let refused = settled(host.as_host(&minted).invoke(&narrow, read())).await;
    assert!(
        matches!(refused, Err(ActionHostError::UnknownAction)),
        "{refused:?}"
    );
    relay.shutdown().await;
}

/// A certificate `root` signs for `key` with `capabilities`, lasting an hour.
fn certified(root: &SigningKey, key: &SigningKey, capabilities: DeviceCapabilities) -> DeviceCert {
    certified_as(root, key, capabilities, "key")
}

/// The same, going by `label`: the name a person's row shows once one of their
/// own devices is in the workspace.
fn certified_as(
    root: &SigningKey,
    key: &SigningKey,
    capabilities: DeviceCapabilities,
    label: &str,
) -> DeviceCert {
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// The caller a key's session with the relay makes it: its person's agent.
fn as_key(cert: &DeviceCert) -> Caller {
    Caller::authenticated(cert.user_public_key, cert.device_public_key).delegated_to("key")
}

/// The person's own workspace `argument` asks for, as `cert`'s key asks.
async fn own_as_key(
    host: &HostWorkspaceActions,
    cert: &DeviceCert,
    argument: Value,
) -> CreatedWorkspace {
    let receipt = settled(
        host.as_host(cert)
            .invoke(&as_key(cert), own_request(argument)),
    )
    .await
    .unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the key's person's workspace is answered: {receipt:?}");
    };
    serde_json::from_value(output.value.unwrap()).unwrap()
}

/// `verb` of `workspace`'s governance with `argument`, asked by `cert`'s key.
async fn govern(
    host: &HostWorkspaceActions,
    workspace: &tilo_relay_session::workspaces::WorkspaceListing,
    cert: &DeviceCert,
    verb: &str,
    argument: Value,
) -> Result<Value, ActionHostError> {
    let request = super::super::tests::request(&workspace.workspace, verb, argument);
    let receipt = settled(
        host.in_workspace(workspace.workspace_id)
            .invoke(&as_key(cert), request),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes at once: {other:?}"),
    }
}

#[tokio::test]
async fn a_server_key_invites_another_person_who_enters_with_a_key_of_their_own() {
    use tilo_action_policy::roles::governance;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());

    // Ada's backend makes her workspace with her server key and enters it.
    let ada = SigningKey::from_bytes(&rand::random());
    let backend_key = SigningKey::from_bytes(&rand::random());
    let backend = certified(&ada, &backend_key, DeviceCapabilities::server_key_grant());
    let created = own_as_key(&host, &backend, json!({"name": "Applicant"})).await;
    join(workspaces, &created, &backend_key, &backend).await;
    let workspace = &created.workspace;

    // It invites as an Editor at most, and does nothing else of governance.
    let invite = |role: &str| json!({ "role": role });
    let above = govern(
        &host,
        workspace,
        &backend,
        governance::INVITE,
        invite("moderator"),
    )
    .await;
    assert!(
        matches!(above, Err(ActionHostError::Refused { .. })),
        "{above:?}"
    );
    let unban = json!({"person": hex::encode([3; 32])});
    let unban = govern(&host, workspace, &backend, governance::UNBAN, unban).await;
    assert!(
        matches!(unban, Err(ActionHostError::UnknownAction)),
        "{unban:?}"
    );
    let issued = govern(
        &host,
        workspace,
        &backend,
        governance::INVITE,
        invite("editor"),
    )
    .await
    .unwrap();
    let invitation = hex::decode(issued["invitation"].as_str().unwrap()).unwrap();

    // An agent Ada connected through consent invites nobody.
    let consented_key = SigningKey::from_bytes(&rand::random());
    let all = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
    let consented = certified(&ada, &consented_key, all);
    let entering = own_as_key(&host, &consented, json!({"room": workspace.workspace})).await;
    join(workspaces, &entering, &consented_key, &consented).await;
    let by_agent = govern(
        &host,
        workspace,
        &consented,
        governance::INVITE,
        invite("viewer"),
    )
    .await;
    assert!(
        matches!(by_agent, Err(ActionHostError::UnknownAction)),
        "{by_agent:?}"
    );

    // Rosa, another person, follows the link on a page of hers: an Editor.
    let rosa = SigningKey::from_bytes(&rand::random());
    let page_key = SigningKey::from_bytes(&rand::random());
    let page = certified(&rosa, &page_key, DeviceCapabilities::member());
    join_with(workspaces, invitation, &page_key, &page).await;
    let rosa_person = rosa.verifying_key().to_bytes();
    assert_eq!(
        standing(workspaces, &workspace.workspace, rosa_person).await,
        Some((MemberRole::Editor, false))
    );

    // Her own server key enters the workspace as her, at the role she holds.
    let reviewer_key = SigningKey::from_bytes(&rand::random());
    let reviewer = certified(&rosa, &reviewer_key, DeviceCapabilities::server_key_grant());
    let entering = own_as_key(&host, &reviewer, json!({"room": workspace.workspace})).await;
    assert_eq!(entering.workspace.workspace, workspace.workspace);
    join(workspaces, &entering, &reviewer_key, &reviewer).await;
    assert_eq!(
        standing(workspaces, &workspace.workspace, rosa_person).await,
        Some((MemberRole::Editor, false))
    );
    // She administers nothing there, so her key invites nobody into it.
    let by_rosa = govern(
        &host,
        workspace,
        &reviewer,
        governance::INVITE,
        invite("viewer"),
    )
    .await;
    assert!(by_rosa.is_err(), "{by_rosa:?}");

    // Ada's backend lists who is in it now.
    let members = govern(&host, workspace, &backend, super::super::MEMBERS, json!({}))
        .await
        .unwrap();
    let rosa_row = members
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["person"] == hex::encode(rosa_person))
        .cloned();
    assert_eq!(rosa_row.unwrap()["role"], "editor", "{members}");
    relay.shutdown().await;
}

/// `device`'s person asks this host to list its workspaces, as the door's
/// account path does.
async fn listed_for(
    host: &HostWorkspaceActions,
    device: &DeviceCert,
) -> Vec<tilo_relay_session::workspaces::WorkspaceListing> {
    let mut ask = own_request(json!({}));
    ask.target.address.action = tilo_relay_session::workspaces::LIST_ROOMS.into();
    serde_json::from_value(invoke(host, device, ask).await.unwrap()).unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one workspace's people and its listing read top to bottom"
)]
async fn people_are_named_as_people_and_their_listing_names_what_they_were_invited_into() {
    use tilo_action_policy::roles::governance;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());

    // Ada's backend makes her workspace, enters it as her, and invites a
    // reviewer into it.
    let ada = SigningKey::from_bytes(&rand::random());
    let ada_person = ada.verifying_key().to_bytes();
    let backend_key = SigningKey::from_bytes(&rand::random());
    let backend = certified_as(
        &ada,
        &backend_key,
        DeviceCapabilities::server_key_grant(),
        "Ada's backend",
    );
    let created = own_as_key(&host, &backend, json!({"name": "Applicant: Jane Doe"})).await;
    join(workspaces, &created, &backend_key, &backend).await;
    let workspace = &created.workspace;
    let invited = govern(
        &host,
        workspace,
        &backend,
        governance::INVITE,
        json!({"role": "editor"}),
    )
    .await
    .unwrap();
    let rosa = SigningKey::from_bytes(&rand::random());
    let rosa_person = rosa.verifying_key().to_bytes();
    let page_key = SigningKey::from_bytes(&rand::random());
    let page = certified_as(&rosa, &page_key, DeviceCapabilities::member(), "Rosa Ortiz");
    join_with(
        workspaces,
        hex::decode(invited["invitation"].as_str().unwrap()).unwrap(),
        &page_key,
        &page,
    )
    .await;

    // Ada follows another invitation on a device of her own, and her agent's
    // key enters the workspace as her too: a key of hers never names her.
    let theirs = govern(
        &host,
        workspace,
        &backend,
        governance::INVITE,
        json!({"role": "editor"}),
    )
    .await
    .unwrap();
    let ada_key = SigningKey::from_bytes(&rand::random());
    let ada_page = certified_as(&ada, &ada_key, DeviceCapabilities::member(), "Ada");
    join_with(
        workspaces,
        hex::decode(theirs["invitation"].as_str().unwrap()).unwrap(),
        &ada_key,
        &ada_page,
    )
    .await;
    let agent_key = SigningKey::from_bytes(&rand::random());
    let now = tilo_clock::unix_now_ns();
    let mut agent = prepare_minted_grant(
        &backend,
        agent_key.verifying_key().to_bytes(),
        hex::decode(workspace.workspace.replace('-', ""))
            .unwrap()
            .try_into()
            .unwrap(),
        "Jane Doe's agent",
        now,
        now + 60_000_000_000,
    );
    sign_device_cert(&mut agent, &backend_key).unwrap();
    // A key minted for one workspace enters it with the invitation the key
    // that minted it asks for, as the door hands it over.
    let entering = own_as_key(&host, &backend, json!({"room": workspace.workspace})).await;
    join(workspaces, &entering, &agent_key, &agent).await;

    // The people in it: Ada under her own name, its owner and administrator,
    // Rosa under hers, and no row for the host's device that runs it or for
    // any key of theirs.
    let members = govern(&host, workspace, &backend, super::super::MEMBERS, json!({}))
        .await
        .unwrap();
    let rows = members.as_array().unwrap();
    let row = |person: &[u8; 32]| {
        rows.iter()
            .find(|row| row["person"] == hex::encode(*person))
            .cloned()
            .unwrap_or_else(|| panic!("{person:02x?} has a row in {members}"))
    };
    let ada_row = row(&ada_person);
    assert_eq!(ada_row["name"], "Ada", "{members}");
    assert_eq!(ada_row["owner"], true, "{members}");
    assert_eq!(ada_row["administrator"], true, "{members}");
    let rosa_row = row(&rosa_person);
    assert_eq!(rosa_row["name"], "Rosa Ortiz", "{members}");
    assert_eq!(rosa_row["role"], "editor", "{members}");
    assert_eq!(rosa_row["owner"], false, "{members}");
    assert_eq!(rows.len(), 2, "the two people, and no one else: {members}");
    assert!(
        !rows
            .iter()
            .any(|row| row["person"] == hex::encode(owner.user_public_key)),
        "the host's device runs the workspace without being one of its people: {members}"
    );
    assert!(
        !rows
            .iter()
            .any(|row| row["name"] == "Jane Doe's agent" || row["name"] == "Ada's backend"),
        "a key of theirs names its key, never its person: {members}"
    );

    // Each person's listing names the workspaces they are in with the part
    // they take: Ada's own is hers as its owner, and the workspace she was
    // invited into is Rosa's as an editor.
    let ada_listing = listed_for(&host, &backend).await;
    assert_eq!(
        ada_listing
            .iter()
            .map(|one| (one.workspace.as_str(), one.role.as_deref()))
            .collect::<Vec<_>>(),
        [(workspace.workspace.as_str(), Some("owner"),)],
        "the workspace of her own"
    );
    let rosa_listing = listed_for(&host, &page).await;
    assert_eq!(
        rosa_listing
            .iter()
            .map(|one| (
                one.workspace.as_str(),
                one.name.as_str(),
                one.role.as_deref()
            ))
            .collect::<Vec<_>>(),
        [(
            workspace.workspace.as_str(),
            "Applicant: Jane Doe",
            Some("editor"),
        )],
        "the workspace she was invited into"
    );
    relay.shutdown().await;
}

fn delete_request(workspace: &str) -> InvokeRequest {
    let mut request = super::super::tests::request("", DELETE_ROOM, json!({ "room": workspace }));
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(HOST_ROOMS_RESOURCE, DELETE_ROOM),
    };
    request
}

#[tokio::test]
async fn only_its_owner_deletes_a_workspace_and_everything_it_kept_goes_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let relay = test_host(dir.path(), &key, &owner).await;
    let workspaces = relay.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = HostWorkspaceActions::new();
    host.attach(workspaces.clone());
    // A person's server key makes a workspace for a customer, and keeps it.
    let root = SigningKey::from_bytes(&rand::random());
    let key = SigningKey::from_bytes(&rand::random());
    let server = certified(
        &root,
        &key,
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let created = own_as_key(&host, &server, json!({"name": "Applicant: Jane Doe"})).await;
    let workspace = created.workspace.workspace.clone();
    let workspace_id = created.workspace.workspace_id;
    join(workspaces, &created, &key, &server).await;
    let rooms = dir.path().join("rooms");
    let record = rooms.join(format!("{workspace}.room"));
    assert!(record.exists());
    let person = root.verifying_key().to_bytes();
    assert!(workspaces.owns(&person, &workspace));

    // A key for that one workspace deletes nothing: it is not offered.
    let narrow = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::agent_grant(AgentScope::Workspace([7; 16])),
    );
    assert!(matches!(
        settled(
            host.as_host(&narrow)
                .invoke(&as_key(&narrow), delete_request(&workspace))
        )
        .await,
        Err(ActionHostError::UnknownAction)
    ));
    // Nor does anyone else, who learns only that it is not theirs.
    let (_, stranger) = guest_device();
    let refused = invoke(&host, &stranger, delete_request(&workspace)).await;
    assert!(
        matches!(&refused, Err(ActionHostError::Refused { reason }) if reason.contains(NOT_YOUR_OWN)),
        "{refused:?}"
    );
    assert!(record.exists(), "a refusal deletes nothing");

    // Its owner's key deletes it.
    let receipt = settled(
        host.as_host(&server)
            .invoke(&as_key(&server), delete_request(&workspace)),
    )
    .await
    .unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the owner deletes their workspace: {receipt:?}");
    };
    assert_eq!(output.value, Some(json!({ "deleted": workspace })));

    // It is gone from every listing and from what its person keeps.
    assert!(!workspaces.owns(&person, &workspace));
    assert!(workspaces.own_workspaces_of(&person).is_empty());
    assert!(
        !workspaces
            .list()
            .await
            .iter()
            .any(|summary| summary.workspace == workspace)
    );
    let account = workspaces.own_account(&person).unwrap();
    assert!(
        account.standing.unwrap().usage.workspaces.is_empty(),
        "what it kept no longer counts against its person"
    );
    // Nothing of it stays on disk but the mark that it was deleted.
    assert!(!record.exists());
    assert!(rooms.join(format!("{workspace}.deleted")).exists());
    let left: Vec<String> = std::fs::read_dir(&rooms)
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(workspace.as_str()))
        .collect();
    assert_eq!(left, [format!("{workspace}.deleted")]);
    assert!(
        !dir.path()
            .join("workspaces")
            .join(workspace_id.to_string())
            .exists()
    );

    // Deleting it again, or making it again under its identity, is refused.
    let again = settled(
        host.as_host(&server)
            .invoke(&as_key(&server), delete_request(&workspace)),
    )
    .await;
    assert!(
        matches!(&again, Err(ActionHostError::Refused { reason }) if reason.contains(NOT_YOUR_OWN)),
        "{again:?}"
    );
    assert!(
        workspaces
            .create_workspace("Again", Some(&workspace))
            .await
            .is_err()
    );
}
