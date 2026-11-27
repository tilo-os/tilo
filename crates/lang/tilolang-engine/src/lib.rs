//! The work this device does for its apps: what `engine.call`, `engine.start` and
//! `engine.cancel` are answered with.
//!
//! An app runs sandboxed and can decode nothing, paint nothing and raster nothing itself, so the
//! device does it: a video cut rendered, a page painted, a drawing rasterised. A crate that owns
//! such an engine registers it for its app at its start ([`register`]), and the app asks by name.
//! Every name is the registering app's: an app asks its own ops and no other's, so one app can
//! never borrow another's engine.
//!
//! An op answers now ([`Provider::call`]) or runs as a job ([`Provider::start`]). Each job op is
//! declared in the engine's registration ([`Provider::jobs`]) with what it needs beyond the
//! engine's code ([`Needs`]): a released secret, the app's network, or what the app's own process
//! keeps. That decides where it runs. Work that needs none of them runs in the device's one
//! engine host, which the daemon owns and which runs every app's jobs under each call's grants; the
//! rest runs in the app's own process. A job tells every move it makes to the [`JobSink`] it was
//! started with, which the device records at `devices/<device>/jobs/<job>`; nobody asks an engine
//! how a job stands. A job whose work is a moving picture is a stream ([`Provider::stream`]): its
//! frames reach the app's window as they arrive, and it stays in the app's process. Answers are
//! JSON; the app reads them into its own records.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tilo_clock::time::{Duration, Instant};

/// One picture a running stream holds for its app: the stream's slot, the frame's number in that
/// stream, and its pixels — `width`×`height` RGBA8, straight alpha.
#[derive(Clone, Debug)]
pub struct Frame {
    pub slot: String,
    pub generation: u64,
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
}

/// What an engine hands its app's window as a stream runs: each frame, as it arrives, and what
/// the stream says of itself. The host keeps the newest of each and wakes the window, which
/// paints the frame and reads the state on its next draw; nothing here is polled.
pub trait Push: Send + Sync {
    /// The stream `frame.slot` shows `frame` now.
    fn frame(&self, frame: Frame);

    /// The stream `slot` says `state` of itself now, as JSON a view reads with `engine.state`:
    /// that it is connecting, what it shows once it runs, or why it ended. The host keeps the
    /// newest and wakes the window; a host that reads no stream state keeps nothing.
    fn state(&self, slot: &str, state: &str) {
        let _ = (slot, state);
    }

    /// The job `slot` ended, saying `state` of itself last: what it made, or why it stopped.
    fn ended(&self, slot: &str, state: &str) {
        self.state(slot, state);
    }
}

/// What a job op needs beyond the engine's code, as its engine declares it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Needs {
    /// A secret the vault released to the app (a database login, an API key): it runs in a
    /// process of the app's own, never one several apps share.
    pub secret: bool,
    /// The network: it reaches out under the app's own grant alone, so it runs in the app's
    /// process, whose egress is its package's.
    pub network: bool,
    /// What the app's own process keeps: the app's content store it writes into, or answers the
    /// app's calls read back from the engine.
    pub app_state: bool,
}

impl Needs {
    /// Nothing beyond the engine's code: work the device's shared engine host runs.
    pub const NOTHING: Self = Self {
        secret: false,
        network: false,
        app_state: false,
    };
    /// A released secret.
    pub const SECRET: Self = Self {
        secret: true,
        network: false,
        app_state: false,
    };
    /// The app's network.
    pub const NETWORK: Self = Self {
        secret: false,
        network: true,
        app_state: false,
    };
    /// A released secret, and the app's network.
    pub const SECRET_AND_NETWORK: Self = Self {
        secret: true,
        network: true,
        app_state: false,
    };
    /// What the app's own process keeps.
    pub const APP_STATE: Self = Self {
        secret: false,
        network: false,
        app_state: true,
    };
    /// The app's network, and what its own process keeps.
    pub const NETWORK_AND_APP_STATE: Self = Self {
        secret: false,
        network: true,
        app_state: true,
    };

