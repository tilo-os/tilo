//! Op-application enforcement: the room's grants, proven on the **real**
//! validate/apply path (`validate_inbound_op` +
//! `ChannelState::apply_and_relay`), not a helper.
//!
//! Every change names its action ([`collab_types::ChangeClaim`]) and is
//! decided against the app's surface. Headline proof: a `commenter` may
//! comment but an edit from the same actor is rejected **at validation** —
//! not applied, not relayed. Plus finer grants: an `editor` held to comments inside one app
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

use std::sync::Arc;

use collab_types::claim::ClaimedAction;
use collab_types::{
    ArtifactId, ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Claim,
    DEFAULT_ROOM, DocumentMutation, Ed25519PublicKey, OpKind, ReceiptSurface, Record, Role,
    RoleAttestation, Signature, SignedOp, SurfaceSource, Surfaces, UnitPath, ValidationError,
    generate_keypair, sign_event, sign_op, validate_inbound_op,
};
use tilo_action_policy::{ActionKind, Answer, Grant, KindSet, Subject, TreePath, roles};
use tilo_actions::{ActionContext, ActionKey, Did, EditOp, FieldEdit, PathStep, VerbCall};

const APP: &str = "notes";
const KIND: &str = "note";

/// The notes app's surface as a receiver reads it: `comment` sits under
/// the comments it writes; every field offers edits.
struct Notes;

impl ReceiptSurface for Notes {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        (action == "comment").then(|| (ActionKind::Undoable, Some(roles::COMMENTS_FIELD.into())))
    }
    fn editable(&self, _: &[PathStep]) -> bool {
        true
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        vec![Vec::new()]
    }
}

impl SurfaceSource for Notes {
    fn surface(&self, _: &Record<'_>, _: Option<&str>) -> Option<Arc<dyn ReceiptSurface>> {
        Some(Arc::new(Self))
    }
}

/// What a change does: comment on the note, or edit its body.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    Comment,
    Edit,
}

fn named(change: Change) -> Did {
    match change {
        Change::Comment => Did::Verb(VerbCall::new(ActionKey::new(APP, KIND, "comment"), [])),
        Change::Edit => Did::Edit(FieldEdit {
            ty: KIND.into(),
            field: "body".into(),
            path: vec![PathStep::Field("body".into())],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        }),
    }
}

/// One change doing `change`, as the payload of a change op: the change names it.
fn change_payload(change: Change, seq: u64) -> Vec<u8> {
    use automerge::transaction::{CommitOptions, Transactable};
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "seq", seq).unwrap();
    let context = ActionContext {
        what: Some(named(change)),
        ..ActionContext::for_resource("n1", KIND, "Note")
    };
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    let mutation = DocumentMutation::Incremental(doc.save_after(&[]));
    mutation.encode().unwrap()
}

/// The node a comment on `artifact` is decided at.
fn comments_of(artifact: ArtifactId) -> TreePath {
    TreePath::room(DEFAULT_ROOM)
        .app(APP)
        .kind(KIND)
        .resource(artifact.0.to_string())
        .field(roles::COMMENTS_FIELD)
}

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
    state.artifact_apps.insert(artifact, APP.into());
    state.surfaces = Surfaces::from_source(Arc::new(Notes));
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
    change: Change,
    seq: u64,
) -> SignedOp {
    let payload = change_payload(change, seq);
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact,
        author_pub,
        attestation_id,
        OpKind::Change,
        t(10),
        seq,
    );
    sign_op(&mut op, author_sk).unwrap();
    op
}

