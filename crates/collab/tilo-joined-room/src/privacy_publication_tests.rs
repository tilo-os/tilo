//! Publication must never fall back to pre-migration retained room history.
use crate::{RoomId, open_file, publication, testing};
use ed25519_dalek::SigningKey;
use neighbour_wire::{NeighbourMsg, Retention};
use std::time::SystemTime;

fn governor() -> (
    tempfile::TempDir,
    crate::NativeWorkspaceOwner,
    SigningKey,
    RoomId,
) {
    let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
    let owner = open_file(&directory.path().join("rooms.json")).unwrap();
    let key = SigningKey::from_bytes(&[61; 32]);
    let certificate = testing::certificate(&SigningKey::from_bytes(&[62; 32]), &key, "Governor");
    testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
    (directory, owner, key, RoomId::parse(testing::ROOM).unwrap())
}

#[test]
fn reconcile_migrates_before_any_private_command_and_releases_only_exact_baseline() {
    let (directory, owner, key, room) = governor();
    let now = SystemTime::now();
    assert!(owner.with_outgoing(room, now, |_, _| Ok(())).is_err());
    owner.reconcile_outgoing(room, &key, now).unwrap();
    let reopened = open_file(&directory.path().join("rooms.json")).unwrap();
    reopened
        .with_outgoing(room, now, |view, outgoing| {
            assert!(view.record.sanitized_baseline.is_some());
            let mut checkpoints = 0;
            for entry in outgoing.entries() {
                if entry.retention() == Retention::Checkpoint {
                    checkpoints += 1;
                    publication::authorize_baseline_checkpoint(
                        view,
                        entry.bytes(),
                        entry.channel(),
                    )?;
                    let mut corrupt = entry.bytes().to_vec();
                    corrupt.push(0);
                    assert!(
                        publication::authorize_baseline_checkpoint(view, &corrupt, entry.channel())
                            .is_err()
                    );
                    assert!(
                        publication::authorize_baseline_checkpoint(view, entry.bytes(), [0; 32])
                            .is_err()
                    );
                    let mut member = view.clone();
                    member.record.device.device_public_key = [7; 32];
                    assert!(
                        publication::authorize_baseline_checkpoint(
                            &member,
                            entry.bytes(),
                            entry.channel()
                        )
                        .is_err()
                    );
                } else {
                    assert!(
                        publication::authorize_baseline_checkpoint(
                            view,
                            entry.bytes(),
                            entry.channel()
                        )
                        .is_err()
                    );
                }
            }
            assert_eq!(checkpoints, 1);
            publication::validate(view)
        })
        .unwrap();
}

#[test]
fn failed_publication_migration_cannot_release_predecessor_bytes() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let (directory, owner, key, room) = governor();
    let now = SystemTime::now();
    let before = owner
        .with_current(room, |view| view.peer().durable())
        .unwrap();
    assert!(
        with_failure(Checkpoint::BeforePublish, || owner
            .reconcile_outgoing(room, &key, now))
        .is_err()
    );
    let reopened = open_file(&directory.path().join("rooms.json")).unwrap();
    for current in [&owner, &reopened] {
        current
            .with_current(room, |view| {
                assert!(view.record.sanitized_baseline.is_none());
                assert_eq!(view.peer().durable()?, before);
                Ok(())
            })
            .unwrap();
        assert!(current.with_outgoing(room, now, |_, _| Ok(())).is_err());
    }
}

#[test]
fn legacy_member_requires_independently_installed_baseline() {
    let directory = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
    let owner = open_file(&directory.path().join("rooms.json")).unwrap();
    let (intent, bootstrap, messages, governor, now) = testing::raw_fixture();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    owner
        .install(&testing::finalize(intent, pending, bootstrap).unwrap(), now)
        .unwrap();
    let room = RoomId::parse(testing::ROOM).unwrap();
    for message in messages.into_iter().skip(1) {
        owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(governor),
                message,
                now,
            )
            .unwrap();
    }
    let member = SigningKey::from_bytes(&[63; 32]);
    assert!(owner.reconcile_outgoing(room, &member, now).is_err());
    assert!(owner.with_outgoing(room, now, |_, _| Ok(())).is_err());
    owner
        .with_current(room, |view| {
            assert!(view.record.sanitized_baseline.is_none());
            assert!(matches!(
                view.peer().room_message(),
                NeighbourMsg::SyncResp { .. }
            ));
            Ok(())
        })
        .unwrap();
}
