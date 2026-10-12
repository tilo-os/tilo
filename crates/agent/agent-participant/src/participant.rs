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
use crate::planner::{
    MOST_OBSERVED, PlanContext, Planner, Step, StepAction, observation, observation_within,
    readable, written_steps,
};
use crate::worker::ConfinedReplica;
use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::{
    DocumentRoute, Host, LiveState, MemoryPeerStore, PeerConfig, PeerEvent, PeerEvents, PeerHandle,
    RemoteMember,
};
use tilo_relay_protocol::agents::{AgentStatus, AgentStep, TaskState};
use tilo_relay_protocol::live::{LiveAnchor, LiveParticipant, LiveRecord, ParticipantKind};
use tilo_user_identity::{DeviceCert, RevocationList};

pub mod actions;
mod planning;
mod unopened;

pub(crate) use unopened::{SharedDesktop, desktop_route};

/// Presence is repeated this often while nothing changes, so late joiners
/// and lossy carriers still show the agent.
const PRESENCE_KEEPALIVE_MS: u64 = 1_000;

/// The longest a pump runs local steps back to back before it takes in the
/// session again.
const STEP_SLICE: Duration = Duration::from_millis(5);

/// The most records of the room a plan opens before the model is asked,
/// because the task names them by their titles.
const MOST_OPENED_BY_TITLE: usize = 2;

/// The most open records whose contents a plan is given
/// ([`PlanContext::snapshots`]), and the most characters of each.
const MOST_SNAPSHOTS: usize = 3;
const MOST_SNAPSHOT_CHARS: usize = 4_000;

/// The reads a record's snapshot is taken with, in order of preference:
/// whichever of them the record publishes that takes no argument.
const SNAPSHOT_READS: &[&str] = &["inspect", "read-text", "read-cells", "read"];

/// The most resources of one record a snapshot reads (a workbook's
/// sheets); what they hold stays within [`MOST_SNAPSHOT_CHARS`], and an
/// empty one costs a line.
const MOST_SNAPSHOT_READS: usize = 8;

/// The most characters of all of a plan's snapshots together, so a first
/// turn stays small and fast.
const MOST_SNAPSHOTS_CHARS: usize = 9_000;

/// An agent granted at most this many apps is given every one's
/// operations in full ([`PlanContext::reference`]); one granted more, only
/// those of the apps open, previewed or named.
const MOST_REFERENCED_APPS: usize = 4;

/// A task whose steps are done, waiting to be reported until what it
/// published is durable on the host.
struct Finishing {
    id: String,
    outcome: Result<String, String>,
    /// Completes once every change published before the task finished is
    /// durable; `None` when the session is closed.
    durable: Option<std::sync::mpsc::Receiver<Result<(), String>>>,
    since: Instant,
}

/// How a plan says nothing offered does what it was asked.
const NOTHING_LEFT: &str = "nothing offered fits";

/// How long a newly joined agent waits for the room's records to stop
/// arriving before it plans its first task.
const ROOM_SETTLE: Duration = Duration::from_millis(300);

/// The longest a finished task waits for the host to acknowledge its
/// changes before it is reported anyway. A record just made waits for its
/// channel's keys first, which can take several seconds while the room's
/// steward admits everyone who asked; reported earlier, the next task
/// would not find it.
const COMMIT_WAIT: Duration = Duration::from_secs(20);

/// The longest a plan waits for the keys of a record it is about. Keys a
/// steward never sends (a device it no longer admits) must not hold the
/// agent forever: the task is planned with what the room shows.
const KEYS_WAIT: Duration = Duration::from_secs(10);

/// How long an export waits after the last edit others' changes merged into
/// an open record: what others finished a moment ago is still arriving, and
/// an export takes the record as it is (at most [`KEYS_WAIT`]).
const EXPORT_QUIET: Duration = Duration::from_millis(1_000);

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
    /// Times the loop waited between pumps for work.
    pub waits: u64,
    /// Times it waited for a finished task's changes to become durable.
    pub durable_waits: u64,
}

/// Where the agent's session goes.
pub struct ParticipantConfig {
    /// The room's host.
    pub host: Host,
    /// The host's certificate.
    pub host_cert: DeviceCert,
    /// The room uuid.
    pub room: String,
    /// The room's content store: content the agent's operations name is
    /// read from it, and content its apps store is kept in it.
    pub content: tilo_content::ContentStore,
}

struct Running {
    id: String,
    /// The task as it was given, which a re-plan is shown beside the rest.
    request: String,
    steps: VecDeque<Step>,
    performed: usize,
    /// What the latest step that produced content returned: the content a
    /// step that [carries](Step::carries) is given. A step that makes no
    /// content (a read, a view, an edit), opening a record or planning
    /// again keeps it.
    last: Option<ActionOutput>,
    /// Content addresses the task's steps produced, oldest first.
    produced: Vec<String>,
    /// What the task's steps answered, oldest first, at most
    /// [`MOST_OBSERVED`]: what the task is planned again with.
    observed: VecDeque<String>,
    /// Whether a failed step was already planned around once: a task gets
    /// one repair, so a model's slip (an argument out of range, a stale
    /// target) is fixed rather than ending it, and a step that cannot work
    /// still fails the task.
    repaired: bool,
    /// The records its steps opened or acted on: what a re-plan is always
    /// offered, whatever the rest of the task calls them.
    touched: Vec<DocumentRoute>,
    /// How many of its performed steps did something (not only opened a
    /// record).
    acted: usize,
}

