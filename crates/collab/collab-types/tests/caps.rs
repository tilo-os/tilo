//! Op-application enforcement: the workspace's grants, proven on the real
//! validate-then-record path the broker runs, not a helper.
//!
//! Every change names its action ([`collab_types::ChangeClaim`]) and is
//! decided against the app's surface. Headline proof: a `commenter` may
//! comment but an edit from the same actor is rejected **at validation** —
//! not applied, not relayed. Plus finer grants: an `editor` held to comments
//! inside one app has its edit denied, and a grant to one person on one
//! artifact extends a viewer's authority there only.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;

use std::sync::Arc;

use collab_types::claim::ClaimedAction;
use collab_types::{
    ArtifactId, ArtifactKind, AttestationId, DEFAULT_WORKSPACE, DocumentMutation, Ed25519PublicKey,
    InboundState, OpKind, ReceiptSurface, Record, Role, SignedOp, SurfaceSource, Surfaces,
    UnitPath, ValidationError, generate_keypair, sign_op, validate_admitted_inbound_op,
};
use tilo_action_policy::{
    ActionKind, Answer, Authority, Grant, Grantee, KindSet, Party, TreePath, roles,
};
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
    fn surface(
        &self,
        _: &Record<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
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
    TreePath::workspace(DEFAULT_WORKSPACE)
        .app(APP)
        .kind(KIND)
        .resource(artifact.0.to_string())
        .field(roles::COMMENTS_FIELD)
}

fn not_granted(result: Result<(), ValidationError>) -> bool {
    result.is_err_and(|error| matches!(error, ValidationError::NotGranted(_)))
}

fn grant(id: &str, subject: Grantee, node: TreePath, answer: Answer) -> Grant {
    Grant::new(
        id,
        subject,
        node,
        KindSet::UNDOABLE,
        answer,
        Grantee::role(Role::Owner),
    )
}

fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(secs))
        .unwrap()
}

const CLOCK_SKEW: Duration = Duration::from_secs(30);

/// A Note artifact in the default workspace, and an author holding `role`
/// under the role presets plus `extra` grants.
struct Fixture {
    state: InboundState,
    author_sk: SigningKey,
    author_pub: Ed25519PublicKey,
    artifact: ArtifactId,
    grants: Vec<Grant>,
    role: Role,
}

impl Fixture {
    fn new(role: Role) -> Self {
        let (author_sk, author_pub) = generate_keypair();
        let artifact = ArtifactId::new();
        let mut state = InboundState::default();
        state.artifacts.insert(artifact, ArtifactKind::Note);
        state.artifact_apps.insert(artifact, APP.into());
        state.surfaces = Surfaces::from_source(Arc::new(Notes));
        Self {
            state,
            author_sk,
            author_pub,
            artifact,
            grants: roles::workspace_role_grants(DEFAULT_WORKSPACE, &Grantee::role(Role::Owner)),
            role,
        }
    }

    fn with(mut self, extra: Grant) -> Self {
        self.grants.push(extra);
        self
    }

    /// The author's standing: their own key and their role, under the
    /// workspace's grants.
    fn subjects(&self) -> Vec<Grantee> {
        vec![
            Grantee::person(hex::encode(self.author_pub.0)),
            Grantee::role(self.role),
        ]
    }

    fn author(&self) -> Authority<'_> {
        Authority::person(Party::new(self.subjects(), &self.grants))
    }

    fn op(&self, change: Change, seq: u64) -> SignedOp {
        let mut op = SignedOp::new_unsigned(
            change_payload(change, seq),
            self.artifact,
            self.author_pub,
            AttestationId::new(),
            OpKind::Change,
            t(10),
            seq,
        )
        // The notes app's surface (the test source returns it for any hash).
        .with_surface(Some([1; 32]));
        sign_op(&mut op, &self.author_sk).unwrap();
        op
    }

    /// A governance op of `kind` (a compaction or a redaction).
    fn governance_op(&self, kind: OpKind) -> SignedOp {
        let mut op = SignedOp::new_unsigned(
            b"payload".to_vec(),
            self.artifact,
            self.author_pub,
            AttestationId::new(),
            kind,
            t(10),
            1,
        );
        sign_op(&mut op, &self.author_sk).unwrap();
        op
    }

    /// Validate, then record — the order the broker's delivery runs in.
    fn admit(&mut self, op: &SignedOp) -> Result<(), ValidationError> {
        let grants = self.grants.clone();
        let author = Authority::person(Party::new(self.subjects(), &grants));
        validate_admitted_inbound_op(op, &author, &mut self.state, t(10), CLOCK_SKEW)?;
        self.state.record_validated_op(op.clone());
        Ok(())
    }

    fn validates(&self, op: &SignedOp) -> Result<(), ValidationError> {
        let author = self.author();
        validate_admitted_inbound_op(op, &author, &mut self.state.clone(), t(10), CLOCK_SKEW)
    }
}

// The headline proof: commenter may comment, may NOT edit — enforced
// at validation, not applied, not relayed.

