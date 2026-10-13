//! Transport-neutral characterization tests for collaboration governance.
//!
//! These intentionally exercise the state shared by Cap'n Proto and peer
//! ingress. Transport tests should prove framing and routing; policy belongs
//! here so retiring an RPC dialect cannot silently retire its guarantees.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use channel_crypto::{ChannelGroup, MemberKey};
use collab_fixtures::{role_attestation, signed_op, signed_op_encrypted};
use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, ChannelId, ChannelState, CompactionCheckpoint,
    Ed25519PublicKey, EncryptionMetadata, OpId, OpKind, RedactRequest, RedactedMarker, Role,
    SignedOp, ValidationError, encode_compaction_payload, encode_redact_payload, generate_keypair,
    hash_op_prefix, hash_target_op, parse_compaction_payload, validate_inbound_op,
};
use ed25519_dalek::SigningKey;
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;
use shell::shell::collab_broker::{CrdtRoute, LocalIdentity, PeerLabels};
use shell::shell::collab_core::{BrokerChannels, apply_redaction_from_envelope};
use threshold_identity::{
    ThresholdIdentity,
    signing::{SigningRound1, SigningRound1Commitment, SigningRound2Partial},
};

struct Fixture {
    owner_sk: SigningKey,
    owner_pk: Ed25519PublicKey,
    editor_sk: SigningKey,
    editor_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    owner_att: AttestationId,
    editor_att: AttestationId,
    state: ChannelState,
}

