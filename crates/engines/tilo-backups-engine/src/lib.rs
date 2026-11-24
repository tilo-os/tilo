//! The Backups app's engine: the snapshots a person's device keeps of a folder, verified and
//! restored, and the retention policy that prunes them.
//!
//! The app runs sandboxed with no access to the file system, so it asks here by name. It
//! names the folder to protect and the folder its snapshots are written to; everything a
//! snapshot holds — the staging directory, the checksums, the manifest and the atomic rename
//! into place — is this crate's.
//!
//! A backup, a verification and a restore are jobs: a folder can hold millions of files and a
//! person must be able to stop the work. A stopped job removes whatever it was writing, so
//! the repository never holds half a snapshot.
//!
//! Paths and credentials never cross the boundary as values the workspace holds: the app
//! keeps the chosen folders in its own record and hands them back when it asks for work.

pub mod model;
mod repository;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

pub use model::{
    Catalog, DestKind, Destination, Health, MAX_KEPT, Retention, RetentionPolicy, Schedule,
    SnapFile, SnapStatus, SnapType, Snapshot, Source, SourceKind,
};
pub use repository::{
    BackupError, BackupManifest, Progress, Stop, Verification, catalog_from_local, create_snapshot,
    list_snapshots, prune_snapshots, restore_snapshot, retained, retained_of, validate_paths,
    verify_snapshot,
};

/// The one app this engine answers for.
pub const APP: &str = "so.tilo.backups";

/// What the app asks the device to do, read from JSON it wrote.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Ask {
    /// The folder this device protects.
    #[serde(default)]
    pub source: String,
    /// The folder snapshots are written to.
    #[serde(default)]
    pub destination: String,
    /// The snapshot a verification or a restore reads.
    #[serde(default)]
    pub snapshot: String,
    /// The folder a restore writes into.
    #[serde(default)]
    pub target: String,
    /// Whether a restore may replace what the target already holds.
    #[serde(default)]
    pub overwrite: bool,
    /// How many snapshots of each class a backup keeps.
    #[serde(default)]
    pub policy_hourly: u32,
    #[serde(default)]
    pub policy_daily: u32,
    #[serde(default)]
    pub policy_weekly: u32,
    #[serde(default)]
    pub policy_monthly: u32,
    /// The stored snapshots the retention page asks about, newest first.
    #[serde(default)]
    pub stored: Vec<StoredSnapshot>,
}

impl Ask {
    /// The retention policy the counts name.
    #[must_use]
    pub fn policy(&self) -> RetentionPolicy {
        RetentionPolicy {
            hourly: self.policy_hourly,
            daily: self.policy_daily,
            weekly: self.policy_weekly,
            monthly: self.policy_monthly,
        }
    }
}

/// One stored snapshot as the retention page knows it: its id and when it was taken.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct StoredSnapshot {
    pub id: String,
    pub seconds: u64,
}

/// What a job or a call answers with.
struct Job {
    cancel: Arc<AtomicBool>,
    answer: Mutex<Option<Result<serde_json::Value, String>>>,
    progress: Mutex<String>,
    cancelled: AtomicBool,
}

fn jobs() -> &'static Mutex<std::collections::BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<std::collections::BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

fn job_counter() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        });
    // The moment and the count together: unique on a device across restarts.
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    at.saturating_mul(1000).saturating_add(count)
}

fn fresh(n: u64) -> String {
    format!("backup-{n}")
}

/// The folder a path names, refusing one the app left out.
fn folder(path: &str, what: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err(format!("Choose {what} first"));
    }
    Ok(PathBuf::from(path))
}

/// The device's repository as the app reads it: one source, its snapshots, one destination.
///
/// # Errors
///
/// The folders do not read, or a stored snapshot's metadata is invalid.
pub fn catalog(ask: &Ask) -> Result<Catalog, String> {
    if ask.source.trim().is_empty() || ask.destination.trim().is_empty() {
        return Ok(Catalog::default());
    }
    let destination = folder(&ask.destination, "a backup destination")?;
    let manifests = list_snapshots(&destination).map_err(|error| error.to_string())?;
    Ok(catalog_from_local(
        &PathBuf::from(&ask.source),
        &destination,
        &manifests,
    ))
}

/// A line a person reads while the job runs.
fn saying(progress: &Progress) -> String {
    match progress.of {
        Some(of) => format!(
            "{} of {} files, {}",
            progress.files,
            of,
            size_text(progress.bytes)
        ),
        None => format!("{} files, {}", progress.files, size_text(progress.bytes)),
    }
}

/// A byte count in the platform's unit for stored data (SI).
#[allow(
    clippy::cast_precision_loss,
    reason = "a size shown to a person, where an exact count is what the caption adds"
)]
fn size_text(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000 * KB;
    const GB: u64 = 1_000 * MB;
    const TB: u64 = 1_000 * GB;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} kB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes < TB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.1} TB", bytes as f64 / TB as f64)
    }
}

