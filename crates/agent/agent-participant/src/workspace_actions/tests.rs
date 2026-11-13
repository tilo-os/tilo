use super::{GaveUp, Needs, sealed_for};
use std::collections::{BTreeSet, HashSet};
use tilo_peer_state::document_frame::DocumentRoute;

const WORKSPACE: &str = "6f1c1b62-4f5e-4b8a-9b52-1f8a3f0f3a11";
const OTHER_WORKSPACE: &str = "0b7f1d2e-5a6c-4d3b-8e9f-a0b1c2d3e4f5";

fn route(workspace: &str, app: &str, record: &str) -> DocumentRoute {
    DocumentRoute {
        app_id: app.into(),
        cluster_id: workspace.into(),
        record: record.into(),
    }
}

/// A failure the caller may ask again is not the invocation's answer; one
/// that is, is, and the same id keeps saying it.
#[test]
fn a_retryable_failure_leaves_the_id_free_to_run_again() {
    let failure = |retryable| super::ActionFailure {
        code: "commit_failed".into(),
        message: "the host did not save the change: the workspace refused the change: \
                  workspace membership changed"
            .into(),
        details: None,
        retryable,
    };
    assert!(
        !super::settles(&failure(true)),
        "asking again runs it again, over the session the workspace holds now"
    );
    assert!(
        super::settles(&failure(false)),
        "a failure of its own is its answer"
    );
}

