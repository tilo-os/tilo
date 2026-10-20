//! The agent in the room: a certified participant beside browsers and
//! native devices.
//!
//! [`AgentParticipant`] holds a certified relay session under the agent's own
//! device key and joins the room's durable and live planes. It holds the
//! records it works in as replicas of their resources, each in its own
//! confined app worker ([`ConfinedReplica`]), acts
//! only through the operations they publish, and commits each resulting
//! change through the session, so the host orders it with everyone else's
//! and every browser receives it as an ordinary durable change attributed to
//! the agent. Around each action it publishes presence — the record it
//! focuses, where in it the action left it (the record's own item and
//! position, [`ConfinedReplica::presence`]), and a [`LiveParticipant`] of kind agent
//! with its label, colour and activity — so every participant's window
//! draws it there as it draws any collaborator. Other participants'
//! presence arrives the same way and is readable through
//! [`AgentParticipant::remote_members`].
//!
//! It never acts while paused or stopped ([`AgentControl`]), never outside
//! its delegated apps, and satisfies a confirmation step only when its
//! delegation allows it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::apps::{AppCatalogue, Seed};
use crate::control::{AgentControl, AgentIdentity};
use crate::planner::{PlanContext, Planner, Step, StepAction, written_steps};
use crate::worker::ConfinedReplica;
use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::{
    DocumentRoute, Host, LiveState, MemoryPeerStore, PeerConfig, PeerEvent, PeerEvents, PeerHandle,
    RemoteMember,
};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::agents::{AgentStatus, AgentStep, TaskState};
use tilo_relay_protocol::live::{LiveAnchor, LiveParticipant, LiveRecord, ParticipantKind};
use tilo_user_identity::{DeviceCert, RevocationList};

pub mod actions;
mod planning;
pub(crate) mod unopened;

pub(crate) use unopened::{SharedDesktop, desktop_route};

/// Presence is repeated this often while nothing changes, so late joiners
/// and lossy carriers still show the agent.
const PRESENCE_KEEPALIVE_MS: u64 = 1_000;

/// The longest a pump runs local steps back to back before it takes in the
/// session again.
const STEP_SLICE: Duration = Duration::from_millis(5);

/// When the agent's loop should pump again, unless woken sooner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Now: a step is ready.
    Now,
    /// After this long: the presence keep-alive is due.
    Within(Duration),
    /// Only when woken: by the session, the agent's steering or its host.
    WhenWoken,
}

/// What [`AgentParticipant::run_until`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pumped {
    /// Pumps taken.
    pub pumps: u64,
    /// Times the loop waited between pumps.
    pub waits: u64,
}

/// Where the agent's session goes.
pub struct ParticipantConfig {
    /// The room's host.
    pub host: Host,
    /// The host's certificate.
    pub host_cert: DeviceCert,
    /// The room uuid.
    pub room: String,
}

/// What performing a step came to.
enum Performed {
    Done(Result<Option<ActionOutput>, String>),
    /// The person is asked to confirm it.
    Asking,
}

/// A step waiting for the person to confirm it on their own device.
struct Asking {
    step: Step,
    route: DocumentRoute,
    address: ActionAddress,
    arguments: serde_json::Map<String, serde_json::Value>,
    /// The person's answer once it arrives; a host that went away without
    /// one answers that confirmation is still required.
    answer: Arc<std::sync::Mutex<Option<WitnessAnswer>>>,
}

type WitnessAnswer =
    Result<tilo_actions::remote::Confirmation, tilo_actions::remote::ActionHostError>;

struct Running {
    id: String,
    steps: VecDeque<Step>,
    performed: usize,
}

