//! Op-application enforcement: the room's grants, proven on the **real**
//! validate/apply path (`validate_inbound_op` +
//! `ChannelState::apply_and_relay`), not a helper.
//!
//! Headline proof: a `commenter` may add a comment op but an edit op from
//! the same actor is rejected **at validation** — not applied, not
//! relayed. Plus finer grants: an `editor` held to comments inside one app
//! has its edit denied, and a grant to one person on one artifact extends a
//! viewer's authority there only.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;

use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    DEFAULT_ROOM, Ed25519PublicKey, OpClass, Role, RoleAttestation, Signature, SignedOp,
    ValidationError, generate_keypair, op_node, sign_event, sign_op, validate_inbound_op,
};
use tilo_action_policy::{Answer, Grant, KindSet, Subject, TreePath, roles};

fn not_granted(result: Result<(), ValidationError>) -> bool {
    result.is_err_and(|error| matches!(error, ValidationError::NotGranted(_)))
}

fn grant(id: &str, subject: Subject, node: TreePath, answer: Answer) -> Grant {
    Grant::new(
        id,
        subject,
        node,
        KindSet::UNDOABLE,
        answer,
        Subject::Role(Role::Owner),
    )
}

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

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
    att.signature = sign_event(&sb, issuer_sk).unwrap();
    RoleAttestation {
        channel,
        role,
        attestation: att,
    }
}

/// A channel with one owner-signer, one `Note` artifact, and an author
/// holding `role`. Returns the state, the author's keypair, the
/// attestation id, and the artifact id.
fn channel_with_author(
    role: Role,
) -> (
    ChannelState,
    SigningKey,
    Ed25519PublicKey,
    AttestationId,
    ArtifactId,
    ChannelId,
) {
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
        t(0),
        t(100_000),
        vec![],
    );
    let attestation_id = ra.attestation.id;

    let mut state = ChannelState::default();
    state.authorised_signers.push(owner_pub);
    state.artifacts.insert(artifact, ArtifactKind::Note);
    state.register_attestation(ra);

    (
        state,
        author_sk,
        author_pub,
        attestation_id,
        artifact,
        channel,
    )
}

fn build_op(
    artifact: ArtifactId,
    author_pub: Ed25519PublicKey,
    author_sk: &SigningKey,
    attestation_id: AttestationId,
    op_class: OpClass,
    seq: u64,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        b"payload".to_vec(),
        artifact,
        author_pub,
        attestation_id,
        op_class,
        t(10),
        seq,
    );
    sign_op(&mut op, author_sk).unwrap();
    op
}

// The headline proof: commenter may comment, may NOT edit — enforced
// at validation, not applied, not relayed.

#[test]
fn commenter_may_comment_but_not_edit_on_the_real_apply_path() {
    let (mut state, author_sk, author_pub, att, artifact, _ch) =
        channel_with_author(Role::Commenter);

    // A comment op passes validation AND is applied (recorded to the op
    // log that fan-out relays from).
    let comment = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert!(
        state.apply_and_relay(&comment).is_ok(),
        "commenter's comment op must validate + apply"
    );
    assert_eq!(
        state.op_log.len(),
        1,
        "the comment op was applied (relayable)"
    );
    assert_eq!(
        state.last_seq_per_author.get(&author_pub).copied(),
        Some(1),
        "seq advanced for the applied comment"
    );

    // An EDIT op (EditMeta) from the SAME actor is rejected at
    // validation: the real validate path returns the role denial...
    let edit = build_op(artifact, author_pub, &author_sk, att, OpClass::EditMeta, 2);
    assert!(
        not_granted(validate_inbound_op(&edit, &mut state.clone())),
        "commenter's edit must be rejected at validation"
    );

    // ...and on the apply-and-relay path it is neither applied nor
    // relayed: the op log is unchanged and the seq counter did not move.
    let before_log = state.op_log.len();
    let before_seq = state.last_seq_per_author.get(&author_pub).copied();
    let res = state.apply_and_relay(&edit);
    assert!(not_granted(res));
    assert_eq!(
        state.op_log.len(),
        before_log,
        "rejected edit was NOT applied"
    );
    assert_eq!(
        state.last_seq_per_author.get(&author_pub).copied(),
        before_seq,
        "rejected edit did NOT advance seq (nothing to relay)"
    );
    // A splice (text edit) is likewise rejected.
    let splice = build_op(artifact, author_pub, &author_sk, att, OpClass::Splice, 2);
    assert!(not_granted(state.apply_and_relay(&splice)));
    assert_eq!(
        state.op_log.len(),
        before_log,
        "rejected splice was NOT applied"
    );
}

