//! The record of long work, and the registry it lives in.
//!
//! One registry per process: the work outlives the window that started it,
//! so a job is found again by whoever looks next, and its progress is read
//! without asking the app that started it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use crate::attention::Owner;
use crate::lock::MutexRecover as _;
use crate::time::{Instant, now};
use crate::wake::WakeHandle;

/// How many rows the registry keeps: running jobs always, and the finished
/// ones nobody has read yet, oldest dropped first. A row leaves when it is
/// read, never on a timer.
const MAX_ROWS: usize = 64;

/// A job's identity, unique for the life of the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(u64);

impl JobId {
    /// The id as it is written in a log line or a test.
    #[must_use]
    pub const fn number(self) -> u64 {
        self.0
    }

    /// The id a [`crate::attention::Target::Job`] carries back to it.
    #[must_use]
    pub const fn from_number(number: u64) -> Self {
        Self(number)
    }
}

/// What a job counts: the unit its progress is read in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Bytes on the disk or on the wire ("312 MB").
    Bytes,
    /// Files ("12,480 files").
    Files,
    /// Things in a list: packages, pages, messages ("40 pages").
    Items,
    /// Steps of work with no unit of their own ("3 of 12").
    #[default]
    Steps,
}

impl Unit {
    /// `value` in this unit, as the row shows it.
    #[must_use]
    pub fn format(self, value: u64) -> String {
        match self {
            Self::Bytes => crate::format::size(value),
            Self::Files => counted(value, "jobs-files-one", "jobs-files"),
            Self::Items => counted(value, "jobs-items-one", "jobs-items"),
            Self::Steps => counted(value, "jobs-steps-one", "jobs-steps"),
        }
    }
}

/// `value` as a `one`/`other` message pair.
fn counted(value: u64, one: &str, other: &str) -> String {
    let key = if value == 1 { one } else { other };
    crate::i18n::widget_format(
        key,
        &[(
            "count",
            crate::i18n::ArgValue::from(crate::format::count(value)),
        )],
    )
}

/// Where a job has got to, as its row reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Progress {
    /// It has started and nothing is counted yet.
    #[default]
    Starting,
    /// A total is known: `done` of `total` in the job's unit.
    Measured { done: u64, total: u64 },
    /// There is no total to read: what it has seen so far.
    Seen { seen: u64 },
}

impl Progress {
    /// How far through it is, when that can be read.
    #[must_use]
    pub fn fraction(&self) -> Option<f64> {
        match self {
            Self::Measured { done, total } if *total > 0 => {
                Some((*done as f64 / *total as f64).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }
}

/// How a job ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// It finished: one sentence saying what came of it.
    Done(String),
    /// It failed, and why, in the person's words.
    Failed(String),
    /// The person stopped it.
    Stopped,
}

/// What a job is about: the app that owns it, the thing it works on, and
/// where pressing the row goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    /// The app that started it, by its identity.
    pub owner: Owner,
    /// The thing itself, as the person names it ("LibreOffice", "Home").
    pub label: String,
    /// What "Show me" opens, in the app's own vocabulary.
    pub target: Option<String>,
}

impl Subject {
    /// A subject in `owner`, about `label`.
    #[must_use]
    pub fn new(owner: Owner, label: impl Into<String>) -> Self {
        Self {
            owner,
            label: label.into(),
            target: None,
        }
    }

    /// The thing "Show me" opens.
    #[must_use]
    pub fn opens(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }
}

/// What a job is when it starts.
#[derive(Clone)]
pub struct Spec {
    title: String,
    subject: Option<Subject>,
    unit: Unit,
    phase: String,
    stoppable: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Spec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spec")
            .field("title", &self.title)
            .field("subject", &self.subject)
            .field("unit", &self.unit)
            .field("phase", &self.phase)
            .field("stoppable", &self.stoppable.is_some())
            .finish()
    }
}

impl Spec {
    /// A job whose title says what is happening, in the person's words
    /// ("Installing LibreOffice").
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subject: None,
            unit: Unit::Steps,
            phase: String::new(),
            stoppable: None,
        }
    }

    /// What it is about, and where its row goes.
    #[must_use]
    pub fn subject(mut self, subject: Subject) -> Self {
        self.subject = Some(subject);
        self
    }

    /// What its progress counts.
    #[must_use]
    pub fn unit(mut self, unit: Unit) -> Self {
        self.unit = unit;
        self
    }

    /// What it is doing when it starts ("Downloading").
    #[must_use]
    pub fn phase(mut self, phase: impl Into<String>) -> Self {
        self.phase = phase.into();
        self
    }

    /// It can be stopped, by running `stop`: the worker's own way out (abort
    /// a task, close a connection). A job without one shows no **Stop**,
    /// because a job whose stopping would not be honoured is a lie.
    #[must_use]
    pub fn stoppable(mut self, stop: impl Fn() + Send + Sync + 'static) -> Self {
        self.stoppable = Some(Arc::new(stop));
        self
    }
}

