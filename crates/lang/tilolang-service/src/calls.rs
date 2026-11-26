//! The calls an app this service runs makes of another app's action, or of its own action a
//! person is asked about first: each goes over a connection the device's daemon vouches for as
//! the calling app, the way its window's calls go ([`tilolang_tilo::dispatched`]). The dispatch
//! decides it as it decides the window's, asks the person where it has to, and the run that made
//! the call goes on with the answer on the service's next poll.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};

use tilo_actions::remote::{InvokeRequest, ReceiptStatus};
use tilo_actions::{ActionOutput, ApiError};
use tilolang_runtime::delegate::Invoked;
use tilolang_runtime::ids::RunId;
use tilolang_tilo::call::Callees;
use tilolang_tilo::dispatched::{CallLink, Calls, Dispatched, answer_of};
use tilolang_tilo::host::Host;

/// What the dispatch answered of each call as it ended, by the call's id: what the service's
/// next poll reads.
type Ended = Arc<Mutex<Vec<(String, ReceiptStatus)>>>;

/// The connection a service's app calls over, and the runs that wait on its answers.
pub(crate) struct OutCalls {
    connect: tilo_service::ConnectApp,
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
    /// The invocation each call's run is, by the call's id: answered when the run ends.
    answers: BTreeMap<String, tilo_actions::remote::Completer<Result<ActionOutput, ApiError>>>,
}

