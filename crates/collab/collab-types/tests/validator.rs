//! Exercises the eight-step inbound-op validation pipeline: each step
//! has a negative test driving the validator to the corresponding
//! `ValidationError` variant, plus one happy-path test running all eight
//! green.
//!
//! Fixture: one owner-signer, one Editor attestation underwriting an
//! author, one Note artifact, no clearance gate by default; individual
//! tests mutate it to trip the desired step.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    Clearance, DEFAULT_ROOM, Ed25519PublicKey, OpKind, Role, RoleAttestation, Signature, SignedOp,
    ValidationError, generate_keypair, sign_event, sign_op, validate_admitted_inbound_op,
    validate_inbound_op, validate_replayed_inbound_op,
};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Authority, Grant, Party, Subject, roles};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

/// Build + sign a role attestation. The trust root (`issuer`) signs
/// over the attestation's `signing_bytes()` view.
fn make_role_attestation(
    issuer_sk: &SigningKey,
    issuer_pub: Ed25519PublicKey,
    subject: Ed25519PublicKey,
    channel: ChannelId,
    role: Role,
    valid_from: SystemTime,
    valid_until: SystemTime,
    claims: Vec<Claim>,
) -> RoleAttestation {
    let mut att = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pub,
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
#[allow(dead_code, reason = "not every merged case reads every fixture field")]
struct Fixture {
    owner_sk: SigningKey,
    owner_pub: Ed25519PublicKey,
    author_sk: SigningKey,
    author_pub: Ed25519PublicKey,
    artifact: ArtifactId,
    channel: ChannelId,
    attestation_id: AttestationId,
    state: ChannelState,
}

fn fixture() -> Fixture {
    fixture_with(Role::Editor, t(0), t(100_000))
}

/// The standard fixture, with the author's role attestation issued for
/// `role` over `valid_from..valid_until`.
fn fixture_with(role: Role, valid_from: SystemTime, valid_until: SystemTime) -> Fixture {
    let (owner_sk, owner_pub) = generate_keypair();
    let (author_sk, author_pub) = generate_keypair();
    let channel = ChannelId::new();
    let artifact = ArtifactId::new();

    let ra = make_role_attestation(
        &owner_sk,
        owner_pub,
        author_pub,
        channel,
        role,
        valid_from,
        valid_until,
        vec![],
    );
    let attestation_id = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pub);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    Fixture {
        owner_sk,
        owner_pub,
        author_sk,
        author_pub,
        artifact,
        channel,
        attestation_id,
        state,
    }
}

fn build_op(f: &Fixture, kind: OpKind, timestamp: SystemTime, seq: u64) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        b"payload".to_vec(),
        f.artifact,
        f.author_pub,
        f.attestation_id,
        kind,
        timestamp,
        seq,
    );
    sign_op(&mut op, &f.author_sk).unwrap();
    op
}

/// A splice op with payload `x`, seq 1, signed by `sk`.
fn build_x_op(
    artifact: ArtifactId,
    author: Ed25519PublicKey,
    attestation: AttestationId,
    timestamp: SystemTime,
    sk: &SigningKey,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        b"x".to_vec(),
        artifact,
        author,
        attestation,
        OpKind::Change,
        timestamp,
        1,
    );
    sign_op(&mut op, sk).unwrap();
    op
}

/// Gives the fixture's author a `ClearedAt(level)` identity attestation
/// signed by a fresh key; the key is accepted as a trust root when `trusted`.
/// Returns the issuer.
fn grant_clearance(f: &mut Fixture, level: Clearance, trusted: bool) -> Ed25519PublicKey {
    let (issuer_sk, issuer_pub) = generate_keypair();
    if trusted {
        f.state.accepted_trust_roots.push(issuer_pub);
    }
    let mut a = Attestation {
        id: AttestationId::new(),
        issued_by: issuer_pub,
        subject: f.author_pub,
        claims: vec![Claim::ClearedAt(level)],
        valid_from: t(0),
        valid_until: t(100_000),
        signature: Signature::from_bytes([0u8; 64]),
    };
    let sb = a.signing_bytes().unwrap();
    a.signature = sign_event(&sb, &issuer_sk).unwrap();
    f.state
        .identity_attestations
        .entry(f.author_pub)
        .or_default()
        .push(a);
    issuer_pub
}

