//! A workspace's activity, served to the windows a desktop hosts in process.
//!
//! A native session serves its activity model to apps through its broker
//! ([`wire::PROVIDER`]); a desktop that hosts its windows in process serves
//! the same provider here, under the same rules. Its reads are live views:
//! the desktop reads each one when it starts and again whenever what its
//! model serves moves ([`ActivityRequests::serve`]). Its writes run on the
//! desktop's thread when it next takes them ([`ActivityRequests::take`]).
//! What this device has seen is the person's own: an agent never marks it,
//! and nothing here reaches a shared record. Whether a change of the history
//! may be written back is the host's to say ([`crate::ActivityHost`]): this
//! door carries the write to the model, and a host that only shows a
//! history refuses it there.
//!
//! The windows ask during their frame, on the desktop's thread, and the
//! desktop answers on its next pass; until then an ask stays pending.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value};
use tilo_actions::remote::workspace_activity as wire;
use tilo_actions::remote::{
    ActionHostError, Caller, Completer, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Pending, ReceiptStatus, pending,
};
use tilo_actions::{ActionFailure, ActionOutput, ActorKind, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use crate::activity::{is_document_write, is_personal, is_read, refused_to_agent};

/// The activity model's reads, as the desktop holds it.
pub trait ActivityReads {
    /// Moves whenever one of its reads would read differently.
    fn revision(&self) -> u64;
    /// Read `action` with `arguments`.
    ///
    /// # Errors
    /// The action is no read, or its arguments were not understood.
    fn read(&mut self, action: &str, arguments: &Map<String, Value>) -> Result<Value, String>;
}

/// What a window asked of the activity model, for the desktop's thread.
pub enum ActivityAsk {
    /// Run write `action` with `arguments`; whether it ran, or why not.
    Act {
        action: String,
        arguments: Map<String, Value>,
        answer: Completer<Result<(), String>>,
    },
    /// The person began reading what is new.
    BeginSummary,
    /// They stopped: what they were shown counts as seen.
    EndSummary,
}

/// One live read a window follows.
struct Follow {
    caller: Caller,
    action: String,
    arguments: Map<String, Value>,
    update: InvocationUpdate,
    /// What it last read, so a read that reads the same sends nothing.
    last: Option<Value>,
    /// The served revision it was read at.
    read_at: Option<u64>,
    /// Waiting observes, woken whenever it may have moved.
    waiters: Vec<Completer<()>>,
}

impl Follow {
    fn finished(&self) -> bool {
        self.update.is_terminal()
    }

    fn sequence(&self) -> u64 {
        match self.update {
            InvocationUpdate::Item { sequence, .. } => sequence,
            _ => 0,
        }
    }
}

#[derive(Default)]
struct State {
    follows: HashMap<InvocationId, Follow>,
    asks: Vec<ActivityAsk>,
    /// The revision the model's reads were last served at: how a host knows
    /// that what an app follows may read differently.
    served: Option<u64>,
}

/// The windows' end: what [`super::DesktopSession`] routes to under
/// [`wire::PROVIDER`].
pub(super) struct Door {
    state: std::sync::Mutex<State>,
    woken: Arc<AtomicBool>,
}

/// The desktop's end of the activity its session serves.
#[derive(Clone)]
pub struct ActivityRequests {
    door: Arc<Door>,
}

/// Wake every observe in `waiters`, outside the lock they were kept under.
fn wake(waiters: Vec<Completer<()>>) {
    for waiter in waiters {
        waiter.complete(());
    }
}

/// Whether `caller` is the person at this device, rather than an agent, an
/// app or a Shortcut acting on their behalf.
fn is_person(caller: &Caller) -> bool {
    caller.actor().kind == ActorKind::Person
}

fn refused(reason: String) -> ActionHostError {
    ActionHostError::Refused { reason }
}

impl Door {
    pub(super) fn new(woken: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            state: std::sync::Mutex::default(),
            woken,
        })
    }

    pub(super) fn requests(self: &Arc<Self>) -> ActivityRequests {
        ActivityRequests {
            door: Arc::clone(self),
        }
    }

    fn ask(&self, ask: ActivityAsk) {
        self.state.lock_recover().asks.push(ask);
        self.woken.store(true, Ordering::Release);
    }

    /// Admit `request` from `caller`, a member of the workspace.
    pub(super) fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        let action = request.target.address.action;
        if request.target.address.resource != wire::RESOURCE {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let arguments = match request.argument {
            InvokeArgument::None => Map::new(),
            InvokeArgument::Json(arguments) => arguments,
            InvokeArgument::Text(_) => {
                return Pending::ready(Err(ActionHostError::InvalidRequest {
                    reason: "workspace activity takes typed arguments".into(),
                }));
            }
        };
        if !is_person(caller)
            && let Some(refusal) = refused_to_agent(&action, &arguments)
        {
            return Pending::ready(Err(refused(refusal)));
        }
        if is_read(&action) {
            return Pending::ready(self.follow(caller, invocation, action, arguments));
        }
        if !is_personal(&action) && !is_document_write(&action) && action != wire::OPEN {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let (answer, ran) = pending();
        self.ask(ActivityAsk::Act {
            action,
            arguments,
            answer,
        });
        ran.map(move |ran| {
            Ok(InvocationReceipt {
                invocation,
                status: match ran {
                    Ok(()) => ReceiptStatus::Completed {
                        output: ActionOutput::default(),
                    },
                    Err(message) => ReceiptStatus::Failed {
                        error: ActionFailure {
                            code: "activity_refused".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    },
                },
            })
        })
    }

    /// Start following read `action`.
    fn follow(
        &self,
        caller: &Caller,
        invocation: InvocationId,
        action: String,
        arguments: Map<String, Value>,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let mut state = self.state.lock_recover();
        if let Some(open) = state.follows.get(&invocation) {
            return if open.caller == *caller && open.action == action && open.arguments == arguments
            {
                Ok(InvocationState {
                    update: open.update.clone(),
                    untrusted: false,
                }
                .receipt())
            } else {
                Err(ActionHostError::InvocationConflict)
            };
        }
        // Only the person reading what is new makes it seen; an agent that
        // reads the summary changes nothing for them.
        if action == wire::SUMMARY && is_person(caller) {
            state.asks.push(ActivityAsk::BeginSummary);
        }
        state.follows.insert(
            invocation,
            Follow {
                caller: caller.clone(),
                action,
                arguments,
                update: InvocationUpdate::Accepted { invocation },
                last: None,
                read_at: None,
                waiters: Vec::new(),
            },
        );
        drop(state);
        self.woken.store(true, Ordering::Release);
        Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Accepted,
        })
    }

    pub(super) fn serves(&self, id: &InvocationId) -> bool {
        self.state.lock_recover().follows.contains_key(id)
    }

    pub(super) fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.state
            .lock_recover()
            .follows
            .get(id)
            .filter(|open| open.caller == *caller)
            .map(|open| InvocationState {
                update: open.update.clone(),
                untrusted: false,
            })
    }

    pub(super) fn next_change(&self, caller: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let (waiter, changed) = pending();
        self.state
            .lock_recover()
            .follows
            .get_mut(id)
            .filter(|open| open.caller == *caller && !open.finished())?
            .waiters
            .push(waiter);
        Some(changed)
    }

    pub(super) fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        let mut state = self.state.lock_recover();
        let open = state
            .follows
            .get_mut(id)
            .filter(|open| open.caller == *caller)
            .ok_or(ActionHostError::UnknownInvocation)?;
        if open.finished() {
            return Ok(());
        }
        open.update = InvocationUpdate::Cancelled {
            invocation: *id,
            message: None,
        };
        let waiters = std::mem::take(&mut open.waiters);
        let summary = open.action == wire::SUMMARY && is_person(caller);
        if summary {
            state.asks.push(ActivityAsk::EndSummary);
        }
        drop(state);
        self.woken.store(true, Ordering::Release);
        wake(waiters);
        Ok(())
    }
}

