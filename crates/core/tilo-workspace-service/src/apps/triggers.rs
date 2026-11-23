//! What starts a saved shortcut without anyone invoking it, run by the device's one daemon on
//! every device of the person, a laptop, a phone and a server alike.
//!
//! The runner arms the shortcuts with triggers that the person this device acts for saved in a
//! workspace it keeps, by the proof of a device of theirs ([`armed`]). It turns what it watches
//! into events of the one dispatcher every automation fires through
//! ([`tilo_automation::dispatch`]): a schedule's fire time, a change to a record, an app's event.
//! A shared folder changing and a workspace joined it watches here too. Each firing is a turn of
//! work ([`Turn::Once`]), and [`takes_turn`] decides which of the person's devices starts it:
//! the one that saved the shortcut when it is up, otherwise an always-on server, otherwise any
//! other. Nothing about a firing is kept in a device's own files: a schedule's progress is kept in
//! the workspace ([`tilo_peer_state::trigger_mark`]), and a firing that started is kept done
//! there ([`keep_done`]), so a device that takes over carries on from what was written.
//!
//! A source that changed outside the workspace and a time a resource holds are two more kinds of
//! [`Event`] it hands the dispatcher, placed the same way. So are the events a connector on this
//! device reports ([`ConnectorEvents`]): each is a firing of every armed shortcut whose connector
//! trigger names its app, source and matching fields.

pub mod folders;
#[cfg(feature = "agents")]
mod mail;
#[cfg(feature = "agents")]
mod outside;
#[cfg(feature = "agents")]
mod times;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use automerge::{AutoCommit, ChangeHash};
use serde_json::{Map, Value};
use tilo_actions::ActionContext;
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_automation::dispatch::retry;
use tilo_automation::dispatch::{
    self, AppEvent, ConnectorEvent, DocumentEvent, Event, Firer, ScheduleEvent, event_of,
};
use tilo_automation::schedule::{SchedulePlan, Timed, plan};
#[cfg(feature = "agents")]
use tilo_automation::source_change;
use tilo_automation::{AutomationInvocationId, InvocationReport, Shortcut, TriggerSpec};

use crate::work::{Actor, Claim, NotTaken, Order, Turn, keep_done, takes_turn_in};
use crate::workspaces::Workspaces;
use folders::{FolderChange, FolderWatch};

/// How often the runner looks again with nothing written meanwhile: a schedule's fire time, a
/// firing waiting for its turn and a folder's settled file are seen within it.
const TICK: Duration = Duration::from_secs(1);

/// How long a firing's claim goes between renewals while its run goes or waits to go again.
const RENEW: Duration = Duration::from_secs(20);

/// The input a shortcut declares to be handed which firing started it: the same on every
/// device and every attempt, so a step can name it.
pub const FIRING_INPUT: &str = "firing";

/// Where a firing's run starts: the device's action broker, which checks again that the
/// shortcut still exists and that its saver may still run it.
pub trait StartRun: Send + Sync {
    /// Start `firing`'s run as its saver, as the invocation `run`: the broker job it reports on,
    /// or why it did not start.
    ///
    /// # Errors
    /// Why the run did not start: the shortcut is gone, lost the trigger, or reaches past what
    /// its saver may do now.
    fn start(&self, firing: &Firing, run: AutomationInvocationId) -> Result<u64, ActionCallError>;

    /// How the run `run` of `firing` ended, once it has.
    fn ended(&self, firing: &Firing, run: AutomationInvocationId) -> Option<InvocationReport>;
}

impl StartRun for crate::apps::collab_broker::AppActionBroker {
    fn start(&self, firing: &Firing, run: AutomationInvocationId) -> Result<u64, ActionCallError> {
        let caller = firing.caller();
        self.start_triggered_shortcut_run(
            &caller,
            &firing.workspace,
            &firing.shortcut,
            &firing.trigger,
            run,
            firing.inputs.clone(),
        )
    }

