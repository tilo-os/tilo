//! A workspace asked for on a device of the person's that founds none, made on the one that
//! keeps their root: kept as its record in the person's own workspace, taken in there by the
//! runner of every call kept for the root, put to the person there where their grants say so,
//! taken in from the person's list by the asking device and every other device of theirs, by
//! itself, refused to another person's device and to an agent, and refused in plain words, in
//! time, while no device that keeps the root takes it in.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::asked::{Offer, RESOLVE, Resolve};
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::at_root::{self, Decided};
use tilo_workspace_service::workspaces::person_workspace;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};

use super::*;
use crate::device::clipboard::tests::{TwoDevices, two_devices};
use crate::device::ticket_tests;

/// The second device's key, as the fixture made it: what its programs' API is served as.
const DESK_KEY: [u8; 32] = [0x73; 32];

/// The desk, a device of the person's that founds none of their workspaces, as its programs'
/// account reaches it: it lists none itself, and asks the person's founding device as the
/// daemon does.
struct Desk {
    workspaces: Arc<ServiceWorkspaces>,
}

impl tilo_api::session::Here for Desk {
    fn workspaces(&self, _: &DeviceCert) -> Vec<tilo_api::session::Workspace> {
        Vec::new()
    }

    fn enter(
        &self,
        workspace: &str,
        _: &tilo_api::grant_key::GrantKey,
    ) -> tilo_api::session::Asked<'_, tilo_api::session::Reached> {
        let refused = format!("the desk keeps no {workspace}");
        Box::pin(async move { Err(refused) })
    }

    fn create_where_founded(
        &self,
        creating: tilo_api::session::Creating,
    ) -> tilo_api::session::FoundingAnswer<'_> {
        Box::pin(ask(&self.workspaces, creating))
    }
}

/// Keep `cert` in the person's devices record `workspaces` holds.
async fn keep(workspaces: &ServiceWorkspaces, cert: &DeviceCert) {
    let own = tilo_server_protocol::servers::person_workspace_id(&cert.user_public_key);
    person_workspace::apply_on(
        workspaces.copies(),
        &own,
        DEVICES,
        Devices::keeping(cert).expect("a device certificate encodes"),
    )
    .await
    .expect("the person's record takes a device");
}

/// The two devices: the first keeps the person's root, founded their own workspace and makes
/// their workspaces here, and its record lists the second, which holds the person's own workspace
/// as every other device of theirs does, as a member copy of the first's.
async fn founding_and_desk() -> TwoDevices {
    let devices = two_devices().await;
    person_workspace::found(devices.first.copies())
        .await
        .expect("the person's own workspace");
    devices.first.offer_workspaces_here("Laptop");
    keep(&devices.first, &devices.second_cert).await;
    holds_person_workspace(&devices, &devices.second).await;
    devices
}

/// `device`, one of the person's that the founding device's record lists, holds the person's own
/// workspace as a member copy of the founding device's, as every other device of theirs does.
async fn holds_person_workspace(devices: &TwoDevices, device: &ServiceWorkspaces) {
    let own = person_workspace_id(&devices.person);
    let hint = devices
        .first
        .copies()
        .hosts(&own)
        .await
        .expect("the founding device hosts the person's own workspace")
        .into_iter()
        .find(|hint| hint.host.device_public_key == devices.first_cert.device_public_key)
        .expect("it says where it serves it");
    tilo_test_env::within(
        "the device holds the person's own workspace",
        device
            .copies()
            .hold_as_member(WorkspaceId::parse(&own).unwrap(), &hint),
    )
    .await
    .expect("the device holds the person's own workspace");
}

