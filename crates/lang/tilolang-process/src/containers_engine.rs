//! The Containers app's engine work, as a package reaches it: `engine.start("pull")` answered
//! for `so.tilo.containers` by running rootless `podman pull` on this device.
//!
//! A pull is the app's long work: it streams the engine's output into the job's lines, reports
//! how it stands, and can be stopped, which ends the podman process rather than only hiding the
//! window. Everything a pull changes in the engine shows in the `containers` reading the
//! session's system service publishes.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use tilolang_tilo::instance::Platform;

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.containers";

/// How many output lines a job keeps: enough to read why a pull stalled.
const LINES_KEPT: usize = 500;

/// One `engine.call` for this device's app. A pull changes the engine and streams, so it is a
/// job (`start`) and not a call here.
///
/// # Errors
///
/// The op is unknown.
pub fn call(_platform: &dyn Platform, app: &str, op: &str, _input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    Err(format!("`{op}` is no op this device runs for {app}"))
}

/// One `engine.start`: a pull, answered with the job's id.
///
/// # Errors
///
/// Not this device's app, the image does not read, or podman did not start.
pub fn start(_platform: &dyn Platform, app: &str, op: &str, input: &str) -> Result<String, String> {
    if app != APP {
        return Err(format!("`{op}` is no op this device runs for {app}"));
    }
    match op {
        "pull" => pull(input),
        other => Err(format!("`{other}` is no op this device runs for {app}")),
    }
}

/// How far a job has got, as JSON: its state, the output it kept, and what failed.
///
/// # Errors
///
/// No such job on this device.
pub fn status(job: &str) -> Result<String, String> {
    let known = jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?;
    let Some(state) = known.get(job) else {
        return Err(format!("no job `{job}` runs on this device"));
    };
    let lines = state
        .lines
        .lock()
        .map(|lines| lines.clone())
        .unwrap_or_default();
    let error = state.error.lock().ok().and_then(|error| error.clone());
    let (kind, error) = if state.stopped.load(Ordering::Relaxed) {
        ("cancelled", String::new())
    } else if let Some(error) = error {
        ("failed", error)
    } else if state.finished.load(Ordering::Relaxed) {
        ("done", String::new())
    } else {
        ("running", String::new())
    };
    Ok(serde_json::json!({
        "state": kind,
        "lines": lines,
        "error": error,
    })
    .to_string())
}

/// Stop a job: the podman process is ended. False when it is not this device's or it had
/// finished.
pub fn cancel(job: &str) -> bool {
    let Ok(known) = jobs().lock() else {
        return false;
    };
    let Some(state) = known.get(job) else {
        return false;
    };
    if state.finished.load(Ordering::Relaxed) {
        return false;
    }
    state.stopped.store(true, Ordering::Relaxed);
    if let Ok(mut child) = state.child.lock()
        && let Some(child) = child.as_mut()
    {
        let _ = child.kill();
    }
    true
}

/// One pull in flight, and what the engine wrote.
struct Job {
    finished: AtomicBool,
    stopped: AtomicBool,
    lines: Mutex<Vec<String>>,
    error: Mutex<Option<String>>,
    child: Mutex<Option<Child>>,
}

fn jobs() -> &'static Mutex<BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn next_job_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!("pull-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

fn pull(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Ask {
        image: String,
    }
    let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
    let image = ask.image.trim();
    if image.is_empty() || image.starts_with('-') {
        return Err(format!("`{image}` is not an image to pull"));
    }
    let mut child = Command::new("podman")
        .args(["pull", image])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("podman did not start: {error}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let id = next_job_id();
    let state = Arc::new(Job {
        finished: AtomicBool::new(false),
        stopped: AtomicBool::new(false),
        lines: Mutex::new(Vec::new()),
        error: Mutex::new(None),
        child: Mutex::new(Some(child)),
    });
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), Arc::clone(&state));
    let worker = Arc::clone(&state);
    std::thread::Builder::new()
        .name("containers-pull".to_owned())
        .spawn(move || {
            let read = |reader: Box<dyn BufRead>, lines: &Mutex<Vec<String>>| {
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut lines) = lines.lock() {
                        lines.push(line);
                        if lines.len() > LINES_KEPT {
                            let cut = lines.len() - LINES_KEPT;
                            lines.drain(..cut);
                        }
                    }
                }
            };
            let stderr_state = Arc::clone(&worker);
            let stderr_thread = stderr.map(|stderr| {
                std::thread::spawn(move || {
                    read(Box::new(BufReader::new(stderr)), &stderr_state.lines);
                })
            });
            if let Some(stdout) = stdout {
                read(Box::new(BufReader::new(stdout)), &worker.lines);
            }
            if let Some(thread) = stderr_thread {
                let _ = thread.join();
            }
            let status = worker
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.as_mut().and_then(|child| child.wait().ok()));
            if !worker.stopped.load(Ordering::Relaxed)
                && !status.is_some_and(|status| status.success())
            {
                let error = status.map_or_else(
                    || "podman did not answer".to_owned(),
                    |status| format!("podman pull exited with {status}"),
                );
                if let Ok(mut slot) = worker.error.lock() {
                    *slot = Some(error);
                }
            }
            worker.finished.store(true, Ordering::Relaxed);
        })
        .map_err(|error| format!("could not start the pull: {error}"))?;
    Ok(serde_json::json!({ "job": id }).to_string())
}