fn presets() -> Vec<Grant> {
    roles::room_role_grants(DEFAULT_ROOM, &Subject::Role(Role::Owner))
}

fn party(grants: &[Grant], role: Role) -> Authority<'_> {
    Authority::person(Party::new(vec![Subject::Role(role)], grants))
}

fn rejection(case: &str, result: Result<(), ValidationError>) -> ValidationError {
    match result {
        Err(e) => e,
        Ok(()) => panic!("case {case}: expected a rejection, got Ok"),
    }
}

/// A valid op passes all eight steps — with and without a clearance gate —
/// and the author's replay slot bumps on success only.
#[test]
fn a_valid_op_passes_all_eight_steps_and_only_success_bumps_the_replay_slot() {
    // Case: happy path, all eight pass; the seq map is updated.
    let mut f = fixture();
    let op = build_op(&f, OpKind::Change, t(10), 1);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_ok(), "expected Ok, got {r:?}");
    assert_eq!(
        f.state.last_seq_per_author.get(&f.author_pub).copied(),
        Some(1)
    );

    // Case: happy path with a clearance gate. A ClearedAt(Confidential)
    // identity attestation issued by a trust root the channel accepts, on an
    // artifact classified Internal (lower than the author's clearance).
    let mut f = fixture();
    grant_clearance(&mut f, Clearance::CONFIDENTIAL, true);
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::INTERNAL);
    let op = build_op(&f, OpKind::Change, t(10), 1);
    assert!(validate_inbound_op(&op, &mut f.state).is_ok());

    // Case: the replay slot bumps on success.
    let mut f = fixture();
    let op = build_op(&f, OpKind::Change, t(10), 42);
    assert!(validate_inbound_op(&op, &mut f.state).is_ok());
    assert_eq!(
        f.state.last_seq_per_author.get(&f.author_pub).copied(),
        Some(42)
    );

    // Case: the replay slot does not bump on failure (a tampered op that
    // fails step 3).
    let mut f = fixture();
    let mut op = build_op(&f, OpKind::Change, t(10), 5);
    op.signature = Signature::from_bytes([0xff; 64]);
    let r = validate_inbound_op(&op, &mut f.state);
    assert!(r.is_err());
    assert!(!f.state.last_seq_per_author.contains_key(&f.author_pub));
}

#[test]
fn certified_roster_admission_reuses_the_policy_without_a_role_attestation() {
    let g = presets();
    // Case: admission with no attestations or signers at all.
    let mut f = fixture();
    f.state.attestations.clear();
    f.state.authorised_signers.clear();
    let op = build_op(&f, OpKind::Change, t(10), 1);
    assert!(
        validate_admitted_inbound_op(
            &op,
            &party(&g, Role::Editor),
            &mut f.state,
            t(10),
            Duration::from_secs(5),
        )
        .is_ok()
    );
    assert_eq!(
        f.state.last_seq_per_author.get(&f.author_pub).copied(),
        Some(1)
    );

    // Case: still rejects forgery, role, time and replay.
    let f = fixture();
    let valid = build_op(&f, OpKind::Change, t(10), 7);
    let skew = Duration::from_secs(5);

    let mut forged = valid.clone();
    forged.payload.push(0xff);
    assert!(matches!(
        validate_admitted_inbound_op(
            &forged,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            t(10),
            skew
        ),
        Err(ValidationError::BadOpSignature(_))
    ));
    assert!(matches!(
        validate_admitted_inbound_op(
            &valid,
            &party(&g, Role::Viewer),
            &mut f.state.clone(),
            t(10),
            skew
        ),
        Err(ValidationError::NotGranted(_))
    ));
    assert_eq!(
        validate_admitted_inbound_op(
            &valid,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            t(20),
            skew
        ),
        Err(ValidationError::OutsideValidityWindow)
    );

    let mut state = f.state;
    assert!(
        validate_admitted_inbound_op(&valid, &party(&g, Role::Editor), &mut state, t(10), skew)
            .is_ok()
    );
    assert!(matches!(
        validate_admitted_inbound_op(&valid, &party(&g, Role::Editor), &mut state, t(10), skew),
        Err(ValidationError::ReplaySeq {
            seen: 7,
            incoming: 7
        })
    ));
}

