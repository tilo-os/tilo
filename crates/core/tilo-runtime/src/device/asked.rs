//! A call that waits for its person's yes, kept by the device's one dispatch as its asked
//! invocation record.
//!
//! Every call an agent makes on this device reaches its workspace through the dispatch
//! ([`super::services::RuntimeActionProvider`]), under the agent's own key. One the workspace
//! refuses for want of the person's yes ([`ActionCallError::ConfirmationRequired`]) is written as
//! its asked record and waits there under way: the device's record of the invocation
//! ([`Invocations`]) reads it from here while it waits, and says it waits for its person. The
//! person answers it with `<record>#resolve`, from any device. An approve hands the call back
//! here, which sends it once more, byte for byte and under its id, as the agent that made it,
//! checked against the agent's grants as they stand then. A decline ends it, nothing of it run.
//! Stopping it before they answer withdraws the question ([`AskedStore::withdraw`]).
//!
//! The record keeps the whole call (target, arguments, id and caller) and the run it was made in
//! ([`Run`]), so a call that waits across the device's restart is sent again from it, in a run
//! admitted afresh for the same agent, under the key this device keeps for it.
//!
//! A call only the person's root signs, made on a device that keeps none, is capped
//! (`tilo_action_policy::Cap::ROOTLESS_DEVICE`): it waits in its record for a device of theirs that
//! keeps the root ([`asked::AT_ROOT`], [`DeviceAsked::hand_on`]). That device takes the record up
//! ([`DeviceAsked::kept_changed`]), asks the person once, runs it under its root through the
//! workspace service's one runner ([`AtRoot`]) and writes how it went into the record, which the
//! device that asked shows and then forgets.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::asked::{
    self, AskedBy, AskedDispatch, AskedFacts, AskedRecords, KeptAsk, Named,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Completer, Decided,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionTarget, ResourceAddress, pending,
};
use tilo_actions::{ActionFailure, InvocationId, InvocationUpdate};
use tilo_invocations::{Invocations, Route, TakeBack, take_back_run};
use tilo_lock::MutexRecover as _;
use tilo_remote_actions::asked::AskedStore;
use tilo_workspace_service::workspaces::at_root::{AtRoot, Certifying, RootKept, RootRun};

/// The name the store knows the device's dispatch by, across its restarts.
const DISPATCH: &str = "device";

/// The most calls whose end (declined, withdrawn, or refused when sent again) is kept for their
/// caller to read; the oldest goes first.
const ENDED_KEPT: usize = 1024;

/// How long a follow of a call that waits here is left before it looks again when nothing
/// moved: the bound a next change promises ([`ActionProvider::next_change`]), so a follow that
/// asked to be held at most so long is answered then, while its person may take hours.
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The run a waiting call was made in, as its record keeps it: what admits a run of the same
/// asker again after the device starts again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "run", deny_unknown_fields)]
pub(crate) enum Run {
    /// An agent of the person's, by its id and the name it was given, in `workspace`.
    #[serde(rename_all = "camelCase")]
    Agent {
        workspace: String,
        agent: String,
        name: String,
    },
    /// The person's own call in `workspace`: from their shell (a question a program outside
    /// Tilo asks them, which the shell raises), or pressed in an app's window (`pressed`). Sent
    /// again as the person, through their own dispatch here.
    #[serde(rename_all = "camelCase")]
    Own {
        workspace: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pressed: Option<asked::Pressed>,
    },
    /// The person's read of a record of `workspace` that the app `app` joins for them: it waits
    /// only while that join does, so it ends with the device ([`DeviceAsked::ask_read`]).
    #[serde(rename_all = "camelCase")]
    Read { workspace: String, app: String },
}

impl Run {
    fn workspace(&self) -> &str {
        match self {
            Self::Agent { workspace, .. }
            | Self::Own { workspace, .. }
            | Self::Read { workspace, .. } => workspace,
        }
    }

    /// Who asks, as the person reads it, where the run says it: an agent is its own, whatever
    /// the provider said. The person's own call shows whoever its provider names (a program,
    /// a device, an app).
    fn asker(&self) -> Option<AskedBy> {
        match self {
            Self::Agent { agent, name, .. } => Some(AskedBy::Agent(Named {
                id: agent.clone(),
                name: name.clone(),
            })),
            Self::Read { app, .. } => Some(AskedBy::App {
                app: app.clone(),
                title: app.clone(),
                author: None,
            }),
            Self::Own { .. } => None,
        }
    }
}

/// What admits `run` afresh: where its call is sent, and who it goes as there.
pub(crate) type Admits = Box<dyn Fn(&Run) -> Result<Route, String> + Send + Sync>;

