//! The inbound document-op validators, on their production path: an op
//! admitted by the certified device join and the workspace's live roster
//! passes the signature, freshness, grant, replay and redaction checks, and a
//! host-replayed op may be old.
//!
//! Fixture: one Note artifact in the default workspace, one author, the role
//! presets; individual tests mutate it to trip the check they name.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, DEFAULT_WORKSPACE, Ed25519PublicKey, InboundState,
    OpKind, RedactRequest, Role, Signature, SignedOp, ValidationError, encode_redact_payload,
    generate_keypair, hash_target_op, sign_op, validate_admitted_inbound_op,
    validate_replayed_inbound_op,
};
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Authority, Grant, Grantee, Party, roles};

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

struct Fixture {
    author_sk: SigningKey,
    author_pub: Ed25519PublicKey,
    artifact: ArtifactId,
    state: InboundState,
}

/// One Note record in the default workspace, and the author's keypair.
fn fixture() -> Fixture {
    let (author_sk, author_pub) = generate_keypair();
    let artifact = ArtifactId::new();
    let mut state = InboundState::default();
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.artifact_apps.insert(artifact, "so.tilo.notes".into());
    state.artifact_records.insert(artifact, "page".into());
    Fixture {
        author_sk,
        author_pub,
        artifact,
        state,
    }
}

fn build_op(f: &Fixture, kind: OpKind, timestamp: SystemTime, seq: u64) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        b"payload".to_vec(),
        f.artifact,
        f.author_pub,
        AttestationId::new(),
        kind,
        timestamp,
        seq,
    );
    sign_op(&mut op, &f.author_sk).unwrap();
    op
}

fn presets() -> Vec<Grant> {
    roles::workspace_role_grants(DEFAULT_WORKSPACE, &Grantee::role(Role::Owner))
}

fn party(grants: &[Grant], role: Role) -> Authority<'_> {
    Authority::person(Party::new(vec![Grantee::role(role)], grants))
}

const SKEW: Duration = Duration::from_secs(30);

fn rejection(case: &str, result: Result<(), ValidationError>) -> ValidationError {
    match result {
        Err(e) => e,
        Ok(()) => panic!("case {case}: expected a rejection, got Ok"),
    }
}

/// A valid op passes every check and bumps the author's replay slot on
/// success only.
#[test]
fn an_admitted_op_passes_and_only_success_bumps_the_replay_slot() {
    let g = presets();

    let mut f = fixture();
    let op = build_op(&f, OpKind::Change, t(10), 42);
    assert!(
        validate_admitted_inbound_op(&op, &party(&g, Role::Editor), &mut f.state, t(10), SKEW)
            .is_ok()
    );
    assert_eq!(
        f.state.last_seq_per_author.get(&f.author_pub).copied(),
        Some(42)
    );

    // A forged op is refused and leaves the replay slot alone.
    let f = fixture();
    let mut op = build_op(&f, OpKind::Change, t(10), 5);
    op.signature = Signature::from_bytes([0xff; 64]);
    let op = op;
    assert!(matches!(
        validate_admitted_inbound_op(
            &op,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            t(10),
            SKEW
        ),
        Err(ValidationError::BadOpSignature(_))
    ));
    assert!(!f.state.last_seq_per_author.contains_key(&f.author_pub));
}

#[test]
fn a_live_op_must_be_fresh_and_a_replayed_one_may_be_old() {
    let g = presets();
    let f = fixture();
    let old = build_op(&f, OpKind::Change, t(10), 3);
    let an_hour_later = t(10 + 3600);

    assert_eq!(
        validate_admitted_inbound_op(
            &old,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            an_hour_later,
            Duration::from_secs(300)
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
            Duration::from_secs(300)
        )
        .is_ok()
    );
    assert!(matches!(
        validate_replayed_inbound_op(
            &old,
            &party(&g, Role::Editor),
            &mut state,
            an_hour_later,
            Duration::from_secs(300)
        ),
        Err(ValidationError::ReplaySeq { .. })
    ));
    assert_eq!(
        validate_replayed_inbound_op(
            &old,
            &party(&g, Role::Editor),
            &mut f.state.clone(),
            t(0),
            Duration::from_secs(300)
        ),
        Ok(()),
        "within the skew ahead",
    );

    // A future-dated op is refused on the replay path too.
    let mut f = f;
    let future = build_op(&f, OpKind::Change, t(10 + 3600), 4);
    assert_eq!(
        validate_replayed_inbound_op(
            &future,
            &party(&g, Role::Editor),
            &mut f.state,
            t(10),
            Duration::from_secs(300)
        ),
        Err(ValidationError::OutsideValidityWindow),
    );
}

