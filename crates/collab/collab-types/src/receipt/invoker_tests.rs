//! A change that names who made it is decided as that invoker, bounded by the
//! person whose device signed it, the same way the device that admitted it
//! decided it live.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ROOT};
use tilo_action_policy::{
    Acting, Answer, Context, Grant, Grantee, KindSet, MemberAccess, Role, WorkspacePolicy,
};
use tilo_actions::{ActionContext, ActionInvoker, ActionKey, Did, VerbCall};

use super::super::*;

const WORKSPACE: &str = "workspace-1";
const FILES: &str = "so.tilo.files";
const TREE: &str = "files.tree";
const AGENT: &str = "claude";

fn tree() -> WrittenDocument<'static> {
    WrittenDocument {
        workspace: WORKSPACE,
        app: FILES,
        document: "tree",
    }
}

/// The file tree's package surface: a write and a trash, each changing the
/// whole tree.
struct Tree;

impl ReceiptSurface for Tree {
    fn kind(&self) -> &str {
        TREE
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        matches!(action, "write" | "trash").then_some((ActionKind::Undoable, None))
    }
    fn editable(&self, _: &[tilo_actions::PathStep]) -> bool {
        false
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        vec![Vec::new()]
    }
}

impl SurfaceSource for Tree {
    fn surface(
        &self,
        _: &WrittenDocument<'_>,
        _: Option<&str>,
        _: Option<&[u8; 32]>,
    ) -> Option<std::sync::Arc<dyn ReceiptSurface>> {
        Some(std::sync::Arc::new(Self))
    }
}

/// Ada and Bob edit the workspace; Vic only reads it. `writes` is what the
/// workspace answers for the agent changing Files, where it answers anything:
/// it then reads there too.
fn policy(writes: Option<Answer>) -> WorkspacePolicy {
    let mut policy = WorkspacePolicy::new(WORKSPACE, "owner");
    policy.members.insert("ada".into(), Role::Editor);
    policy.members.insert("bob".into(), Role::Editor);
    policy.members.insert("vic".into(), Role::Viewer);
    if let Some(writes) = writes {
        let files = TreePath::workspace(WORKSPACE).app(FILES);
        for (id, kinds, answer) in [
            ("agent-reads-files", KindSet::READ, Answer::Allow),
            ("agent-changes-files", KindSet::UNDOABLE, writes),
        ] {
            policy.grants.push(Grant::new(
                id,
                Grantee::agent(AGENT),
                files.clone(),
                kinds,
                answer,
                Grantee::person("ada"),
            ));
        }
    }
    policy
}

/// A cleartext op writing a file into the tree, made by `invoker` (the
/// signer itself when `None`).
fn write_by(invoker: Option<&Acting>) -> crate::op::SignedOp {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "notes.md", "hello").unwrap();
    let mut entry = ActionContext {
        what: Some(Did::Verb(VerbCall::new(
            ActionKey::new(FILES, TREE, "write"),
            [],
        ))),
        ..ActionContext::for_resource("tree", TREE, "Files")
    };
    if let Some(invoker) = invoker {
        entry = entry.with_invoker(ActionInvoker::admitted(invoker));
    }
    doc.commit_with(CommitOptions::default().with_message(entry.change_message().unwrap()));
    let payload = crate::op::DocumentMutation::Incremental(doc.save_after(&[]))
        .encode()
        .unwrap();
    crate::op::SignedOp::new_unsigned(
        payload,
        crate::document_artifact_id(FILES, WORKSPACE, "tree"),
        crate::Ed25519PublicKey::from_bytes([7; 32]),
        crate::document_attestation_id(FILES, WORKSPACE, "tree"),
        crate::role::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        1,
    )
    .with_surface(Some([1; 32]))
}

/// How a receiver decides `op` signed by the device `signer` is.
fn received(op: &crate::op::SignedOp, signer: &MemberAccess) -> Result<Receipt, ReceiptRefusal> {
    let cx = Context::at_secs(1);
    let allows = |invoker: Option<&Acting>, node: &TreePath, kind: ActionKind| {
        ChangeAuthor::answer(signer, invoker, node, kind, &cx) == Answer::Allow
    };
    let surfaces = Surfaces::from_source(std::sync::Arc::new(Tree));
    decide_op(op, &tree(), None, None, &surfaces, &allows)
}