/// One `engine.call`: what is answered now. `catalog` reads the repository; `retained` says
/// what a policy keeps without removing anything.
///
/// # Errors
///
/// The op is unknown, the input does not read, or the repository refused.
pub fn call(op: &str, input: &str) -> Result<String, String> {
    let ask: Ask = serde_json::from_str(input)
        .map_err(|error| format!("the request does not read: {error}"))?;
    match op {
        "catalog" => serde_json::to_string(&catalog(&ask)?)
            .map_err(|error| format!("the catalog does not write: {error}")),
        "retained" => {
            ask.policy().valid()?;
            let stored: Vec<(String, u64)> = ask
                .stored
                .iter()
                .map(|snapshot| (snapshot.id.clone(), snapshot.seconds))
                .collect();
            let kept_set = retained_of(&stored, &ask.policy());
            let mut kept: Vec<&String> = kept_set.iter().collect();
            kept.sort();
            serde_json::to_string(&kept)
                .map_err(|error| format!("the answer does not write: {error}"))
        }
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    }
}

/// One `engine.start`: a backup, a verification or a restore, answered with the job's id.
///
/// # Errors
///
/// The op is unknown, the request does not read, the paths it names are unsafe, or the
/// device could not start the work.
pub fn start(op: &str, input: &str) -> Result<String, String> {
    let ask: Ask = serde_json::from_str(input)
        .map_err(|error| format!("the request does not read: {error}"))?;
    let job = Arc::new(Job {
        cancel: Arc::new(AtomicBool::new(false)),
        answer: Mutex::new(None),
        progress: Mutex::new(String::new()),
        cancelled: AtomicBool::new(false),
    });
    let kind = op.to_owned();
    let work = Arc::clone(&job);
    let run = move |progress: &Progress| {
        if let Ok(mut line) = work.progress.lock() {
            *line = saying(progress);
        }
    };
    let worker = match op {
        "backup" => {
            let source = folder(&ask.source, "a folder to protect")?;
            let destination = folder(&ask.destination, "a backup destination")?;
            ask.policy().valid()?;
            let policy = ask.policy();
            let work = Arc::clone(&job);
            std::thread::Builder::new()
                .name("backups-engine backup".to_owned())
                .spawn(move || {
                    let cancel = Arc::clone(&work.cancel);
                    let asked = || cancel.load(Ordering::Acquire);
                    let stop = Stop::on(&asked);
                    let answer = create_snapshot(&source, &destination, stop, run)
                        .and_then(|_| prune_snapshots(&destination, &policy))
                        .and_then(|_| {
                            let manifests = list_snapshots(&destination)?;
                            Ok(catalog_from_local(&source, &destination, &manifests))
                        })
                        .map_err(|error| error.to_string())
                        .and_then(|catalog| {
                            serde_json::to_value(catalog)
                                .map_err(|error| format!("the catalog does not write: {error}"))
                        });
                    finish(&work, answer);
                })
                .map_err(|error| format!("this device could not start the backup: {error}"))?
        }
        "verify" => {
            let destination = folder(&ask.destination, "a backup destination")?;
            if ask.snapshot.trim().is_empty() {
                return Err("Choose a snapshot to verify".to_owned());
            }
            let id = ask.snapshot;
            let work = Arc::clone(&job);
            std::thread::Builder::new()
                .name("backups-engine verify".to_owned())
                .spawn(move || {
                    let cancel = Arc::clone(&work.cancel);
                    let asked = || cancel.load(Ordering::Acquire);
                    let stop = Stop::on(&asked);
                    let answer = verify_snapshot(&destination, &id, stop, run)
                        .map_err(|error| error.to_string())
                        .and_then(|checked| {
                            serde_json::to_value(Checked {
                                files: checked.files,
                                bytes: checked.bytes,
                            })
                            .map_err(|error| format!("the answer does not write: {error}"))
                        });
                    finish(&work, answer);
                })
                .map_err(|error| format!("this device could not start the check: {error}"))?
        }
        "restore" => {
            let destination = folder(&ask.destination, "a backup destination")?;
            if ask.snapshot.trim().is_empty() {
                return Err("Choose a snapshot to restore".to_owned());
            }
            let target = folder(&ask.target, "a folder to restore into")?;
            let id = ask.snapshot;
            let overwrite = ask.overwrite;
            let work = Arc::clone(&job);
            std::thread::Builder::new()
                .name("backups-engine restore".to_owned())
                .spawn(move || {
                    let cancel = Arc::clone(&work.cancel);
                    let asked = || cancel.load(Ordering::Acquire);
                    let stop = Stop::on(&asked);
                    let answer = restore_snapshot(&destination, &id, &target, overwrite, stop, run)
                        .map_err(|error| error.to_string())
                        .and_then(|restored| {
                            serde_json::to_value(Checked {
                                files: restored.files,
                                bytes: restored.bytes,
                            })
                            .map_err(|error| format!("the answer does not write: {error}"))
                        });
                    finish(&work, answer);
                })
                .map_err(|error| format!("this device could not start the restore: {error}"))?
        }
        other => return Err(format!("`{other}` is no op this device runs for {APP}")),
    };
    let _ = worker;
    let id = fresh(job_counter());
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.clone(), job);
    Ok(id)
}