impl ActivityRequests {
    /// What the windows asked of the activity model since this was last
    /// taken, in the order they asked.
    #[must_use]
    pub fn take(&self) -> Vec<ActivityAsk> {
        std::mem::take(&mut self.door.state.lock_recover().asks)
    }
    /// Whether a window follows the activity or asked something of it.
    #[must_use]
    pub fn wanted(&self) -> bool {
        let state = self.door.state.lock_recover();
        !state.asks.is_empty() || state.follows.values().any(|open| !open.finished())
    }

    /// Ask the model to run write `action` with `arguments` on its thread;
    /// answered once it ran, or with why it did not.
    pub fn act(&self, action: &str, arguments: Map<String, Value>) -> Pending<Result<(), String>> {
        let (answer, ran) = pending();
        self.door.ask(ActivityAsk::Act {
            action: action.to_owned(),
            arguments,
            answer,
        });
        ran
    }

    /// Start following read `action` for `caller`, as a window or an app that
    /// reaches the model through the host does.
    ///
    /// # Errors
    /// The invocation already follows something else.
    pub fn follow(
        &self,
        caller: &Caller,
        invocation: InvocationId,
        action: &str,
        arguments: Map<String, Value>,
    ) -> Result<(), ActionHostError> {
        self.door
            .follow(caller, invocation, action.to_owned(), arguments)
            .map(|_| ())
    }

