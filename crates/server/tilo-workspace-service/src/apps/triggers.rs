//! What starts a saved shortcut without anyone invoking it, run by the device's one daemon on
//! every device of the person, a laptop, a phone and a server alike.
//!
//! The runner arms the shortcuts with triggers that the person this device acts for saved in a
//! workspace it keeps, by the proof of a device of theirs ([`armed`]). It turns what it watches
//! into events of the one dispatcher every automation fires through
//! ([`tilo_automation::dispatch`]): a schedule's fire time, a change to a record, an app's event.
//! A shared folder changing and a workspace joined it watches here too. Each firing is a turn of
//! work ([`Turn::Firing`]), and [`takes_turn`] decides which of the person's devices starts it:
//! the one that saved the shortcut when it is up, otherwise an always-on server, otherwise any
//! other. Nothing about a firing is kept in a device's own files: a schedule's progress is kept in
//! the workspace ([`tilo_peer_state::trigger_mark`]), and a firing that started is kept done
//! there ([`keep_done`]), so a device that takes over carries on from what was written.
//!
//! A watch the runner has more of later (a source that changed, a time a resource names) is one
//! more kind of [`Event`] it hands the dispatcher, placed the same way. So are the events a
//! connector on this device reports ([`ConnectorEvents`]): each is a firing of every armed
//! shortcut whose connector trigger names its app, source and matching fields.

pub mod folders;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use automerge::{AutoCommit, ChangeHash};
use serde_json::{Map, Value};
use tilo_actions::ActionContext;
use tilo_actions::remote::Caller;
use tilo_automation::dispatch::{
    self, AppEvent, ConnectorEvent, DocumentEvent, Event, Firer, ScheduleEvent, event_of,
};
use tilo_automation::schedule::{SchedulePlan, Timed, plan};
use tilo_automation::{AutomationInvocationId, Shortcut, TriggerSpec};

use crate::work::{NotTaken, Order, Turn, keep_done, takes_turn_in};
use crate::workspaces::Workspaces;
use folders::{FolderChange, FolderWatch};

/// How often the runner looks again with nothing written meanwhile: a schedule's fire time, a
/// firing waiting for its turn and a folder's settled file are seen within it.
const TICK: Duration = Duration::from_secs(1);

/// How often the shortcuts are armed again although no workspace's governance said it changed.
const REARM: Duration = Duration::from_secs(30);

/// Where a firing's run starts: the device's action broker, which checks again that the
/// shortcut still exists and that its saver may still run it.
pub trait StartRun: Send + Sync {
    /// Start `firing`'s run as its saver: the broker job it reports on, or why it did not start.
    ///
    /// # Errors
    /// Why the run did not start: the shortcut is gone, lost the trigger, or reaches past what
    /// its saver may do now.
    fn start(&self, firing: &Firing) -> Result<u64, String>;
}

impl StartRun for crate::apps::collab_broker::AppActionBroker {
    fn start(&self, firing: &Firing) -> Result<u64, String> {
        let caller = Caller::local_person(firing.saver.clone()).in_workspace(&firing.workspace);
        self.start_triggered_shortcut_run(
            &caller,
            &firing.workspace,
            &firing.shortcut,
            &firing.trigger,
            firing.invocation,
            firing.inputs.clone(),
        )
        .map_err(|error| error.to_string())
    }
}

/// A shortcut this device's person saved, armed in a workspace this device keeps.
#[derive(Clone, Debug)]
pub struct Armed {
    pub workspace: String,
    pub shortcut: Shortcut,
    /// Who saved it, by their device's proof: its triggers fire on their behalf only.
    pub saver: String,
    /// The device that saved it: the one its firings start on.
    pub starter: [u8; 32],
    /// The workspace as its governance describes it.
    pub described: tilo_desktop_model::Workspace,
}

