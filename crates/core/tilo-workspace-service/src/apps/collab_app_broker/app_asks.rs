//! An app's own call that waits for its person's yes, kept as its asked record.
//!
//! An app calls its own actions as the person using it: the Browser raises each prompt a page
//! waits on (a site asking for the camera, a download, a sign-in) as its own call of a page
//! action, which asks first. Nothing on the app's screen answers it: on a device whose workspaces
//! keep asked records ([`crate::workspaces::Workspaces::asked_store`]), the call is kept as its
//! asked `tilo.invocations` record, naming the app by its name, and waits there under way. The
//! person answers it with `<record>#resolve` from any device; their approve hands the same call
//! back here, carrying it, and it is admitted again on the connection that made it, as the person
//! the connection was admitted for. A decline ends it cancelled, nothing of it run, and so does
//! a withdraw: the app's own cancel of the call, or the store's.
//!
//! Nothing is pushed to an app: it follows its call (`Observe`), and that answer waits here until
//! the call moves. A call that is admitted again is followed where it runs from then on.
//!
//! A host that keeps no asked records (no shell link, so not the device's daemon) answers the
//! call refused for want of the person's yes, as before.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tilo_actions::remote::asked::{AskedBy, AskedDispatch, AskedFacts, AskedRecords, RecordRef};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvocationReceipt, InvocationState,
    InvokeRequest, ReceiptStatus, RemoteActionMessage,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use super::AppBrokerState;
use super::action_provider::ConnectionActionProvider;

/// The name the asked store knows this broker's dispatch of apps' calls by.
const DISPATCH: &str = "apps";

/// How many calls that ended here are kept for their app to read how they ended.
const ENDED_KEPT: usize = 256;

/// What the app's cancel of a call that waits says on its record.
const NO_LONGER_ASKED: &str = "The app no longer asks";

/// A call kept here: who made it, on which app's connection, and how it stands.
struct Kept {
    app: String,
    caller: Caller,
    state: InvocationState,
    /// While it waits: the connection's provider an approve sends it again through, and its
    /// record.
    waiting: Option<(ConnectionActionProvider, RecordRef)>,
    /// The follows of it that wait for it to move.
    watching: Vec<tokio::sync::oneshot::Sender<()>>,
}

#[derive(Default)]
struct Calls {
    by_id: HashMap<InvocationId, Kept>,
    /// The calls that ended here, oldest first, so only the last [`ENDED_KEPT`] are kept.
    ended: VecDeque<InvocationId>,
}

impl Calls {
    /// The call `invocation` moved: it ends at `state`, or, with none, it left for the dispatch
    /// that runs it. Whoever follows it reads it again. What it waited with; `None`, and nothing
    /// changed, for a call that waits here no more.
    fn moved(
        &mut self,
        invocation: InvocationId,
        state: Option<InvocationState>,
    ) -> Option<(ConnectionActionProvider, RecordRef)> {
        let kept = self.by_id.get_mut(&invocation)?;
        let waiting = kept.waiting.take()?;
        for watcher in kept.watching.drain(..) {
            let _ = watcher.send(());
        }
        match state {
            Some(state) => {
                kept.state = state;
                self.ended.push_back(invocation);
                while self.ended.len() > ENDED_KEPT {
                    if let Some(oldest) = self.ended.pop_front() {
                        self.by_id.remove(&oldest);
                    }
                }
            }
            None => {
                self.by_id.remove(&invocation);
            }
        }
        Some(waiting)
    }
}

/// The asked records of the apps' calls this broker keeps, and the calls kept in them.
pub(super) struct AppAsks {
    /// The records and their store, once the device's workspaces keep one.
    records: OnceLock<(
        Arc<dyn AskedRecords>,
        Arc<tilo_actions::remote::asked::AskedStore>,
    )>,
    calls: Mutex<Calls>,
}

