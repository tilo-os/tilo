//! Outside effects wait in the workspace record for the device that owns them: approved twice, the
//! letter leaves once.

use automerge::AutoCommit;
use tilolang_runtime::{InvokeOutcome, MemorySink, Principal, Resolution, Value};

use crate::fixtures::Replica;
use tilolang_tilo::meta::MetaExt as _;

const LETTERS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.tl");
const LETTERS_IDS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.ids");

/// A letters workspace with one draft and an agent's suggestion to send it, on `ada`'s replica.
fn suggested_workspace() -> (Replica, tilolang_runtime::ResourceRef) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(LETTERS, LETTERS_IDS)]);
    let meta = packages[0].meta.clone();
    let desk = ada.host.root_of("test.letters").unwrap();
    ada.commit();
    let new_draft = meta.actions.iter().find(|a| a.name == "new_draft").unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &Principal::new("ada"),
            (new_draft.id.as_str(), vec![Value::resource(&desk)]),
        )
        .unwrap();
    ada.commit();
    let Value::List(drafts) = ada.host.runtime().read(&desk, "drafts").unwrap() else {
        panic!("the desk lists its drafts");
    };
    let Value::Ref(draft) = &drafts[0] else {
        panic!("a draft is a reference");
    };
    let source = format!("letters.send(@letters.Draft:{})", draft.target.id_string());
    let tilolang_runtime::AgentBlock::Invocation { record, resource } = ada
        .host
        .runtime()
        .run_agent_block(&Principal::new("agent-task:1"), &source)
        .unwrap()
    else {
        panic!("a send is an invocation");
    };
    assert_eq!(record.state, "suggested");
    ada.commit();
    (ada, resource)
}

#[test]
fn two_replicas_approving_the_same_suggestion_deliver_the_letter_once() {
    let (mut ada, invocation) = suggested_workspace();
    let mut grace = ada.fork("grace", &[(LETTERS, LETTERS_IDS)]);
    let (sent_by_ada, sent_by_grace) = (MemorySink::new(), MemorySink::new());
    ada.host.runtime().set_sink(sent_by_ada.clone());
    grace.host.runtime().set_sink(sent_by_grace.clone());
    // Both approve while apart.
    for (who, replica) in [("ada", &mut ada), ("grace", &mut grace)] {
        replica
            .host
            .runtime()
            .resolve_invocation(&invocation, &Principal::new(who), Resolution::Approve)
            .unwrap();
        replica.commit();
    }
    ada.sync_with(&mut grace);
    // Each carries out what it owns: the entry has one owner, whichever device's approval won.
    for replica in [&mut ada, &mut grace] {
        replica.host.settle_outbox().unwrap();
        replica.commit();
    }
    ada.sync_with(&mut grace);
    for replica in [&mut ada, &mut grace] {
        replica.host.settle_outbox().unwrap();
        replica.commit();
    }
    let delivered = sent_by_ada.delivered().len() + sent_by_grace.delivered().len();
    assert_eq!(
        delivered,
        1,
        "{:?} {:?}",
        sent_by_ada.delivered(),
        sent_by_grace.delivered()
    );
    // Settled in the record: no device has anything left to carry out.
    assert_eq!(ada.host.store().outbox_pending_count(), 0);
    assert_eq!(grace.host.store().outbox_pending_count(), 0);
}

#[test]
fn approving_one_suggestion_twice_on_one_replica_queues_it_once() {
    let (mut ada, invocation) = suggested_workspace();
    let sink = MemorySink::new();
    ada.host.runtime().set_sink(sink.clone());
    let approve = |ada: &Replica| {
        ada.host.runtime().resolve_invocation(
            &invocation,
            &Principal::new("ada"),
            Resolution::Approve,
        )
    };
    approve(&ada).unwrap();
    // Decided already: a second approval is refused, and nothing is queued twice.
    assert!(approve(&ada).is_err());
    ada.commit();
    assert_eq!(ada.host.store().outbox_pending_count(), 1);
    // Marked as being handed over first, a change of its own; handed over once that is in.
    ada.host.settle_outbox().unwrap();
    ada.commit();
    assert!(sink.delivered().is_empty());
    ada.host.settle_outbox().unwrap();
    ada.commit();
    assert_eq!(sink.delivered().len(), 1);
}