/// Every shortcut with triggers in `workspaces` that the person this device acts for in its
/// workspace (`person`) saved, by the proof of a device of theirs ([`tilo_peer_state::workspace::proven_saver`]) that is valid at `now_ns` against the
/// revocations the workspace records.
#[must_use]
pub fn armed(
    workspaces: &[tilo_desktop_model::Workspace],
    revocations: impl Fn(&str) -> Option<tilo_user_identity::RevocationList>,
    person: impl Fn(&str) -> String,
    now_ns: u64,
) -> Vec<Armed> {
    let mut armed = Vec::new();
    for workspace in workspaces {
        let id = workspace.id().to_string();
        let mut triggered = workspace
            .shortcuts
            .iter()
            .filter_map(|saved| {
                saved
                    .decode()
                    .inspect_err(|error| {
                        tracing::warn!(%error, "a saved shortcut does not read back; it arms no trigger");
                    })
                    .ok()
            })
            .filter(|shortcut| !shortcut.triggers.is_empty())
            .peekable();
        if triggered.peek().is_none() {
            continue;
        }
        let Some(revocations) = revocations(&id) else {
            continue;
        };
        let person = person(&id);
        for shortcut in triggered {
            if tilo_peer_state::workspace::proven_saver(&id, &shortcut, &revocations, now_ns)
                .as_ref()
                != Some(&person)
            {
                continue;
            }
            let Some(starter) = saving_device(&shortcut) else {
                continue;
            };
            armed.push(Armed {
                workspace: id.clone(),
                shortcut,
                saver: person.clone(),
                starter,
                described: workspace.clone(),
            });
        }
    }
    armed
}

/// The device whose proof says it saved `shortcut`.
fn saving_device(shortcut: &Shortcut) -> Option<[u8; 32]> {
    let proof = shortcut.saved_proof.as_ref()?;
    let device: tilo_user_identity::DeviceCert =
        serde_json::from_value(proof.device_cert.clone()).ok()?;
    Some(device.device_public_key)
}

/// One trigger that fired: run `shortcut` in `workspace` as `invocation`, for `saver`.
#[derive(Clone, Debug, PartialEq)]
pub struct Firing {
    pub workspace: String,
    pub shortcut: String,
    pub trigger: TriggerSpec,
    pub invocation: AutomationInvocationId,
    /// The trigger's payload, as values for the shortcut's inputs.
    pub inputs: Map<String, Value>,
    pub saver: String,
    /// For a schedule's fire time: the progress kept once it ran.
    pub answers: Option<Answer>,
}

/// How far a schedule was answered once its fire time ran.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub trigger: usize,
    pub mark: String,
    pub answered_ms: i64,
}

impl Firing {
    /// The turn of work this firing is, started on `starter` at `came_up_ms`.
    #[must_use]
    pub fn turn(&self, starter: [u8; 32], came_up_ms: i64) -> Turn {
        Turn::Firing {
            firing: format!("{}/{}", self.shortcut, self.invocation.0),
            saver: self.saver.clone(),
            starter,
            came_up_ms,
        }
    }
}

/// A firing waiting for its turn here.
#[derive(Clone, Debug)]
struct Waiting {
    firing: Firing,
    turn: Turn,
}

/// One event a connector on this device reported: what fired it and what it carried.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportedEvent {
    /// Its place in the connector's log, so the next look asks for what follows it.
    pub seq: u64,
    /// The source the connector declared it under (`message-created`).
    pub source: String,
    /// The service's id for it, which tells it from a replay of itself.
    pub id: String,
    /// When the service said it happened, ms since the epoch.
    pub at_ms: i64,
    /// What it carried, which a trigger's filter narrows and its run reads.
    pub payload: Map<String, Value>,
}

/// What the connectors this device runs reported, as the trigger runner reads them.
pub trait ConnectorEvents: Send + Sync {
    /// The events the connector `app` reported after `cursor`, oldest first.
    fn since(&self, app: &str, cursor: u64) -> Vec<ReportedEvent>;
}

/// The firings `event` causes among `armed`, through the dispatcher every automation fires
/// through.
fn dispatched(armed: &[Armed], event: Event<'_>, came_up_ms: i64) -> Vec<Waiting> {
    let mut firings = Vec::new();
    for armed in armed {
        let fired = dispatch::dispatch(
            &[dispatch::Armed {
                firer: Firer::Shortcut {
                    shortcut: &armed.shortcut.id,
                    saver: &armed.saver,
                },
                shortcut: &armed.shortcut,
            }],
            event,
        );
        firings.extend(fired.firings.into_iter().map(|fired| {
            let firing = Firing {
                workspace: armed.workspace.clone(),
                shortcut: armed.shortcut.id.clone(),
                trigger: fired.trigger,
                invocation: fired.invocation,
                inputs: fired.inputs,
                saver: armed.saver.clone(),
                answers: None,
            };
            Waiting {
                turn: firing.turn(armed.starter, came_up_ms),
                firing,
            }
        }));
    }
    firings
}

