//! Workspaces of people's own on a host that keeps them: anyone's device
//! asks, the ones they have answer every time, another is made for each name
//! with no bound on how many, the person administers each from their first
//! device on, and their workspaces share what the host gives them and keep no
//! more of their writes when it is full.

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::server_account::{self, Reason, ServerAccount};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, CatalogueQuery, InvocationReceipt, InvokeRequest,
    ReceiptStatus,
};
use tilo_peer_state::join::{Role, decode_and_verify_invitation};
use tilo_server_protocol::workspaces::JoinReply;
use tilo_server_protocol::workspaces::{
    AdminRefusal, CREATE_OWN_WORKSPACE, CreatedWorkspace, DELETE_WORKSPACE, HOST_ACTIONS_PROVIDER,
    HOST_WORKSPACES_RESOURCE,
};
use tilo_user_identity::{
    AgentScope, DeviceCapabilities, DeviceCert, RevocationList, prepare_minted_grant,
    sign_device_cert,
};

use super::super::ServiceWorkspaceActions;
use super::super::tests::{Answering, answered, caller, settled};
use crate::workspaces::fixtures::list_itself_on_link;
use crate::workspaces::tests::{governor, guest_device, test_host};
use crate::workspaces::{IssueInvitationRequest, OwnWorkspaceOffer, WaitListSecrets, Workspaces};

/// `request` by `caller` on the connection `device` certified to the host, answered by its
/// person where it waits for them and one answers ([`Answering`]).
async fn on_host(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<InvocationReceipt, ActionCallError> {
    let first = settled(host.as_host(device).invoke(caller, request.clone())).await;
    let workspaces = host.shared.workspaces.get().unwrap();
    answered(workspaces, (None, Some(device)), caller, &request, first).await
}

fn own_request(argument: Value) -> InvokeRequest {
    let mut request = super::super::tests::request("", CREATE_OWN_WORKSPACE, argument);
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(HOST_WORKSPACES_RESOURCE, CREATE_OWN_WORKSPACE),
    };
    request
}

async fn invoke(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
    request: InvokeRequest,
) -> Result<Value, ActionCallError> {
    let receipt = on_host(host, device, &caller(device), request).await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

pub(super) async fn create_own(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
    argument: Value,
) -> Result<CreatedWorkspace, ActionCallError> {
    invoke(host, device, own_request(argument))
        .await
        .map(|value| serde_json::from_value(value).unwrap())
}

/// Name the home workspace whose people `workspaces` keeps workspaces of
/// their own for, as a site's server names its own: returns it.
async fn home_of(workspaces: &Workspaces) -> String {
    let home = workspaces.create("Lounge").await.unwrap().workspace;
    assert!(workspaces.admit_own_through(&home));
    home
}

/// A host keeping workspaces of people's own for the people its home
/// workspace lets in: returns the home.
pub(super) async fn open(workspaces: &Workspaces) -> String {
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    home_of(workspaces).await
}

/// Let `person` in as the site does for someone it invites: an invitation
/// into `home` names them.
pub(super) async fn let_in(workspaces: &Workspaces, home: &str, person: [u8; 32]) -> Vec<u8> {
    workspaces
        .issue(
            home,
            workspaces.device_cert(),
            IssueInvitationRequest {
                request_id: rand::random(),
                role: Role::Viewer,
                recipient_user_public_key: Some(person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap()
        .signed_invitation
}

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

/// `device` redeems `created`'s invitation, as a page joining does.
async fn join(
    workspaces: &Workspaces,
    created: &CreatedWorkspace,
    device: (&SigningKey, &DeviceCert),
) {
    let issued = created.invitation.as_ref().unwrap().decode().unwrap();
    join_with(workspaces, issued.signed_invitation, device).await;
}

/// `device` redeems `invitation`, the signed bytes of one.
async fn join_with(
    workspaces: &Workspaces,
    invitation: Vec<u8>,
    (key, device): (&SigningKey, &DeviceCert),
) {
    let admitted = workspaces
        .admit_proven(
            invitation,
            (device.clone(), key),
            rand::random(),
            // As the page or shell joining says what its person is called.
            device.device_label.as_str(),
            None,
        )
        .await;
    assert!(
        matches!(admitted, JoinReply::Admitted { .. }),
        "{admitted:?}"
    );
}

/// How `person` stands in `workspace`: their role and whether they
/// administer it.
async fn access(
    workspaces: &Workspaces,
    workspace: &str,
    person: [u8; 32],
) -> Option<(Role, bool)> {
    workspaces
        .list()
        .await
        .into_iter()
        .find(|summary| summary.workspace == workspace)?
        .members
        .into_iter()
        .find(|member| member.root() == Some(person))
        .and_then(|member| Some((member.role?, member.administrator)))
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "a person's own workspace from the offer through asking again"
)]
async fn a_person_let_in_gets_a_workspace_of_their_own_and_administers_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = super::super::tests::host_dispatch(workspaces);
    let (guest_key, guest) = guest_device();

    // A host that keeps none offers a stranger nothing.
    assert!(offered(&host, &guest, &caller(&guest)).await.is_empty());
    assert!(matches!(
        create_own(&host, &guest, json!({})).await,
        Err(ActionCallError::Refused { .. })
    ));

    assert!(
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap()
    );
    let home = home_of(workspaces).await;
    let_in(workspaces, &home, guest.user_public_key).await;
    let mut offers = offered(&host, &guest, &caller(&guest)).await;
    offers.sort();
    assert_eq!(
        offers,
        [
            tilo_server_protocol::workspaces::BRANCH_OWN_WORKSPACE,
            CREATE_OWN_WORKSPACE,
            DELETE_WORKSPACE,
            tilo_server_protocol::workspaces::LIST_WORKSPACES,
        ],
        "a stranger makes, branches, deletes and lists the workspaces of their own, and nothing \
         else"
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
    assert_eq!(invitation.grant().role, Role::Editor);
    assert!(
        decode_and_verify_invitation(
            &issued.signed_invitation,
            Some([9; 32]),
            &RevocationList::new(),
            tilo_clock::unix_now_ns()
        )
        .is_err()
    );

    // It is theirs: its governance names them its owner, and their device
    // comes in as the owner's.
    join(workspaces, &created, (&guest_key, &guest)).await;
    assert_eq!(
        access(
            workspaces,
            &created.workspace.workspace,
            guest.user_public_key
        )
        .await,
        Some((Role::Owner, true))
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
        json!({"workspace": created.workspace.workspace.clone()}),
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
    let refused = create_own(&host, &guest, json!({"workspace": other.workspace.clone()}))
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ActionCallError::Refused { .. }),
        "{refused:?}"
    );

    // Someone else gets their own; the server's own workspaces are no one's.
    let (_, stranger) = guest_device();
    let_in(workspaces, &home, stranger.user_public_key).await;
    let theirs = create_own(&host, &stranger, json!({})).await.unwrap();
    assert_eq!(theirs.workspace.name, "My workspace");
    assert_ne!(theirs.workspace.workspace, created.workspace.workspace);

    // One who reads German founds theirs in German, and it keeps the name it
    // was founded under however they ask after that.
    let (_, german) = guest_device();
    let_in(workspaces, &home, german.user_public_key).await;
    let ihr = create_own(&host, &german, json!({"language": "de-DE"}))
        .await
        .unwrap();
    assert_eq!(ihr.workspace.name, "Mein Arbeitsbereich");
    let wieder = create_own(&host, &german, json!({"language": "en-US"}))
        .await
        .unwrap();
    assert_eq!(wieder.workspace.workspace, ihr.workspace.workspace);
    assert_eq!(wieder.workspace.name, "Mein Arbeitsbereich");
    server.shutdown().await;
}

/// How many workspaces of their own one person is given in one test: enough
/// that a create or a listing that walked the ones they keep would show.
const MANY: usize = 24;

#[tokio::test]
async fn a_person_may_keep_many_workspaces_of_their_own_and_they_stay_quick() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let home = open(&workspaces).await;
    let host = super::super::tests::host_dispatch(&workspaces);
    let (_, guest) = guest_device();
    let_in(&workspaces, &home, guest.user_public_key).await;
    let mut made = Vec::new();
    let listed_before = workspaces.own_directory_listings();
    for number in 0..MANY {
        let created = create_own(
            &host,
            &guest,
            json!({"name": format!("Workspace {number}")}),
        )
        .await
        .unwrap();
        made.push(created.workspace.workspace);
    }
    assert_eq!(
        workspaces.own_workspaces_of(&guest.user_public_key),
        made,
        "there is no bound on how many workspaces of their own a person keeps"
    );
    // What a create costs does not grow with the workspaces on the host: it
    // reads none of them, so their last costs what their first did.
    assert_eq!(
        workspaces.own_directory_listings(),
        listed_before,
        "making one of a person's workspaces reads none of the server's"
    );
    // Their listing names every one, and is looked up by their own workspaces
    // rather than by reading the ones the host keeps.
    let mut ask = own_request(json!({}));
    ask.target.address.action = tilo_server_protocol::workspaces::LIST_WORKSPACES.into();
    let listed: Vec<tilo_server_protocol::workspaces::WorkspaceListing> =
        serde_json::from_value(invoke(&host, &guest, ask).await.unwrap()).unwrap();
    assert_eq!(listed.len(), MANY, "{listed:?}");
    assert!(
        listed.iter().all(|one| made.contains(&one.workspace)),
        "a listing names the workspaces of their own"
    );
    assert_eq!(
        workspaces.own_directory_listings(),
        listed_before,
        "listing their own reads none of the workspaces the host keeps"
    );
    // Their account read measures every one in a single pass over the
    // records directory, not one pass per workspace.
    let measured_before = workspaces.own_directory_listings();
    let account = workspaces.own_account(&guest.user_public_key).unwrap();
    assert_eq!(
        workspaces.own_directory_listings() - measured_before,
        1,
        "their account read lists the workspaces directory once for all {MANY}"
    );
    let subscription = account.subscription.unwrap();
    assert_eq!(subscription.usage.workspaces.len(), MANY);
    assert_eq!(
        subscription.limits.unwrap().max_workspaces,
        None,
        "how many of their own a person may keep is unbounded"
    );
    // The file that says whose each of them is holds every one, one rewrite
    // per workspace made.
    let index = dir.path().join("workspaces/own-workspaces.json");
    let kept: serde_json::Value = serde_json::from_slice(&std::fs::read(&index).unwrap()).unwrap();
    assert_eq!(
        kept[&hex::encode(guest.user_public_key)]
            .as_array()
            .map(Vec::len),
        Some(MANY)
    );
    eprintln!(
        "{MANY} workspaces of their own: made, listed and read with no pass over the server's resources; whose-names file {} bytes",
        std::fs::metadata(&index).unwrap().len()
    );
    server.shutdown().await;
}

/// Two of a person's devices asking for a workspace of their own in the same
/// moment are answered one workspace: the first makes it, and the second
/// waits for what the first made rather than making another.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_of_a_persons_devices_asking_at_once_are_answered_one_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let home = open(&workspaces).await;
    let actions = super::super::tests::host_dispatch(&workspaces);
    let root = SigningKey::from_bytes(&rand::random());
    let person = root.verifying_key().to_bytes();
    let_in(&workspaces, &home, person).await;
    let phone = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::member(),
    );
    let laptop = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::member(),
    );
    let (one, two) = tokio::join!(
        create_own(&actions, &phone, json!({})),
        create_own(&actions, &laptop, json!({})),
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_eq!(
        one.workspace.workspace, two.workspace.workspace,
        "both of their devices are answered the same workspace"
    );
    assert_eq!(
        workspaces.own_workspaces_of(&person),
        std::slice::from_ref(&one.workspace.workspace),
        "they keep one workspace of their own"
    );
    assert_eq!(
        workspaces
            .own_account(&person)
            .unwrap()
            .subscription
            .unwrap()
            .usage
            .workspaces
            .len(),
        1
    );
    server.shutdown().await;
}