    fn ended(&self, firing: &Firing, run: AutomationInvocationId) -> Option<InvocationReport> {
        self.shortcut_run_report(run, &firing.shortcut)
    }
}

/// A shortcut armed in a workspace this device keeps: one its person saved, or, on a device that
/// runs every member's shortcuts, one any member saved.
#[derive(Clone, Debug)]
pub struct Armed {
    pub workspace: String,
    pub shortcut: Shortcut,
    /// Who saved it, by their device's proof: its triggers fire on their behalf only, bounded by
    /// its grant.
    pub saver: String,
    /// The agent of theirs that saved it with its own key, and that key: its runs act as that
    /// agent, held to the agent's grants as well as its own.
    pub agent: Option<(String, [u8; 32])>,
    /// The device that saved it: the one its firings start on.
    pub starter: [u8; 32],
    /// The workspace as its governance describes it.
    pub described: tilo_desktop_model::Workspace,
}

/// Every shortcut with triggers in `workspaces` saved by the proof of a member's device
/// ([`tilo_peer_state::workspace::proven_saver`]) that is valid at `now_ns` against the
/// revocations the workspace records: those the person this device acts for in its workspace
/// (`person`) saved, or every member's when this device runs them all (`hosts`).
#[must_use]
pub fn armed(
    workspaces: &[tilo_desktop_model::Workspace],
    revocations: impl Fn(&str) -> Option<tilo_user_identity::RevocationList>,
    person: impl Fn(&str) -> String,
    hosts: bool,
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
            let Some(proven) =
                tilo_peer_state::workspace::proven(&id, &shortcut, &revocations, now_ns)
                    .filter(|saver| hosts || saver.person == person)
            else {
                continue;
            };
            let Some(starter) = saving_device(&shortcut) else {
                continue;
            };
            armed.push(Armed {
                workspace: id.clone(),
                shortcut,
                agent: proven.agent.map(|agent| (agent, proven.key)),
                saver: proven.person,
                starter,
                described: workspace.clone(),
            });
        }
    }
    armed
}

/// What a device kept in the workspace of `trigger` of `armed`, marked `mark`, at its last
/// look, read back as the progress its runner holds; `None` where none was kept, or none that
/// reads.
#[cfg(feature = "agents")]
async fn kept_progress<T: serde::de::DeserializeOwned>(
    workspaces: &Workspaces,
    armed: &Armed,
    trigger: usize,
    mark: &str,
) -> Option<T> {
    let kept = workspaces
        .trigger_progress(&armed.workspace, &armed.shortcut.id, trigger, mark)
        .await
        .inspect_err(|error| {
            tracing::warn!(shortcut = %armed.shortcut.id, %error, "a trigger's kept progress could not be read; it starts over");
        })
        .ok()??;
    tilo_failure::ignored!(
        "progress another build kept that this one cannot read starts over",
        serde_json::from_value(kept).ok()
    )
}

/// Keep `progress` in the workspace as what this device made of `trigger` of `armed`, marked
/// `mark`: whichever device looks next, this one after a restart too, carries on from it.
#[cfg(feature = "agents")]
async fn keep_progress(
    workspaces: &Workspaces,
    armed: &Armed,
    trigger: usize,
    mark: &str,
    progress: &impl serde::Serialize,
) {
    let kept = match serde_json::to_value(progress) {
        Ok(progress) => {
            workspaces
                .keep_trigger_progress(
                    &armed.workspace,
                    &armed.shortcut.id,
                    trigger,
                    mark,
                    progress,
                )
                .await
        }
        Err(error) => Err(error.to_string()),
    };
    if let Err(error) = kept {
        tracing::warn!(shortcut = %armed.shortcut.id, %error, "a trigger's progress was not kept");
    }
}

/// A person's root, from its hex.
fn person_key(person: &str) -> Option<[u8; 32]> {
    hex::decode(person).ok()?.try_into().ok()
}