impl Running {
    /// The reply to a task whose steps all ran: how many, and the content
    /// they produced.
    fn done(&self) -> String {
        let performed = self.performed;
        let produced = if self.produced.is_empty() {
            String::new()
        } else {
            format!(" Produced {}.", self.produced.join(", "))
        };
        format!(
            "Done: {performed} action{}.{produced}",
            if performed == 1 { "" } else { "s" }
        )
    }
}

impl Running {
    /// A task that starts with `steps`, and what was read while planning
    /// them.
    fn new(id: String, request: String, steps: Vec<Step>, read: Vec<String>) -> Self {
        let mut running = Self {
            id,
            request,
            steps: steps.into(),
            performed: 0,
            last: None,
            produced: Vec::new(),
            observed: VecDeque::new(),
            repaired: false,
            touched: Vec::new(),
            acted: 0,
        };
        for seen in read {
            running.observe(seen);
        }
        running
    }

    /// Keep `seen` for the rest of the task to be planned with: the newest
    /// [`MOST_OBSERVED`].
    fn observe(&mut self, seen: String) {
        if self.observed.len() >= MOST_OBSERVED {
            self.observed.pop_front();
        }
        self.observed.push_back(seen);
    }

    /// `step` answered `output`: its value — what a view or read found, an
    /// artwork's size or a document's text — is kept for the rest of the
    /// task to be planned with, and when it produced content, that is what
    /// a step that carries is given next. A read between an export and the
    /// step that places it leaves the export to be placed.
    fn answered(&mut self, step: &Step, output: ActionOutput) {
        if let StepAction::Invoke { address, .. } = &step.action
            && let Some(observed) = observation(&step.app, address, &output)
        {
            self.observe(observed);
        }
        if !produced(&output).is_empty() {
            self.last = Some(output);
        }
    }
}

