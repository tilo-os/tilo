//! The agent in the workspace: a certified member beside browsers and
//! native devices.
//!
//! [`AgentMember`] holds a certified server session under the agent's own
//! device key and joins the workspace's durable and live sessions. It works on
//! each task as one turn of its model over the tools every agent in Tilo works
//! through (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find` and the rest,
//! [`tilo_agent_turn`]): each call is asked of the host's actions through the
//! agent's own session, so the host runs it under the agent's delegation, as
//! the agent, and orders the change it makes with everyone else's. Every
//! browser receives that change as an ordinary durable change attributed to
//! the agent.
//!
//! The turn runs on a thread of its own; the member's loop keeps the session,
//! says the agent's presence — its label, colour and what it is doing now
//! ([`tilo_session_client::saying`]) — and hears everyone else's, readable
//! through [`AgentMember::remote_members`]. It takes a task's outcome when the
//! turn ends and finishes the task with the model's answer.
//!
//! It never begins a task while paused or stopped ([`AgentControl`]); a turn
//! under way holds while its agent is paused and ends before its next step
//! once its task is cancelled or its agent stopped.

pub mod actions;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::presence::Shown;
use iroh::EndpointId;
use tilo_agent_turn::{Answer, Asked, Doing, Outcome, SessionActions};
use tilo_live::presence::KEEPALIVE_MS;
use tilo_mcp::McpServer;
use tilo_model_gateway::host_backend::protocol::{ModelRequest, ModelResponse, ToolCall};
use tilo_server_client::ServerAddress;
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::agents::{AgentStatus, AgentStep, TaskState};
use tilo_session_client::{
    Cmd, Event, Events, Link, MemoryPeerStore, Outbox as _, PeerConfig, TransportPhase,
};
use tilo_user_identity::DeviceCert;

use crate::control::{AgentControl, AgentIdentity};
use crate::model::{MakeTurns, PlanTurns};

pub(crate) use crate::workspace_documents::desktop_route;

/// What every agent member is told before its task, `{name}` its label.
const INSTRUCTIONS: &str = "You are {name}, an agent working in a Tilo workspace for the person \
     who made you. You act only through the tools you are given: they list, read and change what \
     the workspace's apps (Notes, Sheets, Tasks, Chat and the others) hold, as your grant allows. \
     Do the work with them; never claim you did something a tool did not do. When an action is \
     refused, say what was refused and why. Work until the request is done, then answer the person \
     in a few plain sentences: what you did and anything they should look at.";

/// What the person reads when the host runs no model its agents can work with.
const NO_MODEL: &str = "no agent model is configured: choose a provider and default model in \
     Settings, or start the host with --agent-model";

/// When the agent's loop should pump again, unless woken sooner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Now: there is work it can take.
    Now,
    /// After this long: the presence keep-alive is due.
    Within(Duration),
    /// Only when woken: by the session, the agent's steering, its turn or its host.
    WhenWoken,
}

/// What [`AgentMember::run_until`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pumped {
    /// Pumps taken.
    pub pumps: u64,
    /// Times the loop waited between pumps for work.
    pub waits: u64,
}

/// Where the agent's session goes.
pub struct MemberConfig {
    /// The workspace's host.
    pub host: ServerAddress,
    /// The host's certificate: whose actions the agent's tools ask.
    pub server_cert: DeviceCert,
    /// The workspace uuid.
    pub workspace: String,
}

/// A task the agent works on now: its turn on a thread of its own.
struct Working {
    id: String,
    /// How the turn ended, once it did.
    ended: Receiver<Outcome>,
}

/// One agent in one workspace.
pub struct AgentMember {
    identity: AgentIdentity,
    control: Arc<AgentControl>,
    peer: Link,
    events: Events,
    workspace: String,
    workspace_id: WorkspaceId,
    /// The host's actions, asked through the agent's session: what its tools act through.
    actions: SessionActions,
    /// What each task's turn asks its model through.
    model: MakeTurns,
    /// Who is here and what each says of themselves, said on this agent's session.
    presence: tilo_session_client::Presence,
    /// What the agent's presence last said it is doing.
    said: Option<String>,
    working: Option<Working>,
    /// Raised when the agent leaves: its turn ends before its next step.
    leaving: Arc<AtomicBool>,
    /// The host admitted the workspace; everything it replayed has arrived.
    ready: bool,
    left: bool,
}

impl AgentMember {
    /// Connect as `identity` and join `config.workspace`'s durable and live
    /// sessions, working on each task with the model `model` makes.
    pub fn start(
        config: MemberConfig,
        identity: AgentIdentity,
        control: Arc<AgentControl>,
        model: MakeTurns,
    ) -> Result<Self, String> {
        Self::start_with_store(
            config,
            identity,
            control,
            model,
            Arc::new(MemoryPeerStore::default()),
        )
    }

