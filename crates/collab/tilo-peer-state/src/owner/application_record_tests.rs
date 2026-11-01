use super::ApplicationRecord;
use crate::join::{self, JoinTransportRequest, JoinTransportResponse};
use crate::owner::{NativeWorkspaceOwner, VerifiedNativeEnrollmentIntent, open_file, testing};
use collab_types::CommitNamed as _;
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_user_identity::{DeviceCert, RevocationList};

pub(crate) struct Fixture {
    pub(crate) directory: tempfile::TempDir,
    pub(crate) owner: NativeWorkspaceOwner,
    pub(crate) key: SigningKey,
    pub(crate) certificate: DeviceCert,
    pub(crate) now: SystemTime,
    pub(crate) governor: testing::governor::TestGovernor,
}

pub(crate) fn fixture() -> Fixture {
    let key = SigningKey::from_bytes(&[92; 32]);
    let root = SigningKey::from_bytes(&[91; 32]);
    fixture_with(&root, key.clone(), key, join::MemberRole::Editor, false)
}

/// A device of a second person, enrolled into a room another person governs:
/// a participant that neither governs nor hosts the room.
pub(crate) fn member_device_fixture() -> Fixture {
    fixture_with(
        &SigningKey::from_bytes(&[96; 32]),
        SigningKey::from_bytes(&[92; 32]),
        SigningKey::from_bytes(&[94; 32]),
        join::MemberRole::Moderator,
        true,
    )
}

/// A device of `root` at `key`, enrolled into the room `governor_key`'s
/// device of the room owner's root governs.
fn fixture_with(
    root: &SigningKey,
    key: SigningKey,
    governor_key: SigningKey,
    role: join::MemberRole,
    administers: bool,
) -> Fixture {
    let governor_root = SigningKey::from_bytes(&[91; 32]);
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("owner.json")).unwrap();
    let certificate = testing::certificate(root, &key, "Governor");
    let mut governor = testing::governor::TestGovernor::empty_interactive(
        testing::certificate(&governor_root, &governor_key, "Governor"),
        governor_key,
        testing::ROOM,
    )
    .unwrap();
    let endpoint = iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(
            &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
        )
        .unwrap(),
    );
    if administers {
        governor
            .allow_governance(
                &hex::encode(certificate.user_public_key),
                tilo_action_policy::roles::governance::GRANT,
            )
            .unwrap();
    }
    let hint = governor.hint(endpoint);
    let now = SystemTime::now();
    // The owner side runs on `now` throughout; the governor answers on the
    // same instant so invitations and challenges never lapse mid-test on a
    // slow machine.
    governor.pin_clock(now);
    let now_ns = testing::ns(now).unwrap();
    let invitation = governor.issue(
        vec![hint.clone()],
        role,
        Some(certificate.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(certificate.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let response = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id: [95; 32],
        }),
    );
    let JoinTransportResponse::Challenge(response) = response else {
        panic!("challenge refused")
    };
    let proof = join::sign_join_proof(response.challenge, &key).unwrap();
    let complete = join::JoinCompleteRequest { proof };
    let intent = VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        certificate.clone(),
        hint,
        now,
    )
    .unwrap();
    let pending = owner.prepare_enrollment_into(&intent, None, now).unwrap();
    let response = governor.handle(
        certificate.device_public_key,
        JoinTransportRequest::Complete(complete),
    );
    let JoinTransportResponse::Complete(bootstrap) = response else {
        panic!("completion refused")
    };
    assert!(bootstrap.records.is_empty());
    let enrollment = testing::finalize(intent, pending, bootstrap).unwrap();
    owner.install(&enrollment, now).unwrap();
    Fixture {
        directory,
        owner,
        key,
        certificate,
        now,
        governor,
    }
}

fn route() -> ApplicationRecord {
    ApplicationRecord::declared(
        "so.tilo.chat",
        "",
        "chat.workspace",
        &["chat.workspace".into()],
    )
    .unwrap()
}

