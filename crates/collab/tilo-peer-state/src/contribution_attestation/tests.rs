use super::*;
use neighbour_wire::contribution::{
    ContributionAttestorV1, ContributionCheckpointBodyV1, ContributionContractV1,
    ContributionConversationReplyV1, ContributionOperationV1, ContributionOriginalPolicyV1,
    ContributionPurposeV1, ContributionReceiptV1, ContributionRoomBasisV1,
    ContributionTaskFrontierV1,
};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
    sign_device_cert, sign_revocation_entry,
};

const ROOM: &str = "6b3f9a10-0000-4000-8000-00000000000c";

fn certificate(root: &ed25519_dalek::SigningKey, device: &ed25519_dalek::SigningKey) -> DeviceCert {
    let mut certificate = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "test device".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut certificate, root).unwrap();
    certificate
}

fn checkpoint_body(original: DeviceCert) -> ContributionCheckpointBodyV1 {
    ContributionCheckpointBodyV1 {
        version: 1,
        room: ROOM.into(),
        workspace_id: tilo_relay_protocol::room_workspace_id(ROOM).unwrap(),
        generation: 1,
        previous_digest: None,
        authored_at_ns: 10,
        room_bases: vec![ContributionRoomBasisV1 {
            heads: vec![[2; 32]],
            digest: contribution_room_basis_digest(ROOM, &[[2; 32]]).unwrap(),
        }],
        certificates: vec![original],
        contracts: vec![ContributionContractV1 {
            digest: [6; 32],
            permission: 3,
        }],
        operations: vec![ContributionOperationV1 {
            app: "so.tilo.chat".into(),
            record: String::new(),
            artifact: [7; 16],
            operation_id: [8; 32],
            outer_signer: [9; 32],
            room_basis: 0,
            receipts: vec![ContributionReceiptV1 {
                change_hash: [10; 32],
                original_certificate: 0,
                original_policy: ContributionOriginalPolicyV1::ManagedAgent,
                actor: "scribe".into(),
                participant: Some("scribe".into()),
                source_invocation: [11; 16],
                action: "write-topic".into(),
                contract: 0,
                purpose: Some(ContributionPurposeV1::Task(ContributionTaskFrontierV1 {
                    task_id: "task-a".into(),
                    agent: "scribe".into(),
                    run_epoch: 1,
                    direction_revision: 2,
                })),
            }],
        }],
    }
}

struct Authorities {
    governor_root: ed25519_dalek::SigningKey,
    governor_key: ed25519_dalek::SigningKey,
    governor: DeviceCert,
    host_root: ed25519_dalek::SigningKey,
    host_key: ed25519_dalek::SigningKey,
    host: DeviceCert,
}

#[test]
fn room_basis_digest_binds_the_canonical_room_and_exact_head_set() {
    let heads = [[2; 32], [4; 32]];
    let digest = contribution_room_basis_digest(ROOM, &heads).unwrap();
    assert_eq!(
        digest,
        contribution_room_basis_digest(ROOM, &heads).unwrap()
    );
    assert_ne!(
        digest,
        contribution_room_basis_digest(ROOM, &[[2; 32]]).unwrap()
    );
    assert_ne!(
        digest,
        contribution_room_basis_digest("6b3f9a10-0000-4000-8000-00000000000d", &heads).unwrap()
    );
    for malformed in [
        Vec::new(),
        vec![[0; 32]],
        vec![[2; 32], [2; 32]],
        vec![[4; 32], [2; 32]],
    ] {
        assert!(contribution_room_basis_digest(ROOM, &malformed).is_err());
    }
    assert!(contribution_room_basis_digest(&ROOM.to_uppercase(), &heads).is_err());
    let oversized = (1..=257_u16)
        .map(|index| {
            let mut head = [0; 32];
            head[..2].copy_from_slice(&index.to_be_bytes());
            head
        })
        .collect::<Vec<_>>();
    assert!(contribution_room_basis_digest(ROOM, &oversized).is_err());
}