/// `trigger` of `armed` firing where it was seen, with `payload` as the shortcut's inputs where
/// it declares inputs of the same name and type.
#[must_use]
pub fn seen_here(armed: &Armed, trigger: &TriggerSpec, payload: &[(&str, Value)]) -> Firing {
    use tilo_actions::params::ParamType;
    let inputs = payload
        .iter()
        .filter(|(name, value)| {
            armed.shortcut.inputs.iter().any(|input| {
                input.name == *name
                    && match input.ty {
                        ParamType::Boolean => value.is_boolean(),
                        ParamType::Text => value.is_string(),
                        ParamType::Object => value.is_object(),
                        _ => false,
                    }
            })
        })
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    Firing {
        workspace: armed.workspace.clone(),
        shortcut: armed.shortcut.id.clone(),
        trigger: trigger.clone(),
        invocation: AutomationInvocationId::new(),
        inputs,
        saver: armed.saver.clone(),
        answers: None,
    }
}

/// The changes of `doc` after `since`, in causal order, as events; on a record's first look
/// (`since` is `None`) only the changes made from `from_s`, seconds since the epoch, on.
#[must_use]
pub fn record_events(
    (workspace, record, kind): (&str, &str, &str),
    doc: &AutoCommit,
    since: Option<&[ChangeHash]>,
    from_s: i64,
) -> Vec<DocumentEvent> {
    let committed = doc.committed_document();
    if since.is_some_and(|since| committed.get_heads() == since) {
        return Vec::new();
    }
    committed
        .get_changes(since.unwrap_or_default())
        .into_iter()
        .filter(|change| since.is_some() || change.timestamp() >= from_s)
        .filter_map(|change| {
            let context = ActionContext::from_change_message(change.message()?)?;
            Some(event_of(
                workspace,
                record,
                kind,
                &change.hash().to_string(),
                &context,
            ))
        })
        .collect()
}

/// The events of the changes to the resources the store document `record` of `app` keeps,
/// committed since `since`: one for each resource a stamped change touched, named by its id and
/// kind, by whoever invoked the verb. The store keeps no time in its changes, so a document's
/// first look (no `since`) reports only the changes at its heads: the write that woke the runner,
/// never what the document held before.
#[must_use]
pub fn store_events(
    (workspace, app, record): (&str, &str, &str),
    doc: &AutoCommit,
    since: Option<&[ChangeHash]>,
) -> Vec<DocumentEvent> {
    let committed = doc.committed_document();
    let heads = committed.get_heads();
    if since.is_some_and(|since| heads == since) {
        return Vec::new();
    }
    let mut events = Vec::new();
    for change in committed.get_changes(since.unwrap_or_default()) {
        if since.is_none() && !heads.contains(&change.hash()) {
            continue;
        }
        let (Some(stamp), Some(touched)) = (
            collab_types::Stamp::of(&change),
            tilo_resource_store::touches(committed, record, &change),
        ) else {
            continue;
        };
        let mut actor = tilo_actions::Acting::person(String::new());
        if let Some(by) = stamp.invoked_by() {
            actor.by = by;
        }
        for (id, touch) in touched.resources {
            let Some(kind) = touch.kind else {
                continue;
            };
            events.push(DocumentEvent {
                workspace: workspace.to_owned(),
                record: id,
                did: tilo_actions::Did::verb(
                    tilo_actions::ActionKey::new(app, &kind, &stamp.verb),
                    [],
                ),
                kind,
                change: change.hash().to_string(),
                actor: actor.clone(),
                arguments: Map::new(),
                item: None,
                before: None,
                after: None,
                caused_by: None,
                incoming_message: false,
                housekeeping: false,
                shortcut: None,
                invocation: None,
            });
        }
    }
    events
}