impl AppAsks {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            records: OnceLock::new(),
            calls: Mutex::default(),
        })
    }

    /// Where calls are kept: the store of the device's workspaces, as the shell's services
    /// reach them. `None` on a host that keeps none.
    fn records(self: &Arc<Self>, state: &AppBrokerState) -> Option<Arc<dyn AskedRecords>> {
        if let Some((records, _)) = self.records.get() {
            return Some(Arc::clone(records));
        }
        let workspaces = &state.services.shell.as_ref()?.workspaces;
        let store = workspaces.asked_store()?;
        let dispatch: Weak<dyn AskedDispatch> = Arc::downgrade(self) as Weak<dyn AskedDispatch>;
        let records = workspaces.told(store.for_dispatch(DISPATCH, dispatch));
        Some(Arc::clone(&self.records.get_or_init(|| (records, store)).0))
    }
}

/// How `invocation` ends without running: `message` says why.
fn cancelled(invocation: InvocationId, message: &str) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Cancelled {
            invocation,
            message: Some(message.to_owned()),
        },
    )
}

impl AppBrokerState {
    /// Keep `request`, which `caller` made on the connection of `app` and which `first`
    /// refused for want of the person's yes, as its asked record: answered waiting in it, and
    /// sent again through `provider` once the person approves. Any other answer, a call that
    /// carries an approve already, and a host that keeps no asked records, answer `first`.
    pub(super) fn keep_app_ask(
        &self,
        (provider, app): (&ConnectionActionProvider, &str),
        caller: &Caller,
        request: &InvokeRequest,
        first: RemoteActionMessage,
    ) -> RemoteActionMessage {
        let RemoteActionMessage::Receipt(InvocationReceipt {
            status:
                ReceiptStatus::Rejected {
                    error: ActionCallError::ConfirmationRequired(facts),
                },
            ..
        }) = &first
        else {
            return first;
        };
        if request.approval().is_some() {
            return first;
        }
        let Some(workspace) = caller.workspace().map(str::to_owned) else {
            return first;
        };
        let asks = self.app_asks.get_or_init(AppAsks::new);
        let Some(records) = asks.records(self) else {
            return first;
        };
        let mut facts: AskedFacts = facts.as_ref().clone();
        // Who asks is the app whose code made the call, as the person using it, by its name.
        if matches!(facts.by, AskedBy::Device(_)) {
            facts.by = AskedBy::App {
                app: app.to_owned(),
                title: self.app_title(app),
                author: None,
            };
        }
        let invocation = request.invocation;
        let status = match records.write_asked(&workspace, caller, request, &facts) {
            Ok(record) => {
                let state = InvocationState::new(invocation, record.waiting(invocation));
                let status = state.receipt().status;
                asks.calls.lock_recover().by_id.insert(
                    invocation,
                    Kept {
                        app: app.to_owned(),
                        caller: caller.clone(),
                        state,
                        waiting: Some((provider.clone(), record)),
                        watching: Vec::new(),
                    },
                );
                status
            }
            Err(why) => ReceiptStatus::Rejected {
                error: ActionCallError::Refused {
                    reason: format!(
                        "it waits for its person's yes, and could not be kept for them to \
                         answer: {why}"
                    ),
                },
            },
        };
        RemoteActionMessage::Receipt(InvocationReceipt { invocation, status })
    }

    /// The name the person knows app `app` by: its installed manifest's, else its id.
    fn app_title(&self, app: &str) -> String {
        self.services
            .launcher
            .as_ref()
            .and_then(|launcher| launcher.applications_for(app).into_iter().next())
            .map_or_else(|| app.to_owned(), |installed| installed.display_name)
    }