#[test]
fn each_operation_retains_its_own_original_governed_room_basis() {
    let fixture = authorities();
    let mut body = checkpoint_body(fixture.host.clone());
    body.room_bases.push(ContributionRoomBasisV1 {
        heads: vec![[4; 32]],
        digest: contribution_room_basis_digest(ROOM, &[[4; 32]]).unwrap(),
    });
    let mut second = body.operations[0].clone();
    second.operation_id = [12; 32];
    second.room_basis = 1;
    second.receipts[0].change_hash = [13; 32];
    let Some(ContributionPurposeV1::Task(task)) = second.receipts[0].purpose.as_mut() else {
        panic!("an exact Task receipt")
    };
    task.run_epoch = 2;
    body.operations.push(second);
    let signed = sign_contribution_checkpoint(
        body.clone(),
        ContributionAttestorV1::Governor {
            certificate: Box::new(fixture.governor.clone()),
        },
        &fixture.governor,
        &RevocationList::new(),
        &fixture.governor_key,
    )
    .unwrap();
    verify_contribution_checkpoint_signature(
        &signed,
        &fixture.governor,
        &RevocationList::new(),
        10,
    )
    .unwrap();
    let encoded = postcard::to_allocvec(&signed).unwrap();
    let decoded: neighbour_wire::contribution::DocumentContributionCheckpointV1 =
        postcard::from_bytes(&encoded).unwrap();
    assert_eq!(decoded.body, body);
    assert_eq!(decoded.body.operations[0].room_basis, 0);
    assert_eq!(decoded.body.operations[1].room_basis, 1);
    let mut substituted = signed;
    substituted.body.operations[0].room_basis = 1;
    assert!(
        verify_contribution_checkpoint_signature(
            &substituted,
            &fixture.governor,
            &RevocationList::new(),
            10
        )
        .is_err()
    );
}

#[test]
fn signer_refuses_unbound_unused_or_mismatched_governed_room_bases() {
    let fixture = authorities();
    for mutation in 0..5 {
        let mut body = checkpoint_body(fixture.host.clone());
        match mutation {
            0 => body.operations[0].room_basis = 1,
            1 => body.room_bases[0].digest = [5; 32],
            2 => body.room_bases[0].heads.push([2; 32]),
            3 => body.room_bases.push(ContributionRoomBasisV1 {
                heads: vec![[4; 32]],
                digest: contribution_room_basis_digest(ROOM, &[[4; 32]]).unwrap(),
            }),
            _ => {
                assert_eq!(mutation, 4);
                body.room_bases.clear();
            }
        }
        assert!(
            sign_contribution_checkpoint(
                body,
                ContributionAttestorV1::Governor {
                    certificate: Box::new(fixture.governor.clone())
                },
                &fixture.governor,
                &RevocationList::new(),
                &fixture.governor_key,
            )
            .is_err()
        );
    }
}

fn authorities() -> Authorities {
    let governor_root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    let governor_key = ed25519_dalek::SigningKey::from_bytes(&[2; 32]);
    let governor = certificate(&governor_root, &governor_key);
    let host_root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let host_key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let host = certificate(&host_root, &host_key);
    Authorities {
        governor_root,
        governor_key,
        governor,
        host_root,
        host_key,
        host,
    }
}

#[test]
fn certified_person_original_policy_preserves_foreign_restriction() {
    let fixture = authorities();
    for restricted in [false, true] {
        let mut body = checkpoint_body(fixture.host.clone());
        let receipt = &mut body.operations[0].receipts[0];
        receipt.purpose = None;
        receipt.original_policy = ContributionOriginalPolicyV1::CertifiedPerson {
            foreign_restriction: restricted,
        };
        let signed = sign_contribution_checkpoint(
            body,
            ContributionAttestorV1::Governor {
                certificate: Box::new(fixture.governor.clone()),
            },
            &fixture.governor,
            &RevocationList::new(),
            &fixture.governor_key,
        )
        .unwrap();
        verify_contribution_checkpoint_signature(
            &signed,
            &fixture.governor,
            &RevocationList::new(),
            10,
        )
        .unwrap();
        let bytes = postcard::to_allocvec(&signed).unwrap();
        let decoded: neighbour_wire::contribution::DocumentContributionCheckpointV1 =
            postcard::from_bytes(&bytes).unwrap();
        assert_eq!(
            decoded.body.operations[0].receipts[0].original_policy,
            ContributionOriginalPolicyV1::CertifiedPerson {
                foreign_restriction: restricted
            }
        );
        let mut substituted = signed;
        substituted.body.operations[0].receipts[0].original_policy =
            ContributionOriginalPolicyV1::CertifiedPerson {
                foreign_restriction: !restricted,
            };
        assert!(
            verify_contribution_checkpoint_signature(
                &substituted,
                &fixture.governor,
                &RevocationList::new(),
                10
            )
            .is_err()
        );
    }
}