/// A call that waits: the run it was made in, who follows it, where an approve sends it, and the
/// provider whose record holds the changes it already made. A read waits for its join instead
/// (`read`), which the person's answer lets through or refuses; nothing is sent.
struct Waiting {
    run: Run,
    caller: Caller,
    route: Route,
    at: String,
    read: Option<Completer<Decided>>,
}

/// The asked records of the device's dispatch, and the calls that wait in them.
pub(crate) struct DeviceAsked {
    me: Weak<Self>,
    store: Arc<AskedStore>,
    records: Arc<dyn AskedRecords>,
    /// Where a call only the person's root signs waits for a device that keeps it.
    at_root: Arc<dyn AskedRecords>,
    /// What runs those calls here, on a device that keeps the person's root.
    serving_at_root: std::sync::OnceLock<Arc<AtRoot>>,
    invocations: Arc<Invocations>,
    waiting: Mutex<HashMap<InvocationId, Waiting>>,
    /// The calls this dispatch handed on to a device that keeps the person's root, by their
    /// invocation: what it shows how it ended, whoever asked them.
    handed_on: Mutex<std::collections::HashSet<InvocationId>>,
    /// How each call this dispatch keeps stands while nothing of it runs: waiting, declined,
    /// withdrawn, or refused when it was sent again; with who it runs as, who alone reads it.
    standing: Arc<Mutex<Standing>>,
    /// The calls sent on that the provider took rather than answered, while they run: one that
    /// then stops for its person's yes is kept as their question once it is read here, as one
    /// refused for it at once is.
    followed: Mutex<HashMap<InvocationId, Followed>>,
    admits: Admits,
}

/// A call this dispatch sent on and follows while it runs: what keeps it, should it stop for its
/// person's yes, and where it is read once it ends.
struct Followed {
    run: Run,
    caller: Caller,
    route: Route,
    /// The call, as it was sent and as its asked record would keep it.
    call: InvokeRequest,
}

#[derive(Default)]
struct Standing {
    by_id: HashMap<InvocationId, (Caller, InvocationState)>,
    order: VecDeque<InvocationId>,
    /// Who waits for each one's next change.
    watching: HashMap<InvocationId, Vec<Completer<()>>>,
}

impl Standing {
    /// Have `state`'s invocation stand at it for `acting` to read, with nothing of it running.
    fn stand(standing: &Mutex<Self>, acting: &Caller, state: InvocationState) {
        let id = state.invocation();
        let mut standing = standing.lock_recover();
        if standing.by_id.insert(id, (acting.clone(), state)).is_none() {
            standing.order.push_back(id);
        }
        while standing.order.len() > ENDED_KEPT {
            if let Some(oldest) = standing.order.pop_front() {
                standing.by_id.remove(&oldest);
            }
        }
        let watching = standing.watching.remove(&id).unwrap_or_default();
        drop(standing);
        for watcher in watching {
            watcher.complete(());
        }
    }

    /// `id` no longer stands here: it runs where it was sent, and is read there.
    fn leave(standing: &Mutex<Self>, id: InvocationId) {
        let mut standing = standing.lock_recover();
        standing.by_id.remove(&id);
        standing.order.retain(|kept| *kept != id);
        let watching = standing.watching.remove(&id).unwrap_or_default();
        drop(standing);
        for watcher in watching {
            watcher.complete(());
        }
    }
}

