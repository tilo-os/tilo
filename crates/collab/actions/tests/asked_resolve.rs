//! A person's answer to an asked invocation record, as the store that keeps the record admits
//! it: only from the person it asks, only while it is asked, once, with a choice among its own;
//! and what reaches the dispatch that keeps the call. The dispatch here notes what it is
//! handed, as the host's own dispatches act on it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_action_policy::{ActionKind, Answer, Grant, Grantee, KindSet};
use tilo_actions::InvocationId;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::asked::{
    AskedBy, AskedDispatch, AskedFacts, AskedGrants, AskedRecords, AskedStore, Named, Offer,
    RecordRef, Resolve,
};
use tilo_actions::remote::{
    ActionCallError, Caller, Confirmation, InvokeArgument, InvokeRequest, Pending,
    RemoteActionTarget,
};

const ADA: [u8; 32] = [1; 32];
const BO: [u8; 32] = [2; 32];
const WORKSPACE: &str = "acme";

/// What the dispatch was handed.
#[derive(Debug, PartialEq)]
enum Handed {
    Readmit(Caller, InvokeRequest),
    Declined(Caller, InvocationId),
}

#[derive(Default)]
struct Noted(Mutex<Vec<Handed>>);

impl AskedDispatch for Noted {
    fn readmit(&self, caller: &Caller, request: InvokeRequest) -> Result<(), ActionCallError> {
        self.0
            .lock()
            .unwrap()
            .push(Handed::Readmit(caller.clone(), request));
        Ok(())
    }

    fn declined(&self, caller: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        self.0
            .lock()
            .unwrap()
            .push(Handed::Declined(caller.clone(), invocation));
        Ok(())
    }
}

/// Writes every grant it is handed, or refuses each with `refusal`.
struct Grants {
    written: Mutex<Vec<(String, String, Grant)>>,
    refusal: Option<String>,
}

impl AskedGrants for Grants {
    fn grant(&self, workspace: &str, person: &str, grant: Grant) -> Pending<Result<(), String>> {
        if let Some(refusal) = &self.refusal {
            return Pending::ready(Err(refusal.clone()));
        }
        self.written
            .lock()
            .unwrap()
            .push((workspace.to_owned(), person.to_owned(), grant));
        Pending::ready(Ok(()))
    }
}

struct Host {
    store: Arc<AskedStore>,
    dispatch: Arc<Noted>,
    records: Arc<dyn AskedRecords>,
}

fn host() -> Host {
    let store = Arc::new(AskedStore::default());
    let dispatch = Arc::new(Noted::default());
    let weak: std::sync::Weak<dyn AskedDispatch> =
        Arc::downgrade(&(Arc::clone(&dispatch) as Arc<dyn AskedDispatch>));
    let records = store.for_dispatch(weak);
    Host {
        store,
        dispatch,
        records,
    }
}

/// Ada's agent Scout, on her server.
fn scout() -> Caller {
    Caller::delegated_agent("scout", ADA, [9; 32]).in_workspace(WORKSPACE)
}

fn ada() -> Caller {
    Caller::authenticated(ADA, [3; 32]).in_workspace(WORKSPACE)
}

fn set_alarm() -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("at".into(), "07:00".into());
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: "record:clocks:alarms".into(),
            address: ActionAddress::new("clocks.alarms", "set-alarm"),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn facts() -> AskedFacts {
    AskedFacts {
        verb: "set".into(),
        plural: Some("alarms".into()),
        kind: "clocks.alarms".into(),
        effect: Some(ActionKind::Undoable),
        ..AskedFacts::by(AskedBy::Agent(Named {
            id: "scout".into(),
            name: "Scout".into(),
        }))
    }
    .with_fact("At", "07:00")
}

fn answered(pending: Pending<Result<(), ActionCallError>>) -> Result<(), ActionCallError> {
    pending
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("the answer is admitted or refused at once")
}

fn resolve(
    host: &Host,
    at: &RecordRef,
    by: &Caller,
    resolution: Offer,
    choice: Option<usize>,
) -> Result<(), ActionCallError> {
    answered(host.store.resolve(at, by, &Resolve { resolution, choice }))
}

