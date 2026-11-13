//! Pasting files copied to the clipboard into a folder, on a thread of its own. A paste is not a
//! wish: its copies are staged beside the names they will take (`.tilo-transfer-<id>` inside the
//! receiving folder), synced, written down in a receipt, and only then renamed into place, so a
//! power cut never shows a partly copied file under a real name and a cut never loses its source.
//! Copying runs here, never on the session's loop. The receipt (`paste.json` in the session's state
//! directory, written atomically and synced) holds the whole paste and how far each item got; it is
//! written once per batch, and its removal is the last thing a finished paste does.
//! [`Paste::unfinished`] undoes what a session that stopped left behind: every item ends as either
//! its source, untouched, or its copy, complete.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use host_fs::transfer::{
    Staging, best_effort, best_effort_by, free_name, quarantine_at, quarantine_path, remove_path,
    restore_source, staging_path, sync_directory, sync_parent, trash,
};
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use ui::dnd::CopiedFiles;

/// The most files one paste carries.
const MAX_ITEMS: usize = 4096;
/// The most items one batch stages before their copies are published.
const BATCH_ITEMS: usize = 256;
/// The most names one item's publication tries before it gives up.
const MAX_NAME_TRIES: usize = 4;
/// The environment names a moment of a paste a crash test dies at (see [`crash_point`]).
const CRASH_POINT_ENV: &str = "TILO_FILE_PASTE_CRASH_POINT";
/// The receipt this writes and reads.
const RECEIPT_FILE: &str = "paste.json";
/// The shape of that receipt.
const RECEIPT_VERSION: u32 = 1;

/// What a folder already holding one of the pasted names gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Conflict {
    /// The copy takes the first free numbered name; what is there stays.
    KeepBoth,
    /// The copy is left out.
    Skip,
    /// What is there goes to the Trash and the copy takes its name.
    Replace,
}

impl Conflict {
    /// What the caller's word for it means; `None` for a word this does not know, and for none at
    /// all.
    pub(crate) fn parse(text: Option<&str>) -> Option<Self> {
        match text {
            None | Some("keep-both") => Some(Self::KeepBoth),
            Some("skip") => Some(Self::Skip),
            Some("replace") => Some(Self::Replace),
            Some(_) => None,
        }
    }
}

/// Why a paste did nothing at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// There was nothing to paste: every name is taken, or the files already sit in that folder.
    Nothing,
    /// `path` is not a folder that can hold a paste.
    NotAFolder(PathBuf),
    /// The copy stopped before it could finish, with what the host said.
    Stopped(String),
}

/// The object `path` is, told apart from whatever else takes its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    /// Whether `path` is still that very object.
    fn matches(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok_and(|metadata| Self::of(&metadata) == *self)
    }
}

/// How far one item of a paste got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum State {
    /// Written down; nothing of it has been touched.
    Pending,
    /// Its copy is whole at its staged path, which is this object.
    Staged(Identity),
    /// Its copy stands under its name (a copy, never a cut).
    Published,
    /// Its copy stands under its name and its source is gone.
    Moved,
    /// It could not be pasted; nothing of it is left to undo.
    Failed,
}

/// One file, folder or link of a paste.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Item {
    /// What is pasted, as it was when the paste was asked for.
    source: PathBuf,
    /// The name it takes in the receiving folder.
    target: PathBuf,
    state: State,
    /// Where a cut's source hides while its copy is published: written down before the source is
    /// moved, so a session that stops knows where to look for it.
    #[serde(default)]
    held: Option<PathBuf>,
}

/// A paste, as far as it got.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Receipt {
    version: u32,
    moving: bool,
    conflict: Conflict,
    destination: PathBuf,
    /// The folder inside the destination the copies are staged in.
    staging: PathBuf,
    started_ms: u64,
    items: Vec<Item>,
}

/// What a running paste has to say about itself.
#[derive(Debug, Default)]
struct Progress {
    total: usize,
    landed: usize,
    /// What did not land, by the name the person knows it by, and why.
    failed: Vec<(String, String)>,
    /// Why nothing landed at all.
    refusal: Option<Refused>,
    /// Whether this undid a paste a session that stopped was in the middle of.
    undone: bool,
    finished: bool,
}

/// A paste running on a thread of its own: what it has done, and whether it is over.
pub(crate) struct Paste {
    progress: Arc<Mutex<Progress>>,
    worker: Option<thread::JoinHandle<()>>,
}

/// What a finished paste reports to the session.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// How many items the paste carried.
    pub total: usize,
    /// How many of them landed.
    pub landed: usize,
    /// What did not, by name and why.
    pub failed: Vec<(String, String)>,
    /// Why nothing was pasted at all.
    pub refusal: Option<Refused>,
    /// Whether this undid a paste a session that stopped was in the middle of.
    pub undone: bool,
}