/// A third device of the person's, certified by their root and listed in their devices record,
/// which holds only their own workspace, as a member copy, and asks nothing of anyone.
async fn studio(devices: &TwoDevices) -> Arc<ServiceWorkspaces> {
    let root = ticket_tests::root_in(devices.state.path().to_path_buf())()
        .expect("the root reads")
        .expect("the issuer keeps the person's root");
    let key = SigningKey::from_bytes(&[0x75; 32]);
    let mut cert = DeviceCert {
        user_public_key: devices.person,
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Studio".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).expect("the root certifies it");
    keep(&devices.first, &cert).await;
    let studio = ServiceWorkspaces::open_for_test(
        &devices.state.path().join("studio"),
        &key,
        &cert,
        &tilo_server_client::ServerAddress::at(devices.first.endpoint().addr()),
    )
    .await
    .expect("the studio's service opens");
    holds_person_workspace(devices, &studio).await;
    studio
}

/// The first device runs what is kept for the person's root, as the one keeping it: what the
/// guard it answers says ([`crate::device::services::root_record_for_test`]).
fn serve_root(devices: &TwoDevices) -> impl Drop {
    let kept = crate::device::services::root_record_for_test(
        tilo_identity::store::resolve_active_identity(devices.state.path())
            .expect("the founding device keeps the person's root"),
    );
    devices
        .first
        .copies()
        .serve_at_root(crate::device::asked::root_kept(
            &devices.first,
            devices.first_tickets.clone(),
        ))
        .expect("the founding device runs what is kept for the person's root");
    kept
}

/// The desk's account, as its person's programs reach it.
fn desk_account(desk: &Arc<ServiceWorkspaces>, cert: &DeviceCert) -> tilo_api::direct::Direct {
    tilo_api::direct::Direct::here(
        Arc::new(Desk {
            workspaces: Arc::clone(desk),
        }) as Arc<dyn tilo_api::session::Here>,
        SigningKey::from_bytes(&DESK_KEY),
        cert.clone(),
        "Desk",
    )
    .expect("the desk's API starts")
}

/// `account/workspaces#create` with `arguments`, as the person asks it on the desk: what it
/// answered, or how it refused.
async fn create_on_desk(
    desk: Arc<ServiceWorkspaces>,
    cert: DeviceCert,
    arguments: Value,
) -> Result<Value, String> {
    let direct = desk_account(&desk, &cert);
    let tree = tilo_test_env::within("the desk's account answers", direct.account_tree())
        .await
        .expect("the account's own tree answers");
    let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let Value::Object(arguments) = arguments else {
        panic!("a create names its arguments");
    };
    let receipt = tree
        .invoke(
            &caller,
            InvokeRequest::plain(
                RemoteActionTarget {
                    provider: tilo_api::account::PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_api::account::WORKSPACES,
                        "create",
                    ),
                },
                arguments,
            ),
        )
        .await
        .expect("the account answers");
    match receipt {
        Ok(receipt) => match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
            other => Err(format!("{other:?}")),
        },
        Err(refused) => Err(format!("{refused:?}")),
    }
}

/// [`create_on_desk`] on the desk of `devices`, while the test goes on.
fn create_on_desk_meanwhile(
    devices: &TwoDevices,
    arguments: Value,
) -> tokio::task::JoinHandle<Result<Value, String>> {
    tokio::spawn(create_on_desk(
        Arc::clone(&devices.second),
        devices.second_cert.clone(),
        arguments,
    ))
}

/// Whether `workspaces` lists `workspace` and opens it: holds its copy, as a member.
async fn opens(workspaces: &ServiceWorkspaces, workspace: &str) -> bool {
    workspaces
        .copies()
        .list()
        .await
        .iter()
        .any(|summary| summary.workspace == workspace)
        && workspaces.copies().state(workspace).await.is_some()
}

