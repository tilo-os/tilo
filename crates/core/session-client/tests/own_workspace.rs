//! A person's own workspace, as a device in the site's home workspace has it: the host makes it the
//! first time and answers the same one after, the very device that asked redeems its invitation
//! (so the key it holds is the one that opens it), and every refusal reads typed, as the host said
//! it. The page asks through this same entry (`enterOwnWorkspace`), as the CLI and the API server
//! do.
//!
//! What is the person's own is their device's: their Servers workspace is founded on it, never by
//! the site, and their list of workspaces is a record of that workspace, which their device keeps
//! and writes. The site holds no copy of it, so nothing of theirs is written there.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the test is one entry, in order; the host lives until it ends; the person's own workspace, their computer and their list are what it is about"
)]

use crate::support::new_key;
use std::cell::Cell;
use std::sync::Arc;

use serde_json::{Map, json};
use tilo_server_client::invite::guest_certificate;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_session_client::device::{Awaiting, Device, DeviceRefusal};
use tilo_session_client::{Outbox as _, PeerConfig};
use tilo_user_identity::{AgentScope, DeviceCapabilities, UserIdentity};
use tilo_workspace_service::workspaces::OwnWorkspaceOffer;

use crate::support::{
    enrol, enter as enter_as, enter_page, entry_grant, in_workspace, start_workspace_host,
};

/// The names the person's list of workspaces gives them, as read on the computer that holds
/// their own workspace: what a person sees their workspaces in.
async fn listed_names(workspaces: &tilo_workspace_service::workspaces::Workspaces) -> Vec<String> {
    tilo_workspace_service::workspaces::person_workspace::read::<tilo_user_identity::WorkspaceList>(
        workspaces,
        tilo_server_protocol::workspaces::IDENTITY_LIST,
    )
    .await
    .expect("the person's own computer reads their list")
    .listed()
    .map(|(_, listed)| listed.name.clone())
    .collect()
}

/// Have `device` take in `workspace`, which `site` keeps, as a device of the person's does for
/// each workspace it takes part in: it proves its membership at the address the site signs for
/// it, and holds the copy from then on. Taking one in lists it in the person's own list, written
/// by the device that hosts that workspace.
async fn take_in(
    device: &tilo_workspace_service::workspaces::Workspaces,
    site: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
) {
    let kept = site
        .kept_at(workspace)
        .await
        .expect("the site names where it keeps the workspace");
    let hint = kept.hint.expect("the site signs its address for it");
    let id = tilo_server_protocol::WorkspaceId::parse(workspace).expect("a workspace's id");
    assert_eq!(
        device
            .hold_as_member(id, &hint)
            .await
            .expect("the person's device takes the workspace in"),
        id
    );
}

/// How long a key follows an operation its person was asked about, once they answered.
const FOLLOWING: std::time::Duration = std::time::Duration::from_secs(30);