    /// Answer `message` of app `app`'s connection, made as `caller`, when it follows or cancels
    /// a call of theirs kept here: a follow is answered once the call moves, or as it stands
    /// after the wait it names; a cancel of a call that waits withdraws its record. `None` for
    /// anything else, which the dispatch answers.
    pub(super) fn answer_kept_app_call(
        &self,
        app: &str,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<super::wire::Answer<RemoteActionMessage>> {
        let asks = self.app_asks.get()?;
        let ours = |kept: &Kept| kept.app == app && kept.caller == *caller;
        match message {
            RemoteActionMessage::Observe {
                correlation,
                invocation,
                wait_ms,
                ..
            } => {
                let (correlation, invocation) = (*correlation, *invocation);
                let mut calls = asks.calls.lock_recover();
                let kept = calls.by_id.get_mut(&invocation).filter(|kept| ours(kept))?;
                let answer = move |state: InvocationState| RemoteActionMessage::Update {
                    invocation,
                    state: Some(state),
                    correlation: (correlation != 0).then_some(correlation),
                };
                let wait = (*wait_ms).filter(|_| kept.waiting.is_some()).map(|wait| {
                    std::time::Duration::from_millis(u64::from(wait))
                        .min(tilo_actions::remote::MAX_REMOTE_WAIT)
                });
                let Some(wait) = wait else {
                    return Some(Box::pin(std::future::ready(Ok(answer(kept.state.clone())))));
                };
                let (moved, goes_on) = tokio::sync::oneshot::channel();
                kept.watching.push(moved);
                drop(calls);
                let asks = Arc::clone(asks);
                Some(Box::pin(async move {
                    let _ = tokio::time::timeout(wait, goes_on).await;
                    // Admitted again, it is no longer read here: it goes on where it runs.
                    let state = asks
                        .calls
                        .lock_recover()
                        .by_id
                        .get(&invocation)
                        .map_or_else(
                            || {
                                InvocationState::new(
                                    invocation,
                                    InvocationUpdate::Accepted { invocation },
                                )
                            },
                            |kept| kept.state.clone(),
                        );
                    Ok(answer(state))
                }))
            }
            RemoteActionMessage::Cancel { invocation } => {
                let at = {
                    let calls = asks.calls.lock_recover();
                    let kept = calls.by_id.get(invocation).filter(|kept| ours(kept))?;
                    kept.waiting.as_ref().map(|(_, at)| at.clone())
                };
                // The store ends the call cancelled through this dispatch's withdrawn.
                if let (Some(at), Some((_, store))) = (at, asks.records.get()) {
                    store.withdraw(&at, NO_LONGER_ASKED);
                }
                Some(Box::pin(std::future::ready(Ok(
                    RemoteActionMessage::CancelResponse {
                        invocation: *invocation,
                        result: Ok(()),
                    },
                ))))
            }
            _ => None,
        }
    }
}

/// What a call that no longer waits here is refused another answer with.
fn not_waiting() -> ActionCallError {
    ActionCallError::Refused {
        reason: "The call no longer waits for this answer".into(),
    }
}

impl AskedDispatch for AppAsks {
    fn context(&self, _: InvocationId) -> serde_json::Value {
        // The connection that made it is what sends it again: once it is gone, nothing here
        // admits it, after a restart included.
        serde_json::Value::Null
    }

    fn readmit(
        &self,
        _: &Caller,
        request: InvokeRequest,
        _: &serde_json::Value,
    ) -> Result<(), ActionCallError> {
        let invocation = request.invocation;
        let mut calls = self.calls.lock_recover();
        let caller = calls
            .by_id
            .get(&invocation)
            .map(|kept| kept.caller.clone())
            .ok_or_else(not_waiting)?;
        let (provider, _) = calls.moved(invocation, None).ok_or_else(not_waiting)?;
        drop(calls);
        provider.invoke(&caller, request).then(move |answered| {
            if let Some(Err(error)) = answered {
                tracing::warn!(%error, ?invocation, "an app's approved call was not admitted again");
            }
        });
        Ok(())
    }

    fn declined(&self, _: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        self.calls.lock_recover().moved(
            invocation,
            Some(cancelled(invocation, tilo_actions::remote::asked::DECLINED)),
        );
        Ok(())
    }

    fn withdrawn(&self, _: &Caller, invocation: InvocationId, message: &str) {
        self.calls
            .lock_recover()
            .moved(invocation, Some(cancelled(invocation, message)));
    }
}