/// The agent a person's computer handed its session to, as the mount names it.
fn mount_agent() -> Acting {
    Acting::agent(AGENT).handed().in_session("session-1")
}

fn person(policy: &WorkspacePolicy, person: &str) -> MemberAccess {
    MemberAccess::person(policy.clone(), person)
}

/// The write node a change claiming `write` is decided at.
fn write_node() -> TreePath {
    let action = ClaimedAction::Verb {
        action: ActionKey::new(FILES, TREE, "write"),
        item: None,
    };
    claim_node(&tree(), &Tree, &action)
}

tilo_test_env::scenario! {
    fn a_change_is_decided_as_who_made_it;

    phase an_agent_answered_only_read_in_files_cannot_have_a_write_admitted;
    {
        let policy = policy(Some(Answer::Deny));
        let op = write_by(Some(&mount_agent()));
        assert!(matches!(
            received(&op, &person(&policy, "ada")),
            Err(ReceiptRefusal::NotGranted { .. })
        ));
    }

    phase with_writing_in_files_answered_its_write_is_admitted;
    {
        let policy = policy(Some(Answer::Allow));
        let op = write_by(Some(&mount_agent()));
        assert_eq!(received(&op, &person(&policy, "ada")), Ok(Receipt::Admit));
        // Handed by Ada, with nothing answered for it, it carries what her
        // device gives its agents.
        assert_eq!(received(&op, &person(&self::policy(None), "ada")), Ok(Receipt::Admit));
    }

    phase the_signing_person_bounds_the_agent;
    {
        let policy = policy(Some(Answer::Allow));
        let op = write_by(Some(&mount_agent()));
        assert!(
            received(&op, &person(&policy, "vic")).is_err(),
            "a viewer's agent writes no more than the viewer"
        );
    }

    phase a_change_naming_no_invoker_is_decided_as_its_signer;
    {
        let policy = policy(Some(Answer::Deny));
        let op = write_by(None);
        assert_eq!(received(&op, &person(&policy, "ada")), Ok(Receipt::Admit));
        assert!(received(&op, &person(&policy, "vic")).is_err());
    }

    phase only_the_persons_own_device_hands_its_agent_the_delegation;
    {
        let policy = policy(None);
        let for_ada = mount_agent().acting_for(Acting::person("ada"));
        let op = write_by(Some(&for_ada));
        assert_eq!(received(&op, &person(&policy, "ada")), Ok(Receipt::Admit));
        // Bob's device claiming Ada's agent was handed her session.
        assert!(received(&op, &person(&policy, "bob")).is_err());
        // The agent's own key claiming it was handed one.
        let agent_key = MemberAccess::new(
            policy.clone(),
            Acting::agent(AGENT).acting_for(Acting::person("ada")),
        );
        assert!(received(&op, &agent_key).is_err());
        // A device that is not Ada's own.
        assert!(received(&op, &person(&policy, "ada").on_foreign_device()).is_err());
    }

    phase the_writer_and_every_receiver_decide_a_change_alike;
    {
        for answered in [None, Some(Answer::Deny), Some(Answer::Ask), Some(Answer::Allow)] {
            let policy = policy(answered);
            // The live path: Ada's device admits the agent it handed its session to.
            let live = MemberAccess::person(policy.clone(), "ada")
                .as_acting(mount_agent().acting_for(Acting::person("ada")))
                .unwrap()
                .answer(&write_node(), ActionKind::Undoable, &Context::at_secs(1));
            let receipt = received(&write_by(Some(&mount_agent())), &person(&policy, "ada"));
            assert_eq!(
                live == Answer::Allow,
                receipt == Ok(Receipt::Admit),
                "{answered:?}: live {live:?}, received {receipt:?}"
            );
        }
    }
}
