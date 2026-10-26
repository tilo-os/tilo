//! An installed Shortcut bot, run on the host that governs its room.
//!
//! The runner follows the records of the apps the bot's program names and
//! the event logs of the connectors it listens to. Each change and each
//! connector event goes through the dispatcher; every firing is queued and
//! run unattended through the bot's door ([`BotDoor`]) over its own
//! replicas of the room's records, and what a run changed is published by
//! the governor signed by the bot's own device, so every member admits it
//! as the bot's. An outbox entry a step queues is handed to the connector's
//! worker.
//!
//! Cursors and the queue are kept in the installation's state file, so a
//! restarted host carries on after the last change and event it took in;
//! a firing that runs again reuses its run id, and the door answers the
//! steps already in history instead of repeating them.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::remote::{Caller, Pending};
use tilo_agent_participant::bot::door::{BotDoor, OutboxSpec, PerformError, Performer};
use tilo_agent_participant::bot::held::HeldRecords;
use tilo_agent_participant::{AppCatalogue, ReplicaActor};
use tilo_automation::dispatch::queue::{DeadLetter, Finished, FiringQueue};
use tilo_automation::dispatch::{Armed, ConnectorEvent, Event, Firer, Firing};
use tilo_automation::run::{AskRequest, ConfirmationRequest, RunHooks};
use tilo_automation::{RunMode, RunRequest, RunTrigger, Shortcut, TriggerSpec};
use tilo_browser_peer::DocumentRoute;
use tilo_connector::ConnectorProcess;
use tilo_user_identity::DeviceCert;

use crate::rooms::{MemberMark, Rooms};

// The field a connector's records keep their outbox under, which the bot's
// grant lets it settle.
use tilo_automation::grant::OUTBOX_FIELD;
use tilo_lock::MutexRecover as _;
/// How long the runner waits with nothing to do before it looks again.
const IDLE: Duration = Duration::from_secs(30);
/// The input a program declares to be told the room's members and roles.
const ROOM_INPUT: &str = "room";
/// The input a program declares to be told the workspace's own emoji: each
/// name with its picture's content address, the picture kept in the
/// content store its connectors read, so an op may attach it.
const EMOJI_INPUT: &str = "emoji";
/// How many dead letters an installation keeps, newest last.
pub const DEAD_LETTERS_KEPT: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledBot {
    /// The installation's id; the bot acts as `bot:<installation>`.
    pub installation: String,
    /// Where it was installed from: the directory entry it came from, or the
    /// program file on this host when the room runs a program it ships.
    pub entry: String,
    /// Its name: the directory listing's, or the program's own when it is a
    /// program file (a path names nobody).
    pub name: String,
    /// The program, canonical text.
    pub program: String,
    /// Values for the program's declared inputs.
    pub inputs: Map<String, Value>,
    /// When it was installed, ms since the epoch.
    pub installed_at_ms: i64,
}

impl InstalledBot {
    /// The program, parsed.
    ///
    /// # Errors
    /// The stored program does not parse.
    pub fn shortcut(&self) -> Result<Shortcut, String> {
        tilo_automation::program::parse_program(self.program.as_bytes())
            .map_err(|error| error.to_string())
    }

    /// The connectors (by app id) whose events the program listens to.
    #[must_use]
    pub fn connectors(shortcut: &Shortcut) -> BTreeSet<String> {
        shortcut
            .triggers
            .iter()
            .filter_map(|trigger| match trigger {
                TriggerSpec::Connector { app_id, .. } => Some(app_id.clone()),
                _ => None,
            })
            .collect()
    }
}

/// What the runner has taken in, kept across restarts.
#[derive(Default, Serialize, Deserialize)]
struct Progress {
    /// Heads read up to, hex, by `app/record`.
    records: BTreeMap<String, Vec<String>>,
    /// Event log sequence read up to, by connector app id.
    connectors: BTreeMap<String, u64>,
    queue: FiringQueue,
    /// Firings that will not run again on their own, newest last.
    #[serde(default)]
    dead_letters: VecDeque<DeadLetter>,
    /// The room's members as last seen, for a program that follows the
    /// room's governance.
    #[serde(default)]
    members: Option<BTreeMap<String, MemberMark>>,
}

impl Progress {
    fn bury(&mut self, installation: &str, dead: DeadLetter) {
        tracing::warn!(%installation, event = %dead.firing.event, step = ?dead.step, reason = %dead.reason, "bot firing dead-lettered");
        self.dead_letters.push_back(dead);
        while self.dead_letters.len() > DEAD_LETTERS_KEPT {
            self.dead_letters.pop_front();
        }
    }
}

/// A connector's worker as the door's performer. The door performs from
/// the run's own blocking thread, so waiting here never holds a runtime
/// worker; the op itself runs on the runtime.
struct ConnectorPerformer {
    process: Arc<ConnectorProcess>,
    handle: tokio::runtime::Handle,
}

