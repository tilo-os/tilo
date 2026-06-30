//! Exercises the eight-step inbound-op validation pipeline.
//!
//! For each numbered step there is at least one negative test that
//! drives the validator to the corresponding `ValidationError`
//! variant, plus one happy-path test that runs all eight green.
//!
//! The test fixture builds a minimal channel: one owner-signer, one
//! Editor attestation underwriting an author, one Note artifact, no
//! clearance gate by default. Individual tests mutate the fixture
//! to trip the desired step.

use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use qos_collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    Clearance, Ed25519PublicKey, OpClass, Role, RoleAttestation, Signature, SignedOp,
    ValidationError, generate_keypair, sign_event, sign_op, validate_inbound_op,
};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

/// Build + sign a role attestation. The trust root (`issuer`) signs
/// over the attestation's `signing_bytes()` view.
#[allow(clippy::too_many_arguments)]
fn make_role_attestation(
    issuer_sk: &SigningKey,
    issuer_pk: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
    valid_from: SystemTime,
    valid_until: SystemTime,
    claims: Vec<Claim>,
) -> RoleAttestation {
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pk,
        subject,
        claims,
        valid_from,
        valid_until,
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = att.signing_bytes().unwrap();
    let sig = sign_event(&sb, issuer_sk).unwrap();
    att.signature = sig;
    RoleAttestation {
        channel,
        role,
        attestation: att,
    }
}

/// Fixture: an owner, an author, an Editor attestation, one Note,
/// no clearance gate, sig-on-op already applied.
#[allow(dead_code)]
struct Fixture {
    owner_sk: SigningKey,
    owner_pk: Ed25519PublicKey,
    author_sk: SigningKey,
    author_pk: Ed25519PublicKey,
    artifact: ArtifactId,
    channel: ChannelId,
    attestation_id: AttestationId,
    state: ChannelState,
}

fn fixture() -> Fixture {
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let ra = make_role_attestation(
        &owner_sk,
        owner_pk,
        author_pk,
        channel,
        Role::Editor,
        t(0),
        t(100_000),
        vec![],
    );
    let attestation_id = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    Fixture {
        owner_sk,
        owner_pk,
        author_sk,
        author_pk,
        artifact,
        channel,
        attestation_id,
        state,
    }
}

fn build_op(
    f: &Fixture,
    op_class: OpClass,
    timestamp: SystemTime,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        b"payload".to_vec(),
        f.artifact,
        f.author_pk,
        f.attestation_id,
        op_class,
        timestamp,
        seq,
    );
    sign_op(&mut op, &f.author_sk).unwrap();
    op
}

// ---------- happy path ----------

#[test]
fn happy_path_all_eight_pass() {
    let mut f = fixture();
    let op = build_op(&f, OpClass::Splice, t(10), 1);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_ok(), "expected Ok, got {r:?}");
    // Seq map updated.
    assert_eq!(f.state.last_seq_per_author.get(&f.author_pk).copied(), Some(1));
}

#[test]
fn happy_path_with_clearance_gate() {
    let mut f = fixture();
    // Stamp a ClearedAt(Confidential) identity attestation issued
    // by a trust root that the channel accepts.
    let (org_sk, org_pk) = generate_keypair();
    f.state.accepted_trust_roots.push(org_pk);
    let cleared = {
        let mut a = Attestation {
            id: AttestationId::new(),
            issued_by: org_pk,
            subject: f.author_pk,
            claims: vec![Claim::ClearedAt(Clearance::CONFIDENTIAL)],
            valid_from: t(0),
            valid_until: t(100_000),
            signature: Signature::from_bytes([0u8; 64]),
        };
        let sb = a.signing_bytes().unwrap();
        a.signature = sign_event(&sb, &org_sk).unwrap();
        a
    };
    f.state
        .identity_attestations
        .entry(f.author_pk)
        .or_default()
        .push(cleared);
    // Set the artifact's classification to Internal (lower than the
    // author's Confidential clearance).
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::INTERNAL);

    let op = build_op(&f, OpClass::Splice, t(10), 1);
    assert!(validate_inbound_op(&op, &mut f.state).is_ok());
}