    /// Where work with these needs runs.
    #[must_use]
    pub const fn place(self) -> Place {
        if self.secret || self.network || self.app_state {
            Place::App
        } else {
            Place::Shared
        }
    }
}

/// Where a job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// The device's one engine host, which the daemon owns and every app's jobs share.
    Shared,
    /// The app's own sandboxed process.
    App,
}

/// One job op an engine runs, as its registration declares it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobOp {
    pub op: &'static str,
    pub needs: Needs,
}

impl JobOp {
    /// An op that needs `needs`.
    #[must_use]
    pub const fn new(op: &'static str, needs: Needs) -> Self {
        Self { op, needs }
    }
}

/// How a job stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl State {
    /// The word a job's record says.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// The state a record's word names.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Some(match word {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    /// Whether the job has ended: nothing moves after this.
    #[must_use]
    pub const fn ended(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// The least time between two moves of a job that has not ended: at most four a second.
pub const BETWEEN_MOVES: Duration = Duration::from_millis(250);

/// What is told of each move of a job: its state and the engine's own fields.
type Tell = dyn Fn(State, serde_json::Value) + Send + Sync;

/// Where a job tells each move it makes. Moves of a running job come at most four times a second
/// ([`BETWEEN_MOVES`]): one that comes sooner waits, and a later one takes its place; the last
/// still reaches the record once the interval has passed. A job's end is told at once, always,
/// and nothing after it.
#[derive(Clone)]
pub struct JobSink(Arc<Sink>);

struct Sink {
    tell: Box<Tell>,
    held: Mutex<Held>,
}

#[derive(Default)]
struct Held {
    /// When a move was told last.
    told: Option<Instant>,
    /// The newest move that came too soon, waiting for its turn.
    waiting: Option<(State, serde_json::Value)>,
    /// Whether a move is set to be told once the interval passes.
    armed: bool,
    /// Whether the job ended: its end was told.
    ended: bool,
}

impl JobSink {
    /// A sink telling each move to `tell`.
    pub fn new(tell: impl Fn(State, serde_json::Value) + Send + Sync + 'static) -> Self {
        Self(Arc::new(Sink {
            tell: Box::new(tell),
            held: Mutex::new(Held::default()),
        }))
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.0
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The job moved: it now stands at `state`, with the engine's own fields `detail` (an object).
    pub fn moved(&self, state: State, detail: serde_json::Value) {
        let mut held = self.held();
        if held.ended {
            return;
        }
        let now = tilo_clock::now();
        if state.ended() {
            held.ended = true;
            held.waiting = None;
            held.told = Some(now);
            drop(held);
            (self.0.tell)(state, detail);
            return;
        }
        let due = held
            .told
            .and_then(|told| told.checked_add(BETWEEN_MOVES))
            .unwrap_or(now);
        if due <= now && !held.armed {
            held.told = Some(now);
            drop(held);
            (self.0.tell)(state, detail);
            return;
        }
        held.waiting = Some((state, detail));
        if held.armed {
            return;
        }
        held.armed = true;
        drop(held);
        let sink = self.clone();
        let wait = due.saturating_duration_since(now);
        let spawned = std::thread::Builder::new()
            .name("job-moves".into())
            .spawn(move || {
                std::thread::sleep(wait);
                sink.tell_waiting();
            });
        if spawned.is_err() {
            // With no thread to tell it later, the move is told now rather than lost.
            self.tell_waiting();
        }
    }

    /// The job failed with `why`.
    pub fn failed(&self, why: &str) {
        self.moved(State::Failed, serde_json::json!({ "why": why }));
    }

    /// Whether the job's end was told.
    #[must_use]
    pub fn has_ended(&self) -> bool {
        self.held().ended
    }

    fn tell_waiting(&self) {
        let mut held = self.held();
        held.armed = false;
        let Some((state, detail)) = held.waiting.take() else {
            return;
        };
        if held.ended {
            return;
        }
        held.told = Some(tilo_clock::now());
        drop(held);
        (self.0.tell)(state, detail);
    }
}

/// One app's engine, as the crate that owns it answers the device's asks.
pub trait Provider {
    /// The ops this engine runs as jobs, each with what it needs: where each runs is decided from
    /// this, and an op not named here starts nowhere.
    fn jobs(&self) -> &'static [JobOp] {
        &[]
    }

    /// One request answered now. JSON on success, or the words of a refusal.
    ///
    /// # Errors
    ///
    /// An input the engine does not accept, or work it cannot do.
    fn call(&self, op: &str, input: &str) -> Result<String, String>;

    /// One request started as the job `job`, which tells each move it makes to `moved`, its end
    /// last. `job` is what [`Self::cancel`] is asked with.
    ///
    /// # Errors
    ///
    /// As [`Self::call`], for a request that cannot start.
    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        let _ = (job, input, moved);
        Err(format!("`{op}` is no work this device starts"))
    }

    /// One stream started: a job that hands `push` its newest frame as each arrives, the app
    /// showing it by `tilo-live://<job>`. The engine wakes the app's window after each frame.
    ///
    /// # Errors
    ///
    /// As [`Self::call`], for a stream that cannot start, or an engine that runs none.
    fn stream(&self, op: &str, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
        let _ = (op, input, push);
        Err("this engine runs no streams".to_owned())
    }

    /// Stop the job `job`: false when it is not this engine's or it had finished. A job stopped
    /// tells its end (`cancelled`) to its sink.
    fn cancel(&self, job: &str) -> bool {
        let _ = job;
        false
    }
}

