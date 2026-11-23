//! The asked records a host keeps, and the one place a person's answer to one is admitted.
//!
//! Each record is the call that waits, as its dispatch handed it over, beside what its person
//! is shown and the dispatch that keeps it. An answer is admitted only from the person the
//! record asks, only while it is asked, and only once: the record leaves asked before the call
//! is handed back, so a second answer, or the same one again, finds nothing to answer.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use tilo_lock::MutexRecover as _;

use super::super::{ActionCallError, Caller, Completer, InvokeRequest, Pending, pending};
use super::{
    Approval, Asked, AskedBy, AskedDispatch, AskedFacts, AskedGrants, AskedRecords, Offer,
    RecordRef, Resolve, answering_person, asked_person,
};
use crate::outcome::InvocationId;

/// The most answered records kept beside the asked ones; the oldest go first.
const ANSWERED_KEPT: usize = 1024;

/// What a resolve of a record nobody asks the caller is refused with.
pub const NOT_YOURS: &str = "Only the person this question asks can answer it";
/// What a resolve of a record no longer asked is refused with.
pub const ANSWERED: &str = "This question was answered already";
/// What a resolve naming no record kept here is refused with.
pub const NOTHING_ASKED: &str = "Nothing waits for an answer under that record";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Asked,
    Running,
    Declined,
}

struct Record {
    at: RecordRef,
    invocation: InvocationId,
    /// The person asked, their root key in hex.
    person: String,
    caller: Caller,
    request: InvokeRequest,
    facts: AskedFacts,
    asked_at_ms: u64,
    status: Status,
    /// Whether the approved call was admitted from a frame once already
    /// ([`AskedStore::admits_approval`]).
    relayed: bool,
    dispatch: Weak<dyn AskedDispatch>,
}

#[derive(Default)]
struct Kept {
    records: Vec<Record>,
    changed: Vec<Completer<()>>,
}

impl Kept {
    fn find(&mut self, at: &RecordRef) -> Option<&mut Record> {
        self.records.iter_mut().find(|record| record.at == *at)
    }

    /// Wake every listing that waits for a change, and forget the oldest answered records
    /// beyond [`ANSWERED_KEPT`].
    fn moved(&mut self) {
        let answered = self
            .records
            .iter()
            .filter(|record| record.status != Status::Asked)
            .count();
        let mut drop = answered.saturating_sub(ANSWERED_KEPT);
        self.records.retain(|record| {
            if drop > 0 && record.status != Status::Asked {
                drop -= 1;
                false
            } else {
                true
            }
        });
        for waiter in self.changed.drain(..) {
            waiter.complete(());
        }
    }
}

/// The asked records of a host's workspaces, kept in memory with the calls that wait in them.
#[derive(Default)]
pub struct AskedStore {
    kept: Mutex<Kept>,
    grants: OnceLock<Arc<dyn AskedGrants>>,
}

/// One dispatch's way into an [`AskedStore`]: what it writes is kept as its own, so a person's
/// approve of it comes back to it.
struct DispatchRecords {
    store: Arc<AskedStore>,
    dispatch: Weak<dyn AskedDispatch>,
}

impl AskedStore {
    /// Write the grants people give with an approve through `grants`; without, no answer
    /// grants anything.
    pub fn grants_through(&self, grants: Arc<dyn AskedGrants>) {
        let _ = self.grants.set(grants);
    }

    /// The records `dispatch` writes its waiting calls through: an approve of one is handed
    /// back to it.
    #[must_use]
    pub fn for_dispatch(
        self: &Arc<Self>,
        dispatch: Weak<dyn AskedDispatch>,
    ) -> Arc<dyn AskedRecords> {
        Arc::new(DispatchRecords {
            store: Arc::clone(self),
            dispatch,
        })
    }

    /// The record kept here under the address `record`, with the workspace it is in.
    #[must_use]
    pub fn record(&self, record: &str) -> Option<RecordRef> {
        self.kept
            .lock_recover()
            .records
            .iter()
            .find(|kept| kept.at.record == record)
            .map(|kept| kept.at.clone())
    }

    fn write(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
        dispatch: Weak<dyn AskedDispatch>,
    ) -> Result<RecordRef, String> {
        let person = asked_person(caller).ok_or("the call acts for no person to ask")?;
        let at = RecordRef {
            workspace: workspace.to_owned(),
            // The invocation's own id: what the workspace's invocations publish it under.
            record: request.invocation.0.to_string(),
        };
        let mut kept = self.kept.lock_recover();
        if let Some(record) = kept.find(&at) {
            // The same call kept again (a retry under its id) is the record already kept.
            return if record.status == Status::Asked && super::same_call(&record.request, request) {
                Ok(at)
            } else {
                Err(ANSWERED.to_owned())
            };
        }
        kept.records.push(Record {
            at: at.clone(),
            invocation: request.invocation,
            person,
            caller: caller.clone(),
            request: request.clone(),
            facts: facts.clone(),
            asked_at_ms: tilo_clock::unix_now_ms(),
            status: Status::Asked,
            relayed: false,
            dispatch,
        });
        kept.moved();
        Ok(at)
    }