impl Performer for ConnectorPerformer {
    fn perform(&self, queued: Value) -> Result<Value, PerformError> {
        let (answer, answered) = std::sync::mpsc::channel();
        let process = Arc::clone(&self.process);
        self.handle.spawn(async move {
            let _ = answer.send(process.perform(queued).await);
        });
        match answered.recv() {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(tilo_connector::PerformError::Refused(reason))) => {
                Err(PerformError::Refused(reason))
            }
            Ok(Err(tilo_connector::PerformError::Unavailable(reason))) => {
                Err(PerformError::Unavailable(reason))
            }
            Err(_) => Err(PerformError::Unavailable("the connector went away".into())),
        }
    }
}

/// Nobody answers for a bot: an unattended run never confirms or asks,
/// and the door answers every step at once.
struct Unattended;

impl RunHooks for Unattended {
    fn confirm(
        &self,
        _request: ConfirmationRequest,
    ) -> Pending<Option<tilo_actions::remote::Confirmation>> {
        Pending::ready(None)
    }

    fn ask(&self, _request: AskRequest) -> Pending<Option<Value>> {
        Pending::ready(None)
    }

    fn settle(&self, _invocation: tilo_actions::InvocationId) -> Pending<()> {
        Pending::ready(())
    }
}

/// How a firing went on this host.
enum Fired {
    Ran(Box<tilo_automation::RunReport>),
    /// It ran, and the room refused what it changed, for this reason.
    Refused(String),
}

pub struct BotRunner {
    rooms: Rooms,
    room: String,
    host: DeviceCert,
    /// The bot's own device, which signs what its runs change.
    bot_key: ed25519_dalek::SigningKey,
    catalogue: AppCatalogue,
    bot: InstalledBot,
    shortcut: Arc<Shortcut>,
    /// Apps whose records the bot reads and acts on.
    apps: BTreeSet<String>,
    held: Arc<Mutex<HeldRecords>>,
    door: Arc<BotDoor<HeldRecords>>,
    connectors: BTreeMap<String, Arc<ConnectorProcess>>,
    progress: Progress,
    state_file: PathBuf,
    /// The emoji pictures already kept for the connectors, by address.
    kept: Mutex<BTreeSet<String>>,
    /// How many more changes are published before the room is made to
    /// refuse one, for a test.
    #[cfg(test)]
    refuse_after: Mutex<Option<usize>>,
}

impl BotRunner {
    /// The runner of `bot` in `room`, installed by the room's root whose
    /// device `host` governs it, signing as the bot's device `bot_key`
    /// (see [`super::membership::HostRootCertifier::device_key`]), its
    /// connectors already started, its progress kept in `state_file`.
    pub fn new(
        rooms: Rooms,
        room: String,
        host: DeviceCert,
        bot_key: ed25519_dalek::SigningKey,
        bot: InstalledBot,
        connectors: BTreeMap<String, Arc<ConnectorProcess>>,
        state_file: PathBuf,
    ) -> Result<Self, String> {
        let shortcut = bot.shortcut()?;
        let catalogue = AppCatalogue::native();
        let mut apps: BTreeSet<String> = shortcut
            .triggers
            .iter()
            .filter_map(|trigger| match trigger {
                TriggerSpec::RecordChanged { kind, .. } => {
                    catalogue.app_for_kind(kind).map(str::to_owned)
                }
                _ => None,
            })
            .collect();
        apps.extend(connectors.keys().cloned());
        apps.extend(
            tilo_automation::requirements::requirements(&shortcut)
                .apps
                .into_iter()
                .filter(|app| catalogue.get(app).is_some()),
        );
        let installer = hex::encode(host.user_public_key);
        let standing = bot_standing(
            &room,
            &installer,
            &bot.installation,
            &shortcut,
            &connectors.keys().cloned().collect(),
        );
        let held = Arc::new(Mutex::new(HeldRecords::new(
            catalogue.clone(),
            room.clone(),
            ReplicaActor::Bot {
                installation: bot.installation.clone(),
                installer: installer.clone(),
                standing,
            },
        )));
        let handle = tokio::runtime::Handle::current();
        let outboxes = connectors
            .iter()
            .map(|(app_id, process)| OutboxSpec {
                app_id: app_id.clone(),
                field: OUTBOX_FIELD.to_owned(),
                connector: Arc::new(ConnectorPerformer {
                    process: Arc::clone(process),
                    handle: handle.clone(),
                }),
            })
            .collect();
        let door = Arc::new(BotDoor::new(
            Arc::clone(&held),
            bot.installation.clone(),
            installer,
            outboxes,
        ));
        let progress = read_progress(&state_file)?;
        Ok(Self {
            rooms,
            room,
            host,
            bot_key,
            catalogue,
            bot,
            shortcut: Arc::new(shortcut),
            apps,
            held,
            door,
            connectors,
            progress,
            state_file,
            kept: Mutex::new(BTreeSet::new()),
            #[cfg(test)]
            refuse_after: Mutex::new(None),
        })
    }

    /// Have the room refuse the change published after `published` more.
    #[cfg(test)]
    pub(crate) fn refuse_change_after(&self, published: usize) {
        *self.refuse_after.lock_recover() = Some(published);
    }

    #[cfg(test)]
    fn refuse_for_test(&self) -> bool {
        let mut left = self.refuse_after.lock_recover();
        match *left {
            Some(0) => {
                *left = None;
                true
            }
            Some(n) => {
                *left = Some(n.saturating_sub(1));
                false
            }
            None => false,
        }
    }