/// The daemon's trigger runner over the workspaces this device keeps.
pub struct Runner {
    workspaces: Workspaces,
    start: Arc<dyn StartRun>,
    order: Order,
    zone: jiff::tz::TimeZone,
    armed: Vec<Armed>,
    /// When each workspace whose records are watched started being watched, seconds since the
    /// epoch: a record's first look reports the changes made from then on.
    watching: BTreeMap<String, i64>,
    /// The heads each record was last read at, by workspace, app and record.
    cursors: BTreeMap<(String, String, String), Vec<ChangeHash>>,
    folders: FolderWatch,
    joined: Option<BTreeSet<String>>,
    /// Firings waiting for their turn, by their turn's lease.
    pending: BTreeMap<String, Waiting>,
    /// What this device's connectors report, when it runs any.
    connectors: Option<Arc<dyn ConnectorEvents>>,
    /// How far each connector app's event log was read, by app id.
    reported: BTreeMap<String, u64>,
}

impl Runner {
    /// A runner over `workspaces`, starting runs through `start`, keeping `order`.
    #[must_use]
    pub fn new(workspaces: Workspaces, start: Arc<dyn StartRun>, order: Order) -> Self {
        Self {
            workspaces,
            start,
            order,
            zone: jiff::tz::TimeZone::system(),
            armed: Vec::new(),
            watching: BTreeMap::new(),
            cursors: BTreeMap::new(),
            folders: FolderWatch::default(),
            joined: None,
            pending: BTreeMap::new(),
            connectors: None,
            reported: BTreeMap::new(),
        }
    }

    /// Fire the triggers on what the connectors this device runs report, read
    /// through `connectors`.
    #[must_use]
    pub fn watching_connectors(mut self, connectors: Arc<dyn ConnectorEvents>) -> Self {
        self.connectors = Some(connectors);
        self
    }

    /// Read schedules' fire times in `zone`.
    #[must_use]
    pub fn in_zone(mut self, zone: jiff::tz::TimeZone) -> Self {
        self.zone = zone;
        self
    }

