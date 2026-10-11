use super::ApplicationRecord;
use crate::{NativeWorkspaceOwner, VerifiedNativeEnrollmentIntent, open_file, testing};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse};
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
    let directory = tempfile::tempdir().unwrap();
    let owner = open_file(&directory.path().join("owner.json")).unwrap();
    let key = SigningKey::from_bytes(&[92; 32]);
    let certificate = testing::certificate(&SigningKey::from_bytes(&[91; 32]), &key, "Governor");
    let mut governor = testing::governor::TestGovernor::empty_interactive(
        certificate.clone(),
        key.clone(),
        testing::ROOM,
    )
    .unwrap();
    let endpoint = iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(
            &SigningKey::from_bytes(&[93; 32]).verifying_key().to_bytes(),
        )
        .unwrap(),
    );
    let hint = governor.hint(endpoint);
    let now = SystemTime::now();
    let now_ns = testing::ns(now).unwrap();
    let invitation = governor.issue(
        vec![hint.clone()],
        join::MemberRole::Editor,
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

fn created_capture(fixture: &Fixture) -> crate::CommittedDocument {
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
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

fn current_capture(fixture: &Fixture) -> crate::CommittedDocument {
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
    fixture
        .owner
        .with_current(room, |view| {
            let crate::DocumentRead::Captured(document) = view.read_document(
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
        .with_current(crate::RoomId::parse(testing::ROOM).unwrap(), |view| {
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
        })
        .unwrap();
    assert_eq!(
        std::fs::read(fixture.directory.path().join("owner.json")).unwrap(),
        before
    );
}

#[test]
fn creator_initialization_is_durable_and_compare_and_swaps_original_revision() {
    use automerge::{ReadDoc as _, transaction::Transactable as _};
    let fixture = fixture();
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
    let initial = created_capture(&fixture);
    let mut doc = automerge::AutoCommit::load(&initial.bytes).unwrap();
    doc.put(automerge::ROOT, "structural_workspace", testing::ROOM)
        .unwrap();
    let bytes = doc.save();
    let declared = route();
    let initialized = fixture
        .owner
        .initialize_application_record(
            room,
            &fixture.certificate,
            &fixture.key,
            super::ApplicationInitialization {
                route: &declared,
                revision: initial.revision,
                document: &bytes,
            },
            fixture.now,
        )
        .unwrap();
    assert_ne!(initialized, initial.revision);
    assert!(
        fixture
            .owner
            .initialize_application_record(
                room,
                &fixture.certificate,
                &fixture.key,
                super::ApplicationInitialization {
                    route: &declared,
                    revision: initial.revision,
                    document: &bytes
                },
                fixture.now
            )
            .is_err()
    );
    let reopened = open_file(&fixture.directory.path().join("owner.json")).unwrap();
    reopened
        .with_current(room, |view| {
            let crate::DocumentRead::Captured(document) = view.read_document(
                fixture.certificate.device_public_key,
                "so.tilo.chat",
                "",
                None,
                64 * 1024,
                fixture.now,
            )?
            else {
                panic!("durable initialized capture missing");
            };
            assert_eq!(document.revision, initialized);
            let doc = automerge::AutoCommit::load(&document.bytes).unwrap();
            assert_eq!(
                doc.get(automerge::ROOT, "structural_workspace")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_scalar()
                    .and_then(automerge::ScalarValue::as_str),
                Some(testing::ROOM)
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn initialization_failure_does_not_publish_ready_or_change_original_record() {
    use automerge::transaction::Transactable as _;
    let fixture = fixture();
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
    let initial = created_capture(&fixture);
    let mut doc = automerge::AutoCommit::load(&initial.bytes).unwrap();
    doc.put(automerge::ROOT, "should_not_publish", true)
        .unwrap();
    let bytes = doc.save();
    let declared = route();
    let path = fixture.directory.path().join("owner.json");
    std::fs::rename(&path, fixture.directory.path().join("retained-owner.json")).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        fixture
            .owner
            .initialize_application_record(
                room,
                &fixture.certificate,
                &fixture.key,
                super::ApplicationInitialization {
                    route: &declared,
                    revision: initial.revision,
                    document: &bytes
                },
                fixture.now
            )
            .is_err()
    );
    fixture
        .owner
        .with_current(room, |view| {
            assert!(matches!(
                view.read_document(
                    fixture.certificate.device_public_key,
                    "so.tilo.chat",
                    "",
                    Some(initial.revision),
                    64 * 1024,
                    fixture.now
                )?,
                crate::DocumentRead::Unchanged(_)
            ));
            Ok(())
        })
        .unwrap();
}

#[test]
fn initialization_preserves_unknown_objects_and_refuses_wrong_creator_key() {
    use automerge::{ReadDoc as _, transaction::Transactable as _};
    let fixture = fixture();
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
    let initial = created_capture(&fixture);
    let mut original = automerge::AutoCommit::load(&initial.bytes).unwrap();
    let extension = original
        .put_object(
            automerge::ROOT,
            "unknown_extension",
            automerge::ObjType::Map,
        )
        .unwrap();
    original
        .put(&extension, "retained", "opaque future schema")
        .unwrap();
    let message = fixture
        .owner
        .with_current(room, |view| {
            view.peer().clone().publish_document(
                "so.tilo.chat",
                "",
                original.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    fixture
        .owner
        .commit_document(crate::publication::request(&message).unwrap(), fixture.now)
        .unwrap();
    let revision = current_capture(&fixture).revision;
    let mut supplied = automerge::AutoCommit::new();
    supplied
        .put(automerge::ROOT, "workspace", testing::ROOM)
        .unwrap();
    let bytes = supplied.save();
    let declared = route();
    let initialization = || super::ApplicationInitialization {
        route: &declared,
        revision,
        document: &bytes,
    };
    assert!(
        fixture
            .owner
            .initialize_application_record(
                room,
                &fixture.certificate,
                &SigningKey::from_bytes(&[8; 32]),
                initialization(),
                fixture.now
            )
            .is_err()
    );
    fixture
        .owner
        .initialize_application_record(
            room,
            &fixture.certificate,
            &fixture.key,
            initialization(),
            fixture.now,
        )
        .unwrap();
    fixture
        .owner
        .with_current(room, |view| {
            let crate::DocumentRead::Captured(document) = view.read_document(
                fixture.certificate.device_public_key,
                "so.tilo.chat",
                "",
                None,
                64 * 1024,
                fixture.now,
            )?
            else {
                panic!("initialized capture missing");
            };
            let doc = automerge::AutoCommit::load(&document.bytes).unwrap();
            let (_, object) = doc
                .get(automerge::ROOT, "unknown_extension")
                .unwrap()
                .unwrap();
            assert_eq!(
                doc.get(&object, "retained")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_scalar()
                    .and_then(automerge::ScalarValue::as_str),
                Some("opaque future schema")
            );
            assert!(doc.get(automerge::ROOT, "workspace").unwrap().is_some());
            Ok(())
        })
        .unwrap();
}

#[test]
fn actual_empty_record_creation_is_durable_idempotent_and_reopens_with_custody() {
    let fixture = fixture();
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
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
            let crate::DocumentRead::Captured(document) = view.read_document(
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
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
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
            view.peer().clone().publish_document(
                "so.tilo.chat",
                "",
                document.save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    let request = crate::publication::request(&message).unwrap();
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
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
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
