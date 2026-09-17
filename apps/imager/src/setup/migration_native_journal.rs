//! Native V3 receipts. No socket, nonce or generation token is restart authority.
use super::migration::{CopyReport, PinnedDirectory, changed};
use super::migration_journal::{PersistenceBoundary, persistence_result};
use super::migration_journal_log::{Log, read_record, repair};
use super::migration_native::{Plan, PlanEntry};
use super::{migration_durability as durability, nofollow::metadata_nofollow};
use qos_ntfs_protocol::{Entry, Identity, Issue, NativePath};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Seek},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
const MAGIC: &[u8; 8] = b"QOSJNL3\n";
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
pub(super) struct Receipt {
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
pub(super) struct NativeJournal {
    log: Log,
    pub receipts: BTreeMap<NativePath, Receipt>,
    pub destination: PathBuf,
    pub parent: Arc<PinnedDirectory>,
}
pub(super) fn directory_identity(directory: &PinnedDirectory) -> io::Result<durability::Identity> {
    durability::identity(&durability::readable_directory(&directory.file)?)
}
impl NativeJournal {
    pub(super) fn create(plan: &Plan, destination: &Path, journal: &Path) -> io::Result<Self> {
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
            operation: format!("{}-{:?}", std::process::id(), SystemTime::now()),
            source: plan.identity.clone(),
            revision: plan.revision()?,
            selected_root: plan.identity.root.clone(),
            destination: destination.to_path_buf(),
            destination_parent: directory_identity(&parent)?,
            journal: journal.to_path_buf(),
            mapping_version: 1,
            fidelity_version: 1,
        };
        let mut log = Log::create(
            journal,
            *MAGIC,
            &Record::Header {
                header: header.clone(),
            },
        )?;
        for entry in &plan.entries {
            log.append(&Record::Inventory {
                entry: entry.clone(),
            })?;
        }
        for issue in &plan.issues {
            log.append(&Record::InventoryIssue {
                issue: issue.clone(),
            })?;
        }
        log.append(&Record::Seal {
            revision: header.revision,
        })?;
        Ok(Self {
            log,
            receipts: BTreeMap::new(),
            destination: destination.to_path_buf(),
            parent,
        })
    }
    pub(super) fn open(
        plan: &Plan,
        path: &Path,
        validate_source: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Self> {
        let mut log = Log::open(path, *MAGIC)?;
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
        let (receipts, end) = replay(&mut log, &header, plan)?;
        // Validate all retained directory/file identities before modifying a torn tail.
        for (native, receipt) in &receipts {
            let mapped = plan
                .entries
                .iter()
                .find(|e| &e.entry.path == native)
                .and_then(|e| e.mapped.as_ref())
                .ok_or_else(|| changed("receipt mapping missing"))?;
            let full = header.destination.join(mapped);
            let identity = if receipt.source.directory {
                directory_identity(PinnedDirectory::open_absolute(&full)?.as_ref())?
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
        validate_output_names(plan, &header, &receipts)?;
        if !receipts.contains_key(&Vec::new()) && header.destination.try_exists()? {
            return Err(changed(
                "unreceipted native destination root is a preserved conflict",
            ));
        }
        log.validate()?;
        validate_source()?;
        repair(&mut log.file, end, || {
            parent.validate_destination()?;
            if directory_identity(&parent)? != header.destination_parent {
                return Err(changed("destination parent changed before journal repair"));
            }
            Ok(())
        })?;
        Ok(Self {
            destination: header.destination.clone(),
            log,
            receipts,
            parent,
        })
    }
    pub(super) fn save(&mut self, receipt: Receipt) -> io::Result<()> {
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
                self.log.append(&Record::Receipt {
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
    pub(super) fn finish(&mut self, plan: &Plan, report: &CopyReport) -> io::Result<()> {
        self.log.append(&Record::OutcomeStart {
            cancelled: report.cancelled,
            scan_completed: report.scan_completed,
        })?;
        for (failed, entries) in [(true, &report.failed), (false, &report.skipped)] {
            for (path, message) in entries {
                self.log.append(&Record::OutcomeIssue {
                    path: path.clone(),
                    message: message.clone(),
                    failed,
                })?;
            }
        }
        for entry in &plan.entries {
            for reason in &entry.entry.fidelity_pending {
                self.log.append(&Record::Fidelity {
                    path: entry.entry.path.clone(),
                    message: reason.clone(),
                })?;
            }
        }
        self.log.append(&Record::CommitOutcome)
    }
    pub(super) fn record_directory(
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
    pub(super) fn root(&mut self, entry: &Entry) -> io::Result<Arc<PinnedDirectory>> {
        let name = self
            .destination
            .file_name()
            .ok_or_else(|| changed("destination filename missing"))?;
        let root = if let Some(receipt) = self.receipts.get(&Vec::new()) {
            let root = PinnedDirectory::open_absolute(&self.destination)?;
            if directory_identity(&root)? != receipt.destination {
                return Err(changed("native destination root changed"));
            }
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
pub(super) fn sync_created_directory(
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
) -> io::Result<(BTreeMap<NativePath, Receipt>, u64)> {
    let mut inventory = Vec::new();
    let mut issues = Vec::new();
    let mut sealed = false;
    let mut receipts = BTreeMap::<NativePath, Receipt>::new();
    let mut end = log.file.stream_position()?;
    let mut outcome = false;
    let mut details = 0usize;
    let mut inventory_bytes = 0usize;
    while let Some(record) = read_record(&mut log.file, log.sequence)? {
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
pub(super) fn open_file(
    parent: &PinnedDirectory,
    name: &std::ffi::OsStr,
) -> io::Result<std::fs::File> {
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
