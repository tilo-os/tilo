//! The questions programs outside Tilo ask the person here (the Linux portal's dialogs, polkit's
//! authentication, `NetworkManager`'s secrets, a Bluetooth pairing, and the session's own asks
//! that come on the same [`AskReceiver`]), raised as asked records the person answers like every
//! other: on the asking view, by the record's `resolve`.
//!
//! The session raises each question on the program's behalf: it calls its own
//! [`PROVIDER`]`/<handle>#`[`ANSWER`] through the device's dispatch, which this provider refuses
//! for want of the person's yes ([`ActionCallError::ConfirmationRequired`]), so the dispatch keeps
//! the call as its asked record. On approve the dispatch sends the same call back with the
//! person's answer ([`InvokeRequest::approval`]), and the provider hands it to the program that
//! waits. What the person typed travels on that call alone, on this device, and reaches the
//! program and nothing else: never the record, its history or a log.
//!
//! A declined question ends the call, and the program is told no. A program that stops asking
//! (polkit cancelled, the app closed its request, the process went away) takes its question
//! back, and the session cancels its own waiting call, which ends the record.
//!
//! What asks nothing (a code to type on another device) is a notice instead, which stops the
//! program; and an allowance the person gave beyond this session is told in a notice that takes
//! it back ([`ProgramNotices`]).

mod kinds;
mod notices;
#[cfg(test)]
mod tests;

pub(crate) use notices::{ProgramNotices, TAG as NOTICE_TAG};

/// Where the session's programs ask, and its side of it: the ones its caller or its desktop
/// supplied, or one of its own when neither did (a headless session), where its own asks (an
/// enrolment, a machine asking to join) come too.
pub(crate) fn for_session(
    supplied: Option<(AskSender, AskReceiver)>,
    desktop: Option<&(AskSender, AskReceiver)>,
) -> (AskSender, AskReceiver) {
    supplied
        .or_else(|| desktop.cloned())
        .unwrap_or_else(tilo_service::asks::channel)
}

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_service::{AskReceiver, AskSender, Question};

use kinds::{Answered, Grant, Put, Reply, Retry, Shown};

/// The provider a program's question is raised on.
pub(crate) const PROVIDER: &str = "session.asks";
/// The action that answers a question, on the question's handle.
pub(crate) const ANSWER: &str = "answer";

/// What the dispatch is reached through, as the session calls it: the device's one dispatch, and
/// who the session calls it as.
#[derive(Clone)]
pub(crate) struct Dispatch {
    pub actions: Arc<dyn ActionProvider + Send + Sync>,
    pub caller: Caller,
}

/// One question raised: the call it waits in, what it shows, and what an answer means.
struct Raised {
    invocation: InvocationId,
    question: Question,
    facts: AskedFacts,
    reply: Reply,
    grant: Option<Grant>,
}

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    raised: Mutex<BTreeMap<String, Raised>>,
    /// What asks nothing, by its question's handle: told in a notice.
    shown: Mutex<BTreeMap<String, (Question, Shown)>>,
    /// The allowances the person gave beyond this session, not yet told.
    granted: Mutex<Vec<Grant>>,
    dispatch: OnceLock<Dispatch>,
    /// Wakes the session's loop, where the notices are shown.
    wake: OnceLock<Wake>,
}

/// The programs' questions as asked records.
#[derive(Clone)]
pub(crate) struct ProgramAsks {
    questions: AskReceiver,
    inner: Arc<Inner>,
    runtime: tokio::runtime::Handle,
}

impl ProgramAsks {
    /// Raise every question `questions` receives, as it arrives, once the dispatch is reached
    /// ([`Self::serve_on`]); `wake` wakes the session's loop when a notice is due.
    pub(crate) fn over(
        questions: &AskReceiver,
        runtime: tokio::runtime::Handle,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let asks = Self {
            questions: questions.clone(),
            inner: Arc::default(),
            runtime,
        };
        let _ = asks.inner.wake.set(Arc::new(wake));
        let following = asks.clone();
        questions.on_change(move || following.follow());
        asks
    }

    /// Serve [`PROVIDER`] on the device's dispatch over the shell's link `link`, and raise the
    /// questions through that dispatch from then on. Without a link nobody here can be asked, and
    /// the questions wait unraised.
    pub(crate) fn serve_on(&self, link: &tilo_app_host::services::DeviceDaemon) {
        let shell = match link {
            Ok(shell) => shell.clone(),
            Err(reason) => {
                tracing::warn!(%reason, "a program's question reaches nobody here");
                return;
            }
        };
        let asks = self.clone();
        self.runtime.spawn(async move {
            let served = shell
                .provide(
                    PROVIDER,
                    &CataloguePage::default(),
                    Arc::new(asks.clone()) as Arc<dyn ActionProvider + Send + Sync>,
                    tilo_action_calls::serve,
                )
                .await;
            if let Err(error) = served {
                tracing::warn!(%error, "a program's question reaches nobody here");
                return;
            }
            asks.reach(Dispatch {
                actions: Arc::new(shell.dispatch()),
                caller: Caller::local_system(),
            });
        });
    }

