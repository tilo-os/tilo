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
    AskedBy, AskedDispatch, AskedFacts, AskedGrants, AskedKeeping, AskedRecords, KeptAsk, Named,
    Offer, RecordRef, Resolve,
};
use tilo_actions::remote::{
    ActionCallError, Caller, Confirmation, InvokeArgument, InvokeRequest, Pending,
    RemoteActionTarget,
};
use tilo_remote_actions::asked::{AskedStore, admit_frame};

const ADA: [u8; 32] = [1; 32];
const BO: [u8; 32] = [2; 32];
const WORKSPACE: &str = "acme";

/// What the dispatch was handed.
#[derive(Debug, PartialEq)]
enum Handed {
    Readmit(Caller, Box<InvokeRequest>),
    Declined(Caller, InvocationId),
}

#[derive(Default)]
struct Noted(Mutex<Vec<Handed>>);

impl AskedDispatch for Noted {
    fn context(&self, invocation: InvocationId) -> serde_json::Value {
        serde_json::json!({ "invocation": invocation.0.to_string() })
    }

    fn readmit(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        context: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        if *context != self.context(request.invocation) {
            return Err(ActionCallError::Refused {
                reason: format!("handed back with another context: {context}"),
            });
        }
        self.0
            .lock()
            .unwrap()
            .push(Handed::Readmit(caller.clone(), Box::new(request)));
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
    host_on(AskedStore::opened(None, None, Vec::new()).expect("a fresh store keeps its records"))
}

/// The host's dispatch, keeping its calls in `store`.
fn host_on(store: Arc<AskedStore>) -> Host {
    let dispatch = Arc::new(Noted::default());
    let weak: std::sync::Weak<dyn AskedDispatch> =
        Arc::downgrade(&(Arc::clone(&dispatch) as Arc<dyn AskedDispatch>));
    let records = store.for_dispatch("host", weak);
    Host {
        store,
        dispatch,
        records,
    }
}

/// The host's own state, as it keeps asked records across its restarts.
#[derive(Default)]
struct State(Mutex<Vec<KeptAsk>>);

impl AskedKeeping for State {
    fn keep(&self, ask: &KeptAsk) {
        let mut kept = self.0.lock().unwrap();
        kept.retain(|kept| kept.at != ask.at);
        kept.push(ask.clone());
    }