// ---------- step 1: unknown attestation ----------

#[test]
fn step1_unknown_attestation_id() {
    let mut f = fixture();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        f.artifact,
        f.author_pk,
        AttestationId::new(), // unknown to the validator
        OpClass::Splice,
        t(10),
        1,
    );
    sign_op(&mut op, &f.author_sk).unwrap();
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::UnknownAttestation(_)) => {}
        other => panic!("expected UnknownAttestation, got {other:?}"),
    }
}

// ---------- step 2: attestation issuer not authorised ----------

#[test]
fn step2_attestation_issuer_not_in_authorised_signers() {
    let mut f = fixture();
    // Wipe the channel's authorised signers. The attestation in the
    // store was issued by `owner_pk`, which no longer qualifies.
    f.state.authorised_signers.clear();
    let op = build_op(&f, OpClass::Splice, t(10), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::UnauthorisedAttestation(k)) => {
            assert_eq!(k, f.owner_pk);
        }
        other => panic!("expected UnauthorisedAttestation, got {other:?}"),
    }
}

#[test]
fn step2_attestation_signature_corrupted() {
    let mut f = fixture();
    // Mutate the stored attestation's signature so the inner
    // `verify_event` check fails.
    let ra = f.state.attestations.get_mut(&f.attestation_id).unwrap();
    ra.attestation.signature = Signature::from_bytes([0xff; 64]);

    let op = build_op(&f, OpClass::Splice, t(10), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::BadAttestationSignature(_)) => {}
        other => panic!("expected BadAttestationSignature, got {other:?}"),
    }
}

// ---------- step 3: bad op signature ----------

#[test]
fn step3_op_signature_corrupted() {
    let mut f = fixture();
    let mut op = build_op(&f, OpClass::Splice, t(10), 1);
    op.signature = Signature::from_bytes([0xff; 64]);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::BadOpSignature(_)) => {}
        other => panic!("expected BadOpSignature, got {other:?}"),
    }
}

#[test]
fn step3_op_payload_tampered_post_signing() {
    let mut f = fixture();
    let mut op = build_op(&f, OpClass::Splice, t(10), 1);
    op.payload[0] ^= 0xff; // tamper after signing
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::BadOpSignature(_)) => {}
        other => panic!("expected BadOpSignature, got {other:?}"),
    }
}

// ---------- step 4: author != attestation.subject ----------

#[test]
fn step4_author_does_not_match_subject() {
    let mut f = fixture();
    // Forge an op whose author is *not* the attestation's subject.
    // To get past step 3 (op signature check) we sign with a
    // matching key.
    let (forger_sk, forger_pk) = generate_keypair();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        f.artifact,
        forger_pk,
        f.attestation_id,
        OpClass::Splice,
        t(10),
        1,
    );
    sign_op(&mut op, &forger_sk).unwrap();
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::AuthorMismatch) => {}
        other => panic!("expected AuthorMismatch, got {other:?}"),
    }
}

// ---------- step 5: role does not permit op class ----------

#[test]
fn step5_commenter_cannot_splice() {
    // Re-issue the attestation as Commenter, then try to splice.
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let ra = make_role_attestation(
        &owner_sk,
        owner_pk,
        author_pk,
        channel,
        Role::Commenter,
        t(0),
        t(100_000),
        vec![],
    );
    let attestation_id = ra.attestation.id;
    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        artifact,
        author_pk,
        attestation_id,
        OpClass::Splice, // commenter can't splice
        t(10),
        1,
    );
    sign_op(&mut op, &author_sk).unwrap();
    match validate_inbound_op(&op, &mut state) {
        Err(ValidationError::RoleForbidsOpClass) => {}
        other => panic!("expected RoleForbidsOpClass, got {other:?}"),
    }
}

#[test]
fn step5_unknown_artifact() {
    let mut f = fixture();
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        ArtifactId::new(), // not registered with the channel
        f.author_pk,
        f.attestation_id,
        OpClass::Splice,
        t(10),
        1,
    );
    sign_op(&mut op, &f.author_sk).unwrap();
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::UnknownArtifact(_)) => {}
        other => panic!("expected UnknownArtifact, got {other:?}"),
    }
}