#[allow(
    clippy::similar_names,
    reason = "sk/pk (secret key/public key) is the standard cryptography naming pair; \
              renaming either away from it would hurt readability, not help it"
)]
fn fixture() -> Fixture {
    let (owner_sk, owner_pk) = generate_keypair();
    let (editor_sk, editor_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();
    let owner = role_attestation(&owner_sk, owner_pk, owner_pk, channel, Role::Owner);
    let editor = role_attestation(&owner_sk, owner_pk, editor_pk, channel, Role::Editor);
    let owner_att = owner.attestation.id;
    let editor_att = editor.attestation.id;
    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(owner);
    state.register_attestation(editor);
    Fixture {
        owner_sk,
        owner_pk,
        editor_sk,
        editor_pk,
        artifact,
        owner_att,
        editor_att,
        state,
    }
}

fn redact(f: &Fixture, target: &SignedOp, author: Role, seq: u64) -> SignedOp {
    let (sk, pk, att) = match author {
        Role::Owner => (&f.owner_sk, f.owner_pk, f.owner_att),
        _ => (&f.editor_sk, f.editor_pk, f.editor_att),
    };
    let req = RedactRequest {
        artifact: f.artifact,
        target_op_id: OpId::from_op(target),
        target_op_hash: hash_target_op(target).unwrap(),
        reason: "retention policy".into(),
        redacted_by: pk,
        redacted_at: SystemTime::now(),
    };
    signed_op(
        sk,
        pk,
        f.artifact,
        att,
        OpKind::Redaction,
        encode_redact_payload(&req).unwrap(),
        seq,
    )
}

fn threshold_pair() -> (ThresholdIdentity, ThresholdIdentity) {
    let mut rng_a = ChaCha20Rng::from_seed([7; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([8; 32]);
    let mut a = ThresholdIdentity::bootstrap_single_device(&mut rng_a).unwrap();
    let mut b = ThresholdIdentity::bootstrap_single_device(&mut rng_b).unwrap();
    let (mut session_a, round_a) = a.begin_enrolment(1, 2, 2, &mut rng_a).unwrap();
    let (mut session_b, round_b) = b.begin_enrolment(2, 2, 2, &mut rng_b).unwrap();
    let second_a = session_a.receive_round1(&[round_b]).unwrap();
    let second_b = session_b.receive_round1(&[round_a]).unwrap();
    session_a.receive_round2(&[second_b[&1].clone()]).unwrap();
    session_b.receive_round2(&[second_a[&2].clone()]).unwrap();
    a.adopt_dkg_outcome(session_a).unwrap();
    b.adopt_dkg_outcome(session_b).unwrap();
    (a, b)
}

fn threshold_sign(
    a: &ThresholdIdentity,
    b: &ThresholdIdentity,
    bytes: &[u8],
) -> collab_types::Signature {
    let mut rng_a = ChaCha20Rng::from_seed([40; 32]);
    let mut rng_b = ChaCha20Rng::from_seed([60; 32]);
    let round_a: SigningRound1 = a.sign_round1(&mut rng_a).unwrap();
    let round_b: SigningRound1 = b.sign_round1(&mut rng_b).unwrap();
    let commitments: Vec<SigningRound1Commitment> =
        vec![round_a.commitment.clone(), round_b.commitment.clone()];
    let partial_a: SigningRound2Partial = a
        .sign_round2_raw(round_a.nonces, bytes, &commitments)
        .unwrap();
    let partial_b: SigningRound2Partial = b
        .sign_round2_raw(round_b.nonces, bytes, &commitments)
        .unwrap();
    a.aggregate_signature(&[partial_a, partial_b]).unwrap()
}

#[test]
#[allow(
    clippy::similar_names,
    reason = "sk/pk (secret key/public key) is the standard cryptography naming pair; \
renaming either away from it would hurt readability, not help it"
)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "state borrows out of guard and is used through the end of the test, \
including the compliance-record recheck after compaction, so the guard \
must stay held for the whole function"
)]
fn signed_and_mls_ops_validate_replay_and_redact_without_decryption() {
    // Phase `signed_ops_accept_valid_reject_invalid_and_replay`.
    {
        let mut f = fixture();
        let valid = signed_op(
            &f.editor_sk,
            f.editor_pk,
            f.artifact,
            f.editor_att,
            OpKind::Change,
            b"valid".to_vec(),
            1,
        );
        assert_eq!(validate_inbound_op(&valid, &mut f.state), Ok(()));
        f.state.record_validated_op(valid.clone());
        assert!(matches!(
            validate_inbound_op(&valid, &mut f.state),
            Err(ValidationError::ReplaySeq { .. })
        ));

        let (outsider_sk, outsider_pk) = generate_keypair();
        let invalid = signed_op(
            &outsider_sk,
            outsider_pk,
            f.artifact,
            AttestationId::new(),
            OpKind::Change,
            b"forged".to_vec(),
            1,
        );
        assert!(matches!(
            validate_inbound_op(&invalid, &mut f.state),
            Err(ValidationError::UnknownAttestation(_))
        ));
    }

    // Phase `threshold_mls_validation_and_compaction_compose`.
    {
        let (owner_sk, owner_pk) = generate_keypair();
        let (threshold_a, threshold_b) = threshold_pair();
        let threshold_pk = *threshold_a.group_public_key();
        let channel = ChannelId::new();
        let artifact = ArtifactId::new();
        let editor = role_attestation(&owner_sk, owner_pk, threshold_pk, channel, Role::Editor);
        let owner = role_attestation(&owner_sk, owner_pk, owner_pk, channel, Role::Owner);
        let editor_att = editor.attestation.id;
        let mut state = ChannelState::default();
        state.authorised_signers.push(owner_pk);
        state.artifacts.insert(artifact, ArtifactKind::Note);
        state.register_attestation(editor);
        state.register_attestation(owner);
        let alice_key = MemberKey::generate("alice").unwrap();
        let bob_key = MemberKey::generate("bob").unwrap();
        let bob_package = bob_key.fresh_key_package().unwrap();
        let mut alice = ChannelGroup::create(channel, alice_key).unwrap();
        let welcome = alice.add_member(bob_package).unwrap();
        let mut bob = ChannelGroup::process_welcome(channel, &welcome, bob_key).unwrap();

        let ciphertext = alice.encrypt(b"composed edit").unwrap();
        let mut op = SignedOp::new_unsigned_with_encryption(
            ciphertext,
            artifact,
            threshold_pk,
            editor_att,
            OpKind::Change,
            SystemTime::now(),
            1,
            EncryptionMetadata::mls_application(alice.epoch(), channel),
        );
        op.signature = threshold_sign(&threshold_a, &threshold_b, &op.signing_bytes().unwrap());
        validate_inbound_op(&op, &mut state).unwrap();
        assert_eq!(bob.decrypt(&op.payload).unwrap(), b"composed edit");
        state.record_validated_op(op.clone());

        let checkpoint = CompactionCheckpoint {
            artifact,
            up_to_seq: 1,
            prefix_hash: hash_op_prefix(&[op]).unwrap(),
        };
        assert_eq!(state.apply_compaction(&checkpoint).unwrap(), 1);
        assert!(state.scrollback_for(artifact, 2, 10).1);
    }

    // Phase `mls_ciphertext_can_be_redacted_without_decryption`.
    {
        let mut f = fixture();
        let target = signed_op_encrypted(
            &f.editor_sk,
            f.editor_pk,
            f.artifact,
            f.editor_att,
            OpKind::Change,
            b"opaque MLS ciphertext".to_vec(),
            1,
            EncryptionMetadata::mls_application(7, ChannelId::new()),
        );
        validate_inbound_op(&target, &mut f.state).unwrap();
        f.state.record_validated_op(target.clone());
        let op = redact(&f, &target, Role::Owner, 1);
        validate_inbound_op(&op, &mut f.state).unwrap();
        f.state
            .apply_redaction(&collab_types::parse_redact_payload(&op).unwrap())
            .unwrap();
        assert!(f.state.is_redacted(OpId::from_op(&target)));
    }
}

