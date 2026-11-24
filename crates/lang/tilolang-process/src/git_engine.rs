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

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.git-client";

/// One `engine.call` for this device's app, or a refusal.
///
/// # Errors
///
/// The op is unknown, the ask does not read, or the repository refuses it.
pub fn call(app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    tilo_git::run(op, input)
}

/// One `engine.start`: a transfer, answered with the job's id.
///
/// # Errors
///
/// Not this device's app, or the ask does not read.
pub fn start(app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    let transfer: TransferAsk =
        serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))?;
    match op {
        "fetch" | "pull" | "push" => begin(op, transfer),
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

/// How far a transfer has got, as JSON.
///
/// # Errors
///
/// No such job on this device.
pub fn status(job: &str) -> Result<String, String> {
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
#[must_use]
pub fn cancel(job: &str) -> bool {
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

fn begin(op: &str, ask: TransferAsk) -> Result<String, String> {
    if !std::path::Path::new(&ask.path).is_absolute() {
        return Err("the path must be absolute".to_owned());
    }
    let id = format!(
        "git-{op}-{}-{:x}",
        ask.path.len(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    );
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
    Ok(serde_json::json!({ "job": id }).to_string())
}