// ---------- step 6: outside validity window ----------

#[test]
fn step6_op_after_valid_until() {
    let mut f = fixture();
    // Attestation valid until t(100_000); op at t(100_001).
    let op = build_op(&f, OpClass::Splice, t(100_001), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::OutsideValidityWindow) => {}
        other => panic!("expected OutsideValidityWindow (after), got {other:?}"),
    }
}

#[test]
fn step6_op_before_valid_from() {
    // Use a fresh fixture and bump valid_from to a non-zero value.
    let (owner_sk, owner_pk) = generate_keypair();
    let (author_sk, author_pk) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let ra = make_role_attestation(
        &owner_sk,
        owner_pk,
        author_pk,
        channel,
        Role::Editor,
        t(100),
        t(200),
        vec![],
    );
    let attestation_id = ra.attestation.id;
    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pk);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        artifact,
        author_pk,
        attestation_id,
        OpClass::Splice,
        t(50), // before valid_from
        1,
    );
    sign_op(&mut op, &author_sk).unwrap();
    match validate_inbound_op(&op, &mut state) {
        Err(ValidationError::OutsideValidityWindow) => {}
        other => panic!("expected OutsideValidityWindow (before), got {other:?}"),
    }
}

// ---------- step 7: replay seq ----------

#[test]
fn step7_replay_same_seq_rejected() {
    let mut f = fixture();
    let op1 = build_op(&f, OpClass::Splice, t(10), 1);
    let op2 = build_op(&f, OpClass::Splice, t(11), 1); // same seq as op1
    assert!(validate_inbound_op(&op1, &mut f.state).is_ok());
    match validate_inbound_op(&op2, &mut f.state) {
        Err(ValidationError::ReplaySeq { seen, incoming }) => {
            assert_eq!(seen, 1);
            assert_eq!(incoming, 1);
        }
        other => panic!("expected ReplaySeq, got {other:?}"),
    }
}

#[test]
fn step7_smaller_seq_rejected() {
    let mut f = fixture();
    let op1 = build_op(&f, OpClass::Splice, t(10), 10);
    let op2 = build_op(&f, OpClass::Splice, t(11), 5);
    assert!(validate_inbound_op(&op1, &mut f.state).is_ok());
    match validate_inbound_op(&op2, &mut f.state) {
        Err(ValidationError::ReplaySeq { .. }) => {}
        other => panic!("expected ReplaySeq, got {other:?}"),
    }
}

// ---------- step 8: clearance ----------

#[test]
fn step8_insufficient_clearance_no_claim_at_all() {
    let mut f = fixture();
    // Set artifact classification to Confidential without giving
    // the author any clearance.
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::CONFIDENTIAL);
    let op = build_op(&f, OpClass::Splice, t(10), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::InsufficientClearance { required, found }) => {
            assert_eq!(required, Clearance::CONFIDENTIAL);
            assert_eq!(found, None);
        }
        other => panic!("expected InsufficientClearance(None), got {other:?}"),
    }
}

#[test]
fn step8_insufficient_clearance_too_low() {
    let mut f = fixture();
    // Give the author Internal but require Confidential.
    let (org_sk, org_pk) = generate_keypair();
    f.state.accepted_trust_roots.push(org_pk);
    let mut a = Attestation {
        id: AttestationId::new(),
        issued_by: org_pk,
        subject: f.author_pk,
        claims: vec![Claim::ClearedAt(Clearance::INTERNAL)],
        valid_from: t(0),
        valid_until: t(100_000),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = a.signing_bytes().unwrap();
    a.signature = sign_event(&sb, &org_sk).unwrap();
    f.state
        .identity_attestations
        .entry(f.author_pk)
        .or_default()
        .push(a);

    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::CONFIDENTIAL);

    let op = build_op(&f, OpClass::Splice, t(10), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::InsufficientClearance { required, found }) => {
            assert_eq!(required, Clearance::CONFIDENTIAL);
            assert_eq!(found, Some(Clearance::INTERNAL));
        }
        other => panic!("expected InsufficientClearance(too-low), got {other:?}"),
    }
}