impl DeviceAsked {
    /// The device's dispatch, writing its waiting calls through `store`, read through
    /// `invocations`, and admitting a run again with `admits`.
    pub(crate) fn new(
        store: Arc<AskedStore>,
        invocations: Arc<Invocations>,
        admits: Admits,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me: &Weak<Self>| {
            let dispatch: Weak<dyn AskedDispatch> = me.clone();
            // Nothing here runs a call kept for a device that keeps the root, until this device
            // serves them ([`DeviceAsked::serve_at_root`]): it is written for that device alone.
            let none: Weak<dyn AskedDispatch> = Weak::<Self>::new();
            Self {
                me: me.clone(),
                at_root: store.writes_for(asked::AT_ROOT, none),
                serving_at_root: std::sync::OnceLock::new(),
                records: store.for_dispatch(DISPATCH, dispatch),
                store,
                invocations,
                waiting: Mutex::default(),
                handed_on: Mutex::default(),
                standing: Arc::default(),
                followed: Mutex::default(),
                admits,
            }
        })
    }

    /// Withdraw what waited before the device started again on something that ended with it:
    /// a provider the person's shell served over its link (a question a program outside Tilo
    /// asked through it), which nothing serves until a shell links again, and whose program's
    /// call ended with the device; and a press in an app's window, or a read its join waited on,
    /// which closed with it. A call
    /// an agent or a shortcut made, to a provider the device serves itself, waits on.
    pub(crate) fn withdraw_what_ended(&self) {
        for (at, target, context) in self.store.asked_by(DISPATCH) {
            let pressed = matches!(
                serde_json::from_value::<Run>(context),
                Ok(Run::Own {
                    pressed: Some(_),
                    ..
                } | Run::Read { .. })
            );
            if runtime_collab::wire::shell::serves(&target.provider) {
                self.store.withdraw(&at, PROGRAM_GONE);
            } else if pressed {
                self.store.withdraw(&at, WINDOW_GONE);
            }
        }
    }

    /// Keep `request`, which `caller` made in `run` and only the person's root signs, as its
    /// asked record in the workspace for a device of theirs that keeps that root, showing
    /// `facts`: it waits under way here until that device ran it, as the record then says. Where
    /// the workspace keeps its record already (`kept`: the call sent again under its id, also
    /// after a restart), the record is followed as it stands, never written again: the person is
    /// asked once.
    pub(crate) fn hand_on(
        &self,
        run: &Run,
        caller: &Caller,
        request: &InvokeRequest,
        (mut facts, kept): (AskedFacts, Option<KeptAsk>),
    ) -> Result<InvocationReceipt, ActionCallError> {
        // What this device asked on the caller's behalf is its own to show how it ended.
        self.handed_on.lock_recover().insert(request.invocation);
        if let Some(ask) = kept {
            return Ok(self.follow(caller, &ask));
        }
        if let Some(by) = run.asker() {
            facts.by = by;
        }
        let invocation = request.invocation;
        // The same call followed again under its id is the one that waits, or how it ended.
        let receipt = self
            .standing
            .lock_recover()
            .by_id
            .get(&invocation)
            .map(|(_, state)| state.receipt());
        if let Some(receipt) = receipt {
            return Ok(receipt);
        }
        let workspace = run.workspace();
        let program = matches!(facts.by, AskedBy::Program { .. });
        let at = self
            .at_root
            .write_asked(workspace, caller, request, &facts)
            .map_err(|why| ActionCallError::Refused {
                reason: format!(
                    "it waits for a device of yours that keeps your identity, and could not be \
                     kept for it: {why}"
                ),
            })?;
        // A program's question that no device of theirs answers in time is refused, never left
        // waiting on a host nobody is at.
        if program {
            let (store, at) = (Arc::clone(&self.store), at.clone());
            tilo_rt::app().handle().spawn(async move {
                tokio::time::sleep(PROGRAM_ANSWERED_WITHIN).await;
                store.withdraw(&at, &crate::i18n::t("asked-program-unanswered"));
            });
        }
        Ok(self.follow(caller, &self.store.kept_ask(&at).ok_or_else(not_waiting)?))
    }

    /// Follow `ask`, the record of a call `caller` handed on to a device that keeps the person's
    /// root, as it stands: under way until that device wrote how it ended, which this device then
    /// shows, forgetting the record.
    fn follow(&self, caller: &Caller, ask: &KeptAsk) -> InvocationReceipt {
        let invocation = ask.request.invocation;
        // Read from here while it waits, also after the device started again.
        if self.invocations.get(caller, &invocation).is_none() {
            self.invocations.record(
                caller,
                caller,
                &ask.request,
                Some(&ask.at.workspace),
                self.route(caller),
            );
            self.invocations.note_waiting(&invocation);
        }
        let ended = ask.outcome.clone().filter(InvocationUpdate::is_terminal);
        let state = InvocationState::new(
            invocation,
            ended.clone().unwrap_or(InvocationUpdate::Progress {
                invocation,
                sequence: 1,
                phase: asked::AT_ROOT.to_owned(),
                completed: None,
                total: None,
                message: Some(WAITS_FOR_ROOT.to_owned()),
            }),
        );
        let receipt = state.receipt();
        // Forgotten before it shows: whoever reads that it ended finds no record left.
        if ended.is_some() {
            self.handed_on.lock_recover().remove(&invocation);
            self.store.settled(&ask.at);
        }
        Standing::stand(&self.standing, caller, state);
        receipt
    }

    /// Whether this device runs what waits for a device that keeps the person's root: it keeps
    /// one or more of them.
    #[cfg(test)]
    pub(crate) fn serves_at_root(&self) -> bool {
        self.serving_at_root.get().is_some()
    }

    /// Run here what waits for a device that keeps the person's root, through the workspace
    /// service's runner over `workspaces`, with `tickets` for the device's own root-only actions:
    /// this device keeps it.
    pub(crate) fn serve_at_root(
        &self,
        workspaces: &Arc<super::workspaces::ServiceWorkspaces>,
        tickets: super::ticket::Tickets,
    ) {
        let at_root = AtRoot::new(
            &self.store,
            workspaces.copies(),
            root_kept(workspaces, tickets),
        );
        // A device that already serves them keeps the runner that runs: registering a second
        // one would leave the records pointing at a runner this call drops.
        if self.serving_at_root.set(Arc::clone(&at_root)).is_err() {
            return;
        }
        // The workspaces here know they keep the root by it: a change only that root's device
        // makes is made here, signed by it.
        workspaces.copies().runs_at_root(&at_root);
        at_root.serve();
    }

    /// `asks`, the calls the workspaces here keep for a device that keeps the person's root, as
    /// they stand now: a device that keeps it takes up those that wait, for its person to answer
    /// once; the device that asked shows how each it handed on ended, and forgets its record.
    pub(crate) fn kept_changed(&self, asks: Vec<KeptAsk>) {
        if let Some(at_root) = self.serving_at_root.get() {
            at_root.kept_changed(asks.clone());
        }
        let here = self.store.here().map(str::to_owned);
        for ask in asks {
            let caller = ask.caller.clone().restored();
            // The record this device asked for is its own to show, whoever asked it: the person
            // at this device, or an agent of theirs it handed the ask on for. One that keeps the
            // root takes the others up, and shows them none.
            let mine = caller.device_public_key().map(hex::encode) == here
                || self
                    .handed_on
                    .lock_recover()
                    .contains(&ask.request.invocation);
            if ask
                .outcome
                .as_ref()
                .is_some_and(InvocationUpdate::is_terminal)
                && mine
            {
                self.follow(&caller, &ask);
            }
        }
    }

    /// The store its asked records are in: what admits the approve it hands back.
    pub(crate) fn store(&self) -> &AskedStore {
        &self.store
    }

    /// Send the call `request` that `caller` made in `run` to `route`: one refused there for
    /// want of the person's yes is kept as its asked record, and waits there under way; anything
    /// else is answered as `route` answers it. The device's record of the invocation is
    /// `caller`'s, written before it is sent.
    pub(crate) fn send(
        self: &Arc<Self>,
        run: Run,
        caller: &Caller,
        route: Route,
        request: InvokeRequest,
        asks: Option<AskedFacts>,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        // A call its contract makes ask is kept here, before anything runs.
        if let Some(facts) = asks {
            let waiting = Waiting {
                at: request.target.provider.clone(),
                run,
                caller: caller.clone(),
                route,
                read: None,
            };
            return Pending::ready(self.keep(waiting, &request, facts));
        }
        let (provider, acting) = route.clone();
        self.invocations.record(
            caller,
            &acting,
            &request,
            Some(run.workspace()),
            route.clone(),
        );
        let me = Arc::clone(self);
        let (call, caller) = (request.clone(), caller.clone());
        provider
            .invoke(&acting, request)
            .map(move |answered| match refused_for_a_yes(&answered) {
                Some(facts) if call.approval().is_none() => me.keep(
                    Waiting {
                        run,
                        caller,
                        route,
                        at: call.target.provider.clone(),
                        read: None,
                    },
                    &call,
                    facts,
                ),
                // Taken, it may yet stop for its person's yes: an app's action finds that it
                // needs it as it runs. It is read here while it runs, so that it is asked.
                _ => {
                    if call.approval().is_none()
                        && matches!(
                            &answered,
                            Ok(InvocationReceipt {
                                status: ReceiptStatus::Accepted,
                                ..
                            })
                        )
                    {
                        me.follow_taken(Followed {
                            run,
                            caller,
                            route,
                            call,
                        });
                    }
                    answered
                }
            })
    }

    /// Read `followed`, a call sent on and taken, here while it runs.
    fn follow_taken(&self, followed: Followed) {
        let invocation = followed.call.invocation;
        let acting = followed.route.1.clone();
        self.invocations.record(
            &followed.caller,
            &acting,
            &followed.call,
            Some(followed.run.workspace()),
            self.route(&acting),
        );
        self.followed.lock_recover().insert(invocation, followed);
    }

    /// `id`, a call followed here, as it stands where it runs: kept as its person's question when
    /// it stopped for their yes, and read where it ran from now on once it ended otherwise.
    fn followed_state(&self, acting: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let route = self
            .followed
            .lock_recover()
            .get(id)
            .map(|f| f.route.clone())?;
        if !same_asker(&route.1, acting) {
            return None;
        }
        let state = route.0.observe(&route.1, id)?;
        let receipt = state.receipt();
        if let Some(facts) = refused_for_a_yes(&Ok(receipt.clone())) {
            // Whoever reads it first keeps it; a read after that finds it standing.
            let taken = self.followed.lock_recover().remove(id);
            if let Some(followed) = taken {
                let Followed {
                    run,
                    caller,
                    route,
                    call,
                } = followed;
                let at = call.target.provider.clone();
                let waiting = Waiting {
                    run,
                    caller,
                    route,
                    at,
                    read: None,
                };
                if let Err(error) = self.keep(waiting, &call, facts) {
                    Standing::stand(&self.standing, acting, failed(*id, &error));
                }
            }
            return self.observe(acting, id);
        }
        if receipt.status != ReceiptStatus::Accepted
            && let Some(followed) = self.followed.lock_recover().remove(id)
        {
            // Ended: it is read where it ran.
            self.invocations.record(
                &followed.caller,
                &followed.route.1,
                &followed.call,
                Some(followed.run.workspace()),
                followed.route.clone(),
            );
        }
        Some(state)
    }

    fn keep(
        &self,
        waiting: Waiting,
        request: &InvokeRequest,
        mut facts: AskedFacts,
    ) -> Result<InvocationReceipt, ActionCallError> {
        let invocation = request.invocation;
        if let Some(by) = waiting.run.asker() {
            facts.by = by;
        }
        let (caller, acting) = (waiting.caller.clone(), waiting.route.1.clone());
        let workspace = waiting.run.workspace().to_owned();
        // What an approve sends it again with, there before its record is written.
        self.waiting.lock_recover().insert(invocation, waiting);
        let record = match self
            .records
            .write_asked(&workspace, &acting, request, &facts)
        {
            Ok(record) => record,
            Err(why) => {
                self.waiting.lock_recover().remove(&invocation);
                return Err(ActionCallError::Refused {
                    reason: format!(
                        "it waits for its person's yes, and could not be kept for them to \
                         answer: {why}"
                    ),
                });
            }
        };
        let state = InvocationState::new(invocation, record.waiting(invocation));
        let receipt = state.receipt();
        Standing::stand(&self.standing, &acting, state);
        // Read from here while it waits, and listed as waiting for its person: synchronously,
        // so the next read sees it.
        self.invocations.record(
            &caller,
            &acting,
            request,
            Some(&workspace),
            self.route(&acting),
        );
        self.invocations.note_waiting(&invocation);
        Ok(receipt)
    }

    /// This dispatch as where `acting`'s call runs while it waits here.
    fn route(&self, acting: &Caller) -> Route {
        let provider: Arc<dyn ActionProvider + Send + Sync> = match self.me.upgrade() {
            Some(me) => me,
            None => Arc::new(Gone),
        };
        (provider, acting.clone())
    }

    /// The call kept from before the device started again, in a run admitted afresh from its
    /// record for `restored`, who made it.
    fn admitted_again(
        &self,
        restored: &Caller,
        request: &InvokeRequest,
        context: &Value,
    ) -> Result<Waiting, ActionCallError> {
        let run: Run = serde_json::from_value(context.clone()).map_err(|_| not_waiting())?;
        // A read waited for the join that asked it, which ended with the device.
        if matches!(run, Run::Read { .. }) {
            return Err(not_waiting());
        }
        let route = (self.admits)(&run).map_err(|why| ActionCallError::Refused {
            reason: format!("the run that asked could not be admitted again: {why}"),
        })?;
        // Only for the same asker, under the same key.
        if !same_asker(&route.1, restored) {
            return Err(not_waiting());
        }
        Ok(Waiting {
            run,
            caller: restored.clone(),
            route,
            at: request.target.provider.clone(),
            read: None,
        })
    }
}