/// Wait until `workspaces` lists `workspace` and opens it ([`opens`]), which its follower of the
/// person's list brings about by itself.
async fn opened(workspaces: &ServiceWorkspaces, workspace: &str, what: &str) {
    tilo_test_env::within(
        format!("{what}; not here: {:?}", workspaces.copies().not_here()),
        async {
            while !opens(workspaces, workspace).await {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        },
    )
    .await;
    assert_eq!(
        workspaces.copies().is_member(workspace).await,
        Ok(true),
        "it holds it as the person's other devices hold theirs, a member copy: {what}"
    );
}

/// The one question waiting on the founding device in the person's own workspace, once it is
/// there.
fn the_question(devices: &TwoDevices) -> tilo_remote_actions::asked::Waiting {
    let store = devices
        .first
        .copies()
        .asked_store()
        .expect("the founding device keeps the person's questions");
    let own = person_workspace_id(&devices.person);
    tilo_test_env::wait_for("the founding device to ask the person", move || {
        let mut waiting = store.waiting_in(&own);
        let question = waiting.pop()?;
        assert!(waiting.is_empty(), "one question waits: {waiting:?}");
        Some(question)
    })
}

/// The person answers `question` with `offer` on the founding device, picking none of its
/// choices.
async fn answer(
    devices: &TwoDevices,
    question: &tilo_remote_actions::asked::Waiting,
    offer: Offer,
) {
    answer_there(devices, question, (offer, Vec::new())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_asked_for_on_a_device_that_founds_none_is_made_on_the_one_that_does_and_opens_on_both()
 {
    // The desk runs Tilo OS: its person's root certifies it as one of their own, whose actions
    // beyond content need no other device's yes.
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    home.env().set(
        tilo_identity::provisioning::TEST_PLATFORM_ENV,
        tilo_identity::provisioning::TILO_OS_PLATFORM,
    );
    let devices = founding_and_desk().await;
    let studio = studio(&devices).await;
    let _root = serve_root(&devices);

    // The person asks on the desk: the founding device makes it and answers once it has. Each
    // other device of theirs, the desk and the studio, takes it in from the person's list by
    // itself; the studio is asked nothing and does nothing.
    let id = uuid::Uuid::new_v4().to_string();
    let made = create_on_desk(
        Arc::clone(&devices.second),
        devices.second_cert.clone(),
        json!({"name": "Applicant: Jane Doe", "id": id}),
    )
    .await
    .expect("the founding device makes the workspace");
    assert_eq!(made["workspace"]["id"], id, "{made}");
    assert_eq!(made["workspace"]["name"], "Applicant: Jane Doe", "{made}");
    assert!(
        opens(&devices.first, &id).await,
        "the founding device keeps the workspace it made"
    );
    opened(
        &devices.second,
        &id,
        "the desk lists the workspace it asked for and opens it",
    )
    .await;
    opened(
        &studio,
        &id,
        "the studio lists the workspace made from the desk and opens it",
    )
    .await;
    assert!(studio.copies().not_here().is_empty());
    assert!(
        devices
            .first
            .copies()
            .asked_store()
            .expect("the founding device keeps the person's questions")
            .waiting_in(&person_workspace_id(&devices.person))
            .is_empty(),
        "a device of Tilo OS is asked nothing about"
    );

    // A device of another person that takes part in a workspace the founding device keeps (the
    // person invited them into the one just made) asks over the network as the desk does, and
    // has nothing made there: it is no device of this person's.
    let (key, stranger) = tilo_workspace_service::workspaces::fixtures::guest_device();
    let invitation = devices
        .first
        .copies()
        .issue(
            &id,
            &devices.first_cert,
            tilo_workspace_service::workspaces::IssueInvitationRequest {
                request_id: rand::random(),
                role: tilo_peer_state::join::Role::Editor,
                recipient_user_public_key: Some(stranger.user_public_key),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .expect("the person invites someone into the workspace made")
        .signed_invitation;
    let joined = devices
        .first
        .copies()
        .admit_proven(
            invitation,
            (stranger.clone(), &key),
            rand::random(),
            "Cy",
            None,
        )
        .await;
    assert!(
        matches!(
            joined,
            tilo_server_protocol::workspaces::JoinReply::Admitted { .. }
        ),
        "{joined:?}"
    );
    let mut asked = serde_json::Map::new();
    asked.insert("name".into(), json!("Not yours"));
    let request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: at_root::FOUND_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                at_root::FOUND_RESOURCE,
                at_root::FOUND_ACTION,
            ),
        },
        asked,
    );
    for workspace in [id.clone(), person_workspace_id(&devices.person)] {
        let caller = Caller::authenticated(stranger.user_public_key, stranger.device_public_key)
            .in_workspace(workspace.clone());
        assert!(
            matches!(
                devices.first.copies().found_decides(&caller, &workspace),
                Decided::Refuse(_)
            ),
            "a device of another person is refused before anyone is asked in {workspace}"
        );
        let ran = devices
            .first
            .copies()
            .found_asked(&caller, &request, &workspace)
            .await;
        assert!(
            matches!(&ran, tilo_actions::InvocationUpdate::Failed { error, .. }
                if error.message == at_root::NOT_FOR_THIS_DEVICE),
            "nothing is made for a device of another person in {workspace}: {ran:?}"
        );
    }
    studio.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_a_device_off_tilo_os_asks_for_waits_for_the_persons_yes_on_the_founding_device()
 {
    // The desk runs an operating system Tilo does not control: making a workspace reaches beyond
    // it, which the person's grants leave to them.
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = founding_and_desk().await;
    let _root = serve_root(&devices);
    let own = WorkspaceId::parse(&person_workspace_id(&devices.person)).unwrap();

    // The person says yes on the founding device: it makes the workspace, and the desk opens it.
    let id = uuid::Uuid::new_v4().to_string();
    let asking = create_on_desk_meanwhile(&devices, json!({"name": "Quarterly plan", "id": id}));
    let question = the_question(&devices);
    assert_eq!(question.asked.facts.on.as_deref(), Some("Quarterly plan"));
    assert!(
        question.asked.facts.words.contains("Quarterly plan"),
        "the person reads what is made: {:?}",
        question.asked.facts.words
    );
    assert!(
        !opens(&devices.first, &id).await,
        "nothing is made before the person says yes"
    );
    answer(&devices, &question, Offer::Approve).await;
    let made = tilo_test_env::within("the desk hears the founding device", asking)
        .await
        .expect("the desk's ask ends")
        .expect("the founding device makes the workspace once the person says yes");
    assert_eq!(made["workspace"]["id"], id, "{made}");
    assert!(opens(&devices.first, &id).await, "{made}");
    opened(
        &devices.second,
        &id,
        "the desk lists the workspace it asked for and opens it",
    )
    .await;

    // The person says no: nothing is made, and the desk says so in plain words.
    let declined_id = uuid::Uuid::new_v4().to_string();
    let asking =
        create_on_desk_meanwhile(&devices, json!({"name": "Not this one", "id": declined_id}));
    let question = the_question(&devices);
    answer(&devices, &question, Offer::Decline).await;
    let declined = tilo_test_env::within("the desk hears the founding device", asking)
        .await
        .expect("the desk's ask ends")
        .expect_err("the person said no");
    assert!(
        declined.contains(tilo_api::account::FOUNDING_DECLINED)
            && declined
                .contains("you said no to making it on the device that makes your workspaces"),
        "{declined}"
    );
    assert!(!opens(&devices.first, &declined_id).await);
    assert!(!opens(&devices.second, &declined_id).await);

    // An agent acting for the person is refused, from the desk as well: what the person
    // themselves is asked about, through the same device and certificate, it never asks.
    let own = own.to_string();
    let desk = devices.second_cert.device_public_key;
    let person = Caller::authenticated(devices.person, desk).in_workspace(own.clone());
    let agent = Caller::delegated_agent("planner", devices.person, desk).in_workspace(own.clone());
    assert_eq!(
        devices.first.copies().found_decides(&person, &own),
        Decided::Ask,
        "the person is asked"
    );
    assert!(
        matches!(
            devices.first.copies().found_decides(&agent, &own),
            Decided::Refuse(_)
        ),
        "an agent is refused"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_with_no_founding_device_in_reach_is_refused_in_plain_words_in_time() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = founding_and_desk().await;

    // While no device that keeps the person's root takes the ask in, the desk says so within the
    // bound, and nothing is made.
    let asked = Instant::now();
    let none = create_on_desk(
        Arc::clone(&devices.second),
        devices.second_cert.clone(),
        json!({"name": "Too early"}),
    )
    .await
    .expect_err("no device makes it");
    assert!(
        none.contains(tilo_api::account::FOUNDING_DEVICE_UNREACHED),
        "{none}"
    );
    assert!(
        asked.elapsed() <= WAITED + Duration::from_secs(5),
        "the refusal comes within the bound: {:?}",
        asked.elapsed()
    );

    // Once it does but is away, the person hears so within the bound, and nothing else is asked.
    let _root = serve_root(&devices);
    devices
        .first_router
        .shutdown()
        .await
        .expect("the founding device stops answering");
    let asked = Instant::now();
    let away = create_on_desk(
        Arc::clone(&devices.second),
        devices.second_cert.clone(),
        json!({"name": "While away"}),
    )
    .await
    .expect_err("the founding device is away");
    let waited = asked.elapsed();
    assert!(
        away.contains(tilo_api::account::FOUNDING_DEVICE_UNREACHED)
            && away.contains("the device that makes your workspaces didn't answer within"),
        "{away}"
    );
    assert!(
        waited <= WAITED + Duration::from_secs(5),
        "the refusal comes within the bound: {waited:?}"
    );
}

/// A site of another operator's that keeps the person's workspaces: a device of its own person,
/// which keeps workspaces of people's own for those its home workspace lets in, the person among
/// them, and keeps one of the person's that their founding device placed there.
struct Site {
    device: Arc<ServiceWorkspaces>,
    cert: DeviceCert,
    /// The workspace of the person's the site keeps.
    kept: String,
    _router: iroh::protocol::Router,
    _dir: tempfile::TempDir,
    _daemon: tilo_device_identity::persist::DeviceKeyLock,
}

/// [`Site`] for the person of `devices`, whose founding device is the first.
async fn site_of_another_operator(devices: &TwoDevices) -> Site {
    let (site, dir, _key, cert, daemon) = ServiceWorkspaces::test_of_person().await;
    assert_ne!(cert.user_public_key, devices.person);
    let router = ticket_tests::serve(&site, site.copies().action_service());
    site.offer_workspaces_here("site.test");
    let home = site
        .copies()
        .create("Home")
        .await
        .expect("the site's home workspace");
    assert!(site.copies().admit_own_through(&home.workspace));
    tilo_workspace_service::workspaces::fixtures::let_in(
        site.copies(),
        &home.workspace,
        devices.person,
    )
    .await;
    // The founding device keeps the person's workspaces as theirs, and places one at the site.
    devices.first.claim_workspaces_here(&devices.person).await;
    let kept = devices
        .first
        .copies()
        .create("Kept at the site")
        .await
        .expect("the founding device makes a workspace");
    tilo_test_env::within(
        "the site takes the person's workspace",
        devices.first.copies().attach_server(
            &kept.workspace,
            cert.device_public_key,
            site.endpoint().addr(),
            tilo_peer_state::CopyRole::KeyHolder,
        ),
    )
    .await
    .expect("the site keeps the person's workspace");
    Site {
        device: site,
        cert,
        kept: kept.workspace,
        _router: router,
        _dir: dir,
        _daemon: daemon,
    }
}

/// An agent of the person whose root `devices` keeps, its grant reaching `workspace` alone, so
/// making another is the person's to allow: its key, certificate and endpoint.
async fn agent_of(
    devices: &TwoDevices,
    workspace: &str,
) -> (SigningKey, DeviceCert, iroh::Endpoint) {
    let root = ticket_tests::root_in(devices.state.path().to_path_buf())()
        .expect("the root reads")
        .expect("the issuer keeps the person's root");
    let key = SigningKey::from_bytes(&rand::random());
    let cert = agent_grant(
        &root,
        &key,
        tilo_user_identity::AgentScope::parse(workspace).expect("a workspace's scope"),
    );
    let endpoint = ticket_tests::endpoint(&key).await;
    (key, cert, endpoint)
}

/// The grant `root` signs for `key`, an agent's, reaching `scope` for an hour.
fn agent_grant(
    root: &dyn tilo_user_identity::StatementSigner,
    key: &SigningKey,
    scope: tilo_user_identity::AgentScope,
) -> DeviceCert {
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.public_key(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Planner".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::agent_grant(scope),
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(3_600_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("the root certifies it");
    cert
}

/// `workspace.create-own` making `name`, signed by `cert`'s key for `scope` at `signed_at`.
fn create_own(
    cert: &DeviceCert,
    key: &SigningKey,
    name: &str,
    scope: &str,
    signed_at: u64,
) -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".into(), json!(name));
    create_own_with(cert, key, arguments, scope, signed_at)
}

/// The agent's `workspace.create-own` with `arguments`, signed by its key for `scope` at
/// `signed_at`.
fn create_own_with(
    cert: &DeviceCert,
    key: &SigningKey,
    arguments: serde_json::Map<String, Value>,
    scope: &str,
    signed_at: u64,
) -> InvokeRequest {
    let mut request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_server_protocol::workspaces::HOST_WORKSPACES_RESOURCE,
                tilo_server_protocol::workspaces::CREATE_OWN_WORKSPACE,
            ),
        },
        arguments,
    );
    request.wait_ms = Some(tilo_peer_state::actions::CHANGE_WAIT_MS);
    request.signed = Some(Box::new(
        tilo_server_protocol::invocation::sign_invocation_now(
            scope, &request, cert, key, signed_at,
        )
        .expect("the agent signs its call"),
    ));
    request
}

/// What `actions`, a link to a device asked in no workspace, answers `invocation`, once it moves
/// past `seen`: how it ended, or that it waits for its person's yes.
async fn followed_to(
    actions: &tilo_peer_state::actions::RemoteDevice,
    invocation: tilo_actions::InvocationId,
    mut seen: Option<tilo_actions::remote::UpdateMark>,
) -> tilo_actions::InvocationUpdate {
    tilo_test_env::within("the make to move", async {
        loop {
            let answer = actions
                .ask_message(tilo_actions::remote::RemoteActionMessage::Observe {
                    correlation: 0,
                    invocation,
                    wait_ms: Some(tilo_peer_state::actions::CHANGE_WAIT_MS),
                    seen,
                })
                .await
                .expect("the device answers")
                .expect("the device follows the make");
            let tilo_actions::remote::RemoteActionMessage::Update {
                state: Some(state), ..
            } = answer
            else {
                panic!("the device knows the make: {answer:?}");
            };
            if state.is_terminal()
                || tilo_actions::remote::asked::RecordRef::waited_in(&state.update).is_some()
            {
                return state.update;
            }
            seen = Some(tilo_actions::remote::UpdateMark::of(&state.update));
        }
    })
    .await
}

/// How the make `invocation`, which `waits` said waits for its person, ends, as `actions` reads
/// it: what it says while it still waits is passed over.
async fn ended_after(
    actions: &tilo_peer_state::actions::RemoteDevice,
    invocation: tilo_actions::InvocationId,
    waits: &tilo_actions::InvocationUpdate,
) -> tilo_actions::InvocationUpdate {
    let mut update = waits.clone();
    tilo_test_env::within("the make to end", async {
        loop {
            update = followed_to(
                actions,
                invocation,
                Some(tilo_actions::remote::UpdateMark::of(&update)),
            )
            .await;
            if matches!(
                update,
                tilo_actions::InvocationUpdate::Succeeded { .. }
                    | tilo_actions::InvocationUpdate::Failed { .. }
                    | tilo_actions::InvocationUpdate::Cancelled { .. }
            ) {
                return update;
            }
        }
    })
    .await
}

/// The question waiting on the founding device of `devices` for the make `invocation`, kept in
/// `workspace`, once it took it in.
fn question_for(
    devices: &TwoDevices,
    workspace: &str,
    invocation: tilo_actions::InvocationId,
) -> tilo_remote_actions::asked::Waiting {
    let store = devices
        .first
        .copies()
        .asked_store()
        .expect("the founding device keeps the person's questions");
    tilo_test_env::wait_for("the founding device to ask the person", || {
        store
            .waiting_in(workspace)
            .into_iter()
            .find(|waiting| waiting.invocation == invocation)
    })
}

/// The person answers `question` with `offer` on the founding device of `devices`, picking
/// `chosen` among its choices.
async fn answer_there(
    devices: &TwoDevices,
    question: &tilo_remote_actions::asked::Waiting,
    (offer, chosen): (Offer, Vec<usize>),
) {
    let resolving = Resolve {
        resolution: offer,
        chosen,
        ..Resolve::default()
    };
    let Value::Object(arguments) = serde_json::to_value(resolving).unwrap() else {
        panic!("an answer is an object");
    };
    let request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: tilo_action_services::workspace_invocations::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(&question.asked.record.record, RESOLVE),
        },
        arguments,
    );
    let person = Caller::authenticated(devices.person, devices.first_cert.device_public_key)
        .in_workspace(question.asked.record.workspace.clone());
    let answered = tilo_test_env::within(
        "the founding device takes the person's answer",
        devices.first.copies().resolve_asked(&person, &request),
    )
    .await
    .expect("answered")
    .expect("the answer is taken");
    assert!(
        matches!(answered.status, ReceiptStatus::Completed { .. }),
        "{answered:?}"
    );
}