    /// [`Self::start`] with the session's state in `store`: an agent the
    /// host starts again on the same grant gets back its channel
    /// memberships and keys, as a device keeps them across a restart.
    pub fn start_with_store(
        config: MemberConfig,
        identity: AgentIdentity,
        control: Arc<AgentControl>,
        model: MakeTurns,
        store: Arc<dyn tilo_session_client::PeerStore>,
    ) -> Result<Self, String> {
        let workspace_id = WorkspaceId::parse(&config.workspace)
            .ok_or_else(|| "the workspace is not a uuid".to_string())?;
        if !identity.delegation.permits_workspace(workspace_id) {
            return Err("the agent is not delegated this workspace".into());
        }
        let woken = Arc::clone(&control);
        let (peer, events) = join(
            &config,
            &identity,
            workspace_id,
            store,
            Arc::new(move || woken.wake()),
        )?;
        let presence = tilo_session_client::saying::enter(
            &peer,
            &identity.cert,
            &identity.delegation.label,
            &config.workspace,
        )
        .ok_or_else(|| "the agent's certificate names no device".to_string())?;
        let actions =
            SessionActions::new(&peer, workspace_id, config.server_cert.device_public_key);
        Ok(Self {
            identity,
            control,
            peer,
            events,
            workspace: config.workspace,
            workspace_id,
            actions,
            model,
            presence,
            said: None,
            working: None,
            leaving: Arc::new(AtomicBool::new(false)),
            ready: false,
            left: false,
        })
    }

    /// The certified session.
    #[must_use]
    pub const fn peer(&self) -> &Link {
        &self.peer
    }

    /// The agent's steering state.
    #[must_use]
    pub const fn control(&self) -> &Arc<AgentControl> {
        &self.control
    }