/// One agent in one room.
pub struct AgentParticipant {
    identity: AgentIdentity,
    control: Arc<AgentControl>,
    peer: PeerHandle,
    events: PeerEvents,
    room: String,
    workspace_id: u64,
    catalogue: AppCatalogue,
    /// The planner, on a thread of its own.
    planning: planning::Planning,
    /// When the plan the agent now carries out was ready, until its first
    /// step ran.
    plan_ready_at: Option<Instant>,
    apps: BTreeMap<DocumentRoute, ConfinedReplica>,
    /// Records the agent has not mounted, each merged as it changes.
    unopened: unopened::Unopened,
    live: LiveState,
    focus: Option<LiveRecord>,
    /// Where in the focused record its last operation left it.
    place: Option<(LiveAnchor, LiveAnchor)>,
    presence_at_ms: u64,
    running: Option<Running>,
    /// The current task's step the person is asked to confirm.
    asking: Option<Asking>,
    /// The host admitted the room; everything it replayed has arrived.
    ready: bool,
    left: bool,
    /// Bumped whenever what the open records publish may have changed; the
    /// revision remote catalogues are issued at.
    generation: u64,
    /// Remote-action frames waiting for an answer, with their senders.
    remote: Vec<([u8; 32], Vec<u8>)>,
    answered: actions::Answered,
}

impl AgentParticipant {
    /// Connect as `identity` and join `config.room`'s durable and live
    /// planes.
    ///
    /// # Errors
    ///
    /// Returns why the room is not one the delegation covers.
    pub fn start(
        config: ParticipantConfig,
        identity: AgentIdentity,
        control: Arc<AgentControl>,
        catalogue: AppCatalogue,
        planner: Box<dyn Planner>,
    ) -> Result<Self, String> {
        let workspace_id = tilo_relay_protocol::room_workspace_id(&config.room)
            .map_err(|_| "the room is not a uuid".to_string())?;
        if !identity.delegation.permits_workspace(workspace_id) {
            return Err("the agent is not delegated this room".into());
        }
        let planning = planning::Planning::start(planner, Arc::clone(&control))?;
        let peer = PeerHandle::start(
            PeerConfig {
                host: config.host,
                cert: identity.cert.clone(),
                host_cert: config.host_cert,
                revocations: RevocationList::new(),
                watching: false,
            },
            Arc::new(identity.key.clone()),
            Arc::new(MemoryPeerStore::default()),
        );
        let events = peer.events();
        let woken = Arc::clone(&control);
        peer.on_change(Arc::new(move || woken.wake()));
        peer.join(workspace_id)?;
        peer.live_join(workspace_id)?;
        let unopened = unopened::Unopened::new(&config.room);
        Ok(Self {
            identity,
            control,
            peer,
            events,
            room: config.room,
            workspace_id,
            catalogue,
            planning,
            plan_ready_at: None,
            apps: BTreeMap::new(),
            unopened,
            live: LiveState::starting_now(),
            focus: None,
            place: None,
            presence_at_ms: 0,
            running: None,
            asking: None,
            ready: false,
            left: false,
            generation: 0,
            remote: Vec::new(),
            answered: actions::Answered::default(),
        })
    }