    /// Firings that will not run again on their own, newest last: what
    /// failed for good, at which step and why, kept across restarts.
    pub fn dead_letters(&self) -> impl Iterator<Item = &DeadLetter> {
        self.progress.dead_letters.iter()
    }

    /// Run until the room is no longer governed here. The first pass
    /// catches up on everything since the kept cursors, so a restarted
    /// host answers what happened while it was down.
    pub async fn run(mut self) {
        let mut writes = self.rooms.record_writes();
        let mut governance = self.rooms.governance_changes();
        // Any connector's new event wakes the runner.
        let events = Arc::new(tokio::sync::Notify::new());
        for process in self.connectors.values() {
            let mut seq = process.subscribe();
            let events = Arc::clone(&events);
            tokio::spawn(async move {
                while seq.changed().await.is_ok() {
                    events.notify_one();
                }
            });
        }
        loop {
            if let Err(error) = self.step().await {
                tracing::warn!(installation = %self.bot.installation, %error, "bot runner did not finish a pass");
            }
            let wake = self
                .progress
                .queue
                .wake_at_ms()
                .map_or(IDLE, |at| {
                    Duration::from_millis(
                        u64::try_from(at.saturating_sub(tilo_clock::unix_now_ms_i64()))
                            .unwrap_or(0),
                    )
                })
                .min(IDLE);
            tokio::select! {
                write = writes.recv() => {
                    if matches!(write, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        return;
                    }
                }
                changed = governance.recv() => {
                    if matches!(changed, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                        return;
                    }
                }
                () = events.notified() => {}
                () = tokio::time::sleep(wake) => {}
            }
        }
    }

    /// Take in what changed since the cursors, queue the firings, run
    /// those that are due, and keep the progress. Answers the runs' reports.
    ///
    /// A firing the host could not run is tried again later, and
    /// dead-lettered once its attempts are spent.
    ///
    /// # Errors
    /// A record could not be read or the state file written.
    pub async fn step(&mut self) -> Result<Vec<tilo_automation::RunReport>, String> {
        let now = tilo_clock::unix_now_ms_i64();
        for firing in self.take_in().await? {
            if let Some(dead) = self.progress.queue.push(firing, now) {
                self.progress.bury(&self.bot.installation, dead);
            }
        }
        self.keep()?;
        let mut reports = Vec::new();
        while let Some(queued) = self.progress.queue.next_due(tilo_clock::unix_now_ms_i64()) {
            let firing = queued.firing.clone();
            let finished = match self.fire(&firing).await {
                Ok(Fired::Refused(reason)) => {
                    // What the run changed before the refused change stands
                    // and the rest was dropped: run it again, from the
                    // room's records, so it finishes what it started.
                    self.progress
                        .queue
                        .refused(&reason, tilo_clock::unix_now_ms_i64())
                }
                Ok(Fired::Ran(report)) => {
                    let finished = self
                        .progress
                        .queue
                        .finished(&report, tilo_clock::unix_now_ms_i64());
                    tracing::info!(installation = %self.bot.installation, run = %firing.run.0, outcome = ?report.outcome, ?finished, "bot run finished");
                    reports.push(*report);
                    finished
                }
                Err(error) => {
                    tracing::warn!(installation = %self.bot.installation, run = %firing.run.0, %error, "bot run could not be run");
                    self.progress
                        .queue
                        .not_run(&error, tilo_clock::unix_now_ms_i64())
                }
            };
            if let Finished::DeadLettered(dead) = finished {
                self.progress.bury(&self.bot.installation, *dead);
            }
            self.keep()?;
        }
        Ok(reports)
    }