#[tokio::test]
async fn the_account_read_lists_every_workspace_of_their_own_with_no_bound_on_how_many() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let home = open(&workspaces).await;
    let host = super::super::tests::host_dispatch(&workspaces);
    let (_, guest) = guest_device();
    let_in(&workspaces, &home, guest.user_public_key).await;
    let first = create_own(&host, &guest, json!({"name": "Work"}))
        .await
        .unwrap();
    let second = create_own(&host, &guest, json!({"name": "Side project"}))
        .await
        .unwrap();
    let mut read = super::super::tests::request("", server_account::ACCOUNT, json!({}));
    read.target = server_account::target(server_account::ACCOUNT);
    let account: ServerAccount =
        serde_json::from_value(invoke(&host, &guest, read).await.unwrap()).unwrap();
    let subscription = account.subscription.unwrap();
    assert_eq!(account.server, "tilo.test");
    assert_eq!(subscription.account, hex::encode(guest.user_public_key));
    assert_eq!(
        subscription.limits.unwrap().max_workspaces,
        None,
        "no bound on how many workspaces of their own a person keeps"
    );
    assert_eq!(account.plans[0].limits.max_workspaces, None);
    let listed: Vec<&str> = subscription
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
        subscription.usage.stored_bytes(),
        subscription
            .usage
            .workspaces
            .iter()
            .map(|usage| usage.stored_bytes)
            .sum::<u64>()
    );
    assert!(subscription.usage.stored_bytes() > 0);
    server.shutdown().await;
}