#[test]
fn step8_untrusted_clearance_issuer() {
    let mut f = fixture();
    // Give the author a Confidential clearance but issued by a key
    // that the channel does NOT accept as a trust root.
    let (rogue_sk, rogue_pk) = generate_keypair();
    let mut a = Attestation {
        id: AttestationId::new(),
        issued_by: rogue_pk,
        subject: f.author_pk,
        claims: vec![Claim::ClearedAt(Clearance::CONFIDENTIAL)],
        valid_from: t(0),
        valid_until: t(100_000),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = a.signing_bytes().unwrap();
    a.signature = sign_event(&sb, &rogue_sk).unwrap();
    f.state
        .identity_attestations
        .entry(f.author_pk)
        .or_default()
        .push(a);
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::CONFIDENTIAL);
    // accepted_trust_roots is empty.

    let op = build_op(&f, OpClass::Splice, t(10), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::UntrustedClearanceIssuer { issuer }) => {
            assert_eq!(issuer, rogue_pk);
        }
        other => panic!("expected UntrustedClearanceIssuer, got {other:?}"),
    }
}

#[test]
fn step8_channel_min_clearance_floor() {
    let mut f = fixture();
    // Set channel-level min_clearance to Restricted; artifact-level
    // classification is unset. The validator must still enforce.
    f.state.channel_min_clearance = Some(Clearance::RESTRICTED);
    let op = build_op(&f, OpClass::Splice, t(10), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::InsufficientClearance { required, found: _ }) => {
            assert_eq!(required, Clearance::RESTRICTED);
        }
        other => panic!("expected InsufficientClearance, got {other:?}"),
    }
}

#[test]
fn step8_effective_clearance_takes_max() {
    // Channel = Internal, artifact = Restricted. Author has
    // Confidential. Should fail (Confidential < Restricted).
    let mut f = fixture();
    f.state.channel_min_clearance = Some(Clearance::INTERNAL);
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::RESTRICTED);

    let (org_sk, org_pk) = generate_keypair();
    f.state.accepted_trust_roots.push(org_pk);
    let mut a = Attestation {
        id: AttestationId::new(),
        issued_by: org_pk,
        subject: f.author_pk,
        claims: vec![Claim::ClearedAt(Clearance::CONFIDENTIAL)],
        valid_from: t(0),
        valid_until: t(100_000),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = a.signing_bytes().unwrap();
    a.signature = sign_event(&sb, &org_sk).unwrap();
    f.state
        .identity_attestations
        .entry(f.author_pk)
        .or_default()
        .push(a);

    let op = build_op(&f, OpClass::Splice, t(10), 1);
    match validate_inbound_op(&op, &mut f.state) {
        Err(ValidationError::InsufficientClearance { required, found }) => {
            assert_eq!(required, Clearance::RESTRICTED);
            assert_eq!(found, Some(Clearance::CONFIDENTIAL));
        }
        other => panic!("expected InsufficientClearance, got {other:?}"),
    }
}

// ---------- state mutation: replay slot only bumps on success ----------

#[test]
fn replay_slot_does_not_bump_on_failure() {
    let mut f = fixture();
    // Inject a tampered op that fails step 3.
    let mut op = build_op(&f, OpClass::Splice, t(10), 5);
    op.signature = Signature::from_bytes([0xff; 64]);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_err());
    assert!(!f.state.last_seq_per_author.contains_key(&f.author_pk));
}

#[test]
fn replay_slot_bumps_on_success() {
    let mut f = fixture();
    let op = build_op(&f, OpClass::Splice, t(10), 42);
    assert!(validate_inbound_op(&op, &mut f.state).is_ok());
    assert_eq!(
        f.state.last_seq_per_author.get(&f.author_pk).copied(),
        Some(42)
    );
}

// ---------- surface-stream ops: role-table decision pinned ----------
//
// Hosting a live-streamed Wayland surface is authorial: the host is
// publishing live pixels of an app every channel member will see, and
// remote input mutates the live app's state on the host's machine. The
// role table in `src/role.rs` therefore pins all four surface-stream
// op classes to Editor+. These tests drive the *whole* pipeline —
// signature, attestation, role-table — to lock that decision in
// against accidental changes to `role_permits`.