    /// Run until the workspaces' service stops.
    pub async fn run(mut self) {
        let mut writes = self.workspaces.document_writes();
        let mut governance = self.workspaces.governance_changes();
        let mut armed_at = None::<std::time::Instant>;
        let mut written = Vec::new();
        loop {
            let now = tilo_clock::now();
            if armed_at.is_none_or(|at| now.saturating_duration_since(at) >= REARM) {
                self.arm().await;
                armed_at = Some(now);
            }
            self.pass(std::mem::take(&mut written)).await;
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => {
                        written.push(write);
                        while let Ok(more) = writes.try_recv() {
                            written.push(more);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                changed = governance.recv() => match changed {
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        armed_at = None;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                () = tokio::time::sleep(TICK) => {}
            }
        }
    }

    /// Arm the shortcuts this device's person saved in the workspaces it keeps now.
    pub async fn arm(&mut self) {
        let person = hex::encode(self.workspaces.serving_certificate().user_public_key);
        let described = self.workspaces.described().await;
        let mut revocations = BTreeMap::new();
        for workspace in &described {
            let id = workspace.id().to_string();
            if let Some(state) = self.workspaces.state(&id).await
                && let Some(doc) = state.admitted_workspace()
            {
                revocations.insert(id, tilo_peer_state::workspace::workspace_revocations(doc));
            }
        }
        self.armed = armed(
            &described,
            |workspace| revocations.get(workspace).cloned(),
            |_| person.clone(),
            tilo_clock::unix_now_ns(),
        );
        let now_s = tilo_clock::unix_now_i64();
        let watched: BTreeSet<String> = self
            .armed
            .iter()
            .filter(|armed| armed.shortcut.triggers.iter().any(watches_records))
            .map(|armed| armed.workspace.clone())
            .collect();
        self.watching
            .retain(|workspace, _| watched.contains(workspace));
        for workspace in watched {
            self.watching.entry(workspace).or_insert(now_s);
        }
        let watching = &self.watching;
        self.cursors
            .retain(|(workspace, ..), _| watching.contains_key(workspace));
        self.folders.watch(folders::watched_folders(&self.armed));
        let names: BTreeMap<String, String> = described
            .iter()
            .map(|workspace| (workspace.id().to_string(), workspace.name.clone()))
            .collect();
        self.workspaces_joined(&names);
    }

    /// Look at everything once: `written` the records written since the last look.
    pub async fn pass(&mut self, written: Vec<crate::workspaces::ResourceWrite>) {
        let now_ms = tilo_clock::unix_now_ms_i64();
        for event in self.read(written).await {
            let mut firings = dispatched(&self.armed, Event::Record(&event), now_ms);
            if let Some(declared) = AppEvent::of_change(&event) {
                firings.extend(dispatched(&self.armed, Event::AppEvent(&declared), now_ms));
            }
            self.offer(firings);
        }
        self.schedules(now_ms).await;
        self.files(now_ms);
        self.connector_events(now_ms);
        self.take_turns().await;
    }

    /// Fire the triggers on every event the connectors this device runs reported
    /// after the last look, each read once: a firing kept done in the workspace
    /// stops a replay of the same event from starting it again.
    fn connector_events(&mut self, now_ms: i64) {
        let Some(connectors) = self.connectors.clone() else {
            return;
        };
        let watched: BTreeSet<String> = self
            .armed
            .iter()
            .flat_map(|armed| armed.shortcut.triggers.iter())
            .filter_map(|trigger| match trigger {
                TriggerSpec::Connector { app_id, .. } => Some(app_id.clone()),
                _ => None,
            })
            .collect();
        self.reported.retain(|app, _| watched.contains(app));
        let mut firings = Vec::new();
        for app in watched {
            let cursor = self.reported.get(&app).copied().unwrap_or(0);
            let mut last = cursor;
            for reported in connectors.since(&app, cursor) {
                last = last.max(reported.seq);
                let event = ConnectorEvent {
                    app_id: app.clone(),
                    source: reported.source,
                    id: reported.id,
                    at_ms: reported.at_ms,
                    payload: reported.payload,
                };
                firings.extend(dispatched(&self.armed, Event::Connector(&event), now_ms));
            }
            self.reported.insert(app, last);
        }
        self.offer(firings);
    }

    /// The events of the records `written`, in the workspaces whose records an armed shortcut
    /// watches.
    async fn read(&mut self, written: Vec<crate::workspaces::ResourceWrite>) -> Vec<DocumentEvent> {
        let mut events = Vec::new();
        let written: BTreeSet<(String, String, String)> = written
            .into_iter()
            .filter(|write| !write.record.is_empty())
            .map(|write| (write.workspace, write.app, write.record))
            .collect();
        for key in written {
            let (workspace, app, record) = &key;
            let Some(&from_s) = self.watching.get(workspace) else {
                continue;
            };
            let Ok(Some(bytes)) = self
                .workspaces
                .resource_document(workspace, app, record)
                .await
            else {
                continue;
            };
            let Ok(doc) = AutoCommit::load(&bytes) else {
                tracing::warn!(%workspace, %app, %record, "a written record does not load; it fires nothing");
                continue;
            };
            let heads = doc.committed_heads();
            let since = self.cursors.insert(key.clone(), heads);
            if collab_types::is_store_document(record) {
                events.extend(store_events(
                    (workspace, app, record),
                    &doc,
                    since.as_deref(),
                ));
                continue;
            }
            let kind = collab_types::document::identity::identity(&doc)
                .map(|identity| identity.kind.as_str().to_owned())
                .unwrap_or_default();
            events.extend(record_events(
                (workspace, record, &kind),
                &doc,
                since.as_deref(),
                from_s,
            ));
        }
        events
    }

    /// Fire every schedule whose time came, as its progress kept in the workspace says.
    async fn schedules(&mut self, now_ms: i64) {
        // What waits of a schedule is planned again from its progress each pass.
        self.pending
            .retain(|_, waiting| waiting.firing.answers.is_none());
        let mut fired = Vec::new();
        for armed in &self.armed {
            for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let Some(timed) = Timed::of(trigger) else {
                    continue;
                };
                let mark = tilo_peer_state::trigger_mark(trigger);
                let Ok(answered) = self
                    .workspaces
                    .schedule_answered(&armed.workspace, &armed.shortcut.id, index, &mark)
                    .await
                else {
                    continue;
                };
                match plan(timed, answered, now_ms, &self.zone) {
                    SchedulePlan::Waiting { answered_ms } => {
                        // A schedule seen first keeps when it was seen: a fire time before then
                        // is no missed one.
                        if answered.is_none() {
                            self.answer(
                                &armed.workspace,
                                &armed.shortcut.id,
                                index,
                                &mark,
                                answered_ms,
                            )
                            .await;
                        }
                    }
                    SchedulePlan::Fire {
                        due_ms,
                        answered_ms,
                        ..
                    } => {
                        let event = ScheduleEvent {
                            workspace: armed.workspace.clone(),
                            trigger: index,
                            due_ms,
                        };
                        fired.extend(
                            dispatched(
                                std::slice::from_ref(armed),
                                Event::Schedule(&event),
                                due_ms,
                            )
                            .into_iter()
                            .filter(|waiting| waiting.firing.trigger == *trigger)
                            .map(|mut waiting| {
                                waiting.firing.answers = Some(Answer {
                                    trigger: index,
                                    mark: mark.clone(),
                                    answered_ms,
                                });
                                waiting
                            }),
                        );
                    }
                }
            }
        }
        self.offer(fired);
    }

    /// Fire the file-changed triggers on what settled in the folders they watch.
    fn files(&mut self, now_ms: i64) {
        let device = self.workspaces.serving_certificate().device_public_key;
        let changes: Vec<FolderChange> = self.folders.changes();
        let mut firings = Vec::new();
        for change in &changes {
            for armed in self
                .armed
                .iter()
                .filter(|armed| armed.workspace == change.workspace)
            {
                for trigger in &armed.shortcut.triggers {
                    if matches!(trigger, TriggerSpec::FileChanged { capability } if *capability == change.capability)
                    {
                        let firing = seen_here(
                            armed,
                            trigger,
                            &[
                                ("file", Value::from(change.file.as_str())),
                                ("change", Value::from(change.change.label())),
                            ],
                        );
                        firings.push(Waiting {
                            turn: firing.turn(device, now_ms),
                            firing,
                        });
                    }
                }
            }
        }
        self.offer(firings);
    }

    /// Fire the workspace-joined triggers on the workspaces this device keeps now and did not at
    /// the last look. The first look only learns them.
    fn workspaces_joined(&mut self, names: &BTreeMap<String, String>) {
        let now: BTreeSet<String> = names.keys().cloned().collect();
        let Some(before) = self.joined.replace(now) else {
            return;
        };
        let device = self.workspaces.serving_certificate().device_public_key;
        let now_ms = tilo_clock::unix_now_ms_i64();
        let mut firings = Vec::new();
        for (workspace, name) in names.iter().filter(|(id, _)| !before.contains(*id)) {
            for armed in &self.armed {
                for trigger in &armed.shortcut.triggers {
                    if matches!(trigger, TriggerSpec::WorkspaceJoined { workspace: wanted }
                        if wanted.as_ref().is_none_or(|wanted| wanted == workspace))
                    {
                        let firing = seen_here(
                            armed,
                            trigger,
                            &[
                                ("workspace", Value::from(workspace.as_str())),
                                ("workspace_name", Value::from(name.as_str())),
                            ],
                        );
                        firings.push(Waiting {
                            turn: firing.turn(device, now_ms),
                            firing,
                        });
                    }
                }
            }
        }
        self.offer(firings);
    }

    /// Wait for each of `firings` to be this device's turn.
    fn offer(&mut self, firings: Vec<Waiting>) {
        for waiting in firings {
            self.pending.entry(waiting.turn.lease()).or_insert(waiting);
        }
    }

    /// Take each waiting firing whose turn is this device's, and start it.
    async fn take_turns(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for (lease, waiting) in pending {
            let Waiting { firing, turn } = &waiting;
            match takes_turn_in(&self.workspaces, &firing.workspace, turn, self.order).await {
                Ok(claim) => {
                    self.started(firing, turn).await;
                    claim.release().await;
                }
                // Another device may yet start it, or the one that claimed it may go quiet: it
                // waits here meanwhile, however long no device can start it.
                Err(NotTaken::Waiting { .. } | NotTaken::TakenBy(_) | NotTaken::Unwritten(_)) => {
                    self.pending.insert(lease, waiting);
                }
                Err(NotTaken::Done(_)) => {}
                Err(NotTaken::Ineligible(why)) => {
                    tracing::info!(shortcut = %firing.shortcut, %why, "a shortcut trigger fired for a device that may not start it");
                }
            }
        }
    }

    /// Start `firing`, this device's turn, once: a schedule's progress or the firing kept done in
    /// the workspace says it started, for every device.
    async fn started(&self, firing: &Firing, turn: &Turn) {
        let Turn::Firing { came_up_ms, .. } = turn else {
            return;
        };
        if let Some(answer) = &firing.answers {
            // Another device may have answered the fire time while this one waited its turn.
            let answered = self
                .workspaces
                .schedule_answered(
                    &firing.workspace,
                    &firing.shortcut,
                    answer.trigger,
                    &answer.mark,
                )
                .await
                .ok()
                .flatten();
            if answered.is_some_and(|answered| answered >= *came_up_ms) {
                return;
            }
        }
        let start = Arc::clone(&self.start);
        let starting = firing.clone();
        let started = tokio::task::spawn_blocking(move || start.start(&starting))
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
        let kept = match &started {
            Ok(run) => serde_json::json!({ "run": run }),
            Err(error) => {
                // A shortcut deleted or changed elsewhere, or one whose saver may not run it
                // here: nothing started, and it is not tried again for this firing.
                tracing::info!(shortcut = %firing.shortcut, trigger = firing.trigger.kind(), %error, "a shortcut trigger fired but the shortcut did not start");
                serde_json::json!({ "error": error })
            }
        };
        match &firing.answers {
            Some(answer) => {
                self.answer(
                    &firing.workspace,
                    &firing.shortcut,
                    answer.trigger,
                    &answer.mark,
                    answer.answered_ms,
                )
                .await;
            }
            None => {
                if let Err(error) = keep_done(&self.workspaces, &firing.workspace, turn, kept).await
                {
                    tracing::warn!(shortcut = %firing.shortcut, %error, "a started firing was not kept done");
                }
            }
        }
    }

    /// Keep that the schedule at `trigger` of `shortcut` was answered through `answered_ms`.
    async fn answer(
        &self,
        workspace: &str,
        shortcut: &str,
        trigger: usize,
        mark: &str,
        answered_ms: i64,
    ) {
        if let Err(error) = self
            .workspaces
            .answer_schedule(workspace, shortcut, trigger, mark, answered_ms)
            .await
        {
            tracing::warn!(%shortcut, %error, "a shortcut schedule's progress was not kept");
        }
    }

    /// The firings waiting for their turn here.
    #[must_use]
    pub fn waiting(&self) -> Vec<&Firing> {
        self.pending
            .values()
            .map(|waiting| &waiting.firing)
            .collect()
    }
}

/// Whether `trigger` fires on changes to records.
const fn watches_records(trigger: &TriggerSpec) -> bool {
    matches!(
        trigger,
        TriggerSpec::ResourceChanged { .. }
            | TriggerSpec::IncomingMessage { .. }
            | TriggerSpec::AppEvent { .. }
    )
}

/// Run the triggers of the shortcuts this device's person saved, starting each firing that is
/// this device's turn through `broker`, until the workspaces' service stops.
pub async fn run_on_device(
    workspaces: Workspaces,
    broker: Arc<crate::apps::collab_broker::AppActionBroker>,
) {
    Runner::new(workspaces, broker, Order::WORKSPACE)
        .run()
        .await;
}

/// [`run_on_device`], firing the triggers on what the connectors this device runs report, read
/// through `connectors`: the device that runs a connector is the one its events start runs on.
pub async fn run_on_device_with_connectors(
    workspaces: Workspaces,
    starts: Arc<crate::apps::collab_broker::AppActionBroker>,
    connectors: Arc<dyn ConnectorEvents>,
) {
    Runner::new(workspaces, starts, Order::WORKSPACE)
        .watching_connectors(connectors)
        .run()
        .await;
}

#[cfg(test)]
#[path = "triggers_tests.rs"]
mod tests;
