//! The Git app's engine, as its package reaches it: `engine.call` and `engine.start` answered for
//! `so.tilo.git-client` by the git engine on this device.
//!
//! The app runs sandboxed and holds no repository, so it asks by name. Reading and changing a
//! repository answers now; a transfer to or from a remote starts as a job whose progress is kept
//! here, and which can be stopped.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink, Needs};

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.git-client";

/// One `engine.call` for this device's app, or a refusal.
///
/// # Errors
///
/// The op is unknown, the ask does not read, or the repository refuses it.
fn call(_device: &Device, op: &str, input: &str) -> Result<String, String> {
    tilo_git::run(op, input)
}

/// One `engine.start`: a transfer, run as the job the host names.
///
/// # Errors
///
/// An op that is no transfer, or the ask does not read.
fn start(
    _device: &Device,
    job: &str,
    op: &str,
    input: &str,
    _moved: &JobSink,
) -> Result<(), String> {
    let transfer: TransferAsk =
        serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))?;
    match op {
        "fetch" | "pull" | "push" => begin(job, op, transfer),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// How far a transfer has got, as JSON.
///
/// # Errors
///
/// No such job on this device.
fn status(_device: &Device, job: &str) -> Result<String, String> {
    let running = jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .get(job)
        .cloned();
    let Some(running) = running else {
        return Err(format!("no job `{job}` runs on this device"));
    };
    if running.cancel.load(Ordering::Relaxed) {
        return Ok(r#"{"state":"cancelled","error":""}"#.to_owned());
    }
    if let Some(error) = running.error.lock().ok().and_then(|error| error.clone()) {
        return Ok(serde_json::json!({ "state": "failed", "error": error }).to_string());
    }
    if running.finished.load(Ordering::Relaxed) {
        return Ok(r#"{"state":"done","error":""}"#.to_owned());
    }
    Ok(r#"{"state":"running","error":""}"#.to_owned())
}

/// Stop a transfer: false when it is not this device's or it had finished.
fn cancel(_device: &Device, job: &str) -> bool {
    let Ok(known) = jobs().lock() else {
        return false;
    };
    let Some(running) = known.get(job) else {
        return false;
    };
    if running.finished.load(Ordering::Relaxed) {
        return false;
    }
    running.cancel.store(true, Ordering::Relaxed);
    true
}

/// One transfer in flight.
struct Running {
    cancel: Arc<AtomicBool>,
    finished: AtomicBool,
    error: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct TransferAsk {
    path: String,
    #[serde(default)]
    remote: String,
}

fn jobs() -> &'static Mutex<BTreeMap<String, Arc<Running>>> {
    static JOBS: OnceLock<Mutex<BTreeMap<String, Arc<Running>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn begin(job: &str, op: &str, ask: TransferAsk) -> Result<(), String> {
    if !std::path::Path::new(&ask.path).is_absolute() {
        return Err("the path must be absolute".to_owned());
    }
    let id = job.to_owned();
    let running = Arc::new(Running {
        cancel: Arc::new(AtomicBool::new(false)),
        finished: AtomicBool::new(false),
        error: Mutex::new(None),
    });
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::clone(&running));
    let op = op.to_owned();
    let worker = Arc::clone(&running);
    std::thread::Builder::new()
        .name("git-transfer".into())
        .spawn(move || {
            let path = std::path::Path::new(&ask.path);
            let result = match op.as_str() {
                "fetch" => tilo_git::fetch(path, &ask.remote).map(|_| ()),
                "pull" => tilo_git::pull(path, &ask.remote),
                "push" => tilo_git::push(path, &ask.remote),
                _ => Err(format!("`{op}` is no transfer")),
            };
            if let Err(why) = result
                && let Ok(mut error) = worker.error.lock()
            {
                *error = Some(why);
            }
            worker.finished.store(true, Ordering::Relaxed);
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// The jobs this engine runs: a transfer to or from a remote, which reaches the network.
fn job_ops() -> &'static [JobOp] {
    const JOBS: &[JobOp] = &[
        JobOp::new("fetch", Needs::NETWORK),
        JobOp::new("pull", Needs::NETWORK),
        JobOp::new("push", Needs::NETWORK),
    ];
    JOBS
}

/// One stream: this engine runs none.
fn stream(_device: &Device, _op: &str, _input: &str, _frames: &Frames) -> Result<String, String> {
    Err("this engine runs no streams".to_owned())
}

/// The one engine this library answers with.
static API: EngineApi = EngineApi {
    app: APP,
    jobs: job_ops,
    call,
    start,
    status,
    stream,
    cancel,
};

/// The entry the process opens this library for.
#[unsafe(no_mangle)]
pub fn tilo_engine_entry() -> &'static EngineApi {
    &API
}

/// Proves this engine loads and runs against the runtime it is built with: a path that holds no
/// repository is read as no repository, through the git library this engine carries.
pub fn self_check() -> Result<String, String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    if tilo_git::is_repo(dir.path()) {
        return Err("the check's new folder reads as a repository".to_owned());
    }
    let refusal = tilo_git::run(
        "load",
        &serde_json::json!({ "path": dir.path() }).to_string(),
    );
    Ok(format!(
        "read an empty folder as no repository ({})",
        refusal.err().unwrap_or_else(|| "no refusal".to_owned())
    ))
}
