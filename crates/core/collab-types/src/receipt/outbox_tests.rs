//! A resource the workspace owns that queues work for a device: only what a
//! change queued in an outbox reaches outside the workspace, so a device
//! settling an entry it carried out is doing what its role already allows,
//! and only queueing a new one is reaching outside.

use std::sync::Arc;

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc};
use tilo_action_policy::{
    Acting, Answer, Context, Grant, Grantee, MemberAccess, Role, TreePath, WorkspacePolicy,
};
use tilo_actions::{
    ActionContext, ActionInvoker, ActionKey, Did, EditOp, FieldEdit, PathStep, VerbCall,
};

use super::super::*;

const WORKSPACE: &str = "workspace-1";
const APP: &str = "so.tilo.mail";
const KIND: &str = "mail.mailbox";
const ENTRY: &str = "m1";

fn document() -> WrittenDocument<'static> {
    WrittenDocument {
        workspace: WORKSPACE,
        app: APP,
        document: "",
    }
}

/// The workspace mailbox's outbox, whose entries any device holding an
/// account carries out: one unit, as its surface declares the field.
fn outbox() -> UnitPath {
    vec![PathStep::Field("outbox".into())]
}

/// Mail's mailbox as its package publishes it: a send queues a message in the
/// outbox, and an entry is settled by the device that ran it.
struct Mailbox;

impl ReceiptSurface for Mailbox {
    fn kind(&self) -> &str {
        KIND
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        (action == "send").then_some((ActionKind::Outside, None))
    }
    fn editable(&self, path: &[PathStep]) -> bool {
        matches!(path.first(), Some(PathStep::Field(field)) if field == "outbox" || field == "title")
    }
    fn footprint(&self, action: &ClaimedAction) -> Vec<UnitPath> {
        match action {
            ClaimedAction::Edit { .. } => vec![outbox()],
            _ => vec![Vec::new()],
        }
    }
    fn outboxes(&self) -> Vec<UnitPath> {
        vec![outbox()]
    }
}

