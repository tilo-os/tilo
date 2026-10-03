//! The agent in the room: a certified participant beside browsers and
//! native devices.
//!
//! [`AgentParticipant`] holds a certified relay session under the agent's own
//! device key and joins the room's durable and live planes. It mounts the
//! apps it works in ([`HostedApp`]), acts only through their published
//! actions, and commits each resulting change through the session, so the
//! host orders it with everyone else's and every browser receives it as an
//! ordinary durable change attributed to the agent. Around each action it
//! publishes presence — the record it focuses, the control it points at,
//! and a [`LiveParticipant`] of kind agent with its label, colour and
//! activity — so every participant draws its cursor. Other participants'
//! presence arrives the same way and is readable through
//! [`AgentParticipant::remote_members`].
//!
//! It never acts while paused or stopped ([`AgentControl`]), never outside
//! its delegated apps, and satisfies a confirmation step only when its
//! delegation allows it.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use tilo_actions::ActionOutput;
use tilo_actions::door::{ActionAddress, ActionDescriptor};
use tilo_browser_peer::{
    DocumentRoute, Host, LiveState, MemoryPeerStore, PeerConfig, PeerEvent, PeerEvents, PeerHandle,
    RemoteMember,
};
use tilo_relay_protocol::agents::{AgentStatus, AgentStep, TaskState};
use tilo_relay_protocol::live::{LiveParticipant, LiveRecord, ParticipantKind};
use tilo_user_identity::{DeviceCert, RevocationList};
use ui::headless::ControlInput;
use ui::widget::ResourceOperationStart;

use crate::apps::{AppCatalogue, HostedApp, Seed};
use crate::control::{AgentControl, AgentIdentity};
use crate::planner::{PlanContext, Planner, Step, StepAction};

pub mod actions;

/// Presence is repeated this often while nothing changes, so late joiners
/// and lossy carriers still show the agent.
const PRESENCE_KEEPALIVE_MS: u64 = 1_000;

/// Where the agent's session goes.
pub struct ParticipantConfig {
    /// The room's host.
    pub host: Host,
    /// The host's certificate.
    pub host_cert: DeviceCert,
    /// The room uuid.
    pub room: String,
}

struct Running {
    id: String,
    steps: VecDeque<Step>,
    performed: usize,
}

/// The route of a room's shared browser desktop (`tilo-browser-runtime`'s
/// replication): its `desktop` value names each tab's record.
const DESKTOP_APP: &str = "so.tilo.browser-desktop";
const DESKTOP_RECORD: &str = "desktop";

