use super::ApplicationRecord;
use crate::join::{self, JoinTransportRequest, JoinTransportResponse};
use crate::owner::testing::record_on_disk;
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

/// A device of a second person, enrolled into a workspace another person governs:
/// a participant that neither governs nor hosts the workspace.
pub(crate) fn member_device_fixture() -> Fixture {
    fixture_with(
        &SigningKey::from_bytes(&[96; 32]),
        SigningKey::from_bytes(&[92; 32]),
        SigningKey::from_bytes(&[94; 32]),
        join::MemberRole::Moderator,
        true,
    )
}

/// A device of `root` at `key`, enrolled into the workspace `governor_key`'s
/// device of the workspace owner's root governs.
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
        testing::WORKSPACE,
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
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    assert!(
        fixture
            .owner
            .prepare_application_record(
                workspace,
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
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    fixture
        .owner
        .with_current(workspace, |view| {
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
    let before = record_on_disk(&fixture.directory.path().join("owner.json"));
    fixture
        .owner
        .with_current(
            crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap(),
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
        record_on_disk(&fixture.directory.path().join("owner.json")),
        before
    );
}

#[test]
fn actual_empty_record_creation_is_durable_idempotent_and_stays_kept_here() {
    let fixture = fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let path = fixture.directory.path().join("owner.json");
    assert!(
        fixture
            .owner
            .prepare_application_record(
                workspace,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    let bytes = record_on_disk(&path);
    assert!(
        !fixture
            .owner
            .prepare_application_record(
                workspace,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    assert_eq!(record_on_disk(&path), bytes);
    let reopened = open_file(&path).unwrap();
    reopened
        .with_current(workspace, |view| {
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
    // This device keeps the workspace, so the workspace's records are already
    // where its own write landed: no keeper elsewhere is waited for.
    let custody = reopened.outgoing_status(workspace, fixture.now).unwrap();
    assert_eq!(custody.pending, 0);
    assert_eq!(custody.blocked, 0);
    reopened
        .with_outgoing(workspace, fixture.now, |_, outgoing| {
            assert_eq!(
                outgoing.entries().count(),
                0,
                "a workspace kept on this device queues nothing"
            );
            Ok(())
        })
        .unwrap();
}

/// Let a member author an app's records: an administered workspace command
/// this device is allowed to give.
fn grant_app_authorship(fixture: &Fixture, app: &str) {
    fixture
        .owner
        .commit_workspace_membership_command(
            crate::owner_commit::OwnerCommitRequest {
                workspace: testing::WORKSPACE.into(),
                operation: crate::owner_commit::CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: fixture.certificate.clone(),
                    command:
                        crate::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                            app: app.into(),
                        },
                },
            },
            &fixture.key,
            fixture.now,
        )
        .unwrap();
}

/// One write of this device's record, committed to its custody.
fn publish_write(fixture: &Fixture, workspace: crate::owner::WorkspaceId, key: &str) {
    use automerge::transaction::Transactable as _;
    let message = fixture
        .owner
        .with_current(workspace, |view| {
            let mut document =
                automerge::AutoCommit::load(&view.peer().document("so.tilo.chat", "").unwrap())
                    .unwrap();
            let before = document.get_heads();
            document.put(automerge::ROOT, key, key).unwrap();
            document.commit_edit("fixture", key);
            view.peer().clone().publication_mut().publish_incremental(
                "so.tilo.chat",
                "",
                document.save_after(&before),
                &fixture.key,
                fixture.now,
            )
        })
        .unwrap();
    fixture
        .owner
        .commit_document(
            crate::owner::publication::request(&message).unwrap(),
            fixture.now,
        )
        .unwrap();
}

/// A member of the workspace, not one of its owner's devices, restarts with
/// its retained frames and publishes them as it signed them: nothing it sends
/// waits for an owner's checkpoint. A peer handed the later frame first asks
/// for what that frame builds on, and converges once it arrives.
#[test]
fn a_member_device_republishes_its_retained_frames_and_a_peer_converges() {
    let fixture = member_device_fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let path = fixture.directory.path().join("owner.json");
    grant_app_authorship(&fixture, "so.tilo.chat");
    assert!(
        fixture
            .owner
            .prepare_application_record(
                workspace,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    // Two writes of this device, the second building on the first.
    publish_write(&fixture, workspace, "first");
    publish_write(&fixture, workspace, "second");
    let reopened = open_file(&path).unwrap();
    let custody = reopened.outgoing_status(workspace, fixture.now).unwrap();
    assert_eq!(
        custody.pending, 4,
        "the authorship grant, the record's creation and two writes"
    );
    assert_eq!(
        custody.blocked, 0,
        "every retained frame publishes as it was signed"
    );
    reopened.retire_outgoing(workspace).unwrap();
    let frames = reopened
        .with_outgoing(workspace, fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert_eq!(frames.len(), 4);
    let frames = frames
        .into_iter()
        .map(|bytes| postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&bytes).unwrap())
        .collect::<Vec<_>>();
    // A peer that never saw the first write receives the second one first.
    let origin = |fixture: &Fixture| {
        crate::TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key)
    };
    let mut peer = fixture.governor.peer().clone();
    let held = peer
        .receive_from_report(origin(&fixture), frames[3].clone(), fixture.now)
        .unwrap();
    assert_eq!(
        held.primary(),
        &crate::InputDisposition::RetryLater(crate::MissingPrerequisite::RecordCheckpoint)
    );
    for frame in &frames[0..=2] {
        peer.receive_from_report(origin(&fixture), frame.clone(), fixture.now)
            .unwrap();
    }
    let landed = peer
        .receive_from_report(origin(&fixture), frames[3].clone(), fixture.now)
        .unwrap();
    assert!(
        matches!(landed.primary(), crate::InputDisposition::Duplicate(_)),
        "the held write landed once what it builds on arrived: {:?}",
        landed.primary()
    );
    let authored = reopened
        .with_current(workspace, |view| {
            Ok(view.peer().document("so.tilo.chat", ""))
        })
        .unwrap();
    assert_eq!(peer.document("so.tilo.chat", ""), authored);
}

#[test]
fn existing_authored_record_is_not_overwritten_by_reopening_the_app() {
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    let fixture = fixture();
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    fixture
        .owner
        .prepare_application_record(
            workspace,
            &fixture.certificate,
            &fixture.key,
            &route(),
            fixture.now,
        )
        .unwrap();
    let message = fixture
        .owner
        .with_current(workspace, |view| {
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
    let bytes = record_on_disk(&path);
    assert!(
        !fixture
            .owner
            .prepare_application_record(
                workspace,
                &fixture.certificate,
                &fixture.key,
                &route(),
                fixture.now
            )
            .unwrap()
    );
    assert_eq!(record_on_disk(&path), bytes);
    let reopened = open_file(&path).unwrap();
    reopened
        .with_current(workspace, |view| {
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
                fixture.owner.with_current(workspace, |current| Ok(current
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
    let workspace = crate::owner::WorkspaceId::parse(testing::WORKSPACE).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let bytes = record_on_disk(&path);
    assert!(
        fixture
            .owner
            .prepare_application_record(
                workspace,
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
            .prepare_application_record(workspace, &other, &fixture.key, &route(), fixture.now)
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
    assert_eq!(record_on_disk(&path), bytes);
}
