//! The Feeds app's engine: the network and the formats the app reads, and nothing about drawing
//! them.
//!
//! The app runs sandboxed and reaches the network only as this device lets it. A refresh fetches
//! each subscription away from the frame — one job per refresh, its feeds read side by side — so a
//! slow site never holds the window, and the app asks which feeds have arrived and takes each one
//! as it lands. What a feed says is read into the app's own records: its articles as Markdown the
//! app keeps as a document body.
//!
//! Two protocols arrive here: HTTP(S) as a site serves a feed, and `gemini://` as a capsule
//! answers over its own TLS connection. A Gemini server signs its own certificate, so the first
//! fetch pins the one it presents and every later fetch refuses another — the pin is BLAKE3 of
//! the certificate's DER, as hex.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

pub mod fetch;
pub mod opml;
pub mod parse;

/// The one app this engine answers for.
pub const APP: &str = "so.tilo.feeds";

/// How many subscriptions are read at once: enough that a library of feeds finishes together,
/// few enough that a large one does not open a connection per site at once.
const AT_ONCE: usize = 6;

/// One subscription a refresh is asked for.
#[derive(Clone, Debug, Deserialize)]
pub struct Asked {
    /// The subscription's feed address, which names its result.
    pub url: String,
    /// The certificate a Gemini server presented the first time, as hex; empty until then.
    #[serde(default)]
    pub pin: String,
}

/// What one refresh was asked to do.
#[derive(Debug, Deserialize)]
struct Refresh {
    feeds: Vec<Asked>,
}

/// Why a subscription could not be refreshed, as the app words it: a site that refused this
/// reader is not a broken connection, and neither is a page that turned out not to be a feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Unreachable,
    Refused,
    NotAFeed,
    Untrusted,
    Unavailable,
}

/// One feed read, or why it could not be.
#[derive(Clone, Debug)]
pub struct Found {
    /// The title the feed gives itself, when it gives a different one than it had.
    pub title: String,
    /// The certificate a Gemini server presented, as the pin the app keeps from then on.
    pub pin: String,
    pub articles: Vec<Article>,
}

/// One article a feed lists.
#[derive(Clone, Debug, Serialize)]
pub struct Article {
    pub title: String,
    pub author: String,
    pub url: String,
    /// Epoch seconds; 0 when the feed did not say.
    pub published: i64,
    pub summary: String,
    /// Its body as Markdown, which the app keeps as a document body.
    pub body: String,
}

/// A failed fetch: the reason the app words, the code a refusal carried, and the transport's own
/// words for a log.
#[derive(Clone, Debug)]
pub struct Failure {
    pub reason: Reason,
    pub status: u16,
    pub message: String,
}

impl Failure {
    fn unreachable(message: impl Into<String>) -> Self {
        Self {
            reason: Reason::Unreachable,
            status: 0,
            message: message.into(),
        }
    }

    fn refused(status: u16, message: impl Into<String>) -> Self {
        Self {
            reason: Reason::Refused,
            status,
            message: message.into(),
        }
    }

    fn not_a_feed(message: impl Into<String>) -> Self {
        Self {
            reason: Reason::NotAFeed,
            status: 0,
            message: message.into(),
        }
    }

    fn untrusted(message: impl Into<String>) -> Self {
        Self {
            reason: Reason::Untrusted,
            status: 0,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            reason: Reason::Unavailable,
            status: 0,
            message: message.into(),
        }
    }
}