fn created_capture(fixture: &Fixture) -> crate::owner::CommittedDocument {
    let room = crate::owner::RoomId::parse(testing::ROOM).unwrap();
    assert!(
        fixture
            .owner
            .prepare_application_record(
                room,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    current_capture(fixture)
}

fn current_capture(fixture: &Fixture) -> crate::owner::CommittedDocument {
    let room = crate::owner::RoomId::parse(testing::ROOM).unwrap();
    fixture
        .owner
        .with_current(room, |view| {
            let crate::owner::DocumentRead::Captured(document) = view.read_document(
                fixture.certificate.device_public_key,
                "so.tilo.chat",
                "",
                None,
                64 * 1024,
                fixture.now,
            )?
            else {
                panic!("created record must capture");
            };
            Ok(document)
        })
        .unwrap()
}

#[test]
fn first_creation_capture_is_nonempty_loadable_and_respects_its_byte_bound() {
    use automerge::ReadDoc as _;
    let fixture = fixture();
    let captured = created_capture(&fixture);
    assert!(
        !captured.bytes.is_empty(),
        "the actual first-created record must satisfy the broker's nonempty capture contract"
    );
    let document = automerge::AutoCommit::load(&captured.bytes).unwrap();
    assert_eq!(
        document.keys(automerge::ROOT).count(),
        0,
        "apps still start empty"
    );
    let maximum = captured.bytes.len().saturating_sub(1);
    let before = std::fs::read(fixture.directory.path().join("owner.json")).unwrap();
    fixture
        .owner
        .with_current(
            crate::owner::RoomId::parse(testing::ROOM).unwrap(),
            |view| {
                let result = view.read_document(
                    fixture.certificate.device_public_key,
                    "so.tilo.chat",
                    "",
                    None,
                    maximum,
                    fixture.now,
                );
                assert!(
                    matches!(result, Err(ref error) if error.contains("Capacity")),
                    "a too-small capture must be refused"
                );
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        std::fs::read(fixture.directory.path().join("owner.json")).unwrap(),
        before
    );
}

#[test]
fn actual_empty_record_creation_is_durable_idempotent_and_reopens_with_custody() {
    let fixture = fixture();
    let room = crate::owner::RoomId::parse(testing::ROOM).unwrap();
    let path = fixture.directory.path().join("owner.json");
    assert!(
        fixture
            .owner
            .prepare_application_record(
                room,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !fixture
            .owner
            .prepare_application_record(
                room,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let reopened = open_file(&path).unwrap();
    reopened
        .with_current(room, |view| {
            let crate::owner::DocumentRead::Captured(document) = view.read_document(
                fixture.certificate.device_public_key,
                "so.tilo.chat",
                "",
                None,
                64 * 1024,
                fixture.now,
            )?
            else {
                panic!("fresh read must capture")
            };
            assert!(
                automerge::AutoCommit::load(&document.bytes)
                    .unwrap()
                    .get_heads()
                    .is_empty()
            );
            Ok(())
        })
        .unwrap();
    // The original signed snapshot is durably retained, but the native relay
    // only sends admitted incremental projections, never raw snapshots.
    let custody = reopened.outgoing_status(room, fixture.now).unwrap();
    assert_eq!(custody.pending, 1);
    assert_eq!(custody.blocked, 1);
    assert!(
        reopened
            .reconcile_outgoing(room, &fixture.key, fixture.now)
            .unwrap_err()
            .contains("fresh genuine governor checkpoint")
    );
    reopened
        .with_outgoing(room, fixture.now, |_, outgoing| {
            assert_eq!(
                outgoing.entries().count(),
                0,
                "the relay must never downgrade a retained governor snapshot to a delta"
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn existing_authored_record_is_not_overwritten_by_reopening_the_app() {
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    let fixture = fixture();
    let room = crate::owner::RoomId::parse(testing::ROOM).unwrap();
    fixture
        .owner
        .prepare_application_record(
            room,
            &fixture.certificate,
            &fixture.key,
            &route(),
            fixture.now,
        )
        .unwrap();
    let message = fixture
        .owner
        .with_current(room, |view| {
            let mut document = automerge::AutoCommit::new();
            document
                .put(automerge::ROOT, "authored", "keep this user content")
                .unwrap();
            document.commit_edit("fixture", "authored");
            view.peer().clone().publication_mut().publish_document(
                "so.tilo.chat",
                "",
                document.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    let request = crate::owner::publication::request(&message).unwrap();
    fixture.owner.commit_document(request, fixture.now).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        !fixture
            .owner
            .prepare_application_record(
                room,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let reopened = open_file(&path).unwrap();
    reopened
        .with_current(room, |view| {
            let document =
                automerge::AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                    .unwrap();
            assert_eq!(
                document
                    .get(automerge::ROOT, "authored")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("keep this user content")
            );
            assert_eq!(
                view.peer().document("so.tilo.chat", ""),
                fixture.owner.with_current(room, |current| Ok(current
                    .peer()
                    .document("so.tilo.chat", "")))?
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn creation_refuses_wrong_key_wrong_certificate_and_undeclared_kind_without_mutation() {
    let fixture = fixture();
    let room = crate::owner::RoomId::parse(testing::ROOM).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    assert!(
        fixture
            .owner
            .prepare_application_record(
                room,
                &fixture.certificate,
                &SigningKey::from_bytes(&[94; 32]),
                &route(),
                fixture.now
            )
            .is_err()
    );
    let other = testing::certificate(
        &SigningKey::from_bytes(&[96; 32]),
        &fixture.key,
        "Other identity",
    );
    assert!(
        fixture
            .owner
            .prepare_application_record(room, &other, &fixture.key, &route(), fixture.now)
            .is_err()
    );
    assert!(
        ApplicationRecord::declared(
            "so.tilo.chat",
            "",
            "another.kind",
            &["chat.workspace".into()]
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
