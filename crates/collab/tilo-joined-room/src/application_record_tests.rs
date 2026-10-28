use super::ApplicationRecord;
use crate::{NativeWorkspaceOwner, VerifiedNativeEnrollmentIntent, testing};
use collab_types::CommitNamed as _;
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
    // The owner side runs on `now` throughout; the governor answers on the
    // same instant so invitations and challenges never lapse mid-test on a
    // slow machine.
    governor.pin_clock(now);
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

/// A record a room's governor publishes into it: empty, as a host creates
/// an app's first record for a member whose channel it does not steward.
fn governor_creates_empty_record(fixture: &Fixture) -> crate::CommittedDocument {
    let room = crate::RoomId::parse(testing::ROOM).unwrap();
    let message = fixture
        .owner
        .with_current(room, |view| {
            view.peer().clone().publication_mut().publish_document(
                "so.tilo.chat",
                "",
                automerge::AutoCommit::new().save(),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    fixture
        .owner
        .commit_document(crate::publication::request(&message).unwrap(), fixture.now)
        .unwrap();
    current_capture(fixture)
}

#[test]
fn first_creation_capture_is_nonempty_loadable_and_respects_its_byte_bound() {
    use automerge::ReadDoc as _;
    let fixture = fixture();
    let captured = governor_creates_empty_record(&fixture);
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
fn a_record_route_must_be_declared_by_the_installed_package() {
    assert!(
        ApplicationRecord::declared(
            "so.tilo.chat",
            "",
            "another.kind",
            &["chat.workspace".into()]
        )
        .is_err(),
        "a kind the package does not declare is never a record"
    );
    assert!(ApplicationRecord::host("", "").is_err());
    assert!(ApplicationRecord::host("so.tilo.chat\n", "").is_err());
}
