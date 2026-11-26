//! The Containers app's engine work, as a package reaches it: `engine.start("pull")` answered
//! for `so.tilo.containers` by running rootless `podman pull` on this device.
//!
//! A pull is the app's long work: it tells the engine's output as the job's lines as they come,
//! and can be stopped, which ends the podman process rather than only hiding the window.
//! Everything a pull changes in the engine shows in the `containers` reading the session's system
//! service publishes.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use tilolang_engine::{JobOp, JobSink, Needs, Provider, Running, State};

/// The one app this device answers engine work for.
pub const APP: &str = "so.tilo.containers";

/// How many output lines a job keeps: enough to read why a pull stalled.
const LINES_KEPT: usize = 500;

/// Register the engine once: every later ask finds it.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| tilolang_engine::register(APP, Arc::new(Containers)));
}

/// The registry's entry for the Containers app.
struct Containers;

/// One pull in flight, and what the engine wrote.
struct Pull {
    stopped: AtomicBool,
    lines: Mutex<Vec<String>>,
    child: Mutex<Option<Child>>,
    moved: JobSink,
}

impl Pull {
    /// Tell the pull's lines as they stand, at `state`, failing with `error`.
    fn tell(&self, state: State, error: &str) {
        let lines = self
            .lines
            .lock()
            .map(|lines| lines.clone())
            .unwrap_or_default();
        self.moved
            .moved(state, serde_json::json!({ "lines": lines, "error": error }));
    }
}

fn running() -> &'static Running<Pull> {
    static RUNNING: OnceLock<Running<Pull>> = OnceLock::new();
    RUNNING.get_or_init(Running::default)
}

impl Provider for Containers {
    fn jobs(&self) -> &'static [JobOp] {
        // A pull reaches the image's registry over the network.
        const JOBS: &[JobOp] = &[JobOp::new("pull", Needs::NETWORK)];
        JOBS
    }

    fn call(&self, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("`{op}` is no op this device runs for {APP}"))
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        match op {
            "pull" => pull(job, input, moved),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    /// Stop a job: the podman process is ended.
    fn cancel(&self, job: &str) -> bool {
        let Some((pull, _)) = running().get(job) else {
            return false;
        };
        pull.stopped.store(true, Ordering::Relaxed);
        if let Ok(mut child) = pull.child.lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.kill();
        }
        true
    }
}

fn pull(job: &str, input: &str, moved: JobSink) -> Result<(), String> {
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
    let state = Arc::new(Pull {
        stopped: AtomicBool::new(false),
        lines: Mutex::new(Vec::new()),
        child: Mutex::new(Some(child)),
        moved: moved.clone(),
    });
    running().keep(job, Arc::clone(&state), moved);
    state.tell(State::Running, "");
    let worker = Arc::clone(&state);
    let named = job.to_owned();
    std::thread::Builder::new()
        .name("containers-pull".to_owned())
        .spawn(move || {
            let read = |reader: Box<dyn BufRead>, pull: &Pull| {
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut lines) = pull.lines.lock() {
                        lines.push(line);
                        if lines.len() > LINES_KEPT {
                            let cut = lines.len().saturating_sub(LINES_KEPT);
                            lines.drain(..cut);
                        }
                    }
                    pull.tell(State::Running, "");
                }
            };
            let stderr_state = Arc::clone(&worker);
            let stderr_thread = stderr.map(|stderr| {
                std::thread::spawn(move || {
                    read(Box::new(BufReader::new(stderr)), &stderr_state);
                })
            });
            if let Some(stdout) = stdout {
                read(Box::new(BufReader::new(stdout)), &worker);
            }
            if let Some(thread) = stderr_thread {
                let _ = thread.join();
            }
            let status = worker
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.as_mut().and_then(|child| child.wait().ok()));
            if worker.stopped.load(Ordering::Relaxed) {
                worker.tell(State::Cancelled, "");
            } else if status.is_some_and(|status| status.success()) {
                worker.tell(State::Done, "");
            } else {
                let error = status.map_or_else(
                    || "podman did not answer".to_owned(),
                    |status| format!("podman pull exited with {status}"),
                );
                worker.tell(State::Failed, &error);
            }
            running().forget(&named);
        })
        .map_err(|error| format!("could not start the pull: {error}"))?;
    Ok(())
}
