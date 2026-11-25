//! The Files app's engine work, as a package reaches it: `engine.start("transfer")` answered for
//! `so.tilo.file-explorer` by copying or moving entries into a folder on this device, and
//! `engine.call("facts")` by what a file's own header says it is: how long a recording plays, how
//! many pages a document has, and its codec and container.
//!
//! A transfer is long work — a folder of photos onto a slow drive — so it is a job: it says how
//! many bytes of how many it has written, which entry it is on, and it can be stopped, which
//! takes away the entry it was half way through so nothing half-copied is left behind. A name
//! the folder already holds is kept both ("photo (2).jpg"), so a transfer never replaces a file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::Deserialize;

use tilolang_tilo::engine::{JobOp, JobSink, Needs, State};

use crate::files::{STOPPED, copy_tree_told, remove_tree, resolve};

/// The app this engine answers for.
pub const APP: &str = "so.tilo.file-explorer";

/// Registers the engine for the Files app, once per process.
pub fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| tilolang_tilo::engine::register(APP, Arc::new(Transfers)));
}

/// The registry's entry for the Files app.
struct Transfers;

impl tilolang_tilo::engine::Provider for Transfers {
    /// A transfer reads and writes the files of the device the app's window is on, so it runs in
    /// the app's own process.
    fn jobs(&self) -> &'static [JobOp] {
        const JOBS: &[JobOp] = &[JobOp::new("transfer", Needs::APP_STATE)];
        JOBS
    }

    fn call(&self, op: &str, input: &str) -> Result<String, String> {
        match op {
            "facts" => facts(&resolve(input)),
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        match op {
            "transfer" => {
                let ask: Ask = serde_json::from_str(input).map_err(|error| error.to_string())?;
                transfer(job, ask, moved)
            }
            other => Err(format!("`{other}` is no op this device runs for {APP}")),
        }
    }

    fn cancel(&self, job: &str) -> bool {
        cancel(job)
    }
}

/// What the file at `path` states in its header, as JSON: `width`, `height`, `duration` in milliseconds, `pages`, `codec` and
/// `container`, each `0` or empty when it states none. A file too long to read whole is read at
/// its start and its end, where a recording often keeps its index.
fn facts(path: &Path) -> Result<String, String> {
    use std::io::{Read, Seek, SeekFrom};
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut found = tilo_content::MediaFacts::default();
    if tilo_content::has_facts(&name) {
        let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
        let len = file.metadata().map_err(|error| error.to_string())?.len();
        let scan = tilo_content::FACTS_SCAN_BYTES as u64;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(scan)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if len > scan.saturating_mul(2) {
            let back = i64::try_from(scan)
                .ok()
                .and_then(i64::checked_neg)
                .unwrap_or(i64::MIN);
            file.seek(SeekFrom::End(back))
                .map_err(|error| error.to_string())?;
            file.read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
        } else {
            file.read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
        }
        found = tilo_content::facts(&name, &bytes).unwrap_or_default();
    }
    Ok(serde_json::json!({
        "width": found.width.unwrap_or(0),
        "height": found.height.unwrap_or(0),
        "duration": found.duration_ms.unwrap_or(0),
        "pages": found.page_count.unwrap_or(0),
        "codec": found.codec.unwrap_or_default(),
        "container": found.container.unwrap_or_default(),
    })
    .to_string())
}

/// What a transfer is asked to do: put the entries at `paths` into `folder`, moving them or
/// copying them.
#[derive(Debug, Deserialize)]
struct Ask {
    paths: Vec<String>,
    folder: String,
    #[serde(default)]
    moving: bool,
}

/// One transfer in flight, and how far it has got.
#[derive(Default)]
struct Job {
    finished: AtomicBool,
    /// Asked to stop, and whether it stopped before it put every entry in place.
    stopped: AtomicBool,
    cancelled: AtomicBool,
    /// Bytes written, and bytes the whole transfer writes.
    done: AtomicU64,
    total: AtomicU64,
    /// Entries put in place, of how many.
    items: AtomicUsize,
    count: usize,
    /// The name of the entry it is on.
    current: Mutex<String>,
    error: Mutex<Option<String>>,
}

fn jobs() -> &'static Mutex<BTreeMap<String, Arc<Job>>> {
    static JOBS: OnceLock<Mutex<BTreeMap<String, Arc<Job>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// How often a transfer tells how far it got while it writes.
const TELL_EVERY: std::time::Duration = std::time::Duration::from_millis(200);

