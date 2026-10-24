use super::*;
use crate::application_record::tests::{Fixture, fixture};
use tilo_actions::{ActionContext, ActionFacet, EffectReceiptStatus, InvocationId};

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one end-to-end scenario: its setup and assertions stay together"
)]
fn member_effect_uses_original_durable_receipt_and_refuses_changed_or_foreign_audit() {
    let mut fixture = fixture();
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let member = enroll_remote_history_member(&mut fixture, &directory.path().join("member.json"));
    let frames = fixture
        .governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect::<Vec<_>>();
    for frame in frames {
        fixture
            .owner
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                postcard::from_bytes(&frame).unwrap(),
                fixture.now,
            )
            .unwrap();
    }
    let mut operation = request(EffectReceiptStatus::Succeeded);
    let mut effect = decode(&operation).unwrap();
    effect.actor = blake3::Hash::from_bytes(member.user_public_key)
        .to_hex()
        .to_string();
    if let CommitOperation::EffectReceipt { redacted, .. } = &mut operation.operation {
        *redacted = serde_json::to_vec(&effect).unwrap();
    }
    let commit =
        |owner: &NativeWorkspaceOwner, request: &OwnerCommitRequest, caller: &DeviceCert| {
            owner.commit_member_effect_receipt(
                request,
                caller,
                &fixture.certificate,
                &fixture.key,
                fixture.now,
            )
        };
    let original = commit(&fixture.owner, &operation, &member).unwrap();
    let path = fixture.directory.path().join("owner.json");
    let committed = std::fs::read(&path).unwrap();
    let reopened = crate::open_file(&path).unwrap();
    let retry = commit(&reopened, &operation, &member).unwrap();
    assert_eq!(retry.binding, original.binding);
    assert_eq!(retry.generation, original.generation);
    assert_eq!(retry.aggregate_hash, original.aggregate_hash);
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    fill_member_capacity(&fixture, &reopened, &member);
    let committed = std::fs::read(&path).unwrap();
    let archived = commit(&reopened, &operation, &member).unwrap();
    assert_eq!(archived.binding, original.binding);
    assert_eq!(archived.generation, original.generation);
    assert_eq!(archived.aggregate_hash, original.aggregate_hash);
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    let mut conflict = operation.clone();
    effect.status = EffectReceiptStatus::Cancelled;
    if let CommitOperation::EffectReceipt { redacted, .. } = &mut conflict.operation {
        *redacted = serde_json::to_vec(&effect).unwrap();
    }
    assert!(commit(&reopened, &conflict, &member).is_err());
    assert!(commit(&reopened, &operation, &fixture.certificate).is_err());
    let mut stale = member.clone();
    stale.device_label = "changed certified evidence".into();
    assert!(commit(&reopened, &operation, &stale).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), committed);
    let messages = reopened
        .with_current(room, |view| {
            let mut peer = view.peer().clone();
            assert!(peer.governance_mut().remove_member(
                member.user_public_key,
                &fixture.key,
                fixture.now
            )?);
            peer.publication_mut().durable_mutation_messages(
                view.peer(),
                &fixture.key,
                fixture.now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        reopened
            .receive(
                room,
                tilo_peer_state::TransportOrigin::RetainedNativePublication {
                    publisher: fixture.certificate.device_public_key,
                },
                message,
                fixture.now,
            )
            .unwrap();
    }
    let removed = std::fs::read(&path).unwrap();
    assert!(
        commit(&reopened, &operation, &member).is_err(),
        "an existing UUID cannot waive current membership"
    );
    assert_eq!(std::fs::read(&path).unwrap(), removed);
}

fn fill_member_capacity(fixture: &Fixture, owner: &NativeWorkspaceOwner, member: &DeviceCert) {
    // First archive the original member audit under other valid governor audits.
    for _ in 0..128 {
        owner
            .commit_effect_receipt(
                &request(EffectReceiptStatus::Succeeded),
                &fixture.certificate,
                &fixture.key,
                fixture.now,
            )
            .unwrap();
    }
    // The signed public index retains the archived original audit. Fill its
    // remaining per-member capacity with 127 new, admitted requests.
    for _ in 0..127 {
        let mut operation = request(EffectReceiptStatus::Succeeded);
        let mut effect = decode(&operation).unwrap();
        effect.actor = blake3::Hash::from_bytes(member.user_public_key)
            .to_hex()
            .to_string();
        if let CommitOperation::EffectReceipt { redacted, .. } = &mut operation.operation {
            *redacted = serde_json::to_vec(&effect).unwrap();
        }
        owner
            .commit_member_effect_receipt(
                &operation,
                member,
                &fixture.certificate,
                &fixture.key,
                fixture.now,
            )
            .unwrap();
    }
}

fn request(status: EffectReceiptStatus) -> OwnerCommitRequest {
    let invocation = InvocationId::new();
    let mut context = ActionContext::for_resource("workspace", "workspace", "Room");
    context.facets = vec![
        ActionFacet::new("tilo.receipt", "effect"),
        ActionFacet::new("tilo.invocation", invocation.0.to_string()),
    ];
    let receipt = tilo_desktop_model::WorkspaceEffectReceipt {
        invocation,
        actor: "person:test".into(),
        context,
        status,
        completed_at: 1,
    };
    OwnerCommitRequest {
        room: crate::testing::ROOM.into(),
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
    use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse};
    use tilo_user_identity::RevocationList;

    let member_key = SigningKey::from_bytes(&[116; 32]);
    let member = crate::testing::certificate(
        &SigningKey::from_bytes(&[117; 32]),
        &member_key,
        "Remote member",
    );
    let hint = fixture.governor.hint(iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&fixture.certificate.device_public_key).unwrap(),
    ));
    let now_ns = crate::testing::ns(fixture.now).unwrap();
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
    let intent = crate::VerifiedNativeEnrollmentIntent::from_proof(
        verified,
        complete.clone(),
        member.clone(),
        hint,
        fixture.now,
    )
    .unwrap();
    let owner = crate::open_file(path).unwrap();
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
            &crate::testing::finalize(intent, pending, bootstrap).unwrap(),
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
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
    for _ in 0..130 {
        commit(&fixture, &request(EffectReceiptStatus::Succeeded)).unwrap();
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
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    fixture.certificate.device_public_key,
                ),
                postcard::from_bytes(&frame).unwrap(),
            )
            .unwrap();
    }
    let signed_root = fixture
        .governor
        .peer()
        .workspace()
        .workspace_effect_history_root()
        .unwrap();
    assert!(signed_root.is_some());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("member.json");
    let member = enroll_remote_history_member(&mut fixture, &path);
    let reopened = crate::open_file(&path).unwrap();
    reopened
        .with_current(room, |view| {
            assert_eq!(view.device(), &member);
            assert_ne!(view.device(), view.governor());
            assert_eq!(view.record.effect_history_root, None);
            assert_eq!(
                view.peer().workspace().workspace_effect_history_root()?,
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
    commit(&fixture, &request(EffectReceiptStatus::Succeeded)).unwrap();
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
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    fixture.certificate.device_public_key,
                ),
                postcard::from_bytes(&frame).unwrap(),
                fixture.now,
            )
            .unwrap();
    }
    crate::open_file(&path)
        .unwrap()
        .with_current(room, |view| {
            assert_eq!(view.record.effect_history_root, None);
            assert_ne!(
                view.peer().workspace().workspace_effect_history_root()?,
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
                &request(EffectReceiptStatus::Succeeded),
                &fixture.certificate,
                &fixture.key,
                fixture.now,
            )
            .is_err(),
        "remote member cannot claim governor receipt custody"
    );
}