    fn forget(&self, at: &RecordRef) {
        self.0.lock().unwrap().retain(|kept| kept.at != *at);
    }
}

impl State {
    fn kept(&self) -> Vec<KeptAsk> {
        self.0.lock().unwrap().clone()
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
    answered(host.store.resolve(
        at,
        by,
        &Resolve {
            resolution,
            choice,
            ..Resolve::default()
        },
    ))
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
            tilo_remote_actions::asked::ANSWERED
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
        tilo_remote_actions::asked::ANSWERED
    );
}

/// Only Ada answers what asks Ada: not Bo, not her own agent acting for her, not an app's code
/// running as her (the confirmation is the system's), and not the device her agent's connection
/// came in on.
#[test]
fn nobody_but_the_person_asked_answers() {
    let host = host();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &set_alarm(), &facts())
        .unwrap();
    let bo = Caller::authenticated(BO, [4; 32]).in_workspace(WORKSPACE);
    let her_agent = Caller::delegated_agent("helper", ADA, [3; 32]).in_workspace(WORKSPACE);
    let her_app = ada().through_app("so.tilo.notes");
    for (who, by) in [
        ("Bo", bo),
        ("her agent", her_agent),
        ("an app as her", her_app),
        ("the asker", scout()),
    ] {
        assert_eq!(
            refusal(resolve(&host, &at, &by, Offer::Approve, None)),
            tilo_remote_actions::asked::NOT_YOURS,
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
    let written = grants.written.lock().unwrap().clone();
    let [(workspace, person, grant)] = written.as_slice() else {
        panic!("one grant: {written:?}");
    };
    assert_eq!(workspace, WORKSPACE);
    assert_eq!(*person, hex::encode(ADA));
    assert_eq!(grant.grantee, Grantee::agent("scout", hex::encode(ADA)));
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
    drop(handed);
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
    use tilo_actions::remote::asked::{Approval, FORGED};
    let host = host();
    let call = set_alarm();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts())
        .unwrap();
    let forged = |request: &InvokeRequest| {
        let mut request = request.clone();
        request.confirmation = Some(Confirmation::Asked(Approval::new(at.clone(), None)));
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

/// A call that waits is suspended in its record, which the host keeps in its own state: once
/// the host started again, it waits for Ada still, her approve hands it to the dispatch that
/// runs now under the dispatch's name, for the agent that asked and with what the dispatch
/// kept, and a second approve finds nothing to answer. An answered record is kept no more.
#[test]
fn a_call_waits_in_its_record_across_a_restart() {
    let state = Arc::new(State::default());
    let before = host_on(
        AskedStore::opened(
            None,
            Some(Arc::clone(&state) as Arc<dyn AskedKeeping>),
            Vec::new(),
        )
        .expect("the store keeps what waits"),
    );
    let call = set_alarm();
    let at = before
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts())
        .unwrap();
    assert_eq!(state.kept().len(), 1, "kept once written");
    drop(before);

    let after = host_on(
        AskedStore::opened(
            None,
            Some(Arc::clone(&state) as Arc<dyn AskedKeeping>),
            state.kept(),
        )
        .expect("the store keeps what waited"),
    );
    let listed = after.records.asked_of(&ADA);
    assert_eq!(listed.len(), 1, "it waits for Ada after the restart");
    assert_eq!(listed[0].record, at);
    refusal(resolve(
        &after,
        &at,
        &Caller::authenticated(BO, [4; 32]).in_workspace(WORKSPACE),
        Offer::Approve,
        None,
    ));
    resolve(&after, &at, &ada(), Offer::Approve, None).unwrap();
    let handed = after
        .dispatch
        .0
        .lock()
        .unwrap()
        .drain(..)
        .collect::<Vec<_>>();
    let [Handed::Readmit(caller, request)] = handed.as_slice() else {
        panic!("the dispatch is handed the call once: {handed:?}");
    };
    assert_eq!(*caller, scout(), "for the agent that asked");
    assert!(tilo_actions::remote::asked::same_call(request, &call));
    assert_eq!(request.invocation, call.invocation);
    assert!(
        state.kept().is_empty(),
        "an answered record is kept no more"
    );
    assert_eq!(
        refusal(resolve(&after, &at, &ada(), Offer::Approve, None)),
        tilo_remote_actions::asked::ANSWERED
    );
    assert!(after.dispatch.0.lock().unwrap().is_empty(), "it ran once");
}

/// The same call kept again while it waits is the one record, and a person has a bounded
/// number of calls waiting for them: one more is refused, while another person's are their
/// own, and an answer makes room again.
#[test]
fn a_call_is_kept_once_and_a_person_has_a_bounded_number_waiting() {
    let host = host();
    let first = set_alarm();
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &first, &facts())
        .unwrap();
    assert_eq!(
        host.records
            .write_asked(WORKSPACE, &scout(), &first, &facts())
            .unwrap(),
        at,
        "kept again under its id, the one record stands"
    );
    assert_eq!(host.records.asked_of(&ADA).len(), 1);
    for _ in 1..tilo_remote_actions::asked::MOST_WAITING {
        host.records
            .write_asked(WORKSPACE, &scout(), &set_alarm(), &facts())
            .unwrap();
    }
    assert_eq!(
        host.records
            .write_asked(WORKSPACE, &scout(), &set_alarm(), &facts())
            .unwrap_err(),
        tilo_remote_actions::asked::TOO_MANY_WAIT
    );
    let bos_agent = Caller::delegated_agent("scout", BO, [8; 32]).in_workspace(WORKSPACE);
    host.records
        .write_asked(WORKSPACE, &bos_agent, &set_alarm(), &facts())
        .expect("another person's questions are their own");
    resolve(&host, &at, &ada(), Offer::Decline, None).unwrap();
    host.records
        .write_asked(WORKSPACE, &scout(), &set_alarm(), &facts())
        .expect("an answer makes room again");
}

/// Ada changes the call through the form its ask offers as she approves it: the dispatch is
/// handed the call with her change, carrying the arguments so changed in her approve, and that
/// call alone is the one her approve admits, once; the call as it was asked, or with other
/// arguments, is not. A decline changes nothing, nor does an edit to what the form does not
/// offer, and the record stays asked for her to answer.
#[test]
fn an_approve_runs_the_call_as_its_person_changed_it() {
    use tilo_actions::remote::asked::FORGED;
    use tilo_actions::{DraftEdit, DraftField, DraftValue};
    let host = host();
    let call = set_alarm();
    let facts = AskedFacts {
        form: vec![DraftField {
            path: vec!["at".into()],
            label: "At".into(),
            value: DraftValue::Text {
                text: "07:00".into(),
            },
            editable: true,
            multiline: false,
            shows: None,
            role: None,
        }],
        ..facts()
    };
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts)
        .unwrap();
    let mut hers = serde_json::Map::new();
    hers.insert("at".into(), "07:30".into());
    let edit = |path: &str, text: &str| DraftEdit {
        path: vec![path.into()],
        text: text.into(),
    };
    let changing = |resolution, edits: Vec<DraftEdit>| Resolve {
        resolution,
        edits,
        ..Resolve::default()
    };