/// One running refresh.
struct Job {
    total: usize,
    next: AtomicUsize,
    done: AtomicUsize,
    stopped: AtomicBool,
    /// What each feed answered, by its address, until the app takes it.
    results: Mutex<BTreeMap<String, Result<Found, Failure>>>,
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

/// One `engine.call` for this app: read now, or take what a job fetched.
///
/// # Errors
///
/// The op is unknown, an input does not read, or no such job runs.
pub fn call(op: &str, input: &str) -> Result<String, String> {
    match op {
        "take" => take(input),
        "opml" => opml_read(input),
        "opml-export" => opml_export(input),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// One `engine.start`: a refresh, answered with the job's id.
///
/// # Errors
///
/// The op is not a refresh, or the feeds do not read.
pub fn start(op: &str, input: &str) -> Result<String, String> {
    match op {
        "refresh" => refresh(input),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// How far a job has got: its state, its count, and the feeds whose answer is waiting.
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
    let ready: Vec<String> = job
        .results
        .lock()
        .map(|results| results.keys().cloned().collect())
        .unwrap_or_default();
    let state = if job.stopped.load(Ordering::Relaxed) {
        "cancelled"
    } else if done >= job.total {
        "done"
    } else {
        "running"
    };
    Ok(serde_json::json!({
        "state": state,
        "done": done,
        "total": job.total,
        "ready": ready,
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

/// The answer one feed of a job left, removed as it is taken.
///
/// # Errors
///
/// No such job runs, or it holds no answer for that feed yet.
fn take(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Take {
        job: String,
        url: String,
    }
    let ask: Take = serde_json::from_str(input).map_err(|e| format!("take: {e}"))?;
    let job = held()
        .get(&ask.job)
        .cloned()
        .ok_or_else(|| format!("no job `{}` runs on this device", ask.job))?;
    let answer = job
        .results
        .lock()
        .map_err(|_| "the job's answers are unreadable".to_owned())?
        .remove(&ask.url)
        .ok_or_else(|| format!("`{}` has no answer in job `{}`", ask.url, ask.job))?;
    Ok(match answer {
        // Each article travels as its own JSON text: a list of records is a list of resources in
        // the app's language, and JSON text makes none, so the app reads each one into its record.
        Ok(found) => serde_json::json!({
            "ok": true,
            "title": found.title,
            "pin": found.pin,
            "reason": "",
            "status": 0,
            "message": "",
            "articles": found
                .articles
                .iter()
                .map(|article| serde_json::to_string(article).unwrap_or_default())
                .collect::<Vec<_>>(),
        }),
        Err(failure) => serde_json::json!({
            "ok": false,
            "title": "",
            "pin": "",
            "reason": failure.reason,
            "status": failure.status,
            "message": failure.message,
            "articles": [],
        }),
    }
    .to_string())
}

/// Start a refresh of `feeds`, read by a few workers side by side.
fn refresh(input: &str) -> Result<String, String> {
    let asked: Refresh = serde_json::from_str(input).map_err(|e| format!("refresh: {e}"))?;
    let total = asked.feeds.len();
    let job = Arc::new(Job {
        total,
        next: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        stopped: AtomicBool::new(false),
        results: Mutex::new(BTreeMap::new()),
    });
    let id = next_job_id();
    held().insert(id.clone(), Arc::clone(&job));
    let feeds = Arc::new(asked.feeds);
    for _ in 0..AT_ONCE.min(total) {
        let job = Arc::clone(&job);
        let feeds = Arc::clone(&feeds);
        std::thread::spawn(move || {
            loop {
                if job.stopped.load(Ordering::Relaxed) {
                    break;
                }
                let at = job.next.fetch_add(1, Ordering::Relaxed);
                let Some(feed) = feeds.get(at) else { break };
                let answer = fetch::read(&feed.url, &feed.pin);
                if let Ok(mut results) = job.results.lock() {
                    results.insert(feed.url.clone(), answer);
                }
                job.done.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
    Ok(id)
}

/// A job's id: one per refresh, never reused.
fn next_job_id() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(1);
    format!("feeds-refresh-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// What an OPML file lists.
fn opml_read(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Read {
        text: String,
    }
    let ask: Read = serde_json::from_str(input).map_err(|e| format!("opml: {e}"))?;
    // Each outline travels as its own JSON text, as each article does: a list of records is a
    // list of resources in the app's language.
    let outlines: Vec<String> = opml::parse(&ask.text)
        .iter()
        .map(|outline| serde_json::to_string(outline).unwrap_or_default())
        .collect();
    Ok(serde_json::json!({ "outlines": outlines }).to_string())
}

/// One subscription an OPML export writes.
#[derive(Deserialize)]
pub(crate) struct OutLine {
    pub(crate) title: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) site: String,
    #[serde(default)]
    pub(crate) format: String,
    #[serde(default)]
    pub(crate) folder: String,
}

/// The subscriptions of `feeds` as an OPML 2.0 file.
fn opml_export(input: &str) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Export {
        feeds: Vec<OutLine>,
    }
    let ask: Export = serde_json::from_str(input).map_err(|e| format!("opml-export: {e}"))?;
    Ok(serde_json::json!({ "text": opml::export(&ask.feeds) }).to_string())
}

#[cfg(test)]
mod tests;