impl OutCalls {
    pub(crate) fn new(
        connect: &tilo_service::ConnectApp,
        workspace: &str,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            connect: Rc::clone(connect),
            workspace: workspace.to_owned(),
            wake,
            calls: Calls::default(),
            callees: RefCell::default(),
            ended: Ended::default(),
            waiting: BTreeMap::new(),
            answers: BTreeMap::new(),
        }
    }

    /// Send what `host`'s runtime calls of other apps over `app`'s own connection, for
    /// `person`: the packages it may call are the ones `host` holds.
    pub(crate) fn install(&self, host: &Host, app: &str, person: &str) {
        let Some(from) = calls_from(host, app, &self.workspace, person) else {
            tracing::warn!(%app, workspace = %self.workspace, "the app's calls reach no record of it");
            return;
        };
        let callees = Rc::new(callees_of(host, &self.workspace));
        self.callees.replace(Rc::clone(&callees));
        let link = AppLink {
            from,
            connect: Rc::clone(&self.connect),
            open: RefCell::default(),
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

    /// Keep the calls `host`'s last run stopped at, each with the run that waits on it. The
    /// invocation that run is is answered once it ends: what the caller then hears, its refusal
    /// in the words the dispatch gave it included. `None` when the run did not stop.
    pub(crate) fn wait_on(&mut self, host: &Host) -> Option<Answer> {
        let mut last = None;
        while let Some(waiting) = host.take_waiting() {
            if let Some(run) = waiting.run {
                self.waiting.insert(waiting.call.clone(), run);
            }
            last = Some(waiting.call);
        }
        let call = last?;
        let (completer, answer) = tilo_actions::remote::pending();
        self.answers.insert(call, completer);
        Some(answer)
    }

    /// Go on with each run whose call the dispatch answered since the last poll. Whether any
    /// did.
    pub(crate) fn answer(&mut self, host: &Host) -> bool {
        let ended = std::mem::take(&mut *self.ended.lock().unwrap_or_else(PoisonError::into_inner));
        let mut ran = false;
        for (call, status) in ended {
            let invocation = self.calls.borrow_mut().remove(&call);
            let answer = answer_of(&self.callees.borrow(), &call, invocation.as_ref(), &status)
                .or_else(|| match &status {
                    // Where no one keeps the question for the person, the call ends here.
                    ReceiptStatus::Rejected { error } => Some(Invoked::Refused(error.said())),
                    _ => None,
                });
            let Some(answer) = answer else {
                continue;
            };
            let invocation = self.answers.remove(&call);
            let Some(run) = self.waiting.remove(&call) else {
                // The call was the whole of the work: its answer is the invocation's.
                if let Some(invocation) = invocation {
                    invocation.complete(match answer {
                        Invoked::Done(_) => Ok(ActionOutput::default()),
                        Invoked::Refused(reason) | Invoked::Failed(reason) => {
                            Err(ApiError::Failed(reason))
                        }
                        Invoked::Pending(_) => continue,
                    });
                }
                continue;
            };
            ran = true;
            let ended = match host.runtime().answer_call(&run, answer) {
                Ok(tilolang_runtime::RunOutcome::Waits { run, call }) => {
                    self.waiting.insert(call.clone(), run);
                    if let Some(invocation) = invocation {
                        self.answers.insert(call, invocation);
                    }
                    continue;
                }
                Ok(tilolang_runtime::RunOutcome::Done(_)) => Ok(ActionOutput::default()),
                // The dispatch refused what the run called: the run ended, and its invocation
                // ends refused in the words the dispatch gave.
                Err(tilolang_runtime::RuntimeError::Denied(reason)) => {
                    Err(ApiError::Failed(reason))
                }
                Err(error) => {
                    tracing::warn!(%run, %error, "a run did not go on with its call's answer");
                    Err(ApiError::Failed(error.to_string()))
                }
            };
            if let Some(invocation) = invocation {
                invocation.complete(ended);
            }
        }
        ran
    }
}

/// How an invocation whose run waits on a call ends, as its caller hears it.
pub(crate) type Answer = tilo_actions::remote::Pending<Result<ActionOutput, ApiError>>;

/// The record `app`'s calls for `person` in `workspace` are decided on: the one its window joins.
fn calls_from(
    host: &Host,
    app: &str,
    workspace: &str,
    person: &str,
) -> Option<tilo_service::CallsFrom> {
    use tilolang_tilo::meta::MetaExt as _;
    let package = host
        .packages()
        .into_iter()
        .find(|package| package.meta.app == app)?;
    let (cluster, document) =
        tilolang_tilo::dispatched::app_record(&package.meta, workspace.parse().ok()?, person);
    Some(tilo_service::CallsFrom {
        app: app.to_owned(),
        record: collab_types::channel_key::format(
            app,
            &cluster.to_string(),
            document.as_deref().unwrap_or(""),
        ),
        kind: package.meta.resource_type(),
    })
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
            if let Ok(meta) = tilolang_tilo::platform::platform_package(app) {
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

/// The app's link to its workspace: the connection the daemon vouches for as the app, opened at
/// its first call and again once it is gone. Each call ends off the session's loop, and its end
/// is handed to the service's next poll.
struct AppLink {
    from: tilo_service::CallsFrom,
    connect: tilo_service::ConnectApp,
    open: RefCell<Option<Rc<dyn tilo_service::AppCalls>>>,
    ended: Ended,
    wake: tilo_service::Wake,
}

impl AppLink {
    fn ender(&self, call: String) -> Box<dyn FnOnce(ReceiptStatus) + Send> {
        let (ended, wake) = (Arc::clone(&self.ended), Arc::clone(&self.wake));
        Box::new(move |status| {
            ended
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((call, status));
            wake();
        })
    }

    fn opened(&self) -> Result<Rc<dyn tilo_service::AppCalls>, String> {
        if let Some(open) = self.open.borrow().as_ref() {
            return Ok(Rc::clone(open));
        }
        let open = (self.connect)(&self.from)?;
        self.open.replace(Some(Rc::clone(&open)));
        Ok(open)
    }
}

impl CallLink for AppLink {
    fn send(&self, _app: &str, request: InvokeRequest) -> Result<(), String> {
        let call = request.invocation.0.to_string();
        let sent = self
            .opened()?
            .send(request.clone(), self.ender(call.clone()));
        if sent.is_ok() {
            return sent;
        }
        // A connection that ended (its daemon started again) is opened again for this call.
        self.open.replace(None);
        self.opened()?.send(request, self.ender(call))
    }
}