#[test]
fn surface_stream_start_editor_permitted() {
    let mut f = fixture();
    // The default fixture issues an Editor attestation.
    let op = build_op(&f, OpClass::SurfaceStreamStart, t(10), 1);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_ok(), "expected Editor to start a stream, got {r:?}");
}

#[test]
fn surface_stream_end_editor_permitted() {
    let mut f = fixture();
    let op = build_op(&f, OpClass::SurfaceStreamEnd, t(10), 1);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_ok(), "expected Editor to end a stream, got {r:?}");
}

#[test]
fn surface_frame_editor_permitted() {
    let mut f = fixture();
    let op = build_op(&f, OpClass::SurfaceFrame, t(10), 1);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_ok(), "expected Editor to push a frame, got {r:?}");
}

#[test]
fn remote_input_editor_permitted() {
    let mut f = fixture();
    let op = build_op(&f, OpClass::RemoteInput, t(10), 1);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_ok(), "expected Editor to inject input, got {r:?}");
}

#[test]
fn surface_stream_start_commenter_rejected() {
    // Rebuild the fixture with a Commenter attestation and assert
    // every surface-stream op class lands on RoleForbidsOpClass.
    for op_class in [
        OpClass::SurfaceStreamStart,
        OpClass::SurfaceStreamEnd,
        OpClass::SurfaceFrame,
        OpClass::RemoteInput,
    ] {
        let (owner_sk, owner_pk) = generate_keypair();
        let (author_sk, author_pk) = generate_keypair();
        let channel = ChannelId::new();
        let artifact = ArtifactId::new();

        let ra = make_role_attestation(
            &owner_sk,
            owner_pk,
            author_pk,
            channel,
            Role::Commenter,
            t(0),
            t(100_000),
            vec![],
        );
        let attestation_id = ra.attestation.id;
        let mut state = ChannelState::default();
        state.authorised_signers.push(owner_pk);
        state.artifacts.insert(artifact, ArtifactKind::Cluster);
        state.register_attestation(ra);

        let mut op = SignedOp::new_unsigned(
            b"x".to_vec(),
            artifact,
            author_pk,
            attestation_id,
            op_class,
            t(10),
            1,
        );
        sign_op(&mut op, &author_sk).unwrap();
        match validate_inbound_op(&op, &mut state) {
            Err(ValidationError::RoleForbidsOpClass) => {}
            other => panic!(
                "expected RoleForbidsOpClass for Commenter + {op_class:?}, got {other:?}",
            ),
        }
    }
}

#[test]
fn surface_stream_viewer_rejected() {
    // Same shape as the Commenter case but with Viewer + Denied for
    // completeness — anything sub-Editor must not drive a stream.
    for role in [Role::Viewer, Role::Denied] {
        let (owner_sk, owner_pk) = generate_keypair();
        let (author_sk, author_pk) = generate_keypair();
        let channel = ChannelId::new();
        let artifact = ArtifactId::new();

        let ra = make_role_attestation(
            &owner_sk,
            owner_pk,
            author_pk,
            channel,
            role,
            t(0),
            t(100_000),
            vec![],
        );
        let attestation_id = ra.attestation.id;
        let mut state = ChannelState::default();
        state.authorised_signers.push(owner_pk);
        state.artifacts.insert(artifact, ArtifactKind::Cluster);
        state.register_attestation(ra);

        let mut op = SignedOp::new_unsigned(
            b"x".to_vec(),
            artifact,
            author_pk,
            attestation_id,
            OpClass::SurfaceFrame,
            t(10),
            1,
        );
        sign_op(&mut op, &author_sk).unwrap();
        match validate_inbound_op(&op, &mut state) {
            Err(ValidationError::RoleForbidsOpClass) => {}
            other => panic!(
                "expected RoleForbidsOpClass for {role:?} + SurfaceFrame, got {other:?}",
            ),
        }
    }
}
