//! Native V3 receipts. No socket, nonce or generation token is restart authority.
use super::migration::{CopyReport, PinnedDirectory, changed, check_cancel};
use super::migration_journal::{PersistenceBoundary, persistence_result};
use super::migration_journal_log::{Log, read_record};
use super::migration_native::{Plan, PlanEntry};
use super::{migration_durability as durability, nofollow::metadata_nofollow};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Seek},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use tilo_ntfs_protocol::{Entry, Identity, Issue, NativePath};
const MAGIC: &[u8; 8] = b"TILOJL3\n";

// Finite history for this bounded inventory: eight conservative recovery
// envelopes, not renewable session/native budgets. Exhaustion requires review;
// never compact, delete output, or repair an over-budget journal implicitly.
#[derive(Clone, Copy)]
struct HistoryBudget {
    frames: u64,
    bytes: u64,
}
impl HistoryBudget {
    fn for_plan(plan: &Plan) -> io::Result<Self> {
        let entries = u64::try_from(plan.entries.len())
            .map_err(|_| changed("native inventory size overflow"))?;
        let issues =
            u64::try_from(plan.issues.len()).map_err(|_| changed("native issue count overflow"))?;
        let fidelity = plan.entries.iter().try_fold(0u64, |total, entry| {
            total
                .checked_add(
                    u64::try_from(entry.entry.fidelity_pending.len())
                        .map_err(|_| changed("native fidelity count overflow"))?,
                )
                .ok_or_else(|| changed("native fidelity count overflow"))
        })?;
        let envelope = entries
            .checked_mul(4)
            .and_then(|n| n.checked_add(issues))
            .and_then(|n| n.checked_add(fidelity))
            .and_then(|n| n.checked_add(4))
            .ok_or_else(|| changed("native history frame overflow"))?;
        let frames = entries
            .checked_add(issues)
            .and_then(|n| n.checked_add(2))
            .and_then(|n| {
                envelope
                    .checked_mul(8)
                    .and_then(|extra| n.checked_add(extra))
            })
            .ok_or_else(|| changed("native history frame overflow"))?;
        let encoded = u64::try_from(
            serde_json::to_vec(&(&plan.identity, &plan.entries, &plan.issues))?.len(),
        )
        .map_err(|_| changed("native history byte overflow"))?;
        let bytes = encoded
            .checked_mul(24)
            .and_then(|n| n.checked_add(128 * 1024))
            .ok_or_else(|| changed("native history byte overflow"))?
            .min(16 * 1024 * 1024);
        Ok(Self { frames, bytes })
    }
    fn check_log(self, log: &Log) -> io::Result<()> {
        if log.file.metadata()?.len() > self.bytes || log.sequence >= self.frames {
            return Err(changed(
                "native journal history budget exhausted; preserve output and review",
            ));
        }
        Ok(())
    }
    fn reserve(self, log: &Log, records: &[Record]) -> io::Result<()> {
        let mut bytes = log.file.metadata()?.len();
        let mut frames = log.sequence;
        for record in records {
            let size = serde_json::to_vec(record)?.len();
            if size > super::migration_journal_log::MAX_RECORD {
                return Err(changed("native journal record exceeds size bound"));
            }
            bytes = bytes
                .checked_add(
                    u64::try_from(size).map_err(|_| changed("native frame size overflow"))?,
                )
                .and_then(|n| n.checked_add(44))
                .ok_or_else(|| changed("native history byte overflow"))?;
            frames = frames
                .checked_add(1)
                .ok_or_else(|| changed("native history frame overflow"))?;
        }
        if bytes > self.bytes || frames > self.frames {
            return Err(changed(
                "native journal history budget exhausted; preserve output and review",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    operation: String,
    source: Identity,
    revision: [u8; 32],
    selected_root: String,
    destination: PathBuf,
    destination_parent: durability::Identity,
    journal: PathBuf,
    mapping_version: u32,
    fidelity_version: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    #[serde(with = "super::migration_native::entry_facts")]
    pub source: Entry,
    pub destination: durability::Identity,
    pub bytes: u64,
    pub digest: Option<[u8; 32]>,
    pub completed: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "record", deny_unknown_fields)]
enum Record {
    Header {
        header: Header,
    },
    Inventory {
        entry: PlanEntry,
    },
    InventoryIssue {
        issue: Issue,
    },
    Seal {
        revision: [u8; 32],
    },
    Receipt {
        receipt: Receipt,
    },
    OutcomeStart {
        cancelled: bool,
        scan_completed: bool,
    },
    OutcomeIssue {
        path: PathBuf,
        message: String,
        failed: bool,
    },
    Fidelity {
        path: NativePath,
        message: String,
    },
    CommitOutcome,
}
pub struct NativeJournal<'a> {
    cancel: &'a AtomicBool,
    budget: HistoryBudget,
    log: Log,
    pub receipts: BTreeMap<NativePath, Receipt>,
    pub destination: PathBuf,
    pub parent: Arc<PinnedDirectory>,
    completed: BTreeMap<NativePath, cap_primitives::fs::Metadata>,
}
pub fn directory_identity(directory: &PinnedDirectory) -> io::Result<durability::Identity> {
    durability::identity(&durability::readable_directory(&directory.file)?)
}
impl<'a> NativeJournal<'a> {
    pub fn create(
        plan: &Plan,
        destination: &Path,
        journal: &Path,
        cancel: &'a AtomicBool,
    ) -> io::Result<Self> {
        check_cancel(cancel)?;
        let budget = HistoryBudget::for_plan(plan)?;
        durability::supported()?;
        if !destination.is_absolute()
            || destination.as_os_str().len() > 4096
            || destination.file_name().is_none()
        {
            return Err(changed("invalid native destination grant"));
        }
        let parent = PinnedDirectory::open_absolute(
            destination
                .parent()
                .ok_or_else(|| changed("destination parent missing"))?,
        )?;
        parent.validate_destination()?;
        if journal.starts_with(destination)
            || destination.starts_with(
                journal
                    .parent()
                    .ok_or_else(|| changed("journal parent missing"))?,
            )
        {
            return Err(changed(
                "native journal must be in a separate private directory outside output",
            ));
        }
        match metadata_nofollow(
            &parent.file,
            Path::new(
                destination
                    .file_name()
                    .ok_or_else(|| changed("destination name missing"))?,
            ),
        ) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            _ => return Err(changed("native destination must be fresh")),
        }
        let header = Header {
            operation: format!("{}-{:?}", std::process::id(), std::time::SystemTime::now()),
            source: plan.identity.clone(),
            revision: plan.revision()?,
            selected_root: plan.identity.root.clone(),
            destination: destination.to_path_buf(),
            destination_parent: directory_identity(&parent)?,
            journal: journal.to_path_buf(),
            mapping_version: 1,
            fidelity_version: 1,
        };
        let log = Log::create(
            journal,
            *MAGIC,
            &Record::Header {
                header: header.clone(),
            },
        )?;
        let mut journal = Self {
            cancel,
            budget,
            log,
            receipts: BTreeMap::new(),
            destination: destination.to_path_buf(),
            parent,
            completed: BTreeMap::new(),
        };
        for entry in &plan.entries {
            journal.append(&Record::Inventory {
                entry: entry.clone(),
            })?;
        }
        for issue in &plan.issues {
            journal.append(&Record::InventoryIssue {
                issue: issue.clone(),
            })?;
        }
        journal.append(&Record::Seal {
            revision: header.revision,
        })?;
        Ok(journal)
    }
    pub fn open(
        plan: &Plan,
        path: &Path,
        cancel: &'a AtomicBool,
        validate_source: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        check_cancel(cancel)?;
        let budget = HistoryBudget::for_plan(plan)?;
        let mut log = Log::open(path, *MAGIC)?;
        budget.check_log(&log)?;
        let Some(Record::Header { header }) = read_record(&mut log.file, log.sequence)? else {
            return Err(changed("native journal header missing"));
        };
        log.sequence += 1;
        if header.mapping_version != 1
            || header.fidelity_version != 1
            || header.operation.is_empty()
            || header.operation.len() > 128
            || header.source != plan.identity
            || header.revision != plan.revision()?
            || header.journal != path
            || header.selected_root != plan.identity.root
            || !header.destination.is_absolute()
            || header.destination.as_os_str().len() > 4096
        {
            return Err(changed(
                "fresh native selection or journal location disagrees",
            ));
        }
        let parent = PinnedDirectory::open_absolute(
            header
                .destination
                .parent()
                .ok_or_else(|| changed("native destination parent missing"))?,
        )?;
        parent.validate_destination()?;
        if directory_identity(&parent)? != header.destination_parent {
            return Err(changed("native destination parent identity changed"));
        }
        let (receipts, end) = replay(&mut log, &header, plan, budget, cancel)?;
        let completed = validate_receipts(plan, &header, &receipts, cancel)?;
        validate_output_names(plan, &header, &receipts, cancel)?;
        if !receipts.contains_key(&Vec::new()) && header.destination.try_exists()? {
            return Err(changed(
                "unreceipted native destination root is a preserved conflict",
            ));
        }
        log.validate()?;
        check_cancel(cancel)?;
        validate_source()?;
        log.repair(end, || {
            check_cancel(cancel)?;
            parent.validate_destination()?;
            if directory_identity(&parent)? != header.destination_parent {
                return Err(changed("destination parent changed before journal repair"));
            }
            for (path, before) in &completed {
                let relative = plan
                    .entries
                    .iter()
                    .find(|e| &e.entry.path == path)
                    .and_then(|e| e.mapped.as_ref())
                    .ok_or_else(|| changed("completed directory mapping missing"))?;
                let directory = PinnedDirectory::open_absolute(&header.destination.join(relative))?;
                validate_completed_snapshot(before, &directory)?;
            }
            Ok(())
        })?;
        Ok(Self {
            cancel,
            budget,
            destination: header.destination.clone(),
            log,
            receipts,
            parent,
            completed,
        })
    }
    pub fn validate_completed(
        &self,
        entry: &Entry,
        directory: &PinnedDirectory,
    ) -> io::Result<bool> {
        let Some(before) = self.completed.get(&entry.path) else {
            return Ok(false);
        };
        validate_completed_snapshot(before, directory)?;
        Ok(true)
    }
    fn append(&mut self, record: &Record) -> io::Result<()> {
        check_cancel(self.cancel)?;
        self.budget
            .reserve(&self.log, std::slice::from_ref(record))?;
        self.log.append(record)
    }
    pub fn save(&mut self, receipt: Receipt) -> io::Result<()> {
        persistence_result(
            (|| {
                self.parent.validate_destination()?;
                #[cfg(test)]
                if !receipt.source.directory {
                    super::migration_journal::crash_injection::boundary(
                        "native_before_file_receipt",
                    );
                    super::migration_journal::persistence_injection::fail_at(
                        "native_file_receipt",
                    )?;
                }
                self.append(&Record::Receipt {
                    receipt: receipt.clone(),
                })?;
                #[cfg(test)]
                if !receipt.source.directory {
                    super::migration_journal::crash_injection::boundary(
                        "native_after_file_receipt",
                    );
                }
                self.receipts.insert(receipt.source.path.clone(), receipt);
                Ok(())
            })(),
            PersistenceBoundary::OutputUnrecorded,
        )
    }
    pub fn finish(&mut self, plan: &Plan, report: &CopyReport) -> io::Result<()> {
        let cancelled = self.cancel.load(std::sync::atomic::Ordering::Relaxed) || report.cancelled;
        let mut records = vec![Record::OutcomeStart {
            cancelled,
            scan_completed: report.scan_completed && !cancelled,
        }];
        // A cancellation can persist only this two-frame terminal marker. It
        // does not start a new scan of per-path details after cancellation.
        if !cancelled {
            for (failed, entries) in [(true, &report.failed), (false, &report.skipped)] {
                for (path, message) in entries {
                    check_cancel(self.cancel)?;
                    records.push(Record::OutcomeIssue {
                        path: path.clone(),
                        message: message.clone(),
                        failed,
                    });
                }
            }
            for entry in &plan.entries {
                for reason in &entry.entry.fidelity_pending {
                    check_cancel(self.cancel)?;
                    records.push(Record::Fidelity {
                        path: entry.entry.path.clone(),
                        message: reason.clone(),
                    });
                }
            }
        }
        records.push(Record::CommitOutcome);
        // Reserve the entire outcome before writing any of its frames.
        self.budget.reserve(&self.log, &records)?;
        for record in records {
            if !cancelled {
                check_cancel(self.cancel)?;
            }
            self.log.append(&record)?;
        }
        Ok(())
    }
    pub fn record_directory(
        &mut self,
        entry: &Entry,
        directory: &PinnedDirectory,
        completed: bool,
    ) -> io::Result<()> {
        persistence_result(
            (|| {
                self.save(Receipt {
                    source: entry.clone(),
                    destination: directory_identity(directory)?,
                    bytes: 0,
                    digest: None,
                    completed,
                })
            })(),
            PersistenceBoundary::OutputUnrecorded,
        )
    }
    pub fn root(&mut self, entry: &Entry) -> io::Result<Arc<PinnedDirectory>> {
        let name = self
            .destination
            .file_name()
            .ok_or_else(|| changed("destination filename missing"))?;
        let root = if let Some(receipt) = self.receipts.get(&Vec::new()) {
            let root = PinnedDirectory::open_absolute(&self.destination)?;
            if directory_identity(&root)? != receipt.destination {
                return Err(changed("native destination root changed"));
            }
            self.validate_completed(entry, &root)?;
            root
        } else {
            let root = self.parent.create_child(name)?;
            sync_created_directory(&root, &self.parent)?;
            self.record_directory(entry, &root, false)?;
            root
        };
        root.validate_destination()?;
        Ok(root)
    }
}
fn validate_receipts(
    plan: &Plan,
    header: &Header,
    receipts: &BTreeMap<NativePath, Receipt>,
    cancel: &AtomicBool,
) -> io::Result<BTreeMap<NativePath, cap_primitives::fs::Metadata>> {
    let mut completed = BTreeMap::new();
    // Validate all retained directory/file identities before modifying a torn tail.
    for (native, receipt) in receipts {
        check_cancel(cancel)?;
        let mapped = plan
            .entries
            .iter()
            .find(|e| &e.entry.path == native)
            .and_then(|e| e.mapped.as_ref())
            .ok_or_else(|| changed("receipt mapping missing"))?;
        let full = header.destination.join(mapped);
        let identity = if receipt.source.directory {
            let directory = PinnedDirectory::open_absolute(&full)?;
            if receipt.completed {
                let meta = cap_primitives::fs::Metadata::from_file(&directory.file)?;
                let expected = super::migration_native::times(&receipt.source)?;
                if meta.accessed()?.into_std() != expected.accessed
                    || meta.modified()?.into_std() != expected.modified
                {
                    return Err(changed(
                        "completed destination directory timestamps changed",
                    ));
                }
                completed.insert(native.clone(), meta);
            }
            directory_identity(&directory)?
        } else {
            let parent = PinnedDirectory::open_absolute(
                full.parent()
                    .ok_or_else(|| changed("receipt parent missing"))?,
            )?;
            let file = open_file(
                &parent,
                full.file_name()
                    .ok_or_else(|| changed("receipt filename missing"))?,
            )?;
            durability::identity(&file)?
        };
        if identity != receipt.destination {
            return Err(changed("native receipted destination changed"));
        }
    }
    Ok(completed)
}
fn validate_completed_snapshot(
    before: &cap_primitives::fs::Metadata,
    directory: &PinnedDirectory,
) -> io::Result<()> {
    directory.validate_destination()?;
    if !super::migration::same_snapshot(
        before,
        &cap_primitives::fs::Metadata::from_file(&directory.file)?,
    ) {
        return Err(changed(
            "completed destination directory changed during resume",
        ));
    }
    Ok(())
}
pub fn sync_created_directory(
    directory: &PinnedDirectory,
    parent: &PinnedDirectory,
) -> io::Result<()> {
    persistence_result(
        (|| {
            durability::sync_directory(&directory.file)?;
            durability::sync_directory(&parent.file)
        })(),
        PersistenceBoundary::OutputPublicationUncertain,
    )
}
fn validate_output_names(
    plan: &Plan,
    header: &Header,
    receipts: &BTreeMap<NativePath, Receipt>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let expected: BTreeSet<PathBuf> = receipts
        .keys()
        .map(|path| {
            plan.entries
                .iter()
                .find(|e| &e.entry.path == path)
                .and_then(|e| e.mapped.clone())
                .ok_or_else(|| changed("native receipt mapping missing"))
        })
        .collect::<io::Result<_>>()?;
    let mut total = 0usize;
    for (path, receipt) in receipts.iter().filter(|(_, r)| r.source.directory) {
        let relative = plan
            .entries
            .iter()
            .find(|e| &e.entry.path == path)
            .and_then(|e| e.mapped.as_ref())
            .ok_or_else(|| changed("native directory mapping missing"))?;
        let directory = PinnedDirectory::open_absolute(&header.destination.join(relative))?;
        directory.validate_destination()?;
        if directory_identity(&directory)? != receipt.destination {
            return Err(changed("native directory changed during output validation"));
        }
        let entries =
            cap_std::fs::Dir::from_std_file(durability::readable_directory(&directory.file)?);
        for entry in entries.entries()? {
            check_cancel(cancel)?;
            total = total
                .checked_add(1)
                .ok_or_else(|| changed("native output entry overflow"))?;
            if total > 4096 || !expected.contains(&relative.join(entry?.file_name())) {
                return Err(changed(
                    "unreceipted native output or abandoned temporary is a preserved conflict",
                ));
            }
        }
        directory.validate_destination()?;
    }
    Ok(())
}
fn replay(
    log: &mut Log,
    header: &Header,
    plan: &Plan,
    budget: HistoryBudget,
    cancel: &AtomicBool,
) -> io::Result<(BTreeMap<NativePath, Receipt>, u64)> {
    let mut inventory = Vec::new();
    let mut issues = Vec::new();
    let mut sealed = false;
    let mut receipts = BTreeMap::<NativePath, Receipt>::new();
    let mut end = log.file.stream_position()?;
    let mut outcome = false;
    let mut details = 0usize;
    let mut inventory_bytes = 0usize;
    loop {
        check_cancel(cancel)?;
        budget.check_log(log)?;
        let Some(record) = read_record(&mut log.file, log.sequence)? else {
            break;
        };
        check_cancel(cancel)?;
        if !sealed {
            inventory_bytes = inventory_bytes
                .checked_add(serde_json::to_vec(&record)?.len())
                .ok_or_else(|| changed("native inventory size overflow"))?;
            if inventory_bytes > 4 * 1024 * 1024 {
                return Err(changed("native journal inventory exceeds bound"));
            }
        }
        match record {
            Record::Inventory { entry } if !sealed && inventory.len() < 4096 => {
                inventory.push(entry);
            }
            Record::InventoryIssue { issue } if !sealed && issues.len() < 16384 => {
                issues.push(issue);
            }
            Record::Seal { revision }
                if !sealed
                    && revision == header.revision
                    && inventory == plan.entries
                    && issues == plan.issues =>
            {
                sealed = true;
            }
            Record::Receipt { receipt } if sealed => {
                let admitted = plan
                    .entries
                    .iter()
                    .find(|e| e.entry.path == receipt.source.path)
                    .ok_or_else(|| changed("unknown native receipt path"))?;
                if receipt.source != admitted.entry
                    || admitted.mapped.is_none()
                    || (!receipt.source.directory
                        && (receipt.bytes != receipt.source.size
                            || receipt.digest.is_none()
                            || !receipt.completed))
                    || (receipt.source.directory
                        && (receipt.bytes != 0 || receipt.digest.is_some()))
                {
                    return Err(changed("native receipt disagrees with sealed inventory"));
                }
                if let Some(old) = receipts.get(&receipt.source.path)
                    && (!old.source.directory
                        || old.destination != receipt.destination
                        || old.source != receipt.source)
                {
                    return Err(changed("conflicting native receipt"));
                }
                receipts.insert(receipt.source.path.clone(), receipt);
            }
            Record::OutcomeStart { .. } if sealed => {
                outcome = true;
                details = 0;
            }
            Record::OutcomeIssue { path, .. } if outcome && details < 32768 => {
                if path.is_absolute()
                    || path
                        .components()
                        .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    return Err(changed("invalid native outcome path"));
                }
                details += 1;
            }
            Record::Fidelity { path, .. } if outcome && details < 32768 => {
                if !plan.entries.iter().any(|e| e.entry.path == path) {
                    return Err(changed("unknown native fidelity path"));
                }
                details += 1;
            }
            Record::CommitOutcome if outcome => outcome = false,
            _ => return Err(changed("invalid native journal record order or bounds")),
        }
        log.sequence += 1;
        end = log.file.stream_position()?;
    }
    if !sealed {
        return Err(changed("native inventory was never sealed"));
    }
    Ok((receipts, end))
}
pub fn open_file(parent: &PinnedDirectory, name: &std::ffi::OsStr) -> io::Result<std::fs::File> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
    let expected = metadata_nofollow(&parent.file, Path::new(name))?;
    if !expected.is_file() || expected.is_symlink() {
        return Err(changed("native destination is not an ordinary file"));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    if !super::migration::same_snapshot(&expected, &Metadata::from_file(&file)?) {
        return Err(changed("native destination replaced during open"));
    }
    Ok(file)
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::{io::Write, os::unix::fs::PermissionsExt};

    static RUNNING: AtomicBool = AtomicBool::new(false);

    fn fixture() -> (tempfile::TempDir, Plan, PathBuf, PathBuf) {
        let temp = tempfile::Builder::new()
            .prefix("tilo-native-copy-fix1-")
            .tempdir()
            .unwrap();
        let root = std::fs::canonicalize(temp.path()).unwrap();
        let private = root.join("receipts");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let plan = Plan {
            identity: Identity {
                volume: "v".into(),
                root: "r".into(),
                upcase: "u".into(),
            },
            entries: vec![PlanEntry {
                entry: Entry {
                    token: String::new(),
                    path: vec![],
                    key: vec![],
                    record: 5,
                    sequence: 1,
                    size: 0,
                    directory: true,
                    metadata_digest: "a".repeat(64),
                    content_readable: false,
                    enumeration_complete: true,
                    copy_times: Some(tilo_ntfs_protocol::CopyTimes {
                        last_access_100ns: 116_444_736_000_000_000,
                        last_data_change_100ns: 116_444_736_000_000_000,
                    }),
                    copy_times_unavailable_reason: None,
                    fidelity_pending: vec![],
                },
                mapped: Some(PathBuf::new()),
            }],
            issues: vec![],
        };
        let destination = root.join("output");
        let path = private.join("copy.v3");
        let mut journal = NativeJournal::create(&plan, &destination, &path, &RUNNING).unwrap();
        journal.root(&plan.entries[0].entry).unwrap();
        drop(journal);
        (temp, plan, destination, path)
    }

    /// Append and replay budgets.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_journal_budgets_refuse_before_exceeding_limits() {
        {
            let (_temp, plan, destination, path) = fixture();
            let mut journal = NativeJournal::open(&plan, &path, &RUNNING, || Ok(())).unwrap();
            let root = PinnedDirectory::open_absolute(&destination).unwrap();
            // One-entry plan: 3 inventory frames + 8 recovery envelopes of 8 frames.
            // The initial incomplete root receipt has already consumed frame 4.
            for _ in 0..63 {
                journal
                    .record_directory(&plan.entries[0].entry, &root, false)
                    .unwrap();
            }
            let before = std::fs::read(&path).unwrap();
            assert!(
                journal
                    .record_directory(&plan.entries[0].entry, &root, false)
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(destination.is_dir());
        }

        {
            let (_temp, plan, destination, path) = fixture();
            let mut journal = NativeJournal::open(&plan, &path, &RUNNING, || Ok(())).unwrap();
            for _ in 0..31 {
                journal.finish(&plan, &CopyReport::default()).unwrap();
            }
            let before = std::fs::read(&path).unwrap();
            assert!(journal.finish(&plan, &CopyReport::default()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(destination.is_dir());
        }

        {
            use super::super::migration_journal_log::write_record;
            for directory in [true, false] {
                let (_temp, plan, destination, path) = fixture();
                let journal = NativeJournal::open(&plan, &path, &RUNNING, || Ok(())).unwrap();
                let receipt = journal.receipts.values().next().unwrap().clone();
                let mut seq = journal.log.sequence;
                drop(journal);
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap();
                if directory {
                    for _ in 0..64 {
                        write_record(
                            &mut file,
                            &mut seq,
                            &Record::Receipt {
                                receipt: receipt.clone(),
                            },
                        )
                        .unwrap();
                    }
                } else {
                    for _ in 0..32 {
                        write_record(
                            &mut file,
                            &mut seq,
                            &Record::OutcomeStart {
                                cancelled: false,
                                scan_completed: false,
                            },
                        )
                        .unwrap();
                        write_record(&mut file, &mut seq, &Record::CommitOutcome).unwrap();
                    }
                }
                file.write_all(b"torn").unwrap();
                drop(file);
                let before = std::fs::read(&path).unwrap();
                let contacted = std::cell::Cell::new(false);
                let result = NativeJournal::open(&plan, &path, &RUNNING, || {
                    contacted.set(true);
                    Ok(())
                });
                assert!(result.is_err(), "unbounded history accepted");
                assert!(!contacted.get());
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert!(destination.is_dir());
            }
        }

        {
            let (_temp, plan, destination, path) = fixture();
            let mut journal = NativeJournal::open(&plan, &path, &RUNNING, || Ok(())).unwrap();
            let report = CopyReport {
                failed: vec![(PathBuf::new(), "x".repeat(100_000))],
                ..CopyReport::default()
            };
            journal.finish(&plan, &report).unwrap();
            let before = std::fs::read(&path).unwrap();
            assert!(journal.finish(&plan, &report).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(destination.is_dir());
        }
    }

    /// Cancelled replay, cancelled append and relocated journals.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn native_journal_cancellation_and_relocation_preserve_history() {
        {
            let (_temp, plan, destination, path) = fixture();
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"torn")
                .unwrap();
            let before = std::fs::read(&path).unwrap();
            let contacted = std::cell::Cell::new(false);
            let cancel = AtomicBool::new(true);
            let result = NativeJournal::open(&plan, &path, &cancel, || {
                contacted.set(true);
                Ok(())
            });
            assert!(matches!(result, Err(ref e) if e.kind() == io::ErrorKind::Interrupted));
            assert!(!contacted.get());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(destination.is_dir());
        }

        {
            let (_temp, plan, destination, path) = fixture();
            let cancel = AtomicBool::new(false);
            let mut journal = NativeJournal::open(&plan, &path, &cancel, || Ok(())).unwrap();
            let root = PinnedDirectory::open_absolute(&destination).unwrap();
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            let before = std::fs::read(&path).unwrap();
            assert!(
                journal
                    .record_directory(&plan.entries[0].entry, &root, false)
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(destination.is_dir());
        }

        {
            for replace in [false, true] {
                let (_temp, plan, _destination, path) = fixture();
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(b"torn")
                    .unwrap();
                let before = std::fs::read(&path).unwrap();
                let moved = path.with_extension("moved");
                let result = NativeJournal::open(&plan, &path, &RUNNING, || {
                    std::fs::rename(&path, &moved)?;
                    if replace {
                        std::fs::write(&path, b"replacement journal owned by user")?;
                    }
                    Ok(())
                });
                assert!(result.is_err(), "relocated journal was accepted");
                assert_eq!(
                    std::fs::read(&moved).unwrap(),
                    before,
                    "relocated original was truncated"
                );
                if replace {
                    assert_eq!(
                        std::fs::read(&path).unwrap(),
                        b"replacement journal owned by user"
                    );
                } else {
                    assert!(!path.exists());
                }
            }
        }
    }
}