#[tokio::test]
async fn a_plan_sets_the_storage_a_persons_own_workspaces_keep() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let home = open(&workspaces).await;
    let (_, guest) = guest_device();
    let_in(&workspaces, &home, guest.user_public_key).await;
    let plan = tilo_actions::remote::server_account::PlanOffer {
        id: "pro".into(),
        name: "Pro".into(),
        price: None,
        limits: tilo_actions::remote::server_account::Limits {
            storage_bytes: Some(2_000_000_000),
            ..tilo_actions::remote::server_account::Limits::default()
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
    let limits = account.subscription.unwrap().limits.unwrap();
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
    server.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one shared budget, followed from what each keeps to what neither may keep"
)]
async fn their_workspaces_of_their_own_share_the_storage_the_server_gives_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    let home = open(&workspaces).await;
    let host = super::super::tests::host_dispatch(&workspaces);
    let (_, guest) = guest_device();
    let_in(&workspaces, &home, guest.user_public_key).await;
    let first = create_own(&host, &guest, json!({"name": "Work"}))
        .await
        .unwrap();
    let second = create_own(&host, &guest, json!({"name": "Side project"}))
        .await
        .unwrap();
    // What each of them is measured to keep: the file a workspace is made of
    // is written as the server settles it, so one still on its way is measured
    // 0 until it lands.
    let kept = |workspace: &str| {
        workspaces
            .own_account(&guest.user_public_key)
            .and_then(|account| account.subscription)
            .and_then(|subscription| {
                subscription
                    .usage
                    .workspaces
                    .into_iter()
                    .find(|usage| usage.workspace == workspace)
            })
            .map_or(0, |usage| usage.stored_bytes)
    };
    let [one, two] = tilo_test_env::within("both workspaces to be measured", async {
        loop {
            let (one, two) = (
                kept(&first.workspace.workspace),
                kept(&second.workspace.workspace),
            );
            if one > 0 && two > 0 {
                break [one, two];
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    // What the two keep is measured on the disk, and the file a workspace is
    // made of is written as the server settles it: a write still on its way
    // when the account was read lands after it. The budget is set against the
    // size they come to once no write is on its way, which is the size every
    // read of the account finds from then on.
    let settled = |limit: Option<u64>| {
        let workspaces = workspaces.clone();
        let person = guest.user_public_key;
        let workspace = first.workspace.workspace.clone();
        async move {
            let plan = tilo_actions::remote::server_account::PlanOffer {
                limits: tilo_actions::remote::server_account::Limits {
                    storage_bytes: limit,
                    ..tilo_actions::remote::server_account::Limits::default()
                },
                ..tilo_actions::remote::server_account::PlanOffer::default()
            };
            assert!(workspaces.set_own_plan(&person, plan));
            // Measured afresh while they are over what the plan allows: the
            // latest write is in the number before it is taken, and the number
            // counts only once no further write changes it.
            let mut last = None;
            let mut steady = 0;
            for _ in 0..200 {
                let refused = workspaces
                    .storage_refusal(&workspace)
                    .expect("they are over the plan they were given");
                let Reason::StorageFull { used_bytes, .. } = refused.reason else {
                    panic!("{refused:?}");
                };
                steady = if last == Some(used_bytes) {
                    steady + 1
                } else {
                    0
                };
                if steady == 3 {
                    return used_bytes;
                }
                last = Some(used_bytes);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
    let plan = tilo_actions::remote::server_account::PlanOffer {
        limits: tilo_actions::remote::server_account::Limits {
            storage_bytes: Some(total - 1),
            ..tilo_actions::remote::server_account::Limits::default()
        },
        ..tilo_actions::remote::server_account::PlanOffer::default()
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
    assert_eq!(limit_bytes, total - 1);
    // Either of them alone is under the plan: only what the two keep between
    // them fills it. (A write that lands after the plan is set may take them
    // further over, so this is no exact number.)
    assert!(
        used_bytes >= limit_bytes,
        "{used_bytes} B kept of {limit_bytes} B shared"
    );
    let theirs = workspaces
        .storage_refusal(&second.workspace.workspace)
        .expect("either of them is refused while they share a full budget");
    assert!(matches!(theirs.reason, Reason::StorageFull { .. }));
    // Another person's own workspace keeps its own budget whole.
    let (_, stranger) = guest_device();
    let_in(&workspaces, &home, stranger.user_public_key).await;
    let other = create_own(&host, &stranger, json!({})).await.unwrap();
    assert_eq!(workspaces.storage_refusal(&other.workspace.workspace), None);
    server.shutdown().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "each scope of grant, from the one that makes none to the one that makes them all"
)]
async fn an_agents_grant_makes_its_persons_own_workspace_when_it_makes_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    // Nobody answers: the grant's own scope, which its person signed, is their yes to making a
    // workspace.
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);
    let root = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, root.verifying_key().to_bytes()).await;
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
    // A grant for one workspace makes none, and asks for no other than its own.
    let narrow = grant(AgentScope::Workspace([7; 16]));
    assert!(matches!(
        create_own(&host, &narrow, json!({})).await,
        Err(ActionCallError::Refused { .. })
    ));
    assert!(matches!(
        create_own(
            &host,
            &narrow,
            json!({"workspace": "08080808-0808-4808-8808-080808080808"})
        )
        .await,
        Err(ActionCallError::Refused { .. })
    ));
    let all = grant(AgentScope::AllWorkspaces);
    // The grant asks as its session makes it: its person's agent.
    let agent =
        Caller::authenticated(all.user_public_key, all.device_public_key).delegated_to("grant");
    let receipt = on_host(&host, &all, &agent, own_request(json!({})))
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
    join(workspaces, &created, (&grant_key, &all)).await;
    assert_eq!(
        access(
            workspaces,
            &created.workspace.workspace,
            root.verifying_key().to_bytes()
        )
        .await,
        Some((Role::Owner, true)),
        "its person owns their own workspace"
    );
    // A key the grant mints for that workspace alone redeems the person's
    // invitation the grant asks for, as the grant did.
    let again = on_host(&host, &all, &agent, own_request(json!({})))
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
    join(workspaces, &again, (&minted_key, &minted)).await;
    // Listing answers the grant's person only their own workspace, not the
    // host's others.
    workspaces.create("Someone else's").await.unwrap();
    let mut list = own_request(json!({}));
    list.target.address.action = tilo_server_protocol::workspaces::LIST_WORKSPACES.into();
    let listed = on_host(&host, &all, &agent, list).await.unwrap();
    let ReceiptStatus::Completed { output } = listed.status else {
        panic!("the grant lists: {listed:?}");
    };
    let listed: Vec<Value> = serde_json::from_value(output.value.unwrap()).unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    server.shutdown().await;
}

/// An agent's certificate is its person's yes only to what it grants. A grant for one workspace
/// grants making none, so its agent asks before making its person's own workspace, and with
/// nothing of theirs here to keep the question in, it is refused for that. A grant that makes
/// workspaces makes one without asking, and still asks before deleting it: its certificate grants
/// no deleting, and the question waits in the workspace it made.
#[tokio::test]
async fn an_agent_asks_before_a_host_operation_its_certificate_does_not_grant() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);
    let root = SigningKey::from_bytes(&rand::random());
    let person = root.verifying_key().to_bytes();
    let_in(workspaces, &home, person).await;
    let as_agent = |cert: &DeviceCert| {
        Caller::authenticated(cert.user_public_key, cert.device_public_key).delegated_to("grant")
    };

    let narrow = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::agent_grant(AgentScope::Workspace([7; 16])),
    );
    let refused = on_host(&host, &narrow, &as_agent(&narrow), own_request(json!({}))).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason })
            if reason.contains("could not be kept for them to answer")),
        "{refused:?}"
    );
    assert!(
        workspaces.own_workspaces_of(&person).is_empty(),
        "nothing is made"
    );

    let all = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let made = on_host(&host, &all, &as_agent(&all), own_request(json!({})))
        .await
        .unwrap();
    let ReceiptStatus::Completed { output } = made.status else {
        panic!("its grant makes its person's own workspace at once: {made:?}");
    };
    let created: CreatedWorkspace = serde_json::from_value(output.value.unwrap()).unwrap();
    let workspace = created.workspace.workspace.clone();
    assert!(workspaces.owns(&person, &workspace));

    // The dispatch's own receipt, as its caller reads it: [`on_host`] would answer a call that
    // waits with what its person is shown.
    let deleting = delete_request(&workspace);
    let waits = settled(host.as_host(&all).invoke(&as_agent(&all), deleting))
        .await
        .unwrap();
    assert!(
        !matches!(waits.status, ReceiptStatus::Completed { .. }),
        "deleting waits for its person: {waits:?}"
    );
    let asked = tilo_actions::remote::asked::AskedRecords::asked_of(
        &*workspaces.asked_records().unwrap(),
        &person,
    );
    assert!(
        asked.iter().any(|asked| asked.workspace == workspace),
        "the question waits in the workspace it made: {asked:?}"
    );
    assert!(
        workspaces.owns(&person, &workspace),
        "nothing is deleted before a yes"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_full_workspace_of_ones_own_is_refused_and_the_account_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer {
            host: "tilo.test".into(),
            storage_bytes: 1,
        })
        .unwrap();
    let host = super::super::tests::host_dispatch(workspaces);
    let other = workspaces.create("Lounge").await.unwrap();
    assert!(workspaces.admit_own_through(&other.workspace));
    let (_, guest) = guest_device();
    let_in(workspaces, &other.workspace, guest.user_public_key).await;
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
    let mut read = super::super::tests::request("", server_account::ACCOUNT, json!({}));
    read.target = server_account::target(server_account::ACCOUNT);
    let account: ServerAccount =
        serde_json::from_value(invoke(&host, &guest, read).await.unwrap()).unwrap();
    let subscription = account.subscription.unwrap();
    assert_eq!(account.server, "tilo.test");
    assert_eq!(subscription.account, hex::encode(guest.user_public_key));
    assert_eq!(subscription.limits.unwrap().storage_bytes, Some(1));
    assert_eq!(subscription.usage.stored_bytes(), used_bytes);
    assert_eq!(subscription.usage.workspaces[0].workspace, own.workspace);
    server.shutdown().await;
}

