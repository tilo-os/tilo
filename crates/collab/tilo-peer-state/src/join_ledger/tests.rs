use super::*;
use crate::admission_tests::seeded;
use crate::join::{JoinChallengeRequest, JoinCompleteRequest, sign_join_proof};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

fn ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

fn guest() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[41; 32]);
    let key = SigningKey::from_bytes(&[42; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

fn invitation(recipient: Option<[u8; 32]>, now: u64) -> IssueInvitationRequest {
    IssueInvitationRequest {
        request_id: [7; 32],
        role: Role::Editor,
        recipient_user_public_key: recipient,
        invitee_name: Some("Guest".into()),
        expires_at_ns: now.saturating_add(10_000_000_000),
        inviter_name: None,
    }
}

/// One ledger serves every governor: issuing, challenging, redeeming and
/// completing a join through it admits the device into the workspace, answers
/// a retry of the same Complete with the same bootstrap, and spends the
/// invitation.
#[test]
#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one join read top to bottom, through each step of the ledger"
)]
fn a_join_through_the_ledger_admits_once_and_replays_its_answer() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = || Ok(vec![b"governance".to_vec()]);
    let host_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &host_cert,
        workspace_id: 7,
        generation: 1,
        governance_messages: Some(&messages),
    };
    let (guest_key, guest_cert) = guest();
    let mut ledger = JoinLedger::default();
    assert!(ledger.is_empty());

    let issued = ledger
        .issue(
            &workspace,
            &peer.owner_certificates()[0],
            invitation(Some(guest_cert.user_public_key), now_ns),
            Vec::new(),
            Signer {
                key: &authority_key,
                nonce: [43; 16],
            },
            now_ns,
        )
        .unwrap();
    let again = ledger
        .issue(
            &workspace,
            &peer.owner_certificates()[0],
            invitation(Some(guest_cert.user_public_key), now_ns),
            Vec::new(),
            Signer {
                key: &authority_key,
                nonce: [99; 16],
            },
            now_ns,
        )
        .unwrap();
    assert_eq!(issued, again, "the same request answers what it issued");
    assert_eq!(ledger.pending(now_ns).len(), 1);

    let challenge = ledger
        .challenge(
            &workspace,
            guest_cert.device_public_key,
            JoinChallengeRequest {
                request_id: [44; 32],
                signed_invitation: issued.signed_invitation.clone(),
                device_cert: guest_cert.clone(),
            },
            [45; 32],
            now_ns,
        )
        .unwrap();
    let complete = JoinCompleteRequest {
        proof: sign_join_proof(challenge, &guest_key).unwrap(),
    };
    assert!(
        ledger
            .redeem(&workspace, [9; 32], &complete.proof, now_ns)
            .is_err(),
        "a proof is redeemed only from the endpoint it was challenged on"
    );
    let redemption = ledger
        .redeem(
            &workspace,
            guest_cert.device_public_key,
            &complete.proof,
            now_ns,
        )
        .unwrap();
    let proof_digest = request_digest(&complete.proof).unwrap();
    let joins = [Redeeming {
        remote_endpoint: guest_cert.device_public_key,
        request_id: [44; 32],
        proof_digest,
        redemption: &redemption,
        name: Some("Calm Owl"),
    }];
    let mut candidate = peer.clone();
    let refusals = ledger.admit(&mut candidate, &joins, &authority_key, now_ns);
    assert!(refusals.iter().all(Option::is_none), "{refusals:?}");
    let guest_member = candidate
        .governance()
        .governed_members(now)
        .unwrap()
        .into_iter()
        .find(|member| member.user_public_key == guest_cert.user_public_key)
        .unwrap();
    assert_eq!(
        guest_member.display_name.as_deref(),
        Some("Calm Owl"),
        "the workspace names the guest as they named themself, not by their device"
    );
    let host_cert = candidate.owner_certificates()[0].clone();
    let committed = Governed {
        peer: (&candidate).into(),
        host: &host_cert,
        workspace_id: 7,
        generation: 2,
        governance_messages: Some(&messages),
    };
    let delivered = ledger.complete(&committed, &joins, &refusals).unwrap();
    let bootstrap = delivered.into_iter().next().unwrap().unwrap();
    assert_eq!(bootstrap.device_cert, guest_cert);
    assert_eq!(bootstrap.generation, 2);
    assert!(
        candidate
            .permissions(guest_cert.device_public_key, now)
            .is_ok(),
        "the guest is a member of the workspace"
    );
    assert!(ledger.redeemed(issued.invite_id));
    assert_eq!(
        ledger.admitted_device([44; 32]),
        Some(guest_cert.device_public_key)
    );
    assert!(ledger.pending(now_ns).is_empty(), "the invitation is spent");

    let replayed = ledger
        .completed(
            &committed,
            guest_cert.device_public_key,
            [44; 32],
            proof_digest,
            now_ns,
        )
        .unwrap();
    assert_eq!(
        replayed,
        Some(committed.filled(bootstrap).unwrap()),
        "a retry hears the answer the device was sent"
    );
    let later = || Ok(vec![b"later governance".to_vec()]);
    let moved = Governed {
        peer: (&candidate).into(),
        host: &host_cert,
        workspace_id: 7,
        generation: 3,
        governance_messages: Some(&later),
    };
    let refresh = ledger
        .completed(
            &moved,
            guest_cert.device_public_key,
            [44; 32],
            proof_digest,
            now_ns,
        )
        .unwrap_err();
    assert_eq!(refresh.code(), crate::join::JOIN_COMPLETED_REFRESH);
    assert!(
        ledger
            .redeem(
                &committed,
                guest_cert.device_public_key,
                &complete.proof,
                now_ns
            )
            .is_err(),
        "a spent invitation admits nobody again"
    );
}