/// A provider that answers nothing: what an app's engine stands as until its crate registers one.
struct Unregistered;

impl Provider for Unregistered {
    fn call(&self, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("`{op}` is no op this device runs"))
    }

    fn stream(&self, op: &str, _input: &str, _push: Arc<dyn Push>) -> Result<String, String> {
        Err(format!("`{op}` is no stream this device runs"))
    }
}

/// The engines registered so far, by the app that owns each: one registry for the process, which
/// its app host and the crates that register engines both reach.
fn engines() -> &'static Mutex<BTreeMap<String, Arc<dyn Provider + Send + Sync>>> {
    static ENGINES: OnceLock<Mutex<BTreeMap<String, Arc<dyn Provider + Send + Sync>>>> =
        OnceLock::new();
    ENGINES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The registry, held: a lock poisoned by a panic in one engine still registers and answers
/// the rest, since what it guards is a map of providers and no half-written value.
fn held() -> MutexGuard<'static, BTreeMap<String, Arc<dyn Provider + Send + Sync>>> {
    engines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Register `provider` as the engine of `app`: the ops the app names are answered by it from
/// then on. A crate calls this once, at its start, for the app whose engine it owns.
pub fn register(app: &str, provider: Arc<dyn Provider + Send + Sync>) {
    held().insert(app.to_owned(), provider);
}

/// The engine of `app`, or one that refuses every ask: an app whose engine has not been
/// registered yet is refused, never answered by another app's.
///
/// A device's development build asks by the id it runs under (`<app>.dev.<device key>`), and the
/// engine it needs is registered under the app it is a build of: the registry is asked for that
/// app, so a build is answered by its app's engine and by no other's.
fn engine_of(app: &str) -> Arc<dyn Provider + Send + Sync> {
    held()
        .get(tilo_app_id::build_of(app))
        .cloned()
        .unwrap_or_else(|| Arc::new(Unregistered))
}

/// One request answered now for `app`.
///
/// # Errors
///
/// The engine's refusal, or that this device runs none for `app`.
pub fn answer(app: &str, op: &str, input: &str) -> Result<String, String> {
    engine_of(app).call(op, input)
}

/// The job op `op` of `app`'s engine, as its registration declares it.
///
/// # Errors
///
/// The engine runs no such job, or this device runs no engine for `app`.
pub fn job_op(app: &str, op: &str) -> Result<JobOp, String> {
    engine_of(app)
        .jobs()
        .iter()
        .find(|declared| declared.op == op)
        .copied()
        .ok_or_else(|| format!("`{op}` is no work this device starts for {app}"))
}