/// A governance op of `kind` (a compaction or a redaction).
fn governance_op(
    artifact: ArtifactId,
    author_pub: Ed25519PublicKey,
    author_sk: &SigningKey,
    attestation_id: AttestationId,
    kind: OpKind,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        b"payload".to_vec(),
        artifact,
        author_pub,
        attestation_id,
        kind,
        t(10),
        1,
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
    let comment = build_op(artifact, author_pub, &author_sk, att, Change::Comment, 1);
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
    let edit = build_op(artifact, author_pub, &author_sk, att, Change::Edit, 2);
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
    let splice = build_op(artifact, author_pub, &author_sk, att, Change::Edit, 3);
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
    let splice_ok = build_op(artifact, author_pub, &author_sk, att, Change::Edit, 1);
    assert!(
        state.clone().apply_and_relay(&splice_ok).is_ok(),
        "unrestricted editor may splice"
    );

    let finance = TreePath::root()
        .push(tilo_action_policy::Level::Room, DEFAULT_ROOM)
        .app(APP);
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

    let splice = build_op(artifact, author_pub, &author_sk, att, Change::Edit, 1);
    assert!(
        not_granted(validate_inbound_op(&splice, &mut state.clone())),
        "the app deny refuses the editor's splice"
    );
    assert!(not_granted(state.apply_and_relay(&splice)));
    assert_eq!(state.op_log.len(), 0, "restricted splice was NOT applied");

    let comment = build_op(artifact, author_pub, &author_sk, att, Change::Comment, 1);
    assert!(
        state.apply_and_relay(&comment).is_ok(),
        "comments inside the app are still allowed"
    );
    assert_eq!(state.op_log.len(), 1);

    // Phase: a grant on one artifact lets a viewer comment there only.
    let (mut state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(Role::Viewer);
    let other = ArtifactId::new();
    state.artifacts.insert(other, ArtifactKind::Note);

    let denied = build_op(artifact, author_pub, &author_sk, att, Change::Comment, 1);
    assert!(
        not_granted(validate_inbound_op(&denied, &mut state.clone())),
        "viewer cannot comment by role alone"
    );

    state.grants.push(grant(
        "share",
        Subject::Person(hex::encode(author_pub.0)),
        comments_of(artifact),
        Answer::Allow,
    ));
    let comment = build_op(artifact, author_pub, &author_sk, att, Change::Comment, 1);
    assert!(
        state.apply_and_relay(&comment).is_ok(),
        "the grant allows comments on the shared artifact"
    );
    assert_eq!(state.op_log.len(), 1);

    let elsewhere = build_op(other, author_pub, &author_sk, att, Change::Comment, 2);
    assert!(
        not_granted(state.apply_and_relay(&elsewhere)),
        "the grant does not leak to other artifacts"
    );
    let edit_shared = build_op(artifact, author_pub, &author_sk, att, Change::Edit, 2);
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
    let comment = build_op(artifact, author_pub, &author_sk, att, Change::Comment, 1);
    assert!(
        not_granted(state.apply_and_relay(&comment)),
        "the room-wide block is more specific than the root grant"
    );
    assert_eq!(state.op_log.len(), 0);
}

/// The role presets decide comments, edits and the governance ops as their
/// table says.
#[test]
fn role_presets_decide_each_kind_of_change() {
    for role in Role::ALL {
        for change in [Change::Comment, Change::Edit] {
            let (state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(role);
            let signed = build_op(artifact, author_pub, &author_sk, att, change, 1);
            let allowed = !not_granted(validate_inbound_op(&signed, &mut state.clone()));
            let expected = match role {
                Role::Owner | Role::Editor => true,
                Role::Commenter => change == Change::Comment,
                Role::Viewer => false,
            };
            assert_eq!(allowed, expected, "{role:?} / {change:?}");
        }
        for kind in [OpKind::Compaction, OpKind::Redaction] {
            let (state, author_sk, author_pub, att, artifact, _ch) = channel_with_author(role);
            let signed = governance_op(artifact, author_pub, &author_sk, att, kind);
            // A redaction then checks its payload; only the grant decision
            // matters here.
            let allowed = !not_granted(validate_inbound_op(&signed, &mut state.clone()));
            assert_eq!(allowed, role == Role::Owner, "{role:?} / {kind:?}");
        }
    }
}
