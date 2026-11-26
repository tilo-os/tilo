//! The engines this process answers its apps with, and where each of their jobs runs.
//!
//! Engine work is the device's: a job an app starts (`engine.start`) is named by its record,
//! `devices/<device>/jobs/<job>`, and tells every move it makes to the device, which keeps that
//! record ([`FromProcess::Job`]). Where it runs is decided from what its op declared it needs
//! ([`tilolang_engine::Needs`]):
//!
//! - work that needs nothing of the app's own goes to the device, which runs it in its one engine
//!   host, admitted as the call that asked ([`FromProcess::EngineStart`]). The host is a process
//!   like this one, started alone, which runs every app's shared jobs ([`run`]) and holds no
//!   authority of its own: no network, no secret, no app's content. The app's process may end
//!   while the job runs; the job finishes, and its record says so;
//! - work that needs a released secret, the app's network or what the app's process keeps runs
//!   here, in the app's own sandboxed process, which the device holds while it runs.
//!
//! The views reading a job read its record through the record API, as every reader does
//! ([`crate::invocations::Invocations::follow_job`]), and the device runs what the app asked to
//! run as it moves and once it ends (`progress:`, `done:`), which the job's record carries.
//!
//! A process the device did not start on a link (an app's window, a test), or whose device keeps
//! no record of jobs, has no device to hand work to: it runs its jobs itself, and its views read
//! them here ([`engine::job`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use runtime_portal::process_link::{Descriptor, EngineJob, FromProcess, JobNeeds, Packet as _};
use serde_json::json;
use tilolang_engine::{self as engine, JobOp, JobSink, Needs, Place, Provider, State};
use tilolang_tilo::background::Waker;

/// Register every engine this process links, once, before any app asks.
pub fn register_all() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        tilo_disk_usage::register();
        publisher_engine::register();
        engine::register(API_TESTER_APP, Arc::new(ApiTester));
        // The on-screen keyboard's work, which it asks by name wherever its view runs: a host
        // that runs the app alone answers it as the session does.
        tilo_keyboard_engine::register();
        #[cfg(feature = "app-launch")]
        {
            crate::db_engine::register();
            crate::containers_engine::register();
            crate::files_engine::register();
            // Maps' kept areas are this device's pictures, which a window may already have drawn.
            tilo_maps_engine::register();
            // The Terminal's shells: a session's pty, its grid and the runs it marks, answered
            // for the app's `engine.call` and `engine.start` in the process that draws its window.
            tilo_terminal::ops::register(Arc::new(Portal));
        }
        #[cfg(all(
            feature = "app-launch",
            any(target_os = "linux", all(target_os = "macos", target_arch = "aarch64"))
        ))]
        {
            crate::backups_engine::register();
            crate::feed_engine::register();
        }
    });
}

/// The API Tester's app id: the one app the general network client answers requests for.
pub const API_TESTER_APP: &str = "so.tilo.api-tester";

/// A link a program printed, opened in what the person's device opens that kind with: the
/// portal the app's grants already reach, never a second opener of this process's own. A window
/// is what a program's link opens into, so this exists only where windows do.
#[cfg(feature = "app-launch")]
struct Portal;

#[cfg(feature = "app-launch")]
impl tilo_terminal::ops::Opener for Portal {
    fn open_uri(&self, url: &str) -> Result<(), String> {
        tilo_dialogs::portal::open_uri(url).map_err(|error| error.to_string())
    }
}

/// The API Tester's engine: requests sent by the general network client, with the credentials
/// this device keeps for the app.
struct ApiTester;

impl Provider for ApiTester {
    fn jobs(&self) -> &'static [JobOp] {
        // A request carries the app's kept credentials to a host the app's grant names.
        const JOBS: &[JobOp] = &[JobOp::new("send", Needs::SECRET_AND_NETWORK)];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        tilo_network_client::call(API_TESTER_APP, op, input)
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        tilo_network_client::start(API_TESTER_APP, job, op, input, told(moved))
    }

    fn cancel(&self, job: &str) -> bool {
        tilo_network_client::cancel(job)
    }
}

/// How an engine crate that names no job sink tells a job's moves: the state's word and its own
/// fields.
pub type Told = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