/// Starts the transfer `id` on a thread of its own, telling `moved` how far it got as it writes
/// and how it ended.
fn transfer(id: &str, ask: Ask, moved: JobSink) -> Result<(), String> {
    if ask.paths.is_empty() {
        return Err("there is nothing to put there".to_owned());
    }
    let folder = resolve(&ask.folder);
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    let id = id.to_owned();
    let job = Arc::new(Job {
        count: ask.paths.len(),
        ..Job::default()
    });
    jobs()
        .lock()
        .map_err(|_| "the job table is unreadable".to_owned())?
        .insert(id, Arc::clone(&job));
    std::thread::Builder::new()
        .name("files-transfer".to_owned())
        .spawn(move || {
            let sources: Vec<PathBuf> = ask.paths.iter().map(|path| resolve(path)).collect();
            job.total.store(
                sources.iter().map(|path| size_of(path)).sum(),
                Ordering::Relaxed,
            );
            let told = std::cell::Cell::new(std::time::Instant::now());
            let tell = || {
                if told.get().elapsed() >= TELL_EVERY {
                    told.set(std::time::Instant::now());
                    moved.moved(State::Running, said(&job).1);
                }
            };
            moved.moved(State::Running, said(&job).1);
            for from in &sources {
                if job.stopped.load(Ordering::Relaxed) {
                    job.cancelled.store(true, Ordering::Relaxed);
                    break;
                }
                if let Err(error) = put(&job, from, &folder, ask.moving, &tell) {
                    if error == STOPPED {
                        job.cancelled.store(true, Ordering::Relaxed);
                    } else if let Ok(mut slot) = job.error.lock() {
                        *slot = Some(error);
                    }
                    break;
                }
                job.items.fetch_add(1, Ordering::Relaxed);
            }
            job.finished.store(true, Ordering::Relaxed);
            let (state, detail) = said(&job);
            moved.moved(state, detail);
        })
        .map_err(|error| format!("the transfer did not start: {error}"))?;
    Ok(())
}

/// Puts one entry into `folder` under a name the folder does not hold yet.
fn put(job: &Job, from: &Path, folder: &Path, moving: bool, tell: &dyn Fn()) -> Result<(), String> {
    let name = from
        .file_name()
        .ok_or_else(|| format!("{} has no name", from.display()))?;
    if let Ok(mut current) = job.current.lock() {
        *current = name.to_string_lossy().into_owned();
    }
    if from.is_dir() && folder.starts_with(from) {
        return Err(format!("{} cannot go into itself", from.display()));
    }
    let to = free_path(folder, &name.to_string_lossy());
    // A move on one disk is a rename, at once whatever it holds.
    if moving && std::fs::rename(from, &to).is_ok() {
        job.done.fetch_add(size_of(&to), Ordering::Relaxed);
        return Ok(());
    }
    let copied = copy_tree_told(from, &to, &mut |bytes| {
        job.done.fetch_add(bytes, Ordering::Relaxed);
        tell();
        !job.stopped.load(Ordering::Relaxed)
    });
    if let Err(error) = copied {
        // What a stopped or failed copy wrote is taken away: the folder holds the entry whole
        // or not at all.
        if to.symlink_metadata().is_ok() {
            let _ = remove_tree(&to);
        }
        return Err(error);
    }
    if moving {
        // Across disks a move is a copy, and the original goes once the copy is whole.
        remove_tree(from)?;
    }
    Ok(())
}

/// `name` in `folder`, or the first of "name (2).ext", "name (3).ext"… the folder does not hold.
fn free_path(folder: &Path, name: &str) -> PathBuf {
    let taken = |path: &Path| path.symlink_metadata().is_ok();
    let first = folder.join(name);
    if !taken(&first) {
        return first;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (name, String::new()),
    };
    (2..u32::MAX)
        .map(|n| folder.join(format!("{stem} ({n}){extension}")))
        .find(|path| !taken(path))
        .unwrap_or(first)
}

/// How many bytes the entry at `path` holds, folders and all; links count as nothing.
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = path.symlink_metadata() else {
        return 0;
    };
    if meta.is_dir() {
        std::fs::read_dir(path).map_or(0, |items| {
            items
                .filter_map(Result::ok)
                .map(|item| size_of(&item.path()))
                .sum()
        })
    } else if meta.is_file() {
        meta.len()
    } else {
        0
    }
}

/// How far a transfer has got: its state, and as its fields the bytes and entries done of how
/// many, the entry it is on, and what failed.
fn said(job: &Job) -> (State, serde_json::Value) {
    let error = job.error.lock().ok().and_then(|error| error.clone());
    let finished = job.finished.load(Ordering::Relaxed);
    let (state, error) = if let Some(error) = error {
        (State::Failed, error)
    } else if finished && job.cancelled.load(Ordering::Relaxed) {
        (State::Cancelled, String::new())
    } else if finished {
        (State::Done, String::new())
    } else {
        (State::Running, String::new())
    };
    let current = job
        .current
        .lock()
        .map(|current| current.clone())
        .unwrap_or_default();
    (
        state,
        serde_json::json!({
            "done": job.done.load(Ordering::Relaxed),
            "total": job.total.load(Ordering::Relaxed),
            "items": job.items.load(Ordering::Relaxed),
            "count": job.count,
            "current": current,
            "error": error,
        }),
    )
}

