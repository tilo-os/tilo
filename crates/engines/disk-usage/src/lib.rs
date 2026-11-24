//! The disk usage analyzer's device work: a folder measured, all the way down.
//!
//! An app runs sandboxed and reads no disk of its own, so this crate is what
//! `so.tilo.disk-usage` reaches through `engine.start`, `engine.status` and `engine.cancel`.
//! It registers as that app's engine and answers no other app's asks; every answer is JSON the
//! package reads into its own records.
//!
//! Jobs (`engine.start`), asked about by `engine.status` and stopped by `engine.cancel`:
//!
//! | op | asks | answers |
//! |---|---|---|
//! | `scan` (start) | `{path}` | `{job}` |
//! | `status` | the job | `{state, path, bytes, files, total, unreadable, error}` |
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
use tilolang_tilo::engine::Provider;

use crate::scan::{ScanCancellation, ScanResult};

/// The app whose engine this is: the one that asks these ops, and no other.
pub const APP: &str = "so.tilo.disk-usage";

/// Register this crate as the analyzer's engine: what the session calls once at its start,
/// before any app asks.
pub fn register() {
    tilolang_tilo::engine::register(APP, Arc::new(DiskUsage::default()));
}

/// The measurements under way, by the job the app was given.
#[derive(Default)]
pub struct DiskUsage {
    jobs: Mutex<Jobs>,
}

#[derive(Default)]
struct Jobs {
    next: u64,
    running: BTreeMap<String, Job>,
}

struct Job {
    /// The folder measured, as the ask named it.
    path: String,
    scan: Arc<Mutex<Measured>>,
    cancellation: ScanCancellation,
    /// Set once the scan's thread has written its answer.
    finished: Arc<AtomicBool>,
    /// Set when the person stopped it.
    stopped: Arc<AtomicBool>,
}

/// What a scan has measured, as `status` reads it.
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
    lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl DiskUsage {
    fn next_job(jobs: &mut Jobs, name: &str) -> String {
        jobs.next += 1;
        format!("{name}-{}", jobs.next)
    }
}

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
        Ok(serde_json::Value::Array(entries).to_string())
    }

    fn start(&self, op: &str, input: &str) -> Result<String, String> {
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
        let job = Self::next_job(&mut held(&self.jobs), "scan");
        let (writing, truth) = (Arc::clone(&state), Arc::clone(&state));
        let (done, stop) = (Arc::clone(&finished), Arc::clone(&stopped));
        let worker = cancellation.clone();
        std::thread::Builder::new()
            .name(format!("disk-usage-{job}"))
            .spawn(move || {
                let result = scan::scan(path, &worker, &|bytes, files| {
                    let mut seen = held(&writing);
                    seen.bytes = bytes;
                    seen.files = files;
                });
                let mut seen = held(&truth);
                if let Some(error) = &result.error {
                    seen.failure = Some(error.clone());
                }
                seen.result = Some(result);
                done.store(true, Ordering::Relaxed);
                stop.store(worker.is_cancelled(), Ordering::Relaxed);
            })
            .map_err(|error| format!("the scan starts no thread: {error}"))?;
        held(&self.jobs).running.insert(
            job.clone(),
            Job {
                path: ask.path,
                scan: state,
                cancellation,
                finished,
                stopped,
            },
        );
        Ok(json!({ "job": job }).to_string())
    }

    fn status(&self, job: &str) -> Result<String, String> {
        let mut jobs = held(&self.jobs);
        let Some(scan) = jobs.running.get(job) else {
            return Err(format!("no job `{job}` runs on this device"));
        };
        let seen = held(&scan.scan);
        if !scan.finished.load(Ordering::Relaxed) {
            return Ok(json!({
                "state": "running",
                "path": scan.path,
                "bytes": seen.bytes,
                "files": seen.files,
                "total": 0,
                "unreadable": 0,
                "error": "",
            })
            .to_string());
        }
        let state = if scan.stopped.load(Ordering::Relaxed) {
            "cancelled"
        } else if seen.failure.is_some() {
            "failed"
        } else {
            "done"
        };
        let answer = json!({
            "state": state,
            "path": scan.path,
            "bytes": seen.bytes,
            "files": seen.files,
            "total": seen.result.as_ref().map_or(0, |result| result.total),
            "unreadable": seen.result.as_ref().map_or(0, |result| result.unreadable),
            "error": seen.failure.clone().unwrap_or_default(),
        })
        .to_string();
        drop(seen);
        if state != "done" {
            jobs.running.remove(job);
        }
        Ok(answer)
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
        true
    }
}

/// The path the ask names, its `~` read as the person's home folder.
fn expand(path: &str) -> PathBuf {
    if path == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
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

    /// A job answers JSON the package reads, and a folder that is not there fails rather than
    /// answering nothing.
    #[test]
    fn a_scan_answers_the_folders_largest_first_and_a_missing_one_fails() {
        let engine = DiskUsage::default();
        let ask = json!({ "path": "/no/such/folder" }).to_string();
        let started: serde_json::Value =
            serde_json::from_str(&engine.start("scan", &ask).expect("the job starts")).unwrap();
        let job = started["job"].as_str().expect("a job id");
        let answer = loop {
            let said: serde_json::Value =
                serde_json::from_str(&engine.status(job).expect("the job answers")).unwrap();
            if said["state"] == "failed" || said["state"] == "cancelled" {
                break said;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(answer["state"], "failed");
        assert!(
            answer["error"].as_str().is_some_and(|said| !said.is_empty()),
            "a folder that is not there says why: {answer}"
        );
        assert!(engine.status(job).is_err(), "a finished job is let go");
    }
}
