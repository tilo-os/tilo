//! The record as an index and one file per workspace, beside the state file
//! of the host that keeps it. The caller holds exclusive access to the
//! directory. A host that keeps state of its own derived from the record
//! (its workspaces of the workspaces) follows each publication through a
//! [`FileHost`].
//!
//! A publication writes the workspace it changed and the index, never the
//! record whole, so a device holding many workspaces pays for the one it
//! touched. The index names each workspace's file and the revision that file
//! was written at: it is the commit point of a change, and a workspace file
//! the index does not name at that revision is not the record's — a
//! publication that stopped between the two leaves the record as it was.
//!
//! A workspace's file is a snapshot. Each later publication of the
//! workspace alone appends what it changed to the snapshot's journal
//! ([`crate::owner::journal`]) and syncs it before it is installed, so a
//! write costs what it changed rather than all the workspace holds; once the
//! journal outgrows its snapshot, the next publication writes a snapshot
//! again. A publication that changes the index (an enrollment, a workspace
//! held or let go) writes the snapshot and the index as before.
//!
//! The index has names of its own ([`parts_of`], and `index.json` in it), so
//! the whole file a build before the split wrote is read once and then left
//! exactly as it is. A build that predates the split still finds its record
//! there — the one it last wrote, not this one — and never sees the index.
use crate::owner::journal;
use crate::owner::{
    Change, LimitedWriter, MAX_AUTHORITY_BYTES, NativeAuthorityStorage, NativeWorkspaceOwner,
    Store,
    record::{StoredPendingEnrollment, StoredWorkspace},
};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    io::{ErrorKind, Read as _},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};

/// The layout an index is: one snapshot per workspace, named with the
/// revision it was written at, and that snapshot's journal beside it. A build
/// that reads only the snapshots reads layout 1, and refuses this one rather
/// than lose what a journal holds.
const INDEX_VERSION: u32 = 2;
/// The layout before journals: the snapshots alone.
const INDEX_BEFORE_JOURNALS: u32 = 1;
/// A journal is appended to until it holds this much or as much as its
/// snapshot, whichever is more: rewriting the snapshot then costs at most
/// what the writes since the last one appended.
const JOURNAL_FLOOR: usize = 256 * 1024;

/// The file a host whose own state is the file `state` keeps its owner's
/// record in, beside it (`state.json` keeps it in `state.owner.json`).
///
/// A build before the split wrote the record to this file whole; this build
/// reads such a file once, keeps the record beside it from then on, and does
/// not touch the file again.
#[must_use]
pub fn owner_file_of(state: &Path) -> PathBuf {
    state.with_extension("owner.json")
}

/// What a host that keeps state derived from the record does around each
/// publication of it.
pub trait FileHost: std::any::Any + Send + Sync {
    /// The workspace a local slot is, where the host has slots.
    fn slot_workspace(&self, _slot: WorkspaceSlot) -> Option<WorkspaceId> {
        None
    }
    /// Run `write`, which makes `next` durable and installs it, with
    /// whatever the host keeps beside the record: it may refuse the
    /// publication before calling `write`, and follows `change` once
    /// `write` succeeded. Nothing is installed when `write` is not called
    /// or fails.
    ///
    /// # Errors
    /// The host refuses the publication, or `write` failed.
    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        write: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let _ = (next, change);
        write()
    }
}

struct NoHost;
impl FileHost for NoHost {}

/// The record's index: the revision it last published, the workspaces it
/// holds with the revision each one's file was written at, and the
/// enrollments in flight.
#[derive(Serialize, Deserialize)]
struct StoredIndex {
    version: u32,
    revision: u64,
    held: BTreeMap<WorkspaceId, u64>,
    #[serde(default)]
    pending: BTreeMap<WorkspaceId, StoredPendingEnrollment>,
}

/// The revisions each workspace's file was written at.
type Held = BTreeMap<WorkspaceId, u64>;

/// How the record file held the record.
enum Layout {
    /// As one file, by a build before the split: read once, then split.
    Whole,
    Index(Held),
}

