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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

pub use model::{
    Catalog, DestKind, Destination, Health, MAX_KEPT, Retention, RetentionPolicy, Schedule,
    SnapFile, SnapStatus, SnapType, Snapshot, Source, SourceKind, Weekday,
};
pub use repository::{
    BackupError, BackupManifest, Progress, Stop, Verification, catalog_from_local, create_snapshot,
    list_snapshots, prune_snapshots, restore_snapshot, retained, retained_of, validate_paths,
    verify_snapshot,
};

/// The one app this engine answers for.
pub const APP: &str = "so.tilo.backups";

/// What the app asks the device to do, read from JSON it wrote.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
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
    /// The schedule the app sets: its kind (`on_demand`, `hourly`, `daily`, `weekly`), the time
    /// of day it runs at as `18:00`, and the day a weekly one runs on (Monday is zero).
    #[serde(default)]
    pub schedule_kind: String,
    #[serde(default)]
    pub schedule_at: String,
    #[serde(default)]
    pub schedule_weekday: u8,
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
    /// Where each move of the job is told: its state, its line, and what it answered.
    moved: Moved,
}

/// Where a job tells each move it makes: the state's word (`running`, `done`, `failed`,
/// `cancelled`) and the job's own fields.
pub type Moved = Arc<dyn Fn(&str, serde_json::Value) + Send + Sync>;

fn jobs() -> &'static Mutex<std::collections::BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<std::collections::BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(std::collections::BTreeMap::new()))
}

/// The folder a path names, refusing one the app left out.
fn folder(path: &str, what: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err(format!("Choose {what} first"));
    }
    Ok(PathBuf::from(path))
}

/// The hour and minute `at` writes (`18:00`), or nothing when the schedule runs by hand.
fn clock(at: &str) -> Result<(u32, u32), String> {
    if at.trim().is_empty() {
        return Ok((0, 0));
    }
    let (hour, minute) = at
        .split_once(':')
        .ok_or_else(|| format!("`{at}` is no time; write it like 18:00"))?;
    let hour: u32 = hour
        .trim()
        .parse()
        .map_err(|_| format!("`{at}` is no time; write it like 18:00"))?;
    let minute: u32 = minute
        .trim()
        .parse()
        .map_err(|_| format!("`{at}` is no time; write it like 18:00"))?;
    if hour > 23 || minute > 59 {
        return Err(format!("`{at}` is no time; write it like 18:00"));
    }
    Ok((hour, minute))
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

/// One `engine.call`: what is answered now. `catalog` reads the repository; `schedule` keeps
/// when the source backs itself up; `retained` says what a policy keeps without removing
/// anything.
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
        "schedule" => {
            if ask.destination.trim().is_empty() {
                return Err("Choose where snapshots go first".to_owned());
            }
            let destination = folder(&ask.destination, "a backup destination")?;
            let (hour, minute) = clock(&ask.schedule_at)?;
            let schedule = Schedule::from_parts(
                &ask.schedule_kind,
                hour,
                minute,
                Weekday::from_number(ask.schedule_weekday),
            );
            repository::write_schedule(&destination, schedule)
                .map_err(|error| error.to_string())?;
            serde_json::to_string(&catalog(&ask)?)
                .map_err(|error| format!("the catalog does not write: {error}"))
        }
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

/// One `engine.start`: a backup, a verification or a restore, run as the job `id`, which tells
/// each move it makes to `moved`.
///
/// # Errors
///
/// The op is unknown, the request does not read, the paths it names are unsafe, or the
/// device could not start the work.
pub fn start(id: &str, op: &str, input: &str, moved: Moved) -> Result<(), String> {
    let ask: Ask = serde_json::from_str(input)
        .map_err(|error| format!("the request does not read: {error}"))?;
    let job = Arc::new(Job {
        cancel: Arc::new(AtomicBool::new(false)),
        answer: Mutex::new(None),
        progress: Mutex::new(String::new()),
        cancelled: AtomicBool::new(false),
        moved,
    });
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id.to_owned(), Arc::clone(&job));
    let run = progress_writer(&job);
    tell(&job);
    let started = match op {
        "backup" => start_backup(&job, &ask, run),
        "verify" => start_verify(&job, ask, run),
        "restore" => start_restore(&job, ask, run),
        other => Err(format!("`{other}` is no op this device runs for {APP}")),
    };
    if let Err(why) = started {
        if let Ok(mut known) = jobs().lock() {
            known.remove(id);
        }
        return Err(why);
    }
    Ok(())
}