/// Steps 1-4 each reject their input: an unknown attestation, an issuer no
/// longer authorised, a corrupted attestation signature, a corrupted or
/// tampered op signature, and an author who is not the attestation subject.
#[test]
fn host_replayed_history_is_admitted_however_old_but_never_twice_or_from_the_future() {
    let g = presets();
    let mut f = fixture();
    let old = build_op(&f, OpKind::Change, t(10), 3);
    let skew = Duration::from_secs(300);
    let an_hour_later = t(10 + 3600);
    assert_eq!(
        validate_admitted_inbound_op(
            &old,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            an_hour_later,
            skew
        ),
        Err(ValidationError::OutsideValidityWindow),
        "a live op an hour old is stale",
    );
    let mut state = f.state.clone();
    assert!(
        validate_replayed_inbound_op(
            &old,
            &party(&g, Role::Editor),
            &mut state,
            an_hour_later,
            skew
        )
        .is_ok()
    );
    assert!(matches!(
        validate_replayed_inbound_op(
            &old,
            &party(&g, Role::Editor),
            &mut state,
            an_hour_later,
            skew
        ),
        Err(ValidationError::ReplaySeq { .. })
    ));
    assert_eq!(
        validate_replayed_inbound_op(
            &old,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            t(0),
            skew
        ),
        Ok(()),
        "within the skew ahead",
    );
    let future = build_op(&f, OpKind::Change, t(10 + 3600), 4);
    assert_eq!(
        validate_replayed_inbound_op(
            &future,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            t(10),
            skew
        ),
        Err(ValidationError::OutsideValidityWindow),
    );
    let mut forged = old.clone();
    forged.payload.push(0xff);
    assert!(matches!(
        validate_replayed_inbound_op(&forged, &party(&g, Role::Editor), &mut f.state, t(10), skew),
        Err(ValidationError::BadOpSignature(_))
    ));
}

