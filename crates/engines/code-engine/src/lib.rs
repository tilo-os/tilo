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
//! `engine.call`, `engine.start`, `engine.status` and `engine.cancel`. It registers as the
//! engine of `so.tilo.code-editor` and answers that app alone; every answer is JSON the package
//! reads into its own records.
//!
//! Answers now (`engine.call`):
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `load` | `{path}` | `{text, mtime, len}` |
//! | `save` | `{path, text, expected, force}` | `{mtime, len}`, or `conflict` |
//! | `stat` | `{path}` | `{exists, mtime, len}` |
//! | `list` | `{path}` | `{entries}` |
//! | `open` | `{job, uri, language, text}` | `{}` |
//! | `change` | `{job, uri, version, text}` | `{}` |
//! | `saved` | `{job, uri}` | `{}` |
//! | `ask` | `{job, uri, line, column, what}` | `{}` — the answer arrives with `status` |
//!
//! Jobs (`engine.start`), asked about by `engine.status` and stopped by `engine.cancel`:
//!
//! | op | asks | `status` answers |
//! |---|---|---|
//! | `search` | `{root, needle, case_sensitive, whole_word}` | `{state, hits, files, truncated}` |
//! | `run` | `{command, cwd}` | `{state, exit, output, truncated, failure}` |
//! | `server` | `{language, root}` | `{state, server, progress, failure, diagnostics, answer}` |

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::{Value, json};

pub mod disk;
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
    jobs: Mutex<Jobs>,
}

#[derive(Default)]
struct Jobs {
    next: u64,
    running: BTreeMap<String, Job>,
}

enum Job {
    Search(search::Job),
    Run(RunJob),
    Server(LspSession),
}

struct RunJob {
    child: Arc<Mutex<Option<std::process::Child>>>,
    output: Arc<Mutex<RunOutput>>,
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

struct LspSession {
    server: String,
    client: lsp::LspClient,
    diagnostics: BTreeMap<String, Vec<lsp::LspDiagnostic>>,
    /// What the app last asked the server, so an answer is returned as the ask it belongs to.
    asked: Option<String>,
    failure: String,
    progress: Option<lsp::Progress>,
}

impl CodeEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn next_job(jobs: &mut Jobs, name: &str) -> String {
        jobs.next += 1;
        format!("{name}-{}", jobs.next)
    }

    fn job_mut<'a>(jobs: &'a mut Jobs, id: &str) -> Result<&'a mut Job, String> {
        jobs.running
            .get_mut(id)
            .ok_or_else(|| format!("no job `{id}` runs on this device"))
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

#[derive(Deserialize)]
struct ServerAsk {
    language: String,
    root: String,
}

#[derive(Deserialize)]
struct DocumentAsk {
    job: String,
    path: String,
    #[serde(default)]
    language: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    version: i64,
}

#[derive(Deserialize)]
struct SavedAsk {
    job: String,
    path: String,
}

#[derive(Deserialize)]
struct AskAsk {
    job: String,
    path: String,
    #[serde(default)]
    line: u32,
    #[serde(default)]
    column: u32,
    what: String,
}

fn read_ask<T: serde::de::DeserializeOwned>(input: &str) -> Result<T, String> {
    serde_json::from_str(input).map_err(|error| format!("the ask does not read: {error}"))
}

