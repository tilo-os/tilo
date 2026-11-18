//! The turns people ask agents in conversations, each answered in its agent session.
//!
//! An agent connection that publishes the action an agent answers a turn with
//! ([`runtime_collab::ANSWER`]) answers the turns of its agent in its workspace. For each turn the
//! host's agent sessions list ([`crate::apps::services::AgentSessions::turns`]), the broker invokes that
//! action on one of them, as the agent acting for the turn's author in the turn's session
//! ([`Caller::agent_turn`]). Each connection answers one turn at a time.
//!
//! - Every request the connection makes while it answers is that turn's: decided as the turn's
//!   caller, through its agent session ([`super::AgentConnection`]).
//! - A turn the sessions no longer list (its reply finished, the person stopped it, a later message
//!   replaced it) has its invocation cancelled.
//! - What the turn spent comes back as the invocation's items, `{"spent": <micros>}`, and is
//!   charged through [`crate::apps::services::AgentSessions::spent`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilo_actions::remote::Caller;

use super::{
    ActivityTarget, AppBrokerState, BrokerActor, ContinuingCaller, Delivery, InvocationEnvelope,
    InvocationOwner, PendingInvocation,
};
use crate::apps::services::AgentTurn;

/// Whether `action` of `app` is how an agent answers a turn: only the host invokes it.
pub(super) fn is_answer(app: &str, action: &super::PublishedAction) -> bool {
    app == tilo_automation::AGENT_APP_ID
        && action.resource == runtime_collab::TURN_RESOURCE
        && action.action == runtime_collab::ANSWER
}

/// Whether `surface` is an agent's that answers turns.
pub(super) fn answers_turns(surface: &super::PublishedSurface) -> bool {
    surface
        .actions
        .iter()
        .any(|action| is_answer(&surface.app_id, action))
}

/// The turns and the connections that answer them.
#[derive(Default)]
pub(super) struct Turns {
    /// The connections that answer turns, by peer.
    answering: Mutex<HashMap<u64, Answering>>,
    /// The turn each session was last handed, by session.
    handed: Mutex<HashMap<String, Handed>>,
    /// The questions on the person's sheet about a turn, by session.
    asking: Mutex<BTreeMap<String, u32>>,
    /// A handed turn ended: another may go to its connection now.
    due: AtomicBool,
}

/// A connection that answers turns: the workspace and agent it was admitted as.
struct Answering {
    workspace: String,
    agent: String,
}

/// A turn handed to a connection.
#[derive(Clone)]
struct Handed {
    workspace: String,
    turn: tilo_action_policy::turn::Turn,
    attempt: u32,
    peer: u64,
    job: u64,
    invocation: tilo_actions::InvocationId,
    owner: InvocationOwner,
    /// The turn's caller: its execution ends with the turn.
    caller: Caller,
    /// Its answer ended as a failure: the session shows the person that nobody answered, and the
    /// turn is not handed out again behind their back.
    ended: bool,
}

impl Handed {
    fn answers(&self, turn: &AgentTurn) -> bool {
        self.turn == turn.turn && self.attempt == turn.attempt
    }
}

impl Turns {
    /// Connection `peer`, admitted as `agent` in `workspace`, answers turns.
    pub(super) fn answer(&self, peer: u64, workspace: &str, agent: &str) {
        if let Ok(mut answering) = self.answering.lock() {
            answering.insert(
                peer,
                Answering {
                    workspace: workspace.to_owned(),
                    agent: agent.to_owned(),
                },
            );
        }
        self.due.store(true, Ordering::Release);
    }

    /// Connection `peer` went away: a turn it answered goes to another.
    pub(super) fn forget(&self, peer: u64) {
        if let Ok(mut answering) = self.answering.lock()
            && answering.remove(&peer).is_some()
        {
            self.due.store(true, Ordering::Release);
        }
    }

    /// Whether any connection answers turns.
    pub(super) fn answering(&self) -> bool {
        self.answering
            .lock()
            .is_ok_and(|answering| !answering.is_empty())
    }

