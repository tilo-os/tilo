use super::*;
use neighbour_wire::contribution::{
    ContributionAttestorV1, ContributionCheckpointBodyV1, ContributionContractV1,
    ContributionDocumentBasisV1, ContributionOperationV1, ContributionReceiptV1,
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
        governance_heads: vec![[2; 32]],
        task_bases: vec![ContributionDocumentBasisV1 {
            app: "so.tilo.desktop".into(),
            record: "tasks".into(),
            artifact: [3; 16],
            heads: vec![[4; 32]],
            digest: [5; 32],
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
            receipts: vec![ContributionReceiptV1 {
                change_hash: [10; 32],
                original_certificate: 0,
                actor: "scribe".into(),
                participant: Some("scribe".into()),
                source_invocation: [11; 16],
                action: "write-topic".into(),
                contract: 0,
                task: Some(ContributionTaskFrontierV1 {
                    task_id: "task-a".into(),
                    agent: "scribe".into(),
                    run_epoch: 1,
                    direction_revision: 2,
                    task_basis: 0,
                }),
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
        verify_contribution_checkpoint(&direct, &fixture.governor, &revocations, 10).unwrap();
    assert_eq!(
        first,
        verify_contribution_checkpoint(&direct, &fixture.governor, &revocations, 10).unwrap()
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
    verify_contribution_checkpoint(&delegated, &fixture.governor, &revocations, 10).unwrap();

    let mut changed = delegated.clone();
    changed.body.operations[0].receipts[0]
        .task
        .as_mut()
        .unwrap()
        .run_epoch = 2;
    assert!(verify_contribution_checkpoint(&changed, &fixture.governor, &revocations, 10).is_err());
    let mut missing_basis = delegated;
    missing_basis.body.task_bases.clear();
    assert!(
        verify_contribution_checkpoint(&missing_basis, &fixture.governor, &revocations, 10)
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
        verify_contribution_checkpoint(&checkpoint, &fixture.governor, &revoked, 30).is_err(),
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