/// `moved`, as an engine crate that names no job sink tells its moves: the state's word and its
/// own fields.
pub fn told(moved: JobSink) -> Told {
    Arc::new(move |word, detail| {
        moved.moved(State::from_word(word).unwrap_or(State::Running), detail);
    })
}

/// This process's link to its device, once the device started it on one.
struct Device {
    link: Arc<Descriptor>,
    /// Where the device keeps its jobs' records, once it said so.
    under: Mutex<Option<String>>,
    /// What wakes the process's loop: a job that ended may be the last thing it waited for.
    wake: Waker,
}

fn device() -> Option<&'static Device> {
    static DEVICE: OnceLock<Device> = OnceLock::new();
    LINKED.get().map(|(link, wake)| {
        DEVICE.get_or_init(|| Device {
            link: Arc::clone(link),
            under: Mutex::new(None),
            wake: Arc::clone(wake),
        })
    })
}

static LINKED: OnceLock<(Arc<Descriptor>, Waker)> = OnceLock::new();

fn held<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// This process was started by its device on `link`: its jobs are the device's from now on, and
/// `wake` wakes its loop when one ends.
pub fn attach(link: Arc<Descriptor>, wake: Waker) {
    let _ = LINKED.set((link, wake));
}

/// The device keeps its jobs' records under `under` (`devices/<device>/jobs`).
pub fn jobs_under(under: String) {
    if let Some(device) = device() {
        *held(&device.under) = Some(under);
    }
}

/// The jobs this process handed the device's engine host, by the app each is for: what a cancel
/// goes to the device for.
fn handed() -> MutexGuard<'static, BTreeMap<String, String>> {
    static HANDED: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    held(HANDED.get_or_init(|| Mutex::new(BTreeMap::new())))
}

/// The jobs running in this process that the device holds it for, by the app each is for.
fn here() -> MutexGuard<'static, BTreeMap<String, String>> {
    static HERE: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    held(HERE.get_or_init(|| Mutex::new(BTreeMap::new())))
}

/// Whether a job the device holds this process for still runs here: the process ends only once
/// none does.
#[must_use]
pub fn busy() -> bool {
    !here().is_empty()
}

/// Whether this process hands its jobs to a device that records them: their views read them
/// through the record API then, and here otherwise.
#[must_use]
pub fn recorded() -> bool {
    device().is_some_and(|device| held(&device.under).is_some())
}

/// The records of the jobs this process's instances read through the record API, by path, as
/// last read: what an action reads of a job, whichever window started it.
fn read_records() -> MutexGuard<'static, BTreeMap<String, serde_json::Value>> {
    static READ: OnceLock<Mutex<BTreeMap<String, serde_json::Value>>> = OnceLock::new();
    held(READ.get_or_init(|| Mutex::new(BTreeMap::new())))
}

/// The record of job `path` was read through the record API as `record`.
pub fn read_record(path: &str, record: &serde_json::Value) {
    read_records().insert(path.to_owned(), record.clone());
}

/// The job `path` as an action reads it: its record as last read from the device's records, or,
/// for a job this process runs with no device, as it stands here.
#[must_use]
pub fn job(path: &str) -> Option<serde_json::Value> {
    if recorded() {
        return read_records().get(path).cloned();
    }
    engine::job(path)
}

/// Who a job is started for: the app whose engine runs it and the call that asked.
pub struct Caller<'a> {
    pub app: &'a str,
    pub workspace: &'a str,
    /// The invocation the call runs under; empty for a call made under none.
    pub invocation: &'a str,
    /// The member it acts for.
    pub by: &'a str,
    /// What the app asked to run as the job moves and once it ends, as its runtime wrote it:
    /// carried to the device with the job.
    pub handlers: &'a serde_json::Value,
}

/// `engine.start` for `caller`: the job `op` of its app's engine started where it runs, answered
/// with the job's path, `devices/<device>/jobs/<job>`.
///
/// # Errors
///
/// The app's engine runs no such job, the device could not be told, or the engine refused it.
pub fn start(caller: &Caller<'_>, op: &str, input: &str) -> Result<String, String> {
    let declared = engine::job_op(caller.app, op)?;
    start_placed(caller, op, input, declared.needs, |job, moved| {
        engine::begin(caller.app, job, op, input, moved)
    })
}