#[test]
fn steps_1_to_4_reject_unknown_unauthorised_or_forged_ops() {
    // Step 1: unknown attestation id.
    let mut f = fixture();
    let op = build_x_op(
        f.artifact,
        f.author_pub,
        AttestationId::new(), // unknown to the validator
        t(10),
        &f.author_sk,
    );
    let e = rejection("step1", validate_inbound_op(&op, &mut f.state));
    assert!(
        matches!(e, ValidationError::UnknownAttestation(_)),
        "expected UnknownAttestation, got {e:?}"
    );

    // Step 2: attestation issuer not in authorised signers. Wipe the
    // channel's authorised signers — the stored attestation was issued by
    // `owner_pub`, which no longer qualifies.
    let mut f = fixture();
    f.state.authorised_signers.clear();
    let op = build_op(&f, OpKind::Change, t(10), 1);
    assert_eq!(
        rejection("step2 issuer", validate_inbound_op(&op, &mut f.state)),
        ValidationError::UnauthorisedAttestation(f.owner_pub)
    );

    // Step 2: attestation signature corrupted, so the inner `verify_event`
    // check fails.
    let mut f = fixture();
    let ra = f.state.attestations.get_mut(&f.attestation_id).unwrap();
    ra.attestation.signature = Signature::from_bytes([0xff; 64]);
    let op = build_op(&f, OpKind::Change, t(10), 1);
    let e = rejection("step2 signature", validate_inbound_op(&op, &mut f.state));
    assert!(
        matches!(e, ValidationError::BadAttestationSignature(_)),
        "expected BadAttestationSignature, got {e:?}"
    );

    // Step 3: op signature corrupted, or payload tampered after signing.
    let corrupt_signature: fn(&mut SignedOp) =
        |op| op.signature = Signature::from_bytes([0xff; 64]);
    let tamper_payload: fn(&mut SignedOp) = |op| op.payload[0] ^= 0xff;
    for (case, tamper) in [
        ("step3 signature", corrupt_signature),
        ("step3 payload", tamper_payload),
    ] {
        let mut f = fixture();
        let mut op = build_op(&f, OpKind::Change, t(10), 1);
        tamper(&mut op);
        let e = rejection(case, validate_inbound_op(&op, &mut f.state));
        assert!(
            matches!(e, ValidationError::BadOpSignature(_)),
            "case {case}: expected BadOpSignature, got {e:?}"
        );
    }

    // Step 4: author does not match subject. Forge an op whose author is
    // *not* the attestation's subject; sign with a matching key to get past
    // the step-3 signature check.
    let mut f = fixture();
    let (forger_sk, forger_pub) = generate_keypair();
    let op = build_x_op(f.artifact, forger_pub, f.attestation_id, t(10), &forger_sk);
    assert_eq!(
        rejection("step4", validate_inbound_op(&op, &mut f.state)),
        ValidationError::AuthorMismatch
    );
}

/// Steps 5-7 reject an op class the role forbids, an unknown artifact, an op
/// outside the attestation's window on either side, and a replayed or older
/// sequence number.
#[test]
fn steps_5_to_7_reject_forbidden_unknown_out_of_window_or_replayed_ops() {
    // Step 5: a Commenter cannot splice.
    let mut f = fixture_with(Role::Commenter, t(0), t(100_000));
    let op = build_x_op(
        f.artifact,
        f.author_pub,
        f.attestation_id,
        t(10),
        &f.author_sk,
    );
    assert!(matches!(
        rejection("step5 commenter", validate_inbound_op(&op, &mut f.state)),
        ValidationError::NotGranted(_)
    ));

    // Step 5: unknown artifact (not registered with the channel).
    let mut f = fixture();
    let op = build_x_op(
        ArtifactId::new(),
        f.author_pub,
        f.attestation_id,
        t(10),
        &f.author_sk,
    );
    let e = rejection("step5 artifact", validate_inbound_op(&op, &mut f.state));
    assert!(
        matches!(e, ValidationError::UnknownArtifact(_)),
        "expected UnknownArtifact, got {e:?}"
    );

    // Step 6: op after valid_until (attestation valid until t(100_000); op
    // at t(100_001)).
    let mut f = fixture();
    let op = build_op(&f, OpKind::Change, t(100_001), 1);
    assert_eq!(
        rejection("step6 after", validate_inbound_op(&op, &mut f.state)),
        ValidationError::OutsideValidityWindow
    );

    // Step 6: op before valid_from (valid_from bumped to a non-zero value).
    let mut f = fixture_with(Role::Editor, t(100), t(200));
    let op = build_x_op(
        f.artifact,
        f.author_pub,
        f.attestation_id,
        t(50), // before valid_from
        &f.author_sk,
    );
    assert_eq!(
        rejection("step6 before", validate_inbound_op(&op, &mut f.state)),
        ValidationError::OutsideValidityWindow
    );

    // Step 7: the same seq replayed is rejected.
    let mut f = fixture();
    let op1 = build_op(&f, OpKind::Change, t(10), 1);
    let op2 = build_op(&f, OpKind::Change, t(11), 1); // same seq as op1
    assert!(validate_inbound_op(&op1, &mut f.state).is_ok());
    assert_eq!(
        rejection("step7 same seq", validate_inbound_op(&op2, &mut f.state)),
        ValidationError::ReplaySeq {
            seen: 1,
            incoming: 1
        }
    );

    // Step 7: a smaller seq is rejected.
    let mut f = fixture();
    let op1 = build_op(&f, OpKind::Change, t(10), 10);
    let op2 = build_op(&f, OpKind::Change, t(11), 5);
    assert!(validate_inbound_op(&op1, &mut f.state).is_ok());
    let e = rejection("step7 smaller seq", validate_inbound_op(&op2, &mut f.state));
    assert!(
        matches!(e, ValidationError::ReplaySeq { .. }),
        "expected ReplaySeq, got {e:?}"
    );
}