impl DeviceAsked {
    /// Send `waiting`'s call where it runs from now on, its run admitted, and read how it ends
    /// there.
    fn send_again(&self, waiting: Waiting, request: InvokeRequest) {
        let Waiting {
            run,
            caller,
            route,
            read,
            ..
        } = waiting;
        // A read is let through to the join that waits for it; nothing is sent.
        if let Some(read) = read {
            let invocation = request.invocation;
            Standing::stand(
                &self.standing,
                &route.1,
                InvocationState::new(
                    invocation,
                    InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    },
                ),
            );
            read.complete(Ok(()));
            return;
        }
        // Under way here until where it is sent takes it: whoever follows it meanwhile reads
        // that it runs, never that it is unknown.
        let invocation = request.invocation;
        Standing::stand(
            &self.standing,
            &route.1,
            InvocationState::new(invocation, InvocationUpdate::Accepted { invocation }),
        );
        self.invocations.record(
            &caller,
            &route.1,
            &request,
            Some(run.workspace()),
            self.route(&route.1),
        );
        let (provider, acting) = route;
        // The person's yes admits it again as whoever asked, after the turn or session it was
        // asked in ended too: it no longer runs under that run's end.
        let acting = if acting.execution_id().is_some() && !acting.execution_is_live() {
            acting.kept().restored()
        } else {
            acting
        };
        let standing = Arc::downgrade(&self.standing);
        let (invocations, me) = (Arc::clone(&self.invocations), self.me.clone());
        let (sent, workspace) = (request.clone(), run.workspace().to_owned());
        provider
            .clone()
            .invoke(&acting, request)
            .then(move |answered| {
                // Refused at its admission (the grants no longer reach it): it ran nothing, and its
                // caller reads why, here.
                let error = match answered {
                    Some(
                        Ok(InvocationReceipt {
                            status: ReceiptStatus::Rejected { error },
                            ..
                        })
                        | Err(error),
                    ) => error,
                    None => ActionCallError::Unavailable {
                        reason: "the workspace stopped before it answered".into(),
                    },
                    // Taken: it runs where it was sent from now on, and is read there.
                    Some(Ok(_)) => {
                        invocations.record(
                            &caller,
                            &acting,
                            &sent,
                            Some(&workspace),
                            (provider, acting.clone()),
                        );
                        if let Some(standing) = standing.upgrade() {
                            Standing::leave(&standing, invocation);
                        }
                        return;
                    }
                };
                let state = failed(invocation, &error);
                let (Some(standing), Some(me)) = (standing.upgrade(), me.upgrade()) else {
                    return;
                };
                Standing::stand(&standing, &acting, state);
                if let Some(recorded) = invocations.get(&caller, &invocation) {
                    invocations.record(
                        &caller,
                        &acting,
                        &recorded.request,
                        recorded.workspace.as_deref(),
                        me.route(&acting),
                    );
                }
            });
    }
}