/// The device whose proof says it saved `shortcut`.
fn saving_device(shortcut: &Shortcut) -> Option<[u8; 32]> {
    let proof = shortcut.saved_proof.as_ref()?;
    let device: tilo_user_identity::DeviceCert = serde_json::from_value(proof.device_cert.clone())
        .or_else(|_| {
            serde_json::from_value(proof.device_cert.get("grant").cloned().unwrap_or_default())
        })
        .ok()?;
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
    /// The agent of the saver's it acts as, by its id and its grant's key, where one saved it.
    pub agent: Option<(String, [u8; 32])>,
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
    /// Who its run is started as: its saver, or the agent of theirs that saved it.
    #[must_use]
    pub fn caller(&self) -> Caller {
        let caller = match (&self.agent, person_key(&self.saver)) {
            (Some((agent, key)), Some(person)) => {
                Caller::delegated_agent(agent.clone(), person, *key)
            }
            _ => Caller::local_person(self.saver.clone()),
        };
        caller.in_workspace(&self.workspace)
    }

    /// The turn of work this firing is, started on `starter` at `came_up_ms`.
    #[must_use]
    pub fn turn(&self, starter: [u8; 32], came_up_ms: i64) -> Turn {
        Turn::once(
            crate::apps::automation::run_address(&self.shortcut),
            serde_json::json!({ "invocation": self.invocation.0.to_string() }),
        )
        .acting_as(Actor::Person(self.saver.clone()))
        .started(starter, came_up_ms)
    }
}

/// A firing waiting for its turn here.
#[derive(Clone, Debug)]
struct Waiting {
    firing: Firing,
    turn: Turn,
}

/// A firing this device took: its claim is kept until its run ends for good.
struct Going {
    firing: Firing,
    turn: Turn,
    claim: Claim,
    /// The attempt running or next, 1 for the first.
    attempt: u32,
    /// The run going, or `None` while the next attempt waits for `again_at_ms`.
    run: Option<AutomationInvocationId>,
    again_at_ms: i64,
    renewed_ms: i64,
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
    // A paused shortcut takes in what happens and answers none of it.
    for armed in armed.iter().filter(|armed| !armed.shortcut.paused) {
        let fired = dispatch::dispatch(
            &[dispatch::Armed {
                firer: Firer {
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
                agent: armed.agent.clone(),
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
        agent: armed.agent.clone(),
        answers: None,
    }
}

/// The changes of `doc` after `since`, in causal order, as events, each with the item it
/// changed as it left it; on a record's first look (`since` is `None`) only the changes made
/// from `from_s`, seconds since the epoch, on.
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
            let mut event = event_of(
                workspace,
                record,
                kind,
                &change.hash().to_string(),
                &context,
            );
            // What a trigger's filter reads of the changed item: its fields as the change left
            // them.
            event.after = event.item.as_ref().and_then(|item| {
                tilo_automerge::items::item_json(committed, &[change.hash()], &item.id)
            });
            Some(event)
        })
        .collect()
}

/// One resource a change of the resource store touched ([`store_events`]): its event, the
/// change, and the write it is part of, the same in every document the write changed.
pub struct StoreEvent {
    pub event: DocumentEvent,
    pub change: ChangeHash,
    pub write: (String, u64),
}

/// The events of the changes to the resources the store document `record` of `app` keeps,
/// committed since `since`: one for each resource a stamped change touched, named by its id and
/// kind, by whoever invoked the verb, and with `incoming_message` set for a resource an outside
/// origin made or wrote a body into. The store keeps no time in its changes, so a document's
/// first look (no `since`) reports only the changes at its heads: the write that woke the runner,
/// never what the document held before. What the resource holds after the change is the
/// runner's to read from the store ([`Workspaces::store_wrote`]).
#[must_use]
pub fn store_events(
    (workspace, app, record): (&str, &str, &str),
    doc: &AutoCommit,
    since: Option<&[ChangeHash]>,
) -> Vec<StoreEvent> {
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
            // A store keeps no facets beside its changes. A resource an outside origin wrote a
            // body into or made is what the workspace received from outside, so a trigger on a
            // kind of messages reads the change as an incoming message; an origin's write of a
            // flag on a message it already held is not one.
            let incoming_message =
                stamp.verb == collab_types::Stamp::ORIGIN_VERB && (touch.made || touch.content);
            events.push(StoreEvent {
                change: change.hash(),
                write: (stamp.invocation.clone(), stamp.moment),
                event: DocumentEvent {
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
                    origin: None,
                    caused_by: None,
                    incoming_message,
                    housekeeping: false,
                    shortcut: None,
                    invocation: None,
                },
            });
        }
    }
    events
}