/// The job `job` started for `app`, running `op` here, and telling its moves to `moved`. An op
/// the engine did not declare as a job starts nowhere.
///
/// # Errors
///
/// The engine's refusal, or that this device runs no such job for `app`.
pub fn begin(app: &str, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
    job_op(app, op)?;
    engine_of(app).start(job, op, input, moved)
}

/// One stream started for `app`: its frames reach `push` as the device receives them.
///
/// # Errors
///
/// The engine's refusal, or that this device runs none for `app`.
pub fn stream(app: &str, op: &str, input: &str, push: Arc<dyn Push>) -> Result<String, String> {
    engine_of(app).stream(op, input, push)
}

/// Stop the job `job` `app`'s engine runs: false when it runs no such job or it had finished.
pub fn stop(app: &str, job: &str) -> bool {
    engine_of(app).cancel(job)
}

/// The latest record this process heard of each job it started, by its path.
fn heard_jobs() -> &'static Mutex<BTreeMap<String, serde_json::Value>> {
    static HEARD: OnceLock<Mutex<BTreeMap<String, serde_json::Value>>> = OnceLock::new();
    HEARD.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn heard_held() -> MutexGuard<'static, BTreeMap<String, serde_json::Value>> {
    heard_jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The job `job` moved, and `record` is how it stands now (`{id, app, op, state, detail, ..}`):
/// what [`job`] answers from now.
pub fn hear(job: &str, record: serde_json::Value) {
    heard_held().insert(job.to_owned(), record);
}

/// The latest record this process heard of the job `job`; `None` for a job it never heard of.
#[must_use]
pub fn job(job: &str) -> Option<serde_json::Value> {
    heard_held().get(job).cloned()
}

/// The record of `job` whose engine says `said` of it (its state among the engine's own fields).
#[must_use]
pub fn record_of(job: &str, said: &str) -> Option<serde_json::Value> {
    let detail: serde_json::Value = serde_json::from_str(said).ok()?;
    Some(serde_json::json!({
        "id": job,
        "state": detail.get("state").cloned().unwrap_or_default(),
        "detail": detail,
    }))
}

/// The app whose engine runs the job `job` this process started.
#[must_use]
pub fn app_of(job: &str) -> Option<String> {
    heard_held()
        .get(job)
        .and_then(|record| record["app"].as_str().map(str::to_owned))
}

/// The record of the job `job` of `app` running `op`, standing at `state` with the engine's
/// fields `detail`.
#[must_use]
pub fn record(
    job: &str,
    app: &str,
    op: &str,
    state: State,
    detail: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": job,
        "app": app,
        "op": op,
        "state": state.word(),
        "detail": detail,
    })
}

/// The job `job` of `app` started in this process, running `op`: each move is heard here
/// ([`job`]) and told to `also` (the device's record of it).
///
/// # Errors
///
/// As [`begin`].
pub fn begin_here(
    app: &str,
    job: &str,
    op: &str,
    input: &str,
    also: impl Fn(State, &serde_json::Value) + Send + Sync + 'static,
) -> Result<(), String> {
    begin_here_with(app, job, op, also, |moved| {
        begin(app, job, op, input, moved)
    })
}

/// [`begin_here`], with the job started by `start` rather than by `app`'s registered engine: an
/// engine this process opened from its runtime bundle, which it asks itself.
///
/// # Errors
///
/// What `start` refused.
pub fn begin_here_with(
    app: &str,
    job: &str,
    op: &str,
    also: impl Fn(State, &serde_json::Value) + Send + Sync + 'static,
    start: impl FnOnce(JobSink) -> Result<(), String>,
) -> Result<(), String> {
    hear(
        job,
        record(job, app, op, State::Queued, &serde_json::json!({})),
    );
    let (named, owner, doing) = (job.to_owned(), app.to_owned(), op.to_owned());
    let moved = JobSink::new(move |state, detail| {
        hear(&named, record(&named, &owner, &doing, state, &detail));
        also(state, &detail);
    });
    start(moved).inspect_err(|_| {
        heard_held().remove(job);
    })
}