#[test]
fn a_letter_another_process_was_handing_over_is_outcome_unknown_and_never_sent_twice() {
    use tilolang_runtime::store::{ChangeNote, OutboxState, Store as _};
    let (mut ada, invocation) = suggested_workspace();
    let sink = MemorySink::new();
    ada.host.runtime().set_sink(sink.clone());
    ada.host
        .runtime()
        .resolve_invocation(&invocation, &Principal::new("ada"), Resolution::Approve)
        .unwrap();
    ada.commit();
    // Another process of this device marked it as being handed over, and stopped.
    let entry = ada.host.runtime().pending_outbox().remove(0);
    let mut store = ada.host.store().clone();
    let gone = OutboxState::Sending {
        by: "a process that stopped".to_owned(),
    };
    store
        .outbox_mark(&entry, &gone, &ChangeNote::default())
        .unwrap();
    ada.commit();
    ada.host.settle_outbox().unwrap();
    ada.commit();
    ada.host.settle_outbox().unwrap();
    ada.commit();
    assert!(sink.delivered().is_empty(), "never handed over twice");
    assert_eq!(ada.host.store().outbox_pending_count(), 0);
    let states = ada.host.store().outbox_states();
    let (_, state) = states.iter().find(|(doc, _)| *doc == entry.doc).unwrap();
    assert_eq!(state.state, "unknown");
}

/// What committing the action queued under `ticket` through the host's resource needs of a
/// workspace, and what the commit is offered as.
fn commit_need(
    replica: &Replica,
    ticket: u64,
) -> (tilo_actions::ActionKind, tilo_actions::ActionKind) {
    let committer: std::sync::Arc<dyn tilolang_tilo::publish::Committer> =
        std::sync::Arc::new(replica.host.handle());
    let host = tilolang_tilo::publish::host_resource(&committer);
    let commit = host
        .actions
        .iter()
        .find(|action| action.id == tilolang_tilo::publish::COMMIT)
        .unwrap();
    let arguments = serde_json::json!({ "ticket": ticket });
    (
        commit.contract.kind,
        commit.need_for(arguments.as_object(), None).kind,
    )
}

#[test]
fn a_commit_needs_what_the_action_it_carries_is() {
    use tilo_actions::ActionKind::{Outside, Undoable};
    let (ada, invocation) = suggested_workspace();
    // Creating the root is a change of the record. The host offers its commit as one, which a
    // workspace whose grants let the app author the record allows.
    let (fresh, _) = Replica::new("ada", AutoCommit::new(), &[(LETTERS, LETTERS_IDS)]);
    fresh.host.root_of("test.letters").unwrap();
    let created = fresh.host.take_committed();
    assert_eq!(created.len(), 1);
    assert_eq!(commit_need(&fresh, created[0].ticket), (Undoable, Undoable));
    // Approving the send queues the letter: committing that reaches outside the record.
    ada.host
        .runtime()
        .resolve_invocation(&invocation, &Principal::new("ada"), Resolution::Approve)
        .unwrap();
    let approved = ada.host.take_committed();
    assert!(
        approved
            .iter()
            .any(|committed| commit_need(&ada, committed.ticket) == (Undoable, Outside)),
        "{approved:?}"
    );
}

/// A relay app: a post that goes out at once, and the ask for who records it.
const RELAY: &str = r#"app test.relay "Relay" (icon: .mail, category: .office)

type Draft {
    to: Text
    sent: Bool = false
    sealed: Bool = false
}

type Desk {
    drafts: Collection<Draft>
}

root Desk

default view desk for Desk

view desk(desk: Desk) {
    std.list(desk.drafts) { (d, at) ->
        ui.ListItem(item: d, presence: at) {
            text(d.to)
        }
    }
}

/// Starts a letter.
action new_draft(desk: Desk) -> Draft (icon: .plus) {
    desk.drafts.insert(Draft(to: "bo@example.org"))
}

/// Posts the letter, then asks who records it: what went out is out while a person decides.
action post_and_record(d: Draft) {
    d.sent = true
    outside("post", (d.to,))
    record(d)
}

/// Seals the posted letter in the archive.
action record(d: Draft) {
    outside("smtp.send", (d.to,))
    d.sealed = true
}
"#;
const RELAY_APP: &str = "test.relay";
const WORKSPACE: &str = "workspace";

/// `who`'s standing in the relay workspace: an editor of it, allowed to post, so the action runs
/// up to the effect it queues before the ask about recording the letter.
fn relay_standing(meta: &tilolang_runtime::meta::Meta, who: &str) -> tilo_action_policy::Standing {
    use tilo_action_policy::{
        Answer, Grant, KindSet, Role, Standing, Subject, TreePath, WorkspacePolicy,
    };
    let mut policy = WorkspacePolicy::new(WORKSPACE, "sol");
    for member in ["ada", "grace"] {
        policy.members.insert(member.to_owned(), Role::Editor);
    }
    let post = meta
        .actions
        .iter()
        .find(|action| action.name == "post_and_record")
        .expect("the relay posts");
    let node = TreePath::workspace(WORKSPACE)
        .app(RELAY_APP)
        .kind(meta.kind_of("Draft"))
        .action(&post.id);
    let owner = Subject::person("sol");
    policy.grants.extend([
        Grant::new(
            "posts",
            Subject::person(who),
            node.clone(),
            KindSet::OUTSIDE,
            Answer::Allow,
            owner.clone(),
        ),
        // The app's own code bounds it too, and its code asks before reaching outside.
        Grant::new(
            "app-posts",
            Subject::app(RELAY_APP),
            node,
            KindSet::OUTSIDE,
            Answer::Allow,
            owner,
        ),
    ]);
    Standing::person(policy, who)
}