/// The directory beside the record file that holds the record: its index and
/// one file per workspace.
pub(crate) fn parts_of(path: &Path) -> PathBuf {
    path.with_extension("workspaces")
}

/// The record's index, in the directory beside the record file.
///
/// It has a name of its own, and the record file a build before the split
/// wrote is left as it is: a build that predates the split reads that file
/// whole, as it always did, and never sees the index.
pub(crate) fn index_of(parts: &Path) -> PathBuf {
    parts.join("index.json")
}

/// Every file of the record at `path` as the disk holds it — the index and
/// each workspace's file — sorted by path.
#[cfg(test)]
pub(crate) fn record_files(path: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>, std::io::Error> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(parts_of(path)) else {
        return Ok(files);
    };
    for entry in entries {
        let entry = entry?;
        files.push((entry.path(), std::fs::read(entry.path())?));
    }
    files.sort();
    Ok(files)
}

/// The file one workspace's record is in, as written at `revision`.
fn part_of(parts: &Path, workspace: WorkspaceId, revision: u64) -> PathBuf {
    parts.join(format!("{workspace}.{revision}.json"))
}

/// The file a run of `workspace`'s outgoing custody named `hash` is kept in.
fn custody_run_of(parts: &Path, workspace: WorkspaceId, hash: [u8; 32]) -> PathBuf {
    parts.join(crate::owner::custody_run_name(workspace, hash))
}

/// The journal of the workspace file written at `revision`.
fn log_of(parts: &Path, workspace: WorkspaceId, revision: u64) -> PathBuf {
    parts.join(format!("{workspace}.{revision}.log"))
}

/// The workspace and revision a workspace file or its journal is named with,
/// or `None` for a file the record did not write.
fn part_name(name: &OsStr) -> Option<(WorkspaceId, u64)> {
    let name = name.to_str()?;
    let stem = name
        .strip_suffix(".json")
        .or_else(|| name.strip_suffix(".log"))?;
    let (workspace, revision) = stem.rsplit_once('.')?;
    Some((WorkspaceId::parse(workspace)?, revision.parse().ok()?))
}

fn provision(parts: &Path) -> Result<(), String> {
    if parts.is_dir() {
        return Ok(());
    }
    tilo_files::native::atomic::provision_directory(
        parts,
        tilo_files::native::atomic::Privacy::OwnerOnly,
    )
    .map_err(|error| error.into_io_error().to_string())?;
    Ok(())
}

/// Read the bounded file at `path`, or `None` when there is none.
fn read_bounded(path: &Path) -> Result<Option<Vec<u8>>, String> {
    read_within(path, MAX_AUTHORITY_BYTES)
}