/// Step 8 enforces the effective clearance (the max of channel floor and
/// artifact classification) against a clearance from a trusted issuer.
#[test]
fn step_8_enforces_the_effective_clearance_from_trusted_issuers() {
    // Case: no clearance claim at all against a Confidential artifact.
    let mut f = fixture();
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::CONFIDENTIAL);
    let op = build_op(&f, OpKind::Change, t(10), 1);
    assert_eq!(
        rejection("no claim", validate_inbound_op(&op, &mut f.state)),
        ValidationError::InsufficientClearance {
            required: Clearance::CONFIDENTIAL,
            found: None
        }
    );

    // Case: clearance too low — Internal held, Confidential required.
    let mut f = fixture();
    grant_clearance(&mut f, Clearance::INTERNAL, true);
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::CONFIDENTIAL);
    let op = build_op(&f, OpKind::Change, t(10), 1);
    assert_eq!(
        rejection("too low", validate_inbound_op(&op, &mut f.state)),
        ValidationError::InsufficientClearance {
            required: Clearance::CONFIDENTIAL,
            found: Some(Clearance::INTERNAL)
        }
    );

    // Case: untrusted clearance issuer — a Confidential clearance issued by
    // a key the channel does NOT accept as a trust root
    // (accepted_trust_roots is empty).
    let mut f = fixture();
    let rogue_pub = grant_clearance(&mut f, Clearance::CONFIDENTIAL, false);
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::CONFIDENTIAL);
    let op = build_op(&f, OpKind::Change, t(10), 1);
    assert_eq!(
        rejection("untrusted issuer", validate_inbound_op(&op, &mut f.state)),
        ValidationError::UntrustedClearanceIssuer { issuer: rogue_pub }
    );

    // Case: channel min-clearance floor. Channel-level min_clearance
    // Restricted; artifact-level classification unset — the validator must
    // still enforce.
    let mut f = fixture();
    f.state.channel_min_clearance = Some(Clearance::RESTRICTED);
    let op = build_op(&f, OpKind::Change, t(10), 1);
    let e = rejection("channel floor", validate_inbound_op(&op, &mut f.state));
    assert!(
        matches!(
            e,
            ValidationError::InsufficientClearance {
                required: Clearance::RESTRICTED,
                found: _
            }
        ),
        "expected InsufficientClearance, got {e:?}"
    );

    // Case: effective clearance takes the max. Channel = Internal, artifact =
    // Restricted, author has Confidential — fails (Confidential < Restricted).
    let mut f = fixture();
    f.state.channel_min_clearance = Some(Clearance::INTERNAL);
    f.state
        .artifact_classifications
        .insert(f.artifact, Clearance::RESTRICTED);
    grant_clearance(&mut f, Clearance::CONFIDENTIAL, true);
    let op = build_op(&f, OpKind::Change, t(10), 1);
    assert_eq!(
        rejection("effective max", validate_inbound_op(&op, &mut f.state)),
        ValidationError::InsufficientClearance {
            required: Clearance::RESTRICTED,
            found: Some(Clearance::CONFIDENTIAL)
        }
    );
}

