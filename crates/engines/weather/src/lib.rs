//! The Weather app's engine: Open-Meteo's forecast, air quality and place search, and nothing
//! about drawing them.
//!
//! The app runs sandboxed and reaches the network only as this device lets it. A refresh fetches
//! each place away from the frame — one job per refresh, its places read side by side — so a
//! slow service never holds the window, and the app asks which places have landed and takes
//! each answer as it arrives. A place search is a job of its own.
//!
//! What a service answers is read into one flat record per place: its current reading, the
//! hours and the days beside it as parallel lists, and its air quality. A record's field holds
//! no record in the app's language, so the app builds its own hours and days out of the lists.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

pub mod api;
pub mod fetch;

/// The one app this engine answers for.
pub const APP: &str = "so.tilo.weather";

/// How many places are read at once: enough that a handful of places finishes together, few
/// enough that a large library does not open every request at once.
const AT_ONCE: usize = 4;

/// One saved place a refresh is asked for.
#[derive(Clone, Debug, Deserialize)]
pub struct Asked {
    /// The place's id, which names its answer. Not `id`: in the app's language every record's
    /// `.id` is the resource it is, so the field travels under its own name.
    pub place_id: String,
    pub latitude: f64,
    pub longitude: f64,
}

/// What one refresh was asked to do.
#[derive(Debug, Deserialize)]
struct Refresh {
    places: Vec<Asked>,
}

/// Why a place could not be read, as the app words it: a service that refused this reader is
/// not a broken connection, and neither is an answer in a shape the forecast is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Unreachable,
    Refused,
    NotAReading,
    Unavailable,
}

/// A failed read: the reason the app words, the code a refusal carried, and the transport's own
/// words for a log.
#[derive(Clone, Debug)]
pub struct Failure {
    pub reason: Reason,
    pub status: u16,
    pub message: String,
}

impl Failure {
    pub(crate) fn unreachable(message: impl Into<String>) -> Self {
        Self {
            reason: Reason::Unreachable,
            status: 0,
            message: message.into(),
        }
    }

    pub(crate) fn refused(status: u16, message: impl Into<String>) -> Self {
        Self {
            reason: Reason::Refused,
            status,
            message: message.into(),
        }
    }

    pub(crate) fn not_a_reading(message: impl Into<String>) -> Self {
        Self {
            reason: Reason::NotAReading,
            status: 0,
            message: message.into(),
        }
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            reason: Reason::Unavailable,
            status: 0,
            message: message.into(),
        }
    }
}

/// One place read: its forecast, and its air quality when the second service answered.
#[derive(Debug)]
pub struct Found {
    pub readings: api::Readings,
    pub air: Option<api::Air>,
}

/// What one worker finished, waiting for the app to take it.
#[allow(
    clippy::large_enum_variant,
    reason = "a job's outcomes are made one at a time and taken at once; one box would be one allocation on every take"
)]
enum Outcome {
    Place {
        place_id: String,
        answer: Result<Found, Failure>,
        /// Epoch seconds when the answer was read.
        fetched: i64,
    },
    Search(Result<Vec<api::Place>, Failure>),
}

/// One running job.
struct Job {
    total: usize,
    next: AtomicUsize,
    done: AtomicUsize,
    stopped: AtomicBool,
    /// What the workers finished, until the app takes it.
    outcomes: Mutex<Vec<Outcome>>,
}