    /// Whether a turn ended since this was last asked: the broker hands out the next.
    pub(super) fn take_due(&self) -> bool {
        self.due.swap(false, Ordering::AcqRel)
    }

    /// The session job `job` answers a turn in, and the turn it answers, if it answers one.
    fn session_of(&self, job: u64) -> Option<(String, Handed)> {
        self.handed
            .lock()
            .ok()?
            .iter()
            .find(|(_, handed)| handed.job == job)
            .map(|(session, handed)| (session.clone(), handed.clone()))
    }

    /// The jobs whose turn waits on a person now.
    pub(super) fn waiting(&self) -> BTreeSet<u64> {
        let Ok(asking) = self.asking.lock() else {
            return BTreeSet::new();
        };
        if asking.is_empty() {
            return BTreeSet::new();
        }
        self.handed
            .lock()
            .map(|handed| {
                handed
                    .iter()
                    .filter(|(session, _)| asking.contains_key(*session))
                    .map(|(_, handed)| handed.job)
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A question about `session`'s turn is on the person's sheet. Dropping it is their answer, or the
/// sheet going away: with their last question answered, the turn counts its idle time from then on.
pub(super) struct AskedPerson {
    state: Arc<AppBrokerState>,
    session: String,
}

impl Drop for AskedPerson {
    fn drop(&mut self) {
        self.state.answer_given(&self.session);
    }
}

impl AppBrokerState {
    /// A question about what `session`'s turn is doing is put to the person. It is not idle while
    /// they decide, however long they take.
    pub(super) fn asks_person(self: &Arc<Self>, session: &str) -> AskedPerson {
        if let Ok(mut asking) = self.turns.asking.lock() {
            let asked = asking.entry(session.to_owned()).or_default();
            *asked = asked.saturating_add(1);
        }
        self.local_turn.notify_one();
        AskedPerson {
            state: Arc::clone(self),
            session: session.to_owned(),
        }
    }

    /// The person answered one of the questions about `session`'s turn, or its sheet went away: the
    /// turn counts its idle time from when the last of them is answered.
    fn answer_given(&self, session: &str) {
        let answered = if let Ok(mut asking) = self.turns.asking.lock() {
            match asking.get_mut(session) {
                Some(left) if *left > 1 => {
                    *left = left.saturating_sub(1);
                    false
                }
                _ => {
                    asking.remove(session);
                    true
                }
            }
        } else {
            false
        };
        if !answered {
            return;
        }
        let job = self
            .turns
            .handed
            .lock()
            .ok()
            .and_then(|handed| handed.get(session).map(|handed| handed.job));
        if let Some(job) = job
            && let Ok(mut pending) = self.pending_invocations.lock()
            && let Some(pending) = pending.get_mut(&job)
        {
            pending.last_activity = tilo_clock::now();
        }
        self.local_turn.notify_one();
    }

    /// Hand every turn the agent sessions list to a connection of its agent, and cancel the
    /// handed turns they no longer list.
    pub(super) fn refresh_turns(self: &Arc<Self>) {
        let Some(sessions) = self.services.agent_sessions.clone() else {
            return;
        };
        let workspaces: BTreeSet<String> = self
            .turns
            .answering
            .lock()
            .map(|answering| {
                answering
                    .values()
                    .map(|answering| answering.workspace.clone())
                    .collect()
            })
            .unwrap_or_default();
        for workspace in workspaces {
            let turns = sessions.turns(&workspace);
            let ended: Vec<Handed> = match self.turns.handed.lock() {
                Ok(mut handed) => {
                    let gone: Vec<String> = handed
                        .iter()
                        .filter(|(session, handed)| {
                            handed.workspace == workspace
                                && !turns
                                    .iter()
                                    .any(|turn| &turn.session == *session && handed.answers(turn))
                        })
                        .map(|(session, _)| session.clone())
                        .collect();
                    gone.iter()
                        .filter_map(|session| handed.remove(session))
                        .collect()
                }
                Err(error) => {
                    tracing::warn!(%error, "the turns handed out are unavailable");
                    return;
                }
            };
            for handed in ended {
                handed.caller.retire_execution();
                // Already over when it ended by itself: nothing is left to cancel.
                let _ = self.request_cancellation_for_owner(handed.job, &handed.owner);
            }
            for turn in &turns {
                if let Err(error) = self.hand_turn(&workspace, turn) {
                    tracing::warn!(%error, session = %turn.session, "a turn was not handed to its agent");
                }
            }
        }
    }

    /// Hand `turn` to a connection of its agent that answers no other turn now, unless it was
    /// handed already to one that is still there, or its answer ended as a failure the session
    /// shows.
    fn hand_turn(&self, workspace: &str, turn: &AgentTurn) -> Result<(), String> {
        let handed = self
            .turns
            .handed
            .lock()
            .map_err(|_| "the turns are unavailable")?
            .clone();
        let answering = self
            .turns
            .answering
            .lock()
            .map_err(|_| "the turns are unavailable")?
            .iter()
            .filter(|(_, answering)| {
                answering.workspace == workspace && answering.agent == turn.agent
            })
            .map(|(peer, _)| *peer)
            .collect::<Vec<_>>();
        if handed.get(&turn.session).is_some_and(|held| {
            held.answers(turn) && (held.ended || answering.contains(&held.peer))
        }) {
            return Ok(());
        }
        let Some(peer) = answering
            .into_iter()
            .filter(|peer| {
                !handed
                    .values()
                    .any(|held| held.peer == *peer && self.is_pending(held.job))
            })
            .min()
        else {
            return Ok(());
        };
        let delivered = self.deliver_turn(peer, workspace, turn)?;
        self.turns
            .handed
            .lock()
            .map_err(|_| "the turns are unavailable")?
            .insert(turn.session.clone(), delivered);
        Ok(())
    }

    fn is_pending(&self, job: u64) -> bool {
        self.pending_invocations
            .lock()
            .is_ok_and(|pending| pending.contains_key(&job))
    }

    /// Invoke `turn`'s answer on connection `peer`.
    #[allow(
        clippy::too_many_lines,
        reason = "one turn's delivery: its caller, its pending invocation and its envelope together"
    )]
    fn deliver_turn(&self, peer: u64, workspace: &str, turn: &AgentTurn) -> Result<Handed, String> {
        let roster = self
            .services
            .agents
            .as_deref()
            .ok_or("this host reads no workspace's agents")?;
        let name = roster
            .listed(workspace)
            .into_iter()
            .find(|listed| listed.id == turn.agent)
            .map(|listed| listed.name)
            .ok_or_else(|| format!("the workspace lists no agent {}", turn.agent))?;
        let mut request = turn.request.clone();
        request.spend = roster.spend(workspace, &turn.agent);
        let argument = serde_json::to_string(&request).map_err(|error| error.to_string())?;
        let caller = Caller::agent_turn(&turn.agent, &turn.author, turn.turn.clone())
            .in_workspace(workspace)
            .named(&name)
            .in_agent_session(turn.session.clone())
            .in_new_execution();
        let actor = BrokerActor::for_caller(&caller);
        let (surface, action) = self
            .surfaces
            .lock()
            .map_err(|_| "the published surfaces are unavailable")?
            .get(&peer)
            .and_then(|surface| {
                surface.action_projection(runtime_collab::TURN_RESOURCE, runtime_collab::ANSWER)
            })
            .ok_or("the agent's connection no longer answers turns")?;
        let sender = self
            .invocation_subscribers
            .lock()
            .map_err(|_| "the invocation streams are unavailable")?
            .get(&peer)
            .filter(|sender| !sender.is_closed())
            .cloned()
            .ok_or("the agent's connection has no invocation stream")?;
        let id = self
            .invocation_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| "invocation handle space exhausted")?;
        let invocation = tilo_actions::InvocationId::new();
        let context = tilo_actions::ActionContext::for_resource(
            &action.resource,
            &action.resource_kind,
            &action.resource_label,
        )
        .with_app(&surface.app_id, &surface.app_id)
        .with_purpose(request.title);
        let mut context = context;
        context.group = caller.agent_session();
        let owner = InvocationOwner::Local {
            actor: actor.member().clone(),
        };
        let (tx, completion) = std::sync::mpsc::channel();
        let pending = PendingInvocation {
            caller: Some(ContinuingCaller::Caller(caller.clone())),
            // A turn's steps reach as far as the turn's grants let them, never to a privileged
            // action, which no turn takes.
            reach: tilo_action_policy::Reach {
                kind: tilo_action_policy::ActionKind::Outside,
                privileged: false,
            },
            activity: ActivityTarget::new(
                None,
                (&actor, Some(&caller), Some(&context)),
                &surface,
                &action,
                Some(&argument),
            ),
            owner: owner.clone(),
            original: None,
            // The registered clock the turn is decided under. A turn carries no retained
            // caller time of its own, but the broker does register one: leaving this `None`
            // makes every later check (the terminal's effect receipt above all) fail with
            // "original policy clock or origin does not match runtime registration".
            policy_time: self.registered_policy_time()?,
            journal_key: None,
            peer_id: peer,
            tx,
            update: tilo_actions::InvocationUpdate::Accepted { invocation },
            prepared_effect: Arc::default(),
            last_sequence: 0,
            actor: actor.member().clone(),
            cancellable: action.contract.execution.cancellable(),
            retryable_on_disconnect: false,
            execution: action.contract.execution.clone(),
            result: action.contract.result.clone(),
            operation_kind: action.operation_kind,
            cluster_id: workspace.to_owned(),
            context: context.clone(),
            last_activity: tilo_clock::now(),
            cancellation_requested_at: None,
            cancellation_fence: Arc::new(AtomicBool::new(false)),
        };
        let envelope = InvocationEnvelope {
            id,
            invocation,
            resource: action.resource.clone(),
            action: action.action,
            argument: Some(argument),
            actor: actor.actor(),
            context: Some(context),
        };
        let job = self.start_admitted(super::staged_admission::Admitted::Delivered(Box::new(
            Delivery {
                id,
                completion,
                pending,
                envelope,
                sender,
                offload: None,
                untrusted: false,
            },
        )))?;
        Ok(Handed {
            workspace: workspace.to_owned(),
            turn: turn.turn.clone(),
            attempt: turn.attempt,
            peer,
            job: job.id,
            invocation,
            owner,
            caller,
            ended: false,
        })
    }

    /// The caller connection `peer` makes its requests as while it answers a turn: the turn's,
    /// reaching no further than the turn does. `None` while it answers none.
    pub(super) fn turn_caller(&self, peer: u64) -> Option<Caller> {
        let invocation = self
            .turns
            .handed
            .lock()
            .ok()?
            .values()
            .find(|held| held.peer == peer && self.is_pending(held.job))?
            .invocation;
        self.child_caller(peer, invocation)
    }

    /// What job `id`'s new state `update` says of its turn, if it answers one: what it spent is
    /// charged, once it ends its connection may take the next, and an answer that ended as a
    /// failure is the session's to show on the message.
    pub(super) fn turn_updated(&self, id: u64, update: &tilo_actions::InvocationUpdate) {
        let Some((session, handed)) = self.turns.session_of(id) else {
            return;
        };
        if let tilo_actions::InvocationUpdate::Item { value, .. } = update
            && let Some(spent) = value.get("spent").and_then(serde_json::Value::as_u64)
            && let Some(sessions) = self.services.agent_sessions.as_deref()
        {
            sessions.spent(&session, spent);
        }
        if update.is_terminal() {
            handed.caller.retire_execution();
            if let (tilo_actions::InvocationUpdate::Failed { error, .. }, Some(sessions)) =
                (update, self.services.agent_sessions.as_deref())
            {
                sessions.turn_failed(&session, &handed.turn, handed.attempt, &error.message);
                if let Ok(mut held) = self.turns.handed.lock()
                    && let Some(held) = held.get_mut(&session)
                    && held.job == id
                {
                    held.ended = true;
                }
            }
            self.turns.due.store(true, Ordering::Release);
            self.local_turn.notify_one();
        }
    }
}
