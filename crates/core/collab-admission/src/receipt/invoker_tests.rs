//! A change that names who made it is decided as that invoker, bounded by the
//! person whose device signed it, the same way the device that admitted it
//! decided it live.

use automerge::transaction::{CommitOptions, Transactable};
use automerge::{AutoCommit, ROOT};
use tilo_action_policy::{
    Acting, Answer, Context, Grant, Grantee, KindSet, MemberAccess, Role, WorkspacePolicy,
};
use tilo_actions::{ActionContext, ActionInvoker, ActionKey, Did, VerbCall};

use super::*;

pub(super) const WORKSPACE: &str = "workspace-1";
pub(super) const FILES: &str = "so.tilo.files";
pub(super) const TREE: &str = "files.tree";
const AGENT: &str = "claude";

pub(super) fn tree() -> WrittenDocument<'static> {
    WrittenDocument {
        workspace: WORKSPACE,
        app: FILES,
        document: "tree",
    }
}

/// The file tree's package surface: a write and a trash, each changing the
/// whole tree.
pub(super) struct Tree;

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
pub(super) fn policy(writes: Option<Answer>) -> WorkspacePolicy {
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
                Grantee::agent(AGENT, "ada"),
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
fn write_by(invoker: Option<&Acting>) -> collab_types::op::SignedOp {
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
    let payload = collab_types::op::DocumentMutation::Incremental(doc.save_after(&[]))
        .encode()
        .unwrap();
    collab_types::op::SignedOp::new_unsigned(
        payload,
        collab_types::document_artifact_id(FILES, WORKSPACE, "tree"),
        collab_types::Ed25519PublicKey::from_bytes([7; 32]),
        collab_types::document_attestation_id(FILES, WORKSPACE, "tree"),
        collab_types::role::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        1,
    )
    .with_surface(Some([1; 32]))
}

/// The same write sealed: a server and every device that has not opened it
/// read only the claims its envelope carries, and `plaintext` is what a
/// device opens it to.
fn sealed(op: &collab_types::op::SignedOp) -> (collab_types::op::SignedOp, DocumentMutation) {
    let plaintext = op.payload.clone();
    let Ok(collab_types::op::DocumentMutation::Incremental(bytes)) =
        collab_types::op::DocumentMutation::decode(&plaintext)
    else {
        panic!("a write is an incremental mutation");
    };
    let claims =
        collab_types::claim::claims_of(&crate::claim::parse_changes(&bytes).unwrap(), |_| {
            vec![Vec::new()]
        })
        .unwrap();
    let mut sealed = op.clone().with_claims(claims);
    sealed.payload = vec![0; 16];
    sealed.encryption = Some(collab_types::op::EncryptionMetadata::content_key(
        1,
        collab_types::ContentRetention::of_frame(&plaintext),
    ));
    (sealed, DocumentMutation::Incremental(bytes))
}

/// How a receiver decides `op` signed by the device `signer` is.
pub(super) fn received(
    op: &collab_types::op::SignedOp,
    signer: &MemberAccess,
) -> Result<Receipt, ReceiptRefusal> {
    let cx = Context::at_secs(1);
    let allows = |invoker: Option<&Acting>, node: &TreePath, kind: ActionKind| {
        ChangeAuthor::answer(signer, invoker, node, kind, &cx) == Answer::Allow
    };
    let surfaces = Surfaces::from_source(std::sync::Arc::new(Tree));
    decide_op(op, &tree(), None, None, &surfaces, &allows)
}

/// The agent a person's computer handed its session to, as the mount names it.
pub(super) fn mount_agent() -> Acting {
    Acting::agent(AGENT, "ada").handed().in_session("session-1")
}

pub(super) fn person(policy: &WorkspacePolicy, person: &str) -> MemberAccess {
    MemberAccess::person(policy.clone(), person)
}

/// The write node a change claiming `write` is decided at.
pub(super) fn write_node() -> TreePath {
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

    phase a_device_writes_only_for_its_person_and_the_agents_it_runs;
    {
        // A change names whoever made it, and only that member's own device,
        // or an agent it runs, may write it. Another member's device is
        // refused whatever its grants: carrying theirs needs their own call.
        let for_ada = mount_agent().acting_for(Acting::person("ada"));
        let op = write_by(Some(&for_ada));
        assert_eq!(received(&op, &person(&policy(None), "ada")), Ok(Receipt::Admit));
        assert!(
            received(&op, &person(&policy(None), "bob")).is_err(),
            "Bob's device may not write for Ada's agent"
        );
        assert!(
            received(&op, &person(&policy(None), "vic")).is_err(),
            "a viewer's device may not write for Ada's agent"
        );
        let for_vic = mount_agent().acting_for(Acting::person("vic"));
        assert!(
            received(&write_by(Some(&for_vic)), &person(&policy(None), "bob")).is_err(),
            "the agent's person is not the signer's"
        );
        // The agent's own key writes as itself: the member it names is the
        // device.
        let agent_key = MemberAccess::new(
            policy(None),
            Acting::agent(AGENT, "ada").acting_for(Acting::person("ada")),
        );
        assert_eq!(received(&op, &agent_key), Ok(Receipt::Admit));
        // A device that is not Ada's own holds what such a device may.
        assert!(received(&op, &person(&policy(None), "ada").on_foreign_device()).is_err());
    }

    phase a_server_signs_only_what_its_device_may_write_for;
    {
        // The owner's server signs what it carries out for itself: a change
        // it names another member made is refused, whoever that member is.
        let server = person(&policy(Some(Answer::Allow)), "owner");
        let agent_device = Acting::agent(AGENT, "ada").acting_for(Acting::person("ada"));
        for (invoker, admitted) in [
            (Acting::person("owner"), true),
            (Acting::person("ada"), false),
            (mount_agent().acting_for(Acting::person("ada")), false),
            (agent_device, false),
            (Acting::person("vic"), false),
            (Acting::person("stranger"), false),
            (mount_agent().acting_for(Acting::person("stranger")), false),
        ] {
            let receipt = received(&write_by(Some(&invoker)), &server);
            assert_eq!(receipt.is_ok(), admitted, "{invoker:?}: {receipt:?}");
        }
    }

    phase a_keys_agent_is_written_by_its_persons_device_or_refused;
    {
        let key = tilo_action_policy::key_agent_id(&[9; 32]);
        let claimed = Acting::handed_agent(key.clone(), "ada");
        let op = write_by(Some(&claimed));
        // Another member's device writes nothing for it, whatever grant it
        // claims to hold.
        let approved = Acting::handed_agent(key, "ada").handed_preset(WORKSPACE);
        assert!(received(&op, &person(&policy(None), "bob").granted(approved.clone())).is_err());
        // Nor does its person's device without the grant its certificate is.
        assert!(received(&op, &person(&policy(None), "ada")).is_err());
        // Its person's device does, holding that grant.
        assert_eq!(
            received(&op, &person(&policy(None), "ada").granted(approved)),
            Ok(Receipt::Admit)
        );
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

    phase a_sealed_write_is_decided_as_its_invoker_where_nobody_reads_it;
    {
        let (op, _) = sealed(&write_by(Some(&mount_agent())));
        assert!(matches!(
            received(&op, &person(&policy(Some(Answer::Deny)), "ada")),
            Err(ReceiptRefusal::NotGranted { .. })
        ));
        assert_eq!(
            received(&op, &person(&policy(Some(Answer::Allow)), "ada")),
            Ok(Receipt::Admit)
        );
        // The claims survive the wire as signed.
        let bytes = postcard::to_allocvec(&op).unwrap();
        let back: collab_types::op::SignedOp = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.claims, op.claims);
        assert!(op.claims.iter().all(|claim| claim.invoker.is_some()));
        // The writer's live decision and the one of a device deciding the
        // sealed op agree.
        for answered in [None, Some(Answer::Deny), Some(Answer::Ask), Some(Answer::Allow)] {
            let policy = policy(answered);
            let live = MemberAccess::person(policy.clone(), "ada")
                .as_acting(mount_agent().acting_for(Acting::person("ada")))
                .unwrap()
                .answer(&write_node(), ActionKind::Undoable, &Context::at_secs(1));
            let receipt = received(&op, &person(&policy, "ada"));
            assert_eq!(live == Answer::Allow, receipt == Ok(Receipt::Admit), "{answered:?}");
        }
    }

    phase a_device_refuses_a_sealed_change_that_says_other_than_its_claim;
    {
        let (op, opened) = sealed(&write_by(Some(&mount_agent())));
        assert_eq!(check_opened_mutation(&op, &opened), Ok(()));
        // Claimed as the person's own, though the agent made it: decided as
        // the person by whoever cannot read it, refused by whoever opens it.
        let mut as_person = op.clone();
        for claim in &mut as_person.claims {
            claim.invoker = None;
        }
        assert!(matches!(
            check_opened_mutation(&as_person, &opened),
            Err(ReceiptRefusal::Misclaimed(_))
        ));
        let mut unclaimed = op;
        unclaimed.claims.clear();
        assert_eq!(check_opened_mutation(&unclaimed, &opened), Err(ReceiptRefusal::Unclaimed));
    }
}

/// A window's direct edit of a record names who made it. A commit that names no one is refused
/// in the clear, whoever signed it, and so is a sealed claim that names no one; the same edit
/// naming its invoker is decided as them.
#[test]
fn a_native_edit_naming_no_one_is_refused_and_one_naming_its_maker_is_admitted() {
    let native = |message: String| {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "notes.md", "hello").unwrap();
        doc.commit_with(CommitOptions::default().with_message(message));
        let payload = collab_types::op::DocumentMutation::Incremental(doc.save_after(&[]))
            .encode()
            .unwrap();
        collab_types::op::SignedOp::new_unsigned(
            payload,
            collab_types::document_artifact_id(FILES, WORKSPACE, "tree"),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id(FILES, WORKSPACE, "tree"),
            collab_types::role::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        )
    };
    let agent = Acting::agent(AGENT, "ada");
    let asked = std::cell::RefCell::new(Vec::new());
    let allows = |invoker: Option<&Acting>, _: &TreePath, _: ActionKind| {
        asked.borrow_mut().push(invoker.cloned());
        true
    };
    let decide = |op: &collab_types::op::SignedOp| {
        decide_opened(
            (op, None),
            &tree(),
            (None, None, None),
            None,
            &Surfaces::default(),
            &allows,
            (None, Some(&agent)),
        )
    };
    let unnamed = native(FrameworkChange::Native.change_message());
    assert_eq!(decide(&unnamed), Err(ReceiptRefusal::NoInvoker));
    // Sealed, a receiver that has not opened it reads only its claims: one that names no one is
    // refused too.
    let (unnamed_sealed, _) = sealed(&unnamed);
    assert_eq!(decide(&unnamed_sealed), Err(ReceiptRefusal::NoInvoker));

    // The same edit naming who made it is decided as them.
    let named = native(ActionContext::whole_edit_by(&agent));
    assert_eq!(decide(&named), Ok(Receipt::Admit));
    let (named_sealed, _) = sealed(&named);
    assert_eq!(decide(&named_sealed), Ok(Receipt::Admit));
    assert_eq!(asked.into_inner(), vec![Some(agent); 2]);
}