/// One job, as a surface reads it.
#[derive(Clone, Debug)]
pub struct Job {
    id: JobId,
    title: String,
    subject: Subject,
    unit: Unit,
    phase: String,
    progress: Progress,
    outcome: Option<Outcome>,
    started: Instant,
    ended: Option<Instant>,
    stopping: bool,
    stoppable: bool,
}

impl Job {
    /// Its identity.
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    /// What is happening, in the person's words.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// What it is about.
    #[must_use]
    pub const fn subject(&self) -> &Subject {
        &self.subject
    }

    /// What its progress counts.
    #[must_use]
    pub const fn unit(&self) -> Unit {
        self.unit
    }

    /// What it is doing now ("Downloading").
    #[must_use]
    pub fn phase(&self) -> &str {
        &self.phase
    }

    /// Where it has got to.
    #[must_use]
    pub const fn progress(&self) -> &Progress {
        &self.progress
    }

    /// How it ended; `None` while it runs.
    #[must_use]
    pub const fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// When it started.
    #[must_use]
    pub const fn started_at(&self) -> Instant {
        self.started
    }

    /// When it ended; `None` while it runs.
    #[must_use]
    pub const fn ended_at(&self) -> Option<Instant> {
        self.ended
    }

    /// Whether it is still going.
    #[must_use]
    pub const fn is_running(&self) -> bool {
        self.outcome.is_none()
    }

    /// Whether pressing **Stop** would do anything.
    #[must_use]
    pub const fn can_stop(&self) -> bool {
        self.is_running() && self.stoppable && !self.stopping
    }

    /// Whether the person already asked it to stop and it has not ended yet.
    #[must_use]
    pub const fn is_stopping(&self) -> bool {
        self.stopping
    }

    /// Its progress as a percentage, when that can be read ("68%").
    #[must_use]
    pub fn percent(&self) -> Option<String> {
        let fraction = self.progress.fraction()?;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a clamped fraction is a whole percentage point"
        )]
        let percent = (fraction * 100.0).round() as u32;
        Some(crate::i18n::widget_format(
            "jobs-percent",
            &[("percent", crate::i18n::ArgValue::from(percent.to_string()))],
        ))
    }

    /// What it is doing and how far, as its row's secondary line reads it:
    /// "Downloading 312 MB of 480 MB", "Reading 12,480 files", "Starting".
    #[must_use]
    pub fn live_line(&self) -> String {
        let phase = self.phase.trim();
        match (&self.progress, phase.is_empty()) {
            (_, true) => crate::i18n::widget_format("jobs-starting", &[]),
            (Progress::Starting, false) => phase.to_owned(),
            (Progress::Measured { done, total }, false) => crate::i18n::widget_format(
                "jobs-phase-of",
                &[
                    ("phase", crate::i18n::ArgValue::from(phase.to_owned())),
                    ("done", crate::i18n::ArgValue::from(self.unit.format(*done))),
                    (
                        "total",
                        crate::i18n::ArgValue::from(self.unit.format(*total)),
                    ),
                ],
            ),
            (Progress::Seen { seen }, false) => crate::i18n::widget_format(
                "jobs-phase-seen",
                &[
                    ("phase", crate::i18n::ArgValue::from(phase.to_owned())),
                    ("seen", crate::i18n::ArgValue::from(self.unit.format(*seen))),
                ],
            ),
        }
    }

    /// One sentence on how it ended, as its row reads it.
    #[must_use]
    pub fn outcome_line(&self) -> Option<String> {
        match self.outcome.as_ref()? {
            Outcome::Done(summary) => Some(summary.clone()),
            Outcome::Failed(reason) => Some(crate::i18n::widget_format(
                "jobs-failed",
                &[("reason", crate::i18n::ArgValue::from(reason.clone()))],
            )),
            Outcome::Stopped => Some(crate::i18n::widget_format("jobs-stopped", &[])),
        }
    }

    /// What the row's end says about the job: its progress while it runs, or
    /// how it ended.
    #[must_use]
    pub fn end_line(&self) -> String {
        self.outcome_line().unwrap_or_else(|| self.live_line())
    }
}

/// What the surface says about the whole registry at a glance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Jobs running now.
    pub running: usize,
    /// Finished jobs nobody has read yet.
    pub unread: usize,
    /// Of those, the ones that failed.
    pub failed: usize,
}

impl Summary {
    /// Whether there is anything to show.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.running == 0 && self.unread == 0
    }
}