/// Stops a transfer between chunks: the entry it was on is taken away. False when it is not
/// this device's or it had finished.
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
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transfer started, and what it tells as it moves: its job, and its last move once it
    /// ended.
    struct Started {
        job: String,
        ended: std::sync::mpsc::Receiver<serde_json::Value>,
    }

    /// Waits for the transfer to end, answering its last move: its state beside its fields.
    fn finished(started: &Started) -> serde_json::Value {
        started
            .ended
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("the transfer finishes")
    }

    #[test]
    fn a_documents_pages_are_what_its_header_states() {
        let dir = tempfile::tempdir().expect("a folder");
        let pdf = dir.path().join("plan.pdf");
        std::fs::write(
            &pdf,
            b"%PDF-1.7 /Type /Pages /Type /Page /Type /Page /Type /Page\n",
        )
        .expect("a file");
        let said: serde_json::Value =
            serde_json::from_str(&facts(&pdf).expect("it reads")).expect("json");
        assert_eq!(said["pages"], 3);
        assert_eq!(said["container"], "pdf");
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "notes").expect("a file");
        let said: serde_json::Value =
            serde_json::from_str(&facts(&plain).expect("it reads")).expect("json");
        assert_eq!(
            (said["pages"].clone(), said["codec"].clone()),
            (0.into(), "".into())
        );
    }

    fn started(paths: &[&Path], folder: &Path, moving: bool) -> Started {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let job = format!("transfer-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let ask = Ask {
            paths: paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect(),
            folder: folder.to_string_lossy().into_owned(),
            moving,
        };
        let (tell, ended) = std::sync::mpsc::channel();
        let tell = Mutex::new(tell);
        let moved = JobSink::new(move |state, mut detail| {
            if state.ended() {
                detail["state"] = state.word().into();
                let _ = tell.lock().unwrap().send(detail);
            }
        });
        transfer(&job, ask, moved).expect("it starts");
        Started { job, ended }
    }

    #[test]
    fn a_copy_says_how_far_it_got_and_keeps_a_taken_name_both() {
        let dir = tempfile::tempdir().expect("a folder");
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::create_dir_all(from.join("album")).expect("folders");
        std::fs::create_dir_all(&to).expect("folders");
        std::fs::write(from.join("album/a.jpg"), vec![1_u8; 3 << 20]).expect("a picture");
        std::fs::write(from.join("notes.txt"), b"notes").expect("notes");
        std::fs::write(to.join("notes.txt"), b"older notes").expect("older notes");

        let state = finished(&started(
            &[&from.join("album"), &from.join("notes.txt")],
            &to,
            false,
        ));
        assert_eq!(state["state"], "done", "{state}");
        assert_eq!(state["total"], (3 << 20) + 5);
        assert_eq!(state["done"], state["total"], "every byte is counted");
        assert_eq!(
            (state["items"].clone(), state["count"].clone()),
            (2.into(), 2.into())
        );
        assert_eq!(
            std::fs::read(to.join("album/a.jpg"))
                .expect("the copy")
                .len(),
            3 << 20
        );
        assert_eq!(
            std::fs::read(to.join("notes.txt")).expect("kept"),
            b"older notes"
        );
        assert_eq!(
            std::fs::read(to.join("notes (2).txt")).expect("the copy"),
            b"notes"
        );
        assert!(
            from.join("notes.txt").exists(),
            "a copy leaves the original"
        );
    }

    #[test]
    fn a_move_leaves_nothing_behind() {
        let dir = tempfile::tempdir().expect("a folder");
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::create_dir_all(&from).expect("folders");
        std::fs::create_dir_all(&to).expect("folders");
        std::fs::write(from.join("report.pdf"), b"report").expect("a file");

        let state = finished(&started(&[&from.join("report.pdf")], &to, true));
        assert_eq!(state["state"], "done", "{state}");
        assert_eq!(state["done"], 6);
        assert!(to.join("report.pdf").exists() && !from.join("report.pdf").exists());
    }

    #[test]
    fn a_folder_does_not_go_into_itself() {
        let dir = tempfile::tempdir().expect("a folder");
        let album = dir.path().join("album");
        std::fs::create_dir_all(album.join("inner")).expect("folders");

        let state = finished(&started(&[&album], &album.join("inner"), true));
        assert_eq!(state["state"], "failed", "{state}");
        assert!(album.join("inner").is_dir(), "nothing moved");
    }

    #[test]
    fn a_stopped_copy_leaves_no_half_written_file() {
        let dir = tempfile::tempdir().expect("a folder");
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::create_dir_all(&from).expect("folders");
        std::fs::create_dir_all(&to).expect("folders");
        std::fs::write(from.join("film.mkv"), vec![7_u8; 64 << 20]).expect("a film");

        let job = started(&[&from.join("film.mkv")], &to, false);
        // Stopped before it wrote the whole film, or after: either way the folder holds the
        // film whole or not at all.
        cancel(&job.job);
        let state = finished(&job);
        if state["state"] == "cancelled" {
            assert!(!to.join("film.mkv").exists(), "the half copy is taken away");
        } else {
            assert_eq!(state["state"], "done", "{state}");
            assert_eq!(
                std::fs::read(to.join("film.mkv")).expect("whole").len(),
                64 << 20
            );
        }
        assert_eq!(
            std::fs::read(from.join("film.mkv")).expect("kept").len(),
            64 << 20
        );
        assert!(!cancel(&job.job), "a finished transfer stops no more");
    }

    #[test]
    fn an_unknown_job_is_refused() {
        assert!(!cancel("transfer-none"));
    }
}