/// A full ledger makes room by forgetting what settled, never an invocation
/// still to run or running: forgotten, it would never run, or leave its
/// caller no answer, and a retry under its id would run it twice.
#[test]
fn a_full_ledger_forgets_only_settled_invocations() {
    use tilo_actions::remote::{InvokeRequest, RemoteActionTarget};
    use tilo_actions::{InvocationId, InvocationUpdate};
    let caller = tilo_actions::remote::Caller::local_person("ada");
    let request = || InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "record:so.tilo.notes".into(),
            address: tilo_actions::door::ActionAddress::new("notes.page", "append"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let mut ledger = super::Ledger::default();
    let pending = request().invocation;
    let first = InvokeRequest {
        invocation: pending,
        ..request()
    };
    assert!(ledger.insert(&caller, first));
    for _ in 1..super::ledger::REMEMBERED {
        assert!(ledger.insert(&caller, request()));
    }
    assert!(
        !ledger.insert(&caller, request()),
        "with every invocation unsettled, a new one is refused, not one of them dropped"
    );
    assert!(
        ledger.entries.contains_key(&pending),
        "the pending write is kept"
    );

    let settled = ledger.order[1];
    if let Some(entry) = ledger.entries.get_mut(&settled) {
        entry.update = InvocationUpdate::Succeeded {
            invocation: settled,
            output: tilo_actions::ActionOutput::default(),
        };
    }
    assert!(
        ledger.insert(&caller, request()),
        "a settled one makes room"
    );
    assert!(
        !ledger.entries.contains_key(&settled),
        "the settled one is forgotten"
    );
    assert!(
        ledger.entries.contains_key(&pending),
        "the oldest, still pending, is kept"
    );
}

#[test]
fn a_request_waits_only_for_the_keys_of_what_it_reads() {
    let workspace = tilo_relay_protocol::WorkspaceId::parse(WORKSPACE).unwrap();
    let b = route(WORKSPACE, "so.tilo.sheets", "b");
    let elsewhere = route(OTHER_WORKSPACE, "so.tilo.sheets", "a");
    // `a` opened; `b` here and the record elsewhere wait for their keys.
    let routes = BTreeSet::from([b.clone(), elsewhere]);
    let act = |record| Needs::Act {
        app_id: "so.tilo.sheets",
        record,
    };
    let read = |record| Needs::Read {
        app_id: "so.tilo.sheets",
        record,
    };
    let fresh = GaveUp::default();

    assert!(
        sealed_for(&routes, workspace, act("a"), &fresh).is_empty(),
        "another record's missing keys never hold back this one"
    );
    assert!(sealed_for(&routes, workspace, read("a"), &fresh).is_empty());
    assert_eq!(
        sealed_for(&routes, workspace, act("b"), &fresh),
        std::slice::from_ref(&b)
    );
    assert_eq!(
        sealed_for(&routes, workspace, read("b"), &fresh),
        std::slice::from_ref(&b)
    );
    assert!(
        sealed_for(
            &routes,
            workspace,
            Needs::Act {
                app_id: "so.tilo.slides",
                record: "b"
            },
            &fresh
        )
        .is_empty(),
        "a record is named by its app and id together"
    );
    assert_eq!(
        sealed_for(&routes, workspace, Needs::Workspace, &fresh),
        std::slice::from_ref(&b),
        "the first listing waits for this workspace's sealed records only"
    );
    assert!(sealed_for(&routes, workspace, Needs::Joined, &fresh).is_empty());

    let waited = GaveUp {
        listed: true,
        records: HashSet::from([b.clone()]),
    };
    assert!(
        sealed_for(&routes, workspace, Needs::Workspace, &waited).is_empty(),
        "later listings wait for nothing"
    );
    assert!(
        sealed_for(&routes, workspace, read("b"), &waited).is_empty(),
        "a listing waits for the same missing keys once"
    );
    assert_eq!(
        sealed_for(&routes, workspace, act("b"), &waited),
        [b],
        "acting on a record always waits for its keys"
    );
}

/// A write answered pending ends as its save does: done once the host holds
/// it, failed with the host's reason when it refuses it, and failed when the
/// session ends before the host held it, and failed (not to be asked again
/// blindly: it may still be saved) when the host says nothing in time, so
/// whoever follows it always hears how it went. Until then it cannot be
/// cancelled: it ran.
#[test]
fn a_pending_write_settles_as_its_save_does() {
    use std::sync::{Arc, Mutex, mpsc};
    use tilo_actions::remote::{InvokeRequest, RemoteActionTarget};
    use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
    let caller = tilo_actions::remote::Caller::local_person("ada");
    let request = || InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "record:so.tilo.notes".into(),
            address: tilo_actions::door::ActionAddress::new("notes.page", "append"),
        },
        argument: tilo_actions::remote::InvokeArgument::default(),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    };
    let ledger = Arc::new(Mutex::new(super::Ledger::default()));
    // Each write: its save's outcome, sent, dropped (`None`) or never said
    // (`Some(None)` holds the answer back), and how it settled.
    let mut kept = Vec::new();
    let mut settle = |outcome: Option<Option<Result<(), String>>>| {
        let asked = request();
        let invocation = asked.invocation;
        {
            let mut held = ledger.lock().unwrap();
            assert!(held.insert(&caller, asked));
            held.entries.get_mut(&invocation).unwrap().saving = true;
        }
        let (answer, saved) = tilo_session_client::answer();
        let (settled, heard) = mpsc::channel();
        let shared = Arc::clone(&ledger);
        super::publish::Unsaved::waiting(saved).settle_within(
            std::time::Duration::from_millis(200),
            move |saved| {
                super::publish::settle_saved(&shared, invocation, ActionOutput::default(), saved);
                let _ = settled.send(());
            },
        );
        match outcome {
            Some(Some(outcome)) => {
                let _ = answer.send(outcome);
            }
            Some(None) => kept.push(answer),
            None => drop(answer),
        }
        heard
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the save's outcome settles the write");
        let (saving, update) = ledger
            .lock()
            .unwrap()
            .entries
            .get(&invocation)
            .map(|entry| (entry.saving, entry.update.clone()))
            .unwrap();
        assert!(!saving, "a settled write is saving no more");
        update
    };

    assert!(
        matches!(
            settle(Some(Some(Ok(())))),
            InvocationUpdate::Succeeded { .. }
        ),
        "a write the host holds is done"
    );
    let refused = settle(Some(Some(Err("workspace membership changed".into()))));
    assert!(
        matches!(&refused, InvocationUpdate::Failed { error, .. }
            if error.message.contains("workspace membership changed") && error.retryable),
        "a write the host refuses fails with its reason: {refused:?}"
    );
    let ended = settle(None);
    assert!(
        matches!(&ended, InvocationUpdate::Failed { error, .. }
            if error.message.contains("session ended")),
        "a write whose session ended unsaved fails: {ended:?}"
    );
    let silent = settle(Some(None));
    assert!(
        matches!(&silent, InvocationUpdate::Failed { error, .. }
            if !error.retryable && error.message.contains("read the record")),
        "a write the host never confirms fails in time, not to be made twice: {silent:?}"
    );
    assert_eq!(
        kept.len(),
        1,
        "the silent save's answer stayed open throughout"
    );
}
