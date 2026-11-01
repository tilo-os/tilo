use super::*;
use crate::owner::application_record::tests::{Fixture, fixture};
use tilo_actions::{ActionContext, ActionFacet, EffectReceiptStatus, InvocationId};
use tilo_user_identity::DeviceCert;

#[test]
fn a_member_device_records_its_own_receipts_which_the_governor_admits_as_theirs() {
    let mut fixture = fixture();
    let room = RoomId::parse(crate::owner::testing::ROOM).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("member.json");
    let member = enroll_remote_history_member(&mut fixture, &path);
    let member_key = SigningKey::from_bytes(&[116; 32]);
    let owner = crate::owner::open_file(&path).unwrap();
    let operation = request(&member, EffectReceiptStatus::Succeeded);
    let original = owner
        .commit_effect_receipt(&operation, &member_key, fixture.now)
        .unwrap();
    original.validate_for(&operation).unwrap();
    let committed = std::fs::read(&path).unwrap();
    let reopened = crate::owner::open_file(&path).unwrap();
    let retry = reopened
        .commit_effect_receipt(&operation, &member_key, fixture.now)
        .unwrap();
    assert_eq!(retry.binding, original.binding);
    assert_eq!(retry.generation, original.generation);
    assert_eq!(retry.aggregate_hash, original.aggregate_hash);
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    assert!(
        reopened
            .commit_effect_receipt(
                &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
                &member_key,
                fixture.now,
            )
            .is_err(),
        "a member records no one else's receipt"
    );
    assert!(
        reopened
            .commit_effect_receipt(&operation, &fixture.key, fixture.now)
            .is_err(),
        "a receipt is signed by the recording device's own key"
    );
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    let frames = reopened
        .with_outgoing(room, fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert!(
        !frames.is_empty(),
        "the receipt is published as the member's change"
    );
    for frame in frames {
        fixture
            .governor
            .receive(
                crate::TransportOrigin::RetainedRelayDelta(member.device_public_key),
                postcard::from_bytes(&frame).unwrap(),
            )
            .unwrap();
    }
    assert!(
        tilo_desktop_model::projection::workspace_effect_receipts_of(
            fixture.governor.peer().admitted_room().unwrap(),
        )
        .contains(&decode(&operation).unwrap()),
        "the governor admits the member's own receipt"
    );
}

/// A receipt of a new invocation by the person `by` is a device of.
fn request(by: &DeviceCert, status: EffectReceiptStatus) -> OwnerCommitRequest {
    let invocation = InvocationId::new();
    let mut context = ActionContext::for_resource("workspace", "workspace", "Room");
    context.facets = vec![
        ActionFacet::new("tilo.receipt", "effect"),
        ActionFacet::new("tilo.invocation", invocation.0.to_string()),
    ];
    let receipt = tilo_desktop_model::WorkspaceEffectReceipt {
        invocation,
        actor: blake3::Hash::from_bytes(by.user_public_key)
            .to_hex()
            .to_string(),
        context,
        status,
        completed_at: 1,
    };
    OwnerCommitRequest {
        room: crate::owner::testing::ROOM.into(),
        operation: CommitOperation::EffectReceipt {
            invocation: invocation.0,
            redacted: serde_json::to_vec(&receipt).unwrap(),
        },
    }
}

fn enroll_remote_history_member(
    fixture: &mut Fixture,
    path: &std::path::Path,
) -> tilo_user_identity::DeviceCert {
    use crate::join::{self, JoinTransportRequest, JoinTransportResponse};
    use tilo_user_identity::RevocationList;

    let member_key = SigningKey::from_bytes(&[116; 32]);
    let member = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[117; 32]),
        &member_key,
        "Remote member",
    );
    let hint = fixture.governor.hint(iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&fixture.certificate.device_public_key).unwrap(),
    ));
    let now_ns = crate::owner::testing::ns(fixture.now).unwrap();
    let invitation = fixture.governor.issue(
        vec![hint.clone()],
        join::MemberRole::Editor,
        Some(member.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let verified = join::decode_and_verify_invitation(
        &invitation,
        Some(member.user_public_key),
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let JoinTransportResponse::Challenge(challenge) = fixture.governor.handle(
        member.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: member.clone(),
            request_id: [118; 32],
        }),
    ) else {
        panic!("remote member challenge refused")
    };
    let complete = join::JoinCompleteRequest {
        proof: join::sign_join_proof(challenge.challenge, &member_key).unwrap(),
    };
    let intent = crate::owner::VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        member.clone(),
        hint,
        fixture.now,
    )
    .unwrap();
    let owner = crate::owner::open_file(path).unwrap();
    let pending = owner
        .prepare_enrollment_into(&intent, None, fixture.now)
        .unwrap();
    let JoinTransportResponse::Complete(bootstrap) = fixture.governor.handle(
        member.device_public_key,
        JoinTransportRequest::Complete(complete),
    ) else {
        panic!("remote member completion refused")
    };
    owner
        .install(
            &crate::owner::testing::finalize(intent, pending, bootstrap).unwrap(),
            fixture.now,
        )
        .unwrap();
    member
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn effect_archived_signed_history_admits_and_reopens_remote_member_without_governor_blobs() {
    let mut fixture = fixture();
    let room = RoomId::parse(crate::owner::testing::ROOM).unwrap();
    for _ in 0..130 {
        commit(
            &fixture,
            &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
        )
        .unwrap();
    }
    let frames = fixture
        .owner
        .with_outgoing(room, fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    for frame in frames {
        fixture
            .governor
            .receive(
                crate::TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key),
                postcard::from_bytes(&frame).unwrap(),
            )
            .unwrap();
    }
    let signed_root = fixture
        .governor
        .peer()
        .workspace()
        .workspace_effect_history_root(&fixture.certificate)
        .unwrap();
    assert!(signed_root.is_some());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("member.json");
    let member = enroll_remote_history_member(&mut fixture, &path);
    let reopened = crate::owner::open_file(&path).unwrap();
    reopened
        .with_current(room, |view| {
            assert_eq!(view.device(), &member);
            assert_ne!(view.device(), view.room_host());
            assert_eq!(view.record.effect_history_root, None);
            assert_eq!(
                view.peer()
                    .workspace()
                    .workspace_effect_history_root(view.device())?,
                None,
                "the member archived nothing of their own"
            );
            assert_eq!(
                view.peer()
                    .workspace()
                    .workspace_effect_history_root(view.room_host())?,
                signed_root
            );
            assert_eq!(
                tilo_desktop_model::projection::workspace_effect_receipts_of(
                    view.peer().admitted_room().unwrap(),
                )
                .len(),
                130
            );
            Ok(())
        })
        .unwrap();
    // The governor's device takes in the member's admission its fixture
    // wrote, as it would have written it, before it records again.
    let admitted = fixture
        .governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect::<Vec<_>>();
    for frame in admitted {
        fixture
            .owner
            .receive(
                room,
                crate::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                postcard::from_bytes(&frame).unwrap(),
                fixture.now,
            )
            .unwrap();
    }
    commit(
        &fixture,
        &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
    )
    .unwrap();
    let frames = fixture
        .owner
        .with_outgoing(room, fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    for frame in frames {
        reopened
            .receive(
                room,
                crate::TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key),
                postcard::from_bytes(&frame).unwrap(),
                fixture.now,
            )
            .unwrap();
    }
    crate::owner::open_file(&path)
        .unwrap()
        .with_current(room, |view| {
            assert_eq!(view.record.effect_history_root, None);
            assert_ne!(
                view.peer()
                    .workspace()
                    .workspace_effect_history_root(view.room_host())?,
                signed_root
            );
            assert_eq!(
                tilo_desktop_model::projection::workspace_effect_receipts_of(
                    view.peer().admitted_room().unwrap(),
                )
                .len(),
                131
            );
            Ok(())
        })
        .unwrap();
    assert!(
        reopened
            .commit_effect_receipt(
                &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
                &fixture.key,
                fixture.now,
            )
            .is_err(),
        "a member's device does not record with another device's key"
    );
}