#[test]
fn commenter_may_comment_but_not_edit_on_the_real_apply_path() {
    let mut f = Fixture::new(Role::Commenter);

    // A comment op passes validation AND is recorded to the op log that
    // fan-out relays from.
    let comment = f.op(Change::Comment, 1);
    assert!(
        f.admit(&comment).is_ok(),
        "commenter's comment op must validate + apply"
    );
    assert_eq!(f.state.op_log.len(), 1, "the comment op was applied");
    assert_eq!(
        f.state.last_seq_per_author.get(&f.author_pub).copied(),
        Some(1),
        "seq advanced for the applied comment"
    );

    // An EDIT op from the SAME actor is rejected at validation: the real
    // validate path returns the role denial...
    let edit = f.op(Change::Edit, 2);
    assert!(
        not_granted(f.validates(&edit)),
        "commenter's edit must be rejected at validation"
    );

    // ...and on the apply path it is neither applied nor relayed: the op
    // log and the seq counter do not move.
    let res = f.admit(&edit);
    assert!(not_granted(res));
    assert_eq!(f.state.op_log.len(), 1, "rejected edit was NOT applied");
    assert_eq!(
        f.state.last_seq_per_author.get(&f.author_pub).copied(),
        Some(1),
        "rejected edit did NOT advance seq (nothing to relay)"
    );
}

/// Finer grants do real work on the apply path: a Deny on one app holds an
/// Editor to comments there, a grant on one artifact extends a Viewer on
/// exactly that artifact, and a blocked member stays blocked whatever a
/// broader grant says.
#[test]
fn finer_grants_narrow_extend_and_block_on_the_real_path() {
    // Phase: an editor held to comments inside one app.
    let mut f = Fixture::new(Role::Editor);
    let splice_ok = f.op(Change::Edit, 1);
    assert!(
        f.admit(&splice_ok).is_ok(),
        "unrestricted editor may splice"
    );

    let finance = TreePath::root()
        .push(tilo_action_policy::Level::Workspace, DEFAULT_WORKSPACE)
        .app(APP);
    f = f
        .with(grant(
            "finance",
            Grantee::role(Role::Editor),
            finance.clone(),
            Answer::Deny,
        ))
        .with(grant(
            "finance-comments",
            Grantee::role(Role::Editor),
            finance
                .rest()
                .push(tilo_action_policy::Level::Field, roles::COMMENTS_FIELD),
            Answer::Allow,
        ));

    let splice = f.op(Change::Edit, 2);
    assert!(
        not_granted(f.validates(&splice)),
        "the app deny refuses the editor's splice"
    );
    assert!(not_granted(f.admit(&splice)));
    assert_eq!(f.state.op_log.len(), 1, "restricted splice was NOT applied");

    let comment = f.op(Change::Comment, 3);
    assert!(
        f.admit(&comment).is_ok(),
        "comments inside the app are still allowed"
    );
    assert_eq!(f.state.op_log.len(), 2);

    // Phase: a grant on one artifact lets a viewer comment there only.
    let mut f = Fixture::new(Role::Viewer);
    let other = ArtifactId::new();
    f.state.artifacts.insert(other, ArtifactKind::Note);
    let author_pub = f.author_pub;

    let denied = f.op(Change::Comment, 1);
    assert!(
        not_granted(f.validates(&denied)),
        "viewer cannot comment by role alone"
    );

    let artifact = f.artifact;
    f = f.with(grant(
        "share",
        Grantee::person(hex::encode(author_pub.0)),
        comments_of(artifact),
        Answer::Allow,
    ));
    let comment = f.op(Change::Comment, 1);
    assert!(
        f.admit(&comment).is_ok(),
        "the grant allows comments on the shared artifact"
    );
    assert_eq!(f.state.op_log.len(), 1);

    let mut elsewhere = f.op(Change::Comment, 2);
    elsewhere.artifact = other;
    sign_op(&mut elsewhere, &f.author_sk).unwrap();
    assert!(
        not_granted(f.admit(&elsewhere)),
        "the grant does not leak to other artifacts"
    );
    let edit_shared = f.op(Change::Edit, 2);
    assert!(
        not_granted(f.admit(&edit_shared)),
        "a comment grant does not grant edit"
    );

    // Phase: a blocked member is not rescued by a broader grant.
    let mut f = Fixture::new(Role::Editor);
    let person = Grantee::person(hex::encode(f.author_pub.0));
    f = f
        .with(Grant::new(
            "blocked",
            person.clone(),
            TreePath::workspace(DEFAULT_WORKSPACE),
            KindSet::ALL,
            Answer::Deny,
            Grantee::role(Role::Owner),
        ))
        .with(Grant::new(
            "everything",
            person,
            TreePath::root(),
            KindSet::ALL,
            Answer::Allow,
            Grantee::role(Role::Owner),
        ));
    let comment = f.op(Change::Comment, 1);
    assert!(
        not_granted(f.admit(&comment)),
        "the workspace-wide block is more specific than the root grant"
    );
    assert_eq!(f.state.op_log.len(), 0);
}

/// The role presets decide comments, edits and the governance ops as their
/// table says.
#[test]
fn role_presets_decide_each_kind_of_change() {
    for role in Role::ALL {
        for change in [Change::Comment, Change::Edit] {
            let f = Fixture::new(role);
            let signed = f.op(change, 1);
            let allowed = !not_granted(f.validates(&signed));
            let expected = match role {
                Role::Owner | Role::Moderator | Role::Editor => true,
                Role::Commenter => change == Change::Comment,
                Role::Viewer => false,
            };
            assert_eq!(allowed, expected, "{role:?} / {change:?}");
        }
        for kind in [OpKind::Compaction, OpKind::Redaction] {
            let f = Fixture::new(role);
            let signed = f.governance_op(kind);
            // A redaction then checks its payload; only the grant decision
            // matters here.
            let allowed = !not_granted(f.validates(&signed));
            assert_eq!(allowed, role == Role::Owner, "{role:?} / {kind:?}");
        }
    }
}