    /// The firings caused by every record change and connector event after
    /// the cursors, which move past them.
    async fn take_in(&mut self) -> Result<Vec<Firing>, String> {
        let armed = [Armed {
            firer: Firer::Bot {
                installation: &self.bot.installation,
            },
            shortcut: &self.shortcut,
        }];
        let mut firings = Vec::new();
        if self.follows_governance() {
            let (generation, members) = self
                .rooms
                .membership(&self.room)
                .await
                .map_err(|error| error.to_string())?;
            // Like a record, the room's membership first seen is where the
            // bot starts.
            if let Some(before) = self.progress.members.replace(members.clone()) {
                let owner = hex::encode(self.host.user_public_key);
                for event in governance_events(&self.room, &owner, generation, &before, &members) {
                    firings.extend(
                        tilo_automation::dispatch::dispatch(&armed, Event::Record(&event)).firings,
                    );
                }
            }
        }
        for (app, record) in self.routes().await? {
            let Some(saved) = self.read(&app, &record).await? else {
                continue;
            };
            let key = format!("{app}/{record}");
            let mut doc = AutoCommit::load(&saved).map_err(|error| error.to_string())?;
            let heads: Vec<String> = doc.get_heads().iter().map(ToString::to_string).collect();
            // A record first seen starts at its heads: a bot answers what
            // happens after it is installed, not the room's past. A new
            // part of a chain of an app the bot already follows (the next
            // part of a chat's messages) is what happened since: read whole.
            // So is a record every app of the room opens (the workspace's
            // emoji), which its first change creates.
            let part = ui::record::RecordId::parse(&record)
                .is_some_and(|record| record.chain_part().is_some())
                || collab_types::workspace_record(&app, &record).is_some();
            let since = match self.progress.records.get(&key).cloned() {
                Some(since) => since,
                None if part && self.follows_app(&app) => Vec::new(),
                None => {
                    self.progress.records.insert(key, heads);
                    continue;
                }
            };
            // A part is of its own resource (a chat's messages), as its
            // record names it.
            let kind = ui::record::identity(&doc)
                .map(|checkout| checkout.kind.as_str().to_owned())
                .or_else(|| {
                    self.catalogue
                        .get(&app)
                        .map(|definition| definition.record_kind.as_str().to_owned())
                })
                .unwrap_or_default();
            for (event, _) in tilo_agent_participant::bot::events_since(
                &self.room,
                &record,
                &kind,
                doc.document(),
                &since,
            ) {
                firings.extend(
                    tilo_automation::dispatch::dispatch(&armed, Event::Record(&event)).firings,
                );
            }
            self.progress.records.insert(key, heads);
        }
        for (app_id, process) in &self.connectors {
            let cursor = self.progress.connectors.get(app_id).copied().unwrap_or(0);
            let log = process.events();
            let log = log.lock().await;
            let mut last = cursor;
            for logged in log.after(cursor, 0) {
                last = last.max(logged.seq);
                let event = ConnectorEvent {
                    app_id: app_id.clone(),
                    source: logged.source.clone(),
                    id: logged.id.clone(),
                    at_ms: logged.at_ms,
                    payload: logged.payload.clone(),
                };
                firings.extend(
                    tilo_automation::dispatch::dispatch(&armed, Event::Connector(&event)).firings,
                );
            }
            drop(log);
            self.progress.connectors.insert(app_id.clone(), last);
        }
        Ok(firings)
    }

    /// Whether the runner already follows records of `app`.
    fn follows_app(&self, app: &str) -> bool {
        let prefix = format!("{app}/");
        self.progress
            .records
            .keys()
            .any(|key| key.starts_with(&prefix))
    }

    /// Whether the program listens to the room's own governance.
    fn follows_governance(&self) -> bool {
        self.shortcut.triggers.iter().any(|trigger| {
            matches!(trigger, TriggerSpec::RecordChanged { kind, .. } if kind == tilo_action_policy::roles::ROOM_KIND)
        })
    }

    /// The bot's own inputs, with each the host binds that its program
    /// declares: the room's members, the workspace's emoji.
    async fn host_inputs(&self) -> Result<Map<String, Value>, String> {
        let declares = |name: &str| self.shortcut.inputs.iter().any(|input| input.name == name);
        let mut inputs = self.bot.inputs.clone();
        if declares(ROOM_INPUT) {
            inputs.insert(ROOM_INPUT.to_owned(), self.room_input().await?);
        }
        if declares(EMOJI_INPUT) {
            inputs.insert(EMOJI_INPUT.to_owned(), self.emoji_input().await?);
        }
        Ok(inputs)
    }

    /// Run `firing` through the bot's door and publish what it changed.
    async fn fire(&self, firing: &Firing) -> Result<Fired, String> {
        // The replicas are brought up to the room's records first, so the
        // run reads what members did up to now: each app's own records,
        // then the parts of the chains they keep beside them. A record the
        // bot cannot hold is left out of the run (and said), not every run
        // with it.
        let mut routes = self.routes().await?;
        routes.sort_by_key(|(_, record)| {
            ui::record::RecordId::parse(record).is_some_and(|record| record.chain_part().is_some())
        });
        for (app, record) in routes {
            if let Some(saved) = self.read(&app, &record).await? {
                let route = DocumentRoute {
                    app_id: app.clone(),
                    cluster_id: self.room.clone(),
                    record,
                };
                let held = Arc::clone(&self.held);
                let holding = route.clone();
                let held =
                    tokio::task::spawn_blocking(move || held.lock_recover().hold(&holding, saved))
                        .await
                        .map_err(|error| error.to_string())?;
                if let Err(error) = held {
                    tracing::warn!(installation = %self.bot.installation, app = %route.app_id, record = %route.record, %error, "bot run leaves out a record it could not hold");
                }
            }
        }
        let mut inputs = self.host_inputs().await?;
        inputs.extend(firing.inputs.clone());
        let caller = Caller::bot(
            self.bot.installation.clone(),
            self.host.user_public_key,
            self.bot_key.verifying_key().to_bytes(),
        )
        .in_room(self.room.clone());
        let (door, shortcut, handle) = (
            Arc::clone(&self.door),
            Arc::clone(&self.shortcut),
            tokio::runtime::Handle::current(),
        );
        let (run, trigger, cause) = (firing.run, firing.trigger.clone(), firing.cause.clone());
        door.set_cause(cause.clone());
        // Replicas and connector performs block; the run is driven on its
        // own blocking thread so they never hold a runtime worker.
        let report = tokio::task::spawn_blocking(move || {
            handle.block_on(tilo_automation::run::run(
                &*door,
                &Unattended,
                RunRequest {
                    shortcut: &shortcut,
                    run,
                    caller,
                    mode: RunMode::Unattended,
                    trigger: RunTrigger::Fired(trigger),
                    inputs,
                    caused_by: cause,
                },
            ))
        })
        .await
        .map_err(|error| error.to_string())?;
        let changes = self.held.lock_recover().take_changes();
        let count = changes.len();
        let mut changes = changes.into_iter().enumerate();
        while let Some((index, (route, mutation))) = changes.next() {
            if let Err(refusal) = self.publish(&route, mutation).await {
                // The refused change, and every later one built on it, stay
                // out of the room; the bot's replicas they touched start
                // over from the room's records, or every later change of
                // theirs would carry the refused one and be refused too.
                let touched: BTreeSet<DocumentRoute> = std::iter::once(route.clone())
                    .chain(changes.map(|(_, (route, _))| route))
                    .collect();
                let mut held = self.held.lock_recover();
                for route in &touched {
                    held.forget(route);
                }
                drop(held);
                self.door.forget_steps();
                let reason = format!(
                    "the room refused the bot's change to {}/{}: {refusal}",
                    route.app_id, route.record
                );
                tracing::warn!(
                    installation = %self.bot.installation,
                    run = %firing.run.0,
                    app = %route.app_id,
                    record = %route.record,
                    change = index.saturating_add(1),
                    of = count,
                    %refusal,
                    "bot change refused; the changes before it stand, the rest are dropped and the run goes again"
                );
                return Ok(Fired::Refused(reason));
            }
        }
        Ok(Fired::Ran(Box::new(report)))
    }