/// `engine.start` for `caller` of an engine this process opened from its runtime bundle: the job
/// runs here, where the engine reads the app's own content through this process, and its record
/// is the device's as every job's is. `start` starts it under the job's id, telling its moves to
/// the sink it is handed.
///
/// # Errors
///
/// The device could not be told, or the engine refused the job.
pub fn start_loaded(
    caller: &Caller<'_>,
    op: &str,
    input: &str,
    start: impl FnOnce(&str, JobSink) -> Result<(), String>,
) -> Result<String, String> {
    start_placed(caller, op, input, Needs::APP_STATE, start)
}

/// The job `op` of `caller`'s app, needing `needs`, started where that lets it run; `start`
/// starts it when that is here.
fn start_placed(
    caller: &Caller<'_>,
    op: &str,
    input: &str,
    needs: Needs,
    start: impl FnOnce(&str, JobSink) -> Result<(), String>,
) -> Result<String, String> {
    let Some((device, under)) =
        device().and_then(|device| Some((device, held(&device.under).clone()?)))
    else {
        // No device started this process, or it keeps no record of jobs: the process runs the
        // work itself, and its views read the job here.
        let job = format!("job-{}", uuid::Uuid::new_v4().simple());
        let wake = LINKED.get().map(|(_, wake)| Arc::clone(wake));
        engine::begin_here_with(
            caller.app,
            &job,
            op,
            move |_, _| {
                if let Some(wake) = &wake {
                    wake();
                }
            },
            |moved| start(&job, moved),
        )?;
        return Ok(job);
    };
    let place = needs.place();
    let job = EngineJob {
        job: format!("{under}/{}", uuid::Uuid::new_v4().simple()),
        app: caller.app.to_owned(),
        workspace: caller.workspace.to_owned(),
        invocation: caller.invocation.to_owned(),
        by: caller.by.to_owned(),
        op: op.to_owned(),
        input: input.to_owned(),
        needs: needs_of(needs),
        here: place == Place::App,
        handlers: caller.handlers.clone(),
    };
    let answer = job.job.clone();
    let named = job.job.clone();
    // Every job is the device's: it is told of each before it runs, so its record begins there.
    match place {
        Place::Shared => {
            handed().insert(named.clone(), job.app.clone());
        }
        Place::App => {
            here().insert(named.clone(), job.app.clone());
        }
    }
    let ran = job.clone();
    if let Err(error) = FromProcess::EngineStart(Box::new(job)).send(&device.link) {
        handed().remove(&named);
        here().remove(&named);
        return Err(format!("the device was not asked to run the work: {error}"));
    }
    if place == Place::App {
        let (link, wake) = (Arc::clone(&device.link), Arc::clone(&device.wake));
        let refused = ran.clone();
        let started = engine::begin_here_with(
            caller.app,
            &named,
            op,
            move |state, detail| {
                report(&link, &ran, state, detail);
                if state.ended() {
                    here().remove(&ran.job);
                    wake();
                }
            },
            |moved| start(&named, moved),
        );
        if let Err(why) = started {
            // The device holds the process for the job until its record ends.
            report(
                &device.link,
                &refused,
                State::Failed,
                &json!({ "why": why }),
            );
            here().remove(&named);
            return Err(why);
        }
    }
    Ok(answer)
}

/// Work this process runs on its own for `caller` outside any engine (a project's approved
/// check), recorded by the device as the job `op` like every job: its path, and what tells the
/// device each move. `None` when no device keeps this process's jobs' records.
///
/// # Errors
///
/// The device could not be told of it.
pub fn record_here(caller: &Caller<'_>, op: &str) -> Option<Result<Recorded, String>> {
    let device = device()?;
    let under = held(&device.under).clone()?;
    let job = EngineJob {
        job: format!("{under}/{}", uuid::Uuid::new_v4().simple()),
        app: caller.app.to_owned(),
        workspace: caller.workspace.to_owned(),
        invocation: caller.invocation.to_owned(),
        by: caller.by.to_owned(),
        op: op.to_owned(),
        input: String::new(),
        needs: needs_of(Needs::APP_STATE),
        here: true,
        handlers: caller.handlers.clone(),
    };
    here().insert(job.job.clone(), job.app.clone());
    if let Err(error) = FromProcess::EngineStart(Box::new(job.clone())).send(&device.link) {
        here().remove(&job.job);
        return Some(Err(format!("the device was not told of the work: {error}")));
    }
    Some(Ok(Recorded {
        link: Arc::clone(&device.link),
        wake: Arc::clone(&device.wake),
        job,
    }))
}