/// The grant the author's role carries decides the op, the artifact must be
/// one the validator knows, and a tampered payload breaks the signature.
#[test]
fn the_author_grant_the_artifact_and_the_signature_are_checked() {
    let g = presets();

    let mut f = fixture();
    let op = build_op(&f, OpKind::Change, t(10), 7);
    assert!(matches!(
        rejection(
            "viewer",
            validate_admitted_inbound_op(&op, &party(&g, Role::Viewer), &mut f.state, t(10), SKEW)
        ),
        ValidationError::NotGranted(_)
    ));

    let mut f = fixture();
    let mut op = build_op(&f, OpKind::Change, t(10), 7);
    op.payload.push(0xff);
    assert!(matches!(
        validate_admitted_inbound_op(&op, &party(&g, Role::Editor), &mut f.state, t(10), SKEW),
        Err(ValidationError::BadOpSignature(_))
    ));

    let mut f = fixture();
    let mut op = build_op(&f, OpKind::Change, t(10), 7);
    op.artifact = ArtifactId::new();
    sign_op(&mut op, &f.author_sk).unwrap();
    assert!(matches!(
        validate_admitted_inbound_op(&op, &party(&g, Role::Editor), &mut f.state, t(10), SKEW),
        Err(ValidationError::UnknownArtifact(_))
    ));

    // Only an owner may compact or redact.
    for kind in [OpKind::Compaction, OpKind::Redaction] {
        let mut f = fixture();
        let op = build_op(&f, kind, t(10), 7);
        assert!(
            matches!(
                rejection(
                    "editor governance",
                    validate_admitted_inbound_op(
                        &op,
                        &party(&g, Role::Editor),
                        &mut f.state,
                        t(10),
                        SKEW
                    )
                ),
                ValidationError::NotGranted(_)
            ),
            "an editor must not {kind:?}"
        );
    }
}

/// An op whose id is already redacted is refused, a Redact of a Redact is
/// refused, and a Redact whose payload does not parse is refused.
#[test]
fn redaction_checks_refuse_a_redacted_op_a_recursive_redact_and_a_malformed_payload() {
    let g = presets();
    let owner = party(&g, Role::Owner);

    // A redaction that reached this peer before the op did.
    let mut f = fixture();
    let op = build_op(&f, OpKind::Change, t(10), 1);
    let id = collab_types::OpId::from_op(&op);
    f.state.redacted_ops.insert(id);
    assert_eq!(
        validate_admitted_inbound_op(&op, &owner, &mut f.state, t(10), SKEW),
        Err(ValidationError::OpAlreadyRedacted { target: id })
    );

    // A Redact targeting a Redact.
    let mut f = fixture();
    let redact = build_op(&f, OpKind::Redaction, t(10), 1);
    let target = collab_types::OpId::from_op(&redact);
    f.state.record_validated_op(redact);
    let recursive = RedactRequest {
        artifact: f.artifact,
        target_op_id: target,
        target_op_hash: hash_target_op(&f.state.op_log[0]).unwrap(),
        reason: "again".into(),
        redacted_by: f.author_pub,
        redacted_at: t(11),
    };
    let mut op = SignedOp::new_unsigned(
        encode_redact_payload(&recursive).unwrap(),
        f.artifact,
        f.author_pub,
        AttestationId::new(),
        OpKind::Redaction,
        t(11),
        2,
    );
    sign_op(&mut op, &f.author_sk).unwrap();
    assert_eq!(
        validate_admitted_inbound_op(&op, &owner, &mut f.state, t(11), SKEW),
        Err(ValidationError::RecursiveRedaction { target })
    );

    // A payload that does not parse as a RedactRequest.
    let mut f = fixture();
    let op = build_op(&f, OpKind::Redaction, t(10), 1);
    assert!(
        matches!(
            validate_admitted_inbound_op(&op, &owner, &mut f.state, t(10), SKEW),
            Err(ValidationError::MalformedRedact(_))
        ),
        "arbitrary bytes are not a RedactRequest"
    );
}

/// A grant naming a record (at it or below it) names that record's channel;
/// a pattern over an app's records, another workspace's record or a record of
/// another app names none.
#[test]
fn a_grant_naming_a_record_names_its_channel_and_a_pattern_does_not() {
    use collab_types::{document_channel_id, document_node, named_record_of_channel};
    use tilo_action_policy::{Answer, KindSet, Level, TreePath};
    let grant = |node: TreePath| {
        Grant::new(
            "g",
            Grantee::person("carol"),
            node,
            KindSet::READ,
            Answer::Deny,
            Grantee::person("owner"),
        )
    };
    let channel = document_channel_id("so.tilo.sheets", "workspace", "payroll");
    let exact = [grant(document_node(
        "workspace",
        "so.tilo.sheets",
        "payroll",
    ))];
    assert_eq!(
        named_record_of_channel(&exact, "workspace", channel),
        Some(("so.tilo.sheets", "payroll"))
    );
    let below = [grant(
        document_node("workspace", "so.tilo.sheets", "payroll").field("salary"),
    )];
    assert_eq!(
        named_record_of_channel(&below, "workspace", channel),
        Some(("so.tilo.sheets", "payroll"))
    );
    let others = [
        grant(
            TreePath::workspace("workspace")
                .app("so.tilo.sheets")
                .any(Level::Resource),
        ),
        grant(TreePath::workspace("workspace").app("so.tilo.sheets")),
        grant(document_node("other", "so.tilo.sheets", "payroll")),
        grant(document_node("workspace", "so.tilo.notes", "payroll")),
        grant(document_node("workspace", "so.tilo.sheets", "budget")),
    ];
    assert_eq!(named_record_of_channel(&others, "workspace", channel), None);
}
