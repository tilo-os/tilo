//! The asked records a host keeps, and the one place a person's answer to one is admitted.
//!
//! Each record is the call that waits, as its dispatch handed it over, beside what its person
//! is shown and the dispatch that keeps it. An answer is admitted only from the person the
//! record asks, only while it is asked, and only once: the record leaves asked before the call
//! is handed back, so a second answer, or the same one again, finds nothing to answer.
//!
//! A record that waits is kept in the host's own state as well ([`AskedKeeping`]): the call
//! is suspended in it, and once the host started again it waits in it still, for its person
//! to answer as before. An approve then admits it as a new call under its id, for the caller
//! kept with it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tilo_lock::MutexRecover as _;
use zeroize::Zeroizing;

use super::super::{ActionCallError, Caller, Completer, InvokeRequest, Pending, pending};
use super::{
    Approval, Asked, AskedBy, AskedDispatch, AskedFacts, AskedGrants, AskedKeeping, AskedRecords,
    KeptAsk, Offer, RecordRef, Resolve, answering_person, asked_person,
};
use crate::outcome::InvocationId;

/// The most answered records kept beside the asked ones; the oldest go first.
const ANSWERED_KEPT: usize = 1024;

/// The most calls that wait for one person's answer at a time: one more is refused, so no
/// caller buries their questions under its own.
pub const MOST_WAITING: usize = 32;

/// What keeping one call more than [`MOST_WAITING`] for a person is refused with.
pub const TOO_MANY_WAIT: &str = "Too many questions already wait for this person's answer: answer or decline some of them first";

/// What a resolve of a record nobody asks the caller is refused with.
pub const NOT_YOURS: &str = "Only the person this question asks can answer it";
/// What a resolve of a record no longer asked is refused with.
pub const ANSWERED: &str = "This question was answered already";
/// What a resolve naming no record kept here is refused with.
pub const NOTHING_ASKED: &str = "Nothing waits for an answer under that record";
/// What an answer from another device to a question answered only where it was asked (what
/// the person types in secret, the files they pick) is refused with.
pub const ANSWERED_THERE: &str = "This question is answered on the device that asked it";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Asked,
    Running,
    Declined,
    Withdrawn,
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
    /// The dispatch that keeps the call, by its name ([`AskedStore::for_dispatch`]).
    dispatch: String,
    /// What it needs to admit the call again ([`AskedDispatch::context`]).
    context: serde_json::Value,
}

