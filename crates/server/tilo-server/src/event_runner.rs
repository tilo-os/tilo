//! A workspace's events, run on this server: each event's program, carried
//! out for the member who set it up ([`tilo_workspace_service::agents::events`]).
//!
//! The runner follows the records of the apps the program names. Each change
//! goes through the dispatcher; every firing is queued and run unattended
//! through the program's API ([`BotApi`]) over its own replicas of the
//! workspace's records. A schedule's fire time and a trigger's one moment come
//! round on this server too: each fires the program the same way.
//!
//! An event changes no record: its steps are the server's own (a judgement, a
//! count, an instruction handed to its member's assistant). A run that changed
//! a record is refused, as the workspace would refuse a change from an actor
//! that is no member of it.
//!
//! Cursors, the queue and each clock trigger's next fire time are kept in the
//! event's state file, so a restarted server carries on after the last change
//! it took in, and answers a moment that passed while it was down once; a
//! firing that runs again reuses its run id, and the API answers the steps
//! already in history instead of repeating them.
//!
//! What the program itself keeps between its runs, its counts and its note,
//! is in a file beside that one ([`tilo_workspace_service::agents::kept`]), written whenever it
//! changes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tilo_actions::remote::{Caller, Pending};
use tilo_agent_session::bot::api::BotApi;
use tilo_agent_session::bot::held::HeldDocuments;
use tilo_agent_session::bot::host_steps::{HostSteps, Keeps};
use tilo_agent_session::bot::kept::{Ended, LastRun};
use tilo_agent_session::{AppCatalogue, ReplicaActor};
use tilo_automation::dispatch::queue::{DeadLetter, FailureClass, Finished, FiringQueue, classify};
use tilo_automation::dispatch::{Armed, Event, Firer, Firing, ScheduleEvent};
use tilo_automation::invocation::{AskRequest, ConfirmationRequest, InvocationHooks};
use tilo_automation::schedule::{SchedulePlan, Timed, plan};
use tilo_automation::{
    InvocationMode, InvocationRequest, InvocationTrigger, Shortcut, TriggerSpec,
};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_user_identity::DeviceCert;

use crate::workspaces::{MemberMark, Workspaces};
use tilo_workspace_service::agents::kept::KeptFile;

use tilo_lock::MutexRecover as _;

mod sources;
mod times;
/// How long the runner waits with nothing to do before it looks again.
const IDLE: Duration = Duration::from_secs(30);
/// How long a server holds an event schedule's lease before another may
/// take it: three idle looks, so a runner that is alive keeps it.
const SCHEDULE_LEASE: Duration = Duration::from_secs(90);
/// How little of that deadline is left when a live holder pushes it out
/// again: it renews every minute rather than writing the workspace on every
/// lookup.
const SCHEDULE_RENEW_NS: u64 = 30_000_000_000;
/// The input a program declares to be told the workspace's members and roles.
const WORKSPACE_INPUT: &str = "workspace";
/// The input a program declares to be told the workspace's own emoji: each
/// name with its picture's content address.
const EMOJI_INPUT: &str = "emoji";
/// The input a program declares to be told its own note: what it is for,
/// how its person wants it done, the examples kept, and what its last run
/// kept for this one.
const NOTE_INPUT: &str = "note";
/// How many dead letters an event keeps, newest last.
pub const DEAD_LETTERS_KEPT: usize = 200;

/// The program of one event, as it runs here.
#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    /// The event's id; its runs act as `bot:<id>`.
    pub id: String,
    /// The program, canonical text.
    pub program: String,
    /// Values for the program's declared inputs.
    pub inputs: Map<String, Value>,
}

/// What runs a workspace's events here: each one's program, for the member who set it up.
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
        let program = Program {
            id: definition.id.clone(),
            program: definition.program_text()?,
            inputs: definition.inputs(),
        };
        let runner = ProgramRunner::new(
            workspaces.clone(),
            workspace.to_owned(),
            workspaces.governor().clone(),
            program,
            progress,
        )?
        .runs_for(person, steps);
        let kept = runner.kept();
        Ok((kept, tokio::spawn(runner.run())))
    }
}

impl Program {
    /// The program, parsed.
    ///
    /// # Errors
    /// The stored program does not parse.
    pub fn shortcut(&self) -> Result<Shortcut, String> {
        tilo_automation::program::parse_program(self.program.as_bytes())
            .map_err(|error| error.to_string())
    }
}

/// What the runner has taken in, kept across restarts.
#[derive(Default, Serialize, Deserialize)]
struct Progress {
    /// Heads read up to, hex, by `app/record`.
    records: BTreeMap<String, Vec<String>>,
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
    fn bury(&mut self, id: &str, dead: DeadLetter) {
        tracing::warn!(event = %id, fired_by = %dead.firing.event, step = ?dead.step, reason = %dead.reason, "an event's firing is dead-lettered");
        self.dead_letters.push_back(dead);
        while self.dead_letters.len() > DEAD_LETTERS_KEPT {
            self.dead_letters.pop_front();
        }
    }
}

/// Nobody answers for an event: an unattended run never confirms or asks,
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