/// How `invocation` ends when it was refused at its admission: `error` says why.
fn failed(invocation: InvocationId, error: &ActionCallError) -> InvocationState {
    InvocationState::new(
        invocation,
        InvocationUpdate::Failed {
            invocation,
            error: ActionFailure {
                code: error.code().to_owned(),
                message: error.to_string(),
                details: None,
                retryable: false,
            },
        },
    )
}

/// Whether `a` and `b` are the same asker: the same person's, under the same key and agent.
fn same_asker(a: &Caller, b: &Caller) -> bool {
    a.user_public_key() == b.user_public_key()
        && a.device_public_key() == b.device_public_key()
        && a.acting().agent_id() == b.acting().agent_id()
}

/// What `answered` waits for, when it is a refusal for want of the person's yes.
fn refused_for_a_yes(answered: &Result<InvocationReceipt, ActionCallError>) -> Option<AskedFacts> {
    match answered {
        Err(ActionCallError::ConfirmationRequired(facts))
        | Ok(InvocationReceipt {
            status:
                ReceiptStatus::Rejected {
                    error: ActionCallError::ConfirmationRequired(facts),
                },
            ..
        }) => Some(facts.as_ref().clone()),
        _ => None,
    }
}

/// What a call that no longer waits here is refused another answer with.
fn not_waiting() -> ActionCallError {
    ActionCallError::Refused {
        reason: "The call no longer waits for this answer".into(),
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

impl AskedDispatch for DeviceAsked {
    fn context(&self, invocation: InvocationId) -> Value {
        self.waiting
            .lock_recover()
            .get(&invocation)
            .and_then(|waiting| serde_json::to_value(&waiting.run).ok())
            .unwrap_or(Value::Null)
    }

    fn pressed(&self, invocation: InvocationId) -> Option<asked::Pressed> {
        match &self.waiting.lock_recover().get(&invocation)?.run {
            Run::Own { pressed, .. } => pressed.clone(),
            Run::Agent { .. } | Run::Read { .. } => None,
        }
    }

    fn readmit(
        &self,
        restored: &Caller,
        request: InvokeRequest,
        context: &Value,
    ) -> Result<(), ActionCallError> {
        let remembered = self.waiting.lock_recover().remove(&request.invocation);
        let waiting = match remembered {
            Some(waiting) => waiting,
            // Admitting the run again waits on the workspaces: never on the app runtime's own
            // thread, whose tasks would all stop behind that wait. Asked there, the run is
            // admitted and sent from a thread of its own, and a refusal is how the call stands.
            None if tilo_rt::app().is_runtime_thread() => {
                let me = self.me.upgrade().ok_or_else(not_waiting)?;
                let (restored, context) = (restored.clone(), context.clone());
                tilo_rt::app().handle().spawn_blocking(move || {
                    let invocation = request.invocation;
                    match me.admitted_again(&restored, &request, &context) {
                        Ok(waiting) => me.send_again(waiting, request),
                        Err(error) => {
                            Standing::stand(&me.standing, &restored, failed(invocation, &error));
                        }
                    }
                });
                return Ok(());
            }
            None => self.admitted_again(restored, &request, context)?,
        };
        self.send_again(waiting, request);
        Ok(())
    }

    fn declined(&self, _: &Caller, invocation: InvocationId) -> Result<(), ActionCallError> {
        let Some(waiting) = self.waiting.lock_recover().remove(&invocation) else {
            // Kept from before the device started again: nothing of it runs here to end.
            return Ok(());
        };
        Standing::stand(
            &self.standing,
            &waiting.route.1,
            cancelled(invocation, asked::DECLINED),
        );
        if let Some(read) = waiting.read {
            read.complete(Err(ActionCallError::Refused {
                reason: READ_DECLINED.into(),
            }));
            return Ok(());
        }
        // What the run wrote before it asked is taken back, keyed by its invocation: the same
        // take-back `<record>#undo` runs, at the provider whose record holds it.
        let at = TakeBack {
            at: &waiting.at,
            root: None,
            near: None,
        };
        drop(take_back_run(
            &waiting.route,
            &at,
            &invocation.0.to_string(),
            None,
        ));
        Ok(())
    }

    fn withdrawn(&self, _: &Caller, invocation: InvocationId, message: &str) {
        let waiting = self.waiting.lock_recover().remove(&invocation);
        if let Some(waiting) = waiting {
            Standing::stand(
                &self.standing,
                &waiting.route.1,
                cancelled(invocation, message),
            );
            if let Some(read) = waiting.read {
                read.complete(Err(ActionCallError::Refused {
                    reason: message.to_owned(),
                }));
            }
        }
    }
}

/// The device's own root-only actions, run with `tickets`, and the agent keys its person's root
/// certifies in `workspaces`, as the workspace service's runner runs them on a device that keeps
/// that root.
pub(crate) fn root_kept(
    workspaces: &Arc<super::workspaces::ServiceWorkspaces>,
    tickets: super::ticket::Tickets,
) -> Arc<dyn RootKept> {
    Arc::new(DeviceRoot {
        tickets,
        workspaces: Arc::downgrade(workspaces),
    })
}

/// What [`root_kept`] answers.
struct DeviceRoot {
    tickets: super::ticket::Tickets,
    workspaces: Weak<super::workspaces::ServiceWorkspaces>,
}

impl RootKept for DeviceRoot {
    fn mark(&self, person: [u8; 32], device: [u8; 32]) -> Option<[u8; 64]> {
        use tilo_user_identity::StatementSigner as _;
        let record = super::services::root_record_for_person(person)
            .inspect_err(
                |error| tracing::warn!(%error, "the root this device keeps is not here to sign"),
            )
            .ok()?;
        let root = tilo_identity::store::load_from_path(record)
            .inspect_err(|error| tracing::warn!(%error, "the root this device keeps does not read"))
            .ok()?;
        root.sign_digest(&tilo_user_identity::root_holder::root_holder_digest(
            &device,
        ))
        .inspect_err(|error| tracing::warn!(%error, "the root this device keeps did not sign"))
        .ok()
    }

    fn runs(&self, request: &InvokeRequest) -> bool {
        super::ticket::Tickets::root_only(request)
    }

    fn run(&self, request: InvokeRequest, caller: Caller) -> RootRun {
        let tickets = self.tickets.clone();
        Box::pin(async move { tickets.run_to_end(&request, &caller).await })
    }

    fn certify_agent(
        &self,
        person: [u8; 32],
        key: [u8; 32],
        workspace: &str,
        agent: &str,
        name: &str,
    ) -> Certifying {
        use tilo_app_host::agents::agent_keys::AgentGrants as _;

        let grants = super::signing::GrantsOf {
            workspaces: self.workspaces.clone(),
        };
        let (workspace, agent, name) = (workspace.to_owned(), agent.to_owned(), name.to_owned());
        Box::pin(async move {
            let workspace = tilo_server_protocol::WorkspaceId::parse(&workspace)
                .ok_or("the workspace is named by no workspace's id")?;
            tokio::task::spawn_blocking(move || {
                grants.certify(person, key, workspace, &agent, &name)
            })
            .await
            .map_err(|stopped| stopped.to_string())?
        })
    }
}

/// What a call that waits for a device of the person's that keeps their root says meanwhile.
/// How long a program's question waits for a device of the person's to answer it.
const PROGRAM_ANSWERED_WITHIN: std::time::Duration = std::time::Duration::from_secs(300);

const WAITS_FOR_ROOT: &str = "Waiting for a device of yours that keeps your identity";

/// What a question whose program stopped waiting with the device says, once withdrawn.
pub(crate) const PROGRAM_GONE: &str = "The program that asked is no longer running";

/// What an app's join is refused with when the person declined its read.
const READ_DECLINED: &str = "The person declined this read";

impl asked::AskedReads for DeviceAsked {
    fn ask_read(
        &self,
        caller: &Caller,
        app: &str,
        address: &ResourceAddress,
        facts: AskedFacts,
    ) -> Pending<Decided> {
        let request = InvokeRequest {
            asks: false,
            on_behalf_of: None,
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: address.app.clone(),
                address: ActionAddress::new(&address.record, asked::READ),
            },
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        };
        let (read, decided) = pending();
        let waiting = Waiting {
            run: Run::Read {
                workspace: address.workspace.clone(),
                app: app.to_owned(),
            },
            caller: caller.clone(),
            route: self.route(caller),
            at: address.app.clone(),
            read: Some(read),
        };
        match self.keep(waiting, &request, facts) {
            Ok(_) => decided,
            Err(refused) => Pending::ready(Err(refused)),
        }
    }
}

/// What a question pressed in a window that closed with the device says, once withdrawn.
pub(crate) const WINDOW_GONE: &str = "The window it was asked in is no longer open";

/// What stopping a waiting call says it ended with.
const STOPPED: &str = "stopped before its person answered";

/// While a call waits here, the device's record of it reads and stops it here.
impl ActionProvider for DeviceAsked {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, acting: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let standing = self.standing.lock_recover();
        let Some((runs_as, state)) = standing.by_id.get(id) else {
            drop(standing);
            return self.followed_state(acting, id);
        };
        let state = same_asker(runs_as, acting).then(|| state.clone());
        drop(standing);
        state
    }

    fn next_change(&self, acting: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        let followed = self
            .followed
            .lock_recover()
            .get(id)
            .filter(|followed| same_asker(&followed.route.1, acting))
            .map(|followed| followed.route.clone());
        if let Some((provider, runs_as)) = followed {
            return provider.next_change(&runs_as, id);
        }
        let mut standing = self.standing.lock_recover();
        let (runs_as, _) = standing.by_id.get(id)?;
        if !same_asker(runs_as, acting) {
            return None;
        }
        let (watcher, change) = pending();
        let watching = standing.watching.entry(*id).or_default();
        // A follow that looked again on its own already went: its watcher waits for nobody.
        watching.retain(|watcher| !watcher.is_abandoned());
        watching.push(watcher);
        drop(standing);
        Some(change.or_after(LOOK_AGAIN))
    }

    /// Stopping a call that waits withdraws its question: its person is asked no more.
    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        // A call that runs where it was sent is stopped there.
        let followed = self
            .followed
            .lock_recover()
            .get(id)
            .map(|followed| followed.route.clone());
        if let Some((provider, runs_as)) = followed {
            return provider.cancel(&runs_as, id);
        }
        if !self.waiting.lock_recover().contains_key(id) {
            // A call this device handed on waits at a device that keeps the person's root:
            // stopping it withdraws the question there, and its caller reads that it ended.
            if !self.handed_on.lock_recover().contains(id) {
                return Pending::ready(Err(ActionCallError::NotCancellable {
                    reason: "it has ended".into(),
                }));
            }
            let Some(at) = self.store.record(&id.0.to_string()) else {
                return Pending::ready(Err(ActionCallError::UnknownInvocation));
            };
            self.store.withdraw(&at, STOPPED);
            self.handed_on.lock_recover().remove(id);
            Standing::stand(&self.standing, caller, cancelled(*id, STOPPED));
            return Pending::ready(Ok(()));
        }
        let Some(at) = self.store.record(&id.0.to_string()) else {
            return Pending::ready(Err(ActionCallError::UnknownInvocation));
        };
        self.store.withdraw(&at, STOPPED);
        Pending::ready(Ok(()))
    }
}

/// Where a call is read once the dispatch that kept it stopped.
struct Gone;

impl ActionProvider for Gone {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, _: &Caller, _: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

#[cfg(test)]
#[path = "asked_tests.rs"]
mod tests;