/// The person answers the question `awaiting` waits in with `resolution`, from `device`, a device
/// of theirs in the workspace that keeps the question.
async fn answer(device: &Device, awaiting: &Awaiting, resolution: &str) {
    let workspace = tilo_server_protocol::WorkspaceId::parse(&awaiting.ask.workspace)
        .expect("the question is kept in a workspace");
    assert!(
        device.holds(workspace),
        "the person's device is in the workspace that keeps the question: {awaiting:?}"
    );
    device
        .ask_host(
            workspace,
            tilo_actions::remote::RemoteActionTarget {
                provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
                address: tilo_actions::ActionAddress::new(
                    &awaiting.ask.record,
                    tilo_actions::remote::asked::RESOLVE,
                ),
            },
            Map::from_iter([("resolution".into(), json!(resolution))]),
        )
        .await
        .expect("the person's answer is taken");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_in_the_lounge_enters_its_persons_own_workspace_with_its_own_key() {
    // Alice's host keeps the Lounge; its entry lets Bob's tab in.
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();

    let bob = new_key();
    let bob_person = bob.verifying_key().to_bytes();
    // Bob's own computer holds his root and his Servers workspace: founding is signed on the
    // founder's own device, never by the site, and his list of workspaces is a record of that
    // workspace, which his device writes. The site holds no copy of it.
    let bob_computer = enrol(&bob, "bob-computer", DeviceCapabilities::primary());
    let bob_state = tempfile::tempdir().unwrap();
    let bob_host = start_workspace_host(&bob, &bob_computer, bob_state.path(), false).await;
    let bob_workspaces = bob_host.server.workspaces().unwrap();
    assert_eq!(
        tilo_workspace_service::workspaces::person_workspace::found(bob_workspaces)
            .await
            .unwrap(),
        person_workspace_id(&bob_person)
    );
    let tab_key = new_key();
    let tab_cert = guest_certificate(
        &bob,
        tab_key.verifying_key().to_bytes(),
        "Firefox on Linux",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let tab = enter_as(
        &host,
        entry_grant(
            &host,
            &alice,
            &host_device,
            &lounge.workspace,
            bob.verifying_key().to_bytes(),
        )
        .await,
        &tab_cert,
        &tab_key,
        &host_device.cert,
        lounge.workspace_id,
    )
    .await;
    // The tab's own session, as the page starts it, is the device that enters.
    let device = Device::of(
        tab.clone(),
        tab_cert.clone(),
        host_device.cert.clone(),
        "Firefox on Linux",
    );

    // A host that keeps no workspaces of people's own says so in a code the page reads, and names
    // the reason in the refusal its page shows: a host operation it does not let this device
    // carry out is refused with why, not answered as an action the host never had.
    let none = device
        .enter_own_workspace("en-US", None, "Bob", |_| false, None)
        .await
        .unwrap_err();
    assert_eq!(none.code, DeviceRefusal::HOSTS_NONE_FOR_PEOPLE, "{none:?}");
    assert_eq!(
        none.message, "this host keeps no workspaces of people's own",
        "{none:?}"
    );
    assert_eq!(none.refusal, None);

    assert!(
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap()
    );
    // For the people the Lounge's entry let in, as Bob's tab was.
    assert!(workspaces.admit_own_through(&lounge.workspace));

    // An id the host never made is refused, not taken as the workspace to make: the host chooses
    // the id of a person's own workspace.
    let unknown = device
        .own_workspace(
            Map::from_iter([(
                "workspace".into(),
                json!("5c1f2e7a-8b3d-4e6f-9a0b-1c2d3e4f5a6b"),
            )]),
            tilo_actions::InvocationId::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(unknown.code, "refused", "{unknown:?}");
    assert_eq!(
        workspaces.own_workspace_of(&bob.verifying_key().to_bytes()),
        None
    );

    // A plain ask for his own workspace, which only the device that makes his root founds, is
    // answered here, where it is not kept, in words that say where it lives rather than that
    // something failed: nothing is made for it.
    let plain = device
        .enter_own_workspace("en-US", None, "Bob", |_| false, None)
        .await
        .unwrap_err();
    assert_eq!(plain.code, "refused", "{plain:?}");
    assert_eq!(
        plain.message,
        "This site doesn't keep your own workspace: it lives on your devices. Name a new \
         workspace to make one here, or open yours from a device of yours that has it.",
        "the page reads where the workspace is, not that something failed: {plain:?}"
    );
    assert_eq!(
        workspaces.own_workspace_of(&bob.verifying_key().to_bytes()),
        None
    );

    // One he names the host makes, and this device enters it: the workspace admits this very
    // key, and it is Bob's, not the Lounge.
    let asked = Cell::new(0);
    let own = device
        .enter_own_workspace(
            "en-US",
            Some("Bob's workspace"),
            "Bob",
            |_| {
                asked.set(asked.get() + 1);
                false
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(asked.get(), 1);
    assert_ne!(own.workspace, lounge.workspace);
    assert_ne!(
        own.name, "Lounge",
        "the person's own workspace is not titled after the site's"
    );
    assert_eq!(
        workspaces.own_workspace_of(&bob.verifying_key().to_bytes()),
        Some(own.workspace.clone())
    );
    assert_eq!(device.join(&own.workspace).await.unwrap(), own.workspace_id);
    // The tab came in by a row it signed itself as one of Bob's devices, with no invitation: its
    // membership is the owner's, never the Editor's an invitation would have given it.
    let members = workspaces.summary(&own.workspace).await.unwrap().members;
    let bobs = members
        .iter()
        .find(|member| member.root() == Some(bob_person) && !member.removed)
        .expect("Bob is in his own workspace");
    assert_eq!(
        bobs.role,
        Some(tilo_peer_state::join::Role::Owner),
        "{members:?}"
    );

    // The site holds no copy of the person's own workspace, so it writes nothing into it: what
    // is his is his own device's to write.
    assert!(
        !workspaces
            .list_for(&bob_person, &own.workspace, &own.name)
            .await,
        "the site lists nothing in a workspace of someone whose root is elsewhere"
    );
    // Bob's computer takes in the Lounge his tab entered, which is Alice's and only let him in,
    // and the workspace the site just made him, which is his own. Taking one in lists it in the
    // record of his own workspace only when it is his: the one the site made for him, written
    // by the device that hosts that record.
    take_in(bob_workspaces, workspaces, &lounge.workspace).await;
    take_in(bob_workspaces, workspaces, &own.workspace).await;
    let names = listed_names(bob_workspaces).await;
    assert!(
        names.contains(&own.name),
        "the workspace the site made is in the person's list: {names:?}"
    );
    // A workspace someone else's site let him into is not his, and their list never names it:
    // what is added to it there would be a list of Alice's workspaces.
    assert!(
        !names.contains(&lounge.name),
        "a workspace he was let into at someone's site is not on his list: {names:?}"
    );
    // An invitation his own computer redeems itself is his join, the one he asked for: the
    // workspace joins him as an editor and lands on his list, as the workspace of anyone who
    // takes up an invitation they were sent.
    let shared = workspaces.create("Shared").await.unwrap();
    let invitation = workspaces
        .issue(
            &shared.workspace,
            &host_device.cert,
            tilo_workspace_service::workspaces::IssueInvitationRequest {
                request_id: [7; 32],
                role: tilo_server_protocol::workspaces::Role::Editor,
                recipient_user_public_key: Some(bob_person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(600_000_000_000),
                inviter_name: Some("Alice".into()),
            },
        )
        .await
        .expect("the site invites the person into its workspace");
    let link = tilo_peer_state::join::encode_invitation_text(&invitation.signed_invitation);
    assert_eq!(
        bob_workspaces
            .join_member_as(&link, Some("Bob"))
            .await
            .expect("the person's own computer redeems the invitation"),
        tilo_server_protocol::WorkspaceId::parse(&shared.workspace).unwrap()
    );
    let names = listed_names(bob_workspaces).await;
    assert!(
        names.contains(&shared.name),
        "a workspace the person redeemed an invitation to is on their list: {names:?}"
    );

    // A page opening that workspace through its own entry, as the workspace switcher does for one
    // this browser never entered: a device of Bob's the host has not seen enters as his, with no
    // invitation.
    let switcher_key = new_key();
    let (switcher_cert, switcher_entry) = tilo_session_client::device::entry_request(
        &host_device.cert,
        &own.workspace,
        &switcher_key,
        tilo_session_client::device::Entering {
            held: None,
            root: Some(&bob),
            label: "Switcher",
            name: "Bob",
            token: None,
        },
    )
    .await
    .unwrap();
    let switcher = enter_page(
        host.iroh(),
        switcher_cert,
        host_device.cert.clone(),
        switcher_key,
        Arc::new(tilo_session_client::MemoryPeerStore::default()),
        switcher_entry,
    );
    switcher
        .send(tilo_session_client::Cmd::Join(own.workspace_id))
        .unwrap();
    assert_eq!(
        tilo_session_client::device::entered(&switcher)
            .await
            .unwrap()
            .workspace,
        own.workspace
    );
    in_workspace(&switcher, own.workspace_id).await;
    switcher.close();

    // Asked again by its id, it answers the same workspace.
    let (again, _) = device
        .own_workspace(
            Map::from_iter([("workspace".into(), json!(own.workspace))]),
            tilo_actions::InvocationId::new(),
        )
        .await
        .unwrap();
    assert_eq!(again.workspace, own.workspace);
    assert_eq!(again.name, own.name);

    // A make asked again under the invocation its first ask used — a page's dialog
    // asked once more because the first answer never came — answers the workspace
    // the first made, never a second one beside it.
    let once = tilo_actions::InvocationId::new();
    let asked_twice = Map::from_iter([("name".into(), json!("Twice"))]);
    let first = device
        .own_workspace(asked_twice.clone(), once)
        .await
        .unwrap();
    let second = device.own_workspace(asked_twice, once).await.unwrap();
    assert_eq!(first.0.workspace, second.0.workspace);
    assert_eq!(first.0.name, "Twice");
    // His computer takes it in, and the second ask made no second workspace: one entry in the
    // list has that name.
    take_in(bob_workspaces, workspaces, &first.0.workspace).await;
    let twice = listed_names(bob_workspaces)
        .await
        .iter()
        .filter(|name| name.as_str() == "Twice")
        .count();
    assert_eq!(twice, 1, "the second ask made no second workspace");

    // Another device of Bob's that holds the Lounge enters the same workspace of his under its
    // own key, never under the tab's.
    let laptop_key = new_key();
    let laptop_cert = guest_certificate(
        &bob,
        laptop_key.verifying_key().to_bytes(),
        "Laptop",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let laptop_peer = enter_as(
        &host,
        entry_grant(
            &host,
            &alice,
            &host_device,
            &lounge.workspace,
            bob.verifying_key().to_bytes(),
        )
        .await,
        &laptop_cert,
        &laptop_key,
        &host_device.cert,
        lounge.workspace_id,
    )
    .await;
    let laptop = Device::of(
        laptop_peer.clone(),
        laptop_cert,
        host_device.cert.clone(),
        "Laptop",
    );
    let (theirs, _) = laptop
        .own_workspace(
            Map::from_iter([("workspace".into(), json!(own.workspace))]),
            tilo_actions::InvocationId::new(),
        )
        .await
        .unwrap();
    assert_eq!(theirs.workspace, own.workspace);
    assert_eq!(laptop.join(&own.workspace).await.unwrap(), own.workspace_id);

    // A workspace Bob neither owns nor was invited into is refused, typed as the host refused it.
    let elsewhere = workspaces.create("Not Bob's").await.unwrap();
    let mut not_his = Map::new();
    not_his.insert("workspace".into(), json!(elsewhere.workspace));
    let refused = device
        .own_workspace(not_his, tilo_actions::InvocationId::new())
        .await
        .unwrap_err();
    assert_eq!(refused.code, "refused", "{refused:?}");
    assert!(!refused.message.is_empty());
    assert_ne!(refused.code, DeviceRefusal::UNANSWERED);

    // A key Bob made for that one workspace enters it straight from its session with the host, in
    // no other workspace first: the host invites it in on the session and it redeems there.
    let script_key = new_key();
    let script_cert = tilo_server_client::certification::issue_agent_grant(
        &bob,
        script_key.verifying_key().to_bytes(),
        "A script",
        DeviceCapabilities::agent_grant(AgentScope::parse(&own.workspace).unwrap()),
        Some(tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000)),
    )
    .await
    .unwrap();
    let script = Device::over(
        PeerConfig {
            host: host.iroh(),
            cert: script_cert,
            server_cert: host_device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        Arc::new(script_key),
        "A script",
    );
    script.connected().await.unwrap();
    assert_eq!(
        script
            .enter_workspace(&own.workspace, "A script")
            .await
            .unwrap(),
        own.workspace_id
    );
    // It makes no workspace, and enters no other. Its grant names one workspace: another is its
    // person's to allow, and is made on their own device, which this site is not. The site keeps
    // the make in the workspace the key reaches, for a device of Bob's that keeps his identity
    // ([`an_agent_held_to_one_workspace_makes_another_only_at_its_persons_root`]), and the key
    // reads that it waits, and in which record.
    let made = script
        .own_workspace(
            Map::from_iter([("name".into(), json!("More"))]),
            tilo_actions::InvocationId::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(made.code, DeviceRefusal::AWAITING_APPROVAL, "{made:?}");
    let more = made.awaiting.expect("the make that waits is named");
    assert_eq!(more.ask.workspace, own.workspace, "{more:?}");
    assert!(
        script
            .enter_workspace(&elsewhere.workspace, "A script")
            .await
            .is_err()
    );
    script.close();

    // A key Bob made for all his workspaces deletes one of them only once he says yes: its
    // delete waits for him, and his yes answers it with the deletion.
    let scratch = device
        .enter_own_workspace("en-US", Some("Scratch"), "Bob", |_| false, None)
        .await
        .unwrap();
    assert_eq!(
        device.join(&scratch.workspace).await.unwrap(),
        scratch.workspace_id
    );
    let helper_key = new_key();
    let helper_cert = tilo_server_client::certification::issue_agent_grant(
        &bob,
        helper_key.verifying_key().to_bytes(),
        "A helper",
        DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        Some(tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000)),
    )
    .await
    .unwrap();
    let helper = Device::over(
        PeerConfig {
            host: host.iroh(),
            cert: helper_cert,
            server_cert: host_device.cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        Arc::new(helper_key),
        "A helper",
    );
    helper.connected().await.unwrap();
    let deleting = helper
        .host_action(
            tilo_server_protocol::workspaces::DELETE_WORKSPACE,
            Map::from_iter([("workspace".into(), json!(scratch.workspace))]),
        )
        .await
        .expect_err("deleting waits for Bob");
    assert_eq!(
        deleting.code,
        DeviceRefusal::AWAITING_APPROVAL,
        "{deleting:?}"
    );
    let deleting = deleting.awaiting.expect("the delete that waits is named");
    assert!(
        workspaces
            .own_workspaces_of(&bob_person)
            .contains(&scratch.workspace),
        "nothing is deleted before his yes"
    );
    answer(&device, &deleting, "approve").await;
    helper
        .answer_of(&deleting, FOLLOWING)
        .await
        .expect("his yes deletes it, and the key reads the deletion");
    assert!(
        !workspaces
            .own_workspaces_of(&bob_person)
            .contains(&scratch.workspace),
        "his yes deleted the workspace"
    );
    helper.close();

    laptop_peer.close();
    tab.close();
    drop(host.web);
    host.server.shutdown().await;
    drop(bob_host.web);
    bob_host.server.shutdown().await;
}

/// The make `script` asks for a workspace called `name`, which waits for its person.
async fn asks_to_make(script: &Device, name: &str) -> Awaiting {
    let made = script
        .own_workspace(
            Map::from_iter([("name".into(), json!(name))]),
            tilo_actions::InvocationId::new(),
        )
        .await
        .expect_err("the make waits for its person");
    assert_eq!(made.code, DeviceRefusal::AWAITING_APPROVAL, "{made:?}");
    made.awaiting.expect("the make that waits is named")
}

/// The person's answer `resolve` to the make `awaiting`, given as `person` on `device` against
/// `workspaces`' store: once `wait` says it has the question, its result.
async fn answer_with(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    (person, device): ([u8; 32], [u8; 32]),
    awaiting: &Awaiting,
    resolve: tilo_actions::remote::asked::Resolve,
) -> Result<(), tilo_actions::remote::ActionCallError> {
    let store = workspaces
        .asked_store()
        .expect("the host keeps asked records");
    store
        .resolve(
            &awaiting.ask,
            &tilo_actions::remote::Caller::authenticated(person, device),
            &resolve,
        )
        .await
        .expect("the answer is decided at once")
}

/// Wait until `workspaces`, a device that keeps the person's root, took in the make `awaiting`.
async fn taken_in(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    awaiting: &Awaiting,
) {
    let store = workspaces
        .asked_store()
        .expect("the host keeps asked records");
    let deadline = tokio::time::Instant::now() + FOLLOWING;
    while store.kept_ask(&awaiting.ask).is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the device that keeps the root takes the make in: {awaiting:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A key `cert` certifies, on `host`'s computer whose certificate is `server`.
fn on_host(
    host: &crate::support::TestHost,
    server: &tilo_user_identity::DeviceCert,
    (cert, key): (tilo_user_identity::DeviceCert, &ed25519_dalek::SigningKey),
) -> Device {
    Device::over(
        PeerConfig {
            host: host.iroh(),
            cert,
            server_cert: server.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        Arc::new(key.clone()),
        "A script",
    )
}

/// A key Bob made for one workspace asks the site for another. The site makes none: it keeps the
/// make for a device of Bob's that keeps his identity, where he answers it once. His no makes
/// nothing; his yes founds it there, as him and owned by him, and the key gets no reach in it
/// unless he let it work there with the same answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_held_to_one_workspace_makes_another_only_at_its_persons_root() {
    use tilo_actions::remote::asked::{Offer, Resolve};
    use tilo_workspace_service::workspaces::at_root::LET_WORK;

    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_workspace_host(&alice, &host_device, state.path(), false).await;
    let workspaces = host.server.workspaces().unwrap();
    let lounge = workspaces.create("Lounge").await.unwrap();
    assert!(
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap()
    );
    assert!(workspaces.admit_own_through(&lounge.workspace));

    // Bob's computer keeps his root and his Servers workspace.
    let bob = new_key();
    let bob_person = bob.verifying_key().to_bytes();
    let bob_computer = enrol(&bob, "bob-computer", DeviceCapabilities::primary());
    let bob_state = tempfile::tempdir().unwrap();
    let bob_host = start_workspace_host(&bob, &bob_computer, bob_state.path(), false).await;
    let bob_workspaces = bob_host.server.workspaces().unwrap();
    tilo_workspace_service::workspaces::person_workspace::found(bob_workspaces)
        .await
        .unwrap();
    bob_workspaces
        .serve_at_root(Arc::new(
            tilo_workspace_service::workspaces::fixtures::RootIn(bob.clone()),
        ))
        .unwrap();

    // Bob's tab in the Lounge has the site make his own workspace, which his computer takes in.
    let tab_key = new_key();
    let tab_cert = guest_certificate(
        &bob,
        tab_key.verifying_key().to_bytes(),
        "Firefox on Linux",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let tab = enter_as(
        &host,
        entry_grant(&host, &alice, &host_device, &lounge.workspace, bob_person).await,
        &tab_cert,
        &tab_key,
        &host_device.cert,
        lounge.workspace_id,
    )
    .await;
    let device = Device::of(
        tab.clone(),
        tab_cert.clone(),
        host_device.cert.clone(),
        "Firefox on Linux",
    );
    let own = device
        .enter_own_workspace("en-US", Some("Bob's workspace"), "Bob", |_| false, None)
        .await
        .unwrap();
    assert_eq!(device.join(&own.workspace).await.unwrap(), own.workspace_id);
    take_in(bob_workspaces, workspaces, &own.workspace).await;

    // A key Bob made for that one workspace.
    let script_key = new_key();
    let script_cert = tilo_server_client::certification::issue_agent_grant(
        &bob,
        script_key.verifying_key().to_bytes(),
        "A script",
        DeviceCapabilities::agent_grant(AgentScope::parse(&own.workspace).unwrap()),
        Some(tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000)),
    )
    .await
    .unwrap();
    let script = on_host(&host, &host_device.cert, (script_cert.clone(), &script_key));
    script.connected().await.unwrap();
    assert_eq!(
        script
            .enter_workspace(&own.workspace, "A script")
            .await
            .unwrap(),
        own.workspace_id
    );
    let at_root = (bob_person, bob_computer.cert.device_public_key);
    let approve = || Resolve {
        resolution: Offer::Approve,
        ..Resolve::default()
    };
    let held_before = bob_workspaces.ids().await.len();

    // Without a yes at Bob's root nothing is made: the site, which keeps no root of his, answers
    // nothing for it, and his no there ends the make.
    let more = asks_to_make(&script, "More").await;
    let at_site = answer_with(
        workspaces,
        (bob_person, tab_key.verifying_key().to_bytes()),
        &more,
        approve(),
    )
    .await
    .expect_err("the site keeps no root of Bob's");
    assert_eq!(
        at_site.to_string(),
        tilo_actions::remote::ActionCallError::Refused {
            reason: tilo_actions::remote::asked::ANSWERED_AT_ROOT.into()
        }
        .to_string()
    );
    taken_in(bob_workspaces, &more).await;
    answer_with(
        bob_workspaces,
        at_root,
        &more,
        Resolve {
            resolution: Offer::Decline,
            ..Resolve::default()
        },
    )
    .await
    .expect("Bob says no on his computer");
    let declined = script
        .answer_of(&more, FOLLOWING)
        .await
        .expect_err("a declined make makes nothing");
    assert_eq!(declined.code, DeviceRefusal::DECLINED, "{declined:?}");
    assert_eq!(
        bob_workspaces.ids().await.len(),
        held_before,
        "nothing is made"
    );

    // His yes founds it on his computer, once, as him: a second yes finds it answered.
    let plans = asks_to_make(&script, "Plans").await;
    taken_in(bob_workspaces, &plans).await;
    let question = bob_workspaces
        .asked_store()
        .unwrap()
        .kept_ask(&plans.ask)
        .expect("Bob's computer asks him");
    assert!(
        question
            .facts
            .facts
            .contains(&tilo_actions::remote::asked::Fact::new(
                "Kept at",
                "alice-home, run by tilo.test"
            )),
        "the question names where it will live: {:?}",
        question.facts
    );
    answer_with(bob_workspaces, at_root, &plans, approve())
        .await
        .expect("Bob says yes on his computer");
    let again = answer_with(bob_workspaces, at_root, &plans, approve())
        .await
        .expect_err("an approve is used once");
    assert!(
        matches!(again, tilo_actions::remote::ActionCallError::Refused { .. }),
        "{again:?}"
    );
    let made = script
        .answer_of(&plans, FOLLOWING)
        .await
        .expect("his yes makes it, and the key reads it");
    let made_workspace = made["workspace"].as_str().unwrap().to_owned();
    assert_eq!(made["name"], json!("Plans"), "{made}");
    assert!(
        made.get("grant").is_none(),
        "no reach was asked for: {made}"
    );
    let summary = bob_workspaces.summary(&made_workspace).await.unwrap();
    let bobs = summary
        .members
        .iter()
        .find(|member| member.root() == Some(bob_person) && !member.removed)
        .expect("Bob is in the workspace made for him");
    assert_eq!(
        bobs.role,
        Some(tilo_peer_state::join::Role::Owner),
        "{:?}",
        summary.members
    );
    assert!(
        workspaces
            .own_workspaces_of(&bob_person)
            .contains(&made_workspace),
        "it lives at the site the question named, kept there as Bob's own"
    );
    assert_eq!(
        bob_workspaces.ids().await.len(),
        held_before + 1,
        "one is made"
    );
    // The key reaches no further than before: it does not enter the new workspace.
    let outside = on_host(
        &bob_host,
        &bob_computer.cert,
        (script_cert.clone(), &script_key),
    );
    assert!(
        outside
            .enter_workspace(&made_workspace, "A script")
            .await
            .is_err(),
        "a key Bob let work in one workspace has no reach in the one he made"
    );
    outside.close();

    // Let work there with the same answer, the key gets its grant in the new workspace, signed
    // by his root, and enters it.
    let together = asks_to_make(&script, "Together").await;
    taken_in(bob_workspaces, &together).await;
    answer_with(
        bob_workspaces,
        at_root,
        &together,
        Resolve {
            chosen: vec![LET_WORK],
            ..approve()
        },
    )
    .await
    .expect("Bob says yes and lets the script work there");
    let made = script
        .answer_of(&together, FOLLOWING)
        .await
        .expect("his yes makes it");
    let together_workspace = made["workspace"].as_str().unwrap().to_owned();
    let grant: tilo_user_identity::DeviceCert =
        serde_json::from_value(made["grant"].clone()).expect("the key's grant there");
    assert_eq!(
        grant.device_public_key,
        script_key.verifying_key().to_bytes()
    );
    assert_eq!(grant.user_public_key, bob_person);
    let inside = on_host(&bob_host, &bob_computer.cert, (grant, &script_key));
    assert_eq!(
        inside
            .enter_workspace(&together_workspace, "A script")
            .await
            .unwrap()
            .to_string(),
        together_workspace
    );
    inside.close();

    script.close();
    tab.close();
    drop(host.web);
    host.server.shutdown().await;
    drop(bob_host.web);
    bob_host.server.shutdown().await;
}
