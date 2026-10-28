//! A returning browser's device and the room's door: what the room refuses
//! of a device it knows, and whom it admits instead.
use super::tests::{governor, signed_pre_admission, test_host};
use super::*;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

/// Ask a pre-admitted join of `device` under `device_cert` with a fresh
/// pre-admission for its person, as a browser coming through the door does.
async fn pre_admitted_join(
    rooms: &Rooms,
    cert: &DeviceCert,
    room: &str,
    device: &SigningKey,
    device_cert: DeviceCert,
) -> tilo_relay_protocol::rooms::JoinReply {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::rooms::{JoinReply, JoinStep, join_proof_digest};
    let token: [u8; 8] = rand::random();
    let issued = rooms
        .pre_admit(
            cert,
            PreAdmitRequest {
                request_id: rand::random(),
                grant: signed_pre_admission(room, &token),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let JoinReply::Challenge { challenge } = rooms
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
    rooms
        .page_join(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: "Returning".into(),
            token: token.to_vec(),
        })
        .await
}

/// A browser that lost the certificate its device key was admitted under
/// certifies that key anew: the room, which keeps every device's
/// certificate, refuses the fresh pre-admission naming the conflict, and
/// admits the same person on a new device key. The browser relies on both
/// (`workspaces.mjs` `joinViaHost`).
#[tokio::test]
async fn a_device_known_under_another_certificate_is_refused_and_a_new_device_of_the_person_admitted()
 {
    use tilo_relay_protocol::rooms::JoinReply;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let rooms = relay.rooms();
    let created = rooms.create("Lounge").await.unwrap();
    let root = SigningKey::from_bytes(&rand::random());
    let certify = |device: &SigningKey, issued_at_ns: u64| {
        let mut device_cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Browser".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut device_cert, &root).unwrap();
        device_cert
    };
    let device = SigningKey::from_bytes(&rand::random());
    let first = pre_admitted_join(rooms, &cert, &created.room, &device, certify(&device, 1)).await;
    assert!(matches!(first, JoinReply::Admitted { .. }), "{first:?}");

    // The same key, certified again by the same person.
    let again = pre_admitted_join(rooms, &cert, &created.room, &device, certify(&device, 2)).await;
    assert!(
        matches!(&again, JoinReply::Refused(reason) if reason.contains("certificate conflict")),
        "{again:?}"
    );

    let renewed = SigningKey::from_bytes(&rand::random());
    let fresh =
        pre_admitted_join(rooms, &cert, &created.room, &renewed, certify(&renewed, 3)).await;
    assert!(matches!(fresh, JoinReply::Admitted { .. }), "{fresh:?}");
    let members = rooms.summary(&created.room).await.unwrap().members;
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
