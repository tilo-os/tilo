use super::*;
use crate::application_record::tests::{Fixture, fixture};
use tilo_actions::{ActionContext, ActionFacet, EffectReceiptStatus, InvocationId};

fn request(status: EffectReceiptStatus) -> OwnerCommitRequest {
    let invocation = InvocationId::new();
    let mut context = ActionContext::for_resource("Test effect", "workspace", "workspace", "Room");
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
            _ => effect.context.action_label = "control\nlabel".into(),
        }
        if let CommitOperation::EffectReceipt { redacted, .. } = &mut operation.operation {
            *redacted = serde_json::to_vec(&effect).unwrap();
        }
        assert!(commit(&fixture, &operation).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn effect_capacity_refusal_preserves_last_signed_checkpoint_and_retry() {
    let fixture = fixture();
    let first = request(EffectReceiptStatus::Succeeded);
    let accepted = commit(&fixture, &first).unwrap();
    for _ in 1..128 {
        commit(&fixture, &request(EffectReceiptStatus::Succeeded)).unwrap();
    }
    let path = fixture.directory.path().join("owner.json");
    let bytes = std::fs::read(&path).unwrap();
    let refused = request(EffectReceiptStatus::Succeeded);
    assert!(commit(&fixture, &refused).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let retry = commit(&fixture, &first).unwrap();
    assert_eq!(retry.binding, accepted.binding);
    assert_eq!(retry.generation, accepted.generation);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let reopened = crate::open_file(&path).unwrap();
    reopened
        .with_current(RoomId::parse(crate::testing::ROOM).unwrap(), |view| {
            assert_eq!(view.record.effect_receipts.len(), 128);
            assert_eq!(
                tilo_desktop_model::projection::workspace_effect_receipts_of(
                    view.peer().admitted_room().unwrap()
                )
                .len(),
                128
            );
            assert_eq!(view.generation(), 129);
            Ok(())
        })
        .unwrap();
}
