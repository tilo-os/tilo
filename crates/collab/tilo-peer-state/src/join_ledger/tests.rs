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
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

fn invitation(recipient: Option<[u8; 32]>, now: u64) -> IssueInvitationRequest {
    IssueInvitationRequest {
        request_id: [7; 32],
        role: MemberRole::Editor,
        recipient_user_public_key: recipient,
        invitee_name: Some("Guest".into()),
        expires_at_ns: now.saturating_add(10_000_000_000),
        inviter_name: None,
    }
}

/// One ledger serves every governor: issuing, challenging, redeeming and
/// completing a join through it admits the device into the room, answers
/// a retry of the same Complete with the same bootstrap, and spends the
/// invitation.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one join read top to bottom, through each step of the ledger"
)]
fn a_join_through_the_ledger_admits_once_and_replays_its_answer() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = || vec![b"governance".to_vec()];
    let room = Governed {
        peer: &peer,
        workspace_id: 7,
        generation: 1,
        governance_messages: &messages,
    };
    let (guest_key, guest_cert) = guest();
    let mut ledger = JoinLedger::default();
    assert!(ledger.is_empty());

    let issued = ledger
        .issue(
            &room,
            peer.authority(),
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
            &room,
            peer.authority(),
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
            &room,
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
            .redeem(&room, [9; 32], &complete.proof, now_ns)
            .is_err(),
        "a proof is redeemed only from the endpoint it was challenged on"
    );
    let redemption = ledger
        .redeem(&room, guest_cert.device_public_key, &complete.proof, now_ns)
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
        "the room names the guest as they named themself, not by their device"
    );
    let committed = Governed {
        peer: &candidate,
        workspace_id: 7,
        generation: 2,
        governance_messages: &messages,
    };
    let delivered = ledger.complete(&committed, &joins, &refusals).unwrap();
    let bootstrap = delivered.into_iter().next().unwrap().unwrap();
    assert_eq!(bootstrap.device_cert, guest_cert);
    assert_eq!(bootstrap.generation, 2);
    assert!(
        candidate
            .permissions(guest_cert.device_public_key, now)
            .is_ok(),
        "the guest is a member of the room"
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
    assert_eq!(replayed, Some(bootstrap), "a retry hears the same answer");
    let later = || vec![b"later governance".to_vec()];
    let moved = Governed {
        generation: 3,
        governance_messages: &later,
        ..committed
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

#[test]
fn a_cancelled_invitation_is_not_redeemable_and_cancelling_again_changes_nothing() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = Vec::<Vec<u8>>::new;
    let room = Governed {
        peer: &peer,
        workspace_id: 7,
        generation: 1,
        governance_messages: &messages,
    };
    let mut ledger = JoinLedger::default();
    let issued = ledger
        .issue(
            &room,
            peer.authority(),
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
            .inspect(&room, &issued.signed_invitation, now_ns)
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
fn a_join_receipt_keeps_none_of_the_rooms_state_and_replays_while_it_is_unchanged() {
    let (peer, _, _, _, _) = seeded();
    let messages = || vec![b"governance".to_vec()];
    let room = Governed {
        peer: &peer,
        workspace_id: 7,
        generation: 1,
        governance_messages: &messages,
    };
    let (_, cert) = guest();
    let delivered = room.bootstrap([9; 32], cert, MemberRole::Editor);
    assert!(!delivered.certified_roster.is_empty());
    let receipt = JoinCompletionRecord::receipt([1; 32], [2; 32], [3; 32], &delivered).unwrap();
    assert!(receipt.bootstrap.certified_roster.is_empty());
    assert!(receipt.bootstrap.records.is_empty());
    assert_eq!(receipt.replay(&room).unwrap(), delivered);
}
