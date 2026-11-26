//! The disk usage analyzer's device work: a folder measured, all the way down.
//!
//! An app runs sandboxed and reads no disk of its own, so this crate is what
//! `so.tilo.disk-usage` reaches through `engine.start` and `engine.cancel`.
//! It registers as that app's engine and answers no other app's asks; every answer is JSON the
//! package reads into its own records.
//!
//! Jobs (`engine.start`), whose record says how they stand, and stopped by `engine.cancel`:
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `scan` (start) | `{path}` | the job, `{state, path, bytes, files, total, unreadable, error}` |
//! | `entries` (call) | `{job}` | `[{name, path, bytes, files}]` |
//!
//! `entries` are the folders directly in the measured one, largest first, each with what it
//! holds all the way down; the files loose in the measured folder are one more entry named
//! "Files in this folder", which has no folder of its own. A list of records is read on its
//! own (`json.decode<List<Text>>`), never as a field of one, so it answers a call of its own
//! once the job has ended. `bytes` and `files` answer how much has been seen so far, so a
//! window shows the measuring as it goes.

pub mod scan;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Deserialize;
use serde_json::json;
use tilolang_engine::{JobOp, JobSink, Needs, Provider, State};

use crate::scan::{ScanCancellation, ScanResult};

/// The app whose engine this is: the one that asks these ops, and no other.
pub const APP: &str = "so.tilo.disk-usage";

/// Register this crate as the analyzer's engine: what the session calls once at its start,
/// before any app asks.
pub fn register() {
    tilolang_engine::register(APP, Arc::new(DiskUsage::default()));
}

/// The measurements under way, by the job the app was given.
#[derive(Default)]
pub struct DiskUsage {
    jobs: Mutex<Jobs>,
}

#[derive(Default)]
struct Jobs {
    running: BTreeMap<String, Job>,
}

struct Job {
    scan: Arc<Mutex<Measured>>,
    cancellation: ScanCancellation,
    /// Set once the scan's thread has written its answer.
    finished: Arc<AtomicBool>,
    /// Set when the person stopped it.
    stopped: Arc<AtomicBool>,
}

/// What a scan has measured.
#[derive(Default)]
struct Measured {
    bytes: u64,
    files: u64,
    result: Option<ScanResult>,
    failure: Option<String>,
}

#[derive(Deserialize)]
struct ScanAsk {
    path: String,
}

#[derive(Deserialize)]
struct JobAsk {
    job: String,
}

fn held<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl DiskUsage {}