#[test]
fn closed_original_policy_refuses_person_task_and_unbound_agent_receipts() {
    let fixture = authorities();
    for person in [false, true] {
        let mut body = checkpoint_body(fixture.host.clone());
        let receipt = &mut body.operations[0].receipts[0];
        if person {
            receipt.original_policy = ContributionOriginalPolicyV1::CertifiedPerson {
                foreign_restriction: true,
            };
        } else {
            receipt.purpose = None;
        }
        assert!(
            sign_contribution_checkpoint(
                body,
                ContributionAttestorV1::Governor {
                    certificate: Box::new(fixture.governor.clone())
                },
                &fixture.governor,
                &RevocationList::new(),
                &fixture.governor_key,
            )
            .is_err()
        );
    }
}

#[test]
fn governor_and_explicit_delegate_sign_only_canonical_exact_basis_checkpoints() {
    let fixture = authorities();
    let revocations = RevocationList::new();
    let direct = sign_contribution_checkpoint(
        checkpoint_body(fixture.host.clone()),
        ContributionAttestorV1::Governor {
            certificate: Box::new(fixture.governor.clone()),
        },
        &fixture.governor,
        &revocations,
        &fixture.governor_key,
    )
    .unwrap();
    let first =
        verify_contribution_checkpoint_signature(&direct, &fixture.governor, &revocations, 10)
            .unwrap();
    assert_eq!(
        first,
        verify_contribution_checkpoint_signature(&direct, &fixture.governor, &revocations, 10)
            .unwrap()
    );

    let delegation = sign_contribution_delegation(
        ROOM,
        tilo_relay_protocol::room_workspace_id(ROOM).unwrap(),
        &fixture.governor,
        fixture.host.clone(),
        [12; 16],
        Some(100),
        &fixture.governor_key,
    )
    .unwrap();
    let delegated = sign_contribution_checkpoint(
        checkpoint_body(fixture.host.clone()),
        ContributionAttestorV1::DelegatedHost {
            certificate: Box::new(fixture.host),
            delegation: Box::new(delegation),
        },
        &fixture.governor,
        &revocations,
        &fixture.host_key,
    )
    .unwrap();
    verify_contribution_checkpoint_signature(&delegated, &fixture.governor, &revocations, 10)
        .unwrap();

    let mut changed = delegated.clone();
    let Some(ContributionPurposeV1::Task(task)) =
        changed.body.operations[0].receipts[0].purpose.as_mut()
    else {
        panic!("an exact Task receipt")
    };
    task.run_epoch = 2;
    assert!(
        verify_contribution_checkpoint_signature(&changed, &fixture.governor, &revocations, 10)
            .is_err()
    );
    let mut missing_basis = delegated;
    missing_basis.body.room_bases.clear();
    assert!(
        verify_contribution_checkpoint_signature(
            &missing_basis,
            &fixture.governor,
            &revocations,
            10
        )
        .is_err()
    );
}

#[test]
fn revoked_delegate_cannot_backdate_a_newly_observed_checkpoint() {
    let fixture = authorities();
    let delegation = sign_contribution_delegation(
        ROOM,
        tilo_relay_protocol::room_workspace_id(ROOM).unwrap(),
        &fixture.governor,
        fixture.host.clone(),
        [12; 16],
        Some(100),
        &fixture.governor_key,
    )
    .unwrap();
    let checkpoint = sign_contribution_checkpoint(
        checkpoint_body(fixture.host.clone()),
        ContributionAttestorV1::DelegatedHost {
            certificate: Box::new(fixture.host.clone()),
            delegation: Box::new(delegation),
        },
        &fixture.governor,
        &RevocationList::new(),
        &fixture.host_key,
    )
    .unwrap();
    let mut revocation = RevocationEntry {
        user_public_key: fixture.host.user_public_key,
        revoked_device_public_key: fixture.host.device_public_key,
        revoked_at_ns: 20,
        reason: RevocationReason::Compromised,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut revocation, &fixture.host_root).unwrap();
    let mut revoked = RevocationList::new();
    revoked.insert(revocation).unwrap();
    assert!(
        verify_contribution_checkpoint_signature(&checkpoint, &fixture.governor, &revoked, 30)
            .is_err(),
        "a host's self-claimed old authored time cannot bootstrap trust after revocation"
    );
}