    /// Who is in the room and in what role, as of now: `{"members":
    /// [{"person", "role"}]}`, removed people left out. A program reads it
    /// to judge an outside request by the requester's standing here.
    pub(crate) async fn room_input(&self) -> Result<Value, String> {
        let summary = self
            .rooms
            .summary(&self.room)
            .await
            .map_err(|error| error.to_string())?;
        // The room's root owns it whether or not governance lists the
        // person as a member.
        let root = hex::encode(self.host.user_public_key);
        let mut members = vec![serde_json::json!({ "person": root, "role": "owner" })];
        members.extend(
            summary
                .members
                .iter()
                .filter(|member| member.user_public_key != self.host.user_public_key)
                .filter(|member| !member.removed)
                .map(|member| {
                    let role = if member.administrator {
                        "owner"
                    } else {
                        match member.role {
                            tilo_peer_state::join::MemberRole::Viewer => "viewer",
                            tilo_peer_state::join::MemberRole::Commenter => "commenter",
                            tilo_peer_state::join::MemberRole::Editor => "editor",
                            tilo_peer_state::join::MemberRole::Moderator => "moderator",
                        }
                    };
                    serde_json::json!({
                        "person": hex::encode(member.user_public_key),
                        "role": role,
                    })
                }),
        );
        Ok(serde_json::json!({ "members": members }))
    }

    /// The workspace's own emoji as the room holds them now: `{"emoji":
    /// [{"name", "picture"}]}`, each picture its content address. The
    /// pictures live in the emoji record itself, which a bot does not hold
    /// (`HeldRecords`), so the host reads them and keeps each in the
    /// content store of every connector the bot runs, where an op that
    /// attaches it finds it.
    pub(crate) async fn emoji_input(&self) -> Result<Value, String> {
        use tilo_resource::Resource as _;
        use tilo_workspace_emoji::{KIND, OWNER, WorkspaceEmoji};
        let record = collab_types::workspace_record_id(KIND);
        let Some(saved) = self.read(OWNER, &record).await? else {
            return Ok(serde_json::json!({ "emoji": [] }));
        };
        let doc = AutoCommit::load(&saved).map_err(|error| error.to_string())?;
        let emoji = WorkspaceEmoji::read(&doc).map_err(|error| error.to_string())?;
        let mut listed = Vec::new();
        for (name, emoji) in emoji.emoji.iter() {
            let picture = blake3::hash(emoji.image.bytes()).to_hex().to_string();
            if !self.kept.lock_recover().contains(&picture) {
                for process in self.connectors.values() {
                    process.keep(emoji.image.bytes())?;
                }
                self.kept.lock_recover().insert(picture.clone());
            }
            listed.push(serde_json::json!({ "name": name, "picture": picture }));
        }
        Ok(serde_json::json!({ "emoji": listed }))
    }

    /// The room's readable records of the apps the bot works with.
    async fn routes(&self) -> Result<Vec<(String, String)>, String> {
        Ok(self
            .rooms
            .record_routes(&self.room)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|(app, _)| self.apps.contains(app))
            .collect())
    }

    async fn read(&self, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        self.rooms
            .record_document(&self.room, app, record)
            .await
            .map_err(|error| error.to_string())
    }

    /// Put a run's changes into the room's record, signed by the bot's
    /// device, so every member admits them as the bot's. Fails when the
    /// room did not take them (outside the bot's grants, or from a bot it
    /// does not let act).
    async fn publish(
        &self,
        route: &DocumentRoute,
        mutation: DocumentMutation,
    ) -> Result<(), String> {
        #[cfg(test)]
        if self.refuse_for_test() {
            return Err("refused for the test".into());
        }
        let bytes = match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes,
        };
        self.rooms
            .publish_as_bot(
                &self.room,
                &self.host,
                &self.bot_key,
                &self.catalogue,
                &route.app_id,
                &route.record,
                &bytes,
            )
            .await
            .map_err(|error| error.to_string())
    }

    fn keep(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self.progress).map_err(|error| error.to_string())?;
        tilo_files::native::atomic::write(&self.state_file, &bytes)
            .map_err(|error| error.to_string())
    }
}