fn refusal(result: Result<(), ActionCallError>) -> String {
    match result {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// An agent's call asks Ada; she sees it in her list, approves it, and the dispatch is handed
/// the same call for the agent again, carrying her approve of that record. A second approve,
/// or a decline after it, finds nothing to answer, and the record leaves her list.
#[test]
fn an_approve_hands_the_call_back_once() {
    let host = host();
    let call = set_alarm();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts())
        .unwrap();
    assert_eq!(host.store.record(&at.record), Some(at.clone()));
    let listed = host.records.asked_of(&ADA);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].record, at);
    assert_eq!(listed[0].action, "clocks#set-alarm");
    assert_eq!(
        listed[0].offers,
        vec![Offer::Approve, Offer::ApproveAndGrant, Offer::Decline]
    );
    assert!(host.records.asked_of(&BO).is_empty(), "Bo is asked nothing");

    resolve(&host, &at, &ada(), Offer::Approve, None).unwrap();
    let handed = host
        .dispatch
        .0
        .lock()
        .unwrap()
        .drain(..)
        .collect::<Vec<_>>();
    let [Handed::Readmit(caller, request)] = handed.as_slice() else {
        panic!("the dispatch is handed the call once: {handed:?}");
    };
    assert_eq!(
        *caller,
        scout(),
        "it runs for the agent that asked, never as Ada"
    );
    assert_eq!(request.invocation, call.invocation);
    assert!(tilo_actions::remote::asked::same_call(request, &call));
    assert_eq!(
        request
            .approval()
            .map(|approval| (&approval.record, approval.choice)),
        Some((&at, None))
    );

    assert!(
        host.records.asked_of(&ADA).is_empty(),
        "nothing waits any more"
    );
    for again in [Offer::Approve, Offer::Decline] {
        assert_eq!(
            refusal(resolve(&host, &at, &ada(), again, None)),
            tilo_actions::remote::asked::ANSWERED
        );
    }
    assert!(
        host.dispatch.0.lock().unwrap().is_empty(),
        "nothing ran twice"
    );
}

/// A decline ends the call: the dispatch is told for the agent that asked, and nothing runs.
#[test]
fn a_decline_ends_the_call_for_its_caller() {
    let host = host();
    let call = set_alarm();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts())
        .unwrap();
    resolve(&host, &at, &ada(), Offer::Decline, None).unwrap();
    assert_eq!(
        *host.dispatch.0.lock().unwrap(),
        vec![Handed::Declined(scout(), call.invocation)]
    );
    assert_eq!(
        refusal(resolve(&host, &at, &ada(), Offer::Approve, None)),
        tilo_actions::remote::asked::ANSWERED
    );
}

/// Only Ada answers what asks Ada: not Bo, not her own agent acting for her, and not the
/// device her agent's connection came in on.
#[test]
fn nobody_but_the_person_asked_answers() {
    let host = host();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &set_alarm(), &facts())
        .unwrap();
    let bo = Caller::authenticated(BO, [4; 32]).in_workspace(WORKSPACE);
    let her_agent = Caller::delegated_agent("helper", ADA, [3; 32]).in_workspace(WORKSPACE);
    for (who, by) in [("Bo", bo), ("her agent", her_agent), ("the asker", scout())] {
        assert_eq!(
            refusal(resolve(&host, &at, &by, Offer::Approve, None)),
            tilo_actions::remote::asked::NOT_YOURS,
            "{who} cannot answer"
        );
    }
    assert!(host.dispatch.0.lock().unwrap().is_empty());
    assert_eq!(
        host.records.asked_of(&ADA).len(),
        1,
        "it still waits for Ada"
    );
}

/// A question with choices is answered with one of them, and only one it offers.
#[test]
fn a_choice_is_one_the_question_offers() {
    let host = host();
    let call = set_alarm();
    let facts = AskedFacts {
        words: "Which milk?".into(),
        choices: vec!["Oat".into(), "Whole".into()],
        ..AskedFacts::by(AskedBy::Agent(Named {
            id: "scout".into(),
            name: "Scout".into(),
        }))
    };
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts)
        .unwrap();
    assert_eq!(
        host.records.asked_of(&ADA)[0].offers,
        vec![Offer::Approve, Offer::Decline],
        "a question with choices grants nothing"
    );
    refusal(resolve(&host, &at, &ada(), Offer::Approve, Some(2)));
    refusal(resolve(&host, &at, &ada(), Offer::Approve, None));
    refusal(resolve(&host, &at, &ada(), Offer::ApproveAndGrant, Some(0)));
    assert!(host.dispatch.0.lock().unwrap().is_empty());
    resolve(&host, &at, &ada(), Offer::Approve, Some(1)).unwrap();
    let handed = host
        .dispatch
        .0
        .lock()
        .unwrap()
        .drain(..)
        .collect::<Vec<_>>();
    let [Handed::Readmit(_, request)] = handed.as_slice() else {
        panic!("handed once: {handed:?}");
    };
    assert_eq!(
        request.approval().and_then(|approval| approval.choice),
        Some(1)
    );
}