impl tilolang_tilo::engine::Provider for CodeEngine {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            "open" => {
                let ask: DocumentAsk = read_ask(input)?;
                let language = Language::from_label(&ask.language).unwrap_or(Language::PlainText);
                let job = Self::job_mut(&mut jobs, &ask.job)?;
                let Job::Server(session) = job else {
                    return Err("that job is not a language server".to_owned());
                };
                let uri = lsp::path_to_uri(&expand(&ask.path));
                session
                    .client
                    .did_open(&uri, language.server_id(), ask.version, &ask.text);
                Ok("{}".to_owned())
            }
            "change" => {
                let ask: DocumentAsk = read_ask(input)?;
                let job = Self::job_mut(&mut jobs, &ask.job)?;
                let Job::Server(session) = job else {
                    return Err("that job is not a language server".to_owned());
                };
                let uri = lsp::path_to_uri(&expand(&ask.path));
                session.client.did_change(&uri, ask.version, &ask.text);
                Ok("{}".to_owned())
            }
            "saved" => {
                let ask: SavedAsk = read_ask(input)?;
                let job = Self::job_mut(&mut jobs, &ask.job)?;
                let Job::Server(session) = job else {
                    return Err("that job is not a language server".to_owned());
                };
                let uri = lsp::path_to_uri(&expand(&ask.path));
                session.client.did_save(&uri);
                Ok("{}".to_owned())
            }
            "ask" => {
                let ask: AskAsk = read_ask(input)?;
                let position = lsp::Position {
                    line: ask.line,
                    character: ask.column,
                };
                let job = Self::job_mut(&mut jobs, &ask.job)?;
                let Job::Server(session) = job else {
                    return Err("that job is not a language server".to_owned());
                };
                let uri = lsp::path_to_uri(&expand(&ask.path));
                match ask.what.as_str() {
                    "complete" => session.client.request_completion(&uri, position),
                    "hover" => session.client.request_hover(&uri, position),
                    "definition" => session.client.request_definition(&uri, position),
                    "format" => {
                        session.client.request_formatting(&uri, 4, true);
                    }
                    other => return Err(format!("`{other}` is no ask a server answers")),
                }
                session.asked = Some(ask.what);
                Ok("{}".to_owned())
            }
            other => Err(format!("`{other}` is no op this engine answers")),
        }
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match op {
            "search" => {
                let ask: SearchAsk = read_ask(input)?;
                let query = search::Query::new(&ask.needle)
                    .case_sensitive(ask.case_sensitive)
                    .whole_word(ask.whole_word);
                let id = Self::next_job(&mut jobs, "search");
                jobs.running.insert(
                    id.clone(),
                    Job::Search(search::start(&expand(&ask.root), query)),
                );
                Ok(json!({ "job": id }).to_string())
            }
            "run" => {
                let ask: RunAsk = read_ask(input)?;
                let id = Self::next_job(&mut jobs, "run");
                jobs.running.insert(
                    id.clone(),
                    Job::Run(spawn_run(&ask.command, &expand(&ask.cwd))?),
                );
                Ok(json!({ "job": id }).to_string())
            }
            "server" => {
                let ask: ServerAsk = read_ask(input)?;
                let language = Language::from_label(&ask.language).ok_or_else(|| {
                    format!("`{}` is no language this device knows", ask.language)
                })?;
                let client = lsp::LspClient::start(language, &expand(&ask.root))
                    .map_err(|failure| failure.to_string())?;
                let server = client.server_name().to_owned();
                let id = Self::next_job(&mut jobs, "server");
                jobs.running.insert(
                    id.clone(),
                    Job::Server(LspSession {
                        server,
                        client,
                        diagnostics: BTreeMap::new(),
                        asked: None,
                        failure: String::new(),
                        progress: None,
                    }),
                );
                Ok(json!({ "job": id }).to_string())
            }
            other => Err(format!("`{other}` is no job this engine starts")),
        }
    }

    fn status(&self, job: &str) -> Result<String, String> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match Self::job_mut(&mut jobs, job)? {
            Job::Search(search) => {
                let mut hits = Vec::new();
                let mut files = 0;
                let mut truncated = false;
                let mut done = false;
                for message in search.drain() {
                    match message {
                        search::Msg::Hits(found) => {
                            hits.extend(found.into_iter().map(hit_json));
                        }
                        search::Msg::Done {
                            files: scanned,
                            truncated: cut,
                        } => {
                            files = scanned;
                            truncated = cut;
                            done = true;
                        }
                    }
                }
                if done {
                    jobs.running.remove(job);
                }
                Ok(json!({
                    "state": if done { "done" } else { "running" },
                    "hits": hits,
                    "files": files,
                    "truncated": truncated,
                })
                .to_string())
            }
            Job::Run(run) => {
                let output = run
                    .output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let state = if output.finished {
                    if output.failure.is_empty() {
                        "done"
                    } else {
                        "stopped"
                    }
                } else if run.cancelled.load(Ordering::Relaxed) {
                    "stopped"
                } else {
                    "running"
                };
                Ok(json!({
                    "state": state,
                    "exit": output.exit,
                    "output": String::from_utf8_lossy(&output.bytes),
                    "truncated": output.truncated,
                    "failure": output.failure,
                })
                .to_string())
            }
            Job::Server(session) => {
                let mut answer: Option<Value> = None;
                let mut stopped = false;
                for event in session.client.poll() {
                    match event {
                        lsp::LspEvent::Diagnostics { uri, diagnostics } => {
                            session.diagnostics.insert(uri, diagnostics);
                        }
                        lsp::LspEvent::Progress(progress) => {
                            session.progress = session.client.note_progress(&progress);
                        }
                        lsp::LspEvent::Initialized => {
                            session.progress = None;
                        }
                        lsp::LspEvent::Completion(items) => {
                            if session.asked.as_deref() == Some("complete") {
                                answer = Some(json!({
                                    "what": "complete",
                                    "items": items
                                        .into_iter()
                                        .map(|item| json!({
                                            "label": item.label,
                                            "detail": item.detail.unwrap_or_default(),
                                            "insert": item.insert_text,
                                        }))
                                        .collect::<Vec<_>>(),
                                }));
                            }
                        }
                        lsp::LspEvent::Hover(text) => {
                            if session.asked.as_deref() == Some("hover") {
                                answer = Some(json!({
                                    "what": "hover",
                                    "text": text.unwrap_or_default(),
                                }));
                            }
                        }
                        lsp::LspEvent::Definition(locations) => {
                            if session.asked.as_deref() == Some("definition") {
                                answer = Some(json!({
                                    "what": "definition",
                                    "locations": locations
                                        .into_iter()
                                        .map(|location| json!({
                                            "uri": location.uri,
                                            "line": location.range.start.line,
                                            "column": location.range.start.character,
                                        }))
                                        .collect::<Vec<_>>(),
                                }));
                            }
                        }
                        lsp::LspEvent::Formatted(edits) => {
                            if session.asked.as_deref() == Some("format") {
                                answer = Some(match edits {
                                    Ok(edits) => json!({
                                        "what": "format",
                                        "edits": edits
                                            .into_iter()
                                            .map(|edit| json!({
                                                "start": edit.range.start.line,
                                                "start_column": edit.range.start.character,
                                                "end": edit.range.end.line,
                                                "end_column": edit.range.end.character,
                                                "text": edit.new_text,
                                            }))
                                            .collect::<Vec<_>>(),
                                    }),
                                    Err(why) => json!({ "what": "format", "error": why }),
                                });
                            }
                        }
                        lsp::LspEvent::Exited => {
                            session.failure = "the server stopped".to_owned();
                            stopped = true;
                        }
                    }
                }
                let progress = session.progress.as_ref().map(|progress| {
                    json!({
                        "title": progress.title,
                        "detail": progress.message.clone().unwrap_or_default(),
                        "percent": progress.percentage,
                    })
                });
                let diagnostics: Value = session
                    .diagnostics
                    .iter()
                    .map(|(uri, found)| (lsp::uri_to_path(uri), json!(diagnostics(found))))
                    .collect::<serde_json::Map<_, _>>()
                    .into();
                let answer = match answer {
                    Some(answer) => {
                        session.asked = None;
                        answer.to_string()
                    }
                    None => String::new(),
                };
                let state = if stopped {
                    "stopped"
                } else if progress.is_some() {
                    "busy"
                } else if session.failure.is_empty() {
                    "ready"
                } else {
                    "failed"
                };
                let answer = json!({
                    "state": state,
                    "server": session.server,
                    "failure": session.failure,
                    // A list of the one piece of work under way, or none: a record the app
                    // reads is a value it holds, never a reference an option would name.
                    "progress": progress.into_iter().collect::<Vec<_>>(),
                    "diagnostics": diagnostics,
                    "answer": answer,
                })
                .to_string();
                if stopped {
                    jobs.running.remove(job);
                }
                Ok(answer)
            }
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
            Job::Search(search) => search.cancel(),
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
            Job::Server(_) => {}
        }
        true
    }
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

fn spawn_run(command: &str, cwd: &Path) -> Result<RunJob, String> {
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
        std::thread::Builder::new()
            .name("code-editor-run".into())
            .spawn(move || {
                if let Some(out) = stdout.as_mut() {
                    read_into(out, &output);
                }
                if let Some(err) = stderr.as_mut() {
                    read_into(err, &output);
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
            })
            .map_err(|error| format!("the command could not be watched: {error}"))?;
    }
    Ok(RunJob {
        child: held,
        output,
        cancelled,
    })
}

fn read_into(mut reader: impl std::io::Read, output: &Arc<Mutex<RunOutput>>) {
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
    tilolang_tilo::engine::register(APP, Arc::new(CodeEngine::new()));
}