    /// The certified session.
    #[must_use]
    pub const fn peer(&self) -> &PeerHandle {
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

    /// Take in what the session delivered, answer remote actions, then take
    /// the current task's steps: local ones back to back for up to
    /// [`STEP_SLICE`], at most one that plans. Says when to pump again.
    pub fn pump(&mut self) -> Next {
        let now = tilo_clock::unix_now_ms();
        for event in self.events.drain() {
            self.receive(event, now);
        }
        // What arrived for the shared desktop is admitted through the
        // isolate in one batch, before any plan reads it.
        self.unopened.settle();
        self.take_plans();
        if self.left {
            return Next::WhenWoken;
        }
        if self.control.status() == AgentStatus::Stopped {
            self.leave();
            return Next::WhenWoken;
        }
        for (from, frame) in std::mem::take(&mut self.remote) {
            self.answer_remote_action(from, &frame);
        }
        let slice = Instant::now();
        while self.step() && slice.elapsed() < STEP_SLICE {}
        self.publish_changes();
        if self.focus.is_some() && now.saturating_sub(self.presence_at_ms) >= PRESENCE_KEEPALIVE_MS
        {
            self.publish_presence();
        }
        self.next_pump()
    }

    /// When the loop should pump again: at once while a step is ready, else
    /// at the presence keep-alive, else when woken (a plan being made wakes
    /// the loop when it is ready).
    fn next_pump(&self) -> Next {
        let work = self.running.is_some() || self.control.has_queued();
        if work
            && !self.planning.busy()
            && self.ready
            && self.control.may_act()
            && !self.keys_pending()
        {
            return Next::Now;
        }
        if self.focus.is_none() {
            return Next::WhenWoken;
        }
        let due = self.presence_at_ms.saturating_add(PRESENCE_KEEPALIVE_MS);
        let left = due.saturating_sub(tilo_clock::unix_now_ms()).max(1);
        Next::Within(Duration::from_millis(left))
    }

    /// Pump until `stop` holds or `deadline` passes, sleeping between
    /// pumps until the session, the agent's steering or its host wakes the
    /// agent, or its presence keep-alive is due — never on an interval.
    /// `stop` is checked before every pump and after it.
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

    fn receive(&mut self, event: PeerEvent, now: u64) {
        match event {
            PeerEvent::Ready { workspace_id } if workspace_id == self.workspace_id => {
                self.ready = true;
                if self.control.status() == AgentStatus::Starting {
                    self.control.set_status(AgentStatus::Waiting);
                }
            }
            PeerEvent::Document(document) => {
                if document.route.cluster_id != self.room {
                    return;
                }
                let Some(mutation) = document.mutation() else {
                    return;
                };
                let route = document.route.clone();
                if let Some(app) = self.apps.get_mut(&route) {
                    self.generation = self.generation.wrapping_add(1);
                    if let Err(error) = app.apply(&mutation) {
                        tracing::warn!(%error, "agent: a durable change did not merge");
                    }
                    self.live.durable_changed(&LiveRecord {
                        app: route.app_id,
                        record: route.record,
                    });
                } else {
                    let (collab_types::DocumentMutation::Snapshot(bytes)
                    | collab_types::DocumentMutation::Incremental(bytes)) = mutation;
                    self.unopened.receive(route, bytes);
                }
            }
            PeerEvent::Live {
                workspace_id,
                from,
                frame,
            } if workspace_id == self.workspace_id => {
                self.live.receive(from, &frame, now);
            }
            PeerEvent::RemoteAction {
                workspace_id,
                from,
                frame,
            } if workspace_id == self.workspace_id && from != self.device() => {
                self.remote.push((from, frame));
            }
            PeerEvent::LiveMember {
                workspace_id,
                device,
                active,
            } if workspace_id == self.workspace_id => {
                self.live.member(device, active);
            }
            _ => {}
        }
    }

    /// Take one step of the current task, starting the next queued task
    /// (and planning it) when none runs. Returns whether another step that
    /// plans nothing is ready, so the pump may take it at once.
    fn step(&mut self) -> bool {
        // The host answers `Ready` after replaying the room's records, and a
        // sealed record opens once the agent holds its keys, so a step never
        // runs on a room the agent has only half seen.
        if !self.ready || !self.control.may_act() || self.keys_pending() {
            return false;
        }
        if self.planning.busy() {
            // The planner's thread wakes the loop when the plan is ready.
            return false;
        }
        if self.running.is_none() {
            let Some((id, text)) = self.control.begin_next() else {
                return false;
            };
            self.plan(id, &text, false);
            if self.running.is_none() {
                // Being planned, or the plan failed.
                return false;
            }
        }
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        if !self.control.is_running(&running.id) {
            self.running = None;
            self.asking = None;
            return false;
        }
        let id = running.id.clone();
        if let Some(asking) = self.asking.take() {
            let arrived = asking.answer.lock_recover().take();
            let Some(answer) = arrived else {
                self.asking = Some(asking);
                return false;
            };
            let outcome = match answer {
                // The person's own device answered for this invocation.
                Ok(tilo_actions::remote::Confirmation::Witnessed(_)) => self
                    .invoke_as(&asking.route, &asking.address, &asking.arguments, true)
                    .map_err(actions::Refusal::into_text),
                Ok(_) | Err(_) => Err(format!(
                    "{}: the person did not confirm",
                    tilo_actions::door::DoorError::ConfirmationRequired.code()
                )),
            };
            return self.conclude(&id, &asking.step, outcome);
        }
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        let Some(step) = running.steps.pop_front() else {
            let (id, performed) = (running.id.clone(), running.performed);
            self.running = None;
            self.control.finish(
                &id,
                Ok(format!(
                    "Done: {performed} action{}.",
                    if performed == 1 { "" } else { "s" }
                )),
            );
            self.publish_presence();
            return false;
        };
        if let StepAction::Continue(rest) = &step.action {
            let rest = rest.clone();
            self.plan(id, &rest, true);
            return false;
        }
        if let Some(ready_at) = self.plan_ready_at.take() {
            tracing::debug!(after = ?ready_at.elapsed(), "agent: a plan's first step starts");
        }
        let outcome = match self.perform(&step) {
            Performed::Done(outcome) => outcome,
            Performed::Asking => return false,
        };
        self.conclude(&id, &step, outcome)
    }

    /// Record how `step` of task `id` went and carry on: returns whether
    /// another step that plans nothing is ready.
    fn conclude(
        &mut self,
        id: &str,
        step: &Step,
        outcome: Result<Option<ActionOutput>, String>,
    ) -> bool {
        let (resource, action) = step.log_names();
        let record = AgentStep {
            app: step.app.clone(),
            resource,
            action,
            state: if outcome.is_ok() {
                TaskState::Completed
            } else {
                TaskState::Failed
            },
            error: outcome.as_ref().err().cloned(),
        };
        self.control.record_step(id, record);
        match outcome {
            Ok(_) => {
                let Some(running) = self.running.as_mut() else {
                    return false;
                };
                running.performed = running.performed.saturating_add(1);
                // The next step plans nothing unless it is a `Continue`.
                !matches!(
                    running.steps.front().map(|step| &step.action),
                    Some(StepAction::Continue(_))
                )
            }
            Err(error) => {
                self.running = None;
                self.control.finish(id, Err(error));
                false
            }
        }
    }

    /// Whether a record of the room the agent may work on has arrived sealed
    /// and opens once the agent holds its channel keys: until then the room
    /// is only half seen.
    fn keys_pending(&self) -> bool {
        self.peer.status().routes.iter().any(|(route, status)| {
            status.awaiting_keys > 0
                && route.cluster_id == self.room
                && self.identity.delegation.permits_app(&route.app_id)
        })
    }

    fn perform(&mut self, step: &Step) -> Performed {
        let route = match &step.record {
            Some(record) => self.open(&step.app, Some(record)),
            None => match self.apps.keys().find(|route| route.app_id == step.app) {
                Some(route) => Ok(route.clone()),
                None => self.open(&step.app, None),
            },
        };
        let route = match route {
            Ok(route) => route,
            Err(error) => return Performed::Done(Err(error)),
        };
        let StepAction::Invoke { address, arguments } = &step.action else {
            return Performed::Done(Ok(None));
        };
        match self.invoke_as(&route, address, arguments, false) {
            Err(actions::Refusal::Door(tilo_actions::door::DoorError::ConfirmationRequired)) => {
                self.ask(step, route, address, arguments)
            }
            outcome => Performed::Done(outcome.map_err(actions::Refusal::into_text)),
        }
    }

    /// Put `step` to the person through their own host, and show the agent
    /// waiting on the record for them. Nobody to ask refuses the step.
    fn ask(
        &mut self,
        step: &Step,
        route: DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Performed {
        let refused = || {
            Performed::Done(Err(format!(
                "{}: nobody is there to confirm it",
                tilo_actions::door::DoorError::ConfirmationRequired.code()
            )))
        };
        let Some(witness) = self.control.witness() else {
            return refused();
        };
        let caller = tilo_actions::remote::Caller::delegated_agent(
            crate::control::agent_id(&self.identity.delegation).to_owned(),
            self.identity.cert.user_public_key,
            self.identity.cert.device_public_key,
        )
        .in_room(self.room.clone());
        // The action as the record offers it: what the person is asked.
        let offered = self.apps.get(&route).and_then(|app| {
            app.survey()
                .into_iter()
                .find(|descriptor| descriptor.address == *address)
        });
        let context = offered.as_ref().map(|descriptor| {
            tilo_actions::ActionContext::for_resource(
                &address.resource,
                &descriptor.resource_kind,
                &descriptor.resource_label,
            )
            .with_app(&route.app_id, &route.app_id)
            .with_purpose(&descriptor.action_label)
            .with_facet(tilo_actions::ActionFacet::new(
                tilo_actions::RISK_FACET,
                descriptor.contract.risk.name(),
            ))
        });
        let request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: actions::record_provider(&route),
                address: address.clone(),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(arguments.clone()),
            context,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let answer = Arc::new(std::sync::Mutex::new(None));
        let (arrived, control) = (Arc::clone(&answer), Arc::clone(&self.control));
        witness.confirm(&caller, &request).then(move |given| {
            *arrived.lock_recover() = Some(given.unwrap_or(Err(
                tilo_actions::remote::ActionHostError::ConfirmationRequired,
            )));
            control.wake();
        });
        let label = offered.map_or_else(
            || address.action.clone(),
            |descriptor| {
                format!(
                    "{} · {}",
                    descriptor.action_label, descriptor.resource_label
                )
            },
        );
        self.focus = Some(LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        self.control
            .set_activity(Some(format!("Waiting for confirmation: {label}")));
        self.publish_presence();
        self.asking = Some(Asking {
            step: step.clone(),
            route,
            address: address.clone(),
            arguments: arguments.clone(),
            answer,
        });
        Performed::Asking
    }

    /// Commit whatever the open records changed since the last commit.
    fn publish_changes(&mut self) {
        for (route, app) in &mut self.apps {
            match app.take_mutation() {
                Ok(Some(mutation)) => {
                    self.generation = self.generation.wrapping_add(1);
                    if let Err(error) = self.peer.publish(route.clone(), &mutation) {
                        tracing::warn!(%error, "agent: a change could not be committed");
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "agent: a change was not admitted"),
            }
        }
    }

    /// Plan `text` for task `id` (the rest of it when `continues`). Steps
    /// written out in the task are taken at once; anything else goes to the
    /// planner's thread, and the plan is taken when it wakes the loop.
    fn plan(&mut self, id: String, text: &str, continues: bool) {
        let context = self.plan_context();
        if let Some(steps) = written_steps(text, &context) {
            self.apply_plan(planning::Planned {
                task: id,
                continues,
                steps,
                ready_at: Instant::now(),
            });
            return;
        }
        if let Err(error) = self
            .planning
            .ask(id.clone(), text.to_owned(), context, continues)
        {
            self.apply_plan(planning::Planned {
                task: id,
                continues,
                steps: Err(error),
                ready_at: Instant::now(),
            });
        }
    }

    /// Take every plan the planner's thread finished.
    fn take_plans(&mut self) {
        while let Some(planned) = self.planning.take() {
            self.apply_plan(planned);
        }
    }

    /// Carry out `planned`: a new task's steps, or the rest of the running
    /// one's. A task cancelled or stopped while it was planned takes none of
    /// it.
    fn apply_plan(&mut self, planned: planning::Planned) {
        let planning::Planned {
            task,
            continues,
            steps,
            ready_at,
        } = planned;
        if !self.control.is_running(&task) {
            return;
        }
        match steps {
            Ok(steps) if !continues => {
                self.running = Some(Running {
                    id: task,
                    steps: steps.into(),
                    performed: 0,
                });
                self.plan_ready_at = Some(ready_at);
            }
            Ok(steps) => {
                if let Some(running) = self.running.as_mut().filter(|running| running.id == task) {
                    for step in steps.into_iter().rev() {
                        running.steps.push_front(step);
                    }
                    self.plan_ready_at = Some(ready_at);
                }
            }
            Err(error) => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|running| running.id == task)
                {
                    self.running = None;
                }
                self.control.finish(&task, Err(error));
            }
        }
    }

    fn plan_context(&mut self) -> PlanContext {
        let known = self.unopened.people_first();
        let open = self
            .apps
            .iter()
            .map(|(route, app)| (route.clone(), app.survey()))
            .collect();
        PlanContext {
            open,
            known,
            apps: self
                .catalogue
                .iter()
                .map(|definition| definition.app_id().to_owned())
                .filter(|app| self.identity.delegation.permits_app(app))
                .collect(),
        }
    }

    /// Records of the room the agent has seen but not opened.
    #[must_use]
    pub fn known_records(&self) -> Vec<DocumentRoute> {
        self.unopened.routes()
    }

    /// Open `app_id` on `record` (a record the room already has), or on a
    /// new record it publishes. Returns the record's route.
    ///
    /// # Errors
    ///
    /// Returns why the app may not or could not be opened.
    pub fn open(&mut self, app_id: &str, record: Option<&str>) -> Result<DocumentRoute, String> {
        if !self.identity.delegation.permits_app(app_id) {
            return Err(format!("the agent is not delegated {app_id}"));
        }
        let route = |record: &str| DocumentRoute {
            app_id: app_id.to_owned(),
            cluster_id: self.room.clone(),
            record: record.to_owned(),
        };
        let (seed, seeded) = match record {
            Some(record) => {
                let route = route(record);
                if self.apps.contains_key(&route) {
                    return Ok(route);
                }
                let saved = self
                    .unopened
                    .seed(&route)
                    .ok_or_else(|| format!("the room has no record {record} of {app_id} yet"))??;
                let seed = Seed::Existing {
                    // A room-record app (Notes, Chat) publishes its package-primary
                    // record under the empty wire record id.
                    record: if record.is_empty() {
                        ui::record::RecordId::package_primary()
                    } else {
                        ui::record::RecordId::parse(record)
                            .ok_or_else(|| format!("{record} is not a record id"))?
                    },
                    changes: vec![saved],
                };
                (seed, Some(route))
            }
            None => (Seed::New, None),
        };
        let app = ConfinedReplica::open(
            &self.catalogue,
            app_id,
            &self.room,
            crate::control::agent_id(&self.identity.delegation),
            &crate::ReplicaActor::Agent,
            seed,
        )
        .map_err(|error| error.to_string())?;
        if let Some(seeded) = &seeded {
            self.unopened.forget(seeded);
        }
        let route = route(app.wire_record());
        if let Some(mutation) = app.take_mutation().map_err(|error| error.to_string())? {
            self.peer.publish(route.clone(), &mutation)?;
        }
        self.apps.insert(route.clone(), app);
        self.generation = self.generation.wrapping_add(1);
        self.show_on_desk(&route);
        self.focus = Some(LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        self.place = None;
        self.publish_presence();
        Ok(route)
    }

    /// Every operation the record at `route` publishes now.
    #[must_use]
    pub fn survey(&self, route: &DocumentRoute) -> Vec<ActionDescriptor> {
        self.apps
            .get(route)
            .map(ConfinedReplica::survey)
            .unwrap_or_default()
    }

    /// Invoke one action of the record at `route` as the agent, commit the
    /// change it made, and show the agent there.
    ///
    /// # Errors
    ///
    /// Returns the refusal: paused or stopped, outside the delegation, or
    /// the app's own door refusing or failing.
    pub fn invoke(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ActionOutput>, String> {
        // The agent never confirms by its own word: an action that asks
        // first is refused here; a task's step puts it to the person.
        self.invoke_as(route, address, arguments, false)
            .map_err(actions::Refusal::into_text)
    }

    /// [`Self::invoke`], with `confirmed` standing for whoever confirmed:
    /// the person, on their own device, for the agent's own steps; a person
    /// invoking it directly for a remote invocation.
    fn invoke_as(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<Option<ActionOutput>, actions::Refusal> {
        use actions::Refusal;
        if !self.control.may_act() {
            return Err(Refusal::Agent("the agent is paused or stopped".into()));
        }
        if !self.identity.delegation.permits_app(&route.app_id) {
            return Err(Refusal::Agent(format!(
                "the agent is not delegated {}",
                route.app_id
            )));
        }
        let app = self
            .apps
            .get_mut(route)
            .ok_or_else(|| Refusal::Agent("that record is not open".into()))?;
        let descriptor = app
            .survey()
            .into_iter()
            .find(|descriptor| descriptor.address == *address);
        let label = descriptor.as_ref().map(|descriptor| {
            format!(
                "{} · {}",
                descriptor.action_label, descriptor.resource_label
            )
        });
        let bound;
        let arguments = match &descriptor {
            Some(descriptor) => {
                bound = descriptor.bind_prompt_text(arguments.clone());
                &bound
            }
            None => arguments,
        };
        let output = app
            .invoke(address, arguments, confirmed)
            .map_err(Refusal::Door)?;
        if let Some(mutation) = app
            .take_mutation()
            .map_err(|error| Refusal::Agent(error.to_string()))?
        {
            self.generation = self.generation.wrapping_add(1);
            self.peer
                .publish(route.clone(), &mutation)
                .map_err(Refusal::Agent)?;
        }
        let place = app.presence();
        self.show_on_desk(route);
        self.focus = Some(LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        self.place = place;
        self.control.set_activity(label);
        self.publish_presence();
        Ok(Some(output))
    }

    /// The record held at `route`.
    #[must_use]
    pub fn app(&self, route: &DocumentRoute) -> Option<&ConfinedReplica> {
        self.apps.get(route)
    }

    /// Every other live participant as this agent sees them: browsers,
    /// native devices and other agents, with their presence.
    pub fn remote_members(&mut self) -> Vec<RemoteMember> {
        self.live.remote(tilo_clock::unix_now_ms())
    }

    fn participant(&self) -> LiveParticipant {
        let delegation = &self.identity.delegation;
        LiveParticipant {
            kind: ParticipantKind::Agent,
            label: delegation.label.clone(),
            color: delegation.color,
            activity: self.control.activity().map(|activity| {
                let mut end = activity
                    .len()
                    .min(tilo_relay_protocol::live::MAX_PARTICIPANT_ACTIVITY_BYTES);
                while !activity.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                activity.get(..end).unwrap_or_default().to_owned()
            }),
        }
    }

    /// Put the view of the record at `route` on the room's shared desktop
    /// when no tab shows it, beside the view the agent worked in last, so
    /// the people see where it acts ([`crate::desk`]).
    fn show_on_desk(&mut self, route: &DocumentRoute) {
        let desktop = desktop_route(&self.room);
        let Some(record) = self.apps.get(route).map(|app| app.record().to_owned()) else {
            return;
        };
        let beside = self
            .focus
            .as_ref()
            .map(|focus| DocumentRoute {
                app_id: focus.app.clone(),
                cluster_id: self.room.clone(),
                record: focus.record.clone(),
            })
            .filter(|last| last != route)
            .and_then(|last| self.apps.get(&last).map(|app| app.record().to_owned()));
        let title = self
            .catalogue
            .get(&route.app_id)
            .map_or(route.app_id.as_str(), |definition| definition.title());
        let view = crate::desk::View {
            app_id: &route.app_id,
            title,
            record: &record,
            beside: beside.as_deref(),
        };
        let author = format!(
            "agent:{}",
            crate::control::agent_id(&self.identity.delegation)
        );
        let place = |doc: &automerge::AutoCommit| crate::desk::place_view(doc, &author, &view);
        let change = match self.unopened.desktop() {
            Some(doc) => place(doc),
            None => self
                .apps
                .get(&desktop)
                .and_then(|app| app.read(place))
                .flatten(),
        };
        let Some(change) = change else {
            return;
        };
        let mutation = collab_types::DocumentMutation::Incremental(change.clone());
        if let Err(error) = self.peer.publish(desktop.clone(), &mutation) {
            tracing::warn!(%error, "agent: its view did not reach the desk");
            return;
        }
        match self.apps.get(&desktop) {
            Some(app) => {
                if let Err(error) = app.apply(&mutation) {
                    tracing::warn!(%error, "agent: the desk it holds did not take its view");
                }
            }
            None => self.unopened.receive(desktop, change),
        }
    }

    fn publish_presence(&mut self) {
        let now = tilo_clock::unix_now_ms();
        let participant = self.participant();
        let focus = self.focus.clone();
        let place = self.place.clone();
        match self.live.publish_presence(now, |presence| {
            presence.focus = focus;
            presence.selection = place;
            presence.participant = Some(participant);
        }) {
            Ok(frame) => {
                let _ = self.peer.publish_live(self.workspace_id, frame);
                self.presence_at_ms = now;
            }
            Err(error) => tracing::warn!(%error, "agent: presence out of bounds"),
        }
    }

    fn leave(&mut self) {
        self.running = None;
        self.focus = None;
        self.place = None;
        self.publish_presence();
        self.apps.clear();
        self.left = true;
    }

    /// Leave the room and end the session.
    pub fn close(mut self) {
        if !self.left {
            self.leave();
        }
        self.peer.close();
    }
}