/// Approve and grant writes an ordinary grant first: the agent may set alarms in that
/// workspace from now on, at the action's own kind, until revoked, given by Ada. A grant the
/// host refuses leaves the question asked, nothing run.
#[test]
fn approve_and_grant_writes_the_grant_then_approves() {
    let refusing = host();
    refusing.store.grants_through(Arc::new(Grants {
        written: Mutex::default(),
        refusal: Some("Ada may not change alarms herself".into()),
    }));
    let at = refusing
        .records
        .write_asked(WORKSPACE, &scout(), &set_alarm(), &facts())
        .unwrap();
    let why = refusal(resolve(
        &refusing,
        &at,
        &ada(),
        Offer::ApproveAndGrant,
        None,
    ));
    assert!(why.contains("Ada may not change alarms herself"), "{why}");
    assert!(refusing.dispatch.0.lock().unwrap().is_empty());
    assert_eq!(refusing.records.asked_of(&ADA).len(), 1, "it still waits");

    let host = host();
    let grants = Arc::new(Grants {
        written: Mutex::default(),
        refusal: None,
    });
    host.store
        .grants_through(Arc::clone(&grants) as Arc<dyn AskedGrants>);
    let call = set_alarm();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts())
        .unwrap();
    resolve(&host, &at, &ada(), Offer::ApproveAndGrant, None).unwrap();
    let written = grants.written.lock().unwrap();
    let [(workspace, person, grant)] = written.as_slice() else {
        panic!("one grant: {written:?}");
    };
    assert_eq!(workspace, WORKSPACE);
    assert_eq!(*person, hex::encode(ADA));
    assert_eq!(grant.grantee, Grantee::agent("scout"));
    assert_eq!(grant.answer, Answer::Allow);
    assert_eq!(grant.kinds, KindSet::of(ActionKind::Undoable));
    assert_eq!(grant.granted_by, Grantee::person(hex::encode(ADA)));
    assert_eq!(
        tilo_action_policy::agent_answers::AnswerScope::of_node(&grant.node),
        Some(tilo_action_policy::agent_answers::AnswerScope::action(
            "clocks",
            "clocks.alarms",
            "set-alarm"
        ))
    );
    assert!(tilo_action_policy::agent_answers::is_answer(grant));
    let handed = host.dispatch.0.lock().unwrap();
    assert!(
        matches!(handed.as_slice(), [Handed::Readmit(_, request)] if request.invocation == call.invocation),
        "{handed:?}"
    );
}

/// A device's own question grants nothing.
#[test]
fn a_device_question_offers_no_grant() {
    let host = host();
    let facts = AskedFacts {
        plural: Some("alarms".into()),
        kind: "clocks.alarms".into(),
        effect: Some(ActionKind::Undoable),
        ..AskedFacts::by(AskedBy::Device(Named::default()))
    };
    let at = host
        .records
        .write_asked(WORKSPACE, &ada(), &set_alarm(), &facts)
        .unwrap();
    assert_eq!(
        host.records.asked_of(&ADA)[0].offers,
        vec![Offer::Approve, Offer::Decline]
    );
    refusal(resolve(&host, &at, &ada(), Offer::ApproveAndGrant, None));
}

/// The approve a caller writes into its own request is refused whole, whatever it names; the
/// call a resolve handed back is admitted once as a frame, and only byte for byte.
#[test]
fn a_forged_approve_is_refused() {
    use tilo_actions::remote::asked::{Approval, FORGED, admit_frame};
    let host = host();
    let call = set_alarm();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts())
        .unwrap();
    let forged = |request: &InvokeRequest| {
        let mut request = request.clone();
        request.confirmation = Some(Confirmation::Asked(Approval {
            record: at.clone(),
            choice: None,
        }));
        request
    };
    let refused =
        |request: &InvokeRequest, store: Option<&AskedStore>| match admit_frame(store, request) {
            Err(ActionCallError::Refused { reason }) => assert_eq!(reason, FORGED),
            other => panic!("a forged approve is refused: {other:?}"),
        };
    refused(&forged(&call), Some(&host.store));
    refused(&forged(&call), None);
    assert_eq!(host.records.asked_of(&ADA).len(), 1, "no record changed");
    assert!(host.dispatch.0.lock().unwrap().is_empty(), "nothing ran");
    admit_frame(Some(&host.store), &call).expect("a frame without an approve goes on");

    resolve(&host, &at, &ada(), Offer::Approve, None).unwrap();
    let Some(Handed::Readmit(_, handed)) = host.dispatch.0.lock().unwrap().pop() else {
        panic!("the approve is handed back");
    };
    let mut changed = handed.clone();
    changed.argument = InvokeArgument::Json(serde_json::Map::new());
    refused(&changed, Some(&host.store));
    let mut other = handed.clone();
    other.invocation = InvocationId::new();
    refused(&other, Some(&host.store));
    admit_frame(Some(&host.store), &handed).expect("the call the person approved goes on");
    refused(&handed, Some(&host.store));
}