impl Paste {
    /// Start pasting `files` into `destination`, staged and recorded as the
    /// module describes. The receiving folder is read, the receipt written
    /// and the copies made on the worker, so the caller never waits on a
    /// disk. `None` when another paste is still running: there is one receipt,
    /// so a second paste would overwrite the record the first is recovered
    /// from.
    pub(crate) fn start(
        files: &CopiedFiles,
        destination: &Path,
        conflict: Conflict,
        now_ms: u64,
        wake: &tilo_service::Wake,
    ) -> Option<Self> {
        Self::spawn(
            Job::Paste {
                files: files.clone(),
                destination: destination.to_path_buf(),
                conflict,
                now_ms,
            },
            wake,
        )
    }

    /// The paste a session that stopped was in the middle of, to be undone. `None` when it left
    /// nothing behind.
    pub(crate) fn unfinished(wake: &tilo_service::Wake) -> Option<Self> {
        let receipt = read_receipt()?;
        Self::spawn(Job::Undo(receipt), wake)
    }

    fn spawn(job: Job, wake: &tilo_service::Wake) -> Option<Self> {
        let slot = Slot::take()?;
        let progress = Arc::new(Mutex::new(Progress::default()));
        let shared = Arc::clone(&progress);
        let wake = Arc::clone(wake);
        match thread::Builder::new()
            .name("file-paste".to_owned())
            .spawn(move || {
                run(&job, &shared, &wake);
                drop(slot);
            }) {
            Ok(worker) => Some(Self {
                progress,
                worker: Some(worker),
            }),
            Err(error) => {
                tracing::warn!(%error, "the paste of files could not be started");
                let mut state = progress.lock_recover();
                state.refusal = Some(Refused::Stopped(error.to_string()));
                state.finished = true;
                drop(state);
                Some(Self {
                    progress,
                    worker: None,
                })
            }
        }
    }

    /// Whether the worker has said everything it has to say.
    pub(crate) fn finished(&self) -> bool {
        self.progress.lock_recover().finished
    }

    /// What the worker did, once it is over: it is waited for here, which is no wait at all by
    /// then.
    pub(crate) fn outcome(&mut self) -> Outcome {
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            // Its progress is all the outcome says; a worker that panicked leaves the paste
            // half-done with no reason.
            tracing::warn!("the paste worker stopped before it finished");
        }
        let state = std::mem::take(&mut *self.progress.lock_recover());
        Outcome {
            total: state.total,
            landed: state.landed,
            failed: state.failed,
            refusal: state.refusal,
            undone: state.undone,
        }
    }
}

/// The one paste a session carries at a time: the receipt is one file, so a second paste written
/// down beside the first would leave neither whole.
static RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The running paste's claim on the receipt, held until its worker is done.
struct Slot;