/// Whether the person's workspaces on `workspaces` include one named `name`.
async fn has_named(workspaces: &ServiceWorkspaces, person: &[u8; 32], name: &str) -> bool {
    for workspace in workspaces.copies().own_workspaces_of(person) {
        if workspaces
            .copies()
            .summary(&workspace)
            .await
            .is_ok_and(|summary| summary.name == name)
        {
            return true;
        }
    }
    false
}

/// An agent whose grant reaches the workspace a site keeps asks the site for another. The site
/// founds nothing: it keeps the make for a device that keeps the person's root, in the workspace
/// the agent reaches, and the question names the site, its operator and its address. The
/// person's founding device takes it in and asks them there. Their yes founds it there, places
/// it at that site alone, and lets the agent in at the site where they picked that; a no makes
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_make_at_a_site_is_founded_on_the_persons_device_once_they_say_yes_there() {
    use tilo_workspace_service::workspaces::at_root::LET_WORK;

    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = founding_and_desk().await;
    // The founding device's computer keeps the person's root: what lets the agent in.
    let _root_kept_here = crate::device::services::root_record_for_test(
        tilo_identity::store::resolve_active_identity(devices.state.path())
            .expect("the founding device keeps the person's root"),
    );
    let site = site_of_another_operator(&devices).await;
    devices
        .first
        .copies()
        .serve_at_root(crate::device::asked::root_kept(
            &devices.first,
            devices.first_tickets.clone(),
        ))
        .expect("the founding device runs what waits for the person's root");
    let person = devices.person;
    let (key, agent, endpoint) = agent_of(&devices, &site.kept).await;
    let at_site = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    )
    .remote(&endpoint, &site.device.endpoint().addr(), "");
    let caller = Caller::authenticated(person, agent.device_public_key);
    let asks = |name: &str| create_own(&agent, &key, name, "", tilo_clock::unix_now_ns());

    // The agent asks the site for a workspace its grant does not reach. The site founds nothing
    // for an agent: it keeps the make for the person's root, in the workspace the agent reaches,
    // and the agent reads that its make waits, and in which question.
    let asked = asks("Planning");
    let invocation = asked.invocation;
    let receipt =
        tilo_test_env::within("the site answers the agent", at_site.invoke(&caller, asked))
            .await
            .expect("the site answers")
            .expect("the site takes the make");
    assert!(
        !matches!(
            receipt.status,
            ReceiptStatus::Completed { .. } | ReceiptStatus::Rejected { .. }
        ),
        "the make waits for the person: {receipt:?}"
    );
    let waits = followed_to(&at_site, invocation, None).await;
    let ask = tilo_actions::remote::asked::RecordRef::waited_in(&waits)
        .unwrap_or_else(|| panic!("the agent reads that its make waits for its person: {waits:?}"));
    assert_eq!(ask.workspace, site.kept, "kept where the agent reaches");
    let question = question_for(&devices, &site.kept, invocation);
    assert_eq!(
        question.asked.record, ask,
        "the agent's follow names the question"
    );
    let shown: Vec<(String, String)> = question
        .asked
        .facts
        .facts
        .iter()
        .map(|fact| (fact.label.clone(), fact.value.clone()))
        .collect();
    assert!(
        shown.contains(&(
            "Kept at".to_owned(),
            format!("{}, run by site.test", site.cert.device_label)
        )),
        "the question names the site and its operator: {shown:?}"
    );
    assert!(
        shown.iter().any(|(label, value)| label == "Address"
            && value.starts_with(&hex::encode(site.cert.device_public_key))),
        "the question names the site's address: {shown:?}"
    );
    assert!(
        !has_named(&site.device, &person, "Planning").await
            && !has_named(&devices.first, &person, "Planning").await,
        "nothing is made before the person says yes"
    );

    // The person says yes on their own device, letting the agent work there, after the agent
    // stopped following: the make goes on there, and a later follow returns the workspace,
    // placed at the site and kept there as the person's own, with the agent's entry naming the
    // site.
    answer_there(&devices, &question, (Offer::Approve, vec![LET_WORK])).await;
    let made = ended_after(&at_site, invocation, &waits).await;
    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = made else {
        panic!("the make ends with the workspace: {made:?}");
    };
    let answered = output.value.expect("an answer");
    let made: tilo_server_protocol::workspaces::CreatedWorkspace =
        serde_json::from_value(answered.clone()).expect("a made workspace");
    let workspace = made.workspace.workspace.clone();
    assert_eq!(made.workspace.name, "Planning");
    assert_eq!(
        devices
            .first
            .copies()
            .state(&workspace)
            .await
            .expect("the founding device keeps it")
            .governance()
            .founder(),
        Some(devices.first_cert.device_public_key),
        "the founding device founded it"
    );
    assert!(
        site.device
            .copies()
            .own_workspaces_of(&person)
            .contains(&workspace),
        "the site keeps it as the person's own"
    );
    let invitation =
        hex::decode(made.invitation.expect("the agent's entry").invitation).expect("an invitation");
    let entry = tilo_peer_state::join::decode_and_verify_invitation(
        &invitation,
        Some(person),
        &tilo_user_identity::RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .expect("the founding device's invitation verifies");
    assert_eq!(
        entry
            .grant()
            .transport
            .iter()
            .map(|hint| hint.host.device_public_key)
            .collect::<Vec<_>>(),
        vec![site.cert.device_public_key],
        "the agent enters at the site"
    );
    let grant: DeviceCert =
        serde_json::from_value(answered["grant"].clone()).expect("the agent's grant there");
    assert!(
        grant
            .capabilities
            .agent
            .as_ref()
            .is_some_and(|scope| scope.reaches(&workspace)),
        "the person's root lets the agent's key into it alone: {grant:?}"
    );

    // A yes that leaves the agent out makes it, and lets the agent in nowhere.
    let alone = asks("On my own");
    let alone_invocation = alone.invocation;
    tilo_test_env::within("the site answers the agent", at_site.invoke(&caller, alone))
        .await
        .expect("the site answers")
        .expect("the site takes the make");
    let waits = followed_to(&at_site, alone_invocation, None).await;
    let question = question_for(&devices, &site.kept, alone_invocation);
    answer_there(&devices, &question, (Offer::Approve, Vec::new())).await;
    let made = ended_after(&at_site, alone_invocation, &waits).await;
    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = made else {
        panic!("the make ends with the workspace: {made:?}");
    };
    let answered = output.value.expect("an answer");
    assert!(
        answered.get("grant").is_none() && answered["invitation"].is_null(),
        "the agent is let in nowhere: {answered}"
    );
    assert!(has_named(&site.device, &person, "On my own").await);

    // The same make asked again under another id, with the same labels, follows the first: the
    // person is asked once, and both calls end with the one workspace made.
    let labelled = || {
        let mut arguments = serde_json::Map::new();
        arguments.insert("name".into(), json!("Atlas"));
        arguments.insert("labels".into(), json!({ "project": "atlas" }));
        create_own_with(&agent, &key, arguments, "", tilo_clock::unix_now_ns())
    };
    let first = labelled();
    let first_invocation = first.invocation;
    tilo_test_env::within("the site answers the agent", at_site.invoke(&caller, first))
        .await
        .expect("the site answers")
        .expect("the site takes the make");
    let first_waits = followed_to(&at_site, first_invocation, None).await;
    let retried = labelled();
    let retried_invocation = retried.invocation;
    tilo_test_env::within(
        "the site answers the agent",
        at_site.invoke(&caller, retried),
    )
    .await
    .expect("the site answers")
    .expect("the site takes the make asked again");
    let retried_waits = followed_to(&at_site, retried_invocation, None).await;
    assert_eq!(
        tilo_actions::remote::asked::RecordRef::waited_in(&retried_waits),
        tilo_actions::remote::asked::RecordRef::waited_in(&first_waits),
        "the make asked again follows the record kept the first time"
    );
    let question = question_for(&devices, &site.kept, first_invocation);
    answer_there(&devices, &question, (Offer::Approve, Vec::new())).await;
    let made_ids: Vec<Value> = [
        ended_after(&at_site, first_invocation, &first_waits).await,
        ended_after(&at_site, retried_invocation, &retried_waits).await,
    ]
    .into_iter()
    .map(|ended| match ended {
        tilo_actions::InvocationUpdate::Succeeded { output, .. } => {
            output.value.expect("an answer")["workspace"]["workspace"].clone()
        }
        other => panic!("both calls end with the workspace: {other:?}"),
    })
    .collect();
    assert_eq!(made_ids[0], made_ids[1], "one workspace is made for both");

    // The person says no: nothing is made, and the agent reads their no.
    let declined = asks("Not this one");
    let declined_invocation = declined.invocation;
    tilo_test_env::within(
        "the site answers the agent",
        at_site.invoke(&caller, declined),
    )
    .await
    .expect("the site answers")
    .expect("the site takes the make");
    let waits = followed_to(&at_site, declined_invocation, None).await;
    let question = question_for(&devices, &site.kept, declined_invocation);
    answer_there(&devices, &question, (Offer::Decline, Vec::new())).await;
    let no = ended_after(&at_site, declined_invocation, &waits).await;
    assert!(
        matches!(&no, tilo_actions::InvocationUpdate::Cancelled { message, .. }
            if message.as_deref() == Some(tilo_actions::remote::asked::DECLINED)),
        "the agent reads the person's no: {no:?}"
    );
    assert!(!has_named(&site.device, &person, "Not this one").await);
    assert!(!has_named(&devices.first, &person, "Not this one").await);
    site.device.shutdown().await;
}

