//! New deterministic regression graphs; these are not the lost acceptance fixture.
use super::*;
use tilo_joined_room::DocumentRead;
fn capture(
    owner: &RoomOwner,
    reader: [u8; 32],
    known: Option<[u8; 32]>,
    maximum: usize,
) -> Result<DocumentRead, String> {
    owner.capture_record(
        &owner.committed.lock().unwrap().durable,
        reader,
        (APP, RECORD),
        known,
        maximum,
    )
}
fn captured(read: DocumentRead) -> tilo_joined_room::CommittedDocument {
    let DocumentRead::Captured(document) = read else {
        panic!("expected a committed capture");
    };
    document
}
#[test]
fn first_creation_bootstrap_is_durable_empty_and_bound_to_its_exact_revision() {
    let profile = profile();
    let owner = open(&profile, creation(true, false));
    let before = std::fs::read(&owner.path).unwrap();
    let mut stranger = profile.device.clone();
    stranger.device_public_key = SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes();
    assert!(owner.prepare_record(&stranger, (APP, RECORD)).is_err());
    assert_eq!(std::fs::read(&owner.path).unwrap(), before);
    assert!(
        owner
            .prepare_record(&profile.device, (APP, RECORD))
            .unwrap()
    );
    let empty =
        captured(capture(&owner, profile.device.device_public_key, None, 64 * 1024).unwrap());
    let mut document = automerge::AutoCommit::load(&empty.bytes).unwrap();
    assert_eq!(
        document.keys(automerge::ROOT).count(),
        0,
        "no seeded application data"
    );
    let original = std::fs::read(&owner.path).unwrap();
    assert!(
        !owner
            .prepare_record(&profile.device, (APP, RECORD))
            .unwrap()
    );
    assert_eq!(std::fs::read(&owner.path).unwrap(), original);
    document
        .put(automerge::ROOT, "unknown_future_field", "preserve me")
        .unwrap();
    let bootstrap = document.save();
    for (creator, revision, bytes) in [
        (&stranger, empty.revision, bootstrap.as_slice()),
        (&profile.device, [0; 32], bootstrap.as_slice()),
        (&profile.device, empty.revision, b"malformed".as_slice()),
    ] {
        assert!(
            owner
                .initialize_record(creator, (APP, RECORD), revision, bytes)
                .is_err()
        );
        assert_eq!(std::fs::read(&owner.path).unwrap(), original);
    }
    let revision = owner
        .initialize_record(&profile.device, (APP, RECORD), empty.revision, &bootstrap)
        .unwrap();
    assert!(
        matches!(capture(&owner,profile.device.device_public_key,Some(revision),64*1024).unwrap(),DocumentRead::Unchanged(actual) if actual==revision)
    );
    drop(owner);
    let reopened = open(&profile, creation(true, false));
    let read =
        captured(capture(&reopened, profile.device.device_public_key, None, 64 * 1024).unwrap());
    assert_eq!(read.revision, revision);
    let doc = automerge::AutoCommit::load(&read.bytes).unwrap();
    assert_eq!(
        doc.get(automerge::ROOT, "unknown_future_field")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "preserve me"
    );
}
#[test]
fn sealed_capture_after_restart_keeps_original_writer_and_refuses_its_conflicting_cache() {
    let profile = profile();
    let keys = steward_keys(&profile, "channels", std::slice::from_ref(&profile.key));
    let actor = automerge::ActorId::from(b"new-deterministic-original-writer".as_slice());
    let mut document = automerge::AutoCommit::new().with_actor(actor.clone());
    let mut conflicting = document.clone();
    let owner = open_with(&profile, creation(true, false), keys.clone());
    for text in ["first", "second"] {
        let request = seal(&profile, &keys, write(&profile, &mut document, text));
        commit(&owner, &request, Some(KIND))
            .unwrap()
            .validate_for(&request)
            .unwrap();
    }
    drop(owner);
    let owner = open_with(&profile, creation(true, false), keys.clone());
    let original = std::fs::read(&owner.path).unwrap();
    let snapshot =
        captured(capture(&owner, profile.device.device_public_key, None, 1024 * 1024).unwrap());
    assert_eq!(
        std::fs::read(&owner.path).unwrap(),
        original,
        "capturing does not rewrite the owner"
    );
    let mut restored = automerge::AutoCommit::load(&snapshot.bytes)
        .unwrap()
        .with_actor(actor.clone());
    assert_eq!(restored.get_heads(), document.get_heads());
    let stale = seal(
        &profile,
        &keys,
        write(
            &profile,
            &mut conflicting,
            "conflicting original sequence one",
        ),
    );
    let error = commit(&owner, &stale, Some(KIND)).unwrap_err();
    assert!(error.contains("duplicate seq 1"), "{error}");
    assert_eq!(std::fs::read(&owner.path).unwrap(), original);
    let before = restored.get_heads();
    let third = seal(&profile, &keys, write(&profile, &mut restored, "third"));
    let changes = restored.get_changes(&before);
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].actor_id(), &actor);
    assert_eq!(changes[0].seq(), 3);
    commit(&owner, &third, None).unwrap();
    drop(owner);
    let reopened = open_with(&profile, creation(true, false), keys);
    let recovered = captured(
        capture(
            &reopened,
            profile.device.device_public_key,
            None,
            1024 * 1024,
        )
        .unwrap(),
    );
    assert_ne!(recovered.revision, snapshot.revision);
    let recovered = automerge::AutoCommit::load(&recovered.bytes).unwrap();
    assert_eq!(
        recovered
            .get(automerge::ROOT, "text")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "third"
    );
}
#[test]
fn bounded_sealed_reads_refuse_denied_reader_missing_keys_and_small_capture_without_writing() {
    let profile = profile();
    let keys = steward_keys(&profile, "channels", std::slice::from_ref(&profile.key));
    let owner = open_with(&profile, creation(true, false), keys.clone());
    let request = seal(
        &profile,
        &keys,
        write(&profile, &mut automerge::AutoCommit::new(), "sealed state"),
    );
    commit(&owner, &request, Some(KIND)).unwrap();
    let original = std::fs::read(&owner.path).unwrap();
    assert!(
        capture(
            &owner,
            SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
            None,
            1024 * 1024
        )
        .is_err()
    );
    assert!(capture(&owner, profile.device.device_public_key, None, 1).is_err());
    let read =
        captured(capture(&owner, profile.device.device_public_key, None, 1024 * 1024).unwrap());
    assert!(
        matches!(capture(&owner,profile.device.device_public_key,Some(read.revision),1024*1024).unwrap(),DocumentRead::Unchanged(revision) if revision==read.revision)
    );
    keys.set(None);
    let error = capture(&owner, profile.device.device_public_key, None, 1024 * 1024)
        .err()
        .unwrap();
    assert!(error.contains("history keys unavailable"), "{error}");
    assert_eq!(std::fs::read(&owner.path).unwrap(), original);
}
