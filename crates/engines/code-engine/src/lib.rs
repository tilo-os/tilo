// Apps' engine code runs beside the session, and its sizes and offsets are the device's own
// arithmetic: a byte count, a line's place, a job's number. The lints that hold shared crates to
// checked arithmetic do not apply, and panic-producing lints stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "an engine counts bytes, lines and jobs of its own"
)]
#![allow(
    clippy::too_many_lines,
    clippy::large_enum_variant,
    clippy::significant_drop_tightening,
    clippy::significant_drop_in_scrutinee,
    clippy::match_wildcard_for_single_variants,
    clippy::redundant_closure_for_method_calls,
    clippy::inefficient_to_string,
    clippy::needless_pass_by_value,
    clippy::needless_pass_by_ref_mut,
    clippy::assigning_clones,
    reason = "one dispatch over every op and every job reads as one table, and the engine's handles are its own"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::string_slice,
        reason = "a failed unwrap or index in a test is its assertion"
    )
)]

//! The device's work for the code editor: a project's files read and written, searched, and
//! the language servers that answer for them.
//!
//! An app runs sandboxed and starts no process, so this crate is what the app reaches through
//! `engine.call`, `engine.start` and `engine.cancel`. It registers as the
//! engine of `so.tilo.code-editor` and answers that app alone; every answer is JSON the package
//! reads into its own records.
//!
//! A language server is no job: it runs as long as the project is worked on. It is the app's
//! held connection ([`held`]), in the app's own connection process, never in the engine.
//!
//! Answers now (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `load` | `{path}` | `{text, mtime, len}` |
//! | `save` | `{path, text, expected, force}` | `{mtime, len}`, or `conflict` |
//! | `stat` | `{path}` | `{exists, mtime, len}` |
//! | `list` | `{path}` | `{entries}` |
//!
//! Jobs (`engine.start`), each telling every move it makes, and stopped by `engine.cancel`:
//!
//! | op | asks | each move says |
//! |---|---|---|
//! | `search` | `{root, needle, case_sensitive, whole_word}` | `{state, hits, files, truncated}`: every hit so far |
//! | `run` | `{command, cwd}` | `{state, exit, output, truncated, failure}` |

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{Value, json};
use tilolang_engine::{JobOp, JobSink, Needs, State};

pub mod disk;
pub mod held;
mod lang;
mod listing;
mod lsp;
mod search;

pub use lang::{Language, Severity};
pub use listing::{Listing, read_folder, read_folders_along};

/// The app this engine answers.
pub const APP: &str = "so.tilo.code-editor";

/// The most of a command's output one run keeps, in bytes.
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// The engine of the code editor, as the session keeps it for the app.
#[derive(Default)]
pub struct CodeEngine {
    jobs: Arc<Mutex<Jobs>>,
}

#[derive(Default)]
struct Jobs {
    running: BTreeMap<String, Job>,
}

enum Job {
    /// A search, by what stops its walk.
    Search(Arc<AtomicBool>),
    Run(RunJob),
}

struct RunJob {
    child: Arc<Mutex<Option<std::process::Child>>>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct RunOutput {
    bytes: Vec<u8>,
    truncated: bool,
    finished: bool,
    exit: i32,
    failure: String,
}

impl CodeEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Deserialize)]
struct PathAsk {
    path: String,
}

#[derive(Deserialize)]
struct SaveAsk {
    path: String,
    text: String,
    /// The modified time the buffer last agreed with, in nanoseconds; 0 for none.
    #[serde(default)]
    mtime: i64,
    #[serde(default)]
    len: u64,
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
struct SearchAsk {
    root: String,
    needle: String,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    whole_word: bool,
}

#[derive(Deserialize)]
struct RunAsk {
    command: String,
    #[serde(default)]
    cwd: String,
}

fn read_ask<T: serde::de::DeserializeOwned>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}

