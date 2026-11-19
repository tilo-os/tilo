//! Member copies: entering a workspace another device serves, what the
//! workspace's governance does to the copy, and how it is kept across a
//! restart.
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps the serving device to the end"
)]

use std::sync::Arc;

use super::super::PreAdmitRequest;
use super::super::WorkspaceSummary;
use super::super::fixtures::{TestHost, device_cert, governor, signed_pre_admission, test_host};
use super::MemberDevice;
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
async fn a_device_joins_a_workspace_its_invitation_names_and_holds_its_governance() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (_serving, device, created, invitation) = shared(serving_state.path(), "Shared", 11).await;
    let person = device.1.user_public_key;
    let member = member_service(member_state.path(), device).await;
    let entered = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    assert_eq!(entered.to_string(), created.workspace);
    assert!(
        member
            .workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap()
    );
    let state = member
        .workspaces()
        .state(&created.workspace)
        .await
        .expect("the member copy is held");
    assert_eq!(
        state.governance().workspace_name().as_deref(),
        Some("Shared")
    );
    // The surface this device's runtime reads its member copies by.
    let held = member.workspaces();
    assert!(held.member_ids().await.contains(&entered));
    let (name, copy) = held
        .member_copy(entered)
        .await
        .expect("the member copy is listed");
    assert_eq!(name, "Shared");
    assert_eq!(
        copy.governance().workspace_name().as_deref(),
        Some("Shared")
    );
    assert_eq!(held.member_actor(entered).await, Some(hex::encode(person)));
    assert!(
        held.member_key_holders(entered).await.is_some(),
        "the member copy's keepers are read from its governance"
    );
}