/// A job's progress line, written where the job reports it, and told.
fn progress_writer(job: &Arc<Job>) -> impl Fn(&Progress) + Send + 'static {
    let work = Arc::clone(job);
    move |progress: &Progress| {
        if let Ok(mut line) = work.progress.lock() {
            *line = saying(progress);
        }
        tell(&work);
    }
}

/// Start the backup's thread: the snapshot, the prune under the ask's policy, and the catalog
/// the destination then holds.
fn start_backup(
    job: &Arc<Job>,
    ask: &Ask,
    run: impl Fn(&Progress) + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, String> {
    let source = folder(&ask.source, "a folder to protect")?;
    let destination = folder(&ask.destination, "a backup destination")?;
    ask.policy().valid()?;
    let policy = ask.policy();
    let work = Arc::clone(job);
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
        .map_err(|error| format!("this device could not start the backup: {error}"))
}

/// Start the verification's thread: every file the snapshot holds, read and checked.
fn start_verify(
    job: &Arc<Job>,
    ask: Ask,
    run: impl Fn(&Progress) + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, String> {
    let destination = folder(&ask.destination, "a backup destination")?;
    if ask.snapshot.trim().is_empty() {
        return Err("Choose a snapshot to verify".to_owned());
    }
    let id = ask.snapshot;
    let work = Arc::clone(job);
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
        .map_err(|error| format!("this device could not start the check: {error}"))
}

/// Start the restore's thread: the snapshot's files, written into the folder it names.
fn start_restore(
    job: &Arc<Job>,
    ask: Ask,
    run: impl Fn(&Progress) + Send + 'static,
) -> Result<std::thread::JoinHandle<()>, String> {
    let destination = folder(&ask.destination, "a backup destination")?;
    if ask.snapshot.trim().is_empty() {
        return Err("Choose a snapshot to restore".to_owned());
    }
    let target = folder(&ask.target, "a folder to restore into")?;
    let id = ask.snapshot;
    let overwrite = ask.overwrite;
    let work = Arc::clone(job);
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
        .map_err(|error| format!("this device could not start the restore: {error}"))
}

/// What a verification or a restore checked.
#[derive(Clone, Copy, Debug, Serialize)]
struct Checked {
    files: u64,
    bytes: u64,
}

/// Keeps what the job answered, notes that a stopped job stopped, and tells its end.
fn finish(job: &Job, answer: Result<serde_json::Value, String>) {
    if job.cancel.load(Ordering::Acquire) {
        job.cancelled.store(true, Ordering::Release);
    }
    if let Ok(mut slot) = job.answer.lock() {
        *slot = Some(answer);
    }
    tell(job);
    if let Ok(mut known) = jobs().lock() {
        known.retain(|_, kept| !std::ptr::eq(Arc::as_ptr(kept), job));
    }
}

/// Tell how the job stands now: its state, its line, and what it answered.
fn tell(job: &Job) {
    let progress = job
        .progress
        .lock()
        .map_or_else(|_| String::new(), |line| line.clone());
    let (state, answer, error) = if job.cancelled.load(Ordering::Acquire) {
        (
            "cancelled",
            String::new(),
            "The work was stopped.".to_owned(),
        )
    } else {
        match job.answer.lock().ok().and_then(|answer| answer.clone()) {
            None => ("running", String::new(), String::new()),
            Some(Ok(value)) => ("done", value.to_string(), String::new()),
            Some(Err(error)) => ("failed", String::new(), error),
        }
    };
    (job.moved)(
        state,
        serde_json::json!({ "progress": progress, "answer": answer, "why": error }),
    );
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

    fn unheard() -> Moved {
        Arc::new(|_, _| {})
    }

    #[test]
    fn a_job_is_unknown_until_it_starts_and_a_bad_op_is_refused() {
        assert!(!cancel("no-such-job"));
        assert!(start("job-1", "paint", "{}", unheard()).is_err());
        assert!(start("job-2", "backup", "not json", unheard()).is_err());
        assert!(!cancel("job-1"), "a job that did not start is not kept");
        assert!(call("paint", "{}").is_err());
    }

    #[test]
    fn a_backup_without_folders_is_refused_and_an_empty_one_answers_nothing() {
        assert!(
            start(
                "job-3",
                "backup",
                r#"{"source": "", "destination": ""}"#,
                unheard()
            )
            .is_err()
        );
        let empty = catalog(&Ask::default()).unwrap();
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
            policy_hourly: MAX_KEPT + 1,
            ..Ask::default()
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