fn read_progress(path: &Path) -> Result<Progress, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Progress::default()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// What installation `installation` may do: what its installer, the
/// room's root, may, bounded to the grant its program carries (or, for a
/// program that carries none, the one derived from its steps and
/// triggers), and to settling its connectors' outboxes.
fn bot_standing(
    room: &str,
    installer: &str,
    installation: &str,
    shortcut: &Shortcut,
    connectors: &BTreeSet<String>,
) -> tilo_action_policy::Standing {
    use tilo_action_policy::{
        Answer, Grant, KindSet, Level, RoomPolicy, Rule, Standing, Subject, TreePath,
    };
    let bot = Subject::bot(installation);
    let mut rules = if shortcut.grant.is_empty() {
        tilo_automation::grant::derived_rules(
            shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        )
    } else {
        shortcut.grant.clone()
    };
    // Settling an entry is the bot recording what its connector answered.
    // It takes the entry out of the outbox, which the room judges as
    // reaching outside, as it did: the connector reached it.
    rules.extend(connectors.iter().flat_map(|app| {
        let app = TreePath::root().any(Level::Room).app(app.clone());
        let settle = app
            .clone()
            .any(Level::Kind)
            .any(Level::Resource)
            .any(Level::Item)
            .rest()
            .action(tilo_automation::grant::SETTLE_ACTION);
        [
            Rule::new(app, KindSet::READ_UNDOABLE, Answer::Allow),
            Rule::new(settle, KindSet::ALL, Answer::Allow),
        ]
    }));
    let grants = rules
        .into_iter()
        .enumerate()
        .map(|(index, rule)| {
            Grant::new(
                format!("bot:{installation}:{index}"),
                bot.clone(),
                rule.node,
                rule.kinds,
                rule.answer,
                Subject::person(installer),
            )
        })
        .collect();
    Standing::person(RoomPolicy::new(room, installer), installer).bounded_by(bot, grants)
}

/// What changed in the room's membership from `before` to `after`, as
/// the room's own governance verbs on its `tilo.room` resource: `add-member`
/// (joined, or came back), `set-role`, `remove-member` (removed, or left),
/// `ban` and `unban`, each with `person` (hex root) and, where it has one,
/// `role`. A program follows them as any record's changes; the change id
/// names the room's governance generation, so each event fires once.
#[must_use]
pub fn governance_events(
    room: &str,
    owner: &str,
    generation: u64,
    before: &BTreeMap<String, MemberMark>,
    after: &BTreeMap<String, MemberMark>,
) -> Vec<tilo_automation::dispatch::RecordEvent> {
    use tilo_action_policy::roles::governance;
    let mut events = Vec::new();
    let mut verb = |verb: &str, person: &str, role: Option<&str>| {
        let mut args = vec![("person".to_owned(), Value::from(person))];
        if let Some(role) = role {
            args.push(("role".to_owned(), Value::from(role)));
        }
        let arguments: Map<String, Value> = args.iter().cloned().collect();
        let change = blake3::hash(
            serde_json::to_string(&(room, generation, verb, person, role))
                .unwrap_or_default()
                .as_bytes(),
        )
        .to_hex()
        .to_string();
        events.push(tilo_automation::dispatch::RecordEvent {
            room: room.to_owned(),
            record: String::new(),
            kind: tilo_action_policy::roles::ROOM_KIND.to_owned(),
            change,
            actor: tilo_actions::interaction::InteractionActor::new(
                owner,
                tilo_actions::ActorKind::Person,
            ),
            did: tilo_actions::entry::Did::Verb(tilo_actions::entry::VerbCall::new(
                tilo_actions::ActionKey::new(ROOM_APP, tilo_action_policy::roles::ROOM_KIND, verb),
                args.iter().filter_map(|(key, value)| {
                    tilo_actions::entry::Scalar::from_json(value).map(|value| (key.clone(), value))
                }),
            )),
            arguments,
            item: None,
            before: None,
            after: None,
            caused_by: None,
            incoming_message: false,
            housekeeping: false,
            shortcut: None,
            run: None,
        });
    };
    for (person, now) in after {
        let was = before.get(person);
        let present = |mark: Option<&MemberMark>| mark.is_some_and(|mark| !mark.removed);
        match (present(was), present(Some(now))) {
            (false, true) => verb(governance::ADD_MEMBER, person, Some(&now.role)),
            (true, false) => {
                let verb_name = if now.banned {
                    governance::BAN
                } else {
                    governance::REMOVE_MEMBER
                };
                verb(verb_name, person, None);
            }
            (true, true) if was.is_some_and(|was| was.role != now.role) => {
                verb(governance::SET_ROLE, person, Some(&now.role));
            }
            _ => {}
        }
        let was_banned = was.is_some_and(|was| was.banned);
        if was_banned && !now.banned {
            verb(governance::UNBAN, person, None);
        } else if !was_banned && now.banned && !(present(was) && now.removed) {
            // Banned without a removal here: they were not a member.
            verb(governance::BAN, person, None);
        }
    }
    events
}

