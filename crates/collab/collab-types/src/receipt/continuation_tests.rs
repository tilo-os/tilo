//! The keystrokes that continue a gesture are decided as the invoker of the
//! gesture's first entry, which their claim names; a receiver holding the
//! document checks it against that entry.

use automerge::AutoCommit;
use automerge::transaction::{CommitOptions, Transactable};
use tilo_action_policy::{Acting, Answer, Context, MemberAccess};
use tilo_actions::{ActionContext, ActionInvoker, ActionKey, Did, EncodedAction, VerbCall};

use super::super::*;
use super::invoker_tests::{
    FILES, TREE, Tree, WORKSPACE, mount_agent, person, policy, received, tree, write_node,
};

/// A keystroke of a gesture that writes into the tree, typed so far `typed`,
/// by `invoker` (the signing person when `None`).
fn keystroke(invoker: Option<&Acting>, typed: &str) -> ActionContext {
    let entry = ActionContext {
        what: Some(Did::Verb(VerbCall::new(
            ActionKey::new(FILES, TREE, "write"),
            [],
        ))),
        action: Some(EncodedAction {
            type_id: "files.write".into(),
            payload: format!("{typed:?}").into_bytes(),
        }),
        ..ActionContext::for_resource("tree", TREE, "Files")
            .in_gesture(Some(tilo_actions::ActionId(uuid::Uuid::from_u128(7))))
    };
    match invoker {
        Some(invoker) => entry.with_invoker(ActionInvoker::admitted(invoker)),
        None => entry,
    }
}

fn commit(doc: &mut AutoCommit, message: &str, typed: &str) {
    doc.put(automerge::ROOT, "notes.md", typed).unwrap();
    doc.commit_with(CommitOptions::default().with_message(message.to_owned()));
}

/// A document holding the gesture's first entry, by `invoker`.
fn begun(invoker: Option<&Acting>) -> AutoCommit {
    let mut doc = AutoCommit::new();
    commit(
        &mut doc,
        &keystroke(invoker, "a").change_message().unwrap(),
        "a",
    );
    doc
}

/// The next keystroke of the gesture `doc` holds, its claim written as
/// `claimed` would write it, as a cleartext op of that change alone.
fn continued(doc: &mut AutoCommit, claimed: Option<&Acting>) -> crate::op::SignedOp {
    let heads = doc.get_heads();
    let message = keystroke(claimed, "ab")
        .continuation_of(&keystroke(claimed, "a"))
        .unwrap();
    assert!(message.starts_with(tilo_actions::context::CONTINUATION_PREFIX));
    commit(doc, &message, "ab");
    let payload = crate::op::DocumentMutation::Incremental(doc.save_after(&heads))
        .encode()
        .unwrap();
    crate::op::SignedOp::new_unsigned(
        payload,
        crate::document_artifact_id(FILES, WORKSPACE, "tree"),
        crate::Ed25519PublicKey::from_bytes([7; 32]),
        crate::document_attestation_id(FILES, WORKSPACE, "tree"),
        crate::role::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        2,
    )
    .with_surface(Some([1; 32]))
}

/// How a receiver holding `holding` decides `op` signed by Ada's device.
fn held_by(
    op: &crate::op::SignedOp,
    holding: &AutoCommit,
    policy: &tilo_action_policy::WorkspacePolicy,
) -> Result<Receipt, ReceiptRefusal> {
    let signer = person(policy, "ada");
    let cx = Context::at_secs(1);
    let allows = |invoker: Option<&Acting>, node: &TreePath, kind: ActionKind| {
        ChangeAuthor::answer(&signer, invoker, node, kind, &cx) == Answer::Allow
    };
    let surfaces = Surfaces::from_source(std::sync::Arc::new(Tree));
    decide_op_pinned(
        op,
        &tree(),
        (None, None, None),
        Some(holding.committed_document()),
        &surfaces,
        &allows,
    )
}

tilo_test_env::scenario! {
    fn a_continuation_is_decided_as_its_gestures_invoker;

    phase an_agents_continuation_it_is_not_granted_is_refused_and_a_granted_one_taken;
    {
        let agent = mount_agent();
        let mut doc = begun(Some(&agent));
        let holding = doc.clone();
        let op = continued(&mut doc, Some(&agent));
        for receiver in [None, Some(&holding)] {
            let decide = |policy| match receiver {
                Some(holding) => held_by(&op, holding, &policy),
                None => received(&op, &person(&policy, "ada")),
            };
            assert!(matches!(
                decide(policy(Some(Answer::Deny))),
                Err(ReceiptRefusal::NotGranted { .. })
            ));
            assert_eq!(decide(policy(Some(Answer::Allow))), Ok(Receipt::Admit));
        }
        // The writer's live decision of the gesture and a receiver's of its
        // continuation agree.
        for answered in [None, Some(Answer::Deny), Some(Answer::Ask), Some(Answer::Allow)] {
            let policy = policy(answered);
            let live = MemberAccess::person(policy.clone(), "ada")
                .as_acting(agent.clone().acting_for(Acting::person("ada")))
                .unwrap()
                .answer(&write_node(), ActionKind::Undoable, &Context::at_secs(1));
            let receipt = held_by(&op, &holding, &policy);
            assert_eq!(live == Answer::Allow, receipt == Ok(Receipt::Admit), "{answered:?}");
        }
    }

    phase a_continuation_whose_first_entry_is_not_held_is_refused_by_a_holder;
    {
        let agent = mount_agent();
        let mut doc = begun(Some(&agent));
        let op = continued(&mut doc, Some(&agent));
        assert_eq!(
            held_by(&op, &AutoCommit::new(), &policy(Some(Answer::Allow))),
            Err(ReceiptRefusal::NoFirstEntry)
        );
    }

    phase a_continuation_claiming_another_invoker_than_its_first_entry_is_refused;
    {
        // The agent began the gesture; its continuation claims the person.
        let mut doc = begun(Some(&mount_agent()));
        let holding = doc.clone();
        let op = continued(&mut doc, None);
        assert!(matches!(
            held_by(&op, &holding, &policy(Some(Answer::Allow))),
            Err(ReceiptRefusal::Misclaimed(_))
        ));
    }

    phase a_segmented_sealed_op_with_a_mismatched_claim_is_refused_on_opening;
    {
        let agent = mount_agent();
        let mut doc = begun(Some(&agent));
        let heads = Vec::new();
        commit(
            &mut doc,
            &keystroke(Some(&agent), "ab").continuation_of(&keystroke(Some(&agent), "a")).unwrap(),
            "ab",
        );
        let bytes = doc.save_after(&heads);
        let payload = crate::op::DocumentMutation::Incremental(bytes.clone()).encode().unwrap();
        let claims = crate::claim::claims_of(&crate::claim::parse_changes(&bytes).unwrap(), |_| {
            vec![Vec::new()]
        })
        .unwrap();
        assert!(claims.iter().all(|claim| claim.invoker.as_ref() == Some(&agent)));
        let mut last = continued(&mut AutoCommit::new(), None).with_claims(claims);
        last.payload = vec![0; 16];
        last.encryption = Some(crate::op::EncryptionMetadata::content_key(
            1,
            crate::ContentRetention::of_frame(&payload),
        ));
        assert_eq!(check_reassembled(&last, &payload), Ok(()));
        let mut as_person = last;
        for claim in &mut as_person.claims {
            claim.invoker = None;
        }
        assert!(matches!(
            check_reassembled(&as_person, &payload),
            Err(ReceiptRefusal::Misclaimed(_))
        ));
    }
}