#[test]
fn participant_names_use_room_then_handle_then_local_id_without_raw_peer_ids() {
    const HANDLE: u64 = 5_537_458_103_208_628_978;
    let named = LocalIdentity::with_lookup(HANDLE, |_| {
        let mut labels = PeerLabels::new();
        labels.insert(HANDLE, "Xaver".into());
        labels
    });
    let route = CrdtRoute::cluster("design-review");
    for announced in [Some("peer@5537458103208628978"), None] {
        let resolved = named.resolve(Some(&route), announced);
        assert_eq!(resolved, "Xaver");
        assert!(!resolved.starts_with("peer-") && !resolved.starts_with("peer@"));
    }

    let unnamed = LocalIdentity::with_lookup(HANDLE, |_| PeerLabels::new());
    assert_eq!(
        unnamed.resolve(Some(&route), Some("tilo-agent")),
        "tilo-agent"
    );
    let fallback = unnamed.resolve(Some(&route), None);
    assert!(!fallback.is_empty());
    assert!(!fallback.starts_with("peer-") && !fallback.starts_with("peer@"));
}

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "guard borrows out `state` and `records`, used through the compliance-record \
recheck after compaction at the end of the test, so the guard must stay held \
for the whole function"
)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn redaction_scrollback_and_compaction_compose_under_one_policy() {
    // Phase `redaction_authorization_markers_filtering_and_persistence_are_one_policy`.
    {
        let mut f = fixture();
        let target = signed_op(
            &f.editor_sk,
            f.editor_pk,
            f.artifact,
            f.editor_att,
            OpKind::Change,
            b"secret".to_vec(),
            1,
        );
        validate_inbound_op(&target, &mut f.state).unwrap();
        f.state.record_validated_op(target.clone());

        let denied = redact(&f, &target, Role::Editor, 2);
        assert!(matches!(
            validate_inbound_op(&denied, &mut f.state),
            Err(ValidationError::NotGranted(_))
        ));

        let accepted = redact(&f, &target, Role::Owner, 10);
        validate_inbound_op(&accepted, &mut f.state).unwrap();
        f.state.record_validated_op(accepted.clone());
        let recursive = redact(&f, &accepted, Role::Owner, 11);
        let channels = Arc::new(Mutex::new(BrokerChannels::default()));
        channels
            .lock()
            .unwrap()
            .register_channel_state(f.artifact, f.state);
        apply_redaction_from_envelope(&accepted, f.artifact, &channels);

        let mut guard = channels.lock().unwrap();
        let records = guard.redactions.for_artifact(f.artifact);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].reason, "retention policy");
        assert_eq!(records[0].redactor_pk, f.owner_pk);
        let state = guard.state_for(f.artifact);
        let (visible, _) = state.scrollback_for(f.artifact, u64::MAX, 20);
        let rewritten = visible.iter().find(|op| op.seq == 1).unwrap().clone();
        assert_eq!(
            postcard::from_bytes::<RedactedMarker>(&rewritten.payload).unwrap(),
            RedactedMarker::v1()
        );

        // Recursive redaction is forbidden, and an original arriving only after
        // its redaction is remembered is suppressed by the same state.
        assert!(matches!(
            validate_inbound_op(&recursive, state),
            Err(ValidationError::RecursiveRedaction { .. })
        ));

        let checkpoint = CompactionCheckpoint {
            artifact: f.artifact,
            up_to_seq: 1,
            prefix_hash: hash_op_prefix(&[rewritten]).unwrap(),
        };
        state.apply_compaction(&checkpoint).unwrap();
        assert_eq!(
            guard.redactions.for_artifact(f.artifact).len(),
            1,
            "the compliance record must outlive op-log compaction"
        );
    }

    // Phase `a_redaction_received_before_its_target_suppresses_the_late_op`.
    {
        let mut f = fixture();
        let late = signed_op(
            &f.editor_sk,
            f.editor_pk,
            f.artifact,
            f.editor_att,
            OpKind::Change,
            b"arrives later".to_vec(),
            100,
        );
        let redaction = redact(&f, &late, Role::Owner, 1);
        validate_inbound_op(&redaction, &mut f.state).unwrap();
        f.state.record_validated_op(redaction.clone());
        f.state
            .apply_redaction(&collab_types::parse_redact_payload(&redaction).unwrap())
            .unwrap();
        assert!(matches!(
            validate_inbound_op(&late, &mut f.state),
            Err(ValidationError::OpAlreadyRedacted { .. })
        ));
    }

    // Phase `scrollback_recent_empty_paging_compaction_and_authorization_compose`.
    {
        let mut f = fixture();
        let mut ops = Vec::new();
        for seq in 1..=5 {
            let op = signed_op(
                &f.editor_sk,
                f.editor_pk,
                f.artifact,
                f.editor_att,
                OpKind::Change,
                format!("op-{seq}").into_bytes(),
                seq,
            );
            validate_inbound_op(&op, &mut f.state).unwrap();
            f.state.record_validated_op(op.clone());
            ops.push(op);
        }
        let (recent, exhausted) = f.state.scrollback_for(f.artifact, u64::MAX, 2);
        assert_eq!(recent.iter().map(|o| o.seq).collect::<Vec<_>>(), [4, 5]);
        assert!(!exhausted);
        let (first_page, exhausted) = f.state.scrollback_for(f.artifact, 4, 10);
        assert_eq!(
            first_page.iter().map(|o| o.seq).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(exhausted);
        assert_eq!(
            f.state.scrollback_for(ArtifactId::new(), u64::MAX, 10),
            (vec![], true)
        );

        let checkpoint = CompactionCheckpoint {
            artifact: f.artifact,
            up_to_seq: 3,
            prefix_hash: hash_op_prefix(&ops[..3]).unwrap(),
        };
        let payload = encode_compaction_payload(&checkpoint).unwrap();
        let editor_compaction = signed_op(
            &f.editor_sk,
            f.editor_pk,
            f.artifact,
            f.editor_att,
            OpKind::Compaction,
            payload.clone(),
            10,
        );
        assert!(matches!(
            validate_inbound_op(&editor_compaction, &mut f.state),
            Err(ValidationError::NotGranted(_))
        ));
        let owner_compaction = signed_op(
            &f.owner_sk,
            f.owner_pk,
            f.artifact,
            f.owner_att,
            OpKind::Compaction,
            payload,
            100,
        );
        validate_inbound_op(&owner_compaction, &mut f.state).unwrap();
        f.state.record_validated_op(owner_compaction.clone());
        assert_eq!(
            f.state
                .apply_compaction(&parse_compaction_payload(&owner_compaction).unwrap())
                .unwrap(),
            3
        );
        assert_eq!(f.state.scrollback_for(f.artifact, 4, 10), (vec![], true));
        assert_eq!(
            f.state
                .scrollback_for(f.artifact, u64::MAX, 10)
                .0
                .iter()
                .map(|o| o.seq)
                .collect::<Vec<_>>(),
            [4, 5, 100]
        );
    }
}