#[tokio::test]
async fn a_server_key_reads_its_persons_account_and_an_agent_or_a_narrower_key_none() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);
    let root = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, root.verifying_key().to_bytes()).await;
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
        let mut read = super::super::tests::request("", server_account::ACCOUNT, json!({}));
        read.target = server_account::target(server_account::ACCOUNT);
        read
    };
    // A backend's key reads what its person's workspaces keep and ran.
    let agent =
        Caller::authenticated(all.user_public_key, all.device_public_key).delegated_to("grant");
    let receipt = on_host(&host, &all, &agent, read()).await.unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the grant reads its person's account: {receipt:?}");
    };
    let account: ServerAccount = serde_json::from_value(output.value.unwrap()).unwrap();
    assert_eq!(
        account.subscription.unwrap().account,
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
    let refused = on_host(&host, &consented, &assistant, read()).await;
    assert!(
        matches!(refused, Err(ActionCallError::UnknownAction)),
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
    let refused = on_host(&host, &minted, &narrow, read()).await;
    assert!(
        matches!(refused, Err(ActionCallError::UnknownAction)),
        "{refused:?}"
    );
    server.shutdown().await;
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

/// The caller a key's session with the server makes it: its person's agent.
fn as_key(cert: &DeviceCert) -> Caller {
    Caller::authenticated(cert.user_public_key, cert.device_public_key).delegated_to("key")
}

/// The person's own workspace `argument` asks for, as `cert`'s key asks.
async fn own_as_key(
    host: &ServiceWorkspaceActions,
    cert: &DeviceCert,
    argument: Value,
) -> CreatedWorkspace {
    let receipt = on_host(host, cert, &as_key(cert), own_request(argument))
        .await
        .unwrap();
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the key's person's workspace is answered: {receipt:?}");
    };
    serde_json::from_value(output.value.unwrap()).unwrap()
}

/// `verb` of `workspace`'s governance with `argument`, asked by `cert`'s key.
async fn govern(
    host: &ServiceWorkspaceActions,
    workspace: &tilo_server_protocol::workspaces::WorkspaceListing,
    cert: &DeviceCert,
    verb: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let request = super::super::tests::request(&workspace.workspace, verb, argument);
    let key = as_key(cert);
    let first = settled(
        host.in_workspace(workspace.workspace_id)
            .invoke(&key, request.clone()),
    )
    .await;
    let receipt = answered(
        host.shared.workspaces.get().unwrap(),
        (Some(workspace.workspace_id), None),
        &key,
        &request,
        first,
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("governance completes at once: {other:?}"),
    }
}

#[tokio::test]
async fn a_server_key_invites_another_person_who_enters_with_a_key_of_their_own() {
    use tilo_actions::remote::WORKSPACE_INVITE;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);

    // Ada's backend makes her workspace with her server key and enters it.
    let ada = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, ada.verifying_key().to_bytes()).await;
    let backend_key = SigningKey::from_bytes(&rand::random());
    let backend = certified(&ada, &backend_key, DeviceCapabilities::server_key_grant());
    let created = own_as_key(&host, &backend, json!({"name": "Applicant"})).await;
    join(workspaces, &created, (&backend_key, &backend)).await;
    let workspace = &created.workspace;

    // It governs as Ada does: she owns the workspace, so it invites at
    // whatever role she may give.
    let invite = |role: &str| json!({ "role": role });
    govern(
        &host,
        workspace,
        &backend,
        WORKSPACE_INVITE,
        invite("moderator"),
    )
    .await
    .unwrap();
    let issued = govern(
        &host,
        workspace,
        &backend,
        WORKSPACE_INVITE,
        invite("editor"),
    )
    .await
    .unwrap();
    let invitation = hex::decode(issued["invitation"].as_str().unwrap()).unwrap();

    // An agent Ada connected through consent enters, and asks her first
    // before it invites anyone.
    let consented_key = SigningKey::from_bytes(&rand::random());
    let all = DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces);
    let consented = certified(&ada, &consented_key, all);
    let ada_asked = Answering::approving();
    let entering = own_as_key(&host, &consented, json!({"workspace": workspace.workspace})).await;
    join(workspaces, &entering, (&consented_key, &consented)).await;
    govern(
        &host,
        workspace,
        &consented,
        WORKSPACE_INVITE,
        invite("viewer"),
    )
    .await
    .unwrap();
    assert_eq!(
        ada_asked.asked().len(),
        1,
        "inviting; entering asks nothing"
    );

    // Rosa, another person, follows the link on a page of hers: an Editor.
    let rosa = SigningKey::from_bytes(&rand::random());
    let page_key = SigningKey::from_bytes(&rand::random());
    let page = certified(&rosa, &page_key, DeviceCapabilities::member());
    join_with(workspaces, invitation, (&page_key, &page)).await;
    let rosa_person = rosa.verifying_key().to_bytes();
    assert_eq!(
        access(workspaces, &workspace.workspace, rosa_person).await,
        Some((Role::Editor, false))
    );

    // Her own server key enters the workspace as her, at the role she holds.
    let reviewer_key = SigningKey::from_bytes(&rand::random());
    let reviewer = certified(&rosa, &reviewer_key, DeviceCapabilities::server_key_grant());
    let entering = own_as_key(&host, &reviewer, json!({"workspace": workspace.workspace})).await;
    assert_eq!(entering.workspace.workspace, workspace.workspace);
    join(workspaces, &entering, (&reviewer_key, &reviewer)).await;
    assert_eq!(
        access(workspaces, &workspace.workspace, rosa_person).await,
        Some((Role::Editor, false))
    );
    // She administers nothing there, so her key invites nobody into it.
    let by_rosa = govern(
        &host,
        workspace,
        &reviewer,
        WORKSPACE_INVITE,
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
        .find(|member| member["member"]["person"] == hex::encode(rosa_person))
        .cloned();
    assert_eq!(rosa_row.unwrap()["role"], "editor", "{members}");
    server.shutdown().await;
}

/// A workspace this host handed to a person, as a deployment hands its
/// Lounge to its owner, is entered by a key of theirs at once: the invitation
/// the host answers it with admits the key on its person's certificate, as no
/// invitation gives the owner's place, and the workspace stays theirs.
#[tokio::test]
async fn a_key_of_the_person_a_workspace_was_handed_to_enters_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);

    let ada = SigningKey::from_bytes(&rand::random());
    let person = ada.verifying_key().to_bytes();
    workspaces
        .declare_owner(&home, person, Some("Ada"))
        .await
        .unwrap();
    let backend_signer = SigningKey::from_bytes(&rand::random());
    let backend = certified(
        &ada,
        &backend_signer,
        DeviceCapabilities::server_key_grant(),
    );
    let entering = own_as_key(&host, &backend, json!({"workspace": home})).await;
    assert_eq!(entering.workspace.workspace, home);
    join(workspaces, &entering, (&backend_signer, &backend)).await;
    assert_eq!(
        access(workspaces, &home, person)
            .await
            .map(|(role, _)| role),
        Some(Role::Owner),
        "the key's person owns the workspace it entered"
    );
    server.shutdown().await;
}