fn commit(fixture: &Fixture, request: &OwnerCommitRequest) -> Result<OwnerCommitReceipt, String> {
    fixture
        .owner
        .commit_effect_receipt(request, &fixture.key, fixture.now)
}

#[test]
fn effect_terminal_is_durable_exact_retry_and_signed_room_publication() {
    let mut fixture = fixture();
    let failed = request(
        &fixture.certificate,
        EffectReceiptStatus::Failed {
            code: "refused".into(),
        },
    );
    let succeeded = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    for operation in [failed, succeeded] {
        let receipt = commit(&fixture, &operation).unwrap();
        receipt.validate_for(&operation).unwrap();
        let path = fixture.directory.path().join("owner.json");
        let bytes = std::fs::read(&path).unwrap();
        let reopened = crate::owner::open_file(&path).unwrap();
        let retry = reopened
            .commit_effect_receipt(&operation, &fixture.key, fixture.now)
            .unwrap();
        assert_eq!(retry.binding, receipt.binding);
        assert_eq!(retry.generation, receipt.generation);
        assert_eq!(retry.aggregate_hash, receipt.aggregate_hash);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let room = RoomId::parse(crate::owner::testing::ROOM).unwrap();
        let frames = reopened
            .with_outgoing(room, fixture.now, |_, outgoing| {
                Ok(outgoing
                    .entries()
                    .map(|entry| entry.bytes().to_vec())
                    .collect::<Vec<_>>())
            })
            .unwrap();
        assert!(!frames.is_empty());
        for frame in frames {
            fixture
                .governor
                .receive(
                    crate::TransportOrigin::RetainedRelayDelta(
                        fixture.certificate.device_public_key,
                    ),
                    postcard::from_bytes(&frame).unwrap(),
                )
                .unwrap();
        }
        let effects = tilo_desktop_model::projection::workspace_effect_receipts_of(
            fixture.governor.peer().admitted_room().unwrap(),
        );
        let effect = decode(&operation).unwrap();
        assert!(effects.contains(&effect));
        let mut conflict = effect;
        conflict.actor = "another-person".into();
        let mut changed = operation;
        if let CommitOperation::EffectReceipt { redacted, .. } = &mut changed.operation {
            *redacted = serde_json::to_vec(&conflict).unwrap();
        }
        assert!(commit(&fixture, &changed).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn effect_wrong_key_and_failed_publish_never_ack_or_mutate() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let fixture = fixture();
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    let operation = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    let wrong_key = SigningKey::from_bytes(&[119; 32]);
    assert!(
        fixture
            .owner
            .commit_effect_receipt(&operation, &wrong_key, fixture.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(with_failure(Checkpoint::BeforePublish, || commit(&fixture, &operation)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    fixture
        .owner
        .with_current(
            RoomId::parse(crate::owner::testing::ROOM).unwrap(),
            |view| {
                assert!(view.record.effect_receipts.is_empty());
                assert_eq!(view.generation(), 1);
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn effect_oversized_and_unbound_encoding_refuse_before_mutation() {
    let fixture = fixture();
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    let mut operation = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    if let CommitOperation::EffectReceipt { redacted, .. } = &mut operation.operation {
        redacted.resize(16 * 1024 + 1, 0);
    }
    assert!(commit(&fixture, &operation).is_err());
    let mut operation = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    if let CommitOperation::EffectReceipt { invocation, .. } = &mut operation.operation {
        *invocation = uuid::Uuid::new_v4();
    }
    assert!(commit(&fixture, &operation).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn effect_private_facets_and_unbounded_context_refuse_without_publication() {
    let fixture = fixture();
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    for variant in 0..4 {
        let mut operation = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
        let mut effect = decode(&operation).unwrap();
        match variant {
            0 => effect
                .context
                .facets
                .push(ActionFacet::new("private.argument", "not-public")),
            1 => effect.context.purpose = Some("x".repeat(513)),
            2 => {
                effect.status = EffectReceiptStatus::Failed {
                    code: "private error text".into(),
                }
            }
            _ => {
                effect.context.item = Some(tilo_actions::ActionItem {
                    id: "item".into(),
                    label: "control\nlabel".into(),
                });
            }
        }
        if let CommitOperation::EffectReceipt { redacted, .. } = &mut operation.operation {
            *redacted = serde_json::to_vec(&effect).unwrap();
        }
        assert!(commit(&fixture, &operation).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn effect_hot_cache_archives_beyond_128_and_reopens_exact_original_retry() {
    let mut fixture = fixture();
    let first = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    let accepted = commit(&fixture, &first).unwrap();
    for _ in 1..128 {
        commit(
            &fixture,
            &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
        )
        .unwrap();
    }
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    for _ in 128..160 {
        commit(
            &fixture,
            &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
        )
        .unwrap();
    }
    assert_archived_file_layout(&path);
    assert_ne!(std::fs::read(&path).unwrap(), bytes);
    let bytes = std::fs::read(&path).unwrap();
    let reopened = crate::owner::open_file(&path).unwrap();
    let retry = reopened
        .commit_effect_receipt(&first, &fixture.key, fixture.now)
        .unwrap();
    assert_eq!(retry.binding, accepted.binding);
    assert_eq!(retry.generation, accepted.generation);
    assert_eq!(retry.aggregate_hash, accepted.aggregate_hash);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    reopened
        .with_current(
            RoomId::parse(crate::owner::testing::ROOM).unwrap(),
            |view| {
                assert_eq!(view.record.effect_receipts.len(), 128);
                assert_eq!(
                    tilo_desktop_model::projection::workspace_effect_receipts_of(
                        view.peer().admitted_room().unwrap()
                    )
                    .len(),
                    160
                );
                assert_eq!(view.generation(), 161);
                assert_eq!(view.record.version, 3);
                assert!(
                    !view
                        .record
                        .effect_receipts
                        .contains_key(&decode(&first).unwrap().invocation.0.to_string())
                );
                assert_eq!(
                    view.record.effect_history_root,
                    view.peer()
                        .workspace()
                        .workspace_effect_history_root(view.device())?
                );
                Ok(())
            },
        )
        .unwrap();
    relay_to_governor_and_compare_roots(&mut fixture, &reopened);
    let mut conflict = decode(&first).unwrap();
    conflict.completed_at += 1;
    let mut changed = first;
    if let CommitOperation::EffectReceipt { redacted, .. } = &mut changed.operation {
        *redacted = serde_json::to_vec(&conflict).unwrap();
    }
    assert!(
        reopened
            .commit_effect_receipt(&changed, &fixture.key, fixture.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

/// Hand the governor every frame `owner` has outgoing, and check the effect
/// history root it admits is the one `owner` keeps.
fn relay_to_governor_and_compare_roots(
    fixture: &mut Fixture,
    owner: &crate::owner::NativeWorkspaceOwner,
) {
    let room = RoomId::parse(crate::owner::testing::ROOM).unwrap();
    let frames = owner
        .with_outgoing(room, fixture.now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .unwrap();
    assert!(!frames.is_empty());
    for frame in frames {
        fixture
            .governor
            .receive(
                crate::TransportOrigin::RetainedRelayDelta(fixture.certificate.device_public_key),
                postcard::from_bytes(&frame).unwrap(),
            )
            .unwrap();
    }
    let admitted_root = fixture
        .governor
        .peer()
        .workspace()
        .workspace_effect_history_root(&fixture.certificate)
        .unwrap();
    let local_root = owner
        .with_current(room, |view| Ok(view.record.effect_history_root))
        .unwrap();
    assert!(admitted_root.is_some());
    assert_eq!(admitted_root, local_root);
}

fn assert_archived_file_layout(path: &std::path::Path) {
    let storage: crate::owner::NativeAuthorityStorage =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(crate::owner::publication::validate_file_layout(4, &storage).is_ok());
    assert!(crate::owner::publication::validate_file_layout(5, &storage).is_ok());
    assert!(crate::owner::publication::validate_file_layout(3, &storage).is_err());
    let mut mismatched = storage.clone();
    mismatched.rooms.values_mut().next().unwrap().version = 2;
    assert!(crate::owner::publication::validate_file_layout(4, &mismatched).is_err());
    let mut missing = storage;
    missing
        .rooms
        .values_mut()
        .next()
        .unwrap()
        .effect_history_root = None;
    assert!(crate::owner::publication::validate_file_layout(4, &missing).is_err());
}

#[test]
fn effect_archive_io_and_root_publication_failure_preserve_active_custody() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let fixture = fixture();
    let first = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    let original = commit(&fixture, &first).unwrap();
    for _ in 1..128 {
        commit(
            &fixture,
            &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
        )
        .unwrap();
    }
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    let content = fixture.directory.path().join("content");
    std::fs::write(&content, b"not a directory").unwrap();
    let next = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    assert!(commit(&fixture, &next).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(&content).unwrap();
    assert!(with_failure(Checkpoint::BeforePublish, || commit(&fixture, &next)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let retry = commit(&fixture, &first).unwrap();
    assert_eq!(retry.generation, original.generation);
    assert_eq!(retry.aggregate_hash, original.aggregate_hash);
    commit(&fixture, &next).unwrap();
    assert!(crate::owner::open_file(&path).is_ok());
}

#[test]
fn missing_or_corrupt_effect_history_is_not_unknown_uuid_or_successful_restore() {
    let fixture = fixture();
    let first = request(&fixture.certificate, EffectReceiptStatus::Succeeded);
    commit(&fixture, &first).unwrap();
    for _ in 1..129 {
        commit(
            &fixture,
            &request(&fixture.certificate, EffectReceiptStatus::Succeeded),
        )
        .unwrap();
    }
    let room = RoomId::parse(crate::owner::testing::ROOM).unwrap();
    let hash = fixture
        .owner
        .with_current(room, |view| Ok(view.record.effect_history_root.unwrap()))
        .unwrap();
    let address = blake3::Hash::from_bytes(hash).to_hex().to_string();
    let block = fixture
        .directory
        .path()
        .join("content")
        .join(address.get(..2).unwrap())
        .join(&address);
    let block_bytes = std::fs::read(&block).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let state_bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&block).unwrap();
    assert!(commit(&fixture, &first).is_err());
    assert!(
        commit(
            &fixture,
            &request(&fixture.certificate, EffectReceiptStatus::Succeeded)
        )
        .is_err()
    );
    assert!(crate::owner::open_file(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), state_bytes);
    std::fs::write(&block, b"corrupt").unwrap();
    assert!(commit(&fixture, &first).is_err());
    assert!(crate::owner::open_file(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), state_bytes);
    std::fs::write(&block, block_bytes).unwrap();
    assert!(crate::owner::open_file(&path).is_ok());
}
