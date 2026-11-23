//! An installed Shortcut bot, run on the host that governs its workspace.
//!
//! The runner follows the records of the apps the bot's program names and
//! the event logs of the connectors it listens to. Each change and each
//! connector event goes through the dispatcher; every firing is queued and
//! run unattended through the bot's API ([`BotApi`]) over its own
//! replicas of the workspace's records, and what a run changed is published by
//! the governor signed by the bot's own device, so every member admits it
//! as the bot's. An outbox entry a step queues is handed to the connector's
//! worker. A schedule's fire time and a trigger's one moment come round on
//! this host too: each fires the bot the same way, and no member's device
//! runs a bot installed here.
//!
//! Cursors, the queue and each clock trigger's next fire time are kept in the
//! installation's state file, so a restarted host carries on after the last
//! change and event it took in, and answers a moment that passed while it was
//! down once; a firing that runs again reuses its run id, and the API answers
//! the steps already in history instead of repeating them.
//!
//! What the program itself keeps between its runs, its counts and its note,
//! is in a file beside that one ([`tilo_workspace_service::agents::kept`]), written whenever it
//! changes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::remote::{Caller, Pending};
use tilo_agent_session::bot::api::{BotApi, OutboxSpec, PerformError, Performer};
use tilo_agent_session::bot::held::HeldDocuments;
use tilo_agent_session::bot::host_steps::{HostSteps, Keeps};
use tilo_agent_session::bot::kept::{Ended, LastRun};
use tilo_agent_session::{AppCatalogue, ReplicaActor};
use tilo_automation::dispatch::queue::{DeadLetter, FailureClass, Finished, FiringQueue, classify};
use tilo_automation::dispatch::{Armed, ConnectorEvent, Event, Firer, Firing, ScheduleEvent};
use tilo_automation::invocation::{AskRequest, ConfirmationRequest, InvocationHooks};
use tilo_automation::schedule::{SchedulePlan, Timed, plan};
use tilo_automation::{
    InvocationMode, InvocationRequest, InvocationTrigger, Shortcut, TriggerSpec,
};
use tilo_connector::ConnectorProcess;
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_user_identity::DeviceCert;

use crate::workspaces::{MemberMark, Workspaces};
use tilo_content::{ContentStore, WorkspaceStores};
use tilo_workspace_service::agents::kept::KeptFile;

// The field a connector's records keep their outbox under, which the bot's
// grant lets it settle.
use tilo_automation::grant::OUTBOX_FIELD;
use tilo_lock::MutexRecover as _;

mod sources;
mod times;
/// How long the runner waits with nothing to do before it looks again.
const IDLE: Duration = Duration::from_secs(30);
/// How long a key holder holds a bot schedule's lease before another may
/// take it: three idle looks, so a runner that is alive keeps it.
const SCHEDULE_LEASE: Duration = Duration::from_secs(90);
/// How little of that deadline is left when a live holder pushes it out
/// again: it renews every minute rather than writing the workspace on every
/// lookup.
const SCHEDULE_RENEW_NS: u64 = 30_000_000_000;
/// The input a program declares to be told the workspace's members and roles.
const WORKSPACE_INPUT: &str = "workspace";
/// The input a program declares to be told the workspace's own emoji: each
/// name with its picture's content address, the picture kept in the
/// content store its connectors read, so an op may attach it.
const EMOJI_INPUT: &str = "emoji";
/// The input a program declares to be told its own note: what it is for,
/// how its person wants it done, the examples kept, and what its last run
/// kept for this one.
const NOTE_INPUT: &str = "note";
/// How many dead letters an installation keeps, newest last.
pub const DEAD_LETTERS_KEPT: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InstalledBot {
    /// The installation's id; the bot acts as `bot:<installation>`.
    pub installation: String,
    /// Where it was installed from: the directory entry it came from, or the
    /// program file on this host when the workspace runs a program it ships.
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
    /// The version it runs, as the workspace asked for it (empty for a program
    /// file, whose text is what runs): a request for another one replaces
    /// the program and keeps the installation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub release: String,
}

/// What an event's installation says it was installed from.
const EVENT_ENTRY: &str = "event";

/// What runs a workspace's events here: each one's program as a bot of its own, for the
/// member who set it up.
#[derive(Clone, Copy, Debug, Default)]
pub struct EventRunner;