#[tokio::test]
async fn an_invitation_for_another_person_is_never_redeemed_here() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, _) = shared(serving_state.path(), "Elsewhere", 12).await;
    // The serving device invites somebody else, and this device presents that
    // invitation.
    let serving_cert = serving.host().device_cert().clone();
    let stranger = device_cert(
        &SigningKey::from_bytes(&[9; 32]),
        "phone",
        DeviceCapabilities::member(),
    );
    let issued = serving
        .workspaces()
        .pre_admit(
            &serving_cert,
            PreAdmitRequest {
                request_id: [13; 32],
                grant: signed_pre_admission(&created.workspace, b"stranger"),
                recipient_user_public_key: Some(stranger.1.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the serving device issues an invitation");
    let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);
    let member = member_service(member_state.path(), device).await;
    assert!(
        member.workspaces().join_member(&invitation).await.is_err(),
        "an invitation for another person is refused"
    );
    assert!(
        !member
            .workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_member_copy_reads_the_governance_the_serving_device_publishes() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Before", 14).await;
    let member = member_service(member_state.path(), device).await;
    member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    serving
        .workspaces()
        .rename(&created.workspace, "After")
        .await
        .expect("the serving device renames its workspace");
    let workspaces = member.workspaces().clone();
    let workspace = created.workspace.clone();
    tilo_test_env::within("the member reads the workspace's new name", async move {
        loop {
            let renamed = workspaces.state(&workspace).await.is_some_and(|state| {
                state.governance().workspace_name().as_deref() == Some("After")
            });
            if renamed {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
}

#[tokio::test]
async fn a_member_copy_is_held_as_one_when_the_service_opens_again() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (_serving, device, created, invitation) = shared(serving_state.path(), "Kept", 15).await;
    {
        let member = member_service(member_state.path(), device.clone()).await;
        member
            .workspaces()
            .join_member(&invitation)
            .await
            .expect("the invitation is redeemed");
        member.shutdown().await;
    }
    let reopened = member_service(member_state.path(), device).await;
    assert!(
        reopened
            .workspaces()
            .is_member(&created.workspace)
            .await
            .unwrap(),
        "the service keeps the workspace as a member copy"
    );
}

#[tokio::test]
async fn a_members_write_reaches_the_device_serving_the_workspace() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Working", 16).await;
    let member_key = device.1.device_public_key;
    let member = member_service(member_state.path(), device).await;
    member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    let write = member
        .workspaces()
        .member_membership_write(
            &created.workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Lease {
                id: "machine".into(),
                ttl_ms: 60_000,
                rank: tilo_peer_state::unranked(),
            },
        )
        .await
        .expect("the lease is written");
    member
        .workspaces()
        .commit_member_document(&created.workspace, write)
        .await
        .expect("the write is kept");
    let workspaces = serving.workspaces().clone();
    let workspace = created.workspace.clone();
    tilo_test_env::within("the serving device reads the member's write", async move {
        loop {
            let holder = workspaces.state(&workspace).await.and_then(|state| {
                state
                    .workspace()
                    .lease("machine", tilo_clock::wall())
                    .holder
            });
            if holder.is_some_and(|claim| claim.device == member_key) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// The front `workspace`'s layout stands on, as its member copy reads it now.
#[cfg(feature = "apps")]
async fn front(copies: &super::Workspaces, workspace: &str) -> Option<String> {
    use crate::workspace_documents::DocumentRead;
    use automerge::ReadDoc as _;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};

    let read = copies
        .own_read(
            workspace,
            (DESKTOP_APP, DESKTOP_DOCUMENT),
            &[],
            64 * 1024 * 1024,
        )
        .await
        .expect("the layout reads");
    let DocumentRead::Whole { bytes, .. } = read else {
        return None;
    };
    let document = automerge::AutoCommit::load(&bytes).expect("the layout opens");
    match document
        .get(automerge::ROOT, "front")
        .expect("the layout's front")
    {
        Some((automerge::Value::Scalar(value), _)) => value.as_str().map(str::to_owned),
        _ => None,
    }
}

/// Wait until the member copy reads `want` as its layout's front: the write comes back to the copy
/// from the serving device, as a member's own write does.
#[cfg(feature = "apps")]
async fn waiting_front(copies: &super::Workspaces, workspace: &str, want: &str) {
    tilo_test_env::within("the member copy reads the layout it wrote", async {
        loop {
            if front(copies, workspace).await.as_deref() == Some(want) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A member copy's own layout write reaches the workspace and reads back at once, a later change
/// builds on it, and the copy keeps both across a restart of this device's service: what a joined
/// workspace's window positions go through once the session holds it as a member copy.
#[cfg(feature = "apps")]
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one join, the founding write, a change of it and the restart that reads it back"
)]
async fn a_member_copies_layout_write_is_read_back_and_survives_a_restart() {
    use automerge::transaction::Transactable as _;
    use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};

    use super::super::fixtures::{TestGroupKeys, test_host_holding};

    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let keys = Arc::new(TestGroupKeys::default());
    let (serving_key, serving_cert) = governor();
    let serving = test_host_holding(
        serving_state.path(),
        &serving_key,
        &serving_cert,
        Arc::clone(&keys),
    )
    .await;
    let created = serving
        .workspaces()
        .create("Layout")
        .await
        .expect("a workspace is created");
    let workspace = created.workspace.clone();
    let (key, cert) = device_cert(
        &SigningKey::from_bytes(&[21; 32]),
        "laptop",
        DeviceCapabilities::member(),
    );
    let issued = serving
        .workspaces()
        .pre_admit(
            &serving_cert,
            PreAdmitRequest {
                request_id: [21; 32],
                grant: signed_pre_admission(&workspace, &[21; 8]),
                recipient_user_public_key: Some(cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .expect("the serving device issues an invitation");
    let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);

    // This device's service acts as this device, as a desktop's does, and takes part in the
    // workspace's groups with the keys its key service hands it.
    let member = test_host_holding(member_state.path(), &key, &cert, Arc::clone(&keys)).await;
    let copies = member.workspaces().clone();
    copies.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    assert_eq!(
        copies
            .join_member(&invitation)
            .await
            .expect("the invitation is redeemed")
            .to_string(),
        workspace
    );
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: DESKTOP_APP.into(),
        cluster_id: workspace.clone(),
        document: DESKTOP_DOCUMENT.into(),
    };
    let sealed = copies
        .own_route_keys(&route, true)
        .await
        .expect("the device's key service answers")
        .expect("the group opens");

    let mut layout = automerge::AutoCommit::new();
    layout
        .put(automerge::ROOT, "front", "Main")
        .expect("the layout's front");
    copies
        .own_write(
            &workspace,
            (DESKTOP_APP, DESKTOP_DOCUMENT),
            &layout.save(),
            true,
            Some(&sealed),
        )
        .await
        .expect("the layout is written");
    waiting_front(&copies, &workspace, "Main").await;
    let heads = layout.get_heads();
    layout
        .put(automerge::ROOT, "front", "Moved")
        .expect("the layout's front moves");
    copies
        .own_write(
            &workspace,
            (DESKTOP_APP, DESKTOP_DOCUMENT),
            &layout.save_after(&heads),
            false,
            Some(&sealed),
        )
        .await
        .expect("the change is written");
    waiting_front(&copies, &workspace, "Moved").await;

    // The service after a restart reads the copy back.
    drop(copies);
    member.shutdown().await;
    let member = test_host_holding(member_state.path(), &key, &cert, keys).await;
    let copies = member.workspaces().clone();
    copies.set_member_device(Arc::new(MemberDevice {
        cert,
        signer: Arc::new(key),
    }));
    waiting_front(&copies, &workspace, "Moved").await;
    drop(copies);
    member.shutdown().await;
}