/// Finer grants do real work on the apply path: a Deny on one app holds an
/// Editor to comments there, a grant on one artifact extends a Viewer on
/// exactly that artifact, and a blocked member stays blocked whatever a
/// broader grant says.
#[test]
fn finer_grants_narrow_extend_and_block_on_the_real_path() {
    // Phase: an editor held to comments inside one app.
    let (mut state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(Role::Editor);
    let splice_ok = build_op(artifact, author_pub, &author_sk, att, OpClass::Splice, 1);
    assert!(
        state.clone().apply_and_relay(&splice_ok).is_ok(),
        "unrestricted editor may splice"
    );

    state.artifact_apps.insert(artifact, "finance".to_string());
    let finance = TreePath::root()
        .push(tilo_action_policy::Level::Room, DEFAULT_ROOM)
        .app("finance");
    state.grants.push(grant(
        "finance",
        Subject::Role(Role::Editor),
        finance.clone(),
        Answer::Deny,
    ));
    state.grants.push(grant(
        "finance-comments",
        Subject::Role(Role::Editor),
        finance
            .rest()
            .push(tilo_action_policy::Level::Field, roles::COMMENTS_FIELD),
        Answer::Allow,
    ));

    let splice = build_op(artifact, author_pub, &author_sk, att, OpClass::Splice, 1);
    assert!(
        not_granted(validate_inbound_op(&splice, &mut state.clone())),
        "the app deny refuses the editor's splice"
    );
    assert!(not_granted(state.apply_and_relay(&splice)));
    assert_eq!(state.op_log.len(), 0, "restricted splice was NOT applied");

    let comment = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert!(
        state.apply_and_relay(&comment).is_ok(),
        "comments inside the app are still allowed"
    );
    assert_eq!(state.op_log.len(), 1);

    // Phase: a grant on one artifact lets a viewer comment there only.
    let (mut state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(Role::Viewer);
    let other = ArtifactId::new();
    state.artifacts.insert(other, ArtifactKind::Note);

    let denied = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert!(
        not_granted(validate_inbound_op(&denied, &mut state.clone())),
        "viewer cannot comment by role alone"
    );

    state.grants.push(grant(
        "share",
        Subject::Person(hex::encode(author_pub.0)),
        op_node(
            DEFAULT_ROOM,
            None,
            &artifact.0.to_string(),
            OpClass::Comment,
        ),
        Answer::Allow,
    ));
    let comment = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert!(
        state.apply_and_relay(&comment).is_ok(),
        "the grant allows comments on the shared artifact"
    );
    assert_eq!(state.op_log.len(), 1);

    let elsewhere = build_op(other, author_pub, &author_sk, att, OpClass::Comment, 2);
    assert!(
        not_granted(state.apply_and_relay(&elsewhere)),
        "the grant does not leak to other artifacts"
    );
    let edit_shared = build_op(artifact, author_pub, &author_sk, att, OpClass::EditMeta, 2);
    assert!(
        not_granted(state.apply_and_relay(&edit_shared)),
        "a comment grant does not grant edit"
    );

    // Phase: a blocked member is not rescued by a broader grant.
    let (mut state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(Role::Editor);
    let person = Subject::Person(hex::encode(author_pub.0));
    state.grants.push(Grant::new(
        "blocked",
        person.clone(),
        TreePath::room(DEFAULT_ROOM),
        KindSet::ALL,
        Answer::Deny,
        Subject::Role(Role::Owner),
    ));
    state.grants.push(Grant::new(
        "everything",
        person,
        TreePath::root(),
        KindSet::ALL,
        Answer::Allow,
        Subject::Role(Role::Owner),
    ));
    let comment = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert!(
        not_granted(state.apply_and_relay(&comment)),
        "the room-wide block is more specific than the root grant"
    );
    assert_eq!(state.op_log.len(), 0);
}

/// The role presets decide every op class as their table says.
#[test]
fn role_presets_decide_op_classes() {
    let ops = [
        OpClass::Splice,
        OpClass::Move,
        OpClass::EditMeta,
        OpClass::Comment,
        OpClass::Tombstone,
        OpClass::Restore,
        OpClass::Govern,
        OpClass::Compaction,
        OpClass::Redact,
    ];
    for role in Role::ALL {
        for op in ops {
            let (state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(role);
            let signed = build_op(artifact, author_pub, &author_sk, att, op, 1);
            // A redaction then checks its payload; only the grant decision
            // matters here.
            let allowed = !not_granted(validate_inbound_op(&signed, &mut state.clone()));
            let expected = match role {
                Role::Owner => true,
                Role::Editor => {
                    !matches!(op, OpClass::Govern | OpClass::Compaction | OpClass::Redact)
                }
                Role::Commenter => op == OpClass::Comment,
                Role::Viewer => false,
            };
            assert_eq!(allowed, expected, "{role:?} / {op:?}");
        }
    }
}
