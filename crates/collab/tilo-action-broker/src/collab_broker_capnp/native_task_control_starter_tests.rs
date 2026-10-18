//! A second, genuinely enrolled device of the starter, independently revocable.
use super::*;
use tilo_peer_state::join::{self, InviteGrant, JoinChallenge, MemberRole};

fn publish(f: &Fixture, change: impl FnOnce(&mut tilo_peer_state::StatePeer)) {
    let now = std::time::SystemTime::now();
    let messages = f
        .native
        .owner
        .with_current(room(), |view| {
            let before = view.peer().clone();
            let mut next = before.clone();
            change(&mut next);
            next.durable_mutation_messages(
                &before,
                &f.native.key,
                now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        f.native
            .owner
            .receive(
                room(),
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    f.native.key.verifying_key().to_bytes(),
                ),
                message,
                now,
            )
            .unwrap();
    }
}

pub(super) fn approve(f: &Fixture) -> tilo_user_identity::DeviceCert {
    let key = SigningKey::from_bytes(&[94; 32]);
    let cert = tilo_joined_room::testing::certificate(
        &SigningKey::from_bytes(&[91; 32]),
        &key,
        "Independent starter device",
    );
    let workspace = f
        .native
        .owner
        .with_current(room(), |view| Ok(view.workspace()))
        .unwrap();
    publish(f, |peer| {
        let now = std::time::SystemTime::now();
        let now_ns: u64 = now
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .try_into()
            .unwrap();
        let signed = join::issue_invitation(
            InviteGrant {
                workspace_id: workspace,
                room: room().to_string(),
                authority: f.native.certificate.clone(),
                issuer: f.native.certificate.clone(),
                delegation: None,
                role: MemberRole::Editor,
                nonce: [94; 16],
                recipient_user_public_key: Some(cert.user_public_key),
                invitee_name: None,
                expires_at_ns: now_ns.checked_add(60_000_000_000).unwrap(),
                transport: vec![],
            },
            &f.native.key,
        )
        .unwrap();
        let join = join::decode_and_verify_invitation(
            &signed,
            Some(cert.user_public_key),
            &tilo_user_identity::RevocationList::new(),
            now_ns,
        )
        .unwrap();
        let challenge = JoinChallenge::new(
            &join,
            &cert,
            [94; 32],
            [95; 32],
            now_ns.checked_add(30_000_000_000).unwrap(),
            now_ns,
        )
        .unwrap();
        let proof = join::sign_join_proof(challenge.clone(), &key).unwrap();
        let redemption = join::verify_join_proof(
            &proof,
            &challenge,
            &cert,
            &tilo_user_identity::RevocationList::new(),
            now_ns,
        )
        .unwrap();
        peer.governance_mut()
            .admit_invited_member(&redemption, &f.native.key, now)
            .unwrap();
    });
    f.native
        .owner
        .commit_workspace_task_command_with_key(
            OwnerCommitRequest {
                room: room().to_string(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: uuid::Uuid::new_v4(),
                    authority: WorkspaceTaskAuthority::Person {
                        caller: cert.clone(),
                    },
                    command: WorkspaceTaskCommand::ApproveRun {
                        task_id: f.task.clone(),
                        agent_id: "scribe".into(),
                        expected_run_epoch: 0,
                        after_revision: 0,
                    },
                },
            },
            &f.native.key,
            std::time::SystemTime::now(),
        )
        .unwrap();
    cert
}

pub(super) fn revoke(f: &Fixture, cert: &tilo_user_identity::DeviceCert) {
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Retired,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &SigningKey::from_bytes(&[91; 32]))
        .unwrap();
    publish(f, |peer| {
        peer.governance_mut()
            .publish_governed_revocations(&[entry], &f.native.key, std::time::SystemTime::now())
            .unwrap();
    });
}