/// `cert`, going by its own label, joins `workspace` through `ledger` under a
/// bearer invitation, saying `name` as it comes in: the workspace after it.
fn joined(
    ledger: &mut JoinLedger,
    workspace: &Governed<'_>,
    authority_key: &SigningKey,
    (key, cert): (&SigningKey, &DeviceCert),
    name: &str,
    seed: u8,
    now_ns: u64,
) -> StatePeer {
    let issued = ledger
        .issue(
            workspace,
            &workspace.peer.owner_certificates()[0],
            IssueInvitationRequest {
                request_id: [seed; 32],
                role: Role::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: now_ns.saturating_add(10_000_000_000),
                inviter_name: None,
            },
            Vec::new(),
            Signer {
                key: authority_key,
                nonce: [seed.wrapping_add(1); 16],
            },
            now_ns,
        )
        .unwrap();
    let challenge = ledger
        .challenge(
            workspace,
            cert.device_public_key,
            JoinChallengeRequest {
                request_id: [seed.wrapping_add(2); 32],
                signed_invitation: issued.signed_invitation,
                device_cert: cert.clone(),
            },
            [seed.wrapping_add(3); 32],
            now_ns,
        )
        .unwrap();
    let proof = sign_join_proof(challenge, key).unwrap();
    let redemption = ledger
        .redeem(workspace, cert.device_public_key, &proof, now_ns)
        .unwrap();
    let joins = [Redeeming {
        remote_endpoint: cert.device_public_key,
        request_id: [seed.wrapping_add(2); 32],
        proof_digest: request_digest(&proof).unwrap(),
        redemption: &redemption,
        name: Some(name),
    }];
    let mut candidate = (*workspace.peer).clone();
    let refusals = ledger.admit(&mut candidate, &joins, authority_key, now_ns);
    assert!(refusals.iter().all(Option::is_none), "{refusals:?}");
    candidate
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the governed workspace reads the peer through each step of the decision"
)]
fn a_key_made_to_act_for_a_person_never_names_them() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = || Ok(Vec::<Vec<u8>>::new());
    let host_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &host_cert,
        workspace_id: 7,
        generation: 1,
        governance_messages: Some(&messages),
    };
    let root = SigningKey::from_bytes(&[51; 32]);
    let mut ledger = JoinLedger::default();
    // Ada joins on a device of her own, going by her own name.
    let ada_key = SigningKey::from_bytes(&[52; 32]);
    let mut ada = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: ada_key.verifying_key().to_bytes(),
        device_label: "Ada".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut ada, &root).unwrap();
    let joined_ada = joined(
        &mut ledger,
        &workspace,
        &authority_key,
        (&ada_key, &ada),
        "Ada",
        60,
        now_ns,
    );
    let named = |member_of: &StatePeer| {
        member_of
            .governance()
            .governed_members(now)
            .unwrap()
            .into_iter()
            .find(|member| member.user_public_key == root.verifying_key().to_bytes())
            .unwrap()
            .display_name
    };
    assert_eq!(named(&joined_ada).as_deref(), Some("Ada"));
    // A key made to act for her joins too, saying what the key goes by, as
    // the door hands it over: it names the key, never Ada.
    let backend_key = SigningKey::from_bytes(&[53; 32]);
    let mut backend = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: backend_key.verifying_key().to_bytes(),
        device_label: "Ada's backend".into(),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: 2,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut backend, &root).unwrap();
    let again = Governed {
        peer: (&joined_ada).into(),
        host: &host_cert,
        workspace_id: 7,
        generation: 2,
        governance_messages: Some(&messages),
    };
    let joined_both = joined(
        &mut ledger,
        &again,
        &authority_key,
        (&backend_key, &backend),
        "Jane Doe's agent",
        70,
        now_ns,
    );
    assert_eq!(
        named(&joined_both).as_deref(),
        Some("Ada"),
        "a key of hers names its key, never the person it acts for"
    );
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the governed workspace reads the peer through each step of the decision"
)]
fn a_cancelled_invitation_is_not_redeemable_and_cancelling_again_changes_nothing() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = || Ok(Vec::<Vec<u8>>::new());
    let host_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &host_cert,
        workspace_id: 7,
        generation: 1,
        governance_messages: Some(&messages),
    };
    let mut ledger = JoinLedger::default();
    let issued = ledger
        .issue(
            &workspace,
            &peer.owner_certificates()[0],
            invitation(None, now_ns),
            Vec::new(),
            Signer {
                key: &authority_key,
                nonce: [43; 16],
            },
            now_ns,
        )
        .unwrap();
    let cancel = CancelInvitationRequest {
        request_id: [8; 32],
        invite_id: issued.invite_id,
    };
    assert!(ledger.cancel(&cancel).unwrap());
    assert!(!ledger.cancel(&cancel).unwrap());
    assert!(
        ledger
            .cancel(&CancelInvitationRequest {
                request_id: [8; 32],
                invite_id: [1; 32],
            })
            .is_err()
    );
    assert!(ledger.pending(now_ns).is_empty());
    assert!(
        ledger
            .inspect(&workspace, &issued.signed_invitation, now_ns)
            .is_err()
    );
}