    /// The agent's device key.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.identity.cert.device_public_key
    }

    /// Take in what the session delivered and how a task's turn ended, begin
    /// the next task when none runs, and say the agent's presence. Says when
    /// to pump again.
    pub fn pump(&mut self) -> Next {
        for event in self.events.drain() {
            self.receive(event);
        }
        if self.left {
            return Next::WhenWoken;
        }
        if self.control.status() == AgentStatus::Stopped {
            self.leave();
            return Next::WhenWoken;
        }
        self.fail_if_session_ended();
        self.take_outcome();
        if self.working.is_none() && self.ready && self.control.may_act() {
            self.begin_task();
        }
        if self.said != self.activity() {
            self.publish_presence();
        }
        self.presence.due_in(&self.workspace);
        if self.working.is_some() {
            return Next::Within(self.presence_wait());
        }
        Next::WhenWoken
    }

    /// How long this loop may sleep before the board owes another word.
    fn presence_wait(&self) -> Duration {
        let now = now_ms();
        let due = self
            .presence
            .next_due_in(&self.workspace)
            .unwrap_or(now + KEEPALIVE_MS);
        Duration::from_secs_f64(((due - now) / 1_000.0).max(0.001))
    }

    /// Pump until `stop` holds or `deadline` passes, sleeping between
    /// pumps until the session, the agent's steering, its turn or its host
    /// wakes the agent, or its presence keep-alive is due — never on an
    /// interval. `stop` is checked before every pump and after it.
    pub fn run_until(
        &mut self,
        deadline: Option<Instant>,
        mut stop: impl FnMut(&Self) -> bool,
    ) -> Pumped {
        let mut pumped = Pumped::default();
        while !stop(self) {
            let next = self.pump();
            pumped.pumps = pumped.pumps.saturating_add(1);
            if stop(self) {
                break;
            }
            let now = Instant::now();
            let until_deadline = match deadline {
                Some(deadline) if deadline <= now => break,
                Some(deadline) => Some(deadline.duration_since(now)),
                None => None,
            };
            let timeout = match next {
                Next::Now => continue,
                Next::Within(wait) => Some(until_deadline.map_or(wait, |rest| rest.min(wait))),
                Next::WhenWoken => until_deadline,
            };
            pumped.waits = pumped.waits.saturating_add(1);
            self.control.wait_for_change(timeout);
        }
        pumped
    }

    fn receive(&mut self, event: Event) {
        match event {
            Event::HostReady {
                ws: workspace_id, ..
            } if workspace_id == self.workspace_id => {
                self.ready = true;
                if self.control.status() == AgentStatus::Starting {
                    self.control.set_status(AgentStatus::Waiting);
                }
            }
            Event::LiveSession {
                ws: workspace_id,
                from,
                track: tilo_live::live::Track::Presence,
                body,
                ..
            } if workspace_id == self.workspace_id && from != self.device() => {
                if let Ok(from) = EndpointId::from_bytes(&from) {
                    self.presence
                        .heard_body(&self.workspace.clone(), from, &body);
                }
            }
            _ => {}
        }
    }

    /// Fail what this agent holds when the session to its host ended for good
    /// before it joined.
    ///
    /// A session that gave up — a refused device, an expired certificate —
    /// sleeps until someone dials it again, and nobody does for a host's
    /// agent: its tasks would wait for a workspace it never reached, showing
    /// only that it is starting. They fail with the reason instead, so the
    /// person who asked is told what happened.
    fn fail_if_session_ended(&self) {
        if self.ready || self.control.status() == AgentStatus::Failed {
            return;
        }
        let status = self.peer.status();
        if status.phase != TransportPhase::Failed {
            return;
        }
        let reason = status
            .failures
            .last()
            .map(|failure| failure.message.clone())
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "the session to the host ended before it joined".to_owned());
        self.control.fail(&reason);
    }

    /// Begin the oldest queued task: one turn of the agent's model over the
    /// workspace's tools, on a thread of its own, which wakes the loop when
    /// it ends.
    fn begin_task(&mut self) {
        let Some((id, text)) = self.control.begin_next() else {
            return;
        };
        let delegation = &self.identity.delegation;
        let agent = crate::control::agent_id(delegation).to_owned();
        let label = delegation.label.clone();
        let person = hex::encode(delegation.user_public_key);
        let server = McpServer::new(self.actions.clone(), self.workspace.clone(), person)
            .with_agent_session(id.clone())
            .agent_named(&label);
        let mut asked = Asked::new(INSTRUCTIONS.replace("{name}", &label), text);
        asked.key = format!("{agent}:{id}");
        let doing = Steering {
            task: id.clone(),
            control: Arc::clone(&self.control),
            leaving: Arc::clone(&self.leaving),
        };
        let (finished, ended) = std::sync::mpsc::channel();
        let model = Arc::clone(&self.model);
        let control = Arc::clone(&self.control);
        let spawned = std::thread::Builder::new()
            .name(format!("agent-{agent}"))
            .spawn(move || {
                let outcome = work(&server, &model, asked, &doing);
                let _ = finished.send(outcome);
                control.wake();
            });
        match spawned {
            Ok(_) => self.working = Some(Working { id, ended }),
            Err(error) => self.control.finish(&id, Err(error.to_string())),
        }
    }

    /// Finish the running task once its turn ended: with the model's answer,
    /// or failed with why it could not go on. A task its person cancelled
    /// stays cancelled.
    fn take_outcome(&mut self) {
        let Some(working) = &self.working else {
            return;
        };
        let outcome = match working.ended.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                Outcome::Failed("the agent's turn ended without an answer".to_owned())
            }
        };
        let id = working.id.clone();
        self.working = None;
        match outcome {
            Outcome::Answered(words) => self.control.finish(&id, Ok(words)),
            Outcome::Stopped(why) | Outcome::Failed(why) => self.control.finish(&id, Err(why)),
            Outcome::Cancelled => {}
        }
        self.control.set_activity(None);
    }

    /// Everyone here, as every surface that draws them reads them: each with the view it last said
    /// of itself.
    pub fn remote_members(&self) -> Vec<Shown> {
        self.presence.members(&self.workspace)
    }

    /// What this agent is doing now, as its presence says it.
    fn activity(&self) -> Option<String> {
        self.control
            .activity()
            .map(|activity| tilo_session_client::presence::activity(&activity))
    }

    fn publish_presence(&mut self) {
        let activity = self.activity();
        let view = tilo_session_client::presence::view(
            0,
            tilo_clock::unix_now_ms(),
            &self.identity.delegation.label,
            self.identity.delegation.color,
            activity.clone(),
            None,
            None,
        );
        self.presence.set_where(None, Some(Box::new(view)));
        self.said = activity;
    }

    fn leave(&mut self) {
        self.leaving.store(true, Ordering::Release);
        self.working = None;
        self.publish_presence();
        self.presence.taking_part(&[], None);
        self.left = true;
    }

    /// Leave the workspace and end the session.
    pub fn close(mut self) {
        if !self.left {
            self.leave();
        }
        self.peer.close();
    }
}

/// The clock presence is said and heard on.
#[expect(
    clippy::cast_precision_loss,
    reason = "milliseconds since the epoch are exact in an f64 for the life of a session"
)]
fn now_ms() -> f64 {
    tilo_clock::unix_now_ms() as f64
}