/// Stop the job or stream `job` this process started: false when it knows no such job or it had
/// finished. A stream is stopped by whichever engine runs it.
pub fn stop_here(job: &str) -> bool {
    if let Some(app) = app_of(job) {
        return stop(&app, job);
    }
    let providers: Vec<Arc<dyn Provider + Send + Sync>> = held().values().cloned().collect();
    providers.iter().any(|provider| provider.cancel(job))
}

/// The jobs an engine runs, by the id each was started under: what a job keeps to be stopped by,
/// and what it tells its moves to. An engine holds one beside its own work.
pub struct Running<T> {
    jobs: Mutex<BTreeMap<String, (Arc<T>, JobSink)>>,
}

impl<T> Default for Running<T> {
    fn default() -> Self {
        Self {
            jobs: Mutex::new(BTreeMap::new()),
        }
    }
}

impl<T> Running<T> {
    fn held(&self) -> MutexGuard<'_, BTreeMap<String, (Arc<T>, JobSink)>> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keep `job`, running as `work`, telling `moved`.
    pub fn keep(&self, job: &str, work: Arc<T>, moved: JobSink) {
        self.held().insert(job.to_owned(), (work, moved));
    }

    /// The job `job`, while it runs.
    pub fn get(&self, job: &str) -> Option<(Arc<T>, JobSink)> {
        self.held().get(job).cloned()
    }

    /// The job `job`, whose end was told already, no longer kept.
    pub fn forget(&self, job: &str) {
        self.held().remove(job);
    }