fn commit(fixture: &Fixture, request: &OwnerCommitRequest) -> Result<OwnerCommitReceipt, String> {
    fixture
        .owner
        .commit_effect_receipt(request, &fixture.certificate, &fixture.key, fixture.now)
}

#[test]
fn effect_terminal_is_durable_exact_retry_and_signed_room_publication() {
    let mut fixture = fixture();
    let failed = request(EffectReceiptStatus::Failed {
        code: "refused".into(),
    });
    let succeeded = request(EffectReceiptStatus::Succeeded);
    for operation in [failed, succeeded] {
        let receipt = commit(&fixture, &operation).unwrap();
        receipt.validate_for(&operation).unwrap();
        let path = fixture.directory.path().join("owner.json");
        let bytes = std::fs::read(&path).unwrap();
        let reopened = crate::open_file(&path).unwrap();
        let retry = reopened
            .commit_effect_receipt(&operation, &fixture.certificate, &fixture.key, fixture.now)
            .unwrap();
        assert_eq!(retry.binding, receipt.binding);
        assert_eq!(retry.generation, receipt.generation);
        assert_eq!(retry.aggregate_hash, receipt.aggregate_hash);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let room = RoomId::parse(crate::testing::ROOM).unwrap();
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
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(
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
fn effect_wrong_governor_and_failed_publish_never_ack_or_mutate() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let fixture = fixture();
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    let operation = request(EffectReceiptStatus::Succeeded);
    let wrong_key = SigningKey::from_bytes(&[119; 32]);
    let wrong_certificate = crate::testing::certificate(
        &SigningKey::from_bytes(&[118; 32]),
        &wrong_key,
        "Other governor",
    );
    assert!(
        fixture
            .owner
            .commit_effect_receipt(&operation, &wrong_certificate, &wrong_key, fixture.now)
            .is_err()
    );
    assert!(
        fixture
            .owner
            .commit_effect_receipt(&operation, &fixture.certificate, &wrong_key, fixture.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(with_failure(Checkpoint::BeforePublish, || commit(&fixture, &operation)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    fixture
        .owner
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            assert!(view.record.effect_receipts.is_empty());
            assert_eq!(view.generation(), 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn effect_oversized_and_unbound_encoding_refuse_before_mutation() {
    let fixture = fixture();
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    let mut operation = request(EffectReceiptStatus::Succeeded);
    if let CommitOperation::EffectReceipt { redacted, .. } = &mut operation.operation {
        redacted.resize(16 * 1024 + 1, 0);
    }
    assert!(commit(&fixture, &operation).is_err());
    let mut operation = request(EffectReceiptStatus::Succeeded);
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
        let mut operation = request(EffectReceiptStatus::Succeeded);
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
    let first = request(EffectReceiptStatus::Succeeded);
    let accepted = commit(&fixture, &first).unwrap();
    for _ in 1..128 {
        commit(&fixture, &request(EffectReceiptStatus::Succeeded)).unwrap();
    }
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    for _ in 128..160 {
        commit(&fixture, &request(EffectReceiptStatus::Succeeded)).unwrap();
    }
    assert_archived_file_layout(&path);
    assert_ne!(std::fs::read(&path).unwrap(), bytes);
    let bytes = std::fs::read(&path).unwrap();
    let reopened = crate::open_file(&path).unwrap();
    let retry = reopened
        .commit_effect_receipt(&first, &fixture.certificate, &fixture.key, fixture.now)
        .unwrap();
    assert_eq!(retry.binding, accepted.binding);
    assert_eq!(retry.generation, accepted.generation);
    assert_eq!(retry.aggregate_hash, accepted.aggregate_hash);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    reopened
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
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
                view.peer().workspace().workspace_effect_history_root()?
            );
            Ok(())
        })
        .unwrap();
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
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
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    fixture.certificate.device_public_key,
                ),
                postcard::from_bytes(&frame).unwrap(),
            )
            .unwrap();
    }
    let admitted_root = fixture
        .governor
        .peer()
        .workspace()
        .workspace_effect_history_root()
        .unwrap();
    let local_root = reopened
        .with_current(room, |view| Ok(view.record.effect_history_root))
        .unwrap();
    assert!(admitted_root.is_some());
    assert_eq!(admitted_root, local_root);
    let mut conflict = decode(&first).unwrap();
    conflict.completed_at += 1;
    let mut changed = first;
    if let CommitOperation::EffectReceipt { redacted, .. } = &mut changed.operation {
        *redacted = serde_json::to_vec(&conflict).unwrap();
    }
    assert!(
        reopened
            .commit_effect_receipt(&changed, &fixture.certificate, &fixture.key, fixture.now)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

fn assert_archived_file_layout(path: &std::path::Path) {
    let storage: crate::NativeAuthorityStorage =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(crate::publication::validate_file_layout(4, &storage).is_ok());
    assert!(crate::publication::validate_file_layout(5, &storage).is_ok());
    assert!(crate::publication::validate_file_layout(3, &storage).is_err());
    let mut mismatched = storage.clone();
    mismatched.rooms.values_mut().next().unwrap().version = 2;
    assert!(crate::publication::validate_file_layout(4, &mismatched).is_err());
    let mut missing = storage;
    missing
        .rooms
        .values_mut()
        .next()
        .unwrap()
        .effect_history_root = None;
    assert!(crate::publication::validate_file_layout(4, &missing).is_err());
}

#[test]
fn effect_archive_io_and_root_publication_failure_preserve_active_custody() {
    use tilo_files::native::atomic::test_support::{Checkpoint, with_failure};
    let fixture = fixture();
    let first = request(EffectReceiptStatus::Succeeded);
    let original = commit(&fixture, &first).unwrap();
    for _ in 1..128 {
        commit(&fixture, &request(EffectReceiptStatus::Succeeded)).unwrap();
    }
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    let content = fixture.directory.path().join("content");
    std::fs::write(&content, b"not a directory").unwrap();
    let next = request(EffectReceiptStatus::Succeeded);
    assert!(commit(&fixture, &next).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(&content).unwrap();
    assert!(with_failure(Checkpoint::BeforePublish, || commit(&fixture, &next)).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let retry = commit(&fixture, &first).unwrap();
    assert_eq!(retry.generation, original.generation);
    assert_eq!(retry.aggregate_hash, original.aggregate_hash);
    commit(&fixture, &next).unwrap();
    assert!(crate::open_file(&path).is_ok());
}

#[test]
fn missing_or_corrupt_effect_history_is_not_unknown_uuid_or_successful_restore() {
    let fixture = fixture();
    let first = request(EffectReceiptStatus::Succeeded);
    commit(&fixture, &first).unwrap();
    for _ in 1..129 {
        commit(&fixture, &request(EffectReceiptStatus::Succeeded)).unwrap();
    }
    let room = RoomId::parse(crate::testing::ROOM).unwrap();
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
    assert!(commit(&fixture, &request(EffectReceiptStatus::Succeeded)).is_err());
    assert!(crate::open_file(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), state_bytes);
    std::fs::write(&block, b"corrupt").unwrap();
    assert!(commit(&fixture, &first).is_err());
    assert!(crate::open_file(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), state_bytes);
    std::fs::write(&block, block_bytes).unwrap();
    assert!(crate::open_file(&path).is_ok());
}
