//! Outside effects wait in the room record for the device that owns them: approved twice, the
//! letter leaves once.

use automerge::AutoCommit;
use tilolang_runtime::{MemorySink, Principal, Resolution, Value};

use crate::fixtures::Replica;

const LETTERS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.tl");
const LETTERS_IDS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.ids");

/// A letters room with one draft and an agent's suggestion to send it, on `ada`'s replica.
fn suggested_room() -> (Replica, tilolang_runtime::ResourceRef) {
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
    let (mut ada, invocation) = suggested_room();
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
    let (mut ada, invocation) = suggested_room();
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
    let (mut ada, invocation) = suggested_room();
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
/// room, and what the commit is offered as.
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
    let (ada, invocation) = suggested_room();
    // Creating the root is a change of the record. The host offers its commit as one, which a
    // room whose grants let the app author the record allows.
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