/// An owner's server key lets a person in by their key, at once, as an
/// administrator: that person's own key then enters and brings others in, as
/// far as they may. Handing the workspace on is the owner's own to sign, so
/// the host refuses to do it for them. An administrator makes administrators
/// but no owner, and an editor's key adds nobody.
#[tokio::test]
async fn an_owners_key_adds_an_administrator_by_key_whose_own_key_then_brings_people_in() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);

    let ada = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, ada.verifying_key().to_bytes()).await;
    let ada_signer = SigningKey::from_bytes(&rand::random());
    let ada_key = certified(&ada, &ada_signer, DeviceCapabilities::server_key_grant());
    let created = own_as_key(&host, &ada_key, json!({"name": "Lounge"})).await;
    join(workspaces, &created, (&ada_signer, &ada_key)).await;
    let workspace = &created.workspace;
    assert!(
        workspaces
            .is_owner(&workspace.workspace, ada.verifying_key().to_bytes())
            .await,
        "the workspace Ada made here is hers"
    );

    // Grace has never been here. The host does not hand Ada's workspace to
    // her: that is Ada's to sign on a device of her own.
    let grace = SigningKey::from_bytes(&rand::random());
    let grace_person = grace.verifying_key().to_bytes();
    let handed = add_member(&host, workspace, &ada_key, &grace, "owner").await;
    assert!(
        matches!(&handed, Err(ActionCallError::Refused { reason, .. })
            if reason.contains("only the workspace's owner makes this change, and from a device of their own")),
        "{handed:?}"
    );
    assert!(
        !workspaces
            .is_owner(&workspace.workspace, grace_person)
            .await
    );

    // Ada's key makes Grace an administrator by her key.
    let listed = add_member(&host, workspace, &ada_key, &grace, "administrator")
        .await
        .unwrap();
    let row = listed
        .as_array()
        .unwrap()
        .iter()
        .find(|member| member["member"]["person"] == hex::encode(grace_person))
        .cloned()
        .unwrap_or_else(|| panic!("Grace is listed: {listed}"));
    assert_eq!(
        (row["owner"].clone(), row["administrator"].clone()),
        (json!(false), json!(true))
    );

    // Her own key enters, before any device of hers joined, and makes Hal an
    // administrator.
    let grace_key = entered(&host, workspaces, workspace, &grace).await;
    let hal = SigningKey::from_bytes(&rand::random());
    add_member(&host, workspace, &grace_key, &hal, "administrator")
        .await
        .unwrap();
    let hal_person = hal.verifying_key().to_bytes();
    assert_eq!(
        access(workspaces, &workspace.workspace, hal_person).await,
        Some((Role::Editor, true))
    );
    assert!(!workspaces.is_owner(&workspace.workspace, hal_person).await);

    // Hal administers: he adds an editor, but makes no owner.
    let hal_key = entered(&host, workspaces, workspace, &hal).await;
    let ivy = SigningKey::from_bytes(&rand::random());
    add_member(&host, workspace, &hal_key, &ivy, "editor")
        .await
        .unwrap();
    let ivy_person = ivy.verifying_key().to_bytes();
    assert_eq!(
        access(workspaces, &workspace.workspace, ivy_person).await,
        Some((Role::Editor, false))
    );
    let refused = add_member(&host, workspace, &hal_key, &ivy, "owner").await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "{refused:?}"
    );
    assert!(!workspaces.is_owner(&workspace.workspace, ivy_person).await);

    // Ivy, an editor, adds nobody: the action is not hers.
    let ivy_key = entered(&host, workspaces, workspace, &ivy).await;
    let stranger = SigningKey::from_bytes(&rand::random());
    let by_editor = add_member(&host, workspace, &ivy_key, &stranger, "viewer").await;
    assert!(by_editor.is_err(), "{by_editor:?}");
    server.shutdown().await;
}

/// `person`'s server key, certified and entered into `workspace` as the person's own.
async fn entered(
    host: &ServiceWorkspaceActions,
    workspaces: &Workspaces,
    workspace: &tilo_server_protocol::workspaces::WorkspaceListing,
    person: &SigningKey,
) -> DeviceCert {
    let key_signer = SigningKey::from_bytes(&rand::random());
    let key = certified(person, &key_signer, DeviceCapabilities::server_key_grant());
    let entering = own_as_key(host, &key, json!({"workspace": workspace.workspace})).await;
    join(workspaces, &entering, (&key_signer, &key)).await;
    key
}

/// `cert` asks the host to add `person` to `workspace` as `role`, named "Someone".
async fn add_member(
    host: &ServiceWorkspaceActions,
    workspace: &tilo_server_protocol::workspaces::WorkspaceListing,
    cert: &DeviceCert,
    person: &SigningKey,
    role: &str,
) -> Result<Value, ActionCallError> {
    govern(
        host,
        workspace,
        cert,
        super::super::ADD_MEMBER,
        json!({
            "person": hex::encode(person.verifying_key().to_bytes()),
            "role": role,
            "name": "Someone",
        }),
    )
    .await
}