/// A sealed write an app made by itself, signed by the owner's device: a receiver that has not
/// opened it decides it as the app its claims name, by the grant installing the app gave it, and
/// never as the owner whose device signed it. A write whose claims name no one is refused.
#[test]
fn a_sealed_write_by_an_app_is_decided_by_the_apps_grant_not_its_signer() {
    let app = Acting::by_app(FILES);
    let mut written = write_by(Some(&app));
    // An app that publishes no surface is decided as an edit of its record.
    written.surface = None;
    let (op, _) = sealed(&written);
    let decided = |policy: &WorkspacePolicy, op: &collab_types::op::SignedOp| {
        let signer = person(policy, "owner");
        let cx = Context::at_secs(1);
        let allows = |invoker: Option<&Acting>, node: &TreePath, kind: ActionKind| {
            ChangeAuthor::answer(&signer, invoker, node, kind, &cx) == Answer::Allow
        };
        decide_opened(
            (op, None),
            &tree(),
            (None, None, None),
            None,
            &Surfaces::default(),
            &allows,
            (None, Some(&signer.acting)),
        )
    };
    assert!(
        matches!(
            decided(&policy(None), &op),
            Err(ReceiptRefusal::NotGranted { .. })
        ),
        "the owner's device lends an app nothing by signing its write"
    );
    let mut installed = policy(None);
    installed
        .grants
        .push(tilo_action_policy::roles::app_records_grant(
            WORKSPACE, FILES, "owner",
        ));
    assert_eq!(decided(&installed, &op), Ok(Receipt::Admit));
    // The same write in the clear is decided the same way.
    assert!(decided(&policy(None), &written).is_err());
    assert_eq!(decided(&installed, &written), Ok(Receipt::Admit));
    let mut nameless = op;
    for claim in &mut nameless.claims {
        claim.invoker = None;
    }
    assert_eq!(
        decided(&installed, &nameless),
        Err(ReceiptRefusal::NoInvoker),
        "a write that names no one is no one's to admit"
    );
}