    /// Raise the questions through `dispatch`, from now on.
    fn reach(&self, dispatch: Dispatch) {
        if self.inner.dispatch.set(dispatch).is_ok() {
            self.follow();
        }
    }

    /// The questions waiting on programs here: raise what is new or changed, and take back what
    /// its program no longer asks.
    fn follow(&self) {
        let Some(dispatch) = self.inner.dispatch.get().cloned() else {
            return;
        };
        let pending = self.questions.pending();
        let (raise, gone) = {
            let raised = self.inner.raised.lock_recover();
            let shown = self.inner.shown.lock_recover();
            let stands = |question: &Question| {
                raised
                    .get(&question.handle)
                    .is_some_and(|was| was.question.details == question.details)
                    || shown
                        .get(&question.handle)
                        .is_some_and(|(was, _)| was.details == question.details)
            };
            let raise: Vec<Question> = pending
                .iter()
                .filter(|question| !stands(question))
                .cloned()
                .collect();
            let gone: Vec<String> = raised
                .keys()
                .chain(shown.keys())
                .filter(|handle| !pending.iter().any(|question| &question.handle == *handle))
                .cloned()
                .collect();
            drop(raised);
            drop(shown);
            (raise, gone)
        };
        for handle in gone {
            self.withdraw(&dispatch, &handle);
        }
        for question in raise {
            self.raise(&dispatch, question, None);
        }
    }

