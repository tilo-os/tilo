//! Outside effects wait in the workspace document for the device that owns them: approved twice, the
//! letter leaves once.

use crate::fixtures::Workspace;
use tilolang_runtime::store::Store as _;
use tilolang_runtime::{Actor, MemorySink, Resolution, Value};

use crate::fixtures::Replica;

const LETTERS: &str = include_str!("../../tilolang/tests/fixtures/letters/app.tl");
const LETTERS_IDS: &str = include_str!("../../tilolang/tests/fixtures/letters/app.ids");

/// A letters workspace with one draft and an agent's suggestion to send it, on `ada`'s replica.
fn suggested_workspace() -> (Replica, tilolang_runtime::ResourceRef) {
    let (ada, packages) = Replica::new("ada", Workspace::new(), &[(LETTERS, LETTERS_IDS)]);
    let meta = packages[0].meta.clone();
    let letters = ada.host.root_of("test.letters").unwrap();
    ada.commit();
    let new_draft = meta.actions.iter().find(|a| a.name == "new_draft").unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (new_draft.id.as_str(), vec![Value::resource(&letters)]),
        )
        .unwrap();
    ada.commit();
    let Value::List(drafts) = ada.host.runtime().read(&letters, "drafts").unwrap() else {
        panic!("the letters app lists its drafts");
    };
    let Value::Ref(draft) = &drafts[0] else {
        panic!("a draft is a reference");
    };
    let source = format!(
        "suggest letters.send(@letters.Draft:{})",
        draft.target.id_string()
    );
    let tilolang_runtime::AgentBlock::Invocation { record, resource } = ada
        .host
        .runtime()
        .run_agent_block(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::agent("task:1")),
            &source,
        )
        .unwrap()
    else {
        panic!("a send is an invocation");
    };
    assert_eq!(record.state, "asked");
    ada.commit();
    (ada, resource)
}

tilo_test_env::scenario! {
    fn what_the_outbox_holds_leaves_once;

    phase two_replicas_approving_the_same_suggestion_deliver_the_letter_once;
    {
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
                .resolve_invocation(&invocation, &Actor::from(tilolang_tilo::runtime::ids::MemberId::person(who)), Resolution::Approve { choice: None })
                .unwrap();
            replica.commit();
        }
        ada.sync_with(&grace);
        // Each carries out what it owns: the entry has one owner, whichever device's approval won.
        for replica in [&mut ada, &mut grace] {
            replica.host.settle_outbox().unwrap();
            replica.commit();
        }
        ada.sync_with(&grace);
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
        // Settled in the document: no device has anything left to carry out.
        assert_eq!(ada.host.store().outbox_pending().len(), 0);
        assert_eq!(grace.host.store().outbox_pending().len(), 0);
    }

    phase approving_one_suggestion_twice_on_one_replica_queues_it_once;
    {
        let (ada, invocation) = suggested_workspace();
        let sink = MemorySink::new();
        ada.host.runtime().set_sink(sink.clone());
        let approve = |ada: &Replica| {
            ada.host.runtime().resolve_invocation(
                &invocation,
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                Resolution::Approve { choice: None },
            )
        };
        approve(&ada).unwrap();
        // Decided already: a second approval is refused, and nothing is queued twice.
        assert!(approve(&ada).is_err());
        ada.commit();
        assert_eq!(ada.host.store().outbox_pending().len(), 1);
        // Marked as being handed over first, a change of its own; handed over once that is in.
        ada.host.settle_outbox().unwrap();
        ada.commit();
        assert!(sink.delivered().is_empty());
        ada.host.settle_outbox().unwrap();
        ada.commit();
        assert_eq!(sink.delivered().len(), 1);
    }

    phase a_letter_another_process_was_handing_over_is_outcome_unknown_and_never_sent_twice;
    {
        use tilolang_runtime::store::{ChangeNote, OutboxState, Store as _};
        let (ada, invocation) = suggested_workspace();
        let sink = MemorySink::new();
        ada.host.runtime().set_sink(sink.clone());
        ada.host
            .runtime()
            .resolve_invocation(&invocation, &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")), Resolution::Approve { choice: None })
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
        assert_eq!(ada.host.store().outbox_pending().len(), 0);
        let states = ada.host.store().outbox_states();
        let (_, state) = states.iter().find(|(doc, _)| *doc == entry.doc).unwrap();
        assert_eq!(state.state, "unknown");
    }

    phase a_commit_needs_what_the_action_it_carries_is;
    {
        let (ada, invocation) = suggested_workspace();
        // Creating the root asks nothing of the world outside the workspace.
        let (fresh, _) = Replica::new("ada", Workspace::new(), &[(LETTERS, LETTERS_IDS)]);
        fresh.host.root_of("test.letters").unwrap();
        assert!(
            fresh.host.store().outbox_pending().is_empty(),
            "creating the root queues no outside effect"
        );
        // Approving the send queues the letter: carrying it out reaches outside the document.
        ada.host
            .runtime()
            .resolve_invocation(&invocation, &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")), Resolution::Approve { choice: None })
            .unwrap();
        ada.commit();
        assert!(
            !ada.host.store().outbox_pending().is_empty(),
            "the send waits for a device to carry it outside"
        );
    }

}