impl tilo_workspace_service::agents::events::Runner for EventRunner {
    fn run(
        &self,
        workspaces: &Workspaces,
        workspace: &str,
        definition: &tilo_agent_session::bot::event::Definition,
        person: String,
        steps: Arc<dyn HostSteps>,
        progress: PathBuf,
    ) -> Result<(Arc<KeptFile>, tokio::task::JoinHandle<()>), String> {
        let bot = InstalledBot {
            installation: definition.id.clone(),
            entry: EVENT_ENTRY.to_owned(),
            name: definition.name.clone(),
            program: definition.program_text()?,
            inputs: definition.inputs(),
            installed_at_ms: definition.set_up_at_ms,
            release: String::new(),
        };
        // An event changes no record, so nothing is ever signed with this
        // key: it only names the run's caller, anew at each start.
        let key = SigningKey::from_bytes(&rand::random());
        let runner = BotRunner::new(
            workspaces.clone(),
            workspace.to_owned(),
            workspaces.device_cert().clone(),
            key,
            bot,
            BTreeMap::new(),
            progress,
        )?
        .runs_for(person, steps);
        let kept = runner.kept();
        Ok((kept, tokio::spawn(runner.run())))
    }
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
    /// The workspace's members as last seen, for a program that follows the
    /// workspace's governance.
    #[serde(default)]
    members: Option<BTreeMap<String, MemberMark>>,
    /// What each trigger at a time a resource holds has answered, by the
    /// trigger's place among the program's triggers.
    #[serde(default)]
    times: BTreeMap<usize, tilo_automation::resource_time::Answered>,
    /// What each trigger on something new in a source has taken in, by the
    /// trigger's place among the program's triggers.
    #[serde(default)]
    sources: BTreeMap<usize, tilo_automation::source_change::Seen>,
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

/// A connector's worker as the API's performer. The API performs from
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
/// and the API answers every step at once.
struct Unattended;

impl InvocationHooks for Unattended {
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
    Ran(Box<tilo_automation::InvocationReport>),
    /// It ran, and the workspace refused what it changed, for this reason.
    Refused(String),
}

/// The lease one trigger of an installation's schedule is fired under.
fn schedule_lease(installation: &str, trigger: usize) -> String {
    format!("bot.{installation}.trigger.{trigger}")
}

pub struct BotRunner {
    workspaces: Workspaces,
    workspace: String,
    host: DeviceCert,
    /// The bot's own device, which signs what its runs change.
    bot_key: SigningKey,
    catalogue: AppCatalogue,
    bot: InstalledBot,
    shortcut: Arc<Shortcut>,
    /// Apps whose records the bot reads and acts on.
    apps: BTreeSet<String>,
    held: Arc<Mutex<HeldDocuments>>,
    api: Arc<BotApi<HeldDocuments>>,
    /// What the program keeps between its runs: its counts and its note.
    keeps: Arc<KeptFile>,
    connectors: BTreeMap<String, Arc<ConnectorProcess>>,
    progress: Progress,
    state_file: PathBuf,
    /// Where the runner reads the time, for its schedules' fire times.
    clock: Arc<dyn tilo_clock::Clock>,
    /// The next fire time of the schedules this key holder holds, ms since
    /// the epoch, so its idle wait ends at it.
    next_schedule_ms: Option<i64>,
    /// The person the program is carried out for, by their root key in hex,
    /// when it is carried out for one.
    person: Option<String>,
    /// The next moment of a time a resource holds that the program waits
    /// for, ms since the epoch, so its idle wait ends at it too.
    next_time_ms: Option<i64>,
    /// The mail of the person the program is carried out for, heard for as
    /// long as one of its triggers waits on it.
    mail: Option<sources::Mail>,
    /// What looks at the sources outside the program waits on.
    outside: sources::Outside,
    /// The next look at a source outside the program waits on, ms since the
    /// epoch, so its idle wait ends at it too.
    next_source_ms: Option<i64>,
    /// Since when the source that is gone now is, as it was last kept where
    /// the program's last end is read: kept once, and again only after
    /// something else was kept there.
    gone_said: Option<i64>,
    /// The emoji pictures already kept for the connectors, by address.
    kept: Mutex<BTreeSet<String>>,
    /// Records of the workspace the bot follows that this host could not read
    /// as last tried, said in the log as they appear, not on every pass.
    unreadable: Mutex<BTreeSet<(String, String)>>,
    /// How many more changes are published before the workspace is made to
    /// refuse one, for a test.
    #[cfg(test)]
    refuse_after: Mutex<Option<usize>>,
}

impl BotRunner {
    /// The runner of `bot` in `workspace`, installed by the workspace's root whose
    /// device `host` governs it, signing as the bot's device `bot_key`
    /// (see [`super::membership::HostRootCertifier::device_key`]), its
    /// connectors already started, its progress kept in `state_file`.
    pub fn new(
        workspaces: Workspaces,
        workspace: String,
        host: DeviceCert,
        bot_key: SigningKey,
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
                TriggerSpec::ResourceChanged { kind, .. } => {
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
        let access = bot_access(
            &workspace,
            &installer,
            &bot.installation,
            &shortcut,
            &connectors.keys().cloned().collect(),
        );
        let held = Arc::new(Mutex::new(HeldDocuments::new(
            catalogue.clone(),
            workspace.clone(),
            tilo_actions::MemberId::bot(bot.installation.clone()),
            ReplicaActor::Bot {
                installation: bot.installation.clone(),
                installer: installer.clone(),
                access,
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
        let api = Arc::new(BotApi::new(
            Arc::clone(&held),
            bot.installation.clone(),
            installer,
            outboxes,
        ));
        let progress = read_progress(&state_file)?;
        let keeps = Arc::new(KeptFile::open(state_file.with_extension("kept.json"))?);
        api.keeps(Arc::clone(&keeps) as Arc<dyn Keeps>);
        Ok(Self {
            workspaces,
            workspace,
            host,
            bot_key,
            catalogue,
            bot,
            shortcut: Arc::new(shortcut),
            apps,
            held,
            api,
            keeps,
            connectors,
            progress,
            state_file,
            clock: Arc::new(tilo_clock::SystemClock),
            next_schedule_ms: None,
            person: None,
            next_time_ms: None,
            mail: None,
            outside: sources::Outside::default(),
            next_source_ms: None,
            gone_said: None,
            kept: Mutex::new(BTreeSet::new()),
            unreadable: Mutex::new(BTreeSet::new()),
            #[cfg(test)]
            refuse_after: Mutex::new(None),
        })
    }

    /// Carry the bot's program out for `person` (hex root), whose own
    /// calendars its triggers may wait on: the steps of its program that ask
    /// this holder itself, an instruction to that person's assistant, are
    /// answered through `steps` for them. A bot the operator installed runs
    /// for nobody and is offered none.
    #[must_use]
    pub fn runs_for(mut self, person: String, steps: Arc<dyn HostSteps>) -> Self {
        self.person = Some(person.clone());
        self.api.runs_for(person, steps);
        self.hear_sources();
        self
    }

    /// What the bot's program keeps between its runs. A change made here,
    /// as when a person's assistant changes the note, is kept at once and
    /// read by the next run.
    #[must_use]
    pub fn kept(&self) -> Arc<KeptFile> {
        Arc::clone(&self.keeps)
    }

    /// Have the workspace refuse the change published after `published` more.
    #[cfg(test)]
    pub(crate) fn refuse_change_after(&self, published: usize) {
        *self.refuse_after.lock_recover() = Some(published);
    }

    /// Read the time from `clock` rather than this host's own clock, so a
    /// test reaches a schedule's fire time without waiting for it.
    #[cfg(test)]
    pub(crate) fn use_clock(&mut self, clock: Arc<dyn tilo_clock::Clock>) {
        self.clock = clock;
    }

    /// What the clock reads now, ms since the epoch.
    fn now_ms(&self) -> i64 {
        i64::try_from(self.clock.now_ms()).unwrap_or(i64::MAX)
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

    /// Run until the workspace is no longer governed here. The first pass
    /// catches up on everything since the kept cursors, so a restarted
    /// host answers what happened while it was down.
    pub async fn run(mut self) {
        let mut writes = self.workspaces.document_writes();
        let mut governance = self.workspaces.governance_changes();
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
        // A program whose triggers are all on the clock answers no change:
        // what members write does not wake it, so hundreds of reminders
        // cost a workspace's writers nothing. A run still reads the records
        // as they stand when it starts.
        let follows = self.shortcut.triggers.iter().any(|trigger| {
            Timed::of(trigger).is_none()
                && !times::waits_on_a_time(trigger)
                && !sources::waits_on_a_source(trigger)
        });
        // Mail that arrives wakes a program that waits on its person's.
        let hears_mail = self.hears_mail();
        let mut mail = self.workspaces.mail_arrival_changes();
        loop {
            if let Err(error) = self.step().await {
                tracing::warn!(installation = %self.bot.installation, %error, "bot runner did not finish a pass");
            }
            // Wake for the queue's head or a schedule's fire time when
            // either comes before the idle look.
            let wake = self
                .progress
                .queue
                .wake_at_ms()
                .into_iter()
                .chain(self.next_schedule_ms)
                .chain(self.next_time_ms)
                .chain(self.next_source_ms)
                .min()
                .map_or(IDLE, |at| {
                    Duration::from_millis(
                        u64::try_from(at.saturating_sub(self.now_ms())).unwrap_or(0),
                    )
                })
                .min(IDLE);
            tokio::select! {
                write = writes.recv(), if follows => {
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
                arrived = mail.changed(), if hears_mail => {
                    if arrived.is_err() {
                        return;
                    }
                }
                () = tokio::time::sleep(wake) => {}
            }
        }
    }

    /// Take in what changed since the cursors, queue the firings, run
    /// those that are due as it begins, and keep the progress. Answers the
    /// runs' reports.
    ///
    /// A firing the host could not run is tried again later, and
    /// dead-lettered once its attempts are spent.
    ///
    /// # Errors
    /// A record could not be read or the state file written.
    pub async fn step(&mut self) -> Result<Vec<tilo_automation::InvocationReport>, String> {
        let now = self.now_ms();
        // A paused program answers nothing. What comes while it is paused
        // is still taken in, so it is not answered later either; a firing
        // it had queued before waits until it goes on.
        let paused = self.keeps.paused();
        for firing in self.take_in(now).await? {
            if paused {
                continue;
            }
            if let Some(dead) = self.progress.queue.push(firing, now) {
                self.progress.bury(&self.bot.installation, dead);
            }
        }
        self.keep()?;
        let mut reports = Vec::new();
        // What was due when the pass began: a firing put back during it (a
        // refused run, one the host could not run) waits for the next pass
        // at its backoff, even when this pass outlasted the backoff, so a
        // pass never runs one firing twice.
        while let Some(queued) = self.progress.queue.next_due(now).filter(|_| !paused) {
            let (firing, again) = (queued.firing.clone(), queued.attempts > 0);
            let (finished, stopped) = match self.fire(&firing).await {
                Ok(Fired::Refused(reason)) => {
                    // What the run changed before the refused change stands
                    // and the rest was dropped: run it again, from the
                    // workspace's records, so it finishes what it started.
                    let finished = self.progress.queue.refused(&reason, self.now_ms());
                    (finished, Some((Ended::Refused, reason)))
                }
                Ok(Fired::Ran(report)) => {
                    let finished = self.progress.queue.finished(&report, self.now_ms());
                    tracing::info!(installation = %self.bot.installation, run = %firing.invocation.0, outcome = ?report.outcome, ?finished, "bot run finished");
                    let stopped = stopped_at(&report);
                    reports.push(*report);
                    (finished, stopped)
                }
                Err(error) => {
                    tracing::warn!(installation = %self.bot.installation, run = %firing.invocation.0, %error, "bot run could not be run");
                    let finished = self.progress.queue.not_run(&error, self.now_ms());
                    (finished, Some((Ended::Failed, error)))
                }
            };
            self.ran(again, stopped);
            self.gone_said = None;
            if let Finished::DeadLettered(dead) = finished {
                self.progress.bury(&self.bot.installation, *dead);
            }
            self.keep()?;
        }
        self.keep_gone();
        Ok(reports)
    }

    /// Keep how a run ended with what the program keeps, where whoever
    /// lists the program reads it: that it ran, or why it stopped. A run
    /// tried `again` is the same run ending once more.
    fn ran(&self, again: bool, stopped: Option<(Ended, String)>) {
        let (ended, why) = stopped.map_or((Ended::Ran, None), |(ended, why)| (ended, Some(why)));
        let last = LastRun {
            at_ms: self.now_ms(),
            ended,
            why,
        };
        if let Err(error) = self.keeps.ran(last, again) {
            tracing::warn!(installation = %self.bot.installation, ?error, "how a bot's run ended is not kept");
        }
    }

    /// The firings caused by every record change and connector event after
    /// the cursors, which move past them, and by every fire time of a
    /// trigger on the clock that has come.
    async fn take_in(&mut self, now_ms: i64) -> Result<Vec<Firing>, String> {
        let mut firings = self.schedule_firings(now_ms).await;
        firings.extend(self.time_firings(now_ms).await);
        firings.extend(self.source_firings(now_ms).await);
        let armed = [Armed {
            firer: Firer::Bot {
                installation: &self.bot.installation,
            },
            shortcut: &self.shortcut,
        }];
        if self.follows_governance() {
            let (generation, members) = self
                .workspaces
                .membership(&self.workspace)
                .await
                .map_err(|error| error.to_string())?;
            // Like a record, the workspace's membership first seen is where the
            // bot starts.
            if let Some(before) = self.progress.members.replace(members.clone()) {
                let owner = hex::encode(self.host.user_public_key);
                for event in
                    governance_events(&self.workspace, &owner, generation, &before, &members)
                {
                    firings.extend(
                        tilo_automation::dispatch::dispatch(&armed, Event::Record(&event)).firings,
                    );
                }
            }
        }
        for (app, record) in self.routes().await? {
            let Some(saved) = self.reads(&app, &record).await else {
                continue;
            };
            let key = format!("{app}/{record}");
            let mut doc = AutoCommit::load(&saved).map_err(|error| error.to_string())?;
            let heads: Vec<String> = doc.get_heads().iter().map(ToString::to_string).collect();
            // A record first seen starts at its heads: a bot answers what
            // happens after it is installed, not the workspace's past. A new
            // part of a chain of an app the bot already follows (the next
            // part of a chat's messages) is what happened since: read whole.
            // So is a record every app of the workspace opens (the workspace's
            // emoji), which its first change creates.
            let part = collab_types::document::DocumentId::parse(&record)
                .is_some_and(|record| record.chain_part().is_some())
                || collab_types::workspace_document(&app, &record).is_some();
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
            let kind = collaboration::resource::identity(&doc)
                .map(|checkout| checkout.kind.as_str().to_owned())
                .or_else(|| {
                    self.catalogue
                        .get(&app)
                        .map(|definition| definition.resource_type.as_str().to_owned())
                })
                .unwrap_or_default();
            for (event, _) in tilo_agent_session::bot::events_since(
                &self.workspace,
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

    /// The firings of the bot's triggers on the clock whose fire time has
    /// come by `now_ms`, each fire time answered once. The progress is the
    /// workspace's, not this host's: every key holder reads the latest fire
    /// time any of them answered, so a schedule carries on across a restart
    /// and from one key holder to the next, and a time missed while none ran
    /// is caught up once when the trigger asks to and never once per period; a
    /// moment that passed while every key holder was down fires once, however
    /// late, and never again. This device fires a trigger only while it holds
    /// that trigger's lease in the workspace; another key holder takes the
    /// lease over once this one stops renewing it.
    async fn schedule_firings(&mut self, now_ms: i64) -> Vec<Firing> {
        let installation = self.bot.installation.clone();
        let shortcut = Arc::clone(&self.shortcut);
        let workspace = self.workspace.clone();
        let armed = [Armed {
            firer: Firer::Bot {
                installation: &installation,
            },
            shortcut: &shortcut,
        }];
        let now = SystemTime::UNIX_EPOCH
            .checked_add(Duration::from_millis(
                u64::try_from(now_ms).unwrap_or_default(),
            ))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let zone = jiff::tz::TimeZone::system();
        let mut firings = Vec::new();
        let mut next: Option<i64> = None;
        for (index, trigger) in shortcut.triggers.iter().enumerate() {
            let Some(timed) = Timed::of(trigger) else {
                continue;
            };
            let mark = tilo_peer_state::trigger_mark(trigger);
            let lease = schedule_lease(&installation, index);
            // Fires only while this device holds the trigger's lease.
            if !self.hold_schedule_lease(&lease, now).await {
                continue;
            }
            // Read the merged state once more: a key holder that claimed the
            // same term is visible now, and only its ranking's winner fires.
            let Some(state) = self.workspaces.state(&workspace).await else {
                continue;
            };
            let held = state
                .workspace()
                .lease(&lease, now)
                .holder
                .is_some_and(|holder| holder.device == self.host.device_public_key);
            if !held {
                continue;
            }
            let answered = state
                .workspace()
                .schedule_answered(&installation, index, &mark);
            let step = plan(timed, answered, now_ms, &zone);
            let answered_ms = match step {
                SchedulePlan::Waiting { answered_ms } => answered_ms,
                SchedulePlan::Fire {
                    due_ms,
                    answered_ms,
                    ..
                } => {
                    let event = ScheduleEvent {
                        workspace: workspace.clone(),
                        trigger: index,
                        due_ms,
                    };
                    firings.extend(
                        tilo_automation::dispatch::dispatch(&armed, Event::Schedule(&event))
                            .firings,
                    );
                    answered_ms
                }
            };
            self.answer_schedule(index, &mark, answered_ms, now).await;
            if let Some(at) = timed.waiting_ms(answered_ms, &zone) {
                next = Some(next.map_or(at, |kept| kept.min(at)));
            }
        }
        self.next_schedule_ms = next;
        firings
    }

    /// Whether this device holds the schedule's `lease`, taking it or
    /// pushing its deadline out when it does not yet or the deadline is close.
    /// A live holder renews once its deadline is within
    /// [`SCHEDULE_RENEW_NS`], not on every pass: the workspace is written only
    /// when its state changes.
    ///
    /// A draining server (one about to restart) holds none: it lets its claim
    /// go, so another holder of the workspace takes the schedule at once.
    async fn hold_schedule_lease(&self, lease: &str, now: SystemTime) -> bool {
        if self.workspaces.draining() {
            if let Err(error) = self
                .workspaces
                .author_own_workspace_state(
                    &self.workspace,
                    self.workspaces.serving_certificate(),
                    |peer| {
                        peer.workspace_mut().release_lease(
                            self.workspaces.serving_certificate(),
                            lease,
                            now,
                        )
                    },
                )
                .await
            {
                tracing::warn!(%error, "a draining server's schedule claim was not let go");
            }
            return false;
        }
        let Some(state) = self.workspaces.state(&self.workspace).await else {
            return false;
        };
        let now_ns = u64::try_from(
            now.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        )
        .unwrap_or(u64::MAX);
        if let Some(holder) = state.workspace().lease(lease, now).holder
            && holder.device == self.host.device_public_key
            && holder.expires_at_ns.saturating_sub(now_ns) > SCHEDULE_RENEW_NS
        {
            return true;
        }
        let claimed = self
            .workspaces
            .author_own_workspace_state(&self.workspace, &self.host, |peer| {
                peer.workspace_mut().take_lease(
                    &self.host,
                    lease,
                    SCHEDULE_LEASE,
                    tilo_peer_state::unranked(),
                    now,
                )
            })
            .await;
        matches!(claimed, Ok(Some(_)))
    }

    /// Record that this device answered through `answered_ms` for the bot's
    /// schedule at `trigger`, in the workspace.
    async fn answer_schedule(&self, trigger: usize, mark: &str, answered_ms: i64, now: SystemTime) {
        let installation = self.bot.installation.clone();
        let host = &self.host;
        let workspace = &self.workspace;
        if let Err(error) = self
            .workspaces
            .author_own_workspace_state(workspace, host, |peer| {
                peer.workspace_mut().answer_schedule(
                    host,
                    &installation,
                    trigger,
                    mark,
                    answered_ms,
                    now,
                )
            })
            .await
        {
            tracing::warn!(%installation, %error, "a bot schedule's progress was not kept");
        }
    }

    /// Whether the runner already follows records of `app`.
    fn follows_app(&self, app: &str) -> bool {
        let prefix = format!("{app}/");
        self.progress
            .records
            .keys()
            .any(|key| key.starts_with(&prefix))
    }

    /// Whether the program listens to the workspace's own governance.
    fn follows_governance(&self) -> bool {
        self.shortcut.triggers.iter().any(|trigger| {
            matches!(trigger, TriggerSpec::ResourceChanged { kind, .. } if kind == tilo_action_policy::roles::WORKSPACE_KIND)
        })
    }

    /// The bot's own inputs, with each the host binds that its program
    /// declares: the workspace's members, the workspace's emoji.
    async fn host_inputs(&self) -> Result<Map<String, Value>, String> {
        let declares = |name: &str| self.shortcut.inputs.iter().any(|input| input.name == name);
        let mut inputs = self.bot.inputs.clone();
        if declares(WORKSPACE_INPUT) {
            inputs.insert(WORKSPACE_INPUT.to_owned(), self.workspace_input().await?);
        }
        if declares(EMOJI_INPUT) {
            inputs.insert(EMOJI_INPUT.to_owned(), self.emoji_input().await?);
        }
        if declares(NOTE_INPUT) {
            inputs.insert(NOTE_INPUT.to_owned(), self.keeps.note());
        }
        Ok(inputs)
    }

    /// Run `firing` through the bot's API and publish what it changed.
    async fn fire(&self, firing: &Firing) -> Result<Fired, String> {
        // The replicas are brought up to the workspace's records first, so the
        // run reads what members did up to now: each app's own records,
        // then the parts of the chains they keep beside them. A record the
        // bot cannot hold is left out of the run (and said), not every run
        // with it.
        let mut routes = self.routes().await?;
        routes.sort_by_key(|(_, record)| {
            collab_types::document::DocumentId::parse(record)
                .is_some_and(|record| record.chain_part().is_some())
        });
        for (app, record) in routes {
            if let Some(saved) = self.reads(&app, &record).await {
                let route = DocumentRoute {
                    app_id: app.clone(),
                    cluster_id: self.workspace.clone(),
                    document: record,
                };
                let held = Arc::clone(&self.held);
                let holding = route.clone();
                let held =
                    tokio::task::spawn_blocking(move || held.lock_recover().hold(&holding, saved))
                        .await
                        .map_err(|error| error.to_string())?;
                if let Err(error) = held {
                    tracing::warn!(installation = %self.bot.installation, app = %route.app_id, record = %route.document, %error, "bot run leaves out a record it could not hold");
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
        .in_workspace(self.workspace.clone());
        let (api, shortcut, handle) = (
            Arc::clone(&self.api),
            Arc::clone(&self.shortcut),
            tokio::runtime::Handle::current(),
        );
        let (run, trigger, cause) = (
            firing.invocation,
            firing.trigger.clone(),
            firing.cause.clone(),
        );
        api.set_cause(cause.clone());
        // Replicas and connector performs block; the run is driven on its
        // own blocking thread so they never hold a runtime worker.
        let report = tokio::task::spawn_blocking(move || {
            handle.block_on(tilo_automation::invocation::run(
                &*api,
                &Unattended,
                InvocationRequest {
                    shortcut: &shortcut,
                    invocation: run,
                    caller,
                    mode: InvocationMode::Unattended,
                    trigger: InvocationTrigger::Fired(trigger),
                    inputs,
                    caused_by: cause,
                    parent: None,
                    chain: Vec::new(),
                    resume: None,
                    from: None,
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
                // out of the workspace; the bot's replicas they touched start
                // over from the workspace's records, or every later change of
                // theirs would carry the refused one and be refused too.
                let touched: BTreeSet<DocumentRoute> = std::iter::once(route.clone())
                    .chain(changes.map(|(_, (route, _))| route))
                    .collect();
                let mut held = self.held.lock_recover();
                for route in &touched {
                    held.forget(route);
                }
                drop(held);
                self.api.forget_steps();
                let reason = format!(
                    "the workspace refused the bot's change to {}/{}: {refusal}",
                    route.app_id, route.document
                );
                tracing::warn!(
                    installation = %self.bot.installation,
                    run = %firing.invocation.0,
                    app = %route.app_id,
                    record = %route.document,
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

    /// Who is in the workspace, by name and in what role, as of now: `{"members":
    /// [{"member", "name", "role"}]}`. A person the workspace removed keeps their name and has
    /// no role. A program reads it to judge an outside request by the requester's access here
    /// and to name who wrote what it carries.
    pub(crate) async fn workspace_input(&self) -> Result<Value, String> {
        let summary = self
            .workspaces
            .summary(&self.workspace)
            .await
            .map_err(|error| error.to_string())?;
        // The workspace's root owns it whether or not governance lists the
        // person as a member.
        let root = tilo_actions::MemberId::person(hex::encode(self.host.user_public_key));
        let root_name = summary
            .members
            .iter()
            .find(|member| member.id == root)
            .map(|member| member.name.clone());
        let mut members =
            vec![serde_json::json!({ "member": root, "name": root_name, "role": "owner" })];
        members.extend(
            summary
                .members
                .iter()
                .filter(|member| member.root() != Some(self.host.user_public_key))
                .map(|member| {
                    let role = if member.removed {
                        None
                    } else if member.administrator {
                        Some("owner")
                    } else {
                        member.role.map(tilo_peer_state::join::Role::key)
                    };
                    serde_json::json!({
                        "member": member.id,
                        "name": member.name,
                        "role": role,
                    })
                }),
        );
        Ok(serde_json::json!({ "members": members }))
    }

    /// The workspace's own emoji as the workspace holds them now: `{"emoji":
    /// [{"name", "picture"}]}`, each picture its content address. The
    /// pictures live in the emoji record itself, which a bot does not hold
    /// (`HeldDocuments`), so the host reads them and keeps each in the
    /// content store of every connector the bot runs, where an op that
    /// attaches it finds it.
    pub(crate) async fn emoji_input(&self) -> Result<Value, String> {
        use tilo_resource::Resource as _;
        use tilo_workspace_emoji::{KIND, OWNER, WorkspaceEmoji};
        let record = collab_types::workspace_document_id(KIND);
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

    /// The workspace's readable records of the apps the bot works with. A
    /// member's own record — what they are writing and have not sent, their
    /// browsing history — is that person's alone, sealed under a channel only
    /// their devices hold: it is no record of the workspace's, and the bot
    /// never follows one.
    async fn routes(&self) -> Result<Vec<(String, String)>, String> {
        Ok(self
            .workspaces
            .workspace_document_routes(&self.workspace)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|(app, _)| self.apps.contains(app))
            .collect())
    }

    async fn read(&self, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        self.workspaces
            .resource_document(&self.workspace, app, record)
            .await
            .map_err(|error| error.to_string())
    }

    /// `record` of `app` as the governor's copy reads it now, or `None` when it
    /// is not this host's to read: a record this device takes no part in the
    /// channel of (yet), whose keys have not reached it, or which was written
    /// before its channel was sealed and can no longer be opened. Such a
    /// record is left out of the work the bot does and said once, so what the
    /// bot does hold is not held up every pass by a record it never will. A
    /// record shut because its channel has not admitted this device yet is
    /// asked for, on the same path every member takes, and reads once it is.
    async fn reads(&self, app: &str, record: &str) -> Option<Vec<u8>> {
        match self.read(app, record).await {
            Ok(saved) => {
                self.unreadable.lock_recover().remove(&named(app, record));
                saved
            }
            Err(error) => {
                if self.unreadable.lock_recover().insert(named(app, record)) {
                    tracing::warn!(
                        installation = %self.bot.installation,
                        %app,
                        %record,
                        %error,
                        "a record the bot follows cannot be read here; the pass goes on without it"
                    );
                }
                self.workspaces
                    .ask_to_open(&self.workspace, app, record)
                    .await;
                None
            }
        }
    }

    /// Put a run's changes into the workspace's record, signed by the bot's
    /// device, so every member admits them as the bot's. Fails when the
    /// workspace did not take them (outside the bot's grants, or from a bot it
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
        self.workspaces
            .publish_as_bot(
                &self.workspace,
                &self.host,
                &self.bot_key,
                &self.catalogue,
                &route.app_id,
                &route.document,
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

/// Why `report`'s run did not run to its end: refused, where trying again
/// changes nothing, or failed. None for a run that did.
fn stopped_at(report: &tilo_automation::InvocationReport) -> Option<(Ended, String)> {
    match &report.outcome {
        tilo_automation::InvocationOutcome::Completed => None,
        tilo_automation::InvocationOutcome::Failed { failure, .. } => {
            let ended = if classify(failure) == FailureClass::Refusal {
                Ended::Refused
            } else {
                Ended::Failed
            };
            Some((ended, failure.to_string()))
        }
        tilo_automation::InvocationOutcome::Cancelled { .. } => {
            Some((Ended::Failed, "the run was cancelled".to_owned()))
        }
    }
}

/// `record` of `app`, as the runner names one record across passes.
fn named(app: &str, record: &str) -> (String, String) {
    (app.to_owned(), record.to_owned())
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
/// workspace's root, may, bounded to the grant its program carries (or, for a
/// program that carries none, the one derived from its steps and
/// triggers), and to settling its connectors' outboxes.
fn bot_access(
    workspace: &str,
    installer: &str,
    installation: &str,
    shortcut: &Shortcut,
    connectors: &BTreeSet<String>,
) -> tilo_action_policy::MemberAccess {
    use tilo_action_policy::{
        Answer, Grant, Grantee, KindSet, Level, MemberAccess, Rule, TreePath, WorkspacePolicy,
    };
    let bot = Grantee::bot(installation);
    let mut rules = if shortcut.grant.is_empty() {
        tilo_automation::grant::derived_rules(
            shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        )
    } else {
        shortcut.grant.clone()
    };
    // Settling an entry is the bot recording what its connector answered.
    // It takes the entry out of the outbox, which the workspace judges as
    // reaching outside, as it did: the connector reached it.
    rules.extend(connectors.iter().flat_map(|app| {
        let app = TreePath::root().any(Level::Workspace).app(app.clone());
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
    let grants: Vec<Grant> = rules
        .into_iter()
        .enumerate()
        .map(|(index, rule)| {
            Grant::new(
                format!("bot:{installation}:{index}"),
                bot.clone(),
                rule.node,
                rule.kinds,
                rule.answer,
                Grantee::person(installer),
            )
        })
        .collect();
    MemberAccess::person(WorkspacePolicy::new(workspace, installer), installer).acting_as(
        tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::bot(installation)),
        grants,
    )
}

/// What changed in the workspace's membership from `before` to `after`, as
/// the workspace's own governance verbs on its `tilo.workspace` resource: `add-member`
/// (joined, or came back), `set-role`, `remove-member` (removed, or left),
/// `ban` and `unban`, each with `person` (hex root) and, where it has one,
/// `role`. A program follows them as any record's changes; the change id
/// names the workspace's governance generation, so each event fires once.
#[must_use]
pub fn governance_events(
    workspace: &str,
    owner: &str,
    generation: u64,
    before: &BTreeMap<String, MemberMark>,
    after: &BTreeMap<String, MemberMark>,
) -> Vec<tilo_automation::dispatch::DocumentEvent> {
    use tilo_action_policy::roles::governance;
    let mut events = Vec::new();
    let mut verb = |verb: &str, person: &str, role: Option<&str>| {
        let mut args = vec![("person".to_owned(), Value::from(person))];
        if let Some(role) = role {
            args.push(("role".to_owned(), Value::from(role)));
        }
        let arguments: Map<String, Value> = args.iter().cloned().collect();
        let change = blake3::hash(
            serde_json::to_string(&(workspace, generation, verb, person, role))
                .unwrap_or_default()
                .as_bytes(),
        )
        .to_hex()
        .to_string();
        events.push(tilo_automation::dispatch::DocumentEvent {
            workspace: workspace.to_owned(),
            record: String::new(),
            kind: tilo_action_policy::roles::WORKSPACE_KIND.to_owned(),
            change,
            actor: tilo_actions::Acting::person(owner),
            did: tilo_actions::entry::Did::Verb(tilo_actions::entry::VerbCall::new(
                tilo_actions::ActionKey::new(
                    WORKSPACE_APP,
                    tilo_action_policy::roles::WORKSPACE_KIND,
                    verb,
                ),
                args.iter().filter_map(|(key, value)| {
                    tilo_actions::entry::Scalar::from_json(value).map(|value| (key.clone(), value))
                }),
            )),
            arguments,
            item: None,
            before: None,
            after: None,
            caused_by: None,
            origin: None,
            incoming_message: false,
            housekeeping: false,
            shortcut: None,
            invocation: None,
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

/// The app id the workspace's governance verbs are named under.
const WORKSPACE_APP: &str = "so.tilo.desktop";

/// The bot `entry` installed in the workspace with `inputs`: the installation
/// kept under `dir` when the workspace installed it before, else a new one.
/// `entry` is a program file on this host, or a directory entry fetched
/// and verified from the first enabled registry at the `version` input.
///
/// A kept installation from a program file runs the file as it is now: the
/// host's image ships the program, and a fix to it (a grant the bot was
/// missing) reaches a workspace installed by an older image. The installation,
/// its name, its inputs and when it was installed stay; [`super::membership::keep_member`]
/// then updates the workspace's row to the new program and its grants.
///
/// # Errors
/// The program cannot be read, fetched, verified or parsed, or the
/// installation cannot be kept.
pub async fn install(
    dir: &Path,
    entry: &str,
    inputs: Map<String, Value>,
) -> Result<InstalledBot, String> {
    install_release(dir, entry, inputs, None).await
}

/// [`install`], running `program` (a developer's own program for `entry`,
/// at the `version` input) instead of the listed one when given. A kept
/// installation asked for another version runs it from now on and stays
/// the same bot: its installation, name, inputs and install time are kept,
/// and [`super::membership::keep_member`] moves the workspace's row to the new
/// program and its grants.
///
/// # Errors
/// As [`install`].
pub async fn install_release(
    dir: &Path,
    entry: &str,
    mut inputs: Map<String, Value>,
    program: Option<&str>,
) -> Result<InstalledBot, String> {
    let kept = dir.join("bot.json");
    let version = inputs.remove("version");
    let release = version
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    // A program given or shipped as a file on this host runs as it is now.
    let local = match program {
        Some(text) => Some(program_text(entry, text.as_bytes())),
        None => Path::new(entry).is_file().then(|| program_file(entry)),
    };
    if let Ok(bytes) = std::fs::read(&kept) {
        let mut bot: InstalledBot = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: {error}", kept.display()))?;
        if bot.entry == entry {
            // The name stays: the bot's device certificate carries it, and
            // a renewal keeps the same certificate.
            let program = match local {
                Some(local) => local?.1,
                None if bot.release == release => return Ok(bot),
                None => fetch_listed(entry, &release).await?.1,
            };
            if bot.program != program || bot.release != release {
                bot.program = program;
                bot.release = release;
                keep_installed(dir, &kept, &bot)?;
            }
            return Ok(bot);
        }
    }
    let (name, program) = match local {
        Some(local) => local?,
        None => fetch_listed(entry, &release).await?,
    };
    let bot = InstalledBot {
        installation: uuid::Uuid::new_v4().to_string(),
        entry: entry.to_owned(),
        name,
        program,
        inputs,
        installed_at_ms: tilo_clock::unix_now_ms_i64(),
        release,
    };
    keep_installed(dir, &kept, &bot)?;
    Ok(bot)
}

/// The name and program of directory entry `entry` at `version`, fetched
/// and verified from the first enabled registry.
async fn fetch_listed(entry: &str, version: &str) -> Result<(String, String), String> {
    if version.is_empty() {
        return Err(format!(
            "{entry}: a directory bot is installed at a version (version=…)"
        ));
    }
    let version = version.to_owned();
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
    Ok((
        listed.entry.name.resolve("en").unwrap_or(entry).to_owned(),
        listed.program_text,
    ))
}

/// The name and canonical program text of the program file `entry`.
fn program_file(entry: &str) -> Result<(String, String), String> {
    let bytes = std::fs::read(entry).map_err(|error| format!("{entry}: {error}"))?;
    program_text(entry, &bytes)
}

/// The name and canonical text of the program `bytes` for `entry`.
fn program_text(entry: &str, bytes: &[u8]) -> Result<(String, String), String> {
    let shortcut = tilo_automation::program::parse_program(bytes)
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
                tilo_server_protocol::metrics::workspace_textfile_dir(state)
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

/// What connectors run with on this host: their worker programs, what the
/// host serves publicly for a bridge's posts (avatars of authors it bridged
/// out, links to the workspace's resources), and the host's content, where
/// what they fetch is kept in the workspace they serve.
#[derive(Clone)]
pub struct ConnectorHost {
    /// Worker programs by connector, where the default place is not used.
    pub workers: BTreeMap<String, PathBuf>,
    /// What the host serves publicly for a bridge's posts.
    pub public: super::discord::Served,
    content: Arc<WorkspaceStores>,
    avatars: Option<Arc<crate::public_avatars::PublicAvatars>>,
}

impl ConnectorHost {
    /// Connectors keeping what they fetch in `content`, publishing the
    /// avatars a bridge posts through `avatars` when the host serves them.
    #[must_use]
    pub fn new(
        workers: BTreeMap<String, PathBuf>,
        content: Arc<WorkspaceStores>,
        avatars: Option<crate::public_avatars::PublicAvatars>,
        public: super::discord::Served,
    ) -> Self {
        Self {
            workers,
            public,
            content,
            avatars: avatars.map(Arc::new),
        }
    }

    /// The content the connectors of `workspace` keep and read.
    ///
    /// # Errors
    /// `workspace` is not a workspace's id.
    pub fn blobs(&self, workspace: &str) -> Result<Arc<dyn tilo_connector::BlobAccess>, String> {
        Ok(Arc::new(WorkspaceBlobs {
            store: self.content.get(workspace)?,
            avatars: self.avatars.clone(),
        }))
    }

    /// `workspace`'s content store on this host.
    ///
    /// # Errors
    /// `workspace` is not a workspace's id.
    pub fn store(&self, workspace: &str) -> Result<ContentStore, String> {
        self.content.get(workspace)
    }
}

/// One workspace's content, as its connectors' workers download and upload
/// it: a blob a bridge publishes is an avatar served at the host's public
/// avatar URL.
struct WorkspaceBlobs {
    store: ContentStore,
    avatars: Option<Arc<crate::public_avatars::PublicAvatars>>,
}

impl tilo_connector::BlobAccess for WorkspaceBlobs {
    fn read(&self, blake3: &str) -> Option<Vec<u8>> {
        self.store.read(blake3)
    }

    fn store(&self, bytes: &[u8]) -> Result<String, String> {
        self.store.put(bytes).map_err(|error| error.to_string())
    }

    fn publish(&self, blake3: &str) -> Result<(), String> {
        let avatars = self
            .avatars
            .as_ref()
            .ok_or("this host publishes no blobs")?;
        let bytes = self.store.read(blake3).ok_or("no such blob")?;
        avatars
            .publish(&bytes)
            .map(drop)
            .map_err(|error| error.to_string())
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