/// What a verification or a restore checked.
#[derive(Clone, Copy, Debug, Serialize)]
struct Checked {
    files: u64,
    bytes: u64,
}

/// Keeps what the job answered, and notes that a stopped job stopped.
fn finish(job: &Job, answer: Result<serde_json::Value, String>) {
    if job.cancel.load(Ordering::Acquire) {
        job.cancelled.store(true, Ordering::Release);
    }
    if let Ok(mut slot) = job.answer.lock() {
        *slot = Some(answer);
    }
}

/// How far the job `job` has got, as JSON: its state, its line, and what it answered.
///
/// # Errors
///
/// No such job on this device.
pub fn status(job: &str) -> Result<String, String> {
    let found = jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .get(job)
        .cloned();
    let Some(job) = found else {
        return Err(format!("no job `{job}` runs on this device"));
    };
    let progress = job
        .progress
        .lock()
        .map_or_else(|_| String::new(), |line| line.clone());
    if job.cancelled.load(Ordering::Acquire) {
        return Ok(serde_json::json!({
            "state": "cancelled",
            "progress": progress,
            "answer": "",
            "error": "The work was stopped.",
        })
        .to_string());
    }
    let answer = job
        .answer
        .lock()
        .map_err(|_| "the job is unreadable".to_owned())?;
    match answer.as_ref() {
        None => Ok(serde_json::json!({
            "state": "running",
            "progress": progress,
            "answer": "",
            "error": "",
        })
        .to_string()),
        Some(Ok(value)) => Ok(serde_json::json!({
            "state": "done",
            "progress": progress,
            "answer": value.to_string(),
            "error": "",
        })
        .to_string()),
        Some(Err(error)) => Ok(serde_json::json!({
            "state": "failed",
            "progress": progress,
            "answer": "",
            "error": error,
        })
        .to_string()),
    }
}

/// Stop the job `job`: false when it is not this device's or it had finished.
pub fn cancel(job: &str) -> bool {
    let Ok(known) = jobs().lock() else {
        return false;
    };
    let Some(state) = known.get(job) else {
        return false;
    };
    if state.answer.lock().is_ok_and(|answer| answer.is_some()) {
        return false;
    }
    state.cancel.store(true, Ordering::Release);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_is_unknown_until_it_starts_and_a_bad_op_is_refused() {
        assert!(status("no-such-job").is_err());
        assert!(!cancel("no-such-job"));
        assert!(start("paint", "{}").is_err());
        assert!(start("backup", "not json").is_err());
        assert!(call("paint", "{}").is_err());
    }

    #[test]
    fn a_backup_without_folders_is_refused_and_an_empty_one_answers_nothing() {
        assert!(start("backup", r#"{"source": "", "destination": ""}"#).is_err());
        let empty = catalog(&Ask {
            source: String::new(),
            destination: String::new(),
            snapshot: String::new(),
            target: String::new(),
            overwrite: false,
            policy_hourly: 24,
            policy_daily: 7,
            policy_weekly: 4,
            policy_monthly: 12,
            stored: Vec::new(),
        })
        .unwrap();
        assert_eq!(empty, Catalog::default());
    }

    #[test]
    fn retention_keeps_a_snapshot_of_each_recent_period() {
        let stored: Vec<StoredSnapshot> = (0..6)
            .map(|n| StoredSnapshot {
                id: format!("s{n}"),
                seconds: 1_700_000_000 + n * 3_600,
            })
            .collect();
        let answer = call(
            "retained",
            &serde_json::json!({
                "policy_hourly": 2, "policy_daily": 1, "policy_weekly": 0, "policy_monthly": 0,
                "stored": stored,
            })
            .to_string(),
        )
        .unwrap();
        let kept: Vec<String> = serde_json::from_str(&answer).unwrap();
        assert!(kept.contains(&"s5".to_owned()), "the newest stays");
        assert!(kept.len() >= 2);
    }

    #[test]
    fn a_policy_beyond_the_kept_bound_is_refused() {
        let ask = Ask {
            source: String::new(),
            destination: String::new(),
            snapshot: String::new(),
            target: String::new(),
            overwrite: false,
            policy_hourly: MAX_KEPT + 1,
            policy_daily: 7,
            policy_weekly: 4,
            policy_monthly: 12,
            stored: Vec::new(),
        };
        assert!(ask.policy().valid().is_err());
    }

    #[test]
    fn a_size_reads_in_the_platforms_units() {
        assert_eq!(size_text(999), "999 B");
        assert_eq!(size_text(1_500), "1.5 kB");
        assert_eq!(size_text(2_500_000), "2.5 MB");
    }
}