/// Read the file at `path` of at most `cap` bytes, or `None` when there is none.
fn read_within(path: &Path, cap: usize) -> Result<Option<Vec<u8>>, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    file.take((cap as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > cap {
        return Err("joined-workspace record exceeds its cap".into());
    }
    Ok(Some(bytes))
}

/// The record at `path`, empty when there is none, and how it is held: the
/// split layout when the index is there, otherwise the whole file a build
/// before the split wrote.
fn read_record(path: &Path) -> Result<(NativeAuthorityStorage, Layout), String> {
    let parts = parts_of(path);
    if let Some(bytes) = read_bounded(&index_of(&parts))? {
        let index: StoredIndex = serde_json::from_slice(&bytes)
            .map_err(|error| format!("the record's index does not parse: {error}"))?;
        return read_index(path, index);
    }
    let Some(bytes) = read_bounded(path)? else {
        return Ok((
            NativeAuthorityStorage::default(),
            Layout::Index(Held::new()),
        ));
    };
    // A record written as one file, by a build before the split: read whole,
    // and kept beside it from here on, leaving this file as it is.
    let mut record: NativeAuthorityStorage =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    drop_recovery_projections(&mut record);
    Ok((record, Layout::Whole))
}

fn read_index(path: &Path, index: StoredIndex) -> Result<(NativeAuthorityStorage, Layout), String> {
    if index.version != INDEX_VERSION && index.version != INDEX_BEFORE_JOURNALS {
        return Err(format!(
            "the owner record's index is version {}, and this build reads version {INDEX_VERSION}",
            index.version
        ));
    }
    let parts = parts_of(path);
    let mut workspaces = BTreeMap::new();
    let mut latest = index.revision;
    for (&workspace, &revision) in &index.held {
        let part = part_of(&parts, workspace, revision);
        let bytes = read_bounded(&part)?
            .ok_or_else(|| format!("the record names {workspace}'s file and it is missing"))?;
        let mut stored: StoredWorkspace =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        // A journal holds at most its snapshot's size past the larger of
        // that and its floor, and one entry, itself within the cap.
        let journal_cap = MAX_AUTHORITY_BYTES
            .saturating_mul(2)
            .saturating_add(JOURNAL_FLOOR);
        if let Some(log) = read_within(&log_of(&parts, workspace, revision), journal_cap)? {
            let (replayed, last) = journal::replay(stored, &log, MAX_AUTHORITY_BYTES)?;
            stored = replayed;
            latest = latest.max(last.unwrap_or(0));
        }
        if stored.workspace != workspace {
            return Err(format!(
                "{workspace}'s file holds another workspace's record"
            ));
        }
        workspaces.insert(workspace, stored);
    }
    let mut record = NativeAuthorityStorage {
        revision: latest,
        workspaces,
        pending: index.pending,
    };
    drop_recovery_projections(&mut record);
    Ok((record, Layout::Index(index.held)))
}

/// A recovery projection is a re-signed copy of frames this build re-sends
/// as their signed originals. A record that holds one still loads; the copy
/// is dropped here, and the record keeps its originals in custody until the
/// workspace retires them.
fn drop_recovery_projections(record: &mut NativeAuthorityStorage) {
    record.workspaces.values_mut().for_each(|workspace| {
        if let Some(publication) = workspace.publication.as_mut() {
            publication.projection.clear();
        }
    });
}

/// Compact JSON of one workspace's record, within its file's cap.
fn part_bytes(stored: &StoredWorkspace) -> Result<Vec<u8>, String> {
    crate::owner::limits::preflight(stored, MAX_AUTHORITY_BYTES)
        .map_err(|error| error.to_string())?;
    let mut bytes = LimitedWriter::new(Vec::new(), MAX_AUTHORITY_BYTES);
    serde_json::to_writer(&mut bytes, stored).map_err(|error| error.to_string())?;
    Ok(bytes.into_inner())
}

/// Compact JSON of the index of `record`, which names each held workspace's
/// file at `held`'s revision.
fn index_bytes(record: &NativeAuthorityStorage, held: &Held) -> Result<Vec<u8>, String> {
    let index = StoredIndex {
        version: INDEX_VERSION,
        revision: record.revision,
        held: held.clone(),
        pending: record.pending.clone(),
    };
    let mut bytes = LimitedWriter::new(Vec::new(), MAX_AUTHORITY_BYTES);
    serde_json::to_writer(&mut bytes, &index).map_err(|error| error.to_string())?;
    Ok(bytes.into_inner())
}

/// Read the record kept at `path`: the index beside it and the workspace
/// files the index names, or the whole file a build before the split wrote
/// there. Empty when there is neither.
pub fn read_file(path: &Path) -> Result<NativeAuthorityStorage, String> {
    read_record(path).map(|(record, _)| record)
}

/// Open the owner whose record is the file at `path`, starting empty.
pub fn open_file(path: &Path) -> Result<NativeWorkspaceOwner, String> {
    open_file_with(path, Arc::new(NoHost))
}

/// [`open_file`] for a host that follows each publication through `host`.
pub fn open_file_with(
    path: &Path,
    host: Arc<dyn FileHost>,
) -> Result<NativeWorkspaceOwner, String> {
    let (mut authority, layout) = read_record(path)?;
    let migrated = crate::owner::enrollment::migrate_intents(&mut authority)?;
    let whole = matches!(layout, Layout::Whole);
    let held = match layout {
        Layout::Whole => Held::new(),
        Layout::Index(held) => held,
    };
    let store = Arc::new(FileStore::at(path, host, held));
    let owner = NativeWorkspaceOwner::restore(
        Arc::clone(&store) as Arc<dyn Store>,
        authority,
        tilo_clock::wall(),
    )?;
    if whole || migrated {
        // The split layout of a record written as one file, or the form of
        // records already there. Either changes neither a workspace nor its
        // relay workspace, so it goes straight to the files rather than
        // through a publication of the store: nothing a host follows has
        // changed, and the file the record was read from is left as it is.
        let record = owner.0.authority.read();
        store.write_all(&record)?;
    } else {
        // A publication that stopped between a workspace's file and the index
        // left a file the record does not name.
        store.prune_parts(&owner.0.authority.read());
    }
    Ok(owner)
}

/// A snapshot's journal, as this store last appended to it.
struct Journal {
    /// The revision of the snapshot it continues.
    revision: u64,
    /// What its next entry is a delta of.
    written: journal::Written,
    snapshot: usize,
    appended: usize,
}

pub(crate) struct FileStore {
    pub(crate) host: Arc<dyn FileHost>,
    parts: PathBuf,
    history: crate::owner::HistoryContent,
    /// The revision each held workspace's file was written at.
    held: RwLock<Held>,
    /// The journal of each workspace's snapshot this store wrote since it
    /// opened: a workspace read from disk writes a snapshot first.
    journals: Mutex<BTreeMap<WorkspaceId, Journal>>,
    /// The enrollments in flight the index last written names.
    pending: Mutex<Option<Vec<u8>>>,
    /// Every byte this store wrote.
    #[cfg(test)]
    pub(crate) written: std::sync::atomic::AtomicUsize,
    healthy: AtomicBool,
    uncertain: RwLock<BTreeSet<WorkspaceId>>,
}

impl FileStore {
    fn at(path: &Path, host: Arc<dyn FileHost>, held: Held) -> Self {
        Self {
            host,
            parts: parts_of(path),
            history: crate::owner::HistoryContent::for_state_file(path),
            held: RwLock::new(held),
            journals: Mutex::new(BTreeMap::new()),
            pending: Mutex::new(None),
            #[cfg(test)]
            written: std::sync::atomic::AtomicUsize::new(0),
            healthy: AtomicBool::new(true),
            uncertain: RwLock::new(BTreeSet::new()),
        }
    }

    /// Make `record` durable whole: every workspace's file at the record's
    /// revision, then the index that names them.
    fn write_all(&self, record: &NativeAuthorityStorage) -> Result<(), String> {
        let held = record
            .workspaces
            .keys()
            .map(|workspace| (*workspace, record.revision))
            .collect::<Held>();
        for (workspace, stored) in &record.workspaces {
            self.write_part(*workspace, record.revision, stored, record)?;
        }
        self.write_index(record, &held)?;
        *self.held.write() = held;
        self.prune_parts(record);
        Ok(())
    }

    fn write_part(
        &self,
        workspace: WorkspaceId,
        revision: u64,
        stored: &StoredWorkspace,
        record: &NativeAuthorityStorage,
    ) -> Result<(), String> {
        provision(&self.parts)?;
        let bytes = part_bytes(stored)?;
        // The journal, empty, first: the index names the snapshot only
        // after both are there, and a journal the index does not name is
        // pruned.
        let written = journal::first(stored, &bytes)?;
        self.replace(&log_of(&self.parts, workspace, revision), &[], record)?;
        self.replace(&part_of(&self.parts, workspace, revision), &bytes, record)?;
        self.journals.lock().insert(
            workspace,
            Journal {
                revision,
                written,
                snapshot: bytes.len(),
                appended: 0,
            },
        );
        Ok(())
    }

    fn write_index(&self, record: &NativeAuthorityStorage, held: &Held) -> Result<(), String> {
        provision(&self.parts)?;
        let bytes = index_bytes(record, held)?;
        self.replace(&index_of(&self.parts), &bytes, record)?;
        *self.pending.lock() =
            Some(serde_json::to_vec(&record.pending).map_err(|error| error.to_string())?);
        Ok(())
    }

    /// Append `workspace`'s publication in `next` to its snapshot's journal,
    /// synced: `false`, with nothing written, when the publication changes
    /// what the index says, or the journal is due a new snapshot.
    fn append(
        &self,
        next: &NativeAuthorityStorage,
        workspace: WorkspaceId,
    ) -> Result<bool, String> {
        let snapshot = {
            let held = self.held.read();
            if held.len() != next.workspaces.len()
                || next
                    .workspaces
                    .keys()
                    .any(|held_here| !held.contains_key(held_here))
            {
                return Ok(false);
            }
            held.get(&workspace).copied()
        };
        let pending = serde_json::to_vec(&next.pending).map_err(|error| error.to_string())?;
        if self.pending.lock().as_ref() != Some(&pending) {
            return Ok(false);
        }
        let Some(stored) = next.workspaces.get(&workspace) else {
            return Ok(false);
        };
        let mut journals = self.journals.lock();
        let Some(journal) = journals
            .get_mut(&workspace)
            .filter(|journal| snapshot == Some(journal.revision))
        else {
            return Ok(false);
        };
        let (entry, written) = journal::next(&journal.written, stored, next.revision)?;
        let appended = journal.appended.saturating_add(entry.len());
        // A journal past its snapshot is due a new one; and a record near
        // its cap is written whole, where the cap is checked exactly.
        if appended > journal.snapshot.max(JOURNAL_FLOOR)
            || stored.peer.len().saturating_mul(2) > MAX_AUTHORITY_BYTES
        {
            return Ok(false);
        }
        let path = log_of(&self.parts, workspace, journal.revision);
        if let Err(error) = tilo_files::native::atomic::append(&path, &entry) {
            if error.state() != tilo_files::native::atomic::PublicationState::NotPublished {
                // Some of the entry may be on disk: the record is read back
                // before anything else is published.
                self.uncertain.write().extend(next.workspaces());
                self.healthy.store(false, Ordering::Release);
            }
            return Err(error.into_io_error().to_string());
        }
        #[cfg(test)]
        self.written.fetch_add(entry.len(), Ordering::Relaxed);
        journal.written = written;
        journal.appended = appended;
        drop(journals);
        Ok(true)
    }

    /// Replace a file of the record durably. A replacement whose publication
    /// may have happened fences the store: the record the disk holds is then
    /// read back before anything else is published.
    fn replace(
        &self,
        path: &Path,
        bytes: &[u8],
        next: &NativeAuthorityStorage,
    ) -> Result<(), String> {
        match tilo_files::native::atomic::replace(
            path,
            bytes,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        ) {
            Ok(()) => {
                #[cfg(test)]
                self.written.fetch_add(bytes.len(), Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                if error.state() != tilo_files::native::atomic::PublicationState::NotPublished {
                    self.uncertain.write().extend(next.workspaces());
                    self.healthy.store(false, Ordering::Release);
                }
                Err(error.into_io_error().to_string())
            }
        }
    }

    /// Remove the workspace files `record`, the record as published, does not name: the one a
    /// workspace was published over, one a release left behind, one a publication stopped
    /// before its index wrote, and the runs of custody that moved to its head.
    fn prune_parts(&self, record: &NativeAuthorityStorage) {
        let Ok(entries) = std::fs::read_dir(&self.parts) else {
            return;
        };
        let index = index_of(&self.parts);
        let held = self.held.read();
        let runs: BTreeSet<PathBuf> = record
            .custody_runs()
            .map(|name| self.parts.join(name))
            .collect();
        for entry in entries.flatten() {
            let path = entry.path();
            if path == index || runs.contains(&path) {
                continue;
            }
            if path
                .file_name()
                .and_then(part_name)
                .is_some_and(|(workspace, revision)| held.get(&workspace) == Some(&revision))
            {
                continue;
            }
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != ErrorKind::NotFound
            {
                tracing::warn!(%error, "a workspace file the record does not name was kept");
            }
        }
    }

    /// Write what `change` changed and only that: the changed workspace's
    /// file at the new revision and the index that names it, or the index
    /// alone for an enrollment in flight.
    fn write(
        &self,
        next: &NativeAuthorityStorage,
        changed: Option<WorkspaceId>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        if let Some(workspace) = changed
            && self.append(next, workspace)?
        {
            install();
            return Ok(());
        }
        let mut held = self.held.read().clone();
        held.retain(|workspace, _| next.holds(*workspace));
        self.journals
            .lock()
            .retain(|workspace, _| next.holds(*workspace));
        if let Some(workspace) = changed {
            let stored = next
                .workspaces
                .get(&workspace)
                .ok_or("the published workspace is not in the record")?;
            self.write_part(workspace, next.revision, stored, next)?;
            held.insert(workspace, next.revision);
        }
        // A workspace this store has no file for yet is written at this
        // revision: the index names only files that are there.
        for (workspace, stored) in &next.workspaces {
            if held.contains_key(workspace) {
                continue;
            }
            self.write_part(*workspace, next.revision, stored, next)?;
            held.insert(*workspace, next.revision);
        }
        // The index names each workspace's file at the revision it was
        // written at, and is the commit point: once it is written, the record
        // holds the change, and the file it named before is not the record's.
        self.write_index(next, &held)?;
        *self.held.write() = held;
        install();
        self.prune_parts(next);
        Ok(())
    }
}

impl Store for FileStore {
    fn put_history(&self, bytes: &[u8]) -> Result<[u8; 32], String> {
        self.history.put(bytes)
    }
    fn read_history(&self, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        self.history.read(hash)
    }
    fn put_custody(&self, workspace: WorkspaceId, bytes: &[u8]) -> Result<[u8; 32], String> {
        if bytes.len() > crate::owner::publication::MAX_RUN_BYTES {
            return Err("a run of outgoing custody exceeds its cap".into());
        }
        provision(&self.parts)?;
        let hash = *blake3::hash(bytes).as_bytes();
        // Named by its contents: a run kept already is the same run, and nothing names a new
        // one before this returns, so it never fences the store.
        tilo_files::native::atomic::replace(
            &custody_run_of(&self.parts, workspace, hash),
            bytes,
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )
        .map_err(|error| error.into_io_error().to_string())?;
        Ok(hash)
    }
    fn read_custody(&self, workspace: WorkspaceId, hash: [u8; 32]) -> Result<Vec<u8>, String> {
        let path = custody_run_of(&self.parts, workspace, hash);
        let file = std::fs::File::open(&path).map_err(|error| error.to_string())?;
        let limit = crate::owner::publication::MAX_RUN_BYTES;
        let mut bytes = Vec::new();
        file.take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > limit {
            return Err("a run of outgoing custody exceeds its cap".into());
        }
        Ok(bytes)
    }
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn uncertain(&self, workspace: WorkspaceId) -> bool {
        self.uncertain.read().contains(&workspace)
    }

    fn slot_workspace(&self, slot: WorkspaceSlot) -> Option<WorkspaceId> {
        self.host.slot_workspace(slot)
    }

    fn publish(
        &self,
        next: &NativeAuthorityStorage,
        change: Change<'_>,
        install: &mut dyn FnMut(),
    ) -> Result<(), String> {
        if !self.healthy() {
            return Err("joined-workspace store is fenced after an uncertain publication".into());
        }
        let changed = match &change {
            Change::Workspace { workspace, .. } => Some(*workspace),
            Change::Pending | Change::Released { .. } => None,
        };
        self.host
            .publish(next, change, &mut || self.write(next, changed, install))
    }
}

#[cfg(test)]
#[path = "file_store_tests.rs"]
mod tests;
