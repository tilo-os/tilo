use super::*;
use crate::SessionOwner;
use qos_action_broker::collaboration_admin::{
    MediaCallInput, MediaCreateInput, MediaDenialInput, MediaDirectionsChangeInput,
    MediaDirectionsInput, MediaRejectionCode,
};
use qos_peer_state::room::media::{self, CallPhaseV1};
use ui::automerge::{AutoCommit, ObjType, ROOT, ReadDoc, transaction::Transactable};

fn fixture() -> (tempfile::TempDir, SessionOwner) {
    let temp = tempfile::tempdir().unwrap();
    let mut owner = SessionOwner::initialize(&temp.path().join("owner")).unwrap();
    owner.activate_delivery().unwrap();
    let mut candidate = owner.peer().clone();
    let mut chat = AutoCommit::load(&candidate.document("so.tilo.chat", "").unwrap()).unwrap();
    let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
    let voice = chat.put_object(&channels, "voice", ObjType::Map).unwrap();
    chat.put(&voice, "kind", "voice").unwrap();
    chat.put(&voice, "workspace", "home").unwrap();
    candidate
        .publish_document(
            "so.tilo.chat",
            "",
            chat.save(),
            &owner.identity.native,
            SystemTime::now(),
        )
        .unwrap();
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
    (temp, owner)
}
fn create(revision: Option<u64>) -> Operation {
    Operation::MediaCreate(MediaCreateInput {
        app_id: "so.tilo.chat".into(),
        record_id: String::new(),
        channel_id: "voice".into(),
        expected_authorization_revision: revision,
        allowed: MediaDirectionsInput {
            publish: true,
            subscribe: true,
        },
    })
}
fn input(result: &MediaCallResultV1) -> MediaCallInput {
    MediaCallInput {
        call_id: hex::encode(result.call_id),
        expected_authorization_revision: result.authorization_revision,
        expected_epoch: result.epoch,
    }
}
fn establish(owner: &mut SessionOwner) -> MediaCallResultV1 {
    let (candidate, result) = owner
        .prepare_media_transition(&create(None), SystemTime::now())
        .unwrap();
    owner
        .commit_mutation_candidate(candidate, owner.directory().to_vec())
        .unwrap();
    result
}

#[test]
fn neutral_create_has_exact_binding_without_committing_or_authorizing_an_owner() {
    let (_temp, owner) = fixture();
    let before = owner.peer().durable().unwrap();
    let generation = owner.committed_generation();
    let operation = create(None);
    let now = SystemTime::now();
    let (_, native) = owner.prepare_media_transition(&operation, now).unwrap();
    let (candidate, result) = prepare_media_transition(
        owner.peer().clone(),
        &owner.identity.native,
        &operation,
        now,
    )
    .unwrap();
    assert_ne!(result.call_id, [0; 32]);
    assert_eq!(result.binding, native.binding);
    assert_eq!(
        (result.epoch, result.authorization_revision, result.phase),
        (1, 1, CallPhaseV1::Active)
    );
    assert_eq!(
        candidate.validated_media_policy().unwrap().unwrap().calls[&result.call_id].binding,
        result.binding
    );
    assert_eq!(owner.peer().durable().unwrap(), before);
    assert_eq!(owner.committed_generation(), generation);
    assert!(owner.peer().validated_media_policy().unwrap().is_none());
    let wrong_key = SigningKey::from_bytes(&[211; 32]);
    assert!(prepare_media_transition(owner.peer().clone(), &wrong_key, &operation, now).is_err());
    assert_eq!(owner.peer().durable().unwrap(), before);
}