impl Provider for DiskUsage {
    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        if op != "entries" {
            return Err(format!(
                "`{op}` is no op this device answers: a folder is measured as a job (`engine.start`)"
            ));
        }
        let ask: JobAsk = serde_json::from_str(input)
            .map_err(|error| format!("the ask does not read: {error}"))?;
        let mut jobs = held(&self.jobs);
        let Some(scan) = jobs.running.get(&ask.job) else {
            return Err(format!("no job `{}` runs on this device", ask.job));
        };
        if !scan.finished.load(Ordering::Relaxed) {
            return Err("the measuring has not ended yet".to_owned());
        }
        let seen = held(&scan.scan);
        let Some(result) = &seen.result else {
            return Err("the measuring made no entries".to_owned());
        };
        if seen.failure.is_some() || scan.stopped.load(Ordering::Relaxed) {
            return Err("the measuring ended without entries".to_owned());
        }
        let entries = result
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "name": entry.name,
                    "path": entry.path.to_string_lossy(),
                    "bytes": entry.bytes,
                    "files": entry.files,
                })
            })
            .collect::<Vec<_>>();
        drop(seen);
        jobs.running.remove(&ask.job);
        drop(jobs);
        Ok(serde_json::Value::Array(entries).to_string())
    }

    fn jobs(&self) -> &'static [JobOp] {
        // Its entries are read back by a call once it has ended, so the job runs where the
        // app's calls are answered: the app's own process.
        const JOBS: &[JobOp] = &[JobOp::new("scan", Needs::APP_STATE)];
        JOBS
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        if op != "scan" {
            return Err(format!("`{op}` is no op this device runs"));
        }
        let ask: ScanAsk = serde_json::from_str(input)
            .map_err(|error| format!("the ask does not read: {error}"))?;
        let path = expand(&ask.path);
        let state = Arc::new(Mutex::new(Measured::default()));
        let finished = Arc::new(AtomicBool::new(false));
        let stopped = Arc::new(AtomicBool::new(false));
        let cancellation = ScanCancellation::default();
        let (writing, truth) = (Arc::clone(&state), Arc::clone(&state));
        let (done, stop) = (Arc::clone(&finished), Arc::clone(&stopped));
        let worker = cancellation.clone();
        let (telling, told) = (moved.clone(), moved.clone());
        let (asked, named) = (ask.path.clone(), ask.path);
        held(&self.jobs).running.insert(
            job.to_owned(),
            Job {
                scan: state,
                cancellation,
                finished,
                stopped,
            },
        );
        moved.moved(
            State::Running,
            json!({ "path": asked, "bytes": 0, "files": 0, "total": 0, "unreadable": 0, "error": "" }),
        );
        std::thread::Builder::new()
            .name(format!("disk-usage-{job}"))
            .spawn(move || {
                let result = scan::scan(path, &worker, &|bytes, files| {
                    let mut seen = held(&writing);
                    seen.bytes = bytes;
                    seen.files = files;
                    drop(seen);
                    telling.moved(
                        State::Running,
                        json!({
                            "path": asked,
                            "bytes": bytes,
                            "files": files,
                            "total": 0,
                            "unreadable": 0,
                            "error": "",
                        }),
                    );
                });
                let mut seen = held(&truth);
                if let Some(error) = &result.error {
                    seen.failure = Some(error.clone());
                }
                let cancelled = worker.is_cancelled();
                let ended = if cancelled {
                    State::Cancelled
                } else if seen.failure.is_some() {
                    State::Failed
                } else {
                    State::Done
                };
                let detail = json!({
                    "path": named,
                    "bytes": seen.bytes,
                    "files": seen.files,
                    "total": result.total,
                    "unreadable": result.unreadable,
                    "error": seen.failure.clone().unwrap_or_default(),
                });
                seen.result = Some(result);
                drop(seen);
                done.store(true, Ordering::Relaxed);
                stop.store(cancelled, Ordering::Relaxed);
                told.moved(ended, detail);
            })
            .map_err(|error| format!("the scan starts no thread: {error}"))?;
        Ok(())
    }

    fn cancel(&self, job: &str) -> bool {
        let jobs = held(&self.jobs);
        let Some(scan) = jobs.running.get(job) else {
            return false;
        };
        // A scan that already wrote its answer had finished before the person stopped it.
        if scan.finished.load(Ordering::Relaxed) {
            return false;
        }
        scan.cancellation.cancel();
        drop(jobs);
        true
    }
}

/// The path the ask names, its `~` read as the person's home folder.
fn expand(path: &str) -> PathBuf {
    if path == "~"
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home);
    }
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return Path::new(&home).join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A job tells JSON the package reads, and a folder that is not there fails rather than
    /// answering nothing.
    #[test]
    fn a_scan_answers_the_folders_largest_first_and_a_missing_one_fails() {
        let engine = DiskUsage::default();
        let ask = json!({ "path": "/no/such/folder" }).to_string();
        let (tell, moves) = std::sync::mpsc::channel();
        let tell = Mutex::new(tell);
        engine
            .start(
                "scan-1",
                "scan",
                &ask,
                JobSink::new(move |state, detail| {
                    let _ = held(&tell).send((state, detail));
                }),
            )
            .expect("the job starts");
        let (state, answer) = loop {
            let (state, detail) = moves
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the job moves");
            if state.ended() {
                break (state, detail);
            }
        };
        assert_eq!(state, State::Failed);
        assert!(
            answer["error"]
                .as_str()
                .is_some_and(|said| !said.is_empty()),
            "a folder that is not there says why: {answer}"
        );
    }
}
