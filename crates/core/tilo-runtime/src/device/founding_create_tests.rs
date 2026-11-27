//! A workspace asked for on a device of the person's that founds none, made on the one that does,
//! over the action service both serve, each asking in the person's own workspace: asked through
//! the account the device answers its person's programs with, taken in by the asking device from
//! the person's list, put to the person on the founding device where their grants say so,
//! refused to another person's device and to an agent, and refused in plain words, in time, when
//! the founding device is away.

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
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::servers::person_workspace_id;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};
use tilo_workspace_service::workspaces::providers::{WorkspaceAnswers as _, WorkspaceCall};

use super::*;
use crate::device::clipboard::tests::{TwoDevices, answers_at, two_devices};
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
    FoundingCreate::new(&devices.first).register(&devices.first.copies().providers());
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
        "the desk holds the person's own workspace",
        devices
            .second
            .copies()
            .hold_as_member(WorkspaceId::parse(&own).unwrap(), &hint),
    )
    .await
    .expect("the desk holds the person's own workspace");
    devices
}

/// Mark the first device in the person's record as the one keeping their root, signed by that
/// root, say where it answers, and wait until the desk's copy of the record says so.
async fn mark_founding(devices: &TwoDevices) {
    let root = ticket_tests::root_in(devices.state.path().to_path_buf())()
        .expect("the root reads")
        .expect("the issuer keeps the person's root");
    let cert = &devices.first_cert;
    person_workspace::devices::mark_root_holder(devices.first.copies(), &root, cert)
        .await
        .expect("the root marks the device that keeps it");
    person_workspace::apply_on(
        devices.first.copies(),
        &person_workspace_id(&devices.person),
        DEVICES,
        Devices::KEEP_FACT.with((
            hex::encode(cert.device_public_key),
            hex::encode(postcard::to_allocvec(cert).expect("a certificate encodes")),
            "endpoint".into(),
            hex::encode(
                postcard::to_allocvec(&answers_at(&devices.first)).expect("addresses encode"),
            ),
        )),
    )
    .await
    .expect("the record says where the founding device answers");
    until("the desk's record names the founding device", || {
        marks_founding(
            &devices.second,
            devices.person,
            devices.first_cert.device_public_key,
        )
    })
    .await;
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

/// Wait for `done`, a moment at a time, within the hang bound.
async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done() {
        assert!(Instant::now() < deadline, "waited for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Whether the record `workspaces` holds marks `device` as keeping the person's root.
fn marks_founding(workspaces: &Arc<ServiceWorkspaces>, person: [u8; 32], device: [u8; 32]) -> bool {
    let copies = workspaces.copies().clone();
    crate::device::workspaces::block_on_owned(async move {
        person_workspace::devices::of(&copies, &person).await
    })
    .root_holders(&person)
    .contains(&device)
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

/// The one question waiting on the founding device in the person's own workspace, once it is
/// there.
async fn the_question(devices: &TwoDevices) -> tilo_remote_actions::asked::Waiting {
    let store = devices
        .first
        .copies()
        .asked_store()
        .expect("the founding device keeps the person's questions");
    let own = person_workspace_id(&devices.person);
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut waiting = store.waiting_in(&own);
        if let Some(question) = waiting.pop() {
            assert!(waiting.is_empty(), "one question waits: {waiting:?}");
            return question;
        }
        assert!(
            Instant::now() < deadline,
            "waited for the founding device to ask the person"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The person answers `question` with `offer` on the founding device.
async fn answer(
    devices: &TwoDevices,
    question: &tilo_remote_actions::asked::Waiting,
    offer: Offer,
) {
    let resolving = Resolve {
        resolution: offer,
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
        .in_workspace(person_workspace_id(&devices.person));
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
    mark_founding(&devices).await;

    // The person asks on the desk: the founding device makes it, and the desk hands its answer on
    // once it holds the workspace too, taken in from the person's list.
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
    assert!(
        opens(&devices.second, &id).await,
        "the desk lists the workspace it asked for and opens it"
    );
    assert_eq!(
        devices.second.copies().is_member(&id).await,
        Ok(true),
        "the desk holds it as the person's other devices hold theirs: a member copy"
    );
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
    let theirs = ServiceWorkspaces::open_for_test(
        &devices.state.path().join("stranger"),
        &key,
        &stranger,
        &tilo_server_client::ServerAddress::at(devices.first.endpoint().addr()),
    )
    .await
    .expect("the other person's device's service opens");
    let mut asked = serde_json::Map::new();
    asked.insert("name".into(), json!("Not yours"));
    let mut request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, CREATE),
        },
        asked,
    );
    request.wait_ms = Some(tilo_peer_state::actions::CHANGE_WAIT_MS);
    for workspace in [id.clone(), person_workspace_id(&devices.person)] {
        let remote = theirs.copies().action_service().remote(
            &theirs.endpoint(),
            &devices.first.endpoint().addr(),
            &workspace,
        );
        let caller = Caller::authenticated(stranger.user_public_key, stranger.device_public_key)
            .in_workspace(workspace.clone());
        let refused = tilo_test_env::within(
            "the founding device answers another person's device",
            remote.invoke(&caller, request.clone()),
        )
        .await
        .expect("the founding device answers");
        assert!(
            matches!(refused, Err(ActionCallError::NotPermitted)),
            "a device of another person is refused in {workspace}: {refused:?}"
        );
    }
    theirs.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_a_device_off_tilo_os_asks_for_waits_for_the_persons_yes_on_the_founding_device()
 {
    // The desk runs an operating system Tilo does not control: making a workspace reaches beyond
    // it, which the person's grants leave to them.
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = founding_and_desk().await;
    mark_founding(&devices).await;
    let own = WorkspaceId::parse(&person_workspace_id(&devices.person)).unwrap();

    // The person says yes on the founding device: it makes the workspace, and the desk opens it.
    let id = uuid::Uuid::new_v4().to_string();
    let asking = create_on_desk_meanwhile(&devices, json!({"name": "Quarterly plan", "id": id}));
    let question = the_question(&devices).await;
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
    assert!(
        opens(&devices.second, &id).await,
        "the desk lists the workspace it asked for and opens it"
    );

    // The person says no: nothing is made, and the desk says so in plain words.
    let declined_id = uuid::Uuid::new_v4().to_string();
    let asking =
        create_on_desk_meanwhile(&devices, json!({"name": "Not this one", "id": declined_id}));
    let question = the_question(&devices).await;
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
    let founding = FoundingCreate::new(&devices.first);
    let mut asked = serde_json::Map::new();
    asked.insert("name".into(), json!("For the agent"));
    let request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, CREATE),
        },
        asked,
    );
    let desk = devices.second_cert.device_public_key;
    let person = Caller::authenticated(devices.person, desk).in_workspace(own.to_string());
    let agent =
        Caller::delegated_agent("planner", devices.person, desk).in_workspace(own.to_string());
    let answered = |caller: Caller| {
        let (founding, request, certificate) = (
            Arc::clone(&founding),
            request.clone(),
            devices.second_cert.clone(),
        );
        async move {
            let call = WorkspaceCall {
                workspace: Some(own),
                caller: &caller,
                certificate: Some(&certificate),
            };
            founding.answer(call, &request).await
        }
    };
    assert!(
        matches!(
            answered(person).await,
            Err(ActionCallError::ConfirmationRequired(_))
        ),
        "the person is asked"
    );
    assert!(
        matches!(answered(agent).await, Err(ActionCallError::NotPermitted)),
        "an agent is refused"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_with_no_founding_device_in_reach_is_refused_in_plain_words_in_time() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = founding_and_desk().await;

    // While the record names no device that founds the person's workspaces, the desk says so,
    // and asks no one.
    let none = create_on_desk(
        Arc::clone(&devices.second),
        devices.second_cert.clone(),
        json!({"name": "Too early"}),
    )
    .await
    .expect_err("no device makes it");
    assert!(
        none.contains(tilo_api::account::NO_FOUNDING_DEVICE)
            && none.contains("none of your devices makes your workspaces"),
        "{none}"
    );

    // Once it does but is away, the person hears so within the bound, and nothing else is asked.
    mark_founding(&devices).await;
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