#[test]
fn native_and_neutral_moderation_lifecycle_and_noop_bytes_match() {
    let (_temp, mut owner) = fixture();
    let mut result = establish(&mut owner);
    for (step, expected_epoch, expected_phase, noop) in [
        (0, 1, CallPhaseV1::Active, true),
        (1, 2, CallPhaseV1::Active, false),
        (1, 2, CallPhaseV1::Active, true),
        (2, 3, CallPhaseV1::Active, false),
        (2, 3, CallPhaseV1::Active, true),
        (3, 4, CallPhaseV1::Active, false),
        (3, 4, CallPhaseV1::Active, true),
        (4, 5, CallPhaseV1::Ended, false),
        (5, 6, CallPhaseV1::Active, false),
    ] {
        let call = input(&result);
        let operation = match step {
            0 | 1 => Operation::MediaSetDirections(MediaDirectionsChangeInput {
                call_id: call.call_id,
                expected_authorization_revision: call.expected_authorization_revision,
                expected_epoch: call.expected_epoch,
                allowed: MediaDirectionsInput {
                    publish: step == 0,
                    subscribe: true,
                },
            }),
            2 | 3 => Operation::MediaSetParticipantDenial(MediaDenialInput {
                call_id: call.call_id,
                expected_authorization_revision: call.expected_authorization_revision,
                expected_epoch: call.expected_epoch,
                person_root: hex::encode([3; 32]),
                denied: MediaDirectionsInput {
                    publish: step == 2,
                    subscribe: false,
                },
            }),
            4 => Operation::MediaEnd(call),
            5 => Operation::MediaRestart(call),
            _ => unreachable!(),
        };
        let before = owner.peer().durable().unwrap();
        let now = SystemTime::now();
        let (native, native_result) = owner.prepare_media_transition(&operation, now).unwrap();
        let (neutral, neutral_result) = prepare_media_transition(
            owner.peer().clone(),
            &owner.identity.native,
            &operation,
            now,
        )
        .unwrap();
        assert_eq!(native_result, neutral_result);
        assert_eq!(
            (
                neutral_result.epoch,
                neutral_result.authorization_revision,
                neutral_result.phase
            ),
            (expected_epoch, 1, expected_phase)
        );
        assert_eq!(
            native.validated_media_policy().unwrap(),
            neutral.validated_media_policy().unwrap()
        );
        if noop {
            assert_eq!(native.durable().unwrap(), before);
            assert_eq!(neutral.durable().unwrap(), before);
        }
        owner
            .commit_mutation_candidate(native, owner.directory().to_vec())
            .unwrap();
        result = native_result;
    }
}

#[test]
fn native_and_neutral_rejections_keep_identical_typed_errors() {
    let (_temp, mut owner) = fixture();
    let result = establish(&mut owner);
    let call = input(&result);
    let mut stale_epoch = call.clone();
    stale_epoch.expected_epoch = 0;
    let mut stale_revision = call.clone();
    stale_revision.expected_authorization_revision = 0;
    let mut missing = call.clone();
    missing.call_id = hex::encode([9; 32]);
    let mut missing_binding = create(Some(1));
    if let Operation::MediaCreate(value) = &mut missing_binding {
        value.record_id = "other".into();
    }
    for (operation, expected) in [
        (Operation::MediaEnd(stale_epoch), MediaRejectionCode::Stale),
        (
            Operation::MediaEnd(stale_revision),
            MediaRejectionCode::Stale,
        ),
        (Operation::MediaEnd(missing), MediaRejectionCode::Binding),
        (
            Operation::MediaRestart(call),
            MediaRejectionCode::InvalidState,
        ),
        (create(Some(1)), MediaRejectionCode::AlreadyExists),
        (missing_binding, MediaRejectionCode::Binding),
    ] {
        let before = owner.peer().durable().unwrap();
        let now = SystemTime::now();
        let native = owner
            .prepare_media_transition(&operation, now)
            .err()
            .unwrap();
        let neutral = prepare_media_transition(
            owner.peer().clone(),
            &owner.identity.native,
            &operation,
            now,
        )
        .err()
        .unwrap();
        assert_eq!(neutral, native);
        assert!(matches!(neutral, Error::Semantic { code, .. } if code == expected));
        assert_eq!(owner.peer().durable().unwrap(), before);
    }
    assert_eq!(
        media::parse(owner.peer().admitted_room().unwrap())
            .unwrap()
            .unwrap()
            .calls
            .len(),
        1
    );
}
