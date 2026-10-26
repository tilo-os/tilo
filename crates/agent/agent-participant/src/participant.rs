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
//! its delegated apps, and never confirms by its own word: a step that asks
//! first is put to the person on their own device.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::apps::{AppCatalogue, Seed};
use crate::control::{AgentControl, AgentIdentity};
use crate::planner::{MESSAGE_APPS, Planner, Step, StepAction, written_steps};
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
mod authors;
mod bound;
mod context;
mod decompose;
mod facts;
mod introduce;
mod logs;
mod obligations;
mod parts;
mod planning;
mod records;
mod repairs;
mod room_content;
mod running;
pub(crate) mod unopened;
mod waits;

pub(crate) use unopened::{SharedDesktop, desktop_route};

use context::picture_head;
use logs::{record_action, record_ended, record_plan};
use running::{
    FollowUp, NEW_DOCUMENT_NOTE, Running, imports, named_without_documents, produced,
    unknown_action,
};

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

/// How many times a task plans again for news that arrived while it ran.
const MOST_NEWS_REPLANS: u32 = 3;

/// The most message records whose text a wait keeps from when it began, so
/// it can tell what arrived from what was there; a record past them counts
/// all its text as arrived.
const MOST_WAIT_RECORDS: usize = 32;

/// The most characters of what arrived a plan after a wait is shown: the
/// message itself, not the conversation around it.
const MOST_ARRIVED_CHARS: usize = 800;

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
    /// Whether a task this agent's model planned is checked against its
    /// request once its steps ran (the planner's profile says).
    checks_work: bool,
    /// The task last planned again after "nothing offered fits" opened the
    /// room's documents: once per task.
    reopened_for: Option<String>,
    /// When the plan the agent now carries out was ready, until its first
    /// step ran.
    plan_ready_at: Option<Instant>,
    apps: BTreeMap<DocumentRoute, ConfinedReplica>,
    /// The parts of a chain (a chat's messages) held in an open record,
    /// by the part's route: the holder's ([`parts`]).
    beside: BTreeMap<DocumentRoute, DocumentRoute>,
    /// The room's content store, lent to every record the agent holds.
    content: tilo_content::ContentStore,
    /// Records the agent has not mounted, each merged as it changes.
    unopened: unopened::Unopened,
    /// When this agent last asked the host to register its desk namespace
    /// (Unix milliseconds; 0: not yet).
    desk_claim_at_ms: u64,
    live: LiveState,
    focus: Option<LiveRecord>,
    /// Where in the focused record its last operation left it.
    place: Option<(LiveAnchor, LiveAnchor)>,
    presence_at_ms: u64,
    running: Option<Running>,
    /// The current task's step the person is asked to confirm.
    asking: Option<Asking>,
    /// What a new record of each app publishes, surveyed once in a record
    /// that is never published: the operations a plan may name before the
    /// app is open ([`PlanContext::previews`]).
    previews: BTreeMap<String, Vec<ActionDescriptor>>,
    /// How many changes a new record of each app holds (its genesis), read
    /// from its preview: a record holding no more is blank
    /// ([`Self::blank`]).
    genesis_changes: BTreeMap<String, u64>,
    /// The records the host opened new for the current task before its
    /// plan ran ([`Self::open_new`]), until its run takes them as made.
    task_made: Vec<DocumentRoute>,
    /// The room's records another member changed (or made), as their
    /// changes arrived: a written document among them is theirs
    /// ([`Self::others_document`]).
    others_wrote: BTreeSet<DocumentRoute>,
    /// The records this agent's own steps changed, in any task: a document
    /// it wrote in is its own, whoever else wrote there too.
    own_wrote: BTreeSet<DocumentRoute>,
    /// The exports whose answers stated the facts of what they exported,
    /// oldest first: what a later task's check compares the record with.
    exported: Vec<facts::Exported>,
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
    /// The records the current task's plans read while they were made: a
    /// follow-up plan is offered them, as it is the records it touched.
    task_read: Vec<DocumentRoute>,
    /// The current task's budget for asking a model call lost to the
    /// network again, shared by all of its plans.
    task_retries: crate::planner::TurnRetries,
    /// When others' changes last merged into an open record.
    last_merge: Option<Instant>,
    /// Message records ([`MESSAGE_APPS`]) others changed since the running
    /// task last planned, by title when known.
    news: Vec<DocumentRoute>,
    /// Who last changed each message record, by device: whom what a
    /// waiting task waited for came from.
    news_from: BTreeMap<DocumentRoute, [u8; 32]>,
    /// What each record of [`Self::news`] held before the change that made
    /// it news: what arrived since is what the re-plan is told.
    news_before: BTreeMap<DocumentRoute, Arc<str>>,
    left: bool,
    /// Bumped whenever what the open records publish may have changed; the
    /// revision remote catalogues are issued at.
    generation: u64,
    /// Remote-action frames waiting for an answer, with their senders.
    remote: Vec<([u8; 32], Vec<u8>)>,
    answered: actions::Answered,
    /// Whether the agent may show the room's own pictures to its model
    /// ([`PlanContext::room_pictures`]).
    room_pictures: crate::vision::RoomPictures,
    /// Whether each item of a task's checklist is done by an agent of its
    /// own ([`crate::profile::DECOMPOSE_ENV`]).
    mode: decompose::Mode,
    /// The running coordinated task's coordinator ([`decompose`]).
    coordinator: Option<decompose::Coordinator>,
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
        let checks_work = planner.checks_work();
        // Its planner's profile, or a setting naming this agent.
        let itemised = planner.decomposes()
            || crate::profile::named_on(
                crate::control::agent_id(&identity.delegation),
                &identity.delegation.label,
                std::env::var(crate::profile::DECOMPOSE_ENV).ok().as_deref(),
            );

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
            store,
        );
        let events = peer.events();
        let woken = Arc::clone(&control);
        peer.on_change(Arc::new(move || woken.wake()));
        peer.join(workspace_id)?;
        peer.live_join(workspace_id)?;
        let unopened = unopened::Unopened::new(&config.room);
        // The room's own pictures go to the model's provider only where the
        // person running the agent said so.
        let room_pictures =
            crate::vision::RoomPictures::from(crate::vision::room_pictures_allowed(
                crate::control::agent_id(&identity.delegation),
                &identity.delegation.label,
                std::env::var(crate::vision::ROOM_PICTURES_ENV)
                    .ok()
                    .as_deref(),
            ));
        Ok(Self {
            identity,
            control,
            peer,
            events,
            room: config.room,
            workspace_id,
            catalogue,
            planning,
            checks_work,
            reopened_for: None,
            plan_ready_at: None,
            apps: BTreeMap::new(),
            beside: BTreeMap::new(),
            content: config.content,
            unopened,
            desk_claim_at_ms: 0,
            live: LiveState::starting_now(),
            focus: None,
            place: None,
            presence_at_ms: 0,
            running: None,
            asking: None,
            previews: BTreeMap::new(),
            genesis_changes: BTreeMap::new(),
            task_made: Vec::new(),
            others_wrote: BTreeSet::new(),
            own_wrote: BTreeSet::new(),
            exported: Vec::new(),
            ready: false,
            settling: None,
            finishing: None,
            starting: None,
            keys_wait: None,
            task_touched: Vec::new(),
            previous_touched: Vec::new(),
            task_read: Vec::new(),
            task_retries: crate::planner::TurnRetries::default(),
            last_merge: None,
            news: Vec::new(),
            news_from: BTreeMap::new(),
            news_before: BTreeMap::new(),
            left: false,
            generation: 0,
            remote: Vec::new(),
            answered: actions::Answered::default(),
            room_pictures,
            mode: if itemised {
                decompose::Mode::Itemised
            } else {
                decompose::Mode::Whole
            },
            coordinator: None,
        })
    }

    /// Let this agent show the room's own pictures (scans, photos people
    /// put in the room) to its model's provider, or not; off unless
    /// [`crate::vision::ROOM_PICTURES_ENV`] turned it on. Pictures its apps
    /// render (a canvas look, a PDF page) are shown either way.
    pub fn allow_room_pictures(&mut self, on: bool) {
        self.room_pictures = on.into();
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
    /// `STEP_SLICE`, at most one that plans. Says when to pump again.
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
        if (self.focus.is_some() || self.waiting().is_some() || self.holding())
            && now.saturating_sub(self.presence_at_ms) >= PRESENCE_KEEPALIVE_MS
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
        // A task waiting for a message sleeps until news wakes the loop, its
        // wait runs out or its presence is due: never a pump, nor a plan.
        if let Some(until) = self.waiting().map(|waiting| waiting.until)
            && self.news.is_empty()
            && self.control.may_act()
        {
            let due = self.presence_at_ms.saturating_add(PRESENCE_KEEPALIVE_MS);
            let keepalive =
                Duration::from_millis(due.saturating_sub(tilo_clock::unix_now_ms()).max(1));
            let left = until.saturating_duration_since(Instant::now());
            return Next::Within(left.min(keepalive).max(Duration::from_millis(1)));
        }
        // A coordinated task with only waits left sleeps the same way.
        if let Some(until) = self.holds_until()
            && self.control.may_act()
        {
            let due = self.presence_at_ms.saturating_add(PRESENCE_KEEPALIVE_MS);
            let keepalive =
                Duration::from_millis(due.saturating_sub(tilo_clock::unix_now_ms()).max(1));
            let left = until.saturating_duration_since(Instant::now());
            return Next::Within(left.min(keepalive).max(Duration::from_millis(1)));
        }
        let work =
            self.running.is_some() || self.coordinator.is_some() || self.control.has_queued();
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
                let author = format!(
                    "agent:{}",
                    crate::control::agent_id(&self.identity.delegation)
                );
                self.claim_desk_namespace(crate::desk::salt_of(&author));
                if self.control.status() == AgentStatus::Starting {
                    self.control.set_status(AgentStatus::Waiting);
                }
            }
            PeerEvent::RoomChanged {
                workspace_id,
                room,
                changes,
            } if workspace_id == self.workspace_id && room == self.room => {
                self.unopened.receive_room(changes);
            }
            PeerEvent::Document(document) => {
                if document.author != self.identity.cert.device_public_key
                    && document.route.cluster_id == self.room
                    && MESSAGE_APPS.contains(&document.route.app_id.as_str())
                    && self.identity.delegation.permits_app(&document.route.app_id)
                    && (self.running.is_some()
                        || self.planning.busy()
                        || self.coordinator.is_some())
                {
                    self.news_from
                        .insert(document.route.clone(), document.author);
                    if !self.news.contains(&document.route) {
                        // Read before the change merges below.
                        let before = self.record_messages(&document.route).unwrap_or_default();
                        self.news_before.insert(document.route.clone(), before);
                        self.news.push(document.route.clone());
                    }
                    // What a coordinated task's waits look for, whatever
                    // takes the news.
                    self.before_others_change(&document.route);
                }
                if document.route.cluster_id != self.room {
                    return;
                }
                if document.author != self.identity.cert.device_public_key {
                    self.others_wrote.insert(document.route.clone());
                }
                let Some(mutation) = document.mutation() else {
                    return;
                };
                let route = document.route.clone();
                if parts::is_part(&route.record)
                    && let Some(holder) = self.merge_part(&route, &mutation)
                {
                    self.generation = self.generation.wrapping_add(1);
                    self.last_merge = Some(Instant::now());
                    self.live.durable_changed(&LiveRecord {
                        app: holder.app_id,
                        record: holder.record,
                    });
                } else if let Some(app) = self.apps.get_mut(&route) {
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
            PeerEvent::ContentWanted {
                workspace_id,
                from,
                request,
            } if workspace_id == self.workspace_id && from != self.device() => {
                self.content_wanted(from, &request);
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
        if self.coordinator.is_some() {
            self.take_in_arrivals();
        }
        if self.planning.busy() {
            // The planner's thread wakes the loop when the plan is ready.
            return false;
        }
        if self.running.is_none() {
            // Between a coordinated task's items, its coordinator carries it
            // on; otherwise the next task begins. Either is planned on the
            // planner's thread, and its plan taken when ready.
            if self.coordinator.is_some() {
                self.coordinate();
            } else {
                self.begin_task();
            }
            return false;
        }
        let Some(running) = self.running.as_ref() else {
            return false;
        };
        if !self.control.is_running(&running.id) {
            self.running = None;
            self.asking = None;
            self.coordinator = None;
            return false;
        }
        if self.asking.is_some() {
            return self.take_answer();
        }
        let Some(running) = self.running.as_mut() else {
            return false;
        };
        if running.waits.current.is_some() {
            self.check_wait();
            return false;
        }
        self.gather_news();
        let (coordinated, checks_work) = (self.coordinator.is_some(), self.checks_work);
        let Some(running) = self.running.as_mut().filter(|_| !self.planning.busy()) else {
            return false;
        };
        // A coordinated task's coordinator checks the whole once every item
        // ended; an item's agent is checked only to be shown the pictures
        // its steps rendered, which it would otherwise never see.
        running.plan_more(if coordinated {
            !running.looked.is_empty()
        } else {
            checks_work
        });
        if running.steps.is_empty() {
            running.remind_open();
        }
        let Some(step) = running.steps.pop_front() else {
            let (id, done) = (running.id.clone(), running.done());
            self.end_running(&Ok(done.clone()));
            self.finish_task(id, Ok(done));
            self.publish_presence();
            return false;
        };
        let id = running.id.clone();
        let (request, previous) = (running.request.clone(), running.last.clone());
        if let StepAction::Wait(wait) = step.action {
            self.begin_wait(wait);
            return false;
        }
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
        let generation = self.generation;
        let outcome = match self.perform(&step, previous.as_ref()) {
            Ok(Performed::Asking) => return false,
            Ok(Performed::Done(outcome)) => outcome,
            Err(error) => Err(error),
        };
        let edited = self.generation != generation;
        self.conclude(&id, &step, outcome, edited)
    }

    /// The person's answer to the step they were asked to confirm, once it
    /// arrived: the step runs confirmed by them, or fails unconfirmed.
    /// Returns whether another step that plans nothing is ready.
    fn take_answer(&mut self) -> bool {
        let Some(asking) = self.asking.take() else {
            return false;
        };
        let arrived = asking.answer.lock_recover().take();
        let Some(answer) = arrived else {
            self.asking = Some(asking);
            return false;
        };
        let Some(id) = self.running.as_ref().map(|running| running.id.clone()) else {
            return false;
        };
        let generation = self.generation;
        let outcome = match answer {
            // The person's own device answered for this invocation.
            Ok(tilo_actions::remote::Confirmation::Witnessed(_)) => self
                .invoke_step(&asking.route, &asking.address, &asking.arguments, true)
                .map_err(actions::Refusal::into_text),
            Ok(_) | Err(_) => Err(format!(
                "{}: the person did not confirm",
                tilo_actions::door::DoorError::ConfirmationRequired.code()
            )),
        };
        let edited = self.generation != generation;
        if edited {
            self.own_wrote.insert(asking.route.clone());
        }
        self.conclude(&id, &asking.step, outcome, edited)
    }

    /// Record how `step` of task `id` went (`edited` when it changed its
    /// record) and carry on: a failed step is planned around while the
    /// task's repairs last ([`repairs::Repairs`]), else it ends the task.
    /// Returns whether another step that plans nothing is ready.
    fn conclude(
        &mut self,
        id: &str,
        step: &Step,
        outcome: Result<Option<ActionOutput>, String>,
        edited: bool,
    ) -> bool {
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
        self.control.record_step(id, record);
        match outcome {
            Ok(output) => {
                if let Some(output) = &output {
                    self.note_export(id, step, output);
                }
                let Some(running) = self.running.as_mut() else {
                    return false;
                };
                running.ran_step(step, output, content, edited);
                // The next step plans nothing unless it is a `Continue`.
                !matches!(
                    running.steps.front().map(|step| &step.action),
                    Some(StepAction::Continue(_))
                )
            }
            Err(error) => {
                if self.repair(step, &error) {
                    return false;
                }
                self.fail_running(id.to_owned(), error);
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
        self.task_read.clear();
        self.task_made.clear();
        self.task_retries = crate::planner::TurnRetries::default();
        // What arrived before the task began is in the room it is planned on.
        self.news.clear();
        self.news_from.clear();
        self.news_before.clear();
        if !self.begin_coordinated(&id, &text) {
            self.plan(id, &text, None);
        }
    }

    /// Plan `text` for task `id`: a new task, or the rest of the running one
    /// when `request` (the task as it was given) comes beside it. Steps
    /// written out in the task are taken at once; anything else goes to the
    /// planner's thread, and the plan is taken when it wakes the loop.
    /// After a first plan found nothing that fits `text`, open the room's
    /// documents of the apps it may use (those the task names first) and
    /// plan it once more, so a model sees their operations rather than
    /// giving up on a document it never opened. Returns whether it did.
    fn reopen_for(&mut self, task: &str, text: &str) -> bool {
        const MOST_OPENED: usize = 4;
        if self.reopened_for.as_deref() == Some(task) {
            return false;
        }
        // Another member's written document the task does not name is not
        // opened: its operations would be the only ones of its app on the
        // list, and an import or a write planned there lands in it.
        let mut routes: Vec<DocumentRoute> = Vec::new();
        for route in self.unopened.people_first() {
            if self.identity.delegation.permits_app(&route.app_id)
                && !self.apps.contains_key(&route)
                && !parts::is_part(&route.record)
                && !self.others_document(&route, text)
            {
                routes.push(route);
            }
        }
        routes.sort_by_key(|route| !crate::model::task_names_app(text, &route.app_id));
        routes.truncate(MOST_OPENED);
        // An app the task names that has no document in the room yet: a
        // model that took "write it in the Word Processor" to need one
        // opened first is told its operations make the document.
        let offered = self.unopened.people_first();
        let held: Vec<&str> = self
            .apps
            .keys()
            .chain(&offered)
            .map(|route| route.app_id.as_str())
            .collect();
        let granted: Vec<&str> = self
            .catalogue
            .iter()
            .map(|definition| definition.app_id)
            .filter(|app| self.identity.delegation.permits_app(app))
            .collect();
        let fresh = named_without_documents(text, &granted, &held);
        if routes.is_empty() && fresh.is_empty() {
            return false;
        }
        self.reopened_for = Some(task.to_owned());
        for route in routes {
            if let Err(error) = self.open(&route.app_id, Some(&route.record)) {
                tracing::debug!(%error, app = %route.app_id, "agent: a room document did not open");
            }
        }
        if fresh.is_empty() {
            self.plan(task.to_owned(), text, None);
        } else {
            let told = format!("{text}\n\n{NEW_DOCUMENT_NOTE} {})", fresh.join(", "));
            self.plan(task.to_owned(), &told, None);
        }
        true
    }

    fn plan(&mut self, id: String, text: &str, request: Option<String>) {
        let continues = request.is_some();
        // The plan after a wait is being made: what arrives from now on is
        // news to it.
        if let Some(running) = self.running.as_mut() {
            running.waits.woken = None;
        }
        let mut context = self.plan_context(text, request);
        // A check, or the turn after reads, may find nothing left to do.
        context.may_plan_nothing = continues
            && self.running.as_ref().is_some_and(|running| {
                running.id == id
                    && (running.waits.replan
                        || matches!(
                            running.follow_ups.turn,
                            Some(FollowUp::Checking | FollowUp::AfterReads | FollowUp::Obligations)
                        ))
            });
        self.shape_context(&mut context, continues);
        let role = self.plan_role();
        if let Some(steps) = written_steps(text, &context) {
            self.apply_plan(planning::Planned {
                task: id,
                text: text.to_owned(),
                continues,
                steps,
                read: Vec::new(),
                rendered: Vec::new(),
                ready_at: Instant::now(),
                by_model: false,
                obligations: crate::planner::Obligations::default(),
                role,
            });
            return;
        }
        if let Err(error) = self.planning.ask(
            (id.clone(), text.to_owned()),
            context,
            continues,
            role.clone(),
        ) {
            self.apply_plan(planning::Planned {
                task: id,
                text: text.to_owned(),
                continues,
                steps: Err(error),
                read: Vec::new(),
                rendered: Vec::new(),
                ready_at: Instant::now(),
                by_model: false,
                obligations: crate::planner::Obligations::default(),
                role,
            });
        }
    }

    /// Answer the reads the plan being made asks of the open records, and
    /// the content it asks to show its model from the room's store.
    fn answer_reads(&mut self) {
        for ask in self.planning.asked_reads() {
            let answer = self.read_now(&ask.route, &ask.address, &ask.arguments);
            if answer.is_ok() && !self.task_read.contains(&ask.route) {
                self.task_read.push(ask.route.clone());
            }
            let _ = ask.reply.send(answer);
        }
        for ask in self.planning.asked_looks() {
            let answer = self
                .content
                .open(&ask.hash)
                .ok_or_else(|| format!("the room keeps no content tilo.content:{}", ask.hash))
                .and_then(|reader| picture_head(reader, &ask.hash));
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
    /// done, not failed. A coordinator's turn goes to it.
    fn apply_plan(&mut self, planned: planning::Planned) {
        if self.coordinators_turn(&planned.task) {
            self.apply_coordinator(planned);
            return;
        }
        self.record_plan(&planned);
        if let Some(planned) = self.item_answer(planned) {
            self.apply_steps(planned);
        }
    }

    /// [`Self::apply_plan`], once logged: a task's or an item's steps.
    fn apply_steps(&mut self, planned: planning::Planned) {
        let planning::Planned {
            task,
            text,
            continues,
            steps,
            read,
            rendered,
            ready_at,
            by_model,
            obligations,
            role: _,
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
                let mut running = Running::new(task, text, steps, read);
                running.made = std::mem::take(&mut self.task_made);
                running.by_model = by_model;
                running.checklist.adopt(obligations, false);
                self.seed_item(&mut running);
                running.looked_at(rendered);
                self.running = Some(running);
                self.plan_ready_at = Some(ready_at);
            }
            Ok(steps) => {
                if let Some(running) = self.running.as_mut().filter(|_| running_it) {
                    running.made.append(&mut self.task_made);
                    running.by_model |= by_model;
                    running.adopt_obligations(obligations);
                    for (app, seen) in read {
                        running.observe(app, seen);
                    }
                    running.looked_at(rendered);
                    running.queue_answer(steps);
                    self.plan_ready_at = Some(ready_at);
                }
            }
            // News that changes nothing for the task is no reason to end
            // it: a re-plan for it that found nothing to do goes on with
            // the steps it replaced, a wait among them waiting again (at
            // most MOST_NEWS_REPLANS re-plans, and MOST_WAITS waits).
            Err(error)
                if continues
                    && running_it
                    && error.starts_with(NOTHING_LEFT)
                    && self
                        .running
                        .as_mut()
                        .is_some_and(Running::resume_after_news) =>
            {
                tracing::debug!(%error, "agent: news changed nothing; the task goes on");
            }
            Err(error) => {
                if !continues && error.starts_with(NOTHING_LEFT) && self.reopen_for(&task, &text) {
                    return;
                }
                // A first plan's "nothing fits" that said what to do was
                // planned again with that (crate::model::PLAN_WHAT_IT_SAID);
                // found nothing again, it gets the re-open a first plan gets.
                if continues
                    && error.starts_with(NOTHING_LEFT)
                    && text.starts_with(crate::model::PLAN_WHAT_IT_SAID)
                    && let Some(request) = self
                        .running
                        .as_ref()
                        .filter(|running| running.id == task && running.acted == 0)
                        .map(|running| running.request.clone())
                    && self.reopen_for(&task, &request)
                {
                    return;
                }
                if continues
                    && running_it
                    && error.starts_with(NOTHING_LEFT)
                    && self.running.as_mut().is_some_and(Running::check_left)
                {
                    return;
                }
                // A task done but for its open obligations is planned for
                // them before it finishes, as one whose steps ran out is.
                let done = continues
                    && running_it
                    && self
                        .running
                        .as_ref()
                        .is_some_and(|running| running.done_despite(&error));
                if done && self.running.as_mut().is_some_and(Running::remind_open) {
                    return;
                }
                let finished = if done {
                    self.running.as_ref().map(Running::done)
                } else {
                    None
                };
                let outcome = finished.ok_or_else(|| self.unfinished(&task, error));
                if running_it {
                    self.end_running(&outcome);
                }
                self.finish_task(task, outcome);
            }
        }
    }

    /// Log `planned` (`TILO_AGENT_PLAN_LOG`), with the follow-up turn it
    /// answers and the checklist it was shown.
    fn record_plan(&self, planned: &planning::Planned) {
        // A coordinator's turn is shown the items, not an item's checklist.
        let coordinating = planned
            .role
            .as_ref()
            .is_some_and(|role| role.role == "coordinator");
        let continued = self
            .running
            .as_ref()
            .filter(|running| running.id == planned.task && planned.continues && !coordinating);
        let turn = continued.and_then(|running| running.follow_ups.turn);
        let repair = continued
            .filter(|_| turn == Some(FollowUp::Repairing))
            .and_then(|running| running.repairs.logged_latest());
        let shown = if coordinating {
            self.coordinator
                .as_ref()
                .map(decompose::Coordinator::logged)
        } else {
            continued
                .filter(|running| !running.checklist.is_empty())
                .map(|running| running.checklist.logged())
        };
        record_plan(&logs::PlanLine {
            agent: crate::control::agent_id(&self.identity.delegation),
            task: &planned.task,
            text: &planned.text,
            continues: planned.continues,
            turn,
            repair,
            by_model: planned.by_model,
            steps: &planned.steps,
            declared: &planned.obligations,
            shown,
            role: planned.role.as_ref(),
        });
    }

    /// Plan the rest of the running task again after `step` failed with
    /// `error`, knowing what failed and why, beside the request, what ran
    /// so far and the obligations still open, while its repairs last
    /// ([`repairs::Repairs::admit`]). Returns whether it will.
    fn repair(&mut self, step: &Step, error: &str) -> bool {
        // An import the record refused (it holds content an import would
        // replace) wants a new document: the plans after it are offered one.
        if imports(step)
            && let Some(running) = self.running.as_mut()
            && !running.new_documents.contains(&step.app)
        {
            running.new_documents.push(step.app.clone());
        }
        // Steps written out run as written: only a model's slip is planned
        // around.
        let Some(running) = self.running.as_mut().filter(|running| running.by_model) else {
            return false;
        };
        let count = match running.repairs.admit(step, error) {
            Ok(count) => count,
            Err(reason) => {
                tracing::info!(task = %running.id, ?reason, %error, "agent: a failed step ends its task");
                return false;
            }
        };
        tracing::debug!(task = %running.id, count, %error, "agent: a failed step is planned around");
        // Its plan runs no operation again as it ran ([`Running::queue_rest`]).
        running.follow_ups.take(FollowUp::Repairing);
        running.steps.clear();
        running.steps.push_back(Step {
            app: step.app.clone(),
            record: None,
            action: StepAction::Continue(format!(
                "The step {} failed: {error}. Finish the request with steps that work, without \
                 repeating the steps that already ran.{}",
                step.log_names().1,
                if running.ran.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Already done, and kept: {}. Do not do any of them again.",
                        running.ran.join("; ")
                    )
                }
            )),
            carries: None,
            serves: None,
        });
        true
    }

    /// Drop the running task, which ended with `outcome`, logging its
    /// checklist and repairs when it kept one or had any.
    fn end_running(&mut self, outcome: &Result<String, String>) {
        let Some(running) = self.running.take() else {
            return;
        };
        if !running.checklist.is_empty() || running.repairs.any() {
            record_ended(
                crate::control::agent_id(&self.identity.delegation),
                &running.id,
                (outcome, self.plan_role().as_ref()),
                (&running.checklist.logged(), &running.repairs.logged()),
            );
        }
        // An item's task is kept until its coordinator takes in what it did.
        if let Some(coordinator) = self.coordinator.as_mut() {
            coordinator.ended = Some(running);
        }
    }

    /// `error`, which ends task `id`, with what the task's checklist still
    /// held open and set aside, as a finished task's result names them.
    fn unfinished(&self, id: &str, error: String) -> String {
        let outcome = self
            .running
            .as_ref()
            .filter(|running| running.id == id)
            .map(|running| running.checklist.outcome())
            .unwrap_or_default();
        if outcome.is_empty() {
            error
        } else {
            format!("{}.{outcome}", error.trim_end_matches('.'))
        }
    }

    /// End the running task `id`, failed with `error` and what it left
    /// undone.
    fn fail_running(&mut self, id: String, error: String) {
        let failed = Err(self.unfinished(&id, error));
        self.end_running(&failed);
        self.finish_task(id, failed);
    }

    /// End task `id` with `outcome` once the host has acknowledged the
    /// changes it made: published now, it is reported when none is pending
    /// ([`Self::finish_committed`]), so whoever is told it is done finds
    /// its changes in the room.
    fn finish_task(&mut self, id: String, outcome: Result<String, String>) {
        self.publish_changes();
        // An item of a coordinated task ended, not the task.
        if self.item_ended(&id, &outcome) {
            return;
        }
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
        for route in self
            .previous_touched
            .iter()
            .chain(&self.coordinated_records())
        {
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
    /// An operation that asks first is put to the person
    /// ([`Performed::Asking`]); an error is a step that could not start.
    fn perform(
        &mut self,
        step: &Step,
        previous: Option<&ActionOutput>,
    ) -> Result<Performed, String> {
        // What a carried step consumes is settled before anything opens: a
        // step with nothing to consume is refused, not half done.
        let arguments = match &step.action {
            StepAction::Invoke { address, arguments } => Some(
                tilo_agent::plan::carried_arguments(arguments, step.carries.as_deref(), previous)
                    .map_err(|error| {
                    format!(
                        "{} takes what the step before it produced: {error}",
                        address.action
                    )
                })?,
            ),
            StepAction::Open | StepAction::Continue(_) | StepAction::Wait(_) => None,
        };
        // Content a step before it made, which the plan could not name.
        let arguments = arguments.map(|mut arguments| {
            if let Some(latest) = self
                .running
                .as_ref()
                .and_then(|running| running.latest_from_elsewhere(&step.app))
            {
                tilo_agent::plan::bind_latest_content(&mut arguments, latest);
            }
            arguments
        });
        let route = match &step.record {
            // An import replaces what its record holds, so the app refuses
            // one into a record that holds anything: it goes where an
            // import naming no record goes (a blank record the task made,
            // else a new one) rather than failing on whichever document the
            // plan found it offered on.
            Some(record) if imports(step) && self.written(&step.app, record) => {
                self.unnamed_record(step)?
            }
            Some(record) => self.open(&step.app, Some(record))?,
            None if matches!(step.action, StepAction::Open) => self.new_record(&step.app)?,
            None => self.unnamed_record(step)?,
        };
        if let Some(running) = self.running.as_mut()
            && !running.touched.contains(&route)
        {
            running.touched.push(route.clone());
        }
        if !self.task_touched.contains(&route) {
            self.task_touched.push(route.clone());
        }
        let (StepAction::Invoke { address, .. }, Some(arguments)) = (&step.action, arguments)
        else {
            return Ok(Performed::Done(Ok(None)));
        };
        let (settled, arguments) = self.settle(&route, address, step.carries.as_deref(), arguments);
        let address = &settled;
        // Values the plan quotes or computes (`{{…}}`) are the record's as
        // the step runs: what the action log records, while the plan log
        // keeps what the model wrote.
        let arguments = self.bind_values(step, &route, address, arguments)?;
        let generation = self.generation;
        let outcome = self.invoke_step(&route, address, &arguments, false);
        if self.generation != generation {
            self.own_wrote.insert(route.clone());
        }
        Ok(match outcome {
            Err(actions::Refusal::Door(tilo_actions::door::DoorError::ConfirmationRequired)) => {
                self.ask(step, route, address, &arguments)
            }
            outcome => Performed::Done(outcome.map_err(actions::Refusal::into_text)),
        })
    }

    /// The operation `address` names on the record at `route`, as the record
    /// offers it, and `arguments` for it. A step may name the resource by its
    /// kind (`chat.messages`, whose messages live in a part of their own) or
    /// by a field of the record that publishes no resource of its own
    /// (`word-processor.document/body`): the one operation of that name
    /// offered there is meant. Content carried under the generic prompt
    /// (`carries: true` on a record not open when it was planned) goes under
    /// the operation's content parameter.
    fn settle(
        &self,
        route: &DocumentRoute,
        address: &ActionAddress,
        carries: Option<&str>,
        mut arguments: serde_json::Map<String, serde_json::Value>,
    ) -> (ActionAddress, serde_json::Map<String, serde_json::Value>) {
        let Some(app) = self.apps.get(route) else {
            return (address.clone(), arguments);
        };
        let offered = app.survey();
        let only = |matches: &mut dyn Iterator<Item = &ActionDescriptor>| match (
            matches.next(),
            matches.next(),
        ) {
            (Some(one), None) => Some(one.clone()),
            _ => None,
        };
        let named = offered
            .iter()
            .find(|descriptor| descriptor.address == *address)
            .cloned()
            .or_else(|| {
                only(&mut offered.iter().filter(|descriptor| {
                    descriptor.address.action == address.action
                        && descriptor.resource_kind == address.resource
                }))
            })
            .or_else(|| {
                let (parent, _) = address.resource.rsplit_once('/')?;
                only(&mut offered.iter().filter(|descriptor| {
                    descriptor.address.action == address.action
                        && descriptor.address.resource == parent
                }))
            });
        let Some(descriptor) = named else {
            return (address.clone(), arguments);
        };
        if carries == Some(tilo_actions::PROMPTED_PARAM)
            && !descriptor
                .params
                .iter()
                .any(|param| param.name == tilo_actions::PROMPTED_PARAM)
            && let Some(param) = descriptor.params.iter().find(|param| param.is_content())
            && let Some(value) = arguments.remove(tilo_actions::PROMPTED_PARAM)
        {
            arguments.entry(param.name.to_string()).or_insert(value);
        }
        (descriptor.address, arguments)
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

    /// Commit whatever the open records changed since the last commit,
    /// the parts they hold beside them included.
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
        let holders: Vec<DocumentRoute> = self.apps.keys().cloned().collect();
        for holder in holders {
            if let Err(error) = self.publish_parts(&holder) {
                tracing::warn!(%error, "agent: a part's change could not be committed");
            }
        }
    }

    /// Records of the room the agent has seen but not opened.
    #[must_use]
    pub fn known_records(&self) -> Vec<DocumentRoute> {
        self.unopened
            .routes()
            .into_iter()
            .filter(|route| !parts::is_part(&route.record))
            .collect()
    }

    /// Open `app_id` on `record` (a record the room already has), or on a
    /// new record it publishes; an app that keeps one record per room opens
    /// that one either way. Returns the record's route.
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
        // An app that keeps one record per room (Chat) has one route, named
        // empty: asked for a new record, the agent joins the room's own once
        // the room has it, so every agent sees what the others wrote there.
        let room_record = self
            .catalogue
            .get(app_id)
            .is_some_and(ui::application::ApplicationDefinition::keeps_room_record);
        // A part of a chain is held beside its record, never opened: asked
        // for one, the agent opens the record that keeps it.
        let record = record
            .filter(|record| !parts::is_part(record))
            .or_else(|| room_record.then_some(""));
        let (seed, seeded) = match record {
            Some(record) => {
                let route = route(record);
                if self.apps.contains_key(&route) {
                    return Ok(route);
                }
                match self.unopened.seed(&route) {
                    Some(saved) => {
                        let seed = Seed::Existing {
                            record: ui::record::RecordId::from_route(record)
                                .ok_or_else(|| format!("{record} is not a record route"))?,
                            changes: vec![saved?],
                        };
                        (seed, Some(route))
                    }
                    // Nobody has written the room's record yet: its genesis
                    // is the same on every member, so this one converges
                    // with whoever makes it too.
                    None if room_record && record.is_empty() => (Seed::New, None),
                    None => return Err(format!("the room has no record {record} of {app_id} yet")),
                }
            }
            None => (Seed::New, None),
        };
        // Lent the room's content store, so an export keeps its content in
        // the room and content an operation names reaches the worker.
        let app = ConfinedReplica::open(
            &self.catalogue,
            app_id,
            &self.room,
            crate::control::agent_id(&self.identity.delegation),
            &crate::ReplicaActor::Agent,
            seed,
        )
        .map_err(|error| error.to_string())?
        .lending(self.content.clone());
        if let Some(seeded) = &seeded {
            self.unopened.forget(seeded);
        }
        let route = route(app.wire_record());
        if let Some(mutation) = app.take_mutation().map_err(|error| error.to_string())? {
            self.peer.publish(route.clone(), &mutation)?;
        }
        self.apps.insert(route.clone(), app);
        self.hold_waiting_parts(&route);
        self.introduce_in_chat(&route);
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
        self.invoke_step(route, address, arguments, false)
            .map_err(actions::Refusal::into_text)
    }

    /// [`Self::invoke_as`], logged to `TILO_AGENT_ACTION_LOG`
    /// ([`record_action`]) with how it went.
    fn invoke_step(
        &mut self,
        route: &DocumentRoute,
        address: &ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
    ) -> Result<Option<ActionOutput>, actions::Refusal> {
        tracing::trace!(app = %route.app_id, action = %address.action, ?arguments, "agent: invoke");
        let started = tilo_clock::unix_now_ms();
        let outcome = self.invoke_as(route, address, arguments, confirmed);
        let logged = match &outcome {
            Ok(output) => Ok(output.clone()),
            Err(actions::Refusal::Door(error)) => Err(format!("{}: {error}", error.code())),
            Err(actions::Refusal::Agent(reason)) => Err(reason.clone()),
        };
        record_action(
            crate::control::agent_id(&self.identity.delegation),
            route,
            address,
            arguments,
            started,
            &logged,
        );
        outcome
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
        // A document operation is named without mounting the record's
        // component; only an effect's name needs it.
        let named = |descriptors: Vec<ActionDescriptor>| {
            descriptors
                .into_iter()
                .find(|descriptor| descriptor.address == *address)
        };
        let descriptor = named(app.survey());
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
        // A render (`tilo_actions::HOST_RENDER_TAG`) is answered by the
        // host's render service from the record the host holds, never by the
        // record's worker, which has no GPU or media stack: the replica
        // routes it there (`crate::render::route`) when the host found the
        // helper, and its survey offers it only then.
        let output = match app.invoke(address, arguments, confirmed) {
            Err(tilo_actions::DoorError::Unknown(_)) => {
                // Say what the record does offer, so the next plan names it.
                return Err(Refusal::Agent(unknown_action(address, &app.survey())));
            }
            outcome => outcome.map_err(Refusal::Door)?,
        };
        let output = running::content_named_in_value(output);
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
        // A post goes to the chat's head part, held beside the record.
        self.publish_parts(route).map_err(Refusal::Agent)?;
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
    /// the people see where it acts ([`crate::desk`]). The view is this
    /// agent's own entries in the room's document, admitted by the host
    /// within the namespace it claims first.
    fn show_on_desk(&mut self, route: &DocumentRoute) {
        let Some(record) = self
            .apps
            .get(route)
            .map(|app| app.record_id().as_str().to_owned())
        else {
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
            .and_then(|last| {
                self.apps
                    .get(&last)
                    .map(|app| app.record_id().as_str().to_owned())
            });
        let title = self
            .catalogue
            .get(&route.app_id)
            .map_or(route.app_id.as_str(), |definition| definition.title);
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
        let salt = crate::desk::salt_of(&author);
        let Some(desktop) = self.unopened.desktop() else {
            return;
        };
        let claimed = tilo_desktop_model::projection::contents_owners_of(desktop)
            .get(&salt)
            .is_some_and(|device| {
                device.eq_ignore_ascii_case(&hex::encode(self.identity.cert.device_public_key))
            });
        if !claimed {
            self.claim_desk_namespace(salt);
            return;
        }
        let Some(changes) = crate::desk::place_view(desktop, &author, &view) else {
            return;
        };
        self.unopened.receive_room(changes.clone());
        if let Err(error) = self
            .peer
            .publish_room_changes(self.workspace_id, &self.room, changes)
        {
            tracing::warn!(%error, "agent: its view did not reach the room");
        }
    }

    /// Ask the host to register the namespace this agent's desk entries are
    /// minted in. Asked again while the room has not admitted it.
    fn claim_desk_namespace(&mut self, salt: u16) {
        let now = tilo_clock::unix_now_ms();
        if now < self.desk_claim_at_ms {
            return;
        }
        self.desk_claim_at_ms = now.saturating_add(5_000);
        let request = tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: "tilo.member".to_owned(),
                address: tilo_actions::ActionAddress::new(
                    format!("workspace.member:{}", self.room),
                    "topology-namespace.claim",
                ),
            },
            argument: tilo_actions::remote::InvokeArgument::Json(
                [("salt".to_owned(), serde_json::json!(salt))]
                    .into_iter()
                    .collect(),
            ),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let Ok(frame) = tilo_actions::remote::RemoteActionFrame::new(
            tilo_actions::remote::RemoteActionMessage::Invoke(request),
        )
        .encode() else {
            return;
        };
        if let Err(error) =
            self.peer
                .send_remote_action(self.workspace_id, self.peer.host_device(), frame)
        {
            tracing::warn!(%error, "agent: its desk namespace claim did not go");
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
        self.coordinator = None;
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

#[cfg(test)]
mod tests;