impl tilolang_engine::Provider for CodeEngine {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "load" => {
                let ask: PathAsk = read_ask(input)?;
                let loaded = disk::load(&expand(&ask.path)).map_err(|error| error.to_string())?;
                Ok(json!({
                    "text": loaded.text,
                    "mtime": loaded.stamp.mtime.unwrap_or(0),
                    "len": loaded.stamp.len,
                })
                .to_string())
            }
            "save" => {
                let ask: SaveAsk = read_ask(input)?;
                let expected = if ask.force || ask.mtime == 0 {
                    None
                } else {
                    Some(disk::Stamp {
                        mtime: Some(ask.mtime),
                        len: ask.len,
                    })
                };
                let stamp = disk::save(&expand(&ask.path), &ask.text, expected.as_ref()).map_err(
                    |error| match error {
                        disk::SaveError::Conflict => "conflict".to_owned(),
                        other => other.to_string(),
                    },
                )?;
                Ok(json!({ "mtime": stamp.mtime.unwrap_or(0), "len": stamp.len }).to_string())
            }
            "stat" => {
                let ask: PathAsk = read_ask(input)?;
                match disk::Stamp::of(&expand(&ask.path)) {
                    Ok(stamp) => Ok(json!({
                        "exists": true,
                        "mtime": stamp.mtime.unwrap_or(0),
                        "len": stamp.len,
                    })
                    .to_string()),
                    Err(_) => Ok(json!({ "exists": false, "mtime": 0, "len": 0 }).to_string()),
                }
            }
            "list" => {
                let ask: PathAsk = read_ask(input)?;
                let listing = read_folder(&expand(&ask.path)).map_err(|error| error.to_string())?;
                Ok(json!({ "entries": listing_entries(&listing) }).to_string())
            }
            other => Err(format!("`{other}` is no op this engine answers")),
        }
    }

    fn jobs(&self) -> &'static [JobOp] {
        // A search and a command read the person's project: each runs where the app's calls are
        // answered.
        const JOBS: &[JobOp] = &[
            JobOp::new("search", Needs::APP_STATE),
            JobOp::new("run", Needs::APP_STATE),
        ];
        JOBS
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        match op {
            "search" => {
                let ask: SearchAsk = read_ask(input)?;
                let query = search::Query::new(&ask.needle)
                    .case_sensitive(ask.case_sensitive)
                    .whole_word(ask.whole_word);
                let walk = search::start(&expand(&ask.root), query);
                self.held()
                    .running
                    .insert(job.to_owned(), Job::Search(walk.canceller()));
                follow_search(Arc::clone(&self.jobs), job.to_owned(), walk, moved)
            }
            "run" => {
                let ask: RunAsk = read_ask(input)?;
                let run = spawn_run(&ask.command, &expand(&ask.cwd), moved)?;
                self.held().running.insert(job.to_owned(), Job::Run(run));
                Ok(())
            }
            other => Err(format!("`{other}` is no job this engine starts")),
        }
    }

    fn cancel(&self, job: &str) -> bool {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(held) = jobs.running.remove(job) else {
            return false;
        };
        match held {
            Job::Search(stop) => stop.store(true, Ordering::Relaxed),
            Job::Run(run) => {
                run.cancelled.store(true, Ordering::Relaxed);
                if let Some(mut child) = run
                    .child
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        true
    }
}

impl CodeEngine {
    fn held(&self) -> std::sync::MutexGuard<'_, Jobs> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Follow the search `job` walking as `walk`: every hit so far is told as it is found, and its
/// end once the walk is done.
fn follow_search(
    jobs: Arc<Mutex<Jobs>>,
    job: String,
    mut walk: search::Job,
    moved: JobSink,
) -> Result<(), String> {
    let reading = |hits: &[Value], files: usize, truncated: bool, state: &str| json!({ "state": state, "hits": hits, "files": files, "truncated": truncated });
    moved.moved(State::Running, reading(&[], 0, false, "running"));
    std::thread::Builder::new()
        .name("code-editor-search-moves".into())
        .spawn(move || {
            let mut hits = Vec::new();
            while let Some(message) = walk.wait_next() {
                match message {
                    search::Msg::Hits(found) => {
                        hits.extend(found.into_iter().map(hit_json));
                        moved.moved(State::Running, reading(&hits, 0, false, "running"));
                    }
                    search::Msg::Done { files, truncated } => {
                        moved.moved(State::Done, reading(&hits, files, truncated, "done"));
                        break;
                    }
                }
            }
            // A walk that let go without saying it was done was stopped.
            moved.moved(State::Cancelled, reading(&hits, 0, false, "done"));
            jobs.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .running
                .remove(&job);
        })
        .map(|_| ())
        .map_err(|error| format!("the search could not be followed: {error}"))
}

fn hit_json(hit: search::Hit) -> Value {
    json!({
        "path": hit.path.to_string_lossy(),
        "rel": hit.rel,
        "line": hit.line,
        "column": hit.col,
        "len": hit.len,
        "preview": hit.preview,
    })
}

fn diagnostics(found: &[lsp::LspDiagnostic]) -> Vec<Value> {
    found
        .iter()
        .map(|diagnostic| {
            let severity = match lsp::severity_from_lsp(diagnostic.severity) {
                Severity::Error => "error",
                Severity::Warning => "warning",
                Severity::Info => "info",
                Severity::Hint => "hint",
            };
            json!({
                "severity": severity,
                "line": diagnostic.range.start.line,
                "column": diagnostic.range.start.character,
                "message": diagnostic.message,
            })
        })
        .collect()
}

/// A path as a person writes it: `~` is this device's home.
fn expand(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(path)
}

/// What the app reads of a command now.
fn run_reading(output: &RunOutput, cancelled: bool) -> (State, Value) {
    let (state, word) = if output.finished {
        if cancelled {
            (State::Cancelled, "stopped")
        } else if output.failure.is_empty() {
            (State::Done, "done")
        } else {
            (State::Failed, "stopped")
        }
    } else if cancelled {
        (State::Cancelled, "stopped")
    } else {
        (State::Running, "running")
    };
    (
        state,
        json!({
            "state": word,
            "exit": output.exit,
            "output": String::from_utf8_lossy(&output.bytes),
            "truncated": output.truncated,
            "failure": output.failure,
        }),
    )
}

fn spawn_run(command: &str, cwd: &Path, moved: JobSink) -> Result<RunJob, String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_owned());
    let mut child = Command::new(shell)
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("the command could not start: {error}"))?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let output = Arc::new(Mutex::new(RunOutput::default()));
    let held = Arc::new(Mutex::new(Some(child)));
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let output = Arc::clone(&output);
        let held = Arc::clone(&held);
        let cancelled = Arc::clone(&cancelled);
        let tell = {
            let cancelled = Arc::clone(&cancelled);
            move |output: &RunOutput| {
                let (state, reading) = run_reading(output, cancelled.load(Ordering::Relaxed));
                moved.moved(state, reading);
            }
        };
        tell(&RunOutput::default());
        std::thread::Builder::new()
            .name("code-editor-run".into())
            .spawn(move || {
                if let Some(out) = stdout.as_mut() {
                    read_into(out, &output, &tell);
                }
                if let Some(err) = stderr.as_mut() {
                    read_into(err, &output, &tell);
                }
                let exit = held
                    .lock()
                    .ok()
                    .and_then(|mut child| child.take())
                    .and_then(|mut child| child.wait().ok())
                    .and_then(|status| status.code())
                    .unwrap_or(-1);
                let mut output = output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                output.exit = exit;
                output.finished = true;
                if cancelled.load(Ordering::Relaxed) {
                    output.failure = "stopped".to_owned();
                }
                tell(&output);
            })
            .map_err(|error| format!("the command could not be watched: {error}"))?;
    }
    Ok(RunJob {
        child: held,
        cancelled,
    })
}