    /// Put `question` to the person, `retry` saying why their last answer to it reached nothing.
    /// What was put under its handle before is taken back first: it showed what no longer stands.
    fn raise(&self, dispatch: &Dispatch, question: Question, retry: Option<Retry>) {
        let handle = question.handle.clone();
        self.withdraw(dispatch, &handle);
        let raised = match kinds::put(&question, retry) {
            Some(Put::Ask(raised)) => raised,
            Some(Put::Shown(shown)) => {
                self.inner
                    .shown
                    .lock_recover()
                    .insert(handle, (question, shown));
                self.wake();
                return;
            }
            None => {
                tracing::warn!(
                    kind = question.kind,
                    "nobody can be asked this kind of question"
                );
                self.questions.decline(&handle);
                return;
            }
        };
        let kinds::Raised {
            facts,
            reply,
            grant,
        } = *raised;
        let invocation = InvocationId::new();
        self.inner.raised.lock_recover().insert(
            handle.clone(),
            Raised {
                invocation,
                question,
                facts,
                reply,
                grant,
            },
        );
        let request = InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: PROVIDER.to_owned(),
                address: ActionAddress::new(handle.clone(), ANSWER),
            },
            argument: InvokeArgument::Json(serde_json::Map::new()),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let asks = self.clone();
        let dispatch = dispatch.clone();
        self.runtime.spawn(async move {
            let admitted = dispatch.actions.invoke(&dispatch.caller, request).await;
            match &admitted {
                Some(Ok(_)) => {}
                Some(Err(error)) => {
                    tracing::warn!(%error, "a program's question could not be put to the person");
                    asks.ended(&handle, invocation);
                    return;
                }
                None => {
                    tracing::warn!("the dispatch did not answer a program's question");
                    asks.ended(&handle, invocation);
                    return;
                }
            }
            asks.follow_call(&dispatch, &handle, invocation).await;
        });
    }

    /// Follow the call question `handle` waits in until it ends: an approve answered the program
    /// already; anything else declines its question.
    async fn follow_call(&self, dispatch: &Dispatch, handle: &str, invocation: InvocationId) {
        loop {
            let ended = match dispatch.actions.observe(&dispatch.caller, &invocation) {
                Some(state) => match state.update {
                    InvocationUpdate::Succeeded { .. }
                    | InvocationUpdate::Failed { .. }
                    | InvocationUpdate::Cancelled { .. } => true,
                    InvocationUpdate::Accepted { .. }
                    | InvocationUpdate::Progress { .. }
                    | InvocationUpdate::Item { .. } => false,
                },
                None => true,
            };
            if ended {
                self.ended(handle, invocation);
                return;
            }
            let Some(change) = dispatch.actions.next_change(&dispatch.caller, &invocation) else {
                tracing::warn!("the dispatch cannot tell when a question is answered");
                self.ended(handle, invocation);
                return;
            };
            if change.await.is_none() {
                tracing::warn!("the dispatch stopped telling when a question is answered");
                self.ended(handle, invocation);
                return;
            }
        }
    }

    /// The call question `handle` waited in under `invocation` ended. Unless an approve answered
    /// it, or it was raised again since, its program is told no.
    fn ended(&self, handle: &str, invocation: InvocationId) {
        let mut raised = self.inner.raised.lock_recover();
        if raised
            .get(handle)
            .is_some_and(|was| was.invocation == invocation)
        {
            raised.remove(handle);
            drop(raised);
            self.questions.decline(handle);
        }
    }

    /// Take what was put to the person under `handle` back: its program no longer asks it.
    fn withdraw(&self, dispatch: &Dispatch, handle: &str) {
        if self.inner.shown.lock_recover().remove(handle).is_some() {
            self.wake();
        }
        let Some(was) = self.inner.raised.lock_recover().remove(handle) else {
            return;
        };
        let dispatch = dispatch.clone();
        self.runtime.spawn(async move {
            let _ = tilo_failure::ignored!(
                "a question its program took back that ended already has nothing left to end",
                dispatch
                    .actions
                    .cancel(&dispatch.caller, &was.invocation)
                    .await
            );
        });
    }

    /// The person stopped the program that shows what `handle` is: it is told no.
    fn stop(&self, handle: &str) {
        if self.inner.shown.lock_recover().remove(handle).is_some() {
            self.questions.decline(handle);
        }
    }

    /// The person approved question `handle`, which waited in `invocation`: hand the program
    /// their answer.
    fn answer(
        &self,
        handle: &str,
        invocation: InvocationId,
        answered: Answered,
    ) -> Result<(), ActionCallError> {
        let gone = || ActionCallError::Refused {
            reason: crate::i18n::t("program-asks-gone"),
        };
        let (answer, grant) = {
            let raised = self.inner.raised.lock_recover();
            let was = raised
                .get(handle)
                .filter(|was| was.invocation == invocation)
                .ok_or_else(gone)?;
            let answered = (was.reply.answer(answered), was.grant.clone());
            drop(raised);
            answered
        };
        match answer {
            Ok(answer) => {
                let grant = grant.filter(|_| answer.choice.is_some_and(Grant::kept_by));
                self.inner.raised.lock_recover().remove(handle);
                if !self.questions.answer(handle, answer) {
                    return Err(gone());
                }
                if let Some(grant) = grant {
                    self.inner.granted.lock_recover().push(grant);
                    self.wake();
                }
                Ok(())
            }
            // The answer can't be taken as it is: the person is asked again, told why.
            Err(retry) => {
                let question = self
                    .inner
                    .raised
                    .lock_recover()
                    .remove(handle)
                    .map(|was| was.question)
                    .ok_or_else(gone)?;
                if let Some(dispatch) = self.inner.dispatch.get().cloned() {
                    self.raise(&dispatch, question, Some(retry.clone()));
                }
                Err(ActionCallError::Refused { reason: retry })
            }
        }
    }

    /// Wake the session's loop: a notice is due.
    fn wake(&self) {
        if let Some(wake) = self.inner.wake.get() {
            wake();
        }
    }

    /// What question `handle` shows, while it waits under `invocation`.
    fn facts(&self, handle: &str, invocation: InvocationId) -> Option<AskedFacts> {
        self.inner
            .raised
            .lock_recover()
            .get(handle)
            .filter(|was| was.invocation == invocation)
            .map(|was| was.facts.clone())
    }

    /// What shows now with nothing to answer, by handle.
    fn shown(&self) -> BTreeMap<String, Shown> {
        self.inner
            .shown
            .lock_recover()
            .iter()
            .map(|(handle, (_, shown))| (handle.clone(), shown.clone()))
            .collect()
    }

    /// The allowances given since the last look.
    fn take_granted(&self) -> Vec<Grant> {
        std::mem::take(&mut *self.inner.granted.lock_recover())
    }
}

impl ActionProvider for ProgramAsks {
    fn serves(&self, provider: &str) -> bool {
        provider == PROVIDER
    }

    /// Nothing here is offered to call: a question is raised by the session alone.
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        if request.target.provider != PROVIDER || request.target.address.action != ANSWER {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let handle = request.target.address.resource.as_str();
        let invocation = request.invocation;
        let Some(approval) = request.approval() else {
            // Nobody has said yes yet: the dispatch keeps the call as its asked record.
            return Pending::ready(match self.facts(handle, invocation) {
                Some(facts) => Err(ActionCallError::ConfirmationRequired(Box::new(facts))),
                None => Err(ActionCallError::Refused {
                    reason: crate::i18n::t("program-asks-gone"),
                }),
            });
        };
        let answered = Answered {
            choice: approval.choice,
            chosen: approval.chosen.clone(),
            files: approval.files.clone(),
            typed: approval.typed.clone(),
        };
        Pending::ready(self.answer(handle, invocation, answered).map(|()| {
            InvocationState::new(
                invocation,
                InvocationUpdate::Succeeded {
                    invocation,
                    output: ActionOutput::default(),
                },
            )
            .receipt()
        }))
    }

    /// A question's call ends as it is answered: its state is the dispatch's to keep.
    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}
