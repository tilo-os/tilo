//! A returning browser's device and the workspace's door: what the workspace refuses
//! of a device it knows, and whom it admits instead.
#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use super::tests::{governor, signed_pre_admission, test_host};
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

/// Ask a pre-admitted join of `device` under `device_cert` with a fresh
/// pre-admission for its person, as a browser coming through the door does.
async fn pre_admitted_join(
    workspaces: &Workspaces,
    cert: &DeviceCert,
    workspace: &str,
    device: &SigningKey,
    device_cert: DeviceCert,
) -> tilo_relay_protocol::workspaces::JoinReply {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::workspaces::{JoinReply, JoinStep, join_proof_digest};
    let token: [u8; 8] = rand::random();
    let issued = workspaces
        .pre_admit(
            cert,
            PreAdmitRequest {
                request_id: rand::random(),
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let JoinReply::Challenge { challenge } = workspaces
        .page_join(JoinStep::Ask {
            invitation: issued.signed_invitation,
            device_cert,
            request_id: rand::random(),
        })
        .await
    else {
        panic!("no challenge");
    };
    let signature = device.sign(&join_proof_digest(&challenge)).to_bytes();
    workspaces
        .page_join(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: "Returning".into(),
            token: token.to_vec(),
        })
        .await
}

/// A browser whose person issues its device key a certificate again joins on
/// it: a later certificate that grants no more than the one the workspace
/// holds is the same device and replaces it. One that would grant it more, or
/// one issued before the held one, is refused as a conflict, and the same
/// person on a new device key is admitted. The browser relies on it
/// (`workspaces.mjs` `joinViaHost`).
#[tokio::test]
async fn a_later_certificate_of_a_device_replaces_it_and_an_escalating_or_older_one_is_refused() {
    use tilo_relay_protocol::workspaces::JoinReply;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let root = SigningKey::from_bytes(&rand::random());
    let certify = |device: &SigningKey, issued_at_ns: u64, capabilities: DeviceCapabilities| {
        let mut device_cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            capabilities,
            issued_at_ns,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut device_cert, &root).unwrap();
        device_cert
    };
    let device = SigningKey::from_bytes(&rand::random());
    let first = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        &device,
        certify(&device, 1, DeviceCapabilities::member()),
    )
    .await;
    assert!(matches!(first, JoinReply::Admitted { .. }), "{first:?}");

    // The same key certified later with more than the workspace holds: the
    // device would become more than it was admitted as.
    let escalated = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        &device,
        certify(&device, 2, DeviceCapabilities::primary()),
    )
    .await;
    assert!(
        matches!(&escalated, JoinReply::Refused(reason) if reason.contains("certificate conflict")),
        "{escalated:?}"
    );

    // The same key certified again by the same person, granting no more.
    let again = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        &device,
        certify(&device, 3, DeviceCapabilities::member()),
    )
    .await;
    assert!(matches!(again, JoinReply::Admitted { .. }), "{again:?}");

    // A certificate older than the one the workspace now holds.
    let older = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        &device,
        certify(&device, 2, DeviceCapabilities::member()),
    )
    .await;
    assert!(
        matches!(&older, JoinReply::Refused(reason) if reason.contains("certificate conflict")),
        "{older:?}"
    );

    let renewed = SigningKey::from_bytes(&rand::random());
    let fresh = pre_admitted_join(
        workspaces,
        &cert,
        &created.workspace,
        &renewed,
        certify(&renewed, 4, DeviceCapabilities::member()),
    )
    .await;
    assert!(matches!(fresh, JoinReply::Admitted { .. }), "{fresh:?}");
    let members = workspaces
        .summary(&created.workspace)
        .await
        .unwrap()
        .members;
    assert_eq!(
        members
            .iter()
            .filter(|member| member.user_public_key == root.verifying_key().to_bytes())
            .count(),
        1,
        "one person, whatever devices they came with"
    );
    relay.shutdown().await;
}