/// The app id the room's governance verbs are named under.
const ROOM_APP: &str = "so.tilo.desktop";

/// The bot `entry` installed in the room with `inputs`: the installation
/// kept under `dir` when the room installed it before, else a new one.
/// `entry` is a program file on this host, or a directory entry fetched
/// and verified from the first enabled registry at the `version` input.
///
/// A kept installation from a program file runs the file as it is now: the
/// host's image ships the program, and a fix to it (a grant the bot was
/// missing) reaches a room installed by an older image. The installation,
/// its name, its inputs and when it was installed stay; [`super::membership::keep_member`]
/// then updates the room's row to the new program and its grants.
///
/// # Errors
/// The program cannot be read, fetched, verified or parsed, or the
/// installation cannot be kept.
pub async fn install(
    dir: &Path,
    entry: &str,
    mut inputs: Map<String, Value>,
) -> Result<InstalledBot, String> {
    let kept = dir.join("bot.json");
    let file = Path::new(entry).is_file().then(|| program_file(entry));
    if let Ok(bytes) = std::fs::read(&kept) {
        let mut bot: InstalledBot = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", kept.display()))?;
        if bot.entry == entry {
            let Some(file) = file else {
                return Ok(bot);
            };
            // The name stays: the bot's device certificate carries it, and
            // a renewal keeps the same certificate.
            let (_, program) = file?;
            if bot.program != program {
                bot.program = program;
                keep_installed(dir, &kept, &bot)?;
            }
            return Ok(bot);
        }
    }
    let version = inputs.remove("version");
    let (name, program) = if let Some(file) = file {
        file?
    } else {
        let version = version
            .as_ref()
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!("{entry}: a directory bot is installed at a version (version=…)")
            })?
            .to_owned();
        let entry_id = entry.to_owned();
        let listed = tokio::task::spawn_blocking(move || {
            let registries = app_cli::config::load_registries();
            let registry = registries
                .enabled()
                .next()
                .ok_or_else(|| "no registry is enabled on this host".to_owned())?;
            tilo_bot_directory::fetch_listed_bot(registry, &entry_id, &version)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())??;
        (
            listed.entry.name.resolve("en").unwrap_or(entry).to_owned(),
            listed.program_text,
        )
    };
    let bot = InstalledBot {
        installation: uuid::Uuid::new_v4().to_string(),
        entry: entry.to_owned(),
        name,
        program,
        inputs,
        installed_at_ms: tilo_clock::unix_now_ms_i64(),
    };
    keep_installed(dir, &kept, &bot)?;
    Ok(bot)
}

/// The name and canonical program text of the program file `entry`.
fn program_file(entry: &str) -> Result<(String, String), String> {
    let bytes = std::fs::read(entry).map_err(|error| format!("{entry}: {error}"))?;
    let shortcut = tilo_automation::program::parse_program(&bytes)
        .map_err(|error| format!("{entry}: {error}"))?;
    let canonical =
        tilo_automation::program::canonical_bytes(&shortcut).map_err(|error| error.to_string())?;
    let text = String::from_utf8(canonical).map_err(|error| error.to_string())?;
    Ok((shortcut.name, text))
}

fn keep_installed(dir: &Path, kept: &Path, bot: &InstalledBot) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(bot).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write(kept, &bytes).map_err(|error| error.to_string())
}