    /// Whether `request`, arriving as a frame from outside a dispatch, may carry the person's
    /// approve it carries: only the call a resolve of its record approved and handed back,
    /// byte for byte the same, under the same id, and once. A frame that carries an approve
    /// this does not admit is refused whole, never run without it: a caller cannot approve
    /// its own call. A frame that carries none is admitted as it is.
    pub fn admits_approval(&self, request: &InvokeRequest) -> bool {
        let Some(approval) = request.approval() else {
            return true;
        };
        let mut kept = self.kept.lock_recover();
        let Some(record) = kept.find(&approval.record) else {
            return false;
        };
        let admitted = record.status == Status::Running
            && !record.relayed
            && record.invocation == request.invocation
            && super::same_call(&record.request, request);
        if admitted {
            record.relayed = true;
        }
        admitted
    }

    /// Answer the record `at` as `answering` says ([`Resolve`]): admitted only from the person
    /// it asks, only while it is asked, with an answer it offers and a choice among its own.
    /// An approve hands the call back to the dispatch that keeps it, which admits it again for
    /// its original caller; a decline ends it, its caller told so; an approve and grant writes
    /// the grant first. Answered once the call is admitted again or ended, not once it ran.
    pub fn resolve(
        self: &Arc<Self>,
        at: &RecordRef,
        answering: &Caller,
        answer: &Resolve,
    ) -> Pending<Result<(), ActionCallError>> {
        match self.admit(at, answering, answer) {
            Err(error) => Pending::ready(Err(error)),
            Ok(Admitted::Declined(caller, invocation, dispatch)) => Pending::ready(
                dispatch
                    .upgrade()
                    .ok_or_else(gone)
                    .and_then(|dispatch| dispatch.declined(&caller, invocation)),
            ),
            Ok(Admitted::Approved(caller, request, dispatch)) => {
                Pending::ready(readmit(&dispatch, &caller, request))
            }
            Ok(Admitted::Granting(caller, request, dispatch, grant, person)) => {
                let Some(grants) = self.grants.get() else {
                    self.reopen(at);
                    return Pending::ready(Err(refused(
                        "No grant can be written here; approve it once instead",
                    )));
                };
                let written = grants.grant(&at.workspace, &person, grant);
                let (done, answer) = pending();
                let (store, at) = (Arc::clone(self), at.clone());
                let reopen = move || store.reopen(&at);
                written.then(move |written| {
                    let result = match written {
                        Some(Ok(())) => readmit(&dispatch, &caller, *request),
                        Some(Err(why)) => {
                            reopen();
                            Err(refused(&format!("The grant was not written: {why}")))
                        }
                        None => {
                            reopen();
                            Err(refused("The grant was not written"))
                        }
                    };
                    done.complete(result);
                });
                answer
            }
        }
    }

    /// Check `answer` to the record `at` from `answering` and take the record out of asked.
    fn admit(
        &self,
        at: &RecordRef,
        answering: &Caller,
        answer: &Resolve,
    ) -> Result<Admitted, ActionCallError> {
        let person = answering_person(answering).ok_or_else(|| refused(NOT_YOURS))?;
        let mut kept = self.kept.lock_recover();
        let record = kept.find(at).ok_or_else(|| refused(NOTHING_ASKED))?;
        if record.person != person {
            return Err(refused(NOT_YOURS));
        }
        if record.status != Status::Asked {
            return Err(refused(ANSWERED));
        }
        if !Offer::of(&record.facts).contains(&answer.resolution) {
            return Err(refused("This question does not offer that answer"));
        }
        let choices = record.facts.choices.len();
        match (answer.resolution, answer.choice) {
            (Offer::Decline, None) => {}
            (Offer::Decline, Some(_)) => return Err(refused("A decline picks no choice")),
            (_, Some(choice)) if choice >= choices => {
                return Err(refused(&format!(
                    "This question offers {choices} choices; there is none at {choice}"
                )));
            }
            (_, None) if choices > 0 => {
                return Err(refused("This question is answered with one of its choices"));
            }
            _ => {}
        }
        let grant = match answer.resolution {
            Offer::ApproveAndGrant => Some(grant_of(record)?),
            Offer::Approve | Offer::Decline => None,
        };
        let caller = record.caller.clone();
        let dispatch = record.dispatch.clone();
        let admitted = if answer.resolution == Offer::Decline {
            record.status = Status::Declined;
            Admitted::Declined(caller, record.invocation, dispatch)
        } else {
            record.status = Status::Running;
            let mut request = record.request.clone();
            request.confirmation = Some(super::super::Confirmation::Asked(Approval {
                record: at.clone(),
                choice: answer.choice,
            }));
            match grant {
                Some(grant) => Admitted::Granting(
                    caller,
                    Box::new(request),
                    dispatch,
                    grant,
                    person.to_owned(),
                ),
                None => Admitted::Approved(caller, request, dispatch),
            }
        };
        kept.moved();
        Ok(admitted)
    }