/// Two replicas of a relay workspace: ada posted a letter, and recording it waits on a person.
fn relay_workspace() -> (Replica, Replica, tilolang_runtime::ResourceRef) {
    // The ids the app compiles to: every replica loads the same lock, so they agree on them.
    let (_, probe) = Replica::new("probe", AutoCommit::new(), &[(RELAY, "")]);
    let meta = probe[0].meta.clone();
    let ids = probe[0].ids.clone();
    let packages = [(RELAY, ids.as_str())];
    let (mut ada, _) = Replica::standing(
        "ada",
        AutoCommit::new(),
        &packages,
        Some(relay_standing(&meta, "ada")),
    );
    let desk = ada.host.root_of(RELAY_APP).unwrap();
    ada.commit();
    let new_draft = meta
        .actions
        .iter()
        .find(|action| action.name == "new_draft")
        .unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &Principal::new("ada"),
            (new_draft.id.as_str(), vec![Value::resource(&desk)]),
        )
        .unwrap();
    ada.commit();
    let Value::List(drafts) = ada.host.runtime().read(&desk, "drafts").unwrap() else {
        panic!("the desk lists its drafts");
    };
    let Value::Ref(draft) = &drafts[0] else {
        panic!("a draft is a reference");
    };
    let draft = tilolang_runtime::ResourceRef::clone(&draft.target);
    let post = meta
        .actions
        .iter()
        .find(|action| action.name == "post_and_record")
        .unwrap();
    let args = vec![Value::resource(&draft)];
    let InvokeOutcome::Suggested(suggestion) = ada
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (post.id.as_str(), args.clone()))
        .unwrap()
    else {
        panic!("recording the post asks first");
    };
    let invocation = ada
        .host
        .runtime()
        .record_suggestion(&Principal::new("ada"), post.id.as_str(), args, &suggestion)
        .unwrap();
    ada.commit();
    let grace = Replica::standing(
        "grace",
        ada.doc.fork(),
        &packages,
        Some(relay_standing(&meta, "grace")),
    )
    .0;
    (ada, grace, invocation)
}

#[test]
fn a_post_that_went_out_before_the_ask_leaves_once_when_both_replicas_run_it() {
    let (mut ada, mut grace, invocation) = relay_workspace();
    let (by_ada, by_grace) = (MemorySink::new(), MemorySink::new());
    ada.host.runtime().set_sink(by_ada.clone());
    grace.host.runtime().set_sink(by_grace.clone());
    // Ada answers: the run goes on from its journal, and the post it already queued stays one.
    assert_eq!(
        ada.host
            .runtime()
            .resolve_invocation(&invocation, &Principal::new("ada"), Resolution::Approve)
            .unwrap(),
        "done"
    );
    ada.commit();
    // Grace holds no journal of the run: she runs the whole action again, posting once more,
    // and reaches the same ask. Answering that goes on from the journal she made.
    assert_eq!(
        grace
            .host
            .runtime()
            .resolve_invocation(&invocation, &Principal::new("grace"), Resolution::Approve)
            .unwrap(),
        "suggested"
    );
    grace.commit();
    assert_eq!(
        grace
            .host
            .runtime()
            .resolve_invocation(&invocation, &Principal::new("grace"), Resolution::Approve)
            .unwrap(),
        "done"
    );
    grace.commit();
    ada.sync_with(&mut grace);
    for replica in [&mut ada, &mut grace] {
        replica.host.settle_outbox().unwrap();
        replica.commit();
    }
    ada.sync_with(&mut grace);
    for replica in [&mut ada, &mut grace] {
        replica.host.settle_outbox().unwrap();
        replica.commit();
    }
    let mut posted = Vec::new();
    let mut sealed = Vec::new();
    for (capability, payload) in by_ada.delivered().into_iter().chain(by_grace.delivered()) {
        match capability.as_str() {
            "post" => posted.push(payload),
            "smtp.send" => sealed.push(payload),
            other => panic!("nothing else leaves: {other}"),
        }
    }
    assert_eq!(posted.len(), 1, "the post leaves once: {posted:?}");
    assert_eq!(sealed.len(), 1, "the letter is sealed once: {sealed:?}");
    assert_eq!(ada.host.store().outbox_pending_count(), 0);
    assert_eq!(grace.host.store().outbox_pending_count(), 0);
}