/// The content addresses among what `output` names.
fn produced(output: &ActionOutput) -> Vec<String> {
    output
        .resources
        .iter()
        .filter(|resource| tilo_actions::content_hash(resource).is_some())
        .cloned()
        .collect()
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
    /// The room's content store, lent to every record the agent holds.
    content: tilo_content::ContentStore,
    /// Records the agent has not mounted, each merged as it changes.
    unopened: unopened::Unopened,
    live: LiveState,
    focus: Option<LiveRecord>,
    /// Where in the focused record its last operation left it.
    place: Option<(LiveAnchor, LiveAnchor)>,
    presence_at_ms: u64,
    running: Option<Running>,
    /// What a new record of each app publishes, surveyed once in a record
    /// that is never published: the operations a plan may name before the
    /// app is open ([`PlanContext::previews`]).
    previews: BTreeMap<String, Vec<ActionDescriptor>>,
    /// The host admitted the room; everything it replayed has arrived.
    ready: bool,
    /// When the room last delivered a record the agent had not seen, since
    /// it was admitted: a newly joined agent plans once the room's records
    /// stop arriving ([`ROOM_SETTLE`]).
    settling: Option<Instant>,
    /// A task whose steps are done, with its outcome, waiting until the
    /// host has its changes ([`COMMIT_WAIT`]): it ends when others can see
    /// what it did, not before.
    finishing: Option<Finishing>,
    /// The task begun but waiting for the keys of a record it names.
    starting: Option<(String, String)>,
    /// Since when a plan waits for a record's keys ([`KEYS_WAIT`]).
    keys_wait: Option<Instant>,
    /// The records the current task's steps acted on, and the previous
    /// task's: a task that follows on ("the same suppliers", "the end
    /// dates") is planned with what the one before it made in view.
    task_touched: Vec<DocumentRoute>,
    previous_touched: Vec<DocumentRoute>,
    /// When others' changes last merged into an open record.
    last_merge: Option<Instant>,
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
        Self::start_with_store(
            config,
            identity,
            control,
            catalogue,
            planner,
            Arc::new(MemoryPeerStore::default()),
        )
    }

    /// [`Self::start`] with the session's state in `store`: an agent the
    /// host starts again on the same grant gets back its channel
    /// memberships and keys, as a device keeps them across a restart, and
    /// opens what the room sealed while it was away.
    ///
    /// # Errors
    ///
    /// Returns why the room is not one the delegation covers.
    pub fn start_with_store(
        config: ParticipantConfig,
        identity: AgentIdentity,
        control: Arc<AgentControl>,
        catalogue: AppCatalogue,
        planner: Box<dyn Planner>,
        store: Arc<dyn tilo_browser_peer::PeerStore>,
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
            },
            Arc::new(identity.key.clone()),
            store,
        );
        let events = peer.events();
        let woken = Arc::clone(&control);
        peer.on_change(Arc::new(move || woken.wake()));
        // Records of apps the grant does not cover are not this agent's to
        // open: its session is not admitted to their channels.
        let outside: Vec<String> = catalogue
            .iter()
            .map(|definition| definition.app_id.to_owned())
            .filter(|app| !identity.delegation.permits_app(app))
            .collect();
        peer.ignore_apps(outside)?;
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
            content: config.content,
            unopened,
            live: LiveState::starting_now(),
            focus: None,
            place: None,
            presence_at_ms: 0,
            running: None,
            previews: BTreeMap::new(),
            ready: false,
            settling: None,
            finishing: None,
            starting: None,
            keys_wait: None,
            task_touched: Vec::new(),
            previous_touched: Vec::new(),
            last_merge: None,
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
        self.answer_reads();
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
        self.finish_committed();
        if self.focus.is_some() && now.saturating_sub(self.presence_at_ms) >= PRESENCE_KEEPALIVE_MS
        {
            self.publish_presence();
        }
        self.next_pump()
    }

    /// When the loop should pump again: at once while a step is ready, else
    /// at the presence keep-alive, else when woken.
    fn next_pump(&self) -> Next {
        // Waiting on the host (a finished task's changes, the keys of a
        // record the next task names): look again shortly.
        if self.finishing.is_some() || self.starting.is_some() || self.keys_wait.is_some() {
            return Next::Within(Duration::from_millis(10));
        }
        let work = self.running.is_some() || self.control.has_queued();
        if work && !self.planning.busy() && self.ready && self.control.may_act() {
            if let Some(since) = self.settling {
                return Next::Within(
                    ROOM_SETTLE
                        .saturating_sub(since.elapsed())
                        .max(Duration::from_millis(1)),
                );
            }
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
            if self.finishing.is_some() {
                pumped.durable_waits = pumped.durable_waits.saturating_add(1);
            } else {
                pumped.waits = pumped.waits.saturating_add(1);
            }
            self.control.wait_for_change(timeout);
        }
        pumped
    }

    fn receive(&mut self, event: PeerEvent, now: u64) {
        match event {
            PeerEvent::Ready { workspace_id } if workspace_id == self.workspace_id => {
                if !self.ready {
                    self.settling = Some(Instant::now());
                }
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
                    self.last_merge = Some(Instant::now());
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
                    if self.settling.is_some()
                        && self.identity.delegation.permits_app(&route.app_id)
                        && !self.unopened.routes().contains(&route)
                    {
                        self.settling = Some(Instant::now());
                    }
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

    /// Whether a step may be taken: the room is admitted and settled, the
    /// agent may act, no record waits for keys and no finished task for
    /// its changes to be acknowledged.
    fn may_step(&mut self) -> bool {
        self.ready && self.control.may_act() && self.settled() && !self.finish_committed()
    }

    /// Whether the room's records have stopped arriving since the agent
    /// joined. Records another agent made just before this one joined can
    /// still be on their way after `Ready`: the first plan waits until none
    /// has arrived for [`ROOM_SETTLE`], so it is offered what the room holds.
    fn settled(&mut self) -> bool {
        if let Some(since) = self.settling {
            if since.elapsed() < ROOM_SETTLE {
                return false;
            }
            self.settling = None;
        }
        true
    }

    /// Take one step of the current task, starting the next queued task
    /// (and planning it) when none runs. Returns whether another step that
    /// plans nothing is ready, so the pump may take it at once.
    fn step(&mut self) -> bool {
        // The host answers `Ready` after replaying the room's records, and a
        // sealed record opens once the agent holds its keys, so a step never
        // runs on a room the agent has only half seen.
        if !self.may_step() {
            return false;
        }
        if self.planning.busy() {
            // The planner's thread wakes the loop when the plan is ready.
            return false;
        }
        if self.running.is_none() {
            // Planned on the planner's thread; its plan is taken when ready.
            self.begin_task();
            return false;
        }
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        if !self.control.is_running(&running.id) {
            self.running = None;
            return false;
        }
        let Some(step) = running.steps.pop_front() else {
            let (id, done) = (running.id.clone(), running.done());
            self.running = None;
            self.finish_task(id, Ok(done));
            self.publish_presence();
            return false;
        };
        let id = running.id.clone();
        let (request, previous) = (running.request.clone(), running.last.clone());
        let waits = if matches!(step.action, StepAction::Continue(_)) {
            self.touched_keys_pending()
        } else {
            self.step_record_pending(&step) || self.edits_arriving(&step)
        };
        if waits && self.may_wait_for_keys() {
            // A record the task just opened is planned with once it opens,
            // and a step acts on a record once the edits others sealed for
            // it are open: an export or an edit taken from half the record
            // would drop or overwrite what is still held back.
            if let Some(running) = self.running.as_mut() {
                running.steps.push_front(step);
            }
            return false;
        }
        self.end_keys_wait();
        if let StepAction::Continue(rest) = &step.action {
            // The rest is planned with the request as it was given beside
            // it, so what the model left out of the rest is still asked.
            let rest = rest.clone();
            self.plan(id, &rest, Some(request));
            return false;
        }
        if let Some(ready_at) = self.plan_ready_at.take() {
            tracing::debug!(after = ?ready_at.elapsed(), "agent: a plan's first step starts");
        }
        let outcome = self.perform(&step, previous.as_ref());
        let (resource, action) = step.log_names();
        let content = match &outcome {
            Ok(Some(output)) => produced(output),
            _ => Vec::new(),
        };
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
            content: content.clone(),
        };
        self.control.record_step(&id, record);
        match outcome {
            Ok(output) => {
                let Some(running) = self.running.as_mut() else {
                    return false;
                };
                running.performed = running.performed.saturating_add(1);
                if matches!(step.action, StepAction::Invoke { .. }) {
                    running.acted = running.acted.saturating_add(1);
                }
                if let Some(output) = output {
                    running.answered(&step, output);
                }
                running.produced.extend(content);
                // The next step plans nothing unless it is a `Continue`.
                !matches!(
                    running.steps.front().map(|step| &step.action),
                    Some(StepAction::Continue(_))
                )
            }
            Err(error) => {
                if self.repair(&step, &error) {
                    return false;
                }
                self.running = None;
                self.finish_task(id, Err(error));
                false
            }
        }
    }

    /// Start the next queued task and have it planned. A record of an app
    /// the task names that arrived sealed opens once the agent holds its
    /// keys: the task is planned then, so it sees what the room holds.
    /// Records of other apps do not hold it up.
    fn begin_task(&mut self) {
        let Some((id, text)) = self.starting.take().or_else(|| self.control.begin_next()) else {
            return;
        };
        if self.keys_pending_for(&text) && self.may_wait_for_keys() {
            self.starting = Some((id, text));
            return;
        }
        self.end_keys_wait();
        self.previous_touched = std::mem::take(&mut self.task_touched);
        self.plan(id, &text, None);
    }

    /// Plan `text` for task `id`: a new task, or the rest of the running one
    /// when `request` (the task as it was given) comes beside it. Steps
    /// written out in the task are taken at once; anything else goes to the
    /// planner's thread, and the plan is taken when it wakes the loop.
    fn plan(&mut self, id: String, text: &str, request: Option<String>) {
        let continues = request.is_some();
        let context = self.plan_context(text, request);
        if let Some(steps) = written_steps(text, &context) {
            self.apply_plan(planning::Planned {
                task: id,
                text: text.to_owned(),
                continues,
                steps,
                read: Vec::new(),
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
                text: text.to_owned(),
                continues,
                steps: Err(error),
                read: Vec::new(),
                ready_at: Instant::now(),
            });
        }
    }

    /// Answer the reads the plan being made asks of the open records.
    fn answer_reads(&mut self) {
        for ask in self.planning.asked_reads() {
            let answer = self.read_now(&ask.route, &ask.address, &ask.arguments);
            let _ = ask.reply.send(answer);
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
    /// it; a rest that finds nothing left to do after steps ran is the task
    /// done, not failed.
    fn apply_plan(&mut self, planned: planning::Planned) {
        let planning::Planned {
            task,
            text,
            continues,
            steps,
            read,
            ready_at,
        } = planned;
        if !self.control.is_running(&task) {
            return;
        }
        let running_it = self
            .running
            .as_ref()
            .is_some_and(|running| running.id == task);
        match steps {
            Ok(steps) if !continues => {
                self.running = Some(Running::new(task, text, steps, read));
                self.plan_ready_at = Some(ready_at);
            }
            Ok(steps) => {
                if let Some(running) = self.running.as_mut().filter(|_| running_it) {
                    for seen in read {
                        running.observe(seen);
                    }
                    for step in steps.into_iter().rev() {
                        running.steps.push_front(step);
                    }
                    self.plan_ready_at = Some(ready_at);
                }
            }
            Err(error) => {
                let finished = if running_it {
                    self.running
                        .take()
                        .filter(|running| {
                            continues && running.acted > 0 && error.starts_with(NOTHING_LEFT)
                        })
                        .map(|running| running.done())
                } else {
                    None
                };
                self.finish_task(task, finished.ok_or(error));
            }
        }
    }

    /// Plan the rest of the running task again after `step` failed with
    /// `error`, knowing what failed and why, beside the request and what ran
    /// so far — once per task. Returns whether it will.
    fn repair(&mut self, step: &Step, error: &str) -> bool {
        let Some(running) = self.running.as_mut().filter(|running| !running.repaired) else {
            return false;
        };
        running.repaired = true;
        running.steps.clear();
        running.steps.push_back(Step {
            app: step.app.clone(),
            record: None,
            action: StepAction::Continue(format!(
                "The step {} failed: {error}. Finish the request with steps that work, without \
                 repeating the steps that already ran.",
                step.log_names().1
            )),
            carries: false,
        });
        true
    }

    /// End task `id` with `outcome` once the host has acknowledged the
    /// changes it made: published now, it is reported when none is pending
    /// ([`Self::finish_committed`]), so whoever is told it is done finds
    /// its changes in the room.
    fn finish_task(&mut self, id: String, outcome: Result<String, String>) {
        self.publish_changes();
        // Frames of a record just made can wait for its keys before they
        // are even sent; the barrier completes once all of them are durable.
        let durable = self.peer.barrier().ok();
        self.finishing = Some(Finishing {
            id,
            outcome,
            durable,
            since: Instant::now(),
        });
        self.finish_committed();
    }

    /// Report the finishing task once its changes are acknowledged, or
    /// after [`COMMIT_WAIT`]. Returns whether one is still waiting.
    fn finish_committed(&mut self) -> bool {
        let Some(finishing) = &self.finishing else {
            return false;
        };
        let waiting = finishing.durable.as_ref().is_some_and(|durable| {
            matches!(
                durable.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            )
        });
        if waiting && finishing.since.elapsed() < COMMIT_WAIT {
            return true;
        }
        if waiting {
            tracing::warn!("agent: a task is reported before the host acknowledged its changes");
        }
        tracing::debug!(after = ?finishing.since.elapsed(), "agent: a task's changes are durable");
        if let Some(finishing) = self.finishing.take() {
            self.control.finish(&finishing.id, finishing.outcome);
        }
        false
    }

    /// The wait for keys is over (they arrived, or it gave up).
    fn end_keys_wait(&mut self) {
        if let Some(since) = self.keys_wait.take() {
            tracing::debug!(after = ?since.elapsed(), "agent: keys waited for");
        }
    }

    /// Whether a plan may still wait for keys: [`KEYS_WAIT`] from the first
    /// time it did.
    fn may_wait_for_keys(&mut self) -> bool {
        let since = *self.keys_wait.get_or_insert_with(Instant::now);
        if since.elapsed() < KEYS_WAIT {
            return true;
        }
        tracing::debug!(after = ?since.elapsed(), "agent: waited for keys");
        tracing::warn!("agent: a record's keys did not arrive; planning without them");
        false
    }

    /// The records the running task's steps opened or acted on, and those
    /// the task before it acted on.
    fn touched(&self) -> Vec<DocumentRoute> {
        let mut touched = self
            .running
            .as_ref()
            .map(|running| running.touched.clone())
            .unwrap_or_default();
        for route in &self.previous_touched {
            if !touched.contains(route) {
                touched.push(route.clone());
            }
        }
        touched
    }

    /// Whether `step` exports while others' changes are still arriving.
    fn edits_arriving(&self, step: &Step) -> bool {
        matches!(&step.action, StepAction::Invoke { address, .. } if address.action.starts_with("export"))
            && self
                .last_merge
                .is_some_and(|merged| merged.elapsed() < EXPORT_QUIET)
    }

    /// Whether an open record of the app `step` acts in holds edits still
    /// waiting for their keys.
    fn step_record_pending(&self, step: &Step) -> bool {
        let status = self.peer.status();
        let held = |route: &DocumentRoute| {
            status
                .routes
                .get(route)
                .is_some_and(|route| route.awaiting_keys > 0)
        };
        // By app: the record a step names may not be open yet.
        self.apps
            .keys()
            .any(|route| route.app_id == step.app && held(route))
    }

    /// Whether a record the running task opened still waits for its keys.
    fn touched_keys_pending(&self) -> bool {
        let Some(running) = &self.running else {
            return false;
        };
        let status = self.peer.status();
        running.touched.iter().any(|route| {
            status
                .routes
                .get(route)
                .is_some_and(|route| route.awaiting_keys > 0)
        })
    }

    /// Whether a record of the room the agent may work on has arrived sealed
    /// and opens once the agent holds its channel keys: until then the room
    /// is only half seen.
    fn keys_pending_for(&self, task: &str) -> bool {
        self.peer.status().routes.iter().any(|(route, status)| {
            status.awaiting_keys > 0
                && route.cluster_id == self.room
                && self.identity.delegation.permits_app(&route.app_id)
                && crate::model::task_names_app(task, &route.app_id)
        })
    }

    /// Take `step`, after `previous`: what the last invoked step returned.
    fn perform(
        &mut self,
        step: &Step,
        previous: Option<&ActionOutput>,
    ) -> Result<Option<ActionOutput>, String> {
        // What a carried step consumes is settled before anything opens: a
        // step with nothing to consume is refused, not half done.
        let arguments = match &step.action {
            StepAction::Invoke { address, arguments } => Some(
                tilo_agent::plan::carried_arguments(arguments, step.carries, previous).map_err(
                    |error| {
                        format!(
                            "{} takes what the step before it produced: {error}",
                            address.action
                        )
                    },
                )?,
            ),
            StepAction::Open | StepAction::Continue(_) => None,
        };
        let route = match &step.record {
            Some(record) => self.open(&step.app, Some(record))?,
            None => match self.apps.keys().find(|route| route.app_id == step.app) {
                Some(route) => route.clone(),
                None => self.open(&step.app, None)?,
            },
        };
        if let Some(running) = self.running.as_mut()
            && !running.touched.contains(&route)
        {
            running.touched.push(route.clone());
        }
        if !self.task_touched.contains(&route) {
            self.task_touched.push(route.clone());
        }
        match (&step.action, arguments) {
            (StepAction::Invoke { address, .. }, Some(arguments)) => {
                self.invoke(&route, address, &arguments)
            }
            _ => Ok(None),
        }
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

    /// What `task` is planned with. Before the model is asked, the records
    /// of the room the task names by their titles are opened; an app it is
    /// about of which the room has no record yet is previewed; and what the
    /// open records it is about hold is read. `request` is the task as it
    /// was given when `task` is the rest of it.
    fn plan_context(&mut self, task: &str, request: Option<String>) -> PlanContext {
        let apps: Vec<String> = self
            .catalogue
            .iter()
            .map(|definition| definition.app_id.to_owned())
            .filter(|app| self.identity.delegation.permits_app(app))
            .collect();
        // The records offered to open are named as their apps name them (a
        // document's first heading), so a task can say which: read in a
        // confined worker, started only when one changed since it was read.
        let mut offered: BTreeMap<&str, usize> = BTreeMap::new();
        let mut titles = crate::worker::ConfinedTitles::default();
        let mut known: Vec<(DocumentRoute, Option<String>)> = self
            .unopened
            .people_first()
            .into_iter()
            .map(|route| {
                let Some(app) = apps.iter().find(|app| **app == route.app_id) else {
                    return (route, None);
                };
                let count = offered.entry(app.as_str()).or_default();
                if *count >= crate::model::KNOWN_OFFERED {
                    return (route, None);
                }
                *count = count.saturating_add(1);
                let title = self
                    .unopened
                    .title(&route, |doc| titles.title(&route.app_id, doc));
                (route, title)
            })
            .collect();
        drop(titles);
        let mut about = self.open_named(task, &apps, &known);
        about.extend(self.open_new(task, &apps, &known));
        known.retain(|(route, _)| !self.apps.contains_key(route));
        let mut open: Vec<(DocumentRoute, Vec<ActionDescriptor>)> = self
            .apps
            .iter()
            .map(|(route, app)| (route.clone(), app.survey_with_effects()))
            .collect();
        // The records the task names, by title or by app, are what it is
        // about, and only theirs are offered: a record open from an earlier
        // task (the note, beside the page under review) would cost its
        // whole list on every plan. A task that names none is offered all.
        let touched = self.touched();
        let named: Vec<DocumentRoute> = open
            .iter()
            .map(|(route, _)| route)
            .filter(|route| {
                about.contains(route)
                    || touched.contains(route)
                    || crate::model::task_names_app(task, &route.app_id)
            })
            .cloned()
            .collect();
        let about = if named.is_empty() {
            open.iter().map(|(route, _)| route.clone()).collect()
        } else {
            open.retain(|(route, _)| named.contains(route));
            named
        };
        // Only an app of which the room has no record yet: where it has
        // one, a task about the app may well mean it ("add a sheet"), and
        // is offered to open it rather than to start another.
        let previews: Vec<(String, Vec<ActionDescriptor>)> = apps
            .iter()
            .filter(|app| {
                !open.iter().any(|(route, _)| route.app_id == **app)
                    && !known.iter().any(|(route, _)| route.app_id == **app)
                    && (apps.len() <= 2 || crate::model::task_names_app(task, app))
            })
            .filter_map(|app| Some((app.clone(), self.preview(app)?)))
            .collect();
        let mut snapshots = self.snapshots(&about, &open);
        snapshots.extend(content_facts(task, &self.content));
        // What the apps can do, the same whatever the room holds: every
        // granted app's, or for an agent granted many, those in play.
        let referenced: Vec<String> = apps
            .iter()
            .filter(|app| {
                apps.len() <= MOST_REFERENCED_APPS
                    || open.iter().any(|(route, _)| route.app_id == **app)
                    || previews.iter().any(|(previewed, _)| previewed == *app)
                    || crate::model::task_names_app(task, app)
            })
            .cloned()
            .collect();
        let reference = self.reference(referenced);
        let (produced, observed) = self
            .running
            .as_ref()
            .map(|running| {
                (
                    running.produced.clone(),
                    running.observed.iter().cloned().collect(),
                )
            })
            .unwrap_or_default();
        PlanContext {
            open,
            known,
            apps,
            produced,
            observed,
            previews,
            snapshots,
            request,
            reference,
            room: self.room.clone(),
        }
    }

    /// Make a new record of each of `apps` the task names that the room has
    /// none of, before it is planned: its channel's keys are asked for and
    /// granted while the model plans, rather than after, when the plan's
    /// first step would make it. Returns the records made.
    fn open_new(
        &mut self,
        task: &str,
        apps: &[String],
        known: &[(DocumentRoute, Option<String>)],
    ) -> Vec<DocumentRoute> {
        let mut made = Vec::new();
        for app in apps {
            if !crate::model::task_names_app_itself(task, app)
                || self.apps.keys().any(|route| route.app_id == *app)
                || known.iter().any(|(route, _)| route.app_id == *app)
            {
                continue;
            }
            match self.open(app, None) {
                Ok(route) => {
                    // A new record publishes nothing until its first edit:
                    // ask for its channel's keys now.
                    if let Err(error) = self.peer.open_route(route.clone()) {
                        tracing::warn!(%error, "agent: keys for a new record were not asked for");
                    }
                    made.push(route);
                }
                Err(error) => tracing::warn!(%error, app, "agent: a new record did not open"),
            }
        }
        if !made.is_empty() {
            self.publish_changes();
        }
        made
    }

    /// Open the room's records of `apps` whose titles `task` names (of
    /// `known`, each with its title) before it is planned. Opening is
    /// local, and an open record publishes its operations and can be read,
    /// so the task is planned in one go rather than opened, then planned
    /// again. Returns the records opened.
    fn open_named(
        &mut self,
        task: &str,
        apps: &[String],
        known: &[(DocumentRoute, Option<String>)],
    ) -> Vec<DocumentRoute> {
        let lower = task.to_lowercase();
        let mut named: Vec<&DocumentRoute> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for (route, title) in known {
            let Some(title) = title.as_deref().map(str::trim) else {
                continue;
            };
            let title = title.to_lowercase();
            if named.len() >= MOST_OPENED_BY_TITLE
                || title.chars().count() < 3
                || seen.contains(&title)
                || !apps.contains(&route.app_id)
                || !lower.contains(&title)
            {
                continue;
            }
            seen.push(title);
            named.push(route);
        }
        // A task about an app the room has one record of means that one,
        // titled or not (a spreadsheet has no title to name it by).
        for app in apps {
            if !crate::model::task_names_app(task, app)
                || named.iter().any(|route| route.app_id == *app)
                || self.apps.keys().any(|route| route.app_id == *app)
            {
                continue;
            }
            let mut records = known.iter().filter(|(route, _)| route.app_id == *app);
            if let (Some((only, _)), None) = (records.next(), records.next()) {
                named.push(only);
            }
        }
        let mut opened = Vec::new();
        for route in named {
            match self.open(&route.app_id, Some(&route.record)) {
                Ok(route) => opened.push(route),
                Err(error) => {
                    tracing::warn!(%error, record = %route.record, "agent: a named record did not open");
                }
            }
        }
        opened
    }

    /// What the records at `about` (of `open`, each with what it publishes)
    /// hold now: [`Self::snapshot`]s of the first [`MOST_SNAPSHOTS`], within
    /// [`MOST_SNAPSHOTS_CHARS`] together.
    fn snapshots(
        &mut self,
        about: &[DocumentRoute],
        open: &[(DocumentRoute, Vec<ActionDescriptor>)],
    ) -> Vec<String> {
        let mut snapshots = Vec::new();
        let mut left = MOST_SNAPSHOTS_CHARS;
        for route in about.iter().take(MOST_SNAPSHOTS) {
            let descriptors = open
                .iter()
                .find(|(open, _)| open == route)
                .map(|(_, descriptors)| descriptors.as_slice())
                .unwrap_or_default();
            for seen in self.snapshot(route, descriptors, left.min(MOST_SNAPSHOT_CHARS)) {
                left = left.saturating_sub(seen.chars().count());
                snapshots.push(seen);
            }
        }
        snapshots
    }

    /// `apps`, sorted, each with what a new record of it publishes
    /// ([`PlanContext::reference`]).
    fn reference(&mut self, mut apps: Vec<String>) -> Vec<(String, Vec<ActionDescriptor>)> {
        apps.sort();
        apps.into_iter()
            .filter_map(|app| {
                let descriptors = self.preview(&app)?;
                Some((app, descriptors))
            })
            .collect()
    }

    /// What a new record of `app` publishes, surveyed once in a record that
    /// is never published. `None` when the app could not be opened.
    fn preview(&mut self, app: &str) -> Option<Vec<ActionDescriptor>> {
        if let Some(descriptors) = self.previews.get(app) {
            return Some(descriptors.clone());
        }
        let record = ConfinedReplica::open(
            &self.catalogue,
            app,
            &self.room,
            &self.identity.delegation.agent_id,
            true,
            Seed::New,
        );
        let descriptors = match record {
            Ok(record) => record.survey_with_effects(),
            Err(error) => {
                tracing::warn!(%error, %app, "agent: an app could not be previewed");
                return None;
            }
        };
        if descriptors.is_empty() {
            return None;
        }
        self.previews.insert(app.to_owned(), descriptors.clone());
        Some(descriptors)
    }

    /// What the open record at `route` holds now, read with the first of
    /// [`SNAPSHOT_READS`] it publishes (of `descriptors`) on each resource
    /// that publishes it, summarised within `most` characters.
    fn snapshot(
        &mut self,
        route: &DocumentRoute,
        descriptors: &[ActionDescriptor],
        most: usize,
    ) -> Vec<String> {
        let takes_nothing = |descriptor: &&ActionDescriptor| {
            readable(descriptor) && descriptor.params.iter().all(|param| !param.required)
        };
        let Some(action) = SNAPSHOT_READS.iter().find(|action| {
            descriptors
                .iter()
                .filter(takes_nothing)
                .any(|descriptor| descriptor.address.action == **action)
        }) else {
            return Vec::new();
        };
        let mut left = most;
        let mut found = Vec::new();
        for descriptor in descriptors
            .iter()
            .filter(takes_nothing)
            .filter(|descriptor| descriptor.address.action == *action)
            .take(MOST_SNAPSHOT_READS)
        {
            if left < 200 {
                break;
            }
            let output = match self.read_now(route, &descriptor.address, &serde_json::Map::new()) {
                Ok(output) => output,
                Err(error) => {
                    tracing::debug!(%error, "agent: a record could not be read for its plan");
                    continue;
                }
            };
            if let Some(seen) =
                observation_within(&route.app_id, &descriptor.address, &output, left)
            {
                left = left.saturating_sub(seen.chars().count());
                found.push(format!("(document {}) {seen}", route.record));
            }
        }
        found
    }

    /// Run the read at `address` of the open record at `route`: only an
    /// operation the record publishes now that only reads it
    /// ([`readable`]), never confirmed for anyone.
    fn read_now(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ActionOutput, String> {
        let app = self
            .apps
            .get(route)
            .ok_or_else(|| "that document is not open".to_string())?;
        let descriptor = app
            .survey_with_effects()
            .into_iter()
            .find(|descriptor| descriptor.address == *address)
            .ok_or_else(|| format!("{} is not published now", address.action))?;
        if !readable(&descriptor) {
            return Err(format!(
                "{} changes or makes something; only reads run while planning",
                descriptor.action_label
            ));
        }
        self.invoke_as(route, address, arguments, false)
            .map(Option::unwrap_or_default)
            .map_err(|refusal| match refusal {
                actions::Refusal::Door(error) => format!("{}: {error}", error.code()),
                actions::Refusal::Agent(reason) => reason,
            })
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
                    record: ui::record::RecordId::parse(record)
                        .ok_or_else(|| format!("{record} is not a record id"))?,
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
            &self.identity.delegation.agent_id,
            true,
            seed,
        )
        .map_err(|error| error.to_string())?
        .with_content(self.content.clone());
        if let Some(seeded) = &seeded {
            self.unopened.forget(seeded);
        }
        let route = route(app.wire_record());
        if let Some(mutation) = app.take_mutation().map_err(|error| error.to_string())? {
            self.peer.publish(route.clone(), &mutation)?;
        }
        self.apps.insert(route.clone(), app);
        self.generation = self.generation.wrapping_add(1);
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
        tracing::trace!(app = %route.app_id, action = %address.action, ?arguments, "agent: invoke");
        let may_confirm = self.identity.delegation.scope.may_confirm;
        let started = tilo_clock::unix_now_ms();
        let outcome = self
            .invoke_as(route, address, arguments, may_confirm)
            .map_err(|refusal| match refusal {
                actions::Refusal::Door(error) => format!("{}: {error}", error.code()),
                actions::Refusal::Agent(reason) => reason,
            });
        record_action(
            &self.identity.delegation.agent_id,
            route,
            address,
            arguments,
            started,
            &outcome,
        );
        outcome
    }

    /// [`Self::invoke`], with `confirmed` standing for whoever confirmed:
    /// the delegation for the agent's own steps, the caller's explicit
    /// confirmation for a remote invocation.
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
        // A document operation is named without mounting the record's
        // component; only an effect's name needs it.
        let named = |descriptors: Vec<ActionDescriptor>| {
            descriptors
                .into_iter()
                .find(|descriptor| descriptor.address == *address)
        };
        let descriptor = named(app.survey()).or_else(|| named(app.survey_with_effects()));
        let label = descriptor.as_ref().map(|descriptor| {
            format!(
                "{} · {}",
                descriptor.action_label, descriptor.resource_label
            )
        });
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
        self.focus = Some(LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        self.place = app.presence();
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
        if let Some(finishing) = self.finishing.take() {
            self.control.finish(&finishing.id, finishing.outcome);
        }
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

/// What the task's `tilo.content:` addresses hold, as far as a plan needs
/// it without reading them: a picture's format and size.
fn content_facts(task: &str, content: &tilo_content::ContentStore) -> Vec<String> {
    const PREFIX: &str = "tilo.content:";
    let mut facts = Vec::new();
    for (at, _) in task.match_indices(PREFIX) {
        let hash: String = task
            .get(at.saturating_add(PREFIX.len())..)
            .unwrap_or_default()
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        let fact = content
            .read(&hash)
            .and_then(|bytes| picture_size(&bytes))
            .map(|(format, width, height)| {
                format!("{PREFIX}{hash} is a {format} picture {width}×{height} pixels.")
            });
        if let Some(fact) = fact
            && !facts.contains(&fact)
        {
            facts.push(fact);
        }
    }
    facts
}

/// A PNG's or JPEG's format and size, from its header.
fn picture_size(bytes: &[u8]) -> Option<(&'static str, u32, u32)> {
    let be16 = |at: usize| -> Option<u32> {
        Some(u32::from(u16::from_be_bytes([
            *bytes.get(at)?,
            *bytes.get(at.checked_add(1)?)?,
        ])))
    };
    let be32 = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(
            bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
        ))
    };
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(("PNG", be32(16)?, be32(20)?));
    }
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2_usize;
    while *bytes.get(at)? == 0xFF {
        let marker = *bytes.get(at.checked_add(1)?)?;
        // A start-of-frame segment holds the size; the others are skipped.
        if (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker) {
            return Some(("JPEG", be16(at.checked_add(7)?)?, be16(at.checked_add(5)?)?));
        }
        let length = usize::try_from(be16(at.checked_add(2)?)?).ok()?;
        at = at.checked_add(2)?.checked_add(length)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(app: &str, resource: &str, action: &str) -> Step {
        Step {
            app: app.into(),
            record: None,
            action: StepAction::Invoke {
                address: ActionAddress::new(resource, action),
                arguments: serde_json::Map::new(),
            },
            carries: true,
        }
    }

    fn exported(hash: &str) -> ActionOutput {
        ActionOutput {
            value: Some(serde_json::json!({"format": "png", "width": 1520})),
            resources: vec![tilo_actions::content_resource(hash).unwrap()],
        }
    }

    /// Export, open, read, insert: the insert is given what the export
    /// made, though a read (and an open) ran in between; a later export
    /// is what a later step is given.
    #[test]
    fn a_carried_step_is_given_what_the_latest_step_that_made_content_made() {
        let mut running = Running::new(
            "t1".into(),
            "export the artwork, then insert it in the launch page".into(),
            Vec::new(),
            vec!["read while planning".into()],
        );
        assert_eq!(running.observed.len(), 1);
        let canvas = "so.tilo.canvas";
        let writer = "so.tilo.word-processor";
        running.answered(&invoke(canvas, "canvas.artwork", "export"), exported("abc"));
        // Opening the page answers nothing; reading it answers text only.
        running.answered(
            &invoke(writer, "tilo.rich-text", "read-text"),
            ActionOutput {
                value: Some(serde_json::json!({"content": {"markdown": "# Launch page"}})),
                resources: Vec::new(),
            },
        );
        let bound = tilo_agent::plan::carried_arguments(
            &serde_json::Map::new(),
            true,
            running.last.as_ref(),
        )
        .unwrap();
        assert_eq!(bound[tilo_actions::PROMPTED_PARAM], "abc");
        assert!(
            running
                .observed
                .iter()
                .any(|seen| seen.contains("tilo.rich-text#read-text answered")),
            "{:?}",
            running.observed
        );

        running.answered(&invoke(canvas, "canvas.artwork", "export"), exported("def"));
        let bound = tilo_agent::plan::carried_arguments(
            &serde_json::Map::new(),
            true,
            running.last.as_ref(),
        )
        .unwrap();
        assert_eq!(bound[tilo_actions::PROMPTED_PARAM], "def");

        // Nothing produced yet: still refused.
        let fresh = Running::new("t2".into(), String::new(), Vec::new(), Vec::new());
        assert!(
            tilo_agent::plan::carried_arguments(&serde_json::Map::new(), true, fresh.last.as_ref())
                .is_err()
        );
    }
}

/// Appends one JSON line per invoked action to `TILO_AGENT_ACTION_LOG`, when
/// set: who did what to which record, with its arguments, when, and how it
/// went. A run can be replayed from it, every record rebuilt step by step.
fn record_action(
    agent: &str,
    route: &DocumentRoute,
    address: &ActionAddress,
    arguments: &serde_json::Map<String, serde_json::Value>,
    started_ms: u64,
    outcome: &Result<Option<ActionOutput>, String>,
) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("TILO_AGENT_ACTION_LOG") else {
        return;
    };
    let line = serde_json::json!({
        "started_unix_ms": started_ms,
        "ended_unix_ms": tilo_clock::unix_now_ms(),
        "agent": agent,
        "app": route.app_id,
        "record": route.record,
        "resource": address.resource,
        "action": address.action,
        "arguments": arguments,
        "ok": outcome.is_ok(),
        "error": outcome.as_ref().err(),
        "produced": outcome.as_ref().ok().and_then(Option::as_ref).map(produced).unwrap_or_default(),
        // The resources it answered with: a replay maps the ids it made (a
        // new sheet's) onto the ones the replay makes.
        "resources": outcome
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|output| serde_json::to_value(&output.resources).unwrap_or_default()),
        "value": outcome.as_ref().ok().and_then(Option::as_ref).and_then(|output| output.value.clone()),
    });
    // One write per line, as the usage log does: agents append at once.
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{line}\n").as_bytes()));
    if let Err(error) = written {
        tracing::warn!(%error, "cannot record an action");
    }
}
