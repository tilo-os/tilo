use super::*;
use crate::agent_message_commit::tests::{add_agent, publisher, source};

fn governed_reader(
    fixture: &crate::application_record::tests::Fixture,
    revoke: bool,
) -> DeviceCert {
    let authority_key = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
    let key = ed25519_dalek::SigningKey::from_bytes(&[114; 32]);
    let reader = crate::testing::certificate(&authority_key, &key, "Reader");
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let messages = fixture
        .owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            if revoke {
                let mut entry = tilo_user_identity::RevocationEntry {
                    user_public_key: reader.user_public_key,
                    revoked_device_public_key: reader.device_public_key,
                    revoked_at_ns: crate::testing::ns(fixture.now).unwrap(),
                    reason: tilo_user_identity::RevocationReason::Retired,
                    signature: [0; 64],
                };
                tilo_user_identity::sign_revocation_entry(&mut entry, &authority_key).unwrap();
                candidate.publish_governed_revocations(&[entry], &fixture.key, fixture.now)?;
            } else {
                candidate.admit_certificate(reader.clone(), &fixture.key, fixture.now)?;
            }
            candidate.durable_mutation_messages(
                &before,
                &fixture.key,
                fixture.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        fixture
            .owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    fixture.certificate.device_public_key,
                ),
                message,
                fixture.now,
            )
            .unwrap();
    }
    reader
}

#[test]
fn retired_actual_reader_certificate_cannot_observe_receipt_or_absence() {
    let fixture = fixture();
    let reader = governed_reader(&fixture, false);
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let original = uuid::Uuid::from_u128(1104);
    let row = format!("m_{}", original.simple());
    assert!(
        fixture
            .owner
            .agent_message_public_progress(room, &reader, original, &row, fixture.now)
            .unwrap()
            .is_none()
    );
    governed_reader(&fixture, true);
    assert!(
        fixture
            .owner
            .agent_message_public_progress(room, &reader, original, &row, fixture.now)
            .is_err()
    );
    assert!(
        fixture
            .owner
            .agent_message_public_progress(room, &fixture.certificate, original, &row, fixture.now)
            .unwrap()
            .is_none(),
        "retiring another reader does not poison the owner or unaffected Person Read"
    );
}
use crate::application_record::tests::{fixture, fixture_with_expiry};

#[test]
fn signed_awaiting_projection_is_read_only_and_absence_is_authoritative() {
    let fixture = fixture();
    add_agent(&fixture);
    let (request, proof) = source(&fixture, 0);
    let original = proof.data.invocation;
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    fixture
        .owner
        .commit_agent_messages(request, &[proof], &publisher(&fixture), fixture.now)
        .unwrap();
    let before = fixture
        .owner
        .with_current(room, |view| Ok((view.generation(), view.peer().durable()?)))
        .unwrap();
    let progress = fixture
        .owner
        .agent_message_public_progress(
            room,
            &fixture.certificate,
            original,
            &format!("m_{}", original.simple()),
            fixture.now,
        )
        .unwrap()
        .unwrap();
    assert_eq!(progress.phase, AgentMessagePublicPhase::WaitingForReply);
    assert_eq!(progress.generation, before.0);
    let other = uuid::Uuid::from_u128(1101);
    assert!(
        fixture
            .owner
            .agent_message_public_progress(
                room,
                &fixture.certificate,
                other,
                &format!("m_{}", other.simple()),
                fixture.now
            )
            .unwrap()
            .is_none()
    );
    fixture
        .owner
        .with_current(room, |view| {
            assert_eq!((view.generation(), view.peer().durable()?), before);
            Ok(())
        })
        .unwrap();
}

#[test]
fn public_projection_refuses_foreign_room_row_and_expired_current_read() {
    let fixture = fixture_with_expiry(Some(std::time::Duration::from_secs(2)));
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let id = uuid::Uuid::from_u128(1102);
    let row = format!("m_{}", id.simple());
    assert!(
        fixture
            .owner
            .agent_message_public_progress(
                RoomId::parse(&uuid::Uuid::from_u128(1103).to_string()).unwrap(),
                &fixture.certificate,
                id,
                &row,
                fixture.now
            )
            .is_err()
    );
    assert!(
        fixture
            .owner
            .agent_message_public_progress(
                room,
                &fixture.certificate,
                id,
                "another-row",
                fixture.now
            )
            .is_err()
    );
    assert!(
        fixture
            .owner
            .agent_message_public_progress(room, &fixture.certificate, id, &row, fixture.now)
            .unwrap()
            .is_none()
    );
    let later = fixture
        .now
        .checked_add(std::time::Duration::from_secs(3))
        .unwrap();
    assert!(
        fixture
            .owner
            .agent_message_public_progress(room, &fixture.certificate, id, &row, later)
            .is_err(),
        "expired current Read must not expose even absence"
    );
}
