//! Op-application enforcement: the `(subject × scope × verb)` capability
//! layer, proven on the **real** validate/apply path
//! (`validate_inbound_op` + `ChannelState::apply_and_relay`), not a helper.
//!
//! Headline proof: a `commenter` may add a comment op but an edit op from
//! the same actor is rejected **at validation** — not applied, not
//! relayed. Plus the richer layer: an `editor` restricted to
//! comment-only inside one app has its edit denied (something the
//! coarse role table alone would allow), and a per-user share extends a
//! viewer's authority on one artifact.

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

use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    Ed25519PublicKey, OpClass, Role, RoleAttestation, Signature, SignedOp, ValidationError,
    generate_keypair, role_caps, role_permits, sign_event, sign_op, validate_inbound_op,
    verb_for_opclass,
};

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
    assert_eq!(
        validate_inbound_op(&edit, &mut state.clone()),
        Err(ValidationError::RoleForbidsOpClass),
        "commenter's edit must be rejected at validation"
    );

    // ...and on the apply-and-relay path it is neither applied nor
    // relayed: the op log is unchanged and the seq counter did not move.
    let before_log = state.op_log.len();
    let before_seq = state.last_seq_per_author.get(&author_pub).copied();
    let res = state.apply_and_relay(&edit);
    assert_eq!(res, Err(ValidationError::RoleForbidsOpClass));
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
    assert_eq!(
        state.apply_and_relay(&splice),
        Err(ValidationError::RoleForbidsOpClass)
    );
    assert_eq!(
        state.op_log.len(),
        before_log,
        "rejected splice was NOT applied"
    );
}

/// Capability overrides do real work on the apply path: an app-scoped
/// Restrict narrows an Editor below the role table, a per-resource share
/// extends a Viewer on exactly one artifact, and nothing rescues Denied.
#[test]
fn capability_overrides_narrow_extend_but_never_rescue_on_the_real_path() {
    // Phase: an app-scoped restriction caps an editor to comment only. The
    // richer layer does real work: an app-scoped Restrict narrows an Editor
    // below what the coarse role table allows.
    let (mut state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(Role::Editor);

    // Baseline: the coarse role table permits an Editor to splice, and
    // with no overrides the op applies.
    assert!(role_permits(
        Role::Editor,
        ArtifactKind::Note,
        OpClass::Splice
    ));
    let splice_ok = build_op(artifact, author_pub, &author_sk, att, OpClass::Splice, 1);
    assert!(
        state.clone().apply_and_relay(&splice_ok).is_ok(),
        "unrestricted editor may splice"
    );

    // Mark the artifact as owned by the "finance" app, then restrict the
    // Editor role to comment-only within that app.
    state.artifact_apps.insert(artifact, "finance".to_string());
    state.cap_overrides.push(CapOverride::restrict_role_in_app(
        Role::Editor,
        "finance",
        Verb::Comment,
    ));

    // Now a splice inside the finance app is denied by the CAPABILITY
    // layer (distinct from a pure role denial) on the real path...
    let splice = build_op(artifact, author_pub, &author_sk, att, OpClass::Splice, 1);
    assert_eq!(
        validate_inbound_op(&splice, &mut state.clone()),
        Err(ValidationError::CapabilityDenied),
        "app restriction denies the editor's splice"
    );
    let res = state.apply_and_relay(&splice);
    assert_eq!(res, Err(ValidationError::CapabilityDenied));
    assert_eq!(state.op_log.len(), 0, "restricted splice was NOT applied");

    // ...but a comment inside the same app is still allowed (the
    // restriction caps AT comment, not below it).
    let comment = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert!(
        state.apply_and_relay(&comment).is_ok(),
        "comment is within the restricted cap ceiling"
    );
    assert_eq!(state.op_log.len(), 1);

    // Phase: a per-resource share grants a viewer comment on one artifact.
    // A Grant extends a Viewer's authority on ONE artifact without changing
    // their role.
    let (mut state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(Role::Viewer);

    // A second artifact the viewer is NOT shared on.
    let other = ArtifactId::new();
    state.artifacts.insert(other, ArtifactKind::Note);

    // By role alone a Viewer cannot comment anywhere.
    let denied = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert_eq!(
        validate_inbound_op(&denied, &mut state.clone()),
        Err(ValidationError::RoleForbidsOpClass),
        "viewer cannot comment by role alone"
    );

    // Share THIS artifact with the viewer, comment-only.
    state
        .cap_overrides
        .push(CapOverride::share(author_pub, artifact, Verb::Comment));

    // Now a comment on the shared artifact is allowed on the real path...
    let comment = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert!(
        state.apply_and_relay(&comment).is_ok(),
        "share grants comment on the shared artifact"
    );
    assert_eq!(state.op_log.len(), 1);

    // ...but the same viewer still cannot comment on a DIFFERENT
    // artifact (the share is instance-scoped), nor edit the shared one.
    let elsewhere = build_op(other, author_pub, &author_sk, att, OpClass::Comment, 2);
    assert_eq!(
        state.apply_and_relay(&elsewhere),
        Err(ValidationError::RoleForbidsOpClass),
        "the share does not leak to other artifacts"
    );
    let edit_shared = build_op(artifact, author_pub, &author_sk, att, OpClass::EditMeta, 2);
    assert_eq!(
        state.apply_and_relay(&edit_shared),
        Err(ValidationError::RoleForbidsOpClass),
        "a comment-only share does not grant edit"
    );

    // Phase: a denied role cannot be rescued by a grant. Denied is an
    // explicit block: no grant can rescue it.
    let (mut state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(Role::Denied);

    // Even a workspace-wide Admin grant to this exact user is inert
    // against an explicit Denied block.
    state.cap_overrides.push(CapOverride::new(
        Subject::User(author_pub),
        Scope::Workspace,
        Effect::Grant,
        Verb::Admin,
    ));

    let comment = build_op(artifact, author_pub, &author_sk, att, OpClass::Comment, 1);
    assert_eq!(
        state.apply_and_relay(&comment),
        Err(ValidationError::RoleForbidsOpClass),
        "Denied overrides any inherited or shared grant"
    );
    assert_eq!(state.op_log.len(), 0);
}

// The cap-bundle model is coherent with the coarse role table: for
// every (role, op-class), role_permits agrees with "role's cap
// ceiling implies the op's required verb".

#[test]
fn caps_agree_with_role_table() {
    let roles = [
        Role::Owner,
        Role::Editor,
        Role::Commenter,
        Role::Viewer,
        Role::Denied,
    ];
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
    for role in roles {
        for op in ops {
            let table = role_permits(role, ArtifactKind::Note, op);
            let bundle =
                role_caps(role).is_some_and(|ceiling| ceiling.implies(verb_for_opclass(op)));
            assert_eq!(
                table, bundle,
                "role_permits and the cap-bundle disagree for {role:?} / {op:?}"
            );
        }
    }
}