/// `device`'s person asks this host to list its workspaces, as the entry's
/// account path does.
async fn listed_for(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
) -> Vec<tilo_server_protocol::workspaces::WorkspaceListing> {
    let mut ask = own_request(json!({}));
    ask.target.address.action = tilo_server_protocol::workspaces::LIST_WORKSPACES.into();
    serde_json::from_value(invoke(host, device, ask).await.unwrap()).unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one workspace's people and its listing read top to bottom"
)]
async fn people_are_named_as_people_and_their_listing_names_what_they_were_invited_into() {
    use tilo_actions::remote::WORKSPACE_INVITE;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);

    // Ada's backend makes her workspace, enters it as her, and invites a
    // reviewer into it.
    let ada = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, ada.verifying_key().to_bytes()).await;
    let ada_person = ada.verifying_key().to_bytes();
    let backend_key = SigningKey::from_bytes(&rand::random());
    let backend = certified_as(
        &ada,
        &backend_key,
        DeviceCapabilities::server_key_grant(),
        "Ada's backend",
    );
    let created = own_as_key(&host, &backend, json!({"name": "Applicant: Jane Doe"})).await;
    join(workspaces, &created, (&backend_key, &backend)).await;
    let workspace = &created.workspace;
    let invited = govern(
        &host,
        workspace,
        &backend,
        WORKSPACE_INVITE,
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
        (&page_key, &page),
    )
    .await;

    // Ada follows another invitation on a device of her own, and her agent's
    // key enters the workspace as her too: a key of hers never names her.
    let theirs = govern(
        &host,
        workspace,
        &backend,
        WORKSPACE_INVITE,
        json!({"role": "editor"}),
    )
    .await
    .unwrap();
    let ada_key = SigningKey::from_bytes(&rand::random());
    let ada_page = certified_as(&ada, &ada_key, DeviceCapabilities::member(), "Ada");
    join_with(
        workspaces,
        hex::decode(theirs["invitation"].as_str().unwrap()).unwrap(),
        (&ada_key, &ada_page),
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
    // that minted it asks for, as the entry hands it over.
    let entering = own_as_key(&host, &backend, json!({"workspace": workspace.workspace})).await;
    join(workspaces, &entering, (&agent_key, &agent)).await;

    // The people in it: Ada under her own name, its owner and administrator,
    // Rosa under hers, and no row for the host's device that runs it or for
    // any key of theirs.
    let members = govern(&host, workspace, &backend, super::super::MEMBERS, json!({}))
        .await
        .unwrap();
    let rows = members.as_array().unwrap();
    let row = |person: &[u8; 32]| {
        rows.iter()
            .find(|row| row["member"]["person"] == hex::encode(*person))
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
            .any(|row| row["member"]["person"] == hex::encode(owner.user_public_key)),
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
    server.shutdown().await;
}

fn delete_request(workspace: &str) -> InvokeRequest {
    let mut request =
        super::super::tests::request("", DELETE_WORKSPACE, json!({ "workspace": workspace }));
    request.target = tilo_actions::remote::RemoteActionTarget {
        provider: HOST_ACTIONS_PROVIDER.into(),
        address: tilo_actions::ActionAddress::new(HOST_WORKSPACES_RESOURCE, DELETE_WORKSPACE),
    };
    request
}

#[tokio::test]
async fn only_its_owner_deletes_a_workspace_and_everything_it_kept_goes_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);
    // A person's server key makes a workspace for a customer, and keeps it: the key acts as its
    // person, so nobody is asked.
    let root = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, root.verifying_key().to_bytes()).await;
    let key = SigningKey::from_bytes(&rand::random());
    let server = certified(&root, &key, DeviceCapabilities::server_key_grant());
    let created = own_as_key(&host, &server, json!({"name": "Applicant: Jane Doe"})).await;
    let workspace = created.workspace.workspace.clone();
    let workspace_id = created.workspace.workspace_id;
    join(workspaces, &created, (&key, &server)).await;
    let stored = dir.path().join("workspaces");
    let record = stored.join(format!("{workspace}.workspace"));
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
        on_host(&host, &narrow, &as_key(&narrow), delete_request(&workspace)).await,
        Err(ActionCallError::Refused { .. })
    ));
    // Nor does anyone else, who learns only that it is not theirs.
    let (_, stranger) = guest_device();
    let refused = invoke(&host, &stranger, delete_request(&workspace)).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if AdminRefusal::from_reason(reason) == Some(AdminRefusal::NotYourOwn)),
        "{refused:?}"
    );
    assert!(record.exists(), "a refusal deletes nothing");

    // Its owner's key deletes it.
    let receipt = on_host(&host, &server, &as_key(&server), delete_request(&workspace))
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
        account.subscription.unwrap().usage.workspaces.is_empty(),
        "what it kept no longer counts against its person"
    );
    // Nothing of it stays on disk but the mark that it was deleted.
    assert!(!record.exists());
    assert!(stored.join(format!("{workspace}.deleted")).exists());
    let left: Vec<String> = std::fs::read_dir(&stored)
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
    let again = on_host(&host, &server, &as_key(&server), delete_request(&workspace)).await;
    assert!(
        matches!(&again, Err(ActionCallError::Refused { reason }) if AdminRefusal::from_reason(reason) == Some(AdminRefusal::NotYourOwn)),
        "{again:?}"
    );
    assert!(
        workspaces
            .create_workspace("Again", Some(&workspace))
            .await
            .is_err()
    );
}

/// A host keeping workspaces of people's own, with one listed workspace its
/// entry takes people into by invitation only and keeps a wait list for: the
/// setup the two checks below share. Returns the host's actions, its
/// workspaces, the listed workspace, a stranger who left an address there,
/// and the state directory they live in.
async fn held_host() -> (
    ServiceWorkspaceActions,
    Workspaces,
    String,
    (SigningKey, tilo_user_identity::DeviceCert),
    tempfile::TempDir,
) {
    use tilo_server_protocol::workspaces::Role;

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let host = super::super::tests::host_dispatch(&workspaces);
    // The site's home workspace: its entry, or an invitation into it, is the
    // one way to workspaces of one's own here.
    let lounge = home_of(&workspaces).await;
    workspaces
        .publish(
            &lounge,
            &owner,
            tilo_server_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: Role::Editor,
            },
        )
        .await
        .unwrap();
    workspaces
        .list_invitation_only(&lounge, &owner, true)
        .await
        .unwrap();
    assert!(
        workspaces.offer_wait_list(tilo_server_protocol::directory::EntryWaitList {
            contact: Some("hello@tilo.example".into()),
            mail: true,
        })
    );
    let (stranger_key, stranger) = guest_device();
    assert!(
        workspaces
            .wait(
                &lounge,
                "stranger@example.org",
                WaitListSecrets {
                    removal: [1; 32],
                    confirm: Some([2; 32]),
                },
                "en",
                1,
            )
            .await
            .is_ok()
    );
    (host, workspaces, lounge, (stranger_key, stranger), dir)
}

/// The owner of a workspace whose entry takes people by invitation is let in
/// through that entry: nobody here may invite them, so an entry that turned its
/// own owner away would leave the workspace with nobody in it to invite
/// anyone.
#[tokio::test]
async fn the_owners_own_device_is_let_in_past_an_invitation_only_entry() {
    use tilo_server_protocol::directory::EntryJoin;

    let (key, owner) = governor();
    let dir = tempfile::tempdir().unwrap();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces().clone();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let lounge = home_of(&workspaces).await;
    workspaces
        .publish(
            &lounge,
            &owner,
            tilo_server_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    workspaces
        .list_invitation_only(&lounge, &owner, true)
        .await
        .unwrap();
    workspaces
        .admit_listed(
            &lounge,
            &EntryJoin {
                user_public_key: hex::encode(owner.user_public_key),
                display_name: "The operator".into(),
            },
        )
        .await
        .expect("the owner is let in through their own workspace's closed entry");
}

/// The wait list holds the entry: the invitation-only entry lets nobody in on
/// the strength of an address, and says it keeps one.
#[tokio::test]
async fn the_wait_list_holds_the_entry_and_lets_nobody_in_on_an_address() {
    use tilo_server_protocol::directory::EntryJoin;

    let (_, workspaces, lounge, (_, stranger), _state) = held_host().await;
    let refused = workspaces
        .admit_listed(
            &lounge,
            &EntryJoin {
                user_public_key: hex::encode(stranger.user_public_key),
                display_name: "Stranger".into(),
            },
        )
        .await
        .expect_err("an invitation-only entry admits nobody who left an address");
    assert!(
        matches!(refused, crate::workspaces::EntryRefusal::InvitationOnly),
        "{refused:?}"
    );
    let said = refused.answer_with(workspaces.wait_list_page());
    assert!(
        said.wait_list && said.mail && said.contact.as_deref() == Some("hello@tilo.example"),
        "the refusal offers the wait list: {said:?}"
    );
    assert!(
        workspaces
            .own()
            .expect("this host keeps workspaces of people's own")
            .of(&stranger.user_public_key)
            .is_empty(),
        "a stranger at the wait list keeps nothing here"
    );
}

/// The wait list's refusal `result` carries: the entry's own, offering the
/// wait list it keeps, as the host refuses a person it never let in.
fn waits(result: &Result<impl std::fmt::Debug, ActionCallError>) {
    use tilo_server_protocol::directory::{EntryRefusalCode, EntryRefused};
    let Err(ActionCallError::Refused { reason }) = result else {
        panic!("a person never let in is refused: {result:?}");
    };
    let entry = EntryRefused::from_reason(reason)
        .unwrap_or_else(|| panic!("the refusal is the entry's: {reason}"));
    assert_eq!(entry.code, EntryRefusalCode::InvitationOnly, "{entry:?}");
    assert!(
        entry.wait_list && entry.mail && entry.contact.as_deref() == Some("hello@tilo.example"),
        "the refusal offers the wait list: {entry:?}"
    );
    assert!(entry.error.contains("wait list"), "{}", entry.error);
}

/// Anyone can make a root: a certificate it signed is no way past the entry.
/// A fresh root, a key of it and an agent's grant of it are each refused a
/// workspace of their own with the entry's refusal and its wait list, and the
/// host keeps nothing for them: no workspace, no workspace list.
#[tokio::test]
async fn a_fresh_root_and_its_keys_are_refused_with_the_wait_list() {
    let (host, workspaces, _, (_, stranger), _state) = held_host().await;
    // Its person allows what their agents ask of them.
    Answering::approving();
    waits(&create_own(&host, &stranger, json!({})).await);
    waits(&create_own(&host, &stranger, json!({"name": "Mine"})).await);
    let root = SigningKey::from_bytes(&rand::random());
    let grant = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    waits(
        &on_host(
            &host,
            &grant,
            &as_key(&grant),
            own_request(json!({"name": "Agent's"})),
        )
        .await,
    );
    let server = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::server_key_grant(),
    );
    waits(&on_host(&host, &server, &as_key(&server), own_request(json!({}))).await);
    for person in [stranger.user_public_key, root.verifying_key().to_bytes()] {
        assert!(
            workspaces.own_workspaces_of(&person).is_empty(),
            "nothing is kept for a person never let in"
        );
    }
}