/// The lease one trigger of an event's schedule is fired under.
fn schedule_lease(id: &str, trigger: usize) -> String {
    format!("bot.{id}.trigger.{trigger}")
}

pub struct ProgramRunner {
    workspaces: Workspaces,
    workspace: String,
    host: DeviceCert,
    /// The key that names the runs' caller, anew at each start: a run changes
    /// no record, so nothing is ever signed with it.
    caller_key: [u8; 32],
    catalogue: AppCatalogue,
    program: Program,
    shortcut: Arc<Shortcut>,
    /// Apps whose records the program reads.
    apps: BTreeSet<String>,
    held: Arc<Mutex<HeldDocuments>>,
    api: Arc<BotApi<HeldDocuments>>,
    /// What the program keeps between its runs: its counts and its note.
    keeps: Arc<KeptFile>,
    progress: Progress,
    state_file: PathBuf,
    /// Where the runner reads the time, for its schedules' fire times.
    clock: Arc<dyn tilo_clock::Clock>,
    /// The next fire time of the schedules this server holds, ms since
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
    /// Records of the workspace the program follows that this server could not
    /// read as last tried, said in the log as they appear, not on every pass.
    unreadable: Mutex<BTreeSet<(String, String)>>,
}

impl ProgramRunner {
    /// The runner of `program` in `workspace`, for the person whose device
    /// `host` serves it, its progress kept in `state_file`.
    ///
    /// # Errors
    /// The program does not parse, or its kept progress does not read.
    pub fn new(
        workspaces: Workspaces,
        workspace: String,
        host: DeviceCert,
        program: Program,
        state_file: PathBuf,
    ) -> Result<Self, String> {
        let shortcut = program.shortcut()?;
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
        apps.extend(
            tilo_automation::requirements::requirements(&shortcut)
                .apps
                .into_iter()
                .filter(|app| catalogue.get(app).is_some()),
        );
        let installer = hex::encode(host.user_public_key);
        let access = program_access(&workspace, &installer, &program.id, &shortcut);
        let held = Arc::new(Mutex::new(HeldDocuments::new(
            catalogue.clone(),
            workspace.clone(),
            tilo_actions::MemberId::bot(program.id.clone()),
            ReplicaActor::Bot {
                installation: program.id.clone(),
                installer: installer.clone(),
                access,
            },
        )));
        let api = Arc::new(BotApi::new(
            Arc::clone(&held),
            program.id.clone(),
            installer,
            Vec::new(),
        ));
        let progress = read_progress(&state_file)?;
        let keeps = Arc::new(KeptFile::open(state_file.with_extension("kept.json"))?);
        api.keeps(Arc::clone(&keeps) as Arc<dyn Keeps>);
        Ok(Self {
            workspaces,
            workspace,
            host,
            caller_key: rand::random(),
            catalogue,
            program,
            shortcut: Arc::new(shortcut),
            apps,
            held,
            api,
            keeps,
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
            unreadable: Mutex::new(BTreeSet::new()),
        })
    }

    /// Carry the program out for `person` (hex root), whose own calendars its
    /// triggers may wait on: the steps of its program that ask this server
    /// itself, an instruction to that person's assistant, are answered
    /// through `steps` for them.
    #[must_use]
    pub fn runs_for(mut self, person: String, steps: Arc<dyn HostSteps>) -> Self {
        self.person = Some(person.clone());
        self.api.runs_for(person, steps);
        self.hear_sources();
        self
    }