/// The handle a worker keeps: what it reports through, and how it hears that
/// the person stopped it. Clones report on the same job.
#[derive(Clone)]
pub struct Running {
    id: JobId,
    stopped: Arc<AtomicBool>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("id", &self.id)
            .field("stopped", &self.stopped.load(Ordering::Relaxed))
            .finish()
    }
}

impl Running {
    /// Its identity.
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    /// Whether the person stopped it: a worker checks this between units of
    /// work and ends the job with [`Self::stopped`].
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }

    /// Say what it is doing now ("Verifying").
    pub fn phase(&self, phase: impl Into<String>) {
        update(self.id, |entry| entry.phase = phase.into());
    }

    /// Report a total and how much of it is done.
    pub fn progress(&self, done: u64, total: u64) {
        update(self.id, |entry| {
            entry.progress = Progress::Measured { done, total };
        });
    }

    /// Report what it has seen, where there is no total to read.
    pub fn seen(&self, seen: u64) {
        update(self.id, |entry| {
            entry.progress = Progress::Seen { seen };
        });
    }

    /// End it: what came of it, in one sentence ("Installed LibreOffice").
    pub fn done(&self, summary: impl Into<String>) {
        finish(self.id, Outcome::Done(summary.into()));
    }

    /// End it: it failed, and this is why, in the person's words.
    pub fn failed(&self, reason: impl Into<String>) {
        finish(self.id, Outcome::Failed(reason.into()));
    }

    /// End it: the person stopped it.
    pub fn stopped(&self) {
        finish(self.id, Outcome::Stopped);
    }
}

/// An entry in the registry.
struct Entry {
    job: Job,
    stop: Arc<AtomicBool>,
    canceller: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Default)]
struct Registry {
    next_job: u64,
    next_watch: u64,
    entries: Vec<Entry>,
    revision: u64,
    watchers: Vec<(u64, WakeHandle)>,
}

impl Registry {
    fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    fn wakes(&self) -> Vec<WakeHandle> {
        self.watchers.iter().map(|(_, wake)| wake.clone()).collect()
    }

    /// Drop the oldest finished rows past the cap.
    fn trim(&mut self) {
        while self.entries.len() > MAX_ROWS {
            let Some(index) = self
                .entries
                .iter()
                .position(|entry| !entry.job.is_running())
            else {
                break;
            };
            let _ = self.entries.remove(index);
        }
    }

    fn job(&self, id: JobId) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.job.id == id)
    }

    fn job_mut(&mut self, id: JobId) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|entry| entry.job.id == id)
    }
}

fn registry() -> MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock_recover()
}

/// Change `id`'s record and wake whoever watches, unless it has ended.
fn update(id: JobId, change: impl FnOnce(&mut Job)) {
    let wakes = {
        let mut registry = registry();
        let Some(entry) = registry.job_mut(id) else {
            return;
        };
        if !entry.job.is_running() {
            return;
        }
        change(&mut entry.job);
        registry.changed();
        registry.wakes()
    };
    for wake in wakes {
        wake.wake();
    }
}

/// End `id` with `outcome`, once; a job already ended keeps its outcome.
fn finish(id: JobId, outcome: Outcome) {
    let wakes = {
        let mut registry = registry();
        let Some(entry) = registry.job_mut(id) else {
            return;
        };
        if !entry.job.is_running() {
            return;
        }
        entry.job.outcome = Some(outcome.clone());
        entry.job.ended = Some(now());
        entry.job.stopping = false;
        registry.changed();
        registry.trim();
        registry.wakes()
    };
    // A failure is something a person has to decide about, so it waits in
    // Needs you as well as staying in the list. Nothing is raised while the
    // registry is locked.
    if let Outcome::Failed(reason) = outcome {
        let subject = registry()
            .job(id)
            .map(|entry| (entry.job.subject.clone(), entry.job.title.clone()));
        if let Some((subject, title)) = subject {
            crate::attention::publish(
                crate::attention::Notice::needs(
                    subject.owner.clone(),
                    title,
                    crate::attention::Action::new(
                        crate::attention::SHOW_JOB,
                        crate::i18n::widget_format("jobs-show-me", &[]),
                    ),
                )
                .about(subject.label)
                .because(reason)
                .subject(crate::attention::Target::Job(id.number())),
            );
        }
    }
    for wake in wakes {
        wake.wake();
    }
}