impl Record {
    fn kept(&self) -> KeptAsk {
        KeptAsk {
            at: self.at.clone(),
            person: self.person.clone(),
            caller: self.caller.kept(),
            request: self.request.clone(),
            facts: self.facts.clone(),
            asked_at_ms: self.asked_at_ms,
            dispatch: self.dispatch.clone(),
            context: self.context.clone(),
            taken: None,
            outcome: None,
        }
    }
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
        let mut excess = answered.saturating_sub(ANSWERED_KEPT);
        self.records.retain(|record| {
            if excess > 0 && record.status != Status::Asked {
                excess = excess.saturating_sub(1);
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
    keeping: OnceLock<Arc<dyn AskedKeeping>>,
    /// Each dispatch that keeps calls here, by its name.
    dispatches: Mutex<HashMap<String, Weak<dyn AskedDispatch>>>,
    /// The device this store asks on, its key in hex: where a question answered only where it
    /// was asked is answered.
    here: OnceLock<String>,
}

/// One dispatch's way into an [`AskedStore`]: what it writes is kept as its own, so a person's
/// approve of it comes back to it.
struct DispatchRecords {
    store: Arc<AskedStore>,
    name: String,
    dispatch: Weak<dyn AskedDispatch>,
}

impl AskedStore {
    /// Ask on the device whose key is `device`: a question answered only where it was asked
    /// ([`AskedFacts::answered_where_asked`]) names it, and is answered only from it.
    pub fn asks_on(&self, device: &[u8; 32]) {
        let _ = self.here.set(hex::encode(device));
    }

    /// The device this store asks on, its key in hex, once it is told ([`Self::asks_on`]).
    #[must_use]
    pub fn here(&self) -> Option<&str> {
        self.here.get().map(String::as_str)
    }

    /// The one store a host keeps the calls that wait in: grants people give with an approve
    /// are written through `grants`, every record that waits is kept through `keeping` as well,
    /// and each of `earlier` — the records kept before the host started — waits here again.
    /// Both `grants` and `keeping` are the host's own, and a host that supplies neither keeps a
    /// store that forgets its records and grants nothing.
    ///
    /// # Errors
    /// A store that keeps its records somewhere already.
    pub fn opened(
        grants: Option<Arc<dyn AskedGrants>>,
        keeping: Option<Arc<dyn AskedKeeping>>,
        earlier: Vec<KeptAsk>,
    ) -> Result<Arc<Self>, String> {
        let store = Arc::new(Self::default());
        if let Some(grants) = grants {
            store.grants_through(grants);
        }
        match keeping {
            Some(keeping) => store.keep_through(keeping, earlier)?,
            None if earlier.is_empty() => {}
            None => {
                return Err("a store handed records to wait for keeps them nowhere".to_owned());
            }
        }
        Ok(store)
    }

    /// Write the grants people give with an approve through `grants`; without, no answer
    /// grants anything.
    pub fn grants_through(&self, grants: Arc<dyn AskedGrants>) {
        let _ = self.grants.set(grants);
    }

    /// Keep every record that waits through `keeping` as well, and wait in this store again
    /// for each of `earlier`, the records it kept before the host started: once.
    ///
    /// # Errors
    /// A store that keeps its records somewhere already.
    pub fn keep_through(
        &self,
        keeping: Arc<dyn AskedKeeping>,
        earlier: Vec<KeptAsk>,
    ) -> Result<(), String> {
        self.keeping
            .set(keeping)
            .map_err(|_| "this store keeps its records already".to_owned())?;
        let mut kept = self.kept.lock_recover();
        for ask in earlier {
            // A call kept for a device that keeps the person's root is taken in where one does
            // ([`Self::take_in`]), by how it stands then.
            if ask.dispatch == super::AT_ROOT || kept.find(&ask.at).is_some() {
                continue;
            }
            kept.records.push(Record {
                invocation: ask.request.invocation,
                at: ask.at,
                person: ask.person,
                caller: ask.caller.restored(),
                request: ask.request,
                facts: ask.facts,
                asked_at_ms: ask.asked_at_ms,
                status: Status::Asked,
                relayed: false,
                dispatch: ask.dispatch,
                context: ask.context,
            });
        }
        kept.moved();
        drop(kept);
        Ok(())
    }

    /// The records the dispatch `name` writes its waiting calls through: an approve of one is
    /// handed back to it, also one it wrote before the host started again under that name.
    #[must_use]
    pub fn for_dispatch(
        self: &Arc<Self>,
        name: &str,
        dispatch: Weak<dyn AskedDispatch>,
    ) -> Arc<dyn AskedRecords> {
        self.dispatches
            .lock_recover()
            .insert(name.to_owned(), dispatch.clone());
        Arc::new(DispatchRecords {
            store: Arc::clone(self),
            name: name.to_owned(),
            dispatch,
        })
    }

    /// The dispatch `name`, while it runs.
    fn dispatch(&self, name: &str) -> Weak<dyn AskedDispatch> {
        self.dispatches
            .lock_recover()
            .get(name)
            .cloned()
            .unwrap_or_else(|| -> Weak<dyn AskedDispatch> { Weak::<Gone>::new() })
    }

    /// Keep `record` where the host keeps the records that wait, if it keeps them anywhere.
    fn keep(&self, record: &KeptAsk) {
        if let Some(keeping) = self.keeping.get() {
            keeping.keep(record);
        }
    }

    /// Forget the record `at` where the host keeps them: it waits no more.
    fn forget(&self, at: &RecordRef) {
        if let Some(keeping) = self.keeping.get() {
            keeping.forget(at);
        }
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
        (name, dispatch): (&str, &Weak<dyn AskedDispatch>),
    ) -> Result<RecordRef, String> {
        let person = asked_person(caller).ok_or("the call acts for no person to ask")?;
        let at = RecordRef {
            workspace: workspace.to_owned(),
            // The invocation's own id: what the workspace's invocations publish it under.
            record: request.invocation.0.to_string(),
            device: facts
                .answered_where_asked()
                .then(|| self.here.get().cloned())
                .flatten(),
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
        let waiting = kept
            .records
            .iter()
            .filter(|record| record.status == Status::Asked && record.person == person)
            .count();
        if waiting >= MOST_WAITING {
            return Err(TOO_MANY_WAIT.to_owned());
        }
        let context = dispatch
            .upgrade()
            .map_or(serde_json::Value::Null, |dispatch| {
                dispatch.context(request.invocation)
            });
        let record = Record {
            at: at.clone(),
            invocation: request.invocation,
            person,
            caller: caller.clone(),
            request: request.clone(),
            facts: facts.clone(),
            asked_at_ms: tilo_clock::unix_now_ms(),
            status: Status::Asked,
            relayed: false,
            dispatch: name.to_owned(),
            context,
        };
        self.keep(&record.kept());
        kept.records.push(record);
        kept.moved();
        drop(kept);
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
        drop(kept);
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
        // A call run at a device that keeps the person's root stays in its record until how it
        // ended is written there ([`Self::settle`]): a restart in between resumes it.
        let at_root = self
            .kept
            .lock_recover()
            .find(at)
            .is_some_and(|record| record.dispatch == super::AT_ROOT);
        let admitted = self.admit(at, answering, answer);
        if admitted.is_ok() && !at_root {
            self.forget(at);
        }
        match admitted {
            Err(error) => Pending::ready(Err(error)),
            Ok(Admitted::Declined(caller, invocation, dispatch)) => {
                Pending::ready(match self.dispatch(&dispatch).upgrade() {
                    Some(dispatch) => dispatch.declined(&caller, invocation),
                    // The call ended with the dispatch that kept it: nothing runs it.
                    None => Ok(()),
                })
            }
            Ok(Admitted::Approved(caller, request, (dispatch, context))) => Pending::ready(
                readmit(&self.dispatch(&dispatch), &caller, *request, &context),
            ),
            Ok(Admitted::Granting(caller, request, (dispatch, context), grant, person)) => {
                let dispatch = self.dispatch(&dispatch);
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
                        Some(Ok(())) => readmit(&dispatch, &caller, *request, &context),
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
        // A call only the person's root signs is answered where a device keeps it: elsewhere no
        // dispatch runs it, and the record waits on for that device.
        if record.dispatch == super::AT_ROOT && self.dispatch(&record.dispatch).upgrade().is_none()
        {
            return Err(refused(super::ANSWERED_AT_ROOT));
        }
        // What is typed in secret, or picked from the device's files, reaches the asker only on
        // the device that asked: answered by the person there (a caller of no other device's).
        if let Some(device) = &record.at.device
            && answering
                .device_public_key()
                .is_some_and(|answering| hex::encode(answering) != *device)
        {
            return Err(refused(ANSWERED_THERE));
        }
        if !Offer::of(&record.facts).contains(&answer.resolution) {
            return Err(refused("This question does not offer that answer"));
        }
        admits_what_it_takes(&record.facts, answer)?;
        let choices = record.facts.choices.len();
        match (answer.resolution, answer.choice) {
            (Offer::Decline, None) => {}
            (Offer::Decline, Some(_)) => return Err(refused("A decline picks no choice")),
            (_, Some(choice)) if choice >= choices => {
                return Err(refused(&format!(
                    "This question offers {choices} choices; there is none at {choice}"
                )));
            }
            (_, None) if choices > 0 && !record.facts.many => {
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
        let context = record.context.clone();
        let admitted = if answer.resolution == Offer::Decline {
            record.status = Status::Declined;
            Admitted::Declined(caller, record.invocation, dispatch)
        } else {
            record.status = Status::Running;
            let mut request = record.request.clone();
            request.confirmation = Some(super::super::Confirmation::Asked(Approval {
                chosen: answer.chosen.clone(),
                typed: answer.typed.iter().cloned().map(Zeroizing::new).collect(),
                files: answer.files.clone(),
                ..Approval::new(at.clone(), answer.choice)
            }));
            match grant {
                Some(grant) => Admitted::Granting(
                    caller,
                    Box::new(request),
                    (dispatch, context),
                    grant,
                    person.to_owned(),
                ),
                None => Admitted::Approved(caller, Box::new(request), (dispatch, context)),
            }
        };
        kept.moved();
        drop(kept);
        Ok(admitted)
    }

    /// Withdraw the record `at` while it is asked, as whoever asked it does when what asked
    /// goes away (the app instance or the page it depended on closed, the program that asked
    /// ended): it waits no more, every card of it reads `message`, and the call ends cancelled
    /// at its dispatch. The one way an ask ends without its person's answer. A record already
    /// answered, or not kept here, is left as it is.
    pub fn withdraw(&self, at: &RecordRef, message: &str) {
        let mut kept = self.kept.lock_recover();
        let Some(record) = kept
            .find(at)
            .filter(|record| record.status == Status::Asked)
        else {
            return;
        };
        record.status = Status::Withdrawn;
        let (caller, invocation, dispatch) = (
            record.caller.clone(),
            record.invocation,
            record.dispatch.clone(),
        );
        kept.moved();
        drop(kept);
        self.forget(at);
        if let Some(dispatch) = self.dispatch(&dispatch).upgrade() {
            dispatch.withdrawn(&caller, invocation, message);
        }
    }

    /// Every record the dispatch `name` keeps that is asked still, with the target its call
    /// goes to: what a dispatch reads once it started again, to withdraw what asked of a
    /// provider that ended with it.
    #[must_use]
    pub fn asked_by(&self, name: &str) -> Vec<(RecordRef, super::super::RemoteActionTarget)> {
        self.kept
            .lock_recover()
            .records
            .iter()
            .filter(|record| record.status == Status::Asked && record.dispatch == name)
            .map(|record| (record.at.clone(), record.request.target.clone()))
            .collect()
    }

    /// Take in each of `asks`, a call kept in the workspace to be run at a device that keeps the
    /// person's root ([`super::AT_ROOT`]), that this store does not keep yet: one nobody took
    /// waits here for the person's answer, as one written here does; one this device took before
    /// it started again is run again, as the person answered it.
    pub fn take_in(&self, asks: Vec<KeptAsk>) {
        let here = self.here.get().cloned();
        let mut resumed = Vec::new();
        let mut kept = self.kept.lock_recover();
        let before = kept.records.len();
        for ask in asks {
            if ask.dispatch != super::AT_ROOT
                || ask.outcome.is_some()
                || kept.find(&ask.at).is_some()
            {
                continue;
            }
            let status = match &ask.taken {
                None => Status::Asked,
                Some(taken) if Some(taken) == here.as_ref() => Status::Running,
                // Another device of the person's took it, and runs it.
                Some(_) => continue,
            };
            let caller = ask.caller.restored();
            if status == Status::Running {
                resumed.push((caller.clone(), ask.request.clone(), ask.context.clone()));
            }
            kept.records.push(Record {
                invocation: ask.request.invocation,
                at: ask.at,
                person: ask.person,
                caller,
                request: ask.request,
                facts: ask.facts,
                asked_at_ms: ask.asked_at_ms,
                status,
                relayed: false,
                dispatch: ask.dispatch,
                context: ask.context,
            });
        }
        if kept.records.len() != before {
            kept.moved();
        }
        drop(kept);
        let dispatch = self.dispatch(super::AT_ROOT);
        for (caller, request, context) in resumed {
            let invocation = request.invocation;
            if let Err(error) = readmit(&dispatch, &caller, request, &context) {
                self.settle_for(
                    invocation,
                    crate::InvocationUpdate::Failed {
                        invocation,
                        error: crate::ActionFailure {
                            code: "not_resumed".into(),
                            message: format!(
                                "It was answered, and could not be run again after this device \
                                 started again: {error}"
                            ),
                            details: None,
                            retryable: true,
                        },
                    },
                );
            }
        }
    }

    /// Write into the record of `invocation` that this device took it to run, before it runs.
    pub fn take(&self, invocation: InvocationId) {
        if let Some(mut ask) = self.kept_of(invocation) {
            ask.taken = self.here.get().cloned();
            self.keep(&ask);
        }
    }

    /// Write how the call `invocation` ended where it ran, `outcome`, into its record in the
    /// workspace: what the device that asked it follows ([`KeptAsk::outcome`]). This device keeps
    /// it no longer.
    pub fn settle_for(&self, invocation: InvocationId, outcome: crate::InvocationUpdate) {
        let Some(mut ask) = self.kept_of(invocation) else {
            return;
        };
        ask.taken = self.here.get().cloned();
        ask.outcome = Some(outcome);
        self.keep(&ask);
        let mut kept = self.kept.lock_recover();
        kept.records.retain(|record| record.at != ask.at);
        kept.moved();
    }

    /// The record `at` as it is kept here.
    #[must_use]
    pub fn kept_ask(&self, at: &RecordRef) -> Option<KeptAsk> {
        self.kept
            .lock_recover()
            .records
            .iter()
            .find(|record| record.at == *at)
            .map(Record::kept)
    }

    /// The record of `invocation` as it is kept.
    fn kept_of(&self, invocation: InvocationId) -> Option<KeptAsk> {
        self.kept
            .lock_recover()
            .records
            .iter()
            .find(|record| record.invocation == invocation)
            .map(Record::kept)
    }

    /// Forget the record `at`, whose call ended where it ran: here and in the workspace.
    pub fn settled(&self, at: &RecordRef) {
        let mut kept = self.kept.lock_recover();
        kept.records.retain(|record| record.at != *at);
        kept.moved();
        drop(kept);
        self.forget(at);
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
            let ask = record.kept();
            kept.moved();
            drop(kept);
            self.keep(&ask);
        }
    }
}

/// The dispatch that keeps a call, by its name, and what it needs to admit it again.
type KeptBy = (String, serde_json::Value);

enum Admitted {
    Declined(Caller, InvocationId, String),
    Approved(Caller, Box<InvokeRequest>, KeptBy),
    Granting(
        Caller,
        Box<InvokeRequest>,
        KeptBy,
        tilo_action_policy::Grant,
        String,
    ),
}

/// No dispatch: what a call whose dispatch is not running is handed back to.
struct Gone;

impl AskedDispatch for Gone {
    fn context(&self, _: InvocationId) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn readmit(
        &self,
        _: &Caller,
        _: InvokeRequest,
        _: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        Err(gone())
    }

    fn declined(&self, _: &Caller, _: InvocationId) -> Result<(), ActionCallError> {
        Ok(())
    }
}

/// Refused where `answer` carries what `facts` does not ask for: typed entries other than one
/// per take, picked files where none are asked, several choices where one is, or any of them
/// with a decline.
fn admits_what_it_takes(facts: &AskedFacts, answer: &Resolve) -> Result<(), ActionCallError> {
    if answer.resolution == Offer::Decline {
        return if answer.typed.is_empty() && answer.files.is_empty() && answer.chosen.is_empty() {
            Ok(())
        } else {
            Err(refused("A decline carries nothing typed, picked or chosen"))
        };
    }
    if answer.typed.len() != facts.takes.len() {
        return Err(refused(&format!(
            "This question takes {} typed answers; {} came",
            facts.takes.len(),
            answer.typed.len()
        )));
    }
    if !answer.files.is_empty() && facts.files.is_none() {
        return Err(refused("This question asks for no files"));
    }
    if !answer.chosen.is_empty() {
        if !facts.many {
            return Err(refused("This question is answered with one choice"));
        }
        if let Some(beyond) = answer
            .chosen
            .iter()
            .find(|chosen| **chosen >= facts.choices.len())
        {
            return Err(refused(&format!(
                "This question offers {} choices; there is none at {beyond}",
                facts.choices.len()
            )));
        }
    }
    Ok(())
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
    context: &serde_json::Value,
) -> Result<(), ActionCallError> {
    dispatch
        .upgrade()
        .ok_or_else(gone)
        .and_then(|dispatch| dispatch.readmit(caller, request, context))
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
        AskedBy::Device(_) | AskedBy::Program { .. } => {
            return Err(refused("A device's or a program's question grants nothing"));
        }
    };
    let effect = record
        .facts
        .effect
        .ok_or_else(|| refused("This action declares no kind to grant"))?;
    if record.facts.kind.is_empty() {
        return Err(refused("This action is on no resource type to grant"));
    }
    let (app, kind, action) = (
        app_of(&record.request.target.provider),
        record.facts.kind.clone(),
        record.request.target.address.action.clone(),
    );
    // The resource the call is on, where it names one other than its type's collection: one
    // site's "always" is not every site's.
    let resource = &record.request.target.address.resource;
    let scope = if resource.is_empty() || *resource == kind {
        AnswerScope::action(app, kind, action)
    } else {
        AnswerScope::resource(app, kind, resource.clone(), action)
    };
    Ok(answer_grant(
        &record.at.workspace,
        delegate,
        (&scope, effect),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(record.person.clone()),
    ))
}

impl Record {
    /// The record as its person reads it.
    fn row(&self) -> Asked {
        let target = &self.request.target;
        let app = app_of(&target.provider).to_owned();
        Asked {
            record: self.at.clone(),
            workspace: self.at.workspace.clone(),
            workspace_name: self.at.workspace.clone(),
            action: format!("{app}#{}", target.address.action),
            name: if self.facts.verb.is_empty() {
                target.address.action.clone()
            } else {
                self.facts.verb.clone()
            },
            app,
            asked_at_ms: self.asked_at_ms,
            pressed: None,
            offers: Offer::of(&self.facts),
            facts: self.facts.clone(),
        }
    }
}

/// A call that waits in its asked record, as a reading of its workspace's invocations shows it.
#[derive(Clone, Debug)]
pub struct Waiting {
    pub invocation: InvocationId,
    /// The person asked, the hex of their root key.
    pub person: String,
    /// Who made the call.
    pub caller: Caller,
    pub asked: Asked,
}

impl AskedStore {
    /// Whether a record is asked of its person here: a call kept for a device that keeps their
    /// root ([`super::AT_ROOT`]) is asked only there, and is under way everywhere else.
    fn answered_here(&self) -> impl Fn(&Record) -> bool {
        let serves_root = self.dispatch(super::AT_ROOT).upgrade().is_some();
        move |record| serves_root || record.dispatch != super::AT_ROOT
    }

    /// Every call that waits for its person's answer in `workspace`, oldest first: what the
    /// reading of the workspace's invocations shows of each.
    #[must_use]
    pub fn waiting_in(&self, workspace: &str) -> Vec<Waiting> {
        let answered_here = self.answered_here();
        let kept = self.kept.lock_recover();
        let mut rows: Vec<Waiting> = kept
            .records
            .iter()
            .filter(|record| record.status == Status::Asked && record.at.workspace == workspace)
            .filter(|record| answered_here(record))
            .map(|record| Waiting {
                invocation: record.invocation,
                person: record.person.clone(),
                caller: record.caller.clone(),
                asked: record.row(),
            })
            .collect();
        drop(kept);
        rows.sort_by_key(|row| row.asked.asked_at_ms);
        rows
    }
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
        let answered_here = self.answered_here();
        let kept = self.kept.lock_recover();
        let mut rows: Vec<Asked> = kept
            .records
            .iter()
            .filter(|record| record.status == Status::Asked && record.person == person)
            .filter(|record| answered_here(record))
            .map(Record::row)
            .collect();
        drop(kept);
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
        self.store.write(
            workspace,
            caller,
            request,
            facts,
            (&self.name, &self.dispatch),
        )
    }

    fn asked_of(&self, person: &[u8; 32]) -> Vec<Asked> {
        self.store.asked_of(person)
    }

    fn changed(&self) -> Pending<()> {
        self.store.changed()
    }
}