/// A person the site let in (the home workspace's invitation redeemed, as
/// its entry hands one) makes a second and a third workspace of their own:
/// the wait list is at the entry, never in the way of someone already in.
/// Their agent's grant makes one too, within its scope; a key for one
/// workspace makes none but enters its own. The stranger who only left an
/// address keeps nothing.
#[tokio::test]
async fn a_person_let_in_makes_as_many_workspaces_as_they_like() {
    let (host, workspaces, lounge, (_, stranger), _state) = held_host().await;
    // Its person allows what their agents ask of them.
    Answering::approving();
    let root = SigningKey::from_bytes(&rand::random());
    let person = root.verifying_key().to_bytes();
    let page_signer = SigningKey::from_bytes(&rand::random());
    let page = certified(&root, &page_signer, DeviceCapabilities::member());
    let invitation = let_in(&workspaces, &lounge, person).await;
    join_with(&workspaces, invitation, (&page_signer, &page)).await;
    let first = create_own(&host, &page, json!({"name": "Work"}))
        .await
        .unwrap();
    join(&workspaces, &first, (&page_signer, &page)).await;
    for name in ["Side project", "Third"] {
        create_own(&host, &page, json!({"name": name}))
            .await
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
    }
    assert_eq!(workspaces.own_workspaces_of(&person).len(), 3);

    // Their agent's grant, reaching all their workspaces, makes another.
    let grant = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
    );
    let agents = own_as_key(&host, &grant, json!({"name": "Agent's"})).await;
    assert_eq!(agents.workspace.name, "Agent's");
    let mine = workspaces.own_workspaces_of(&person);
    assert_eq!(mine.len(), 4, "the grant's is the person's own: {mine:?}");

    // A key for one of them makes none, and enters that one.
    let work: [u8; 16] = hex::decode(first.workspace.workspace.replace('-', ""))
        .unwrap()
        .try_into()
        .unwrap();
    let narrow = certified(
        &root,
        &SigningKey::from_bytes(&rand::random()),
        DeviceCapabilities::agent_grant(AgentScope::Workspace(work)),
    );
    // Its person allows what their agents ask of them, so the call is admitted again on their
    // answer, and the host's own check refuses it then: the caller reads that it ended refused.
    let refused = on_host(
        &host,
        &narrow,
        &as_key(&narrow),
        own_request(json!({"name": "Another"})),
    )
    .await
    .expect("the person's answer admits it again");
    let ReceiptStatus::Failed { error } = &refused.status else {
        panic!("the host refuses it: {refused:?}");
    };
    let reason = error
        .message
        .strip_prefix("refused: ")
        .unwrap_or(&error.message);
    assert_eq!(
        AdminRefusal::from_reason(reason),
        Some(AdminRefusal::NotYourOwn),
        "{error:?}"
    );
    let entered = own_as_key(
        &host,
        &narrow,
        json!({"workspace": first.workspace.workspace.clone()}),
    )
    .await;
    assert_eq!(entered.workspace.workspace, first.workspace.workspace);
    assert_eq!(workspaces.own_workspaces_of(&person).len(), 4);

    assert!(
        workspaces
            .own_workspaces_of(&stranger.user_public_key)
            .is_empty(),
        "the stranger who only left an address keeps nothing here"
    );
}

/// Someone a person let in invited into their workspace enters it with any
/// key of theirs, and the invitation lets them past the site's invite-only
/// sign-up: whoever invited them let them in, so they make workspaces of
/// their own as well.
#[tokio::test]
async fn an_invited_member_enters_what_they_were_invited_into() {
    let (host, workspaces, lounge, (stranger_key, stranger), _state) = held_host().await;
    let (ada_signer, ada) = guest_device();
    let invitation = let_in(&workspaces, &lounge, ada.user_public_key).await;
    join_with(&workspaces, invitation, (&ada_signer, &ada)).await;
    let created = create_own(&host, &ada, json!({"name": "Shared"}))
        .await
        .unwrap();
    join(&workspaces, &created, (&ada_signer, &ada)).await;
    let invitation = workspaces
        .issue(
            &created.workspace.workspace,
            &ada,
            IssueInvitationRequest {
                request_id: rand::random(),
                role: Role::Editor,
                recipient_user_public_key: Some(stranger.user_public_key),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap()
        .signed_invitation;
    join_with(&workspaces, invitation, (&stranger_key, &stranger)).await;
    let entered = create_own(
        &host,
        &stranger,
        json!({"workspace": created.workspace.workspace.clone()}),
    )
    .await
    .unwrap();
    assert_eq!(entered.workspace.workspace, created.workspace.workspace);
    // Invited into a workspace here, they are past the site's invite-only
    // sign-up: a workspace of their own is theirs to make too.
    let mine = create_own(&host, &stranger, json!({"name": "Mine"}))
        .await
        .expect("someone invited anywhere here makes workspaces of their own");
    assert_ne!(mine.workspace.workspace, created.workspace.workspace);
}

/// A device of the person whose root is `root`, certified now for an hour, as
/// a page's root certifies a tab.
fn tab_of(root: &SigningKey, label: &str) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&rand::random());
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(1_000_000_000),
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// How `workspace` on `workspaces`' server answers `device` subscribing, as a
/// page's join does: admitted, or refused and why.
async fn subscribe(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
) -> Result<(), crate::workspace_access::DenyReason> {
    let id = tilo_server_protocol::WorkspaceId::parse(workspace).unwrap();
    let runtime = workspaces
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&id)
        .cloned()
        .unwrap();
    let revocations = std::sync::RwLock::new(RevocationList::new());
    let policy = std::sync::RwLock::new(crate::policy::TrustPolicy::AllowAny);
    let agents = crate::registry::AgentRegistry::default();
    let environments = crate::environment::EnvironmentRegistry::default();
    let blocked = std::sync::RwLock::new(crate::workspaces::blocked::BlockedKeys::default());
    let own_user = workspaces.device_cert().user_public_key;
    let trust = crate::workspace_access::ServerTrust {
        revocations: &revocations,
        policy: &policy,
        blocked: &blocked,
        own_user: &own_user,
        agents: &agents,
        environments: &environments,
    };
    let peer = crate::auth::AuthenticatedPeer {
        cert: device.clone(),
        user_public_key: device.user_public_key,
        device_public_key: device.device_public_key,
    };
    crate::workspace_io::admit_for(
        &runtime,
        &trust,
        &peer,
        crate::workspace_access::WorkspaceOperation::Subscribe,
    )
    .await
}