/// What a site keeps for the person's root it writes as the device the person placed for their
/// record of asked calls, and as nothing else: a record beside it that the site writes is
/// refused. Once the person revokes the site, a make an agent asks there ends at once, saying
/// the question could not be kept; nothing waits for an answer that cannot come.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_site_writes_the_asked_calls_alone_and_ends_an_ask_it_cannot_keep_once_revoked() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = founding_and_desk().await;
    let site = site_of_another_operator(&devices).await;
    devices
        .first
        .copies()
        .serve_at_root(crate::device::asked::root_kept(
            &devices.first,
            devices.first_tickets.clone(),
        ))
        .expect("the founding device runs what waits for the person's root");
    let (key, agent, endpoint) = agent_of(&devices, &site.kept).await;
    let at_site = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    )
    .remote(&endpoint, &site.device.endpoint().addr(), "");
    let caller = Caller::authenticated(devices.person, agent.device_public_key);
    let asks = |name: &str| create_own(&agent, &key, name, "", tilo_clock::unix_now_ns());

    // While its placement stands, the site keeps an agent's make for the person's root.
    let asked = asks("Planning");
    let invocation = asked.invocation;
    tilo_test_env::within("the site answers the agent", at_site.invoke(&caller, asked))
        .await
        .expect("the site answers")
        .expect("the site takes the make");
    let waits = followed_to(&at_site, invocation, None).await;
    assert!(
        tilo_actions::remote::asked::RecordRef::waited_in(&waits).is_some(),
        "the make waits for the person: {waits:?}"
    );
    question_for(&devices, &site.kept, invocation);

    // Its placed device writes nothing beside that record.
    let beside = tilo_test_env::within(
        "the site's write beside the asked calls is decided",
        tilo_workspace_service::workspaces::fixtures::write_as_asked_calls(
            site.device.copies(),
            &site.kept,
            (
                tilo_action_services::workspace_invocations::PROVIDER,
                "tilo.invocations.asked",
                "beside",
            ),
        ),
    )
    .await;
    assert!(
        beside
            .as_ref()
            .is_err_and(|refusal| refusal.contains("records it was placed for alone")),
        "a record beside the asked calls is refused: {beside:?}"
    );

    // Started again, the site keeps its placement from disk, and keeps the next make as before.
    tilo_workspace_service::workspaces::fixtures::restart_asks_placements(site.device.copies());
    let again = asks("After a restart");
    let again_invocation = again.invocation;
    tilo_test_env::within("the site answers the agent", at_site.invoke(&caller, again))
        .await
        .expect("the site answers")
        .expect("the site takes the make");
    let waits_again = followed_to(&at_site, again_invocation, None).await;
    assert!(
        tilo_actions::remote::asked::RecordRef::waited_in(&waits_again).is_some(),
        "after a restart the make waits for the person: {waits_again:?}"
    );
    question_for(&devices, &site.kept, again_invocation);

    // The person's root revokes the site, and the site's copy hears it from their device.
    let root = ticket_tests::root_in(devices.state.path().to_path_buf())()
        .expect("the root reads")
        .expect("the issuer keeps the person's root");
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: devices.person,
        revoked_device_public_key: site.cert.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &root).expect("the root revokes");
    tilo_test_env::within(
        "the site's copy hears the revocation",
        tilo_workspace_service::workspaces::fixtures::hear_revocations(
            site.device.copies(),
            &site.kept,
            &devices.first_cert,
            vec![revoked],
        ),
    )
    .await;

    // The agent asks again: the site cannot keep it, and the make ends with that at once.
    let again = asks("Later");
    let again_invocation = again.invocation;
    tilo_test_env::within("the site answers the agent", at_site.invoke(&caller, again))
        .await
        .expect("the site answers")
        .expect("the site takes the make");
    let first = followed_to(&at_site, again_invocation, None).await;
    let ended = if tilo_actions::remote::asked::RecordRef::waited_in(&first).is_some() {
        ended_after(&at_site, again_invocation, &first).await
    } else {
        first
    };
    assert!(
        matches!(&ended, tilo_actions::InvocationUpdate::Failed { error, .. }
            if error.code == "unkept" && error.message.contains("could not be kept")),
        "the make ends, saying its question could not be kept: {ended:?}"
    );
    tilo_test_env::within("the site stops", site.device.shutdown()).await;
}