/// Work on `asked` through `server`'s tools with the model `model` makes, on a
/// runtime of the turn's own: how the turn ended.
fn work(
    server: &McpServer<SessionActions>,
    model: &MakeTurns,
    mut asked: Asked,
    doing: &Steering,
) -> Outcome {
    let Some(turns) = model() else {
        return Outcome::Failed(NO_MODEL.to_owned());
    };
    asked.sees = turns.sees();
    // The turn runs each call that is the agent's own on a worker it may
    // block, which only a runtime of more than one thread gives it.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return Outcome::Failed(error.to_string()),
    };
    let model = Arc::new(Mutex::new(Asking(turns)));
    let (outcome, cost) = runtime.block_on(tilo_agent_turn::work(server, model, &asked, doing));
    tracing::info!(task = %doing.task, cost, "an agent's task ended");
    outcome
}

/// A model's turns as a turn asks them.
struct Asking(Box<dyn PlanTurns>);

impl tilo_agent_turn::Model for Asking {
    fn turn(
        &mut self,
        request: &ModelRequest,
        key: &str,
        deadline: Instant,
    ) -> Result<ModelResponse, String> {
        self.0
            .turn(request, key, deadline, false)
            .map_err(|error| format!("My model did not answer: {}", error.message))
    }
}

/// How one task's turn is steered: what the agent's person said of it, and
/// what of it they see.
struct Steering {
    task: String,
    control: Arc<AgentControl>,
    leaving: Arc<AtomicBool>,
}

impl Doing<McpServer<SessionActions>> for Steering {
    /// A paused agent holds here until its person resumes or stops it.
    fn stopped(&self) -> bool {
        if self.control.paused_on(&self.task) {
            tokio::task::block_in_place(|| self.control.hold_while_paused(&self.task));
        }
        self.leaving.load(Ordering::Acquire) || !self.control.is_running(&self.task)
    }

    async fn calling(&self, call: &ToolCall, _: tilo_agent_turn::Step) {
        let activity = step_of(call).map_or_else(
            || call.name.clone(),
            |(_, resource, action)| format!("{action} {resource}"),
        );
        self.control.set_activity(Some(activity));
        self.control.wake();
    }

    /// Each action the agent ran is a step of its task.
    fn called(&self, call: &ToolCall, _: tilo_agent_turn::Step, answer: &Answer) {
        let Some((app, resource, action)) = step_of(call) else {
            return;
        };
        let step = AgentStep {
            app,
            resource,
            action,
            state: if answer.is_error {
                TaskState::Failed
            } else {
                TaskState::Completed
            },
            error: answer.is_error.then(|| answer.text.clone()),
            content: Vec::new(),
        };
        self.control.record_step(&self.task, step);
    }
}

/// The action `call` runs, as `(app, resource, action)`: a `tilo_do` of
/// `notes/Groceries#append-paragraph` is `append-paragraph` on
/// `notes/Groceries` in `notes`. `None` for a call that runs no action.
fn step_of(call: &ToolCall) -> Option<(String, String, String)> {
    if call.name != "tilo_do" {
        return None;
    }
    let path = call.input.get("path")?.as_str()?;
    let (resource, action) = path.rsplit_once('#')?;
    let app = resource
        .split('/')
        .next()
        .filter(|app| !app.is_empty())
        .unwrap_or(resource);
    Some((app.to_owned(), resource.to_owned(), action.to_owned()))
}

/// The agent's session with the workspace's host, joined to the workspace,
/// waking the agent whenever something arrives.
pub(crate) fn join(
    config: &MemberConfig,
    identity: &AgentIdentity,
    workspace_id: WorkspaceId,
    store: Arc<dyn tilo_session_client::PeerStore>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<(Link, Events), String> {
    let peer = tilo_session_client::start_session(
        PeerConfig::member(
            config.host.clone(),
            identity.cert.clone(),
            config.server_cert.clone(),
        ),
        Arc::new(identity.key.clone()),
        store,
    );
    let events = peer.events();
    peer.on_change(wake);
    peer.send(Cmd::Join(workspace_id))?;
    Ok((peer, events))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "1".into(),
            name: name.into(),
            input,
            extra_content: None,
        }
    }

    /// An action the agent ran is a step on the record it named; a read is none.
    #[test]
    fn an_action_is_a_step_and_a_read_is_not() {
        assert_eq!(
            step_of(&call(
                "tilo_do",
                serde_json::json!({"path": "notes/Groceries#append-paragraph"})
            )),
            Some((
                "notes".to_owned(),
                "notes/Groceries".to_owned(),
                "append-paragraph".to_owned()
            ))
        );
        assert_eq!(
            step_of(&call(
                "tilo_do",
                serde_json::json!({"path": "notes#new-note"})
            )),
            Some((
                "notes".to_owned(),
                "notes".to_owned(),
                "new-note".to_owned()
            ))
        );
        assert_eq!(
            step_of(&call(
                "tilo_read",
                serde_json::json!({"path": "notes/Groceries"})
            )),
            None
        );
    }
}
