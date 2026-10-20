//! Outside effects wait in the room record for the device that owns them: approved twice, the
//! letter leaves once.

use automerge::AutoCommit;
use tilolang_runtime::{MemorySink, Principal, Resolution, Value};

use crate::fixtures::Replica;

const LETTERS: &str = include_str!("../../../../apps/tl/letters/app.tl");
const LETTERS_IDS: &str = include_str!("../../../../apps/tl/letters/app.ids");

/// A letters room with one draft and an agent's suggestion to send it, on `ada`'s replica.
fn suggested_room() -> (Replica, tilolang_runtime::ResourceRef) {
    let (mut ada, packages) = Replica::new("ada", AutoCommit::new(), &[(LETTERS, LETTERS_IDS)]);
    let meta = packages[0].meta.clone();
    let desk = ada.host.root_of("so.tilo.letters").unwrap();
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
    ada.host.settle_outbox().unwrap();
    ada.commit();
    assert_eq!(sink.delivered().len(), 1);
}