/// The jobs running on this device.
fn jobs() -> &'static Mutex<BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn held() -> MutexGuard<'static, BTreeMap<String, Arc<Job>>> {
    jobs()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// One `engine.call` for this app: take what a job finished.
///
/// # Errors
///
/// The op is unknown, an input does not read, or no such job runs.
pub fn call(op: &str, input: &str) -> Result<String, String> {
    match op {
        "take" => take(input),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// One `engine.start`: a refresh of some places, or a place search, answered with the job's id.
///
/// # Errors
///
/// The op is unknown, or the request does not read.
pub fn start(op: &str, input: &str) -> Result<String, String> {
    match op {
        "refresh" => refresh(input),
        "search" => search(input),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// How far a job has got: its state and its count.
///
/// # Errors
///
/// No such job runs on this device.
pub fn status(job: &str) -> Result<String, String> {
    let job = held()
        .get(job)
        .cloned()
        .ok_or_else(|| format!("no job `{job}` runs on this device"))?;
    let done = job.done.load(Ordering::Relaxed);
    let state = if job.stopped.load(Ordering::Relaxed) {
        "cancelled"
    } else if done >= job.total {
        "done"
    } else {
        "running"
    };
    Ok(json!({
        "state": state,
        "done": done,
        "total": job.total,
    })
    .to_string())
}

/// Stop a job: false when no engine runs it or it had finished.
pub fn cancel(job: &str) -> bool {
    let Some(job) = held().get(job).cloned() else {
        return false;
    };
    let running = job.done.load(Ordering::Relaxed) < job.total;
    job.stopped.store(true, Ordering::Relaxed);
    running
}

/// Everything a job finished since it was last taken, removed as it is taken. A job that has
/// finished and has nothing left stops running on this device.
///
/// # Errors
///
/// No such job runs, or its outcomes are unreadable.
fn take(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Take {
        job: String,
    }
    let ask: Take = serde_json::from_str(input).map_err(|e| format!("take: {e}"))?;
    let job = held()
        .get(&ask.job)
        .cloned()
        .ok_or_else(|| format!("no job `{}` runs on this device", ask.job))?;
    let outcomes = {
        let mut outcomes = job
            .outcomes
            .lock()
            .map_err(|_| "the job's answers are unreadable".to_owned())?;
        std::mem::take(&mut *outcomes)
    };
    let mut answers: Vec<String> = Vec::new();
    let mut places: Vec<String> = Vec::new();
    let mut error = String::new();
    for outcome in outcomes {
        match outcome {
            Outcome::Place {
                place_id,
                answer,
                fetched,
            } => answers.push(answer_text(&place_id, answer, fetched)),
            Outcome::Search(Ok(found)) => places.extend(
                found
                    .iter()
                    .map(|place| serde_json::to_string(place).unwrap_or_default()),
            ),
            Outcome::Search(Err(failure)) => error = failure_text(&failure),
        }
    }
    // A job whose workers finished and whose outcomes are all taken is gone: nothing waits on
    // it any more, and the app asked for the last of it here.
    if job.done.load(Ordering::Relaxed) >= job.total {
        let drained = job
            .outcomes
            .lock()
            .is_ok_and(|outcomes| outcomes.is_empty());
        if drained {
            held().remove(&ask.job);
        }
    }
    Ok(json!({ "answers": answers, "places": places, "error": error }).to_string())
}

/// One place's answer as the app reads it: its reading and its air reading beside each other,
/// each list of hours and days beside its own.
fn answer_text(id: &str, answer: Result<Found, Failure>, fetched: i64) -> String {
    let mut out = serde_json::Map::new();
    out.insert("place_id".to_owned(), json!(id));
    out.insert("fetched".to_owned(), json!(fetched));
    match answer {
        Ok(found) => {
            out.insert("ok".to_owned(), json!(true));
            out.insert("reason".to_owned(), json!(""));
            out.insert("status".to_owned(), json!(0));
            out.insert("message".to_owned(), json!(""));
            merge(&mut out, &found.readings);
            match &found.air {
                Some(air) => {
                    out.insert("air_ok".to_owned(), json!(true));
                    let mut air_fields = serde_json::Map::new();
                    merge(&mut air_fields, air);
                    for (name, value) in air_fields {
                        out.insert(format!("air_{name}"), value);
                    }
                }
                None => {
                    out.insert("air_ok".to_owned(), json!(false));
                }
            }
        }
        Err(failure) => {
            out.insert("ok".to_owned(), json!(false));
            out.insert("reason".to_owned(), json!(failure.reason));
            out.insert("status".to_owned(), json!(failure.status));
            out.insert("message".to_owned(), json!(failure.message));
            // The reading's own fields still travel, as empty as an unread place is: the app
            // reads one shape from every answer, and a refusal is read by its reason alone.
            merge(&mut out, &api::Readings::default());
            out.insert("air_ok".to_owned(), json!(false));
        }
    }
    Json::Object(out).to_string()
}

/// A reading's fields, written into `out` by their names.
fn merge<T: Serialize>(out: &mut serde_json::Map<String, Json>, value: &T) {
    if let Ok(Json::Object(fields)) = serde_json::to_value(value) {
        for (name, value) in fields {
            out.insert(name, value);
        }
    }
}

/// Why a read failed, as the app words it in a place's row.
fn failure_text(failure: &Failure) -> String {
    match failure.reason {
        Reason::Unreachable => "Couldn't reach the service".to_owned(),
        Reason::Refused => format!("The service refused the request (HTTP {})", failure.status),
        Reason::NotAReading => "The answer wasn't a forecast".to_owned(),
        Reason::Unavailable => "This device reaches no services".to_owned(),
    }
}

/// Start a refresh of `places`, read by a few workers side by side.
fn refresh(input: &str) -> Result<String, String> {
    let asked: Refresh = serde_json::from_str(input).map_err(|e| format!("refresh: {e}"))?;
    let total = asked.places.len();
    let job = Arc::new(Job {
        total,
        next: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        stopped: AtomicBool::new(false),
        outcomes: Mutex::new(Vec::new()),
    });
    let id = next_job_id("refresh");
    held().insert(id.clone(), Arc::clone(&job));
    let places = Arc::new(asked.places);
    for _ in 0..AT_ONCE.min(total) {
        let job = Arc::clone(&job);
        let places = Arc::clone(&places);
        std::thread::spawn(move || {
            loop {
                if job.stopped.load(Ordering::Relaxed) {
                    break;
                }
                let at = job.next.fetch_add(1, Ordering::Relaxed);
                let Some(place) = places.get(at) else { break };
                let answer = fetch::place(place.latitude, place.longitude);
                if let Ok(mut outcomes) = job.outcomes.lock() {
                    outcomes.push(Outcome::Place {
                        place_id: place.place_id.clone(),
                        answer,
                        fetched: now(),
                    });
                }
                job.done.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
    Ok(id)
}

/// Start a search for the places `query` names.
fn search(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Search {
        query: String,
    }
    let asked: Search = serde_json::from_str(input).map_err(|e| format!("search: {e}"))?;
    let job = Arc::new(Job {
        total: 1,
        next: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        stopped: AtomicBool::new(false),
        outcomes: Mutex::new(Vec::new()),
    });
    let id = next_job_id("search");
    held().insert(id.clone(), Arc::clone(&job));
    let query = asked.query;
    std::thread::spawn(move || {
        let found = fetch::search(&query);
        if let Ok(mut outcomes) = job.outcomes.lock() {
            outcomes.push(Outcome::Search(found));
        }
        job.done.fetch_add(1, Ordering::Relaxed);
    });
    Ok(id)
}

/// The wall clock in epoch seconds, read here so an answer says when it was read.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// A job's id: one per refresh or search, never reused.
fn next_job_id(kind: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    format!("weather-{kind}-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests;