    /// Put the record `at` back to asked where its answer could not go on (its grant was not
    /// written): nothing of it ran.
    fn reopen(&self, at: &RecordRef) {
        let mut kept = self.kept.lock_recover();
        if let Some(record) = kept
            .find(at)
            .filter(|record| record.status == Status::Running)
        {
            record.status = Status::Asked;
            kept.moved();
        }
    }
}

enum Admitted {
    Declined(Caller, InvocationId, Weak<dyn AskedDispatch>),
    Approved(Caller, InvokeRequest, Weak<dyn AskedDispatch>),
    Granting(
        Caller,
        Box<InvokeRequest>,
        Weak<dyn AskedDispatch>,
        tilo_action_policy::Grant,
        String,
    ),
}

fn refused(reason: &str) -> ActionCallError {
    ActionCallError::Refused {
        reason: reason.to_owned(),
    }
}

fn gone() -> ActionCallError {
    ActionCallError::Unavailable {
        reason: "what kept this call is no longer running".into(),
    }
}

fn readmit(
    dispatch: &Weak<dyn AskedDispatch>,
    caller: &Caller,
    request: InvokeRequest,
) -> Result<(), ActionCallError> {
    dispatch
        .upgrade()
        .ok_or_else(gone)
        .and_then(|dispatch| dispatch.readmit(caller, request))
}

/// The app a call's target names: a record's app, an app's own provider, or the provider.
fn app_of(provider: &str) -> &str {
    if let Some(rest) = provider
        .strip_prefix(super::super::RESOURCE_PROVIDER)
        .and_then(|rest| rest.strip_prefix(':'))
    {
        return rest.split_once(':').map_or(rest, |(app, _)| app);
    }
    super::super::application_provider(provider).map_or(provider, |(_, app)| app)
}

/// The grant an approve and grant of `record` gives: its asker the asked action on every
/// resource of its type, at the action's own kind, until revoked, given by the person asked.
fn grant_of(record: &Record) -> Result<tilo_action_policy::Grant, ActionCallError> {
    use tilo_action_policy::agent_answers::{AnswerScope, answer_grant};
    let delegate = match &record.facts.by {
        AskedBy::Agent(agent) => tilo_action_policy::Grantee::agent(agent.id.clone()),
        AskedBy::App { app, .. } => tilo_action_policy::Grantee::app(app.clone()),
        AskedBy::Device(_) => return Err(refused("A device's question grants nothing")),
    };
    let effect = record
        .facts
        .effect
        .ok_or_else(|| refused("This action declares no kind to grant"))?;
    if record.facts.kind.is_empty() {
        return Err(refused("This action is on no resource type to grant"));
    }
    let scope = AnswerScope::action(
        app_of(&record.request.target.provider),
        record.facts.kind.clone(),
        record.request.target.address.action.clone(),
    );
    Ok(answer_grant(
        &record.at.workspace,
        delegate,
        (&scope, effect),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(record.person.clone()),
    ))
}

impl AskedRecords for AskedStore {
    fn write_asked(
        &self,
        _workspace: &str,
        _caller: &Caller,
        _request: &InvokeRequest,
        _facts: &AskedFacts,
    ) -> Result<RecordRef, String> {
        Err("no dispatch keeps this call to admit it again".to_owned())
    }

    fn asked_of(&self, person: &[u8; 32]) -> Vec<Asked> {
        let person = hex::encode(person);
        let kept = self.kept.lock_recover();
        let mut rows: Vec<Asked> = kept
            .records
            .iter()
            .filter(|record| record.status == Status::Asked && record.person == person)
            .map(|record| {
                let target = &record.request.target;
                let app = app_of(&target.provider).to_owned();
                Asked {
                    record: record.at.clone(),
                    workspace: record.at.workspace.clone(),
                    workspace_name: record.at.workspace.clone(),
                    action: format!("{app}#{}", target.address.action),
                    name: if record.facts.verb.is_empty() {
                        target.address.action.clone()
                    } else {
                        record.facts.verb.clone()
                    },
                    app,
                    asked_at_ms: record.asked_at_ms,
                    pressed: None,
                    offers: Offer::of(&record.facts),
                    facts: record.facts.clone(),
                }
            })
            .collect();
        rows.sort_by_key(|row| row.asked_at_ms);
        rows
    }

    fn changed(&self) -> Pending<()> {
        let (done, answer) = pending();
        self.kept.lock_recover().changed.push(done);
        answer
    }
}

impl AskedRecords for DispatchRecords {
    fn write_asked(
        &self,
        workspace: &str,
        caller: &Caller,
        request: &InvokeRequest,
        facts: &AskedFacts,
    ) -> Result<RecordRef, String> {
        self.store
            .write(workspace, caller, request, facts, self.dispatch.clone())
    }

    fn asked_of(&self, person: &[u8; 32]) -> Vec<Asked> {
        self.store.asked_of(person)
    }

    fn changed(&self) -> Pending<()> {
        self.store.changed()
    }
}