/// A person's key makes a workspace of their own, and a device of theirs joins it before any
/// of theirs has entered it: the workspace is theirs from the moment it is made, so their
/// first device comes in as its owner's, which the stranger's device below is still not.
#[tokio::test]
async fn a_persons_first_device_joins_their_own_workspace_before_any_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = super::super::tests::host_dispatch(workspaces);
    let home = open(workspaces).await;
    let root = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, root.verifying_key().to_bytes()).await;

    // The person's key makes the workspace: no device of theirs has entered it.
    let (_, first) = tab_of(&root, "Firefox on Linux");
    let created = create_own(&host, &first, json!({})).await.unwrap();
    let workspace = created.workspace.workspace.clone();
    let (computer_key, computer) = tab_of(&root, "Ada's laptop");
    assert_eq!(
        subscribe(workspaces, &workspace, &computer).await,
        Err(crate::workspace_access::DenyReason::Unlisted),
        "the member list does not list the device before it lists itself"
    );
    let linked = list_itself_on_link(&server, &workspace, (&computer_key, &computer)).await;
    assert_eq!(
        subscribe(workspaces, &workspace, &computer).await,
        Ok(()),
        "a device of the person joins a workspace of their own before any of theirs entered"
    );
    linked.abort();
    assert_eq!(
        access(workspaces, &workspace, root.verifying_key().to_bytes()).await,
        Some((Role::Owner, true)),
        "the person owns the workspace made for them"
    );
    let (_, stranger) = guest_device();
    assert_eq!(
        subscribe(workspaces, &workspace, &stranger).await,
        Err(crate::workspace_access::DenyReason::NotAMember),
        "a stranger's device is still no member"
    );
    server.shutdown().await;
}

/// A person's browser keeps one device key per tab: the tab that set them up
/// redeemed the invitation into their own workspace, and a tab that comes
/// later on another key (a second tab, the same tab after its key slot was
/// taken) is a further device of theirs. It joins their workspace as theirs,
/// the way the entry lets a member's further device in, and the first tab
/// stays in; a stranger's device is still no member.
#[tokio::test]
async fn a_further_tab_of_the_person_joins_their_own_workspace_and_the_first_stays_in() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = super::super::tests::host_dispatch(workspaces);
    let home = open(workspaces).await;
    let root = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, root.verifying_key().to_bytes()).await;

    let (first_key, first) = tab_of(&root, "Firefox on Linux");
    let created = create_own(&host, &first, json!({})).await.unwrap();
    join(workspaces, &created, (&first_key, &first)).await;
    let workspace = created.workspace.workspace.clone();
    assert_eq!(subscribe(workspaces, &workspace, &first).await, Ok(()));

    let (later_key, later) = tab_of(&root, "Firefox on Linux");
    let linked = list_itself_on_link(&server, &workspace, (&later_key, &later)).await;
    linked.abort();
    assert_eq!(
        subscribe(workspaces, &workspace, &later).await,
        Ok(()),
        "a further tab of the person joins their own workspace"
    );
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert_eq!(
        members
            .iter()
            .filter(|member| member.root() == Some(later.user_public_key))
            .count(),
        1,
        "one person, on both tabs"
    );
    assert_eq!(
        subscribe(workspaces, &workspace, &first).await,
        Ok(()),
        "the first tab stays in"
    );

    let (_, stranger) = guest_device();
    assert_eq!(
        subscribe(workspaces, &workspace, &stranger).await,
        Err(crate::workspace_access::DenyReason::NotAMember),
        "a stranger's device is no member"
    );
    server.shutdown().await;
}

/// A browser asks for the person's own workspace by no id: the host makes it
/// once, under an id of its own, and answers that one to every later ask, from
/// a browser that forgot the site as from the person's agent. An id the host
/// never made is refused, never read as the workspace the person has: with a
/// workspace per customer, a wrong id must fail, not land in another one.
#[tokio::test]
async fn own_workspace_is_asked_for_by_no_id_and_an_unknown_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let host = super::super::tests::host_dispatch(workspaces);
    let home = open(workspaces).await;
    let root = SigningKey::from_bytes(&rand::random());
    let person = root.verifying_key().to_bytes();
    let_in(workspaces, &home, person).await;

    let (first_key, first) = tab_of(&root, "Firefox on Linux");
    let made = create_own(&host, &first, json!({})).await.unwrap();
    join(workspaces, &made, (&first_key, &first)).await;
    let own = made.workspace.workspace;

    let (_, later) = tab_of(&root, "Firefox on Linux");
    let entered = create_own(&host, &later, json!({})).await.unwrap();
    assert_eq!(entered.workspace.workspace, own, "the workspace they have");

    let unknown = uuid::Uuid::new_v4().to_string();
    let refused = create_own(&host, &later, json!({"workspace": unknown})).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if AdminRefusal::from_reason(reason) == Some(AdminRefusal::NotYourOwn)),
        "{refused:?}"
    );
    assert_eq!(
        workspaces.own_workspaces_of(&person),
        std::slice::from_ref(&own),
        "and no second one"
    );

    // A person with no workspace yet is refused an id the host never made too:
    // the host makes theirs under an id of its own.
    let newcomer = SigningKey::from_bytes(&rand::random());
    let_in(workspaces, &home, newcomer.verifying_key().to_bytes()).await;
    let refused = create_own(
        &host,
        &tab_of(&newcomer, "Chrome").1,
        json!({"workspace": uuid::Uuid::new_v4().to_string()}),
    )
    .await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if AdminRefusal::from_reason(reason) == Some(AdminRefusal::NotYourOwn)),
        "{refused:?}"
    );
    assert!(
        workspaces
            .own_workspaces_of(&newcomer.verifying_key().to_bytes())
            .is_empty()
    );

    // Another person's workspace is not theirs either.
    let theirs = Box::pin(create_own(&host, &tab_of(&newcomer, "Chrome").1, json!({})))
        .await
        .unwrap()
        .workspace
        .workspace;
    let refused = create_own(&host, &later, json!({"workspace": theirs})).await;
    assert!(
        matches!(&refused, Err(ActionCallError::Refused { reason }) if AdminRefusal::from_reason(reason) == Some(AdminRefusal::NotYourOwn)),
        "{refused:?}"
    );
    server.shutdown().await;
}

/// Someone the operator declares an owner of the home workspace holds it by
/// their root from that moment: a server key of theirs lists the workspace,
/// enters it, and invites the next person in, with no device of theirs ever
/// having joined it.
#[tokio::test]
async fn a_declared_owners_key_lists_the_workspace_and_invites_before_any_device_joined() {
    use tilo_actions::remote::WORKSPACE_INVITE;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let home = open(workspaces).await;
    let host = super::super::tests::host_dispatch(workspaces);

    let ada = SigningKey::from_bytes(&rand::random());
    let ada_person = ada.verifying_key().to_bytes();
    workspaces
        .set_member_administrator(
            &home,
            workspaces.device_cert(),
            rand::random(),
            ada_person,
            None,
        )
        .await
        .unwrap();

    let backend_key = SigningKey::from_bytes(&rand::random());
    let backend = certified(&ada, &backend_key, DeviceCapabilities::server_key_grant());
    let listed = listed_for(&host, &backend).await;
    let lounge = listed
        .iter()
        .find(|listed| listed.workspace == home)
        .unwrap_or_else(|| panic!("the declared owner's key lists the home: {listed:?}"))
        .clone();

    let entering = own_as_key(&host, &backend, json!({"workspace": home})).await;
    assert_eq!(entering.workspace.workspace, home);
    join(workspaces, &entering, (&backend_key, &backend)).await;
    let issued = govern(
        &host,
        &lounge,
        &backend,
        WORKSPACE_INVITE,
        json!({"role": "editor"}),
    )
    .await
    .unwrap();
    assert!(issued["invitation"].is_string(), "{issued}");
    server.shutdown().await;
}