/// Start a job described by `spec`; the returned handle is the worker's.
///
/// Starting is cheap and never blocks: the caller's own worker does the
/// work, this only puts the job where the whole system can see it.
pub fn start(spec: Spec) -> Running {
    let stop = Arc::new(AtomicBool::new(false));
    let (id, wakes) = {
        let mut registry = registry();
        let id = JobId(registry.next_job);
        registry.next_job = registry.next_job.wrapping_add(1);
        let title = spec.title.clone();
        let subject = spec.subject.clone().unwrap_or_else(|| {
            Subject::new(
                Owner::new(crate::attention::UNKNOWN_APP, title.clone()),
                title.clone(),
            )
        });
        registry.entries.push(Entry {
            job: Job {
                id,
                title: spec.title,
                subject,
                unit: spec.unit,
                phase: spec.phase,
                progress: Progress::Starting,
                outcome: None,
                started: now(),
                ended: None,
                stopping: false,
                stoppable: spec.stoppable.is_some(),
            },
            stop: Arc::clone(&stop),
            canceller: spec.stoppable,
        });
        registry.trim();
        registry.changed();
        (id, registry.wakes())
    };
    for wake in wakes {
        wake.wake();
    }
    Running { id, stopped: stop }
}

/// Ask the job `id` to stop. It ends as [`Outcome::Stopped`] when its worker
/// notices; a job that cannot be stopped is left alone. Whether anything was
/// asked of it.
pub fn stop(id: JobId) -> bool {
    let (canceller, wakes) = {
        let mut registry = registry();
        let Some(entry) = registry.job_mut(id) else {
            return false;
        };
        if entry.job.outcome.is_some() || entry.canceller.is_none() || entry.job.stopping {
            return false;
        }
        entry.job.stopping = true;
        entry.stop.store(true, Ordering::Release);
        let canceller = entry.canceller.clone();
        registry.changed();
        (canceller, registry.wakes())
    };
    for wake in wakes {
        wake.wake();
    }
    // The worker's own way out, run outside the lock: it may abort a task
    // whose cancellation path looks at the registry.
    if let Some(canceller) = canceller {
        canceller();
    }
    true
}

/// The jobs running now, oldest first.
#[must_use]
pub fn running() -> Vec<Job> {
    registry()
        .entries
        .iter()
        .filter(|entry| entry.job.is_running())
        .map(|entry| entry.job.clone())
        .collect()
}

/// The jobs that ended and have not been read, newest first.
#[must_use]
pub fn finished() -> Vec<Job> {
    let registry = registry();
    let mut rows: Vec<Job> = registry
        .entries
        .iter()
        .filter(|entry| !entry.job.is_running())
        .map(|entry| entry.job.clone())
        .collect();
    rows.reverse();
    rows
}

/// Every row the surface shows: what runs, then what ended unread.
#[must_use]
pub fn rows() -> Vec<Job> {
    let mut rows = running();
    rows.extend(finished());
    rows
}

/// What the whole registry holds, at a glance.
#[must_use]
pub fn summary() -> Summary {
    let registry = registry();
    let mut summary = Summary::default();
    for entry in &registry.entries {
        if entry.job.is_running() {
            summary.running = summary.running.saturating_add(1);
        } else {
            summary.unread = summary.unread.saturating_add(1);
            if matches!(entry.job.outcome, Some(Outcome::Failed(_))) {
                summary.failed = summary.failed.saturating_add(1);
            }
        }
    }
    summary
}

/// Whether anything is running.
#[must_use]
pub fn is_active() -> bool {
    summary().running > 0
}

/// The subject of job `id`: what "Show me" opens.
#[must_use]
pub fn show(id: JobId) -> Option<Subject> {
    registry().job(id).map(|entry| entry.job.subject.clone())
}

/// Read a finished job's row: it leaves the surface, and so does the
/// attention item a failure raised. Running jobs are not dismissed.
pub fn dismiss(id: JobId) {
    let wakes = {
        let mut registry = registry();
        let Some(index) = registry
            .entries
            .iter()
            .position(|entry| entry.job.id == id && !entry.job.is_running())
        else {
            return;
        };
        let _ = registry.entries.remove(index);
        registry.changed();
        registry.wakes()
    };
    crate::attention::dismissed(crate::attention::Target::Job(id.number()));
    for wake in wakes {
        wake.wake();
    }
}

/// Bumped by every change, so a surface can skip rebuilding rows that did
/// not change.
#[must_use]
pub fn revision() -> u64 {
    registry().revision
}

pub(crate) fn watch(wake: WakeHandle) -> crate::jobs::Watch {
    let mut registry = registry();
    let id = registry.next_watch;
    registry.next_watch = registry.next_watch.wrapping_add(1);
    registry.watchers.push((id, wake));
    crate::jobs::Watch::new(id)
}

pub(crate) fn unwatch(id: u64) {
    let mut registry = registry();
    registry.watchers.retain(|(watcher, _)| *watcher != id);
}

/// Take every job and attention item away: a test's clean slate, and what a
/// session reset does.
pub fn clear() {
    let mut registry = registry();
    registry.entries.clear();
    registry.changed();
    drop(registry);
    crate::attention::clear();
}
