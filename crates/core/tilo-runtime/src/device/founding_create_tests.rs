//! A workspace asked for on a device of the person's that founds none, made on the one that does,
//! over the action service both serve: asked through the account the device answers its person's
//! programs with, refused to anyone else, and refused in plain words, in time, when the founding
//! device is away.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace;
use tilo_workspace_service::workspaces::person_workspace::devices::{DEVICES, Devices};

use super::*;
use crate::device::clipboard::tests::{TwoDevices, answers_at, two_devices};
use crate::device::ticket_tests;

/// The second device's key, as the fixture made it: what its programs' API is served as.
const DESK_KEY: [u8; 32] = [0x73; 32];

/// The desk, a device of the person's that founds none of their workspaces, as its programs'
/// account reaches it: it keeps none, and asks the person's founding device in `workspace`, which
/// both keep, as the daemon asks it in the person's own.
struct Desk {
    workspaces: Arc<ServiceWorkspaces>,
    workspace: String,
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
        Box::pin(ask(&self.workspaces, &self.workspace, creating))
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

/// The two devices, each holding the person's own workspace: the first keeps the person's root
/// and makes their workspaces here, and its record lists the second.
async fn founding_and_desk() -> TwoDevices {
    let devices = two_devices().await;
    for workspaces in [&devices.first, &devices.second] {
        person_workspace::found(workspaces.copies())
            .await
            .expect("the person's own workspace");
    }
    devices.first.offer_workspaces_here("Laptop");
    keep(&devices.first, &devices.second_cert).await;
    FoundingCreate::new(&devices.first).register(&devices.first.copies().providers());
    devices
}

/// Mark the first device in the second's record as the one keeping the person's root, signed
/// by that root, and say where it answers.
async fn mark_founding(devices: &TwoDevices) {
    let root = ticket_tests::root_in(devices.state.path().to_path_buf())()
        .expect("the root reads")
        .expect("the issuer keeps the person's root");
    person_workspace::devices::mark_root_holder(
        devices.second.copies(),
        &root,
        &devices.first_cert,
    )
    .await
    .expect("the root marks the device that keeps it");
    let cert = &devices.first_cert;
    let own = tilo_server_protocol::servers::person_workspace_id(&devices.person);
    person_workspace::apply_on(
        devices.second.copies(),
        &own,
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
}

/// The desk's account, as its person's programs reach it.
fn desk_account(devices: &TwoDevices) -> tilo_api::direct::Direct {
    tilo_api::direct::Direct::here(
        Arc::new(Desk {
            workspaces: Arc::clone(&devices.second),
            workspace: devices.workspace.clone(),
        }) as Arc<dyn tilo_api::session::Here>,
        SigningKey::from_bytes(&DESK_KEY),
        devices.second_cert.clone(),
        "Desk",
    )
    .expect("the desk's API starts")
}

/// `account/workspaces#create` with `arguments`, as the person asks it on the desk: what it
/// answered, or how it refused.
async fn create_on_desk(devices: &TwoDevices, arguments: Value) -> Result<Value, String> {
    let direct = desk_account(devices);
    let tree = tilo_test_env::within("the desk's account answers", direct.account_tree())
        .await
        .expect("the account's own tree answers");
    let caller = Caller::authenticated(devices.person, devices.second_cert.device_public_key);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_asked_for_on_a_device_that_founds_none_is_made_on_the_one_that_does() {
    // The desk runs Tilo OS: its person's root certifies it as one of their own, whose actions
    // beyond content need no other device's yes.
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    home.env().set(
        tilo_identity::provisioning::TEST_PLATFORM_ENV,
        tilo_identity::provisioning::TILO_OS_PLATFORM,
    );
    let devices = founding_and_desk().await;
    mark_founding(&devices).await;
    until("the desk's record names the founding device", || {
        marks_founding(
            &devices.second,
            devices.person,
            devices.first_cert.device_public_key,
        )
    })
    .await;

    // The person asks on the desk: the founding device makes it, and the desk hands its answer on.
    let id = uuid::Uuid::new_v4().to_string();
    let made = create_on_desk(&devices, json!({"name": "Applicant: Jane Doe", "id": id}))
        .await
        .expect("the founding device makes the workspace");
    assert_eq!(made["workspace"]["id"], id, "{made}");
    assert_eq!(made["workspace"]["name"], "Applicant: Jane Doe", "{made}");
    assert!(
        devices
            .first
            .copies()
            .list()
            .await
            .iter()
            .any(|summary| summary.workspace == id),
        "the founding device keeps the workspace it made"
    );
    assert!(
        !devices
            .second
            .copies()
            .list()
            .await
            .iter()
            .any(|summary| summary.workspace == id),
        "the desk made nothing itself"
    );

    // A device of another person that takes part in a workspace the founding device keeps has
    // nothing made there: it is no device of this person's.
    let stranger = Caller::authenticated([9; 32], [8; 32]).in_workspace(devices.workspace.clone());
    let founding = FoundingCreate::new(&devices.first);
    let mut asked = serde_json::Map::new();
    asked.insert("name".into(), json!("Not yours"));
    let refused = founding
        .invoke(
            &stranger,
            InvokeRequest::plain(
                RemoteActionTarget {
                    provider: PROVIDER.to_owned(),
                    address: tilo_actions::ActionAddress::new(RESOURCE, CREATE),
                },
                asked,
            ),
        )
        .await
        .expect("the founding device answers");
    assert!(
        matches!(refused, Err(ActionCallError::NotPermitted)),
        "a device of another person is refused: {refused:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_with_no_founding_device_in_reach_is_refused_in_plain_words_in_time() {
    let _home = tilo_test_env::TestHome::new().expect("a home");
    let devices = founding_and_desk().await;

    // While the record names no device that founds the person's workspaces, the desk says so,
    // and asks no one.
    let none = create_on_desk(&devices, json!({"name": "Too early"}))
        .await
        .expect_err("no device makes it");
    assert!(
        none.contains(tilo_api::account::NO_FOUNDING_DEVICE)
            && none.contains("none of your devices makes your workspaces"),
        "{none}"
    );

    // Once it does but is away, the person hears so within the bound, and nothing else is asked.
    mark_founding(&devices).await;
    until("the desk's record names the founding device", || {
        marks_founding(
            &devices.second,
            devices.person,
            devices.first_cert.device_public_key,
        )
    })
    .await;
    devices
        .first_router
        .shutdown()
        .await
        .expect("the founding device stops answering");
    let asked = Instant::now();
    let away = create_on_desk(&devices, json!({"name": "While away"}))
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
