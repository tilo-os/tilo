//! Outside effects wait in the workspace document for the device that owns them: approved on one
//! replica and synced to another, the letter leaves once.

use crate::fixtures::Workspace;
use tilolang_runtime::store::Store as _;
use tilolang_runtime::{Actor, InvocationEntry, MemorySink, Value};

use crate::fixtures::Replica;

const LETTERS: &str = include_str!("../../tilolang/tests/fixtures/letters/app.tl");
const LETTERS_IDS: &str = include_str!("../../tilolang/tests/fixtures/letters/app.ids");

/// A letters workspace with one draft and an agent's suggestion to send it, on `ada`'s replica,
/// and the record of the call that asks her.
fn suggested_workspace() -> (Replica, InvocationEntry) {
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
    let tilolang_runtime::AgentBlock::Invocation { record, .. } = ada
        .host
        .runtime()
        .run_agent_block(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::agent(
                "task:1", "ada",
            )),
            &source,
        )
        .unwrap()
    else {
        panic!("a send is an invocation");
    };
    assert_eq!(record.state, "asked");
    ada.commit();
    (ada, *record)
}

/// Ada approves the send on the system's confirm: the dispatch hands the call back on her
/// replica, for the agent that made it.
fn approve(ada: &Replica, record: &InvocationEntry) {
    ada.host
        .runtime()
        .invoke_approved_through(
            &record.author,
            None,
            (record.action.as_str(), record.arguments().unwrap()),
            None,
        )
        .unwrap();
}

tilo_test_env::scenario! {
    fn what_the_effects_hold_leaves_once;

    phase a_letter_approved_on_one_copy_leaves_once_from_its_owner;
    {
        let (mut ada, record) = suggested_workspace();
        let mut grace = ada.fork("grace", &[(LETTERS, LETTERS_IDS)]);
        let (sent_by_ada, sent_by_grace) = (MemorySink::new(), MemorySink::new());
        ada.host.runtime().set_sink(sent_by_ada.clone());
        grace.host.runtime().set_sink(sent_by_grace.clone());
        approve(&ada, &record);
        ada.commit();
        ada.sync_with(&grace);
        // Each carries out what it owns: the entry has one owner, the device that queued it.
        for replica in [&mut ada, &mut grace] {
            replica.host.settle_effects().unwrap();
            replica.commit();
        }
        ada.sync_with(&grace);
        for replica in [&mut ada, &mut grace] {
            replica.host.settle_effects().unwrap();
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
        assert_eq!(ada.host.store().pending_effects().len(), 0);
        assert_eq!(grace.host.store().pending_effects().len(), 0);
    }

    phase a_letter_another_process_was_handing_over_is_outcome_unknown_and_never_sent_twice;
    {
        use tilolang_runtime::store::{ChangeNote, EffectState, Store as _};
        let (ada, record) = suggested_workspace();
        let sink = MemorySink::new();
        ada.host.runtime().set_sink(sink.clone());
        approve(&ada, &record);
        ada.commit();
        // Another process of this device marked it as being handed over, and stopped.
        let entry = ada.host.runtime().pending_effects().remove(0);
        let mut store = ada.host.store().clone();
        let gone = EffectState::Sending {
            by: "a process that stopped".to_owned(),
        };
        store
            .mark_effect(&entry, &gone, &ChangeNote::default())
            .unwrap();
        ada.commit();
        ada.host.settle_effects().unwrap();
        ada.commit();
        ada.host.settle_effects().unwrap();
        ada.commit();
        assert!(sink.delivered().is_empty(), "never handed over twice");
        assert_eq!(ada.host.store().pending_effects().len(), 0);
        let states = ada.host.store().effect_states();
        let (_, state) = states.iter().find(|(doc, _)| *doc == entry.doc).unwrap();
        assert_eq!(state.state, "unknown");
    }

    phase a_commit_needs_what_the_action_it_carries_is;
    {
        let (ada, record) = suggested_workspace();
        // Creating the root asks nothing of the world outside the workspace.
        let (fresh, _) = Replica::new("ada", Workspace::new(), &[(LETTERS, LETTERS_IDS)]);
        fresh.host.root_of("test.letters").unwrap();
        assert!(
            fresh.host.store().pending_effects().is_empty(),
            "creating the root queues no outside effect"
        );
        // Approving the send queues the letter: carrying it out reaches outside the document.
        approve(&ada, &record);
        ada.commit();
        assert!(
            !ada.host.store().pending_effects().is_empty(),
            "the send waits for a device to carry it outside"
        );
    }

}

/// A settlement a pass could not write — the workspace refusing the app's own records, a sink
/// that is not there — is final, not something to try again on every pass: the effects waiting
/// as they stood are left until a person's next one queues, or a service reachable again asks
/// for them.
#[test]
fn an_effect_a_pass_could_not_settle_is_not_written_for_by_every_pass() {
    let (ada, record) = suggested_workspace();
    approve(&ada, &record);
    ada.commit();
    assert!(
        ada.host.effects_wait(),
        "the send waits to be carried outside"
    );
    // Settling writes the effect's mark; with no sink set the write fails, as a write the
    // workspace refuses does.
    assert!(
        ada.host.settle_effects().is_err(),
        "the mark is not written, so the effect waits"
    );
    assert!(
        !ada.host.effects_wait(),
        "the same effects are not written for again by the next pass"
    );
    ada.host
        .settle_effects()
        .expect("a pass with a refused effect writes nothing and says nothing");
    // A service reachable again asks for what waited on it once more.
    ada.host.runtime().retry_waiting();
    assert!(ada.host.effects_wait(), "the retry asks for them again");
    assert!(
        ada.host.settle_effects().is_err(),
        "the effects are tried once more"
    );
}