impl Slot {
    fn take() -> Option<Self> {
        (!RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        RUNNING.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// What a worker was started for.
enum Job {
    Paste {
        files: CopiedFiles,
        destination: PathBuf,
        conflict: Conflict,
        now_ms: u64,
    },
    Undo(Receipt),
}

fn run(job: &Job, progress: &Mutex<Progress>, wake: &tilo_service::Wake) {
    let stopped = match job {
        Job::Paste {
            files,
            destination,
            conflict,
            now_ms,
        } => paste(files, destination, *conflict, *now_ms, progress),
        Job::Undo(receipt) => {
            progress.lock_recover().undone = true;
            undo(receipt, progress).map_err(Refused::Stopped)
        }
    };
    let mut state = progress.lock_recover();
    if let Err(refusal) = stopped {
        state.refusal = Some(refusal);
    }
    state.finished = true;
    drop(state);
    // The loop only asks whether the paste is over: the answer is here.
    wake();
}

/// Carry out a new paste: what it means is written down before its first item is touched, and its
/// receipt is the last thing to go.
fn paste(
    files: &CopiedFiles,
    destination: &Path,
    conflict: Conflict,
    now_ms: u64,
    progress: &Mutex<Progress>,
) -> Result<(), Refused> {
    let mut receipt = plan(files, destination, conflict, now_ms)?;
    progress.lock_recover().total = receipt.items.len();
    save(&receipt).map_err(Refused::Stopped)?;
    let mut staging = match Staging::take(destination, &receipt.staging) {
        Ok(staging) => staging,
        Err(error) => {
            if let Err(error) = clear_receipt() {
                tracing::warn!(%error, "the receipt of a paste that could not start was kept");
            }
            return Err(Refused::Stopped(format!(
                "{} could not hold the paste: {error}",
                destination.display()
            )));
        }
    };
    if let Err(error) = transfer(&mut receipt, &staging, progress) {
        // Nothing of a paste that could not be recorded is left standing: what landed stays whole
        // and the rest goes back where it was.
        if let Err(kept) = undo(&receipt, progress) {
            staging.keep();
            tracing::warn!(
                %kept,
                %error,
                destination = %destination.display(),
                "a paste that could not be recorded could not be put back"
            );
        }
        progress.lock_recover().undone = false;
        return Err(Refused::Stopped(error));
    }
    staging.finish_success();
    if let Err(error) = clear_receipt() {
        tracing::warn!(%error, "a finished paste's receipt was kept");
    }
    Ok(())
}

/// Send every name this paste replaced to the Trash, once the copies that replaced them are
/// published and synced.
fn trash_asides(receipt: &Receipt) {
    for index in 0..receipt.items.len() {
        trash(&replaced_path(&receipt.staging, index));
    }
}

/// Plan the paste of `files` into `destination` : what lands where, and the private folder their
/// copies are staged in. Nothing is touched on the disk here: the paste is written down before its
/// staging folder exists.
fn plan(
    files: &CopiedFiles,
    destination: &Path,
    conflict: Conflict,
    now_ms: u64,
) -> Result<Receipt, Refused> {
    if !destination.is_dir() {
        return Err(Refused::NotAFolder(destination.to_path_buf()));
    }
    let staging = staging_path(destination);
    let mut items = Vec::new();
    for source in &files.paths {
        if items.len() >= MAX_ITEMS {
            break;
        }
        if !source.is_absolute() {
            continue;
        }
        let Some(name) = source.file_name() else {
            continue;
        };
        let target = match conflict {
            Conflict::KeepBoth => {
                free_name(destination, name).map_err(|error| Refused::Stopped(error.to_string()))?
            }
            Conflict::Skip => {
                let target = destination.join(name);
                fs::symlink_metadata(&target).is_err().then_some(target)
            }
            Conflict::Replace => Some(destination.join(name)),
        };
        // Pasted into the folder it already sits in, a file has no second name to take.
        let Some(target) = target.filter(|target| target != source) else {
            continue;
        };
        let held = if files.cut {
            quarantine_path(source).ok()
        } else {
            None
        };
        if files.cut && held.is_none() {
            continue;
        }
        items.push(Item {
            source: source.clone(),
            target,
            state: State::Pending,
            held,
        });
    }
    if items.is_empty() {
        return Err(Refused::Nothing);
    }
    Ok(Receipt {
        version: RECEIPT_VERSION,
        moving: files.cut,
        conflict,
        destination: destination.to_path_buf(),
        staging,
        started_ms: now_ms,
        items,
    })
}

/// The staged path of the item at `index` .
fn staged_path(staging: &Path, index: usize) -> PathBuf {
    staging.join(index.to_string())
}

/// Where the name the item at `index` replaces waits for the Trash.
fn replaced_path(staging: &Path, index: usize) -> PathBuf {
    staging.join("replaced").join(index.to_string())
}

/// Carry the paste out a batch at a time.
fn transfer(
    receipt: &mut Receipt,
    staging: &Staging,
    progress: &Mutex<Progress>,
) -> Result<(), String> {
    let total = receipt.items.len();
    let mut start = 0usize;
    while start < total {
        let end = start.saturating_add(BATCH_ITEMS).min(total);
        for index in start..end {
            stage(receipt, staging, index, progress);
        }
        // Every staged copy is flushed and whole; the receipt now names them, so a session that
        // stops can tell a published copy from a stray one.
        staging.sync().map_err(|error| {
            format!("{} could not be synced: {error}", receipt.staging.display())
        })?;
        save(receipt)?;
        crash_point("staging");
        for index in start..end {
            publish(receipt, index, progress);
            if start == 0 && index == 0 {
                crash_point("publishing");
            }
        }
        sync_directory(&receipt.destination).map_err(|error| {
            format!(
                "{} could not be synced: {error}",
                receipt.destination.display()
            )
        })?;
        if receipt.moving {
            for index in start..end {
                unhold(receipt, index, progress);
            }
        }
        save(receipt)?;
        start = end;
    }
    // What the paste replaced is out of the way for good, and goes where the person can get it back
    // from.
    trash_asides(receipt);
    Ok(())
}

/// End the process at `stage` of a paste when a crash test asks for it: a test that kills a session
/// mid-paste names the moment to die at rather than sampling the disk for it, so it dies at that
/// moment and no other.
fn crash_point(stage: &str) {
    let dying = cfg!(debug_assertions)
        && std::env::var_os(CRASH_POINT_ENV).as_deref() == Some(std::ffi::OsStr::new(stage));
    if dying {
        // SAFETY: the process ends here, as a power cut would end it: the
        // kill cannot be caught, so no destructor runs and nothing of the
        // paste is tidied up.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    }
}

/// Stage the item at `index` : its whole copy, synced, under the paste's staging folder.
fn stage(receipt: &mut Receipt, staging: &Staging, index: usize, progress: &Mutex<Progress>) {
    let Some(item) = receipt.items.get(index) else {
        return;
    };
    if !matches!(item.state, State::Pending) {
        return;
    }
    let source = item.source.clone();
    let staged = staged_path(&receipt.staging, index);
    let name = display_name(&source);
    let leaf = index.to_string();
    let copied = staging
        .stage(&source, Path::new(&leaf), &mut ())
        .and_then(|()| {
            let metadata = fs::symlink_metadata(&staged)?;
            Ok(Identity::of(&metadata))
        });
    match copied {
        Ok(identity) => {
            if let Some(item) = receipt.items.get_mut(index) {
                item.state = State::Staged(identity);
            }
        }
        Err(error) => {
            best_effort(
                "removing what was staged for a file that was not pasted",
                &staged,
                || remove_path(&staged),
            );
            tracing::warn!(%error, source = %source.display(), "a file was not pasted");
            fail(receipt, index, progress, &name, &error.to_string());
        }
    }
}

/// Move the name this paste replaces out of the way, into the staging folder until the paste is
/// over and the Trash after that. False when the item could not be published at all.
fn set_aside(
    receipt: &mut Receipt,
    index: usize,
    target: &Path,
    name: &str,
    progress: &Mutex<Progress>,
) -> bool {
    let aside = replaced_path(&receipt.staging, index);
    let Some(beside) = aside.parent() else {
        return false;
    };
    if let Err(error) = fs::create_dir_all(beside) {
        fail(
            receipt,
            index,
            progress,
            name,
            &format!(
                "there is nowhere to set {} aside: {error}",
                target.display()
            ),
        );
        return false;
    }
    // A clash this paste already put aside goes to the Trash: only the last one is ever put back.
    if fs::symlink_metadata(&aside).is_ok() {
        trash(&aside);
    }
    if let Err(error) = fs::rename(target, &aside) {
        fail(
            receipt,
            index,
            progress,
            name,
            &format!("{} could not go to the Trash: {error}", target.display()),
        );
        return false;
    }
    best_effort(
        "making the paste's folder survive a crash",
        &receipt.destination,
        || sync_directory(&receipt.destination),
    );
    true
}

/// A cut's source goes into hiding: its copy stands under its name only once its source has a
/// hidden name the receipt records, so a session that stops either has the copy or can put the
/// source back.
fn hide_source(
    receipt: &mut Receipt,
    index: usize,
    name: &str,
    progress: &Mutex<Progress>,
) -> bool {
    let hiding = receipt
        .items
        .get(index)
        .and_then(|item| item.held.clone().map(|held| (item.source.clone(), held)));
    let Some((source, held)) = hiding else {
        return true;
    };
    if let Err(error) = quarantine_at(&source, &held, &mut ()) {
        best_effort("making a hidden source survive a crash", &source, || {
            sync_parent(&source)
        });
        fail(
            receipt,
            index,
            progress,
            name,
            &format!("it could not be taken from its folder: {error}"),
        );
        return false;
    }
    true
}

/// Publish the item at `index` under its name, after what it replaces has gone out of the way.
fn publish(receipt: &mut Receipt, index: usize, progress: &Mutex<Progress>) {
    let Some(item) = receipt.items.get(index) else {
        return;
    };
    if !matches!(item.state, State::Staged(_)) {
        return;
    }
    let source = item.source.clone();
    let mut target = item.target.clone();
    let staged = staged_path(&receipt.staging, index);
    let name = display_name(&source);
    let conflict = receipt.conflict;
    let mut tries = 0usize;
    while tries < MAX_NAME_TRIES {
        tries = tries.saturating_add(1);
        if conflict == Conflict::Replace
            && fs::symlink_metadata(&target).is_ok()
            && !set_aside(receipt, index, &target, &name, progress)
        {
            return;
        }
        if receipt.moving && !hide_source(receipt, index, &name, progress) {
            return;
        }
        match host_fs::rename_noreplace(&staged, &target) {
            Ok(()) => {
                if let Some(item) = receipt.items.get_mut(index) {
                    item.state = if receipt.moving {
                        State::Moved
                    } else {
                        State::Published
                    };
                    item.target = target;
                }
                let mut state = progress.lock_recover();
                state.landed = state.landed.saturating_add(1);
                drop(state);
                return;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if let Some(item) = receipt.items.get(index)
                    && let Err(error) = put_back(item)
                {
                    best_effort_by("putting a cut's source back", &item.source, error);
                }
                if conflict == Conflict::Replace {
                    continue;
                }
                let Some(folder) = target.parent() else {
                    fail(receipt, index, progress, &name, "it has no folder");
                    return;
                };
                let Some(target_name) = target.file_name() else {
                    fail(receipt, index, progress, &name, "its name has no leaf");
                    return;
                };
                let free = match free_name(folder, target_name) {
                    Ok(Some(free)) => free,
                    Ok(None) => {
                        fail(
                            receipt,
                            index,
                            progress,
                            &name,
                            "every free name beside it is taken",
                        );
                        return;
                    }
                    Err(error) => {
                        fail(receipt, index, progress, &name, &error.to_string());
                        return;
                    }
                };
                target = free;
                if let Some(item) = receipt.items.get_mut(index) {
                    item.target.clone_from(&target);
                }
                if let Err(error) = save(receipt) {
                    fail(receipt, index, progress, &name, &error);
                    return;
                }
            }
            Err(error) => {
                if let Some(item) = receipt.items.get(index)
                    && let Err(kept) = put_back(item)
                {
                    best_effort_by("putting a cut's source back", &item.source, kept);
                }
                fail(receipt, index, progress, &name, &error.to_string());
                return;
            }
        }
    }
    fail(receipt, index, progress, &name, "its name kept being taken");
}

/// Take a cut's source away, now that its copy is published and synced.
fn unhold(receipt: &mut Receipt, index: usize, progress: &Mutex<Progress>) {
    let Some(item) = receipt.items.get(index) else {
        return;
    };
    if !matches!(item.state, State::Moved) {
        return;
    }
    let Some(held) = item.held.clone() else {
        return;
    };
    let source = item.source.clone();
    let name = display_name(&source);
    if let Err(error) = remove_path(&held) {
        tracing::warn!(%error, held = %held.display(), "a cut's source was not taken away");
        fail(
            receipt,
            index,
            progress,
            &name,
            &format!("it was copied, but its source was not removed: {error}"),
        );
        return;
    }
    best_effort(
        "making a cut's emptied source survive a crash",
        &source,
        || sync_parent(&source),
    );
}

/// Record that the item at `index` did not land, and why.
fn fail(receipt: &mut Receipt, index: usize, progress: &Mutex<Progress>, name: &str, reason: &str) {
    if let Some(item) = receipt.items.get_mut(index) {
        // A cut's source comes out of hiding before anything else.
        if let Err(error) = put_back(item) {
            best_effort_by(
                "putting a cut's source back after a failure",
                &item.source,
                error,
            );
        }
        item.state = State::Failed;
    }
    progress
        .lock_recover()
        .failed
        .push((name.to_owned(), reason.to_owned()));
}

/// Undo a paste, item by item: what stood is kept whole, what did not is put back where it was. The
/// staging folder and the receipt go last.
fn undo(receipt: &Receipt, progress: &Mutex<Progress>) -> Result<(), String> {
    let mut trouble: Option<String> = None;
    for index in (0..receipt.items.len()).rev() {
        let Some(item) = receipt.items.get(index) else {
            continue;
        };
        let staged = staged_path(&receipt.staging, index);
        let aside = replaced_path(&receipt.staging, index);
        let name = display_name(&item.source);
        let stood = match item.state {
            // Its copy stands under its name: it stays.
            State::Moved | State::Published => true,
            State::Staged(identity) => identity.matches(&item.target),
            State::Pending | State::Failed => false,
        };
        if stood {
            if let Err(error) = remove_held(item) {
                record(&mut trouble, &name, &error, progress);
            }
            trash(&aside);
            continue;
        }
        if let Err(error) = remove_path(&staged) {
            record(&mut trouble, &name, &error.to_string(), progress);
        }
        // The name this paste replaced comes back where it was.
        if fs::symlink_metadata(&aside).is_ok() {
            let came_back = fs::symlink_metadata(&item.target).is_err()
                && fs::rename(&aside, &item.target).is_ok();
            if !came_back {
                trash(&aside);
            }
            best_effort(
                "making a restored name survive a crash",
                &item.target,
                || sync_parent(&item.target),
            );
        }
        if let Err(error) = put_back(item) {
            record(&mut trouble, &name, &error, progress);
        }
    }
    if let Some(reason) = trouble {
        // Something could not be put back — a drive that was unplugged, a folder that was taken
        // away. The receipt stays, staging folder and all, so the next session tries again rather
        // than leaving a source hidden under a name nothing remembers.
        return Err(reason);
    }
    remove_staging(&receipt.staging)
        .map_err(|error| format!("the staging folder could not be removed: {error}"))?;
    if let Err(error) = clear_receipt() {
        tracing::warn!(%error, "a put-back paste's receipt was kept");
    }
    Ok(())
}

fn record(trouble: &mut Option<String>, name: &str, reason: &str, progress: &Mutex<Progress>) {
    tracing::warn!(name, reason, "a paste could not be put back");
    progress
        .lock_recover()
        .failed
        .push((name.to_owned(), reason.to_owned()));
    if trouble.is_none() {
        *trouble = Some(reason.to_owned());
    }
}

/// Put a cut's hidden source back under its name.
fn put_back(item: &Item) -> Result<(), String> {
    let Some(held) = &item.held else {
        return Ok(());
    };
    restore_source(held, &item.source, &mut ()).map_err(|error| error.to_string())
}

/// Take a finished item's hidden source away.
fn remove_held(item: &Item) -> Result<(), String> {
    let Some(held) = &item.held else {
        return Ok(());
    };
    if fs::symlink_metadata(held).is_err() {
        return Ok(());
    }
    remove_path(held).map_err(|error| error.to_string())?;
    best_effort(
        "making a removed name survive a crash",
        &item.source,
        || sync_parent(&item.source),
    );
    Ok(())
}

/// The name the person knows `path` by.
fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Take the paste's staging folder away.
fn remove_staging(staging: &Path) -> io::Result<()> {
    remove_path(staging)?;
    best_effort("removing the staging folder", staging, || {
        sync_parent(staging)
    });
    Ok(())
}

/// The receipt's file: this session's own state, never the shared settings document, which the
/// person's devices keep in step.
fn receipt_path() -> Option<PathBuf> {
    tilo_paths::state_dir().map(|root| root.join("tilo/file-paste").join(RECEIPT_FILE))
}

/// Write the paste down durably, so a session that stops knows what it was doing.
fn save(receipt: &Receipt) -> Result<(), String> {
    let Some(path) = receipt_path() else {
        return Err("there is nowhere to write a paste down".to_owned());
    };
    let bytes = serde_json::to_vec(receipt)
        .map_err(|error| format!("the paste could not be written down: {error}"))?;
    tilo_files::native::atomic::write_private(&path, &bytes)
        .map_err(|error| format!("the paste could not be written down: {error}"))
}

/// The paste a session that stopped left behind, if any.
fn read_receipt() -> Option<Receipt> {
    let path = receipt_path()?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                tracing::warn!(%error, path = %path.display(), "a paste was not read");
            }
            return None;
        }
    };
    match serde_json::from_slice::<Receipt>(&bytes) {
        Ok(receipt) if receipt.version == RECEIPT_VERSION => Some(receipt),
        Ok(receipt) => {
            tracing::warn!(
                version = receipt.version,
                "a paste written by another version of Tilo was left alone"
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "a paste was not read");
            None
        }
    }
}

/// Forget the paste: it is over, one way or the other.
fn clear_receipt() -> io::Result<()> {
    let Some(path) = receipt_path() else {
        return Ok(());
    };
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    match path.parent() {
        Some(folder) => sync_directory(folder),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn scratch(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("clipboard-files-{name}-"))
            .tempdir()
            .expect("a scratch folder")
    }

    fn files(paths: &[&Path], cut: bool) -> CopiedFiles {
        CopiedFiles {
            paths: paths.iter().map(Path::new).map(Path::to_path_buf).collect(),
            cut,
        }
    }

    fn take_staging(receipt: &Receipt) -> Staging {
        Staging::take(&receipt.destination, &receipt.staging).expect("the staging folder")
    }

    fn carry(receipt: &mut Receipt, staging: &Staging, progress: &Mutex<Progress>) {
        transfer(receipt, staging, progress).expect("the paste was carried out");
    }

    /// What the paste has to say about itself, taken without holding on to the paste's own lock.
    fn reported(progress: &Mutex<Progress>) -> (usize, Vec<(String, String)>) {
        let state = progress.lock_recover();
        (state.landed, state.failed.clone())
    }

    #[test]
    fn a_paste_gives_each_file_its_own_free_name() {
        let dir = scratch("names");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(&source).expect("the source folder");
        fs::create_dir_all(&destination).expect("the destination folder");
        let note = source.join("note.txt");
        fs::write(&note, b"mine").expect("a file to copy");
        fs::write(destination.join("note.txt"), b"theirs").expect("a file in the way");

        let receipt =
            plan(&files(&[&note], false), &destination, Conflict::KeepBoth, 7).expect("a paste");
        assert_eq!(receipt.items.len(), 1);
        let item = receipt.items.first().expect("the item");
        assert_eq!(item.target, destination.join("note 2.txt"));
        assert_eq!(item.state, State::Pending);
        assert!(!receipt.moving);
        assert_eq!(receipt.started_ms, 7);

        // Pasted into the folder the files already sit in, a kept copy takes a second name.
        let beside = plan(&files(&[&note], false), &source, Conflict::KeepBoth, 7)
            .expect("a paste into its own folder");
        assert_eq!(
            beside.items.first().expect("the item").target,
            source.join("note 2.txt")
        );
        // A destination that is not a folder takes nothing.
        assert_eq!(
            plan(&files(&[&note], false), &note, Conflict::KeepBoth, 7).err(),
            Some(Refused::NotAFolder(note.clone()))
        );
        // Skipping a name the folder has leaves it out of the paste.
        assert_eq!(
            plan(&files(&[&note], false), &destination, Conflict::Skip, 7).err(),
            Some(Refused::Nothing)
        );
    }

    #[test]
    fn a_paste_carries_its_items_out_and_a_cut_takes_the_sources() {
        let dir = scratch("carry");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(source.join("folder")).expect("a source folder");
        fs::create_dir_all(&destination).expect("a destination folder");
        let loose = source.join("note.txt");
        fs::write(&loose, b"hello").expect("a file");
        fs::write(source.join("folder").join("inner.txt"), b"inner").expect("a nested file");

        let mut receipt = plan(
            &files(&[&loose, &source.join("folder")], true),
            &destination,
            Conflict::KeepBoth,
            0,
        )
        .expect("a paste");
        let progress = Mutex::new(Progress::default());
        let staging = take_staging(&receipt);
        carry(&mut receipt, &staging, &progress);
        assert_eq!(
            fs::read(destination.join("note.txt")).expect("pasted"),
            b"hello"
        );
        assert_eq!(
            fs::read(destination.join("folder").join("inner.txt")).expect("pasted tree"),
            b"inner"
        );
        assert!(!loose.exists(), "a cut takes its source once it is across");
        assert!(!source.join("folder").exists());
        assert!(
            fs::read_dir(&receipt.staging).is_ok_and(|mut entries| entries.next().is_none()),
            "no copy stays behind in staging: {}",
            receipt.staging.display()
        );
        let (landed, failed) = reported(&progress);
        assert_eq!(landed, 2);
        assert!(failed.is_empty(), "{failed:?}");
    }

    #[test]
    fn a_copy_is_whole_under_its_name_with_its_links_and_its_times() {
        let dir = scratch("whole");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(source.join("tree").join("deeper")).expect("a source tree");
        fs::create_dir_all(&destination).expect("a destination folder");
        let note = source.join("tree").join("deeper").join("note.txt");
        fs::write(&note, b"the whole story").expect("a nested file");
        std::os::unix::fs::symlink("../deeper/note.txt", source.join("tree").join("link"))
            .expect("a link");
        let changed = fs::metadata(&note)
            .expect("the file's metadata")
            .modified()
            .expect("a modification time");

        let mut receipt = plan(
            &files(&[&source.join("tree")], false),
            &destination,
            Conflict::KeepBoth,
            0,
        )
        .expect("a paste");
        let progress = Mutex::new(Progress::default());
        let staging = take_staging(&receipt);
        carry(&mut receipt, &staging, &progress);
        let pasted = destination.join("tree");
        assert_eq!(
            fs::read(pasted.join("deeper").join("note.txt")).expect("pasted"),
            b"the whole story"
        );
        assert_eq!(
            fs::read_link(pasted.join("link")).expect("the link was copied as a link"),
            Path::new("../deeper/note.txt")
        );
        assert_eq!(
            fs::metadata(pasted.join("deeper").join("note.txt"))
                .expect("the copy's metadata")
                .modified()
                .expect("a modification time"),
            changed,
            "a copy keeps the time it last changed"
        );
        assert!(source.join("tree").exists(), "a copy leaves its source");
    }

    #[test]
    fn replacing_sends_the_name_that_was_there_to_the_trash() {
        let dir = scratch("replace");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(&source).expect("a source folder");
        fs::create_dir_all(&destination).expect("a destination folder");
        let note = source.join("note.txt");
        fs::write(&note, b"the new one").expect("a file");
        fs::write(destination.join("note.txt"), b"the old one").expect("a file in the way");

        let mut receipt =
            plan(&files(&[&note], false), &destination, Conflict::Replace, 0).expect("a paste");
        let progress = Mutex::new(Progress::default());
        let staging = take_staging(&receipt);
        carry(&mut receipt, &staging, &progress);
        assert_eq!(
            fs::read(destination.join("note.txt")).expect("pasted"),
            b"the new one"
        );
        let Some(files) = host_fs::trash::trash_dir().filter(|files| files.is_dir()) else {
            return;
        };
        let kept = fs::read_dir(&files).is_ok_and(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                let path = entry.path();
                path.is_file() && fs::read(&path).is_ok_and(|bytes| bytes == b"the old one")
            })
        });
        assert!(kept, "what was there is in the Trash, not gone");
    }

    #[test]
    fn a_cut_source_comes_back_beside_a_name_that_was_taken_again() {
        let dir = scratch("retaken");
        let source = dir.path().join("source");
        fs::create_dir_all(&source).expect("a source folder");
        let note = source.join("note.txt");
        fs::write(&note, b"mine").expect("a file");
        let held = quarantine_path(&note).expect("a hidden name");
        fs::rename(&note, &held).expect("the source is hidden");
        fs::write(&note, b"someone else's").expect("the name is taken again");

        let item = Item {
            source: note.clone(),
            target: dir.path().join("note.txt"),
            state: State::Staged(Identity {
                device: 0,
                inode: 0,
            }),
            held: Some(held),
        };
        put_back(&item).expect("the source comes back");
        assert_eq!(fs::read(&note).expect("the new file"), b"someone else's");
        assert_eq!(
            fs::read(source.join("note 2.txt")).expect("the source beside it"),
            b"mine"
        );
    }

    #[test]
    fn what_landed_stays_and_what_did_not_goes_back_when_a_paste_is_undone() {
        let dir = scratch("undo");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(&source).expect("a source folder");
        fs::create_dir_all(&destination).expect("a destination folder");
        let across = source.join("across.txt");
        let behind = source.join("behind.txt");
        let away = source.join("away.txt");
        fs::write(&across, b"across").expect("a file");
        fs::write(&behind, b"behind").expect("a file");
        fs::write(&away, b"away").expect("a file");

        let mut receipt = plan(
            &files(&[&across, &behind, &away], true),
            &destination,
            Conflict::KeepBoth,
            0,
        )
        .expect("a paste");
        let progress = Mutex::new(Progress::default());
        // The first item went all the way; the second is only staged; the third was written down
        // and never touched, as a session that stopped partway leaves them.
        let (first, second) = (0usize, 1usize);
        let staging = take_staging(&receipt);
        stage(&mut receipt, &staging, first, &progress);
        publish(&mut receipt, first, &progress);
        unhold(&mut receipt, first, &progress);
        stage(&mut receipt, &staging, second, &progress);
        save(&receipt).expect("the receipt is written");

        undo(&receipt, &progress).expect("the paste was undone");
        assert_eq!(
            fs::read(destination.join("across.txt")).expect("the published copy stays"),
            b"across"
        );
        assert!(!across.exists(), "its source went, as a finished move does");
        assert!(
            !destination.join("behind.txt").exists(),
            "nothing half written"
        );
        assert_eq!(fs::read(&behind).expect("the source is back"), b"behind");
        assert_eq!(
            fs::read(&away).expect("the untouched source stays"),
            b"away"
        );
        assert!(fs::symlink_metadata(&receipt.staging).is_err());
        assert!(
            read_receipt().is_none(),
            "a paste that is over leaves no receipt"
        );
    }

    #[test]
    fn an_unreadable_file_is_reported_and_the_rest_of_the_paste_lands() {
        let dir = scratch("unreadable");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(&source).expect("a source folder");
        fs::create_dir_all(&destination).expect("a destination folder");
        let secret = source.join("secret.txt");
        let fine = source.join("fine.txt");
        fs::write(&secret, b"closed").expect("a file");
        fs::write(&fine, b"open").expect("a file");
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o000)).expect("a closed file");
        if fs::read(&secret).is_ok() {
            // Root reads everything; there is no refusal to prove here.
            return;
        }

        let mut receipt = plan(
            &files(&[&secret, &fine], false),
            &destination,
            Conflict::KeepBoth,
            0,
        )
        .expect("a paste");
        let progress = Mutex::new(Progress::default());
        let staging = take_staging(&receipt);
        carry(&mut receipt, &staging, &progress);
        assert_eq!(
            fs::read(destination.join("fine.txt")).expect("the readable one landed"),
            b"open"
        );
        let (landed, failed) = reported(&progress);
        assert_eq!(failed.len(), 1, "one file was reported: {failed:?}");
        assert_eq!(
            failed.first().map(|(name, _)| name.as_str()),
            Some("secret.txt")
        );
        assert_eq!(landed, 1);
    }
}