fn read_into(
    mut reader: impl std::io::Read,
    output: &Arc<Mutex<RunOutput>>,
    tell: &impl Fn(&RunOutput),
) {
    let mut buffer = [0u8; 8192];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(size) => {
                let mut output = output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if output.bytes.len() + size > MAX_OUTPUT_BYTES {
                    let room = MAX_OUTPUT_BYTES.saturating_sub(output.bytes.len());
                    output
                        .bytes
                        .extend_from_slice(buffer.get(..room).unwrap_or_default());
                    output.truncated = true;
                } else {
                    output
                        .bytes
                        .extend_from_slice(buffer.get(..size).unwrap_or_default());
                }
                tell(&output);
            }
        }
    }
}

fn listing_entries(listing: &Listing) -> Vec<Value> {
    let folders = listing.folders().iter().map(|(name, path)| {
        json!({
            "name": name,
            "path": path.to_string_lossy(),
            "folder": true,
            "size": 0,
            "modified": 0,
            "hidden": name.starts_with('.'),
        })
    });
    let files = listing.files().iter().map(|(name, path)| {
        let stamp = disk::Stamp::of(path).unwrap_or_default();
        json!({
            "name": name,
            "path": path.to_string_lossy(),
            "folder": false,
            "size": stamp.len,
            "modified": stamp.mtime.unwrap_or(0),
            "hidden": name.starts_with('.'),
        })
    });
    folders.chain(files).collect()
}

/// Register this crate as the engine of the code editor: what the session calls once at its
/// start, before any app asks.
pub fn register() {
    tilolang_engine::register(APP, Arc::new(CodeEngine::new()));
}