/// The records bound to a tab of `room`'s shared desktop: what people have
/// open.
pub(crate) fn desktop_records(
    durable: &BTreeMap<DocumentRoute, Vec<Vec<u8>>>,
    room: &str,
) -> std::collections::HashSet<String> {
    use automerge::ReadDoc as _;
    use tilo_automerge::ApplyIncoming as _;
    let route = DocumentRoute {
        app_id: DESKTOP_APP.to_owned(),
        cluster_id: room.to_owned(),
        record: DESKTOP_RECORD.to_owned(),
    };
    let Some(changes) = durable.get(&route) else {
        return std::collections::HashSet::new();
    };
    let mut doc = automerge::AutoCommit::new();
    for bytes in changes {
        let _ = doc.load_incoming(bytes);
    }
    let Ok(Some((automerge::Value::Scalar(value), _))) = doc.get(automerge::ROOT, "desktop") else {
        return std::collections::HashSet::new();
    };
    let automerge::ScalarValue::Str(text) = value.as_ref() else {
        return std::collections::HashSet::new();
    };
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|desktop| desktop.get("records").and_then(|r| r.as_array()).cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|pair| pair.get(1).and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// `known` ordered for the planner: records open on the desktop first, then
/// the most recently changed, so "the notes" means the notes people see.
fn people_first(
    mut known: Vec<DocumentRoute>,
    on_desktop: &std::collections::HashSet<String>,
    arrived: &std::collections::HashMap<DocumentRoute, u64>,
) -> Vec<DocumentRoute> {
    known.sort_by_key(|route| {
        (
            !on_desktop.contains(&route.record),
            std::cmp::Reverse(arrived.get(route).copied().unwrap_or(0)),
        )
    });
    known
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
    planner: Box<dyn Planner>,
    apps: BTreeMap<DocumentRoute, HostedApp>,
    /// Durable changes of records the agent has not mounted, in host order.
    durable: BTreeMap<DocumentRoute, Vec<Vec<u8>>>,
    /// When each unmounted record last changed, as a count of changes seen.
    arrived: std::collections::HashMap<DocumentRoute, u64>,
    changes_seen: u64,
    live: LiveState,
    focus: Option<LiveRecord>,
    pointer: Option<[f32; 2]>,
    presence_at_ms: u64,
    running: Option<Running>,
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
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
        let peer = PeerHandle::start(
            PeerConfig {
                host: config.host,
                cert: identity.cert.clone(),
                host_cert: config.host_cert,
                revocations: RevocationList::new(),
            },
            Arc::new(identity.key.clone()),
            Arc::new(MemoryPeerStore::default()),
        );
        let events = peer.events();
        peer.join(workspace_id)?;
        peer.live_join(workspace_id)?;
        Ok(Self {
            identity,
            control,
            peer,
            events,
            room: config.room,
            workspace_id,
            catalogue,
            planner,
            apps: BTreeMap::new(),
            durable: BTreeMap::new(),
            arrived: std::collections::HashMap::new(),
            changes_seen: 0,
            live: LiveState::starting_now(),
            focus: None,
            pointer: None,
            presence_at_ms: 0,
            running: None,
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

    /// Take in what the session delivered, then take at most one step of
    /// the current task. Call it from the agent's own loop.
    pub fn pump(&mut self) {
        let now = now_ms();
        for event in self.events.drain() {
            self.receive(event, now);
        }
        if self.left {
            return;
        }
        if self.control.status() == AgentStatus::Stopped {
            self.leave();
            return;
        }
        for (from, frame) in std::mem::take(&mut self.remote) {
            self.answer_remote_action(from, &frame);
        }
        self.step();
        self.publish_changes();
        if self.focus.is_some() && now.saturating_sub(self.presence_at_ms) >= PRESENCE_KEEPALIVE_MS
        {
            self.publish_presence();
        }
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
                    let bytes = match mutation {
                        collab_types::DocumentMutation::Snapshot(bytes)
                        | collab_types::DocumentMutation::Incremental(bytes) => bytes,
                    };
                    self.changes_seen = self.changes_seen.wrapping_add(1);
                    self.arrived.insert(route.clone(), self.changes_seen);
                    self.durable.entry(route).or_default().push(bytes);
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

    fn step(&mut self) {
        // The host answers `Ready` after replaying the room's records, and a
        // sealed record opens once the agent holds its keys, so a step never
        // runs on a room the agent has only half seen.
        if !self.ready || !self.control.may_act() || self.keys_pending() {
            return;
        }
        if self.running.is_none() {
            let Some((id, text)) = self.control.begin_next() else {
                return;
            };
            let context = self.plan_context();
            match self.planner.plan(&text, &context) {
                Ok(steps) => {
                    self.running = Some(Running {
                        id,
                        steps: steps.into(),
                        performed: 0,
                    });
                }
                Err(error) => {
                    self.control.finish(&id, Err(error));
                    return;
                }
            }
        }
        let Some(running) = self.running.as_mut() else {
            return;
        };
        if !self.control.is_running(&running.id) {
            self.running = None;
            return;
        }
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
            return;
        };
        let id = running.id.clone();
        if let StepAction::Continue(rest) = &step.action {
            let context = self.plan_context();
            match self.planner.plan(rest, &context) {
                Ok(steps) => {
                    if let Some(running) = self.running.as_mut() {
                        for step in steps.into_iter().rev() {
                            running.steps.push_front(step);
                        }
                    }
                }
                Err(error) => {
                    self.running = None;
                    self.control.finish(&id, Err(error));
                }
            }
            return;
        }
        let outcome = self.perform(&step);
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
        self.control.record_step(&id, record);
        match outcome {
            Ok(_) => {
                if let Some(running) = self.running.as_mut() {
                    running.performed = running.performed.saturating_add(1);
                }
            }
            Err(error) => {
                self.running = None;
                self.control.finish(&id, Err(error));
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

    fn perform(&mut self, step: &Step) -> Result<Option<ActionOutput>, String> {
        let route = match &step.record {
            Some(record) => self.open(&step.app, Some(record))?,
            None => match self.apps.keys().find(|route| route.app_id == step.app) {
                Some(route) => route.clone(),
                None => self.open(&step.app, None)?,
            },
        };
        match &step.action {
            StepAction::Invoke { address, arguments } => self.invoke(&route, address, arguments),
            StepAction::Operate { control, input } => {
                self.operate(&route, control, input).map(|()| None)
            }
            StepAction::Open | StepAction::Continue(_) => Ok(None),
        }
    }

    /// Commit whatever the open apps changed since the last commit: an
    /// action's change can land after the invocation returns (a component
    /// applies it on its next wake).
    fn publish_changes(&mut self) {
        for (route, app) in &mut self.apps {
            if let Some(mutation) = app.take_mutation() {
                self.generation = self.generation.wrapping_add(1);
                if let Err(error) = self.peer.publish(route.clone(), &mutation) {
                    tracing::warn!(%error, "agent: a change could not be committed");
                }
            }
        }
    }

    fn plan_context(&mut self) -> PlanContext {
        let open = self
            .apps
            .iter_mut()
            .map(|(route, app)| (route.clone(), app.survey()))
            .collect();
        let controls = self
            .apps
            .iter_mut()
            .map(|(route, app)| (route.clone(), app.controls()))
            .collect();
        PlanContext {
            open,
            controls,
            known: people_first(
                self.durable.keys().cloned().collect(),
                &desktop_records(&self.durable, &self.room),
                &self.arrived,
            ),
            apps: self
                .catalogue
                .iter()
                .map(|definition| definition.app_id.to_owned())
                .filter(|app| self.identity.delegation.permits_app(app))
                .collect(),
        }
    }

    /// Records of the room the agent has seen but not opened.
    #[must_use]
    pub fn known_records(&self) -> Vec<DocumentRoute> {
        self.durable.keys().cloned().collect()
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
        let seed = match record {
            Some(record) => {
                let route = route(record);
                if self.apps.contains_key(&route) {
                    return Ok(route);
                }
                let changes = self
                    .durable
                    .remove(&route)
                    .ok_or_else(|| format!("the room has no record {record} of {app_id} yet"))?;
                Seed::Existing {
                    record: ui::record::RecordId::parse(record)
                        .ok_or_else(|| format!("{record} is not a record id"))?,
                    changes,
                }
            }
            None => Seed::New,
        };
        let mut app = HostedApp::open(
            &self.catalogue,
            app_id,
            &self.room,
            &self.identity.delegation.agent_id,
            true,
            seed,
        )
        .map_err(|error| error.to_string())?;
        let route = route(app.record());
        if let Some(mutation) = app.take_mutation() {
            self.peer.publish(route.clone(), &mutation)?;
        }
        self.apps.insert(route.clone(), app);
        self.generation = self.generation.wrapping_add(1);
        self.pointer = None;
        self.focus = Some(LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        self.publish_presence();
        Ok(route)
    }

    /// Every action the record at `route` publishes now.
    pub fn survey(&mut self, route: &DocumentRoute) -> Vec<ActionDescriptor> {
        self.apps
            .get_mut(route)
            .map(HostedApp::survey)
            .unwrap_or_default()
    }

    /// Every on-screen control the record at `route` shows now.
    pub fn controls(&mut self, route: &DocumentRoute) -> Vec<ui::headless::Control> {
        self.apps
            .get_mut(route)
            .map(HostedApp::controls)
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
        let may_confirm = self.identity.delegation.scope.may_confirm;
        self.invoke_as(route, address, arguments, may_confirm)
            .map_err(|refusal| match refusal {
                actions::Refusal::Door(error) => format!("{}: {error}", error.code()),
                actions::Refusal::Agent(reason) => reason,
            })
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
        // The cursor goes where a person would point before the action
        // runs: it may remove the control it names.
        let pointer = match &descriptor {
            Some(descriptor) => Some(app.pointer_for_action(descriptor)),
            None => app.pointer_for(address),
        };
        let started = app
            .invoke(address, arguments, confirmed)
            .map_err(Refusal::Door)?;
        self.pointer = pointer;
        if let Some(mutation) = app.take_mutation() {
            self.generation = self.generation.wrapping_add(1);
            self.peer
                .publish(route.clone(), &mutation)
                .map_err(Refusal::Agent)?;
        }
        self.focus = Some(LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        self.control.set_activity(label);
        self.publish_presence();
        Ok(match started {
            ResourceOperationStart::Completed(output) => Some(output),
            ResourceOperationStart::Accepted => None,
        })
    }

    /// Operate one on-screen control of the record at `route` as the agent,
    /// the way a person's input does, commit the change it made, and show
    /// the agent's pointer on it.
    ///
    /// # Errors
    ///
    /// Returns the refusal: paused or stopped, outside the delegation, no
    /// such control, or the action it triggered refused.
    pub fn operate(
        &mut self,
        route: &DocumentRoute,
        control: &str,
        input: &ControlInput,
    ) -> Result<(), String> {
        if !self.control.may_act() {
            return Err("the agent is paused or stopped".into());
        }
        if !self.identity.delegation.permits_app(&route.app_id) {
            return Err(format!("the agent is not delegated {}", route.app_id));
        }
        let app = self
            .apps
            .get_mut(route)
            .ok_or_else(|| "that record is not open".to_string())?;
        let target = app.controls().into_iter().find(|c| c.id == control);
        let label = target.map(|target| {
            format!(
                "{} · {}",
                crate::planner::input_name(input),
                target.name.unwrap_or_else(|| format!("{:?}", target.role))
            )
        });
        self.pointer = app.pointer_for_control(control);
        app.operate(control, input)?;
        if let Some(mutation) = app.take_mutation() {
            self.peer.publish(route.clone(), &mutation)?;
        }
        self.focus = Some(LiveRecord {
            app: route.app_id.clone(),
            record: route.record.clone(),
        });
        self.control.set_activity(label);
        self.publish_presence();
        Ok(())
    }

    /// The mounted app of `route`.
    #[must_use]
    pub fn app(&self, route: &DocumentRoute) -> Option<&HostedApp> {
        self.apps.get(route)
    }

    /// Every other live participant as this agent sees them: browsers,
    /// native devices and other agents, with their presence.
    pub fn remote_members(&mut self) -> Vec<RemoteMember> {
        self.live.remote(now_ms())
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
        let now = now_ms();
        let participant = self.participant();
        let (focus, pointer) = (self.focus.clone(), self.pointer);
        match self.live.publish_presence(now, |presence| {
            presence.focus = focus;
            presence.pointer = pointer;
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
        self.pointer = None;
        self.publish_presence();
        for (_, app) in std::mem::take(&mut self.apps) {
            app.close();
        }
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
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]

    use super::*;
    use automerge::transaction::Transactable as _;

    fn route(app: &str, record: &str) -> DocumentRoute {
        DocumentRoute {
            app_id: app.into(),
            cluster_id: "room".into(),
            record: record.into(),
        }
    }

    #[test]
    fn records_people_have_open_come_first_then_the_latest() {
        let known = vec![
            route("sheets", "a"),
            route("sheets", "b"),
            route("sheets", "c"),
        ];
        let on_desktop = std::iter::once("c".to_owned()).collect();
        let arrived = [(route("sheets", "a"), 1), (route("sheets", "b"), 2)]
            .into_iter()
            .collect();
        let ordered: Vec<String> = people_first(known, &on_desktop, &arrived)
            .into_iter()
            .map(|route| route.record)
            .collect();
        assert_eq!(ordered, ["c", "b", "a"]);
    }

    #[test]
    fn the_shared_desktop_names_its_tabs_records() {
        let mut doc = automerge::AutoCommit::new();
        doc.put(
            automerge::ROOT,
            "desktop",
            r#"{"tree":{},"records":[[7,"rec-open"]]}"#,
        )
        .unwrap();
        let mut durable = BTreeMap::new();
        durable.insert(route(DESKTOP_APP, DESKTOP_RECORD), vec![doc.save()]);
        let records = desktop_records(&durable, "room");
        assert!(records.contains("rec-open"));
        assert_eq!(records.len(), 1);
    }
}