impl SurfaceSource for Mailbox {
    fn surface(
        &self,
        _: &WrittenDocument<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn ReceiptSurface>> {
        Some(Arc::new(Self))
    }
}

/// The workspace's mailbox as a receiver holds it, one message waiting in the
/// outbox for the device that holds its account.
fn held() -> AutoCommit {
    let mut doc = AutoCommit::new();
    let index = doc
        .put_object(ROOT, crate::units::UNIT_INDEX, ObjType::List)
        .unwrap();
    doc.insert(&index, 0, "outbox").unwrap();
    doc.put(ROOT, "title", "Mailbox").unwrap();
    let outbox = doc.put_object(ROOT, "outbox", ObjType::Map).unwrap();
    let entry = doc.put_object(&outbox, ENTRY, ObjType::Map).unwrap();
    doc.put(&entry, "k", "a0").unwrap();
    let value = doc.put_object(&entry, "v", ObjType::Map).unwrap();
    doc.put(&value, "to", "ada@example.test").unwrap();
    doc.commit();
    doc
}

fn with_invoker(context: ActionContext, by: &str) -> ActionContext {
    context.with_invoker(ActionInvoker::person(by))
}

/// The change a device makes settling the entry it ran: an edit inside the
/// entry, made as who it names.
fn settle(before: &AutoCommit, by: &str) -> crate::op::SignedOp {
    let heads = before.clone().get_heads();
    let mut doc = before.clone();
    let outbox = doc.get(ROOT, "outbox").unwrap().unwrap().1;
    let entry = doc.get(&outbox, ENTRY).unwrap().unwrap().1;
    let value = doc.get(&entry, "v").unwrap().unwrap().1;
    doc.put(&value, "settled", "done").unwrap();
    let context = with_invoker(
        ActionContext {
            what: Some(Did::Edit(FieldEdit {
                ty: "mail".into(),
                field: "outbox".into(),
                path: vec![
                    PathStep::Field("outbox".into()),
                    PathStep::Item(ENTRY.into()),
                ],
                op: EditOp::Settle,
                old: None,
                new: None,
                args: std::collections::BTreeMap::default(),
            })),
            ..ActionContext::for_resource("", KIND, "Mail")
        },
        by,
    );
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    op(&mut doc, &heads)
}

/// The change a device makes queueing a message to send: the entry it pushes.
fn send(before: &AutoCommit, by: &str) -> crate::op::SignedOp {
    let heads = before.clone().get_heads();
    let mut doc = before.clone();
    let outbox = doc.get(ROOT, "outbox").unwrap().unwrap().1;
    let entry = doc.put_object(&outbox, "m2", ObjType::Map).unwrap();
    doc.put(&entry, "k", "a1").unwrap();
    let value = doc.put_object(&entry, "v", ObjType::Map).unwrap();
    doc.put(&value, "to", "bob@example.test").unwrap();
    let context = with_invoker(
        ActionContext {
            what: Some(Did::Verb(VerbCall::new(
                ActionKey::new(APP, KIND, "send"),
                [],
            ))),
            ..ActionContext::for_resource("", KIND, "Mail")
        },
        by,
    );
    doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
    op(&mut doc, &heads)
}

fn op(doc: &mut AutoCommit, heads: &[automerge::ChangeHash]) -> crate::op::SignedOp {
    let payload = crate::op::DocumentMutation::Incremental(doc.save_after(heads))
        .encode()
        .unwrap();
    crate::op::SignedOp::new_unsigned(
        payload,
        crate::document_artifact_id(APP, WORKSPACE, ""),
        crate::Ed25519PublicKey::from_bytes([7; 32]),
        crate::document_attestation_id(APP, WORKSPACE, ""),
        crate::role::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        1,
    )
    .with_surface(Some([1; 32]))
}

/// Ada made the workspace and owns it; Vic only reads it.
fn policy() -> WorkspacePolicy {
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy.members.insert("vic".into(), Role::Viewer);
    policy.grants.push(Grant::new(
        "vic-reads-mail",
        Grantee::person("vic"),
        TreePath::workspace(WORKSPACE),
        tilo_action_policy::KindSet::READ,
        Answer::Allow,
        Grantee::person("ada"),
    ));
    policy
}

/// The workspace's policy with `person` granted the Mail app itself: its mailbox is a private
/// destination, so a role's grant on the workspace reaches it only through a grant naming the
/// app.
fn writes_mail(person: &str) -> WorkspacePolicy {
    let mut policy = policy();
    policy.grants.push(Grant::new(
        format!("{person}-writes-mail"),
        Grantee::person(person),
        TreePath::workspace(WORKSPACE).app(APP),
        tilo_action_policy::KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person("ada"),
    ));
    policy
}

/// How `op` is decided by a receiver holding `held` whose signer is `signer`.
fn received(
    op: &crate::op::SignedOp,
    held: &AutoCommit,
    signer: &MemberAccess,
) -> Result<Receipt, ReceiptRefusal> {
    let cx = Context::at_secs(1);
    let allows = |invoker: Option<&Acting>, node: &TreePath, kind: ActionKind| {
        ChangeAuthor::answer(signer, invoker, node, kind, &cx) == Answer::Allow
    };
    let surfaces = Surfaces::from_source(Arc::new(Mailbox));
    let held = held.clone();
    let mut holding = held.clone();
    decide_op_pinned(
        op,
        &document(),
        (Some(KIND), Some(held), None),
        Some(holding.document()),
        &surfaces,
        &allows,
    )
}

fn page(policy: &WorkspacePolicy, person: &str) -> MemberAccess {
    MemberAccess::person(policy.clone(), person).on_foreign_device()
}

/// How a receiver that cannot read the change decides `action`, by the units its
/// action may write and its claim: `by` made it.
fn sealed(
    action: ClaimedAction,
    units: Vec<UnitPath>,
    by: &str,
    signer: &MemberAccess,
) -> Result<Receipt, ReceiptRefusal> {
    let claim = crate::claim::ChangeClaim {
        change: [1; 32],
        action,
        units,
        invoker: Some(Acting::person(by).into()),
    };
    let cx = Context::at_secs(1);
    let allows = |invoker: Option<&Acting>, node: &TreePath, kind: ActionKind| {
        ChangeAuthor::answer(signer, invoker, node, kind, &cx) == Answer::Allow
    };
    let surface_for = |_: Option<&str>| Some(Arc::new(Mailbox) as Arc<dyn ReceiptSurface>);
    check_claims_by_kind(
        &[claim],
        &document(),
        Some(KIND),
        false,
        &surface_for,
        &allows,
    )
}

tilo_test_env::scenario! {
    /// What a device of the workspace does to a mailbox the workspace owns is
    /// decided by what its change queued, not by what its action may queue.
    fn an_outbox_settled_is_no_reach_outside;

    phase the_owner_settles_what_their_device_ran;
    {
        let held = held();
        assert_eq!(
            received(&settle(&held, "ada"), &held, &page(&policy(), "ada")),
            Ok(Receipt::Admit),
            "settling an entry queues nothing, so a device of the owner may do it \
             as far as it may write the resource"
        );
        assert_eq!(
            received(&settle(&held, "ada"), &held, &MemberAccess::person(policy(), "ada")),
            Ok(Receipt::Admit)
        );
    }

    phase a_member_settles_what_they_sent;
    {
        let mut policy = writes_mail("bob");
        policy.members.insert("bob".into(), Role::Editor);
        let held = held();
        assert_eq!(
            received(&settle(&held, "bob"), &held, &page(&policy, "bob")),
            Ok(Receipt::Admit),
            "an editor's page settles the entry it ran"
        );
    }

    phase queueing_a_message_still_reaches_outside;
    {
        let held = held();
        for (page, by) in [(page(&policy(), "ada"), "ada"), (page(&policy(), "vic"), "vic")] {
            assert!(
                matches!(
                    received(&send(&held, by), &held, &page),
                    Err(ReceiptRefusal::NotGranted {
                        kind: ActionKind::Outside,
                        ..
                    })
                ),
                "a change that puts a message in the outbox reaches outside: {page:?}"
            );
        }
    }

    phase a_sealed_settle_is_no_reach_outside;
    // A receiver that cannot read the change decides it by its claim, which
    // names what its action may write: an edit whose path lies inside an
    // entry wrote there, and it queued nothing, while an edit of the outbox
    // itself may have put an entry in it.
    {
        let mut policy = writes_mail("bob");
        policy.members.insert("bob".into(), Role::Editor);
        let settle = ClaimedAction::Edit {
            kind: KIND.into(),
            path: vec![
                PathStep::Field("outbox".into()),
                PathStep::Item(ENTRY.into()),
            ],
        };
        assert_eq!(
            sealed(
                settle,
                vec![outbox()],
                "bob",
                &page(&policy, "bob")
            ),
            Ok(Receipt::Admit),
            "settling an entry is what an editor may write"
        );
        let whole = ClaimedAction::Edit {
            kind: KIND.into(),
            path: vec![PathStep::Field("outbox".into())],
        };
        assert!(
            matches!(
                sealed(
                    whole,
                    vec![outbox()],
                    "bob",
                    &page(&policy, "bob")
                ),
                Err(ReceiptRefusal::NotGranted {
                    kind: ActionKind::Outside,
                    ..
                })
            ),
            "an edit of the outbox itself may queue, and reaches outside"
        );
    }

    phase a_viewer_writes_nothing_of_the_mailbox;
    {
        let held = held();
        assert!(matches!(
            received(&settle(&held, "vic"), &held, &page(&policy(), "vic")),
            Err(ReceiptRefusal::NotGranted { .. })
        ));
    }
}