    /// What the program keeps between its runs. A change made here,
    /// as when a person's assistant changes the note, is kept at once and
    /// read by the next run.
    #[must_use]
    pub fn kept(&self) -> Arc<KeptFile> {
        Arc::clone(&self.keeps)
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

    /// Firings that will not run again on their own, newest last: what
    /// failed for good, at which step and why, kept across restarts.
    pub fn dead_letters(&self) -> impl Iterator<Item = &DeadLetter> {
        self.progress.dead_letters.iter()
    }

    /// Run until the workspace is no longer governed here. The first pass
    /// catches up on everything since the kept cursors, so a restarted
    /// server answers what happened while it was down.
    pub async fn run(mut self) {
        let mut writes = self.workspaces.document_writes();
        let mut governance = self.workspaces.governance_changes();
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
                tracing::warn!(event = %self.program.id, %error, "an event's runner did not finish a pass");
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
                self.progress.bury(&self.program.id, dead);
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
                    tracing::info!(event = %self.program.id, run = %firing.invocation.0, outcome = ?report.outcome, ?finished, "an event's run finished");
                    let stopped = stopped_at(&report);
                    reports.push(*report);
                    (finished, stopped)
                }
                Err(error) => {
                    tracing::warn!(event = %self.program.id, run = %firing.invocation.0, %error, "an event's run could not be run");
                    let finished = self.progress.queue.not_run(&error, self.now_ms());
                    (finished, Some((Ended::Failed, error)))
                }
            };
            self.ran(again, stopped);
            self.gone_said = None;
            if let Finished::DeadLettered(dead) = finished {
                self.progress.bury(&self.program.id, *dead);
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
            tracing::warn!(event = %self.program.id, ?error, "how an event's run ended is not kept");
        }
    }

    /// The firings caused by every record change after the cursors, which move past them, and by every fire time of a
    /// trigger on the clock that has come.
    async fn take_in(&mut self, now_ms: i64) -> Result<Vec<Firing>, String> {
        let mut firings = self.schedule_firings(now_ms).await;
        firings.extend(self.time_firings(now_ms).await);
        firings.extend(self.source_firings(now_ms).await);
        let armed = [Armed {
            firer: Firer::Bot {
                installation: &self.program.id,
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
            // program starts.
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
            // A record first seen starts at its heads: a program answers what
            // happens after it is set up, not the workspace's past. A new
            // part of a chain of an app the program already follows (the next
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
        Ok(firings)
    }

    /// The firings of the program's triggers on the clock whose fire time has
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
        let installation = self.program.id.clone();
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

    /// Record that this device answered through `answered_ms` for the
    /// event's schedule at `trigger`, in the workspace.
    async fn answer_schedule(&self, trigger: usize, mark: &str, answered_ms: i64, now: SystemTime) {
        let installation = self.program.id.clone();
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
            tracing::warn!(event = %installation, %error, "an event schedule's progress was not kept");
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

    /// The program's own inputs, with each the server binds that it
    /// declares: the workspace's members, the workspace's emoji, its note.
    async fn host_inputs(&self) -> Result<Map<String, Value>, String> {
        let declares = |name: &str| self.shortcut.inputs.iter().any(|input| input.name == name);
        let mut inputs = self.program.inputs.clone();
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

    /// Run `firing` through the program's API. A run changes no record: one
    /// that did is refused.
    async fn fire(&self, firing: &Firing) -> Result<Fired, String> {
        // The replicas are brought up to the workspace's records first, so the
        // run reads what members did up to now: each app's own records,
        // then the parts of the chains they keep beside them. A record the
        // run cannot hold is left out of it (and said), not every run with it.
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
                    tracing::warn!(event = %self.program.id, app = %route.app_id, record = %route.document, %error, "an event's run leaves out a record it could not hold");
                }
            }
        }
        let mut inputs = self.host_inputs().await?;
        inputs.extend(firing.inputs.clone());
        let caller = Caller::bot(
            self.program.id.clone(),
            self.host.user_public_key,
            self.caller_key,
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
        // Replicas block; the run is driven on its own blocking thread so
        // they never hold a runtime worker.
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
        if let Some((route, _)) = changes.first() {
            // Its replicas start over from the workspace's records, or every
            // later run would carry the change too.
            let touched: BTreeSet<DocumentRoute> =
                changes.iter().map(|(route, _)| route.clone()).collect();
            let mut held = self.held.lock_recover();
            for route in &touched {
                held.forget(route);
            }
            drop(held);
            self.api.forget_steps();
            let reason = format!(
                "an event changes no record, and this run changed {}/{}",
                route.app_id, route.document
            );
            tracing::warn!(event = %self.program.id, run = %firing.invocation.0, %reason, "an event's run is refused");
            return Ok(Fired::Refused(reason));
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
    /// pictures live in the emoji record itself, which a run does not hold
    /// (`HeldDocuments`), so the server reads them.
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
            listed.push(serde_json::json!({ "name": name, "picture": picture }));
        }
        Ok(serde_json::json!({ "emoji": listed }))
    }

    /// The workspace's readable records of the apps the program works with. A
    /// member's own record — what they are writing and have not sent, their
    /// browsing history — is that person's alone, sealed under a channel only
    /// their devices hold: it is no record of the workspace's, and the
    /// program never follows one.
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
    /// record is left out of the program's work and said once, so what the
    /// program does read is not held up every pass by a record it never will. A
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
                        event = %self.program.id,
                        %app,
                        %record,
                        %error,
                        "a record an event follows cannot be read here; the pass goes on without it"
                    );
                }
                self.workspaces
                    .ask_to_open(&self.workspace, app, record)
                    .await;
                None
            }
        }
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

/// What event `id` may do: what the person it is carried out for may, bounded
/// to the grant its program carries (or, for a program that carries none, the
/// one derived from its steps and triggers).
fn program_access(
    workspace: &str,
    person: &str,
    id: &str,
    shortcut: &Shortcut,
) -> tilo_action_policy::MemberAccess {
    use tilo_action_policy::{Grant, Grantee, MemberAccess, WorkspacePolicy};
    let actor = Grantee::bot(id);
    let rules = if shortcut.grant.is_empty() {
        tilo_automation::grant::derived_rules(
            shortcut,
            &tilo_automation::ActionCatalog::new(Vec::new()),
        )
    } else {
        shortcut.grant.clone()
    };
    let grants: Vec<Grant> = rules
        .into_iter()
        .enumerate()
        .map(|(index, rule)| {
            Grant::new(
                format!("bot:{id}:{index}"),
                actor.clone(),
                rule.node,
                rule.kinds,
                rule.answer,
                Grantee::person(person),
            )
        })
        .collect();
    MemberAccess::person(WorkspacePolicy::new(workspace, person), person).acting_as(
        tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::bot(id)),
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

#[cfg(test)]
mod tests;