#[test]
fn an_administration_request_is_answered_once_and_refused_for_another_operation() {
    let mut ledger = JoinLedger::default();
    assert!(matches!(
        ledger.administered([1; 32], [2; 32]).unwrap(),
        Administered::New
    ));
    ledger.record_administration([1; 32], [2; 32], 5);
    assert!(matches!(
        ledger.administered([1; 32], [2; 32]).unwrap(),
        Administered::Done(5)
    ));
    assert!(ledger.administered([1; 32], [3; 32]).is_err());
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the governed workspace reads the peer through each step of the decision"
)]
fn a_join_receipt_keeps_none_of_the_workspaces_state_and_replays_while_it_is_unchanged() {
    let (peer, _, _, _, _) = seeded();
    let messages = || Ok(vec![b"governance".to_vec()]);
    let host_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &host_cert,
        workspace_id: 7,
        generation: 1,
        governance_messages: Some(&messages),
    };
    let (_, cert) = guest();
    let admitted = workspace.admission([9; 32], cert, Role::Editor);
    let delivered = workspace.filled(admitted.clone()).unwrap();
    assert!(!delivered.certified_roster.is_empty());
    let receipt = JoinCompletionRecord::receipt([1; 32], [2; 32], [3; 32], admitted);
    assert!(receipt.bootstrap.certified_roster.is_empty());
    assert!(receipt.bootstrap.records.is_empty());
    assert!(receipt.bootstrap.governance_messages.is_empty());
    assert_eq!(receipt.replay(&workspace).unwrap(), delivered);
    // Any governance change since moves the workspace's generation on: the
    // reply delivered then is not the workspace now, and the member refreshes.
    let moved = Governed {
        generation: 2,
        ..workspace
    };
    assert!(receipt.replay(&moved).is_err());
}