/// A grant naming a record (at it or below it) names that record's channel;
/// a pattern over an app's records, another room's record or a record of
/// another app names none.
#[test]
fn a_grant_naming_a_record_names_its_channel_and_a_pattern_does_not() {
    use collab_types::{document_channel_id, document_node, named_record_of_channel};
    use tilo_action_policy::{Answer, KindSet, Level, TreePath};
    let grant = |node: TreePath| {
        Grant::new(
            "g",
            Subject::person("carol"),
            node,
            KindSet::READ,
            Answer::Deny,
            Subject::person("owner"),
        )
    };
    let channel = document_channel_id("so.tilo.sheets", "room", "payroll");
    let exact = [grant(document_node("room", "so.tilo.sheets", "payroll"))];
    assert_eq!(
        named_record_of_channel(&exact, "room", channel),
        Some(("so.tilo.sheets", "payroll"))
    );
    let below = [grant(
        document_node("room", "so.tilo.sheets", "payroll").field("salary"),
    )];
    assert_eq!(
        named_record_of_channel(&below, "room", channel),
        Some(("so.tilo.sheets", "payroll"))
    );
    let others = [
        grant(
            TreePath::room("room")
                .app("so.tilo.sheets")
                .any(Level::Resource),
        ),
        grant(TreePath::room("room").app("so.tilo.sheets")),
        grant(document_node("other", "so.tilo.sheets", "payroll")),
        grant(document_node("room", "so.tilo.notes", "payroll")),
        grant(document_node("room", "so.tilo.sheets", "budget")),
    ];
    assert_eq!(named_record_of_channel(&others, "room", channel), None);
}

/// A Read Deny over a pattern of records stands for them by one record it
/// covers and nothing names; a grant naming a record, one below records, one
/// that allows, one of another kind or one in another room has no witness.
#[test]
fn a_pattern_read_deny_has_a_witness_and_a_named_or_field_grant_does_not() {
    use collab_types::{document_node, pattern_read_witnesses};
    use tilo_action_policy::{Answer, KindSet, Level, TreePath};
    let grant = |node: TreePath, kinds: KindSet, answer: Answer| {
        Grant::new(
            "g",
            Subject::person("carol"),
            node,
            kinds,
            answer,
            Subject::person("owner"),
        )
    };
    let deny = |node: TreePath| grant(node, KindSet::READ, Answer::Deny);
    let finance = TreePath::room("room").app("finance");
    let grants = [
        deny(finance.clone()),
        deny(TreePath::room("room").app("sheets").any(Level::Resource)),
        grant(
            TreePath::room("room").rest().resource("secret"),
            KindSet::READ,
            Answer::Ask,
        ),
        deny(document_node("room", "finance", "payroll")),
        deny(
            TreePath::room("room")
                .app("finance")
                .any(Level::Resource)
                .field("salary"),
        ),
        grant(
            document_node("room", "finance", "summary"),
            KindSet::READ,
            Answer::Allow,
        ),
        grant(finance.clone(), KindSet::READ, Answer::Allow),
        grant(finance, KindSet::UNDOABLE, Answer::Deny),
        deny(TreePath::room("other").app("finance")),
    ];
    let witnesses = pattern_read_witnesses(&grants, "room");
    let unnamed = "\u{0}unnamed";
    let expected: std::collections::BTreeSet<(String, String)> = [
        ("finance", unnamed),
        ("sheets", unnamed),
        (unnamed, "secret"),
    ]
    .into_iter()
    .map(|(app, record)| (app.to_owned(), record.to_owned()))
    .collect();
    assert_eq!(witnesses, expected);
    for (app, record) in &witnesses {
        assert!(
            grants
                .iter()
                .any(|grant| grant.node.covers(&document_node("room", app, record))),
            "{app}/{record}"
        );
    }
}