#[test]
fn delegation_is_distinct_from_invitation_and_exactly_governor_bound() {
    let fixture = authorities();
    let mut delegation = sign_contribution_delegation(
        ROOM,
        tilo_relay_protocol::room_workspace_id(ROOM).unwrap(),
        &fixture.governor,
        fixture.host,
        [13; 16],
        Some(100),
        &fixture.governor_key,
    )
    .unwrap();
    verify_contribution_delegation(&delegation, &fixture.governor, &RevocationList::new(), 10)
        .unwrap();
    delegation.room = "6b3f9a10-0000-4000-8000-00000000000d".into();
    assert!(
        verify_contribution_delegation(&delegation, &fixture.governor, &RevocationList::new(), 10,)
            .is_err()
    );
    assert_ne!(
        fixture.governor_root.verifying_key(),
        fixture.host_root.verifying_key()
    );
}

fn conversation_body(original: DeviceCert, delivery: uuid::Uuid) -> ContributionCheckpointBodyV1 {
    let mut body = checkpoint_body(original);
    let request = uuid::Uuid::from_u128(101);
    let operation = &mut body.operations[0];
    operation.record = "chat-record-a".into();
    let receipt = &mut operation.receipts[0];
    receipt.action = "complete-agent-message".into();
    receipt.source_invocation = *uuid::Uuid::from_u128(303).as_bytes();
    receipt.purpose = Some(ContributionPurposeV1::ConversationReply(
        ContributionConversationReplyV1 {
            room: ROOM.into(),
            chat_record: operation.record.clone(),
            channel: "channel-a".into(),
            message: format!("m_{}", request.simple()),
            request_invocation: *request.as_bytes(),
            delivery_invocation: *delivery.as_bytes(),
            agent: receipt.actor.clone(),
        },
    ));
    body
}

#[test]
fn conversation_checkpoints_preserve_parent_delivery_and_completion_without_a_task() {
    let fixture = authorities();
    for delivery in [uuid::Uuid::from_u128(101), uuid::Uuid::from_u128(202)] {
        let body = conversation_body(fixture.host.clone(), delivery);
        let signed = sign_contribution_checkpoint(
            body.clone(),
            ContributionAttestorV1::Governor {
                certificate: Box::new(fixture.governor.clone()),
            },
            &fixture.governor,
            &RevocationList::new(),
            &fixture.governor_key,
        )
        .unwrap();
        verify_contribution_checkpoint_signature(
            &signed,
            &fixture.governor,
            &RevocationList::new(),
            10,
        )
        .unwrap();
        let encoded = postcard::to_allocvec(&signed).unwrap();
        let decoded: neighbour_wire::contribution::DocumentContributionCheckpointV1 =
            postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded.body, body);
        assert_eq!(decoded.body.room_bases.len(), 1);
        assert!(matches!(
            decoded.body.operations[0].receipts[0].purpose,
            Some(ContributionPurposeV1::ConversationReply(_))
        ));
        assert_eq!(
            decoded.body.operations[0].receipts[0].source_invocation,
            *uuid::Uuid::from_u128(303).as_bytes()
        );
    }
}

#[test]
fn conversation_checkpoint_signer_refuses_malformed_source_bindings() {
    let fixture = authorities();
    let original = conversation_body(fixture.host.clone(), uuid::Uuid::from_u128(202));
    let refused = |body| {
        assert!(
            sign_contribution_checkpoint(
                body,
                ContributionAttestorV1::Governor {
                    certificate: Box::new(fixture.governor.clone()),
                },
                &fixture.governor,
                &RevocationList::new(),
                &fixture.governor_key,
            )
            .is_err()
        );
    };
    for mutation in 0..9 {
        let mut body = original.clone();
        let operation = &mut body.operations[0];
        let receipt = &mut operation.receipts[0];
        let Some(ContributionPurposeV1::ConversationReply(reply)) = receipt.purpose.as_mut() else {
            panic!("an explicit Conversation reply")
        };
        match mutation {
            0 => reply.room = "another-room".into(),
            1 => reply.chat_record = "another-record".into(),
            2 => reply.channel.clear(),
            3 => reply.message = "m_wrong-parent".into(),
            4 => reply.request_invocation = [0; 16],
            5 => reply.delivery_invocation = [0; 16],
            6 => reply.agent = "another-agent".into(),
            7 => receipt.action = "write-topic".into(),
            _ => {
                assert_eq!(mutation, 8);
                operation.app = "so.tilo.notes".into();
            }
        }
        refused(body);
    }
}