    /// The job `job` ended with `state` and `detail`: told, and no longer kept.
    pub fn end(&self, job: &str, state: State, detail: serde_json::Value) {
        let kept = self.held().remove(job);
        if let Some((_, moved)) = kept {
            moved.moved(state, detail);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct Cut {
        running: Running<()>,
    }

    const CUT_JOBS: &[JobOp] = &[
        JobOp::new("cut.render", Needs::NOTHING),
        JobOp::new("cut.upload", Needs::SECRET_AND_NETWORK),
    ];

    impl Provider for Cut {
        fn jobs(&self) -> &'static [JobOp] {
            CUT_JOBS
        }

        fn call(&self, op: &str, input: &str) -> Result<String, String> {
            match op {
                "cut.probe" => Ok(format!(r#"{{"input":"{input}"}}"#)),
                _ => Err(format!("`{op}` is not a cut op")),
            }
        }

        fn start(&self, job: &str, _op: &str, _input: &str, moved: JobSink) -> Result<(), String> {
            moved.moved(State::Running, serde_json::json!({}));
            self.running.keep(job, Arc::new(()), moved);
            Ok(())
        }

        fn cancel(&self, job: &str) -> bool {
            let known = self.running.get(job).is_some();
            self.running
                .end(job, State::Cancelled, serde_json::json!({}));
            known
        }
    }

    fn cut() -> Arc<Cut> {
        Arc::new(Cut {
            running: Running::default(),
        })
    }

    fn told() -> (JobSink, mpsc::Receiver<(State, serde_json::Value)>) {
        let (send, receive) = mpsc::channel();
        let send = Mutex::new(send);
        let sink = JobSink::new(move |state, detail| {
            let _ = send
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .send((state, detail));
        });
        (sink, receive)
    }

    #[test]
    fn an_app_s_ops_reach_its_own_engine() {
        register("so.tilo.cut", cut());
        assert_eq!(
            answer("so.tilo.cut", "cut.probe", "x"),
            Ok(r#"{"input":"x"}"#.to_owned())
        );
        let (moved, heard) = told();
        assert_eq!(
            begin("so.tilo.cut", "job-1", "cut.render", "{}", moved),
            Ok(())
        );
        assert_eq!(heard.recv().unwrap().0, State::Running);
        assert!(stop("so.tilo.cut", "job-1"));
        assert_eq!(heard.recv().unwrap().0, State::Cancelled);
        assert!(!stop("so.tilo.cut", "job-1"), "a job stops once");
    }

    #[test]
    fn an_engine_declares_what_each_job_needs() {
        register("so.tilo.cut", cut());
        assert_eq!(
            job_op("so.tilo.cut", "cut.render").map(|op| op.needs.place()),
            Ok(Place::Shared)
        );
        let upload = job_op("so.tilo.cut", "cut.upload").unwrap();
        assert!(upload.needs.secret && upload.needs.network);
        assert_eq!(upload.needs.place(), Place::App);
        let (moved, _heard) = told();
        assert_eq!(
            begin("so.tilo.cut", "job-2", "cut.probe", "{}", moved),
            Err("`cut.probe` is no work this device starts for so.tilo.cut".to_owned()),
            "an op the engine did not declare as a job starts nowhere"
        );
    }

    #[test]
    fn an_app_never_borrows_another_s_engine() {
        register("so.tilo.cut", cut());
        assert_eq!(
            answer("so.tilo.other", "cut.probe", "x"),
            Err("`cut.probe` is no op this device runs".to_owned())
        );
        let (moved, _heard) = told();
        assert_eq!(
            begin("so.tilo.other", "job-3", "cut.render", "{}", moved),
            Err("`cut.render` is no work this device starts for so.tilo.other".to_owned())
        );
        assert!(!stop("so.tilo.other", "job-1"));
    }

    #[test]
    fn an_op_the_engine_does_not_know_is_refused_in_its_words() {
        register("so.tilo.cut", cut());
        assert_eq!(
            answer("so.tilo.cut", "cut.paint", ""),
            Err("`cut.paint` is not a cut op".to_owned())
        );
    }

    /// A running job's moves reach its record at most four times a second, the newest of a burst
    /// last; its end is told at once, and nothing after it.
    #[test]
    fn moves_are_told_four_times_a_second_and_the_end_always() {
        let (moved, heard) = told();
        for done in 0..100 {
            moved.moved(State::Running, serde_json::json!({ "done": done }));
        }
        assert_eq!(
            heard.recv().unwrap(),
            (State::Running, serde_json::json!({ "done": 0 })),
            "the first move is told at once"
        );
        assert!(
            heard.try_recv().is_err(),
            "the rest of the burst waits for its turn"
        );
        let (state, detail) = heard.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            (state, detail),
            (State::Running, serde_json::json!({ "done": 99 })),
            "the newest of the burst is told once the interval passed"
        );
        moved.moved(State::Running, serde_json::json!({ "done": 100 }));
        moved.moved(State::Done, serde_json::json!({ "done": 101 }));
        assert_eq!(
            heard.recv().unwrap(),
            (State::Done, serde_json::json!({ "done": 101 })),
            "the end is told at once, in place of the move that waited"
        );
        moved.moved(State::Running, serde_json::json!({ "done": 102 }));
        assert!(
            heard.recv_timeout(BETWEEN_MOVES * 2).is_err(),
            "nothing moves after the end"
        );
        assert!(moved.has_ended());
    }

    struct Demo;

    const DEMO_JOBS: &[JobOp] = &[JobOp::new("demo.scan", Needs::NOTHING)];

    impl Provider for Demo {
        fn jobs(&self) -> &'static [JobOp] {
            DEMO_JOBS
        }

        fn call(&self, op: &str, input: &str) -> Result<String, String> {
            match op {
                "demo.probe" => Ok(format!(r#"{{"input":"{input}"}}"#)),
                _ => Err(format!("`{op}` is not a demo op")),
            }
        }
    }

    #[test]
    fn a_development_build_is_answered_by_the_app_it_is_a_build_of() {
        register("so.tilo.demo", Arc::new(Demo));
        let dev = "so.tilo.demo.dev.0123456789abcdef";
        assert_eq!(
            answer(dev, "demo.probe", "hi").unwrap(),
            r#"{"input":"hi"}"#,
            "a build asks its app's engine by the id it runs under"
        );
        assert_eq!(
            job_op(dev, "demo.scan").unwrap().op,
            "demo.scan",
            "and starts its jobs the same way"
        );
        assert!(
            answer("so.tilo.somebody-else", "demo.probe", "").is_err(),
            "the build is answered by its app's engine and no other's"
        );
    }
}