/// Work this process runs on its own that the device records as one of its jobs.
pub struct Recorded {
    link: Arc<Descriptor>,
    wake: Waker,
    job: EngineJob,
}

impl Recorded {
    /// The job's path: what every window reads it by.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.job.job
    }

    /// The work moved to `state` with the fields `detail`: the device's record of it says so.
    pub fn moved(&self, state: State, detail: &serde_json::Value) {
        report(&self.link, &self.job, state, detail);
        if state.ended() {
            here().remove(&self.job.job);
            (self.wake)();
        }
    }
}

/// `engine.cancel` of the job `job` this process started: false when it knows no such job or it
/// had finished.
pub fn cancel(job: &str) -> bool {
    if handed().contains_key(job) {
        return device().is_some_and(|device| {
            FromProcess::EngineCancel {
                job: job.to_owned(),
            }
            .send(&device.link)
            .is_ok()
        });
    }
    engine::stop_here(job)
}

/// The device's engine host runs the job `job` here, telling each move to the device. Work the
/// shared host may not run (it needs a secret, the app's network or what the app's own process
/// keeps) is refused, and its record says why.
pub fn run(job: &EngineJob) {
    let Some(device) = device() else {
        tracing::error!(
            job = job.job,
            "an engine job came to a process with no device"
        );
        return;
    };
    let refused = match engine::job_op(&job.app, &job.op) {
        Err(why) => Some(why),
        Ok(declared)
            if declared.needs.place() != Place::Shared || !job.needs.shared() || job.here =>
        {
            Some(format!(
                "`{}` needs what only {}'s own process holds, so the device's shared engine host \
                 does not run it",
                job.op, job.app
            ))
        }
        Ok(_) => None,
    };
    if let Some(why) = refused {
        report(&device.link, job, State::Failed, &json!({ "why": why }));
        return;
    }
    here().insert(job.job.clone(), job.app.clone());
    let (link, wake) = (Arc::clone(&device.link), Arc::clone(&device.wake));
    let moved = JobSink::new({
        let job = job.clone();
        move |state, detail| {
            report(&link, &job, state, &detail);
            if state.ended() {
                here().remove(&job.job);
                wake();
            }
        }
    });
    if let Err(why) = engine::begin(&job.app, &job.job, &job.op, &job.input, moved.clone()) {
        moved.failed(&why);
    }
}

/// Stop the job `job` this host runs.
pub fn stop(job: &str) {
    let app = here().get(job).cloned();
    if let Some(app) = app
        && !engine::stop(&app, job)
    {
        tracing::warn!(job, "a job asked to stop had ended");
    }
}

/// What an op declared it needs, as the device reads it.
const fn needs_of(needs: Needs) -> JobNeeds {
    JobNeeds {
        secret: needs.secret,
        network: needs.network,
        app_state: needs.app_state,
    }
}

/// Tell the device the job `job` moved to `state` with the engine's fields `detail`. Fields over
/// what a record carries are left out of it: the job's process still has them.
fn report(link: &Descriptor, job: &EngineJob, state: State, detail: &serde_json::Value) {
    let record = |detail: &serde_json::Value| FromProcess::Job {
        job: job.job.clone(),
        record: json!({
            "id": job.job,
            "app": job.app,
            "workspace": job.workspace,
            "invocation": job.invocation,
            "by": job.by,
            "op": job.op,
            "state": state.word(),
            "detail": detail,
        }),
    };
    let sent = record(detail).send(link).or_else(|error| {
        tracing::warn!(job = job.job, %error, "a job's move was told without its fields");
        record(&json!({ "over": "its fields are more than a record carries" })).send(link)
    });
    if let Err(error) = sent {
        tracing::warn!(job = job.job, %error, "the device was not told a job moved");
    }
}
