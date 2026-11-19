//! Runs admitted to a workspace are pinned to the material they were admitted
//! under, and a member copy enrolled again ends them.
#![allow(
    clippy::unwrap_used,
    clippy::significant_drop_tightening,
    reason = "in a test this is the assertion, and each fixture is kept for its test"
)]

use std::sync::Arc;

use super::super::MemberDevice;
use super::super::PreAdmitRequest;
use super::super::WorkspaceSummary;
use super::super::fixtures::{TestHost, device_cert, governor, signed_pre_admission, test_host};
use super::Refusal;
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// A device serving workspace `name`, the device it invited, and the
/// invitation that device redeems.
async fn shared(
    serving_state: &std::path::Path,
    name: &str,
    seed: u8,
) -> (TestHost, (SigningKey, DeviceCert), WorkspaceSummary, String) {
    let (serving_key, serving_cert) = governor();
    let serving = test_host(serving_state, &serving_key, &serving_cert).await;
    let created = serving
        .workspaces()
        .create(name)
        .await
        .expect("a workspace is created");
    let device = device_cert(
        &SigningKey::from_bytes(&[seed; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let issued = serving
        .workspaces()
        .pre_admit(
            &serving_cert,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(&created.workspace, &[seed; 8]),
                recipient_user_public_key: Some(device.1.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the serving device issues an invitation");
    let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);
    (serving, device, created, invitation)
}

/// A service on `state`, serving the member copies of `(key, cert)`.
async fn member_service(
    state: &std::path::Path,
    (key, cert): (SigningKey, DeviceCert),
) -> TestHost {
    let service = test_host(state, &key, &cert).await;
    service
        .workspaces()
        .set_member_device(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key),
        }));
    service
}

#[tokio::test]
async fn a_run_admitted_to_a_member_copy_ends_when_the_copy_is_enrolled_again() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Run", 21).await;
    let member = member_service(member_state.path(), device.clone()).await;
    let workspace = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    let actor = member
        .workspaces()
        .member_actor(workspace)
        .await
        .expect("the device acts for a person");
    let admission = member
        .workspaces()
        .admit_execution(&workspace.to_string(), &actor, &device.1, Vec::new())
        .await
        .expect("the run is admitted");
    admission
        .check(member.workspaces())
        .expect("the run's material holds");
    // Another invitation redeemed enrolls the copy anew: the run admitted
    // under the enrollment before it is over.
    let again = {
        let serving_cert = serving.host().device_cert().clone();
        let issued = serving
            .workspaces()
            .pre_admit(
                &serving_cert,
                PreAdmitRequest {
                    request_id: [23; 32],
                    grant: signed_pre_admission(&created.workspace, b"enrolled again"),
                    recipient_user_public_key: Some(device.1.user_public_key),
                    inviter_name: None,
                },
            )
            .await
            .expect("the serving device issues another invitation");
        tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation)
    };
    member
        .workspaces()
        .join_member(&again)
        .await
        .expect("the copy is enrolled again");
    assert_eq!(
        admission.check(member.workspaces()),
        Err(Refusal::Ended),
        "a run admitted under another enrollment ends"
    );
}

#[tokio::test]
async fn a_run_admitted_for_another_person_is_refused() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (_serving, device, _created, invitation) = shared(serving_state.path(), "Strict", 22).await;
    let member = member_service(member_state.path(), device.clone()).await;
    let workspace = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    assert!(
        member
            .workspaces()
            .admit_execution(
                &workspace.to_string(),
                &hex::encode([7; 32]),
                &device.1,
                Vec::new(),
            )
            .await
            .is_err(),
        "a run is admitted only as the person the device acts for"
    );
}

#[tokio::test]
async fn a_workspace_this_device_governs_admits_a_run_of_its_own_device() {
    let state = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host(state.path(), &key, &cert).await;
    let created = host
        .workspaces()
        .create("Mine")
        .await
        .expect("a workspace is created");
    let actor = hex::encode(cert.user_public_key);
    let admission = host
        .workspaces()
        .admit_execution(&created.workspace, &actor, &cert, Vec::new())
        .await
        .expect("the workspace's own device is admitted");
    admission
        .check(host.workspaces())
        .expect("the run's material holds");
}