    assert_eq!(
        refusal(answered(host.store.resolve(
            &at,
            &ada(),
            &changing(Offer::Decline, vec![edit("at", "07:30")])
        ))),
        "A decline changes no arguments"
    );
    // Only what the form offers changes: neither an argument it does not name, nor where the
    // call goes.
    for path in ["repeat", "workspace", "target"] {
        assert_eq!(
            refusal(answered(host.store.resolve(
                &at,
                &ada(),
                &changing(Offer::Approve, vec![edit(path, "elsewhere")])
            ))),
            format!("{path} cannot be changed here")
        );
    }
    assert_eq!(host.records.asked_of(&ADA).len(), 1, "it is still asked");

    answered(host.store.resolve(
        &at,
        &ada(),
        &changing(Offer::Approve, vec![edit("at", "07:30")]),
    ))
    .unwrap();
    let Some(Handed::Readmit(caller, handed)) = host.dispatch.0.lock().unwrap().pop() else {
        panic!("the approve is handed back");
    };
    assert_eq!(caller, scout());
    assert_eq!(handed.invocation, call.invocation);
    assert_eq!(
        handed.argument,
        InvokeArgument::Json(hers.clone()),
        "hers run"
    );
    assert_eq!(
        handed
            .approval()
            .and_then(|approval| approval.arguments.as_deref().cloned()),
        Some(hers.clone())
    );
    assert!(tilo_actions::remote::asked::same_call(&call, &handed));

    let refused = |request: &InvokeRequest| match admit_frame(Some(&host.store), request) {
        Err(ActionCallError::Refused { reason }) => assert_eq!(reason, FORGED),
        other => panic!("only the call she approved is admitted: {other:?}"),
    };
    let mut as_asked = (*handed).clone();
    as_asked.argument = call.argument;
    refused(&as_asked);
    let mut other = (*handed).clone();
    let mut others = serde_json::Map::new();
    others.insert("at".into(), "03:00".into());
    other.argument = InvokeArgument::Json(others.clone());
    if let Some(Confirmation::Asked(approval)) = &mut other.confirmation {
        approval.arguments = Some(Box::new(others));
    }
    refused(&other);
    // Her approve, carried by the call she changed, aimed elsewhere: another action, or the
    // same record of another workspace. What she changed is the call's arguments, never where
    // it goes.
    let mut elsewhere = (*handed).clone();
    elsewhere.target.address = ActionAddress::new("clocks.alarms", "delete-alarm");
    refused(&elsewhere);
    let mut other_workspace = (*handed).clone();
    if let Some(Confirmation::Asked(approval)) = &mut other_workspace.confirmation {
        approval.record.workspace = "another-workspace".into();
    }
    refused(&other_workspace);
    admit_frame(Some(&host.store), &handed).expect("the call she approved goes on");
    refused(&handed);
}

/// Ada at a device that is not hers calls there, and the call asks her: that device answers
/// nothing for her, so it never approves its own call. Her own device does.
#[test]
fn the_foreign_device_that_asked_cannot_answer_for_its_person() {
    let host = host();
    let call = set_alarm();
    let foreign = Caller::authenticated(ADA, [7; 32])
        .in_workspace(WORKSPACE)
        .from_foreign_device(true);
    let at = host
        .records
        .write_asked(WORKSPACE, &foreign, &call, &facts())
        .unwrap();
    for resolution in [Offer::Approve, Offer::Decline] {
        assert_eq!(
            refusal(resolve(&host, &at, &foreign, resolution, None)),
            tilo_remote_actions::asked::ON_YOUR_DEVICE
        );
    }
    assert!(
        host.dispatch.0.lock().unwrap().is_empty(),
        "nothing was handed on"
    );
    resolve(&host, &at, &ada(), Offer::Approve, None).expect("her own device approves it");
    assert_eq!(host.dispatch.0.lock().unwrap().len(), 1);
}