/// The worker binary of connector `app_id`: the one the operator named,
/// else `<last segment of the id>-worker` beside this host's binary.
#[must_use]
pub fn worker_path(app_id: &str, named: &BTreeMap<String, PathBuf>) -> PathBuf {
    if let Some(path) = named.get(app_id) {
        return path.clone();
    }
    let name = format!("{}-worker", app_id.rsplit('.').next().unwrap_or(app_id));
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Start the workers of connectors `apps`, each with its own state under
/// `state`, its metrics in `state/metrics.d/<connector>.prom` and the
/// settings `settings` holds for it as its only environment.
///
/// # Errors
/// A worker did not start.
pub async fn start_connectors(
    apps: &BTreeSet<String>,
    state: &Path,
    named: &BTreeMap<String, PathBuf>,
    settings: &BTreeMap<String, Vec<(String, String)>>,
    blobs: &Arc<dyn tilo_connector::BlobAccess>,
) -> Result<BTreeMap<String, Arc<ConnectorProcess>>, String> {
    let mut started = BTreeMap::new();
    for app_id in apps {
        let short = app_id.rsplit('.').next().unwrap_or(app_id);
        let command = tilo_connector::WorkerCommand {
            program: worker_path(app_id, named),
            args: Vec::new(),
            env: settings.get(app_id).cloned().unwrap_or_default(),
            metrics_file: Some(
                tilo_relay_protocol::metrics::room_textfile_dir(state)
                    .join(format!("{short}.prom")),
            ),
        };
        let process = ConnectorProcess::start(
            app_id,
            command,
            &state.join("connectors").join(app_id),
            connector_secrets(app_id),
            Arc::clone(blobs),
        )
        .await
        .map_err(|error| format!("start the {app_id} connector: {error}"))?;
        started.insert(app_id.clone(), Arc::new(process));
    }
    Ok(started)
}

/// The secret store connector `app_id`'s worker reads its secrets from.
fn connector_secrets(app_id: &str) -> tilo_secrets::SecretStore {
    tilo_secrets::SecretStore::for_app(app_id)
}

/// Whether connector `app_id` has a secret under `purpose` on this host.
#[must_use]
pub fn has_connector_secret(app_id: &str, purpose: &'static str) -> bool {
    connector_secrets(app_id)
        .item(purpose, "default")
        .get_blocking()
        .is_ok_and(|secret| secret.is_some())
}

/// What a host gives the connectors its bots listen to: where each worker
/// program is, the blobs they fetch and publish (served to the room's
/// members once the first connector runs), and what the host serves
/// publicly that a bridge's posts point at (avatars of authors it bridged
/// out, links to the room's resources).
#[derive(Clone)]
pub struct ConnectorHost {
    /// Worker programs by connector, where the default place is not used.
    pub workers: BTreeMap<String, PathBuf>,
    /// What the host serves publicly for a bridge's posts.
    pub public: super::discord::Served,
    blobs: Arc<FsBlobs>,
    served: Arc<std::sync::atomic::AtomicBool>,
}

impl ConnectorHost {
    #[must_use]
    pub fn new(
        workers: BTreeMap<String, PathBuf>,
        blobs: FsBlobs,
        public: super::discord::Served,
    ) -> Self {
        Self {
            workers,
            public,
            blobs: Arc::new(blobs),
            served: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// The blob store connectors use, served to `rooms` the first time.
    #[must_use]
    pub fn blobs(&self, rooms: &Rooms) -> Arc<dyn tilo_connector::BlobAccess> {
        self.store(rooms)
    }

    /// The host's one blob store, served to `rooms` the first time: what
    /// connectors fetch and the files members post are kept side by side
    /// ([`super::attachments`]).
    #[must_use]
    pub fn store(&self, rooms: &Rooms) -> Arc<FsBlobs> {
        if !self.served.swap(true, std::sync::atomic::Ordering::AcqRel) {
            rooms.serve_content(self.blobs.clone());
        }
        self.blobs.clone()
    }
}

/// Keep `secret` for connector `app_id` under `purpose`, where its worker
/// asks for it.
///
/// # Errors
/// The store refused it.
pub fn put_connector_secret(
    app_id: &str,
    purpose: &'static str,
    secret: &[u8],
) -> Result<(), String> {
    connector_secrets(app_id)
        .item(purpose, "default")
        .put_blocking(secret)
        .map_err(|error| error.to_string())
}

/// The bytes connector workers download and upload, kept under one
/// directory by their BLAKE3 hash; a published blob is an avatar served at
/// the host's public avatar URL.
pub struct FsBlobs {
    dir: PathBuf,
    public: Option<crate::public_avatars::PublicAvatars>,
}

impl FsBlobs {
    /// Blobs under `dir`, published through `public` when the host serves
    /// avatars.
    ///
    /// # Errors
    /// `dir` cannot be created.
    pub fn new(
        dir: PathBuf,
        public: Option<crate::public_avatars::PublicAvatars>,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir, public })
    }

    fn path(&self, blake3: &str) -> Option<PathBuf> {
        (!blake3.is_empty() && blake3.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| self.dir.join(blake3))
    }

    /// Whether the blob `blake3` is kept here.
    #[must_use]
    pub fn contains(&self, blake3: &str) -> bool {
        self.path(blake3).is_some_and(|path| path.is_file())
    }

    /// Let go of the blob `blake3`; nothing when it is not kept.
    ///
    /// # Errors
    /// The file is there and cannot be removed.
    pub fn remove(&self, blake3: &str) -> std::io::Result<()> {
        let Some(path) = self.path(blake3) else {
            return Ok(());
        };
        match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// Members of the rooms this host's bots act in get what the bots fetched
/// (a bridged person's avatar) and the surfaces the host signs with.
impl crate::rooms::RoomContent for FsBlobs {
    fn read(&self, _room: &str, app: &str, hash: &str) -> Option<Vec<u8>> {
        crate::surfaces::surface_of(app, hash)
            .or_else(|| tilo_connector::BlobAccess::read(self, hash))
    }
}

impl tilo_connector::BlobAccess for FsBlobs {
    fn read(&self, blake3: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path(blake3)?).ok()
    }

    fn store(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let path = self.path(&hash).ok_or("not a hash")?;
        if !path.is_file() {
            tilo_files::native::atomic::write(&path, bytes).map_err(|error| error.to_string())?;
        }
        Ok(hash)
    }

    fn publish(&self, blake3: &str) -> Result<(), String> {
        let public = self.public.as_ref().ok_or("this host publishes no blobs")?;
        let bytes = self.read(blake3).ok_or("no such blob")?;
        public
            .publish(&bytes)
            .map(drop)
            .map_err(|error| error.to_string())
    }
}
