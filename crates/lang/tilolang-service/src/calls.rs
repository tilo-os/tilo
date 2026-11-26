//! The calls an app this service runs makes of another app's action, or of its own action a
//! person is asked about first: each goes through the device's dispatch as the calling app's, for
//! the session's person, the way its window's calls go ([`tilolang_tilo::dispatched`]). The
//! dispatch decides it, asks the person where it has to, and the run that made the call goes on
//! with the answer on the service's next poll.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tilo_actions::InvocationUpdate;
use tilo_actions::remote::{ActionCallError, Caller, InvokeRequest, ReceiptStatus};
use tilolang_runtime::ids::RunId;
use tilolang_tilo::call::Callees;
use tilolang_tilo::dispatched::{CallLink, Calls, Dispatched, answer_of};
use tilolang_tilo::host::Host;

/// How long one wait on the dispatch lasts before it is asked how the call stands again.
const WAIT: Duration = Duration::from_secs(60);

/// What the dispatch answered of each call as it ended, by the call's id: what the service's
/// next poll reads.
type Ended = Arc<Mutex<Vec<(String, ReceiptStatus)>>>;

/// The dispatch a service's apps call through, and the runs that wait on its answers.
pub(crate) struct OutCalls {
    dispatch: tilo_service::Dispatch,
    /// The workspace the calls are made in.
    workspace: String,
    wake: tilo_service::Wake,
    /// Each call sent, by its id, until it is answered.
    calls: Calls,
    /// The packages the calls were built from, which read their answers back.
    callees: RefCell<Rc<Callees>>,
    ended: Ended,
    /// The run that waits on each call, by the call's id.
    waiting: BTreeMap<String, RunId>,
}

impl OutCalls {
    pub(crate) fn new(
        dispatch: &tilo_service::Dispatch,
        workspace: &str,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            dispatch: Arc::clone(dispatch),
            workspace: workspace.to_owned(),
            wake,
            calls: Calls::default(),
            callees: RefCell::default(),
            ended: Ended::default(),
            waiting: BTreeMap::new(),
        }
    }

    /// Send what `host`'s runtime calls of other apps through the dispatch, as `app`'s calls for
    /// `person`: the packages it may call are the ones `host` holds.
    pub(crate) fn install(&self, host: &Host, app: &str, person: &str) {
        let callees = Rc::new(callees_of(host, &self.workspace));
        self.callees.replace(Rc::clone(&callees));
        let link = SessionLink {
            dispatch: Arc::clone(&self.dispatch),
            caller: Caller::local_app(app)
                .acting_for(person)
                .in_workspace(self.workspace.clone()),
            ended: Arc::clone(&self.ended),
            wake: Arc::clone(&self.wake),
        };
        host.runtime().set_foreign_invoker(Dispatched::new(
            self.workspace.clone(),
            callees,
            Rc::clone(&self.calls),
            link,
        ));
    }

    /// Keep the calls `host`'s last run stopped at, each with the run that waits on it.
    pub(crate) fn wait_on(&mut self, host: &Host) {
        while let Some(waiting) = host.take_waiting() {
            if let Some(run) = waiting.run {
                self.waiting.insert(waiting.call, run);
            }
        }
    }

    /// Go on with each run whose call the dispatch answered since the last poll. Whether any
    /// did.
    pub(crate) fn answer(&mut self, host: &Host) -> bool {
        let ended = std::mem::take(&mut *self.ended.lock().unwrap_or_else(PoisonError::into_inner));
        let mut ran = false;
        for (call, status) in ended {
            let invocation = self.calls.borrow_mut().remove(&call);
            let Some(answer) =
                answer_of(&self.callees.borrow(), &call, invocation.as_ref(), &status)
            else {
                continue;
            };
            let Some(run) = self.waiting.remove(&call) else {
                continue;
            };
            ran = true;
            match host.runtime().answer_call(&run, answer) {
                Ok(tilolang_runtime::RunOutcome::Waits { run, call }) => {
                    self.waiting.insert(call, run);
                }
                Ok(tilolang_runtime::RunOutcome::Done(_)) => {}
                Err(error) => {
                    tracing::warn!(%run, %error, "a run did not go on with its call's answer")
                }
            }
        }
        ran
    }
}

/// Every package the apps `host` runs may call: its own, the platform's packages they use, and
/// the apps it describes without running them.
fn callees_of(host: &Host, workspace: &str) -> Callees {
    let mut callees = Callees {
        workspace: Some(workspace.to_owned()),
        ..Callees::default()
    };
    for package in host.packages() {
        for app in &package.meta.uses {
            if let Ok(meta) = tilolang_tilo::platform_apps::platform_package(app) {
                callees.apps.insert(app.clone(), meta);
            }
        }
        callees
            .apps
            .insert(package.meta.app.clone(), package.meta.clone());
    }
    for meta in host.runtime().descriptions() {
        callees.apps.entry(meta.app.clone()).or_insert(meta);
    }
    callees
}

/// The session's link to the dispatch, as a call of the app's reaches it: sent off the session's
/// loop, followed until it ends, and its end handed to the service's next poll.
struct SessionLink {
    dispatch: tilo_service::Dispatch,
    caller: Caller,
    ended: Ended,
    wake: tilo_service::Wake,
}

impl CallLink for SessionLink {
    fn send(&self, _app: &str, request: InvokeRequest) -> Result<(), String> {
        let (dispatch, caller, ended, wake) = (
            Arc::clone(&self.dispatch),
            self.caller.clone(),
            Arc::clone(&self.ended),
            Arc::clone(&self.wake),
        );
        let call = request.invocation.0.to_string();
        std::thread::Builder::new()
            .name("tilolang-call".into())
            .spawn(move || {
                let status = followed(&*dispatch, &caller, request);
                ended
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push((call, status));
                wake();
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// How the call `request` ended at the dispatch, followed from its admission: while it runs, or a
/// person is being asked about it, its next move is waited for.
fn followed(
    dispatch: &(dyn tilo_actions::remote::ActionProvider + Send + Sync),
    caller: &Caller,
    request: InvokeRequest,
) -> ReceiptStatus {
    let unavailable = |reason: &str| ReceiptStatus::Rejected {
        error: ActionCallError::Unavailable {
            reason: reason.to_owned(),
        },
    };
    let id = request.invocation;
    let status = match dispatch.invoke(caller, request).wait_timeout(WAIT) {
        Ok(Some(Ok(receipt))) => receipt.status,
        Ok(Some(Err(error))) => ReceiptStatus::Rejected { error },
        Ok(None) | Err(_) => return unavailable("the device's dispatch did not answer the call"),
    };
    if !matches!(
        status,
        ReceiptStatus::Accepted
            | ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired { .. }
            }
    ) {
        return status;
    }
    loop {
        let Some(change) = dispatch.next_change(caller, &id) else {
            return unavailable("the device's dispatch does not follow the call");
        };
        let _ = change.wait_timeout(WAIT);
        let Some(state) = dispatch.observe(caller, &id) else {
            return unavailable("the device's dispatch lost the call");
        };
        match state.update {
            InvocationUpdate::Succeeded { output, .. } => {
                return ReceiptStatus::Completed { output };
            }
            InvocationUpdate::Failed { error, .. } => return ReceiptStatus::Failed { error },
            InvocationUpdate::Cancelled { message, .. } => {
                return ReceiptStatus::Cancelled { message };
            }
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => {}
        }
    }
}