/// A made-up token: no service ever issued it.
const TOKEN: &str = "made-up-token-6603";

/// A call of an action an app lists only as it is asked (`sync#connect`), which no registry
/// here describes, carrying a token beside a label.
fn connect(token: &str) -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert("label".into(), "made-up-label-4410".into());
    arguments.insert("token".into(), token.into());
    InvokeRequest {
        target: RemoteActionTarget {
            provider: "app:sync".into(),
            address: ActionAddress::new("sync.accounts", "connect"),
        },
        argument: InvokeArgument::Json(arguments),
        ..set_alarm()
    }
}

/// A store that keeps what waits through `state`, and knows the alarm's descriptor, which
/// takes no secret, and nothing else's.
fn describing(state: &Arc<State>) -> Host {
    let store = AskedStore::opened(
        None,
        Some(Arc::clone(state) as Arc<dyn AskedKeeping>),
        state.kept(),
    )
    .expect("the store keeps what waits");
    let alarm = set_alarm().target;
    store.keep_no_secrets(Arc::new(move |target: &RemoteActionTarget| {
        (*target == alarm).then(Vec::new)
    }));
    host_on(store)
}

/// What the dispatch was handed, once.
fn handed_once(host: &Host) -> InvokeRequest {
    let handed = host
        .dispatch
        .0
        .lock()
        .unwrap()
        .drain(..)
        .collect::<Vec<_>>();
    let [Handed::Readmit(_, request)] = handed.as_slice() else {
        panic!("the dispatch is handed the call once: {handed:?}");
    };
    (**request).clone()
}

/// A call of an action described nowhere here waits with no argument's value in its record,
/// every one held in memory: the approve runs it with all of them, and one asked before the
/// device started again is refused, saying to ask again. A call of an action whose descriptor
/// is known, and takes no secret, waits whole, and is approved after a restart as before.
#[test]
fn a_call_of_an_undescribed_action_keeps_no_argument_with_its_record() {
    let state = Arc::new(State::default());
    let host = describing(&state);
    let call = connect(TOKEN);
    let at = host
        .records
        .write_asked(WORKSPACE, &scout(), &call, &facts())
        .unwrap();
    let kept = serde_json::to_string(&state.kept()).unwrap()
        + &serde_json::to_string(&host.store.kept_ask(&at).unwrap()).unwrap()
        + &serde_json::to_string(&host.records.asked_of(&ADA)).unwrap();
    for value in [TOKEN, "made-up-label-4410"] {
        assert!(!kept.contains(value), "{value} kept: {kept}");
    }
    let mut sealed = host.store.kept_ask(&at).unwrap().sealed;
    sealed.sort();
    assert_eq!(
        sealed,
        ["label", "token"],
        "its record names what it left out"
    );
    resolve(&host, &at, &ada(), Offer::Approve, None).unwrap();
    let ran = handed_once(&host);
    assert_eq!(ran.argument, call.argument, "it runs with every argument");
    assert!(tilo_actions::remote::asked::same_call(&ran, &call));

    // Asked before the device starts again: what it carried went with the device's memory.
    let later = connect("made-up-token-9152");
    let later_at = host
        .records
        .write_asked(WORKSPACE, &scout(), &later, &facts())
        .unwrap();
    let alarm = set_alarm();
    let alarm_at = host
        .records
        .write_asked(WORKSPACE, &scout(), &alarm, &facts())
        .unwrap();
    let on_disk = serde_json::to_string(&state.kept()).unwrap();
    assert!(!on_disk.contains("made-up-token-9152"), "{on_disk}");
    assert!(
        on_disk.contains("07:00"),
        "a call whose descriptor takes no secret is kept whole: {on_disk}"
    );
    drop(host);
    let after = describing(&state);
    let refused = refusal(resolve(&after, &later_at, &ada(), Offer::Approve, None));
    assert!(
        refused.contains(tilo_remote_actions::asked::SECRET_GONE),
        "{refused}"
    );
    assert!(after.dispatch.0.lock().unwrap().is_empty(), "nothing ran");
    resolve(&after, &alarm_at, &ada(), Offer::Approve, None).unwrap();
    let ran = handed_once(&after);
    assert!(tilo_actions::remote::asked::same_call(&ran, &alarm));
    assert_eq!(ran.argument, alarm.argument);
}
