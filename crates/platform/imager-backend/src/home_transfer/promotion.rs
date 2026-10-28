//! Non-overwriting adoption of a verified home quarantine. Existing target
//! contents and selected Tilo custody survive; every retained item has a receipt.
use super::super::{
    destination_security,
    migration::{OwnedTemporary, PinnedDirectory, changed},
    migration_durability as durable,
    migration_journal_log::{self, frame_bytes},
    migration_store::NativePath,
    nofollow::metadata_nofollow,
};
use crate::home_transfer::protocol::Fact;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, MetadataExt, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::{CString, OsStr},
    fs::File,
    io::{self, BufRead, Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};
mod checkpoints;
mod recovery;
mod report;
use checkpoints::Done;
use report::{Paths, Reports};
mod replay;
mod root_acl;
mod seeds;
#[cfg(all(test, feature = "native-capture-qualification"))]
mod seeds_native_tests;
mod segmented;
mod startup;
mod storage;
#[cfg(all(test, feature = "native-capture-qualification"))]
mod streaming_tests;
mod structure;
#[cfg(test)]
mod tests;
mod tree;
pub use recovery::AdoptionWitness;
use storage::{Row, Sequence, Store};
const MAX: usize = 64 * 1024 * 1024;
// Atomic replacement retains the current durable journal while its successor
// is written and synced. Archive index scratch cannot spend either copy.
pub const JOURNAL_RESERVE: u64 = 2 * MAX as u64;
const ENTRIES: usize = 1_000_000;
#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: NativePath,
    fact: Fact,
    items: Sequence<NativePath>,
    parent_mode: u32,
    kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<seeds::Witness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    structure: Option<Sequence<structure::Item>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Kind {
    Move,
    Collision { target: Fact },
    InactiveCustody,
    MergedDirectory { target: Fact },
}
#[derive(Clone, Serialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_acl: Option<root_acl::Intent>,
    #[serde(default)]
    applications: bool,
    quarantine: durable::Identity,
    home: durable::Identity,
    source_items: Sequence<NativePath>,
    entries: Sequence<Entry>,
}
#[derive(Clone)]
pub struct PromotionReview {
    quarantine: PathBuf,
    home: PathBuf,
    journal: PathBuf,
    plan: Plan,
}
#[derive(Clone, Debug, Default)]
pub struct PromotionReport {
    pub installed: Paths,
    pub collisions: Paths,
    pub inactive_custody: Paths,
    pub merged_directories: Paths,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    pub verified: bool,
}
impl PromotionReport {
    pub fn complete(&self) -> bool {
        self.verified && !self.cancelled && self.failed.is_empty() && self.collisions.is_empty()
    }
}
pub enum PromotionEvent {
    Finished(PromotionReport),
    Failed(String),
}
pub struct PromotionWorker {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<PromotionEvent>,
    finished: bool,
    pending: Option<PromotionEvent>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl PromotionWorker {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    /// A terminal event is observable only after the actual owned thread has
    /// returned and has been joined. Sending a response or disconnecting its
    /// channel cannot release the caller's operation/receive authority.
    pub fn try_next(&mut self) -> Option<PromotionEvent> {
        if self.finished {
            return None;
        }
        if self.pending.is_none() {
            match self.receiver.try_recv() {
                Ok(value) => self.pending = Some(value),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.pending = Some(PromotionEvent::Failed(
                        "Home adoption worker stopped".into(),
                    ));
                }
            }
        }
        if !self.observe_stopped() {
            return None;
        }
        // The send may race with the first empty try_recv before the join.
        // After actual retirement, read its queued result rather than replacing
        // a genuine completed report with a fabricated disconnect failure.
        if self.pending.is_none() {
            self.pending = Some(match self.receiver.try_recv() {
                Ok(value) => value,
                Err(_) => {
                    PromotionEvent::Failed("Home adoption worker returned without a report".into())
                }
            });
        }
        let event = self.pending.take().expect("retired worker terminal state");
        self.finished = true;
        Some(event)
    }
    /// Nonblocking kernel-thread ownership observation. `is_finished` proves
    /// return before joining, so this never waits for a running worker. A panic
    /// proves retirement but replaces a purported success with terminal failure.
    pub fn observe_stopped(&mut self) -> bool {
        let Some(thread) = self.thread.as_ref() else {
            return true;
        };
        if !thread.is_finished() {
            return false;
        }
        let Some(thread) = self.thread.take() else {
            return false;
        };
        if thread.join().is_err() {
            self.pending = Some(PromotionEvent::Failed(
                "Home adoption worker panicked".into(),
            ));
        }
        true
    }
}
impl Drop for PromotionWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn decoded(path: &NativePath) -> io::Result<PathBuf> {
    let path = path.decode().map_err(io::Error::other)?;
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(changed("Promotion receipt path is not relative"));
    }
    Ok(path)
}
fn parent(root: &Arc<PinnedDirectory>, path: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let mut pin = Arc::clone(root);
    for name in path.components() {
        let Component::Normal(name) = name else {
            return Err(changed("Invalid home adoption path"));
        };
        let metadata = metadata_nofollow(&pin.file, Path::new(name))?;
        pin = pin.open_child(name, &metadata)?;
    }
    Ok(pin)
}
fn roots(
    q: &Path,
    h: &Path,
    j: &Path,
) -> io::Result<(
    Arc<PinnedDirectory>,
    Arc<PinnedDirectory>,
    Arc<PinnedDirectory>,
)> {
    let quarantine = PinnedDirectory::open_absolute(q)?;
    let home = PinnedDirectory::open_absolute(h)?;
    let journal = PinnedDirectory::open_absolute(
        j.parent()
            .ok_or_else(|| changed("Missing adoption journal parent"))?,
    )?;
    for pin in [&quarantine, &home, &journal] {
        pin.validate_destination()?;
        destination_security::check_private_file(&pin.file)?;
    }
    if q == h
        || h.starts_with(q)
        || q.starts_with(h) && !q.starts_with(h.join(".local/state/tilo"))
        || j.starts_with(q)
        || quarantine.identity.dev() != home.identity.dev()
    {
        return Err(changed(
            "Home adoption needs distinct private roots on the same filesystem and a journal outside quarantine",
        ));
    }
    for custody in [".config/tilo", ".local/state/tilo"] {
        let pin = parent(&home, Path::new(custody))?;
        destination_security::check_private_file(&pin.file)?;
    }
    Ok((quarantine, home, journal))
}
impl PromotionReview {
    /// Simulate a process stopping after a reviewed move but before its checkpoint.
    #[cfg(feature = "test-support")]
    pub fn move_one_without_checkpoint_for_test(&self, relative: &Path) -> Result<(), String> {
        let (q, h, j) = roots(&self.quarantine, &self.home, &self.journal)
            .map_err(|error| error.to_string())?;
        let name = self.journal.file_name().ok_or("Missing adoption journal")?;
        let _log = Log::create(&j, name, self.plan.clone()).map_err(|error| error.to_string())?;
        for entry in self.plan.entries.iter() {
            let entry = entry.map_err(|error| error.to_string())?;
            if matches!(entry.kind, Kind::Move)
                && decoded(&entry.path).map_err(|error| error.to_string())? == relative
            {
                return process(&entry, &q, &h, false, &AtomicBool::new(false))
                    .map_err(|error| error.to_string());
            }
        }
        Err("Fixture requires an actual reviewed Move entry".into())
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn move_one_for_test(&self, relative: &Path) -> Result<(), String> {
        let (q, h, j) = roots(&self.quarantine, &self.home, &self.journal)
            .map_err(|error| error.to_string())?;
        let name = self.journal.file_name().ok_or("Missing adoption journal")?;
        let mut log =
            Log::create(&j, name, self.plan.clone()).map_err(|error| error.to_string())?;
        let mut selected = None;
        for (index, entry) in self.plan.entries.iter().enumerate() {
            let entry = entry.map_err(|e| e.to_string())?;
            if matches!(entry.kind, Kind::Move)
                && decoded(&entry.path).map_err(|e| e.to_string())? == relative
            {
                selected = Some((index, entry));
                break;
            }
        }
        let (index, entry) = selected.ok_or("Fixture requires an actual reviewed Move entry")?;
        self.protect_imported_startup(&q, &h, &log.done, &AtomicBool::new(false))
            .map_err(|error| error.to_string())?;
        process(&entry, &q, &h, false, &AtomicBool::new(false))
            .map_err(|error| error.to_string())?;
        log.checkpoint(index).map_err(|error| error.to_string())
    }
    pub fn review(q: &Path, h: &Path, j: &Path) -> Result<Self, String> {
        let (_, _, workspace) = roots(q, h, j).map_err(|e| e.to_string())?;
        let scratch = super::super::migration_review::InventoryScratch::review_for_restore(
            workspace,
            0,
            0,
            MAX,
            JOURNAL_RESERVE,
        )
        .map_err(|e| e.to_string())?;
        Self::review_with_storage(q, h, j, scratch, &AtomicBool::new(false))
    }
    pub fn review_with_storage(
        q: &Path,
        h: &Path,
        j: &Path,
        scratch: Arc<super::super::migration_review::InventoryScratch>,
        cancel: &AtomicBool,
    ) -> Result<Self, String> {
        super::super::migration::check_cancel(cancel).map_err(|e| e.to_string())?;
        let (quarantine, home, _) = roots(q, h, j).map_err(|e| e.to_string())?;
        let store =
            Store::new(scratch, Arc::new(AtomicBool::new(false))).map_err(|e| e.to_string())?;
        let source_items =
            tree::items(&quarantine, Path::new(""), &store, cancel).map_err(|e| e.to_string())?;
        let mut entries = store.builder();
        tree::review(
            &quarantine,
            &home,
            &home,
            Path::new(""),
            &mut entries,
            cancel,
        )
        .map_err(|e| e.to_string())?;
        let confirmed =
            tree::items(&quarantine, Path::new(""), &store, cancel).map_err(|e| e.to_string())?;
        if !same_paths(&source_items, &confirmed).map_err(|e| e.to_string())? {
            return Err("Quarantine contents changed during review".into());
        }
        let plan = Plan {
            version: 1,
            root_acl: None,
            applications: false,
            source_items,
            quarantine: tree::directory_identity(&quarantine).map_err(|e| e.to_string())?,
            home: tree::directory_identity(&home).map_err(|e| e.to_string())?,
            entries: entries.finish(),
        };
        validate_plan(&plan).map_err(|e| e.to_string())?;
        store.freeze().map_err(|e| e.to_string())?;
        Ok(Self {
            quarantine: q.into(),
            home: h.into(),
            journal: j.into(),
            plan,
        })
    }
    pub fn with_home_root_acl(mut self) -> Result<Self, String> {
        let (q, h, _) =
            roots(&self.quarantine, &self.home, &self.journal).map_err(|e| e.to_string())?;
        if let Some(intent) = &self.plan.root_acl {
            intent.verify(&q, &h, false).map_err(|e| e.to_string())?;
        } else {
            if self.journal.try_exists().map_err(|e| e.to_string())? {
                return Err("Retained adoption does not declare Home root ACL fidelity".into());
            }
            self.plan.root_acl = Some(root_acl::Intent::review(&q, &h).map_err(|e| e.to_string())?);
        }
        Ok(self)
    }
    /// Authenticated application consent controls activation, never raw Home bytes.
    pub fn with_application_consent(mut self, allowed: bool) -> Result<Self, String> {
        if self
            .journal
            .try_exists()
            .map_err(|error| error.to_string())?
            && self.plan.applications != allowed
        {
            return Err("Application activation consent differs from retained adoption".into());
        }
        self.plan.applications = allowed;
        Ok(self)
    }
    pub fn preview(&self) -> Result<PromotionReport, String> {
        let mut report = PromotionReport::default();
        let mut rows = Reports::new(&self.plan);
        for entry in self.plan.entries.iter() {
            rows.append(&entry.map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
        rows.finish(&mut report).map_err(|e| e.to_string())?;
        Ok(report)
    }
    pub fn recover(q: &Path, h: &Path, j: &Path) -> Result<Self, String> {
        let (_, _, journal) = roots(q, h, j).map_err(|e| e.to_string())?;
        let scratch = super::super::migration_review::InventoryScratch::review_for_restore(
            journal,
            0,
            0,
            MAX,
            JOURNAL_RESERVE,
        )
        .map_err(|e| e.to_string())?;
        Self::recover_with_storage(q, h, j, scratch)
    }
    pub fn recover_with_storage(
        q: &Path,
        h: &Path,
        j: &Path,
        scratch: Arc<super::super::migration_review::InventoryScratch>,
    ) -> Result<Self, String> {
        let (quarantine, home, journal) = roots(q, h, j).map_err(|e| e.to_string())?;
        let log = Log::open(
            &journal,
            j.file_name().ok_or("Missing adoption journal name")?,
            scratch,
        )
        .map_err(|e| e.to_string())?;
        if log.plan.version != 1
            || log.plan.entries.len() > ENTRIES
            || log.plan.quarantine
                != tree::directory_identity(&quarantine).map_err(|e| e.to_string())?
            || log.plan.home != tree::directory_identity(&home).map_err(|e| e.to_string())?
        {
            return Err("Home adoption roots differ from the retained operation".into());
        }
        for entry in log.plan.entries.iter() {
            let entry = entry.map_err(|error| error.to_string())?;
            decoded(&entry.path).map_err(|e| e.to_string())?;
            for item in entry.items.iter() {
                decoded(&item.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            }
        }
        Ok(Self {
            quarantine: q.into(),
            home: h.into(),
            journal: j.into(),
            plan: log.plan,
        })
    }
    pub fn start(&self, retry: bool) -> Result<PromotionWorker, String> {
        let reviewed = if retry {
            let recovered = Self::recover_with_storage(
                &self.quarantine,
                &self.home,
                &self.journal,
                self.plan.entries.store().scratch(),
            )?;
            if plan_digest(&recovered.plan).map_err(|error| error.to_string())?
                != plan_digest(&self.plan).map_err(|error| error.to_string())?
            {
                return Err("Retained adoption receipt differs from original consent".into());
            }
            recovered
        } else {
            self.clone()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("home-adoption".into())
            .spawn(move || {
                let event = match reviewed.run(retry, &worker_cancel) {
                    Ok(report) => PromotionEvent::Finished(report),
                    Err(e) => PromotionEvent::Failed(e.to_string()),
                };
                let _ = sender.send(event);
            })
            .map_err(|e| e.to_string())?;
        Ok(PromotionWorker {
            cancel,
            receiver,
            finished: false,
            pending: None,
            thread: Some(thread),
        })
    }
    fn run(&self, retry: bool, cancel: &AtomicBool) -> io::Result<PromotionReport> {
        let (quarantine, home, journal) = roots(&self.quarantine, &self.home, &self.journal)?;
        if tree::directory_identity(&quarantine)? != self.plan.quarantine
            || tree::directory_identity(&home)? != self.plan.home
        {
            return Err(changed("A reviewed home adoption root changed"));
        }
        let name = self
            .journal
            .file_name()
            .ok_or_else(|| changed("Missing adoption journal name"))?;
        let mut log = if retry {
            Log::open(&journal, name, self.plan.entries.store().scratch())?
        } else {
            Log::create(&journal, name, self.plan.clone())?
        };
        if plan_digest(&log.plan)? != plan_digest(&self.plan)? {
            return Err(changed(
                "Home adoption consent differs from its durable plan",
            ));
        }
        if retry {
            repair_parent_modes(&self.plan, &quarantine, &home)?;
        }
        if let Some(intent) = &self.plan.root_acl {
            if cancel.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if log.root_receipt.is_some() {
                intent.verify(&quarantine, &home, true)?;
            } else {
                intent.apply(&quarantine, &home)?;
                log.checkpoint_root(intent.receipt(&self.plan)?)?;
            }
        }

        self.protect_imported_startup(&quarantine, &home, &log.done, cancel)?;
        let mut report = PromotionReport::default();
        let mut rows = Reports::new(&self.plan);
        for (index, entry) in self.plan.entries.iter().enumerate() {
            let entry = entry?;
            if cancel.load(Ordering::Relaxed) {
                report.cancelled = true;
                break;
            }
            let path = decoded(&entry.path)?;
            let result = process(
                &entry,
                &quarantine,
                &home,
                log.done.contains(index)?,
                cancel,
            );
            if let Err(error) = result {
                report.failed.push((path, error.to_string()));
                break;
            }
            if !log.done.contains(index)? {
                #[cfg(test)]
                crate::migration_journal::crash_injection::boundary(
                    "adoption_moved_before_checkpoint",
                );
                log.checkpoint(index)?;
            }
            rows.append(&entry)?;
        }
        quarantine.validate_destination()?;
        home.validate_destination()?;
        let mut moved = crate::migration_review::storage::SortedIndex::with_scratch(
            self.plan.entries.store().scratch(),
        );
        for (index, entry) in self.plan.entries.iter().enumerate() {
            let entry = entry?;
            if log.done.contains(index)? && matches!(entry.kind, Kind::Move) {
                moved.insert(decoded(&entry.path)?, recovery::Marker, cancel)?;
            }
        }
        moved.finish(cancel)?;
        if let Err(error) = recovery::verify_remaining(&self.plan, &quarantine, &moved, cancel) {
            if error.kind() == io::ErrorKind::Interrupted {
                report.cancelled = true;
            } else {
                report.failed.push((PathBuf::new(), error.to_string()));
            }
        }
        rows.finish(&mut report)?;
        report.verified = log.done.len() == self.plan.entries.len()
            && report.failed.is_empty()
            && !report.cancelled;
        Ok(report)
    }
}
fn process(
    entry: &Entry,
    q: &Arc<PinnedDirectory>,
    h: &Arc<PinnedDirectory>,
    done: bool,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let path = decoded(&entry.path)?;
    let folder = path
        .parent()
        .ok_or_else(|| changed("Missing adoption parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| changed("Missing adoption name"))?;
    let source = parent(q, folder)?;
    match &entry.kind {
        Kind::MergedDirectory { target } => {
            let destination = parent(h, folder)?;
            if tree::shallow_directory(&source, name)?.0 != entry.fact
                || tree::shallow_directory(&destination, name)?.0 != *target
            {
                return Err(changed("Merged directory grant changed"));
            }
            Ok(())
        }
        Kind::InactiveCustody => {
            if tree::fact_in(&source, name, &path, &entry.items.store(), cancel)? != entry.fact {
                return Err(changed("Retained identity or setup custody changed"));
            }
            Ok(())
        }
        Kind::Collision { target } => {
            let destination = parent(h, folder)?;
            if tree::fact_in(&source, name, &path, &entry.items.store(), cancel)? != entry.fact
                || tree::target_fact(&destination, name, &path)?.0 != *target
            {
                return Err(changed("An existing or retained collision item changed"));
            }
            Ok(())
        }
        Kind::Move => {
            if let Some(seed) = &entry.seed {
                seeds::preserve(h, &path, seed)?;
            }
            let destination = parent(h, folder)?;
            source.validate_destination()?;
            destination.validate_destination()?;
            let writable = durable::readable_directory(&source.file)?;
            let mode = std::os::unix::fs::MetadataExt::mode(&writable.metadata()?) & 0o777;
            if mode != entry.parent_mode && mode != (entry.parent_mode | 0o700) {
                return Err(changed("Quarantine parent permissions changed"));
            }
            let original = metadata_nofollow(&source.file, Path::new(name));
            match original {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    if tree::fact_in(&destination, name, &path, &entry.items.store(), cancel)?
                        != entry.fact
                    {
                        return Err(changed("Previously moved item is missing or changed"));
                    }
                    restore_parent(&source, entry.parent_mode)?;
                    durable::sync_directory(&source.file)?;
                    durable::sync_directory(&destination.file)?;
                    return Ok(());
                }
                Err(e) => return Err(e),
                Ok(_) => (),
            }
            if done {
                return Err(changed("Completed move still has a source entry"));
            }
            if tree::fact_in(&source, name, &path, &entry.items.store(), cancel)? != entry.fact {
                return Err(changed("Reviewed quarantine item changed"));
            }
            if metadata_nofollow(&destination.file, Path::new(name)).is_ok() {
                return Err(changed(
                    "Target item appeared after review; it was not overwritten",
                ));
            }
            writable.set_permissions(std::fs::Permissions::from_mode(entry.parent_mode | 0o700))?;
            durable::sync_directory(&source.file)?;
            let name = CString::new(name.as_bytes()).map_err(io::Error::other)?;
            // SAFETY: both pinned parents are live, name is terminated, and
            // RENAME_NOREPLACE makes an existing target an atomic refusal.
            let moved = unsafe {
                libc::renameat2(
                    source.file.as_raw_fd(),
                    name.as_ptr(),
                    destination.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            let result = if moved == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            };
            let restored = restore_parent(&source, entry.parent_mode);
            durable::sync_directory(&source.file)?;
            durable::sync_directory(&destination.file)?;
            restored?;
            result?;
            if tree::fact_in(
                &destination,
                path.file_name().ok_or_else(|| changed("Missing name"))?,
                &path,
                &entry.items.store(),
                cancel,
            )? != entry.fact
            {
                return Err(changed("Moved item did not match its verified receipt"));
            }
            Ok(())
        }
    }
}
/// Repair only a retained parent inode with the exact temporary permission
/// change this plan authorized. A matching source or moved inode is required.
fn repair_parent_modes(
    plan: &Plan,
    quarantine: &Arc<PinnedDirectory>,
    home: &Arc<PinnedDirectory>,
) -> io::Result<()> {
    for entry in plan.entries.iter() {
        let entry = entry?;
        if !matches!(entry.kind, Kind::Move) || entry.parent_mode == (entry.parent_mode | 0o700) {
            continue;
        }
        let path = decoded(&entry.path)?;
        let folder = path
            .parent()
            .ok_or_else(|| changed("Missing repair parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| changed("Missing repair entry"))?;
        let source = parent(quarantine, folder)?;
        let metadata = durable::readable_directory(&source.file)?.metadata()?;
        let mode = std::os::unix::fs::MetadataExt::mode(&metadata) & 0o777;
        if mode == entry.parent_mode {
            continue;
        }
        if mode != (entry.parent_mode | 0o700) {
            return Err(changed("Interrupted adoption parent permissions changed"));
        }
        let identity = tree::directory_identity(&source)?;
        let expected = if folder.as_os_str().is_empty() {
            plan.quarantine.clone()
        } else {
            let mut found = None;
            for candidate in plan.entries.iter() {
                let candidate = candidate?;
                if matches!(candidate.kind, Kind::MergedDirectory { .. })
                    && decoded(&candidate.path)? == folder
                {
                    found = Some(candidate.fact.identity);
                    break;
                }
            }
            found.ok_or_else(|| changed("Interrupted parent has no retained identity"))?
        };
        if identity != expected {
            return Err(changed("Interrupted adoption parent was replaced"));
        }
        let fact = match metadata_nofollow(&source.file, Path::new(name)) {
            Ok(_) => tree::fact_in(
                &source,
                name,
                &path,
                &entry.items.store(),
                &AtomicBool::new(false),
            )?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => tree::fact_in(
                &parent(home, folder)?,
                name,
                &path,
                &entry.items.store(),
                &AtomicBool::new(false),
            )?,
            Err(error) => return Err(error),
        };
        if fact != entry.fact {
            return Err(changed(
                "Interrupted adoption entry no longer matches its receipt",
            ));
        }
        restore_parent(&source, entry.parent_mode)?;
        durable::sync_directory(&source.file)?;
    }
    Ok(())
}
fn restore_parent(source: &PinnedDirectory, mode: u32) -> io::Result<()> {
    let file = durable::readable_directory(&source.file)?;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}
fn validate_plan(plan: &Plan) -> io::Result<()> {
    if let Some(intent) = &plan.root_acl {
        intent.validate()?;
    }
    if plan.version != 1 || plan.entries.len() > ENTRIES || plan.source_items.len() > ENTRIES {
        return Err(changed("Adoption plan schema or size is invalid"));
    }
    let mut original = plan.source_items.iter();
    let mut previous = None;
    for entry in plan.entries.iter() {
        let entry = entry?;
        let path = decoded(&entry.path)?;
        // Review captures structural proof for exactly the moved entries that
        // may hold installed applications; recovery relies on it being there.
        if entry.structure.is_some()
            != (matches!(entry.kind, Kind::Move) && structure::needed(&path))
        {
            return Err(changed(
                "Adoption structural proof does not match its entry",
            ));
        }
        if let Some(items) = &entry.structure {
            structure::validate(items, &entry)?;
        }
        if let Some(seed) = &entry.seed {
            if !matches!(entry.kind, Kind::Move) {
                return Err(changed("Generated seed witness requires an adoption move"));
            }
            seeds::validate_witness(&path, seed)?;
        }
        if entry.parent_mode > 0o777 || entry.fact.mode > 0o777 || entry.items.len() > ENTRIES {
            return Err(changed("Invalid adoption permissions or item bounds"));
        }
        if matches!(entry.kind, Kind::InactiveCustody)
            && path != Path::new(".config/tilo")
            && path != Path::new(".local/state/tilo")
        {
            return Err(changed("Invalid inactive custody receipt"));
        }
        if entry.items.is_empty() {
            return Err(changed("Adoption receipt omits its root item"));
        }
        for (index, item) in entry.items.iter().enumerate() {
            let item = decoded(&item?)?;
            let source = original
                .next()
                .ok_or_else(|| changed("Adoption receipt has extra items"))??;
            if (index == 0 && item != path)
                || !item.starts_with(&path)
                || decoded(&source)? != item
                || previous.as_ref().is_some_and(|p| p >= &item)
            {
                return Err(changed(
                    "Adoption receipt paths overlap or differ from source",
                ));
            }
            previous = Some(item);
        }
    }
    if let Some(next) = original.next() {
        next?;
        return Err(changed(
            "Adoption plan does not account for all source items",
        ));
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedEntry {
    path: NativePath,
    fact: Fact,
    items: storage::Range,
    parent_mode: u32,
    kind: Kind,
    seed: Option<seeds::Witness>,
    structure: Option<storage::Range>,
}
impl Row for Entry {
    fn encode(&self) -> io::Result<Vec<u8>> {
        serde_json::to_vec(&CachedEntry {
            path: self.path.clone(),
            fact: self.fact.clone(),
            items: self.items.range(),
            parent_mode: self.parent_mode,
            kind: self.kind.clone(),
            seed: self.seed.clone(),
            structure: self.structure.as_ref().map(Sequence::range),
        })
        .map_err(io::Error::other)
    }
    fn decode(bytes: &[u8], store: &Arc<Store>) -> io::Result<Self> {
        let row: CachedEntry = serde_json::from_slice(bytes)?;
        Ok(Self {
            path: row.path,
            fact: row.fact,
            items: store.sequence(row.items)?,
            parent_mode: row.parent_mode,
            kind: row.kind,
            seed: row.seed,
            structure: row
                .structure
                .map(|range| store.sequence(range))
                .transpose()?,
        })
    }
}
fn same_paths(left: &Sequence<NativePath>, right: &Sequence<NativePath>) -> io::Result<bool> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (a, b) in left.iter().zip(right.iter()) {
        if a? != b? {
            return Ok(false);
        }
    }
    Ok(true)
}
struct PlanHash(Sha256);
impl Write for PlanHash {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn plan_digest(plan: &Plan) -> io::Result<[u8; 32]> {
    let mut writer = PlanHash(Sha256::new());
    serde_json::to_writer(&mut writer, plan)?;
    Ok(writer.0.finalize().into())
}
const MAGIC: [u8; 8] = *b"TILOADP2";
type Reservation = crate::migration_review::storage::scratch::MetadataReservation;
/// The adoption journal on the shared framed log: the segmented plan's
/// manifest, published with the header, then one receipt per completed move.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Record {
    Plan {
        nonce: String,
        bytes: u64,
        sha256: [u8; 32],
        segments: usize,
    },
    Segment(segmented::Segment),
    Entry(usize),
    Root {
        root_acl: root_acl::Receipt,
    },
}
struct Log {
    log: migration_journal_log::Log,
    plan: Plan,
    done: Done,
    root_receipt: Option<root_acl::Receipt>,
    journal_reservations: Vec<Reservation>,
    segment_reservation: Reservation,
    #[cfg(all(test, feature = "native-capture-qualification"))]
    manifest: segmented::Manifest,
}
impl Log {
    fn create(parent: &Arc<PinnedDirectory>, name: &OsStr, plan: Plan) -> io::Result<Self> {
        validate_plan(&plan)?;
        let scratch = plan.entries.store().scratch();
        scratch.validate_metadata_destination(parent)?;
        let mut journal_reservation = scratch.reserve_metadata()?;
        let checkpoint_bytes = checkpoint_reserve(&plan)?;
        let (pending, manifest) = segmented::Pending::write(parent, &plan)?;
        let head = Record::Plan {
            nonce: manifest.nonce.clone(),
            bytes: manifest.bytes,
            sha256: manifest.sha256,
            segments: manifest.segments.len(),
        };
        let mut journal_bytes = (MAGIC.len() as u64)
            .checked_add(frame_bytes(&head)?)
            .ok_or_else(|| changed("Adoption root quota overflow"))?;
        for segment in &manifest.segments {
            journal_bytes = journal_bytes
                .checked_add(frame_bytes(&Record::Segment(segment.clone()))?)
                .ok_or_else(|| changed("Adoption root quota overflow"))?;
        }
        let journal_bytes = journal_bytes
            .checked_add(checkpoint_bytes)
            .filter(|n| *n <= MAX as u64)
            .ok_or_else(|| changed("Home adoption root journal exceeds bounds"))?;
        journal_reservation.grow(journal_bytes)?;
        let log =
            migration_journal_log::Log::create_in(parent, name, MAGIC, &head, |file, seq| {
                for segment in &manifest.segments {
                    migration_journal_log::write_record(
                        file,
                        seq,
                        &Record::Segment(segment.clone()),
                    )?;
                }
                Ok(())
            })?;
        drop(log);
        #[cfg(test)]
        crate::migration_journal::crash_injection::boundary("adoption_root_published");
        // Once the root name exists, even an uncertain directory sync must
        // retain referenced segments. Error recovery can verify that root;
        // uncommitted cleanup must never leave it pointing to removed files.
        let reservation = pending.commit()?;
        Self::open_with_reservation(
            parent,
            name,
            scratch,
            Some(reservation),
            Some(journal_reservation),
        )
    }
    fn open(
        parent: &Arc<PinnedDirectory>,
        name: &OsStr,
        scratch: Arc<super::super::migration_review::InventoryScratch>,
    ) -> io::Result<Self> {
        Self::open_with_reservation(parent, name, scratch, None, None)
    }
    fn open_with_reservation(
        parent: &Arc<PinnedDirectory>,
        name: &OsStr,
        scratch: Arc<super::super::migration_review::InventoryScratch>,
        reservation: Option<Reservation>,
        journal_reservation: Option<Reservation>,
    ) -> io::Result<Self> {
        scratch.validate_metadata_destination(parent)?;
        let mut log = migration_journal_log::Log::open_in(parent, name, MAGIC)?;
        if log.file.metadata()?.len() > MAX as u64 {
            return Err(changed(
                "Home adoption journal must be bounded private metadata",
            ));
        }
        let Some(Record::Plan {
            nonce,
            bytes,
            sha256,
            segments: count,
        }) = migration_journal_log::read_record(&mut log.file, log.sequence)?
        else {
            return Err(changed("Home adoption journal is incomplete"));
        };
        log.sequence += 1;
        if count > segmented::SEGMENTS {
            return Err(changed("Adoption segmented manifest exceeds bounds"));
        }
        let mut segments = Vec::with_capacity(count);
        while segments.len() < count {
            let Some(Record::Segment(segment)) =
                migration_journal_log::read_record(&mut log.file, log.sequence)?
            else {
                return Err(changed("Home adoption journal is incomplete"));
            };
            log.sequence += 1;
            segments.push(segment);
        }
        let manifest = segmented::Manifest {
            nonce,
            bytes,
            sha256,
            segments,
        };
        let store = Store::new(scratch.clone(), Arc::new(AtomicBool::new(false)))?;
        #[cfg(all(test, feature = "native-capture-qualification"))]
        let kept = segmented::Manifest {
            nonce: manifest.nonce.clone(),
            bytes: manifest.bytes,
            sha256: manifest.sha256,
            segments: manifest.segments.clone(),
        };
        let mut plan_bytes =
            std::io::BufReader::new(manifest.reader(parent, &scratch, reservation)?);
        let mut line = replay::Line::new(&mut plan_bytes);
        let plan = line.parse(Arc::clone(&store))?;
        drop(line);
        let mut extra = [0u8; 1];
        if plan_bytes.read(&mut extra)? != 0 {
            return Err(changed("Adoption segmented plan has trailing bytes"));
        }
        let segment_reservation = plan_bytes.into_inner().into_reservation()?;
        validate_plan(&plan)?;
        let journal_reservations = if let Some(reservation) = journal_reservation {
            reservation.check()?;
            vec![reservation]
        } else {
            let mut existing = scratch.reserve_metadata()?;
            existing.retain_locked_journal(&log.file)?;
            let mut future = scratch.reserve_metadata()?;
            future.grow(checkpoint_reserve(&plan)?)?;
            vec![existing, future]
        };
        store.freeze()?;
        let mut done = Done::new(plan.entries.len(), store.scratch())?;
        let mut root_receipt = None;
        let mut end = log.file.stream_position()?;
        while let Some(record) = migration_journal_log::read_record(&mut log.file, log.sequence)? {
            match record {
                Record::Entry(index) => {
                    if index >= plan.entries.len() || !done.insert(index)? {
                        return Err(changed("Home adoption journal has an invalid receipt"));
                    }
                }
                Record::Root { root_acl: receipt } => {
                    let expected = plan
                        .root_acl
                        .as_ref()
                        .ok_or_else(|| changed("Unrequested root ACL receipt"))?
                        .receipt(&plan)?;
                    if root_receipt.is_some() || receipt != expected {
                        return Err(changed(
                            "Root ACL adoption receipt differs from durable intent",
                        ));
                    }
                    root_receipt = Some(receipt);
                }
                Record::Plan { .. } | Record::Segment(_) => {
                    return Err(changed("Home adoption journal has an invalid receipt"));
                }
            }
            log.sequence += 1;
            end = log.file.stream_position()?;
        }
        // An interrupted last append has no durable receipt; its move is
        // revalidated and repeated. No earlier receipt is removed.
        log.repair(end, || Ok(()))?;
        Ok(Self {
            log,
            plan,
            done,
            root_receipt,
            journal_reservations,
            segment_reservation,
            #[cfg(all(test, feature = "native-capture-qualification"))]
            manifest: kept,
        })
    }
    fn check_reservations(&self) -> io::Result<()> {
        for reservation in &self.journal_reservations {
            reservation.check()?;
        }
        self.segment_reservation.check()
    }
    fn checkpoint_root(&mut self, receipt: root_acl::Receipt) -> io::Result<()> {
        self.check_reservations()?;
        if self.root_receipt.is_some() {
            return Err(changed("Duplicate root ACL checkpoint"));
        }
        self.log.append(&Record::Root {
            root_acl: receipt.clone(),
        })?;
        self.root_receipt = Some(receipt);
        Ok(())
    }
    fn checkpoint(&mut self, index: usize) -> io::Result<()> {
        self.check_reservations()?;
        if self.done.contains(index)? {
            return Err(changed("Duplicate adoption checkpoint"));
        }
        self.log.append(&Record::Entry(index))?;
        if !self.done.insert(index)? {
            return Err(changed("Duplicate adoption checkpoint"));
        }
        Ok(())
    }
}

/// Publish one exact owned private inode with atomic no-replace semantics.
/// A crash leaves either its temporary name or committed name, never a second
/// writable hardlink alias that would invalidate immutable-file custody.
fn publish_temporary(
    parent: &Arc<PinnedDirectory>,
    temporary: &OwnedTemporary,
    destination: &OsStr,
) -> io::Result<()> {
    parent.validate_destination()?;
    temporary.validate()?;
    let source = CString::new(temporary.name.as_bytes()).map_err(io::Error::other)?;
    let destination = CString::new(destination.as_bytes()).map_err(io::Error::other)?;
    // SAFETY: the retained parent is live, names are terminated, and the
    // existing destination is atomically refused rather than overwritten.
    let result = unsafe {
        libc::renameat2(
            parent.file.as_raw_fd(),
            source.as_ptr(),
            parent.file.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The exact framed size of every receipt the journal may still append.
fn checkpoint_reserve(plan: &Plan) -> io::Result<u64> {
    let last = plan.entries.len().saturating_sub(1);
    let entries = (plan.entries.len() as u64)
        .checked_mul(frame_bytes(&Record::Entry(last))?)
        .ok_or_else(|| changed("Adoption checkpoint reserve overflow"))?;
    let root = match &plan.root_acl {
        Some(intent) => frame_bytes(&Record::Root {
            root_acl: intent.receipt(plan)?,
        })?,
        None => 0,
    };
    entries
        .checked_add(root)
        .ok_or_else(|| changed("Adoption checkpoint reserve overflow"))
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    fn held_tail(disconnect: bool) -> (PromotionWorker, mpsc::Sender<()>) {
        let (sender, receiver) = mpsc::channel();
        let (tail, wait) = mpsc::channel();
        let (entered, ready) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            if !disconnect {
                sender
                    .send(PromotionEvent::Failed("terminal failure".into()))
                    .unwrap();
            }
            drop(sender);
            entered.send(()).unwrap();
            wait.recv().unwrap();
        });
        tilo_test_env::recv("the worker thread to start", &ready);
        (
            PromotionWorker {
                cancel: Arc::new(AtomicBool::new(false)),
                receiver,
                finished: false,
                pending: None,
                thread: Some(thread),
            },
            tail,
        )
    }
    fn finish(mut worker: PromotionWorker, tail: mpsc::Sender<()>) {
        assert!(
            worker.try_next().is_none(),
            "terminal transport is not thread retirement"
        );
        assert!(!worker.observe_stopped());
        tail.send(()).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(event) = worker.try_next() {
                assert!(matches!(event, PromotionEvent::Failed(_)));
                assert!(worker.observe_stopped());
                assert!(worker.thread.is_none());
                assert!(worker.try_next().is_none());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn actual_terminal_event_does_not_release_running_tail() {
        let (worker, tail) = held_tail(false);
        finish(worker, tail);
    }
    #[test]
    fn actual_channel_eof_does_not_release_running_tail() {
        let (worker, tail) = held_tail(true);
        finish(worker, tail);
    }
}