/// `event`, of one resource a store write touched, with what the write left it holding
/// ([`Workspaces::store_wrote`]): its fields and content, under `$in` the resource it is nested
/// in (its kind as `$kind`, its id as `$id`, and its fields), and the item it names.
fn with_what_it_holds(event: &mut DocumentEvent, wrote: tilo_resource_store::Wrote) {
    let tilo_resource_store::Wrote {
        kind,
        id,
        mut holds,
        parent,
    } = wrote;
    if let Some((parent_kind, parent_id, mut fields)) = parent {
        fields.insert("$kind".to_owned(), Value::String(parent_kind));
        fields.insert("$id".to_owned(), Value::String(parent_id));
        holds.insert("$in".to_owned(), Value::Object(fields));
    }
    let label = holds
        .get("title")
        .or_else(|| holds.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    event.item = Some(tilo_automation::dispatch::ItemRef { kind, id, label });
    event.after = Some(Value::Object(holds));
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
    /// Firings this device took whose run has not ended for good, by their turn's lease.
    going: BTreeMap<String, Going>,
    /// What this device's connectors report, when it runs any.
    connectors: Option<Arc<dyn ConnectorEvents>>,
    /// How far each connector app's event log was read, by app id.
    reported: BTreeMap<String, u64>,
    /// What each trigger at a time a resource holds has answered, by workspace, shortcut and
    /// trigger (`times`).
    #[cfg(feature = "agents")]
    times: BTreeMap<(String, String, usize), times::Progress>,
    /// What the looks at each source outside took in, by workspace, shortcut and trigger
    /// (`outside`).
    #[cfg(feature = "agents")]
    looks: BTreeMap<(String, String, usize), source_change::Seen>,
    /// The client the looks at sources outside go through.
    #[cfg(feature = "agents")]
    outside: outside::Outside,
    /// What is heard of the mail arriving in the accounts of the shortcuts' savers (`mail`).
    #[cfg(feature = "agents")]
    heard: mail::Heard,
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
            going: BTreeMap::new(),
            connectors: None,
            reported: BTreeMap::new(),
            #[cfg(feature = "agents")]
            times: BTreeMap::new(),
            #[cfg(feature = "agents")]
            looks: BTreeMap::new(),
            #[cfg(feature = "agents")]
            outside: outside::Outside::default(),
            #[cfg(feature = "agents")]
            heard: mail::Heard::default(),
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
        use tokio::sync::broadcast::error::{RecvError, TryRecvError};

        let mut writes = self.workspaces.document_writes();
        let mut governance = self.workspaces.governance_changes();
        // Armed at the start, and again whenever a workspace's own document changed: a shortcut
        // is saved there, so it is armed by following its save, not on a clock.
        let mut changed = true;
        let mut written = Vec::new();
        loop {
            // What was written is taken first and the workspaces' changes second. A save is told
            // before anything written after it, so a shortcut saved before a write taken here is
            // armed before that write is read: a message written right after its shortcut is
            // answered.
            loop {
                match writes.try_recv() {
                    Ok(write) => written.push(write),
                    Err(TryRecvError::Lagged(_)) => {}
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Closed) => return,
                }
            }
            loop {
                match governance.try_recv() {
                    Ok(_) | Err(TryRecvError::Lagged(_)) => changed = true,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Closed) => return,
                }
            }
            if std::mem::take(&mut changed) {
                self.arm().await;
            }
            self.pass(std::mem::take(&mut written)).await;
            tokio::select! {
                write = writes.recv() => match write {
                    Ok(write) => written.push(write),
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                },
                moved = governance.recv() => match moved {
                    Ok(_) | Err(RecvError::Lagged(_)) => changed = true,
                    Err(RecvError::Closed) => return,
                },
                () = tokio::time::sleep(TICK) => {}
            }
        }
    }

    /// Arm the shortcuts this device's person saved in the workspaces it keeps now, and every
    /// member's on a device that runs them all ([`crate::work::hosts_shortcuts`]).
    pub async fn arm(&mut self) {
        let person = hex::encode(self.workspaces.serving_certificate().user_public_key);
        let hosts = crate::work::hosts_shortcuts(&self.workspaces).await;
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
            hosts,
            tilo_clock::unix_now_ns(),
        );
        // A record's first look reports the changes made from when its workspace started being
        // watched. A change's time is a whole second, and what was written right after the save
        // that armed it may carry the second before this one.
        let now_s = tilo_clock::unix_now_i64().saturating_sub(1);
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
        #[cfg(feature = "agents")]
        self.moments(now_ms).await;
        #[cfg(feature = "agents")]
        self.outside_changes(now_ms).await;
        #[cfg(feature = "agents")]
        self.mail(now_ms).await;
        self.files(now_ms);
        self.connector_events(now_ms);
        self.take_turns().await;
        self.tend(now_ms).await;
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
        let mut seen = BTreeSet::new();
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
                for StoreEvent {
                    mut event,
                    change,
                    write,
                } in store_events((workspace, app, record), &doc, since.as_deref())
                {
                    // A write that changed a resource and its content is one change of it.
                    if !seen.insert((write, event.record.clone())) {
                        continue;
                    }
                    let wrote = self.workspaces.store_wrote(workspace, record, change).await;
                    if let Some(wrote) = wrote.into_iter().find(|wrote| wrote.id == event.record) {
                        with_what_it_holds(&mut event, wrote);
                    }
                    events.push(event);
                }
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
                        // A schedule seen first keeps when it was seen, so a fire time before
                        // then is no missed one; a missed time it skips moves its progress past
                        // that time, so it waits for the next.
                        if answered.is_none_or(|answered| answered_ms > answered) {
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
                    // A paused shortcut's fire time passes unanswered: it is not run later.
                    SchedulePlan::Fire { answered_ms, .. } if armed.shortcut.paused => {
                        self.answer(
                            &armed.workspace,
                            &armed.shortcut.id,
                            index,
                            &mark,
                            answered_ms,
                        )
                        .await;
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
                .filter(|armed| armed.workspace == change.workspace && !armed.shortcut.paused)
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
            for armed in self.armed.iter().filter(|armed| !armed.shortcut.paused) {
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
        let now_ms = tilo_clock::unix_now_ms_i64();
        for (lease, waiting) in pending {
            if self.going.contains_key(&lease) {
                continue;
            }
            let Waiting { firing, turn } = waiting;
            match takes_turn_in(&self.workspaces, &firing.workspace, &turn, self.order).await {
                Ok(claim) => {
                    if self.answered_elsewhere(&firing, &turn).await {
                        claim.release().await;
                        continue;
                    }
                    let mut going = Going {
                        firing,
                        turn,
                        claim,
                        attempt: 1,
                        run: None,
                        again_at_ms: now_ms,
                        renewed_ms: now_ms,
                    };
                    if let Some(ended) = self.begin(&mut going).await {
                        self.finish(going, ended).await;
                    } else {
                        self.going.insert(lease, going);
                    }
                }
                // Another device may yet start it, or the one that claimed it may go quiet: it
                // waits here meanwhile, however long no device can start it.
                Err(NotTaken::Waiting { .. } | NotTaken::TakenBy(_) | NotTaken::Unwritten(_)) => {
                    self.pending.insert(lease, Waiting { firing, turn });
                }
                Err(NotTaken::Done(_)) => {}
                Err(NotTaken::Ineligible(why)) => {
                    tracing::info!(shortcut = %firing.shortcut, %why, "a shortcut trigger fired for a device that may not start it");
                }
            }
        }
    }

    /// Whether another device answered `firing`'s fire time while this one waited its turn.
    async fn answered_elsewhere(&self, firing: &Firing, turn: &Turn) -> bool {
        let (
            Some(answer),
            Turn::Once {
                came_up_ms: Some(came_up_ms),
                ..
            },
        ) = (&firing.answers, turn)
        else {
            return false;
        };
        self.workspaces
            .schedule_answered(
                &firing.workspace,
                &firing.shortcut,
                answer.trigger,
                &answer.mark,
            )
            .await
            .ok()
            .flatten()
            .is_some_and(|answered| answered >= *came_up_ms)
    }

    /// Start `going`'s next attempt: `None` while it runs or waits to go again, or how the
    /// firing ended when it did not start and will not.
    async fn begin(&self, going: &mut Going) -> Option<Ended> {
        // The first attempt is the firing's own invocation, the same on every device; each one
        // after it is a run of its own.
        let run = if going.attempt == 1 {
            going.firing.invocation
        } else {
            AutomationInvocationId::new()
        };
        let mut firing = going.firing.clone();
        firing.inputs = completed(&self.armed, &firing, &going.turn.lease());
        let start = Arc::clone(&self.start);
        let started = tokio::task::spawn_blocking(move || start.start(&firing, run))
            .await
            .unwrap_or_else(|error| {
                Err(ActionCallError::Unavailable {
                    reason: error.to_string(),
                })
            });
        match started {
            Ok(_) => {
                going.run = Some(run);
                None
            }
            Err(error) => {
                // A shortcut deleted or changed elsewhere, or one whose saver may not run it
                // here, did not start, and will not for this firing; one that could not start
                // for now goes again later.
                tracing::info!(shortcut = %going.firing.shortcut, trigger = going.firing.trigger.kind(), %error, "a shortcut trigger fired but the shortcut did not start");
                let transient = matches!(error, ActionCallError::Unavailable { .. });
                self.after(
                    going,
                    retry::after_failure(
                        transient,
                        error.to_string(),
                        None,
                        going.attempt,
                        tilo_clock::unix_now_ms_i64(),
                    ),
                    None,
                )
            }
        }
    }

    /// Whether `next` ends `going`: how, or `None` when it waits to go again.
    fn after(
        &self,
        going: &mut Going,
        next: retry::Next,
        report: Option<&InvocationReport>,
    ) -> Option<Ended> {
        match next {
            retry::Next::Done { ended, step, why } => Some(Ended {
                run: going.run,
                at_ms: tilo_clock::unix_now_ms_i64(),
                ended,
                step,
                why,
                counted: report.is_some_and(|report| {
                    self.armed
                        .iter()
                        .find(|armed| {
                            armed.workspace == going.firing.workspace
                                && armed.shortcut.id == going.firing.shortcut
                        })
                        .is_some_and(|armed| counted(&armed.shortcut, report))
                }),
            }),
            retry::Next::Again { at_ms } => {
                going.run = None;
                going.again_at_ms = at_ms;
                going.attempt = going.attempt.saturating_add(1);
                None
            }
        }
    }

    /// Look after every firing this device took: keep its claim, read how its run ended, start
    /// its next attempt when its wait is over, and keep it done once it ended for good.
    async fn tend(&mut self, now_ms: i64) {
        let going = std::mem::take(&mut self.going);
        for (lease, mut going) in going {
            if now_ms.saturating_sub(going.renewed_ms)
                >= i64::try_from(RENEW.as_millis()).unwrap_or(i64::MAX)
            {
                if !going.claim.renew().await {
                    // Two devices claimed it at once and the other one has it now.
                    tracing::info!(shortcut = %going.firing.shortcut, "another device took a firing this one was running");
                    continue;
                }
                going.renewed_ms = now_ms;
            }
            let ended = match going.run {
                Some(run) => match self.start.ended(&going.firing, run) {
                    Some(report) => {
                        let next = retry::after_run(&report, going.attempt, now_ms);
                        self.after(&mut going, next, Some(&report))
                    }
                    None => None,
                },
                None if now_ms >= going.again_at_ms => self.begin(&mut going).await,
                None => None,
            };
            match ended {
                Some(ended) => self.finish(going, ended).await,
                None => {
                    self.going.insert(lease, going);
                }
            }
        }
    }

    /// Keep `going` done as it `ended`, for every device, and let go of it. A run that failed for
    /// good keeps its failed invocation record, which this row names.
    async fn finish(&self, going: Going, ended: Ended) {
        let Going {
            firing,
            turn,
            claim,
            ..
        } = going;
        if let Some(answer) = &firing.answers {
            self.answer(
                &firing.workspace,
                &firing.shortcut,
                answer.trigger,
                &answer.mark,
                answer.answered_ms,
            )
            .await;
        }
        let kept = serde_json::to_value(&ended).unwrap_or_default();
        if let Err(error) = keep_done(&self.workspaces, &firing.workspace, &turn, kept).await {
            tracing::warn!(shortcut = %firing.shortcut, %error, "an ended firing was not kept done");
        }
        claim.release().await;
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

    /// The firings this device took whose run has not ended for good.
    #[must_use]
    pub fn going(&self) -> Vec<&Firing> {
        self.going.values().map(|going| &going.firing).collect()
    }
}

/// How a firing ended for good, as its done row keeps it: every device reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ended {
    /// The last run it started, whose invocation record says what it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<AutomationInvocationId>,
    /// When it ended, ms since the epoch.
    #[serde(default)]
    pub at_ms: i64,
    pub ended: retry::Ended,
    /// The step it stopped at, when it did not run to its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Whether its run took the shortcut's count step ([`crate::apps::automation::COUNT_ACTION`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub counted: bool,
}

/// `firing`'s inputs, with each the shortcut declares and the trigger left out given its
/// default, and the [`FIRING_INPUT`] given the firing's `lease`.
fn completed(armed: &[Armed], firing: &Firing, lease: &str) -> Map<String, Value> {
    let mut inputs = firing.inputs.clone();
    let Some(armed) = armed
        .iter()
        .find(|armed| armed.workspace == firing.workspace && armed.shortcut.id == firing.shortcut)
    else {
        return inputs;
    };
    for input in &armed.shortcut.inputs {
        let name: &str = input.name.as_ref();
        if inputs.contains_key(name) {
            continue;
        }
        if name == FIRING_INPUT && input.ty == tilo_actions::params::ParamType::Text {
            inputs.insert(name.to_owned(), Value::from(lease));
        } else if let Some(default) = input
            .schema
            .as_deref()
            .and_then(|schema| schema.get("default"))
        {
            inputs.insert(name.to_owned(), default.clone());
        }
    }
    inputs
}

/// Whether `report`'s run took `shortcut`'s count step.
fn counted(shortcut: &Shortcut, report: &InvocationReport) -> bool {
    report.steps.iter().any(|entry| {
        matches!(entry.status, tilo_automation::StepStatus::Succeeded { .. })
            && crate::apps::automation::counts(shortcut, &entry.step)
    })
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

/// Run the triggers of the shortcuts this device arms, starting each firing that is this
/// device's turn through `broker`, until the workspaces' service stops.
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