    /// What the follow `invocation` last read: its value, or why it failed
    /// for good. `None` until the model has read it.
    #[must_use]
    pub fn served(&self, invocation: &InvocationId) -> Option<Result<Value, String>> {
        let state = self.door.state.lock_recover();
        match &state.follows.get(invocation)?.update {
            InvocationUpdate::Item { value, .. } => Some(Ok(value.clone())),
            InvocationUpdate::Failed { error, .. } => Some(Err(error.message.clone())),
            _ => None,
        }
    }

    /// The revision the model last served its reads at: a host that tells apps
    /// when what they follow may read differently watches it.
    #[must_use]
    pub fn served_revision(&self) -> Option<u64> {
        self.door.state.lock_recover().served
    }

    /// Read every followed read `model` has moved since, and send each
    /// that reads differently as its next result; a read that fails ends
    /// its view. A read may move the model itself (it can narrow it): the
    /// revision after the reads is what they count as read at, so the next
    /// pass reads again only when something moved since.
    pub fn serve(&self, model: &mut dyn ActivityReads) {
        let now = model.revision();
        let due: Vec<(InvocationId, String, Map<String, Value>)> = {
            let mut state = self.door.state.lock_recover();
            state.follows.retain(|_, open| !open.finished());
            state
                .follows
                .iter()
                .filter(|(_, open)| open.read_at != Some(now))
                .map(|(id, open)| (*id, open.action.clone(), open.arguments.clone()))
                .collect()
        };
        if due.is_empty() {
            return;
        }
        let reads: Vec<_> = due
            .into_iter()
            .map(|(id, action, arguments)| (id, model.read(&action, &arguments)))
            .collect();
        let read_at = model.revision();
        let mut woken = Vec::new();
        let mut state = self.door.state.lock_recover();
        state.served = Some(read_at);
        for (invocation, value) in reads {
            let Some(open) = state.follows.get_mut(&invocation) else {
                continue;
            };
            if open.finished() {
                continue;
            }
            open.read_at = Some(read_at);
            match value {
                Ok(value) if open.last.as_ref() == Some(&value) => continue,
                Ok(value) => {
                    open.last = Some(value.clone());
                    open.update = InvocationUpdate::Item {
                        invocation,
                        sequence: open.sequence().wrapping_add(1),
                        value,
                    };
                }
                Err(message) => {
                    open.update = InvocationUpdate::Failed {
                        invocation,
                        error: ActionFailure {
                            code: "activity_unreadable".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    };
                }
            }
            woken.append(&mut open.waiters);
        }
        drop(state);
        wake(woken);
    }

    /// Wake every waiting observe, whether or not its view moved: the
    /// desktop calls this on each pass it serves, which bounds how long a
    /// change could go unseen.
    pub fn wake_waiting(&self) {
        let waiters: Vec<_> = self
            .door
            .state
            .lock_recover()
            .follows
            .values_mut()
            .flat_map(|open| std::mem::take(&mut open.waiters))
            .collect();
        wake(waiters);
    }

    /// Stop following `invocation` for `caller`: what it read is no longer
    /// being read. A window that had been reading what is new stops counting
    /// as reading it.
    ///
    /// # Errors
    /// `caller` follows nothing under `invocation`.
    pub fn cancel(
        &self,
        caller: &Caller,
        invocation: &InvocationId,
    ) -> Result<(), ActionHostError> {
        self.door.cancel(caller, invocation)
    }
}

/// A desktop's end of the activity its session serves, before anything
/// serves it: a session that serves the model itself, as a native one does,
/// makes this on its own.
#[must_use]
pub fn activity_requests(woken: Arc<AtomicBool>) -> ActivityRequests {
    Door::new(woken).requests()
}
