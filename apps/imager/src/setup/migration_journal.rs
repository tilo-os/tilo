//! Versioned private receipts. A receipt is a hint to revalidate, never overwrite authority.
use super::migration::{self, CopyProgress, CopyReport, OwnedTemporary, PinnedDirectory, changed};
use super::{
    destination_security,
    migration_durability::{self as durability, Identity},
    nofollow::metadata_nofollow,
};
use advisory_lock::FileLock;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::SystemTime,
};

// The old whole-document bound remains only for legacy decoding. V2 bounds
// each frame, never the complete operation. Checksums detect corruption, not
// malicious edits; receipts still confer no overwrite authority.
const MAX_JOURNAL: u64 = 16 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"TILOJL2\n";

#[derive(Serialize, Deserialize)]
#[serde(tag = "record", deny_unknown_fields)]
enum Record {
    Header {
        document: Document,
    },
    Receipt {
        receipt: Receipt,
    },
    Outcome {
        bytes: u64,
        cancelled: bool,
        scan_completed: bool,
        metadata_limitations: Vec<String>,
    },
    Issue {
        failed: bool,
        path: RelativePath,
        message: String,
    },
    CommitOutcome,
}

use super::migration_journal_log::{read_record, write_record};
/// Internal classification travels with I/O errors; callers never infer
/// persistence state from an error message or ordinary per-file conflicts.
#[derive(Clone, Copy, Debug)]
pub(super) enum PersistenceBoundary {
    JournalUnavailable,
    PreviousCheckpoint,
    JournalReplacementUncertain,
    OutputUnpublished,
    OutputPublicationUncertain,
    OutputUnrecorded,
}
impl PersistenceBoundary {
    fn description(self) -> &'static str {
        match self {
            Self::JournalUnavailable => {
                "journal is unavailable or replaced; reopen and revalidate before trusting any prior checkpoint"
            }
            Self::PreviousCheckpoint => {
                "previous journal checkpoint retained; output without a saved receipt requires review"
            }
            Self::JournalReplacementUncertain => {
                "journal replacement is visible but its persistence is uncertain; reopen and revalidate before resuming"
            }
            Self::OutputUnpublished => {
                "output persistence failed before publication; previous journal checkpoint retained"
            }
            Self::OutputPublicationUncertain => {
                "output publication or metadata persistence is uncertain; previous journal checkpoint retained"
            }
            Self::OutputUnrecorded => {
                "published output has no saved receipt; previous journal checkpoint retained"
            }
        }
    }
}
#[derive(Debug)]
struct PersistenceError {
    boundary: PersistenceBoundary,
    cause: io::Error,
}
impl std::fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cause.fmt(formatter)
    }
}
impl std::error::Error for PersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.cause)
    }
}
pub(super) fn persistence_result<T>(
    result: io::Result<T>,
    boundary: PersistenceBoundary,
) -> io::Result<T> {
    result.map_err(|cause| {
        if cause
            .get_ref()
            .is_some_and(<dyn std::error::Error + Send + Sync>::is::<PersistenceError>)
        {
            cause
        } else {
            io::Error::other(PersistenceError { boundary, cause })
        }
    })
}
#[cfg(any(target_os = "linux", test))]
pub(super) fn persistence_description(error: &io::Error) -> String {
    if let Some(failure) = error
        .get_ref()
        .and_then(|cause| cause.downcast_ref::<PersistenceError>())
    {
        format!("{}: {error}", failure.boundary.description())
    } else {
        error.to_string()
    }
}
struct JournalFailure {
    path: PathBuf,
    message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", content = "units", deny_unknown_fields)]
enum NativeName {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}
impl NativeName {
    fn encode(name: &OsStr) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix(name.as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(name.encode_wide().collect())
        }
    }
    fn decode(&self) -> io::Result<OsString> {
        match self {
            #[cfg(unix)]
            Self::Unix(bytes) if !bytes.contains(&0) => {
                use std::os::unix::ffi::OsStringExt;
                Ok(OsString::from_vec(bytes.clone()))
            }
            #[cfg(windows)]
            Self::Windows(units) if !units.contains(&0) => {
                use std::os::windows::ffi::OsStringExt;
                Ok(OsString::from_wide(units))
            }
            _ => Err(changed("journal path platform mismatch or NUL")),
        }
    }
    fn len(&self) -> usize {
        match self {
            Self::Unix(v) => v.len(),
            Self::Windows(v) => v.len(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelativePath {
    components: Vec<NativeName>,
}
impl RelativePath {
    fn encode(path: &Path) -> io::Result<Self> {
        let components = path
            .components()
            .map(|c| match c {
                Component::Normal(n) => Ok(NativeName::encode(n)),
                _ => Err(changed("non-normal relative path")),
            })
            .collect::<io::Result<Vec<_>>>()?;
        let result = Self { components };
        result.decode()?;
        Ok(result)
    }
    fn decode(&self) -> io::Result<PathBuf> {
        if self.components.len() > 128
            || self.components.iter().map(|n| n.len() + 1).sum::<usize>() > 4096
        {
            return Err(changed("journal path exceeds bounds"));
        }
        let mut path = PathBuf::new();
        for part in &self.components {
            if part.len() == 0 || part.len() > 255 {
                return Err(changed("journal component exceeds bounds"));
            }
            let name = part.decode()?;
            let mut components = Path::new(&name).components();
            if !matches!(components.next(), Some(Component::Normal(n)) if n == name)
                || components.next().is_some()
            {
                return Err(changed("journal component is not a single normal filename"));
            }
            path.push(name);
        }
        Ok(path)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    size: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}
impl Snapshot {
    pub(super) fn new(m: &Metadata) -> io::Result<Self> {
        use cap_fs_ext::MetadataExt;
        Ok(Self {
            size: m.len(),
            modified: m.modified()?.into_std(),
            created: m
                .created()
                .ok()
                .map(cap_primitives::time::SystemTime::into_std),
            inode: m.ino(),
            #[cfg(unix)]
            changed: {
                use cap_primitives::fs::MetadataExt;
                (m.ctime(), m.ctime_nsec())
            },
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileReceipt {
    pub(super) bytes: u64,
    accessed: SystemTime,
    digest: [u8; 32],
    source: Snapshot,
    source_identity: Option<Identity>,
    destination_identity: Option<Identity>,
}
impl FileReceipt {
    pub(super) fn new(
        source: &File,
        original: &Metadata,
        destination: &File,
        bytes: u64,
        digest: [u8; 32],
        durable: bool,
    ) -> io::Result<Self> {
        #[cfg(test)]
        if durable {
            persistence_injection::fail_at("file_receipt_construct")?;
        }
        Ok(Self {
            bytes,
            accessed: original.accessed()?.into_std(),
            digest,
            source: Snapshot::new(original)?,
            source_identity: if durable {
                Some(durability::identity(source)?)
            } else {
                None
            },
            destination_identity: if durable {
                Some(durability::identity(destination)?)
            } else {
                None
            },
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Receipt {
    File {
        path: RelativePath,
        facts: FileReceipt,
    },
    Directory {
        path: RelativePath,
        source: Identity,
        destination: Identity,
        accessed: SystemTime,
        modified: SystemTime,
        completed: bool,
    },
}
impl Receipt {
    fn path(&self) -> &RelativePath {
        match self {
            Self::File { path, .. } | Self::Directory { path, .. } => path,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Outcome {
    verified: Vec<RelativePath>,
    bytes: u64,
    skipped: Vec<(RelativePath, String)>,
    failed: Vec<(RelativePath, String)>,
    cancelled: bool,
    scan_completed: bool,
    metadata_limitations: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    operation: String,
    source: NativeName,
    destination: NativeName,
    source_identity: Identity,
    destination_parent_identity: Identity,
    #[serde(default)]
    reviewed_revision: Option<[u8; 32]>,
    receipts: Vec<Receipt>,
    outcome: Option<Outcome>,
}
/// In-memory owner of one exclusively created, versioned private journal.
/// All persisted paths are native code units; no permissions or grants are serialized.
pub struct MigrationJournal {
    document: Document,
    index: BTreeMap<PathBuf, usize>,
    sequence: u64,
    parent: Arc<PinnedDirectory>,
    name: OsString,
    file: FileLock,
    identity: Metadata,
    resume: bool,
    seen: BTreeSet<PathBuf>,
    failure: Option<JournalFailure>,
    reviewed: Option<super::migration_review::SourceInventory>,
}

fn directory_identity(directory: &PinnedDirectory) -> io::Result<Identity> {
    durability::identity(&durability::readable_directory(&directory.file)?)
}
fn absolute(path: &NativeName) -> io::Result<PathBuf> {
    if path.len() > 4096 {
        return Err(changed("journal root exceeds bounds"));
    }
    let path = PathBuf::from(path.decode()?);
    if !path.is_absolute() {
        return Err(changed("journal root must be native absolute path"));
    }
    Ok(path)
}
fn journal_parent(path: &Path) -> io::Result<(Arc<PinnedDirectory>, OsString)> {
    super::migration_journal_log::parent(path)
}
impl MigrationJournal {
    fn create(
        source_path: &Path,
        destination_path: &Path,
        journal_path: &Path,
    ) -> io::Result<Self> {
        Self::create_reviewed(source_path, destination_path, journal_path, None)
    }

    fn create_reviewed(
        source_path: &Path,
        destination_path: &Path,
        journal_path: &Path,
        reviewed: Option<&(Identity, Identity, [u8; 32])>,
    ) -> io::Result<Self> {
        durability::supported()?;
        let source = PinnedDirectory::open_absolute(source_path)?;
        let destination_parent = PinnedDirectory::open_absolute(
            destination_path
                .parent()
                .ok_or_else(|| changed("destination parent missing"))?,
        )?;
        destination_parent.validate_destination()?;
        if let Some((expected_source, expected_parent, _)) = reviewed
            && (directory_identity(&source)? != *expected_source
                || directory_identity(&destination_parent)? != *expected_parent)
        {
            return Err(changed(
                "reviewed source or destination parent changed before copy",
            ));
        }
        let inventory = reviewed
            .map(|(_, _, revision)| {
                let inventory = super::migration_review::SourceInventory::capture(&source)?;
                if inventory.revision()? != *revision {
                    return Err(changed(
                        "Source contents changed after review; explicitly review the folder again.",
                    ));
                }
                Ok(inventory)
            })
            .transpose()?;
        let (parent, name) = journal_parent(journal_path)?;
        if parent.contains_identity(&source.identity)
            || source.contains_identity(&parent.identity)
            || destination_path.starts_with(
                journal_path
                    .parent()
                    .ok_or_else(|| changed("journal parent missing"))?,
            )
        {
            return Err(changed(
                "journal must be outside both copied trees in a separate private directory",
            ));
        }
        let document = Document {
            version: 2,
            operation: format!("{}-{:?}", std::process::id(), ui::time::wall()),
            source: NativeName::encode(source_path.as_os_str()),
            destination: NativeName::encode(destination_path.as_os_str()),
            source_identity: directory_identity(&source)?,
            destination_parent_identity: directory_identity(&destination_parent)?,
            reviewed_revision: reviewed.map(|r| r.2),
            receipts: Vec::new(),
            outcome: None,
        };
        absolute(&document.source)?;
        absolute(&document.destination)?;
        let (file, identity, sequence) = super::migration_journal_log::create_private(
            &parent,
            &name,
            *MAGIC,
            &Record::Header {
                document: document.clone(),
            },
        )?;
        Ok(Self {
            document,
            index: BTreeMap::new(),
            sequence,
            parent,
            name,
            file,
            identity,
            resume: false,
            seen: BTreeSet::new(),
            failure: None,
            reviewed: inventory,
        })
    }
    fn open(path: &Path) -> io::Result<Self> {
        durability::supported()?;
        let (parent, name) = journal_parent(path)?;
        let (mut file, identity) = super::migration_journal_log::open_private(&parent, &name)?;
        let mut magic = [0; 8];
        let modern = file.read_exact(&mut magic).is_ok() && &magic == MAGIC;
        if &magic == b"TILOJL3\n" {
            return Err(changed(
                "fresh native selection required; use native resume",
            ));
        }
        file.seek(SeekFrom::Start(if modern { 8 } else { 0 }))?;
        let mut sequence = 0;
        let mut end = 8;
        let mut document = if modern {
            let Some(Record::Header { document }) = read_record(&mut file, sequence)? else {
                return Err(changed("journal header missing"));
            };
            sequence += 1;
            end = file.stream_position()?;
            if document.version != 2 || !document.receipts.is_empty() || document.outcome.is_some()
            {
                return Err(changed("invalid journal header"));
            }
            document
        } else {
            let mut data = Vec::new();
            (&mut *file).take(MAX_JOURNAL + 1).read_to_end(&mut data)?;
            if data.len() as u64 > MAX_JOURNAL {
                return Err(changed("legacy journal exceeds size bound"));
            }
            let document: Document = serde_json::from_slice(&data)?;
            if document.version != 1 {
                return Err(changed("unsupported legacy journal schema"));
            }
            document
        };
        if document.operation.is_empty() || document.operation.len() > 128 {
            return Err(changed("unsupported journal operation"));
        }
        absolute(&document.source)?;
        absolute(&document.destination)?;
        let mut index = BTreeMap::new();
        for (position, receipt) in document.receipts.iter().enumerate() {
            if index.insert(receipt.path().decode()?, position).is_some() {
                return Err(changed("duplicate journal receipt"));
            }
        }
        if modern {
            let mut pending = None;
            while let Some(record) = read_record(&mut file, sequence)? {
                Self::replay_record(&mut document, &mut index, &mut pending, record)?;
                sequence += 1;
                end = file.stream_position()?;
            }
        }
        Self::validate_outcome_paths(&document)?;
        let mut result = Self {
            document,
            index,
            sequence,
            parent,
            name,
            file,
            identity,
            resume: true,
            seen: BTreeSet::new(),
            failure: None,
            reviewed: None,
        };
        result.validate()?;
        result.load_reviewed_inventory()?;
        if modern {
            if end != result.file.metadata()?.len() {
                result.validate_recovery_location()?;
                // Revalidate before repairing only the uncommitted final tail.
                #[cfg(test)]
                persistence_injection::fail_at("before_recover_tail")?;
                super::migration_journal_log::repair(&mut result.file, end, || Ok(()))?;
            }
            result.file.seek(SeekFrom::Start(end))?;
        } else {
            result.upgrade_legacy()?;
        }
        Ok(result)
    }
    fn load_reviewed_inventory(&mut self) -> io::Result<()> {
        if let Some(revision) = self.document.reviewed_revision {
            let root = PinnedDirectory::open_absolute(&absolute(&self.document.source)?)?;
            let inventory = super::migration_review::SourceInventory::capture(&root)?;
            if inventory.revision()? != revision {
                return Err(changed(
                    "Source contents changed after review; explicitly review the folder again.",
                ));
            }
            self.reviewed = Some(inventory);
        }
        Ok(())
    }

    fn validate_outcome_paths(document: &Document) -> io::Result<()> {
        if let Some(outcome) = &document.outcome {
            for path in outcome
                .verified
                .iter()
                .chain(outcome.failed.iter().map(|(p, _)| p))
                .chain(outcome.skipped.iter().map(|(p, _)| p))
            {
                path.decode()?;
            }
        }
        Ok(())
    }

    fn replay_record(
        document: &mut Document,
        index: &mut BTreeMap<PathBuf, usize>,
        pending: &mut Option<Outcome>,
        record: Record,
    ) -> io::Result<()> {
        match record {
            Record::Header { .. } => return Err(changed("duplicate journal header")),
            Record::Receipt { receipt } => {
                let path = receipt.path().decode()?;
                if let Some(position) = index.get(&path) {
                    // Only directory completion can update a receipt.
                    match (
                        &*document
                            .receipts
                            .get_mut(*position)
                            .ok_or_else(|| changed("journal receipt index missing"))?,
                        &receipt,
                    ) {
                        (
                            Receipt::Directory {
                                source: old_source,
                                destination: old_destination,
                                accessed: old_accessed,
                                modified: old_modified,
                                ..
                            },
                            Receipt::Directory {
                                source: new_source,
                                destination: new_destination,
                                accessed: new_accessed,
                                modified: new_modified,
                                completed: true,
                                ..
                            },
                        ) if (old_source, old_destination, old_accessed, old_modified)
                            == (new_source, new_destination, new_accessed, new_modified) => {}
                        _ => return Err(changed("conflicting journal receipt update")),
                    }
                    *document
                        .receipts
                        .get_mut(*position)
                        .ok_or_else(|| changed("journal receipt index missing"))? = receipt;
                } else {
                    index.insert(path, document.receipts.len());
                    document.receipts.push(receipt);
                }
            }
            Record::Outcome {
                bytes,
                cancelled,
                scan_completed,
                metadata_limitations,
            } => {
                *pending = Some(Outcome {
                    verified: Vec::new(),
                    bytes,
                    skipped: Vec::new(),
                    failed: Vec::new(),
                    cancelled,
                    scan_completed,
                    metadata_limitations,
                });
                document.outcome = None;
            }
            Record::Issue {
                failed,
                path,
                message,
            } => {
                path.decode()?;
                let outcome = pending
                    .as_mut()
                    .ok_or_else(|| changed("issue outside outcome"))?;
                if failed {
                    outcome.failed.push((path, message));
                } else {
                    outcome.skipped.push((path, message));
                }
            }
            Record::CommitOutcome => {
                document.outcome = Some(
                    pending
                        .take()
                        .ok_or_else(|| changed("outcome commit without outcome"))?,
                );
            }
        }
        Ok(())
    }

    // Repair/conversion writes are permitted only outside both copied trees,
    // after root identity revalidation. Bad locations remain byte-for-byte intact.
    fn validate_recovery_location(&self) -> io::Result<()> {
        let source = PinnedDirectory::open_absolute(&absolute(&self.document.source)?)?;
        let destination_path = absolute(&self.document.destination)?;
        let destination_parent_path = destination_path
            .parent()
            .ok_or_else(|| changed("destination parent missing"))?;
        let destination_parent = PinnedDirectory::open_absolute(destination_parent_path)?;
        if directory_identity(&source)? != self.document.source_identity
            || directory_identity(&destination_parent)? != self.document.destination_parent_identity
        {
            return Err(changed(
                "root identity changed; journal recovery requires review",
            ));
        }
        if self.parent.contains_identity(&source.identity)
            || source.contains_identity(&self.parent.identity)
        {
            return Err(changed(
                "journal recovery must remain outside the source tree",
            ));
        }
        if let Ok(destination) = PinnedDirectory::open_absolute(&destination_path)
            && (self.parent.contains_identity(&destination.identity)
                || destination.contains_identity(&self.parent.identity))
        {
            return Err(changed(
                "journal recovery must remain outside the destination tree",
            ));
        }
        Ok(())
    }

    fn validate(&self) -> io::Result<()> {
        super::migration_journal_log::validate(&self.parent, &self.name, &self.file, &self.identity)
    }
    pub(super) fn has_failed(&self) -> bool {
        self.failure.is_some()
    }

    pub(super) fn observe_persistence_failure(&mut self, path: &Path, error: &io::Error) {
        if self.failure.is_none()
            && let Some(failure) = error
                .get_ref()
                .and_then(|error| error.downcast_ref::<PersistenceError>())
        {
            self.failure = Some(JournalFailure {
                path: path.to_path_buf(),
                message: format!(
                    "Copy stopped because persistence failed: {}; {}",
                    failure.boundary.description(),
                    error
                ),
            });
        }
    }

    pub(super) fn retain_failure_in(&self, report: &mut CopyReport) {
        if let Some(failure) = &self.failure {
            report
                .failed
                .push((failure.path.clone(), failure.message.clone()));
        }
    }

    fn checkpoint<T>(
        &mut self,
        path: &Path,
        boundary: PersistenceBoundary,
        operation: impl FnOnce(&mut Self) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.has_failed() {
            return Err(changed(
                "journal persistence already failed; no further checkpoint attempted",
            ));
        }
        let result = persistence_result(operation(self), boundary);
        if let Err(error) = &result {
            self.observe_persistence_failure(path, error);
        }
        result
    }

    fn append(
        &mut self,
        records: impl FnOnce(&mut File, &mut u64) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut boundary = PersistenceBoundary::PreviousCheckpoint;
        let result = (|| {
            persistence_result(self.validate(), PersistenceBoundary::JournalUnavailable)?;
            #[cfg(test)]
            persistence_injection::fail_at("before_encode")?;
            #[cfg(test)]
            crash_injection::boundary("journal_before_replace");
            #[cfg(test)]
            persistence_injection::fail_at("before_replace")?;
            boundary = PersistenceBoundary::JournalReplacementUncertain;
            super::migration_journal_log::append_synced(
                &mut self.file,
                &mut self.sequence,
                records,
                || {
                    #[cfg(test)]
                    crash_injection::boundary("journal_after_replace");
                    #[cfg(test)]
                    persistence_injection::fail_at("after_replace")?;
                    Ok(())
                },
            )
        })();
        persistence_result(result, boundary)
    }

    fn save_receipt(&mut self, receipt: Receipt) -> io::Result<()> {
        let path = receipt.path().decode()?;
        self.append(|file, sequence| {
            write_record(
                file,
                sequence,
                &Record::Receipt {
                    receipt: receipt.clone(),
                },
            )
        })?;
        if let Some(position) = self.index.get(&path) {
            *self
                .document
                .receipts
                .get_mut(*position)
                .ok_or_else(|| changed("journal receipt index missing"))? = receipt;
        } else {
            self.index.insert(path, self.document.receipts.len());
            self.document.receipts.push(receipt);
        }
        Ok(())
    }

    fn write_outcome(file: &mut File, sequence: &mut u64, outcome: &Outcome) -> io::Result<()> {
        write_record(
            file,
            sequence,
            &Record::Outcome {
                bytes: outcome.bytes,
                cancelled: outcome.cancelled,
                scan_completed: outcome.scan_completed,
                metadata_limitations: outcome.metadata_limitations.clone(),
            },
        )?;
        for (failed, issues) in [(false, &outcome.skipped), (true, &outcome.failed)] {
            for (path, message) in issues {
                write_record(
                    file,
                    sequence,
                    &Record::Issue {
                        failed,
                        path: path.clone(),
                        message: message.clone(),
                    },
                )?;
            }
        }
        write_record(file, sequence, &Record::CommitOutcome)
    }

    fn upgrade_legacy(&mut self) -> io::Result<()> {
        self.validate()?;
        self.validate_recovery_location()?;
        let mut temporary = OwnedTemporary::create(&self.parent, &self.name)?;
        temporary.file.write_all(MAGIC)?;
        let mut sequence = 0;
        let mut header = self.document.clone();
        header.version = 2;
        header.receipts.clear();
        header.outcome = None;
        write_record(
            &mut temporary.file,
            &mut sequence,
            &Record::Header { document: header },
        )?;
        for receipt in &self.document.receipts {
            write_record(
                &mut temporary.file,
                &mut sequence,
                &Record::Receipt {
                    receipt: receipt.clone(),
                },
            )?;
        }
        if let Some(outcome) = &self.document.outcome {
            Self::write_outcome(&mut temporary.file, &mut sequence, outcome)?;
        }
        durability::sync_file(&temporary.file)?;
        let file = FileLock::try_exclusive(temporary.file.try_clone()?)
            .map_err(|e| changed(&format!("journal is busy: {e}")))?;
        self.validate()?;
        #[cfg(test)]
        persistence_injection::fail_at("legacy_before_replace")?;
        cap_primitives::fs::rename(
            &self.parent.file,
            Path::new(&temporary.name),
            &self.parent.file,
            Path::new(&self.name),
        )?;
        durability::sync_directory(&self.parent.file)?;
        self.file = file;
        self.identity = Metadata::from_file(&self.file)?;
        self.sequence = sequence;
        self.document.version = 2;
        Ok(())
    }

    pub(super) fn validate_relative(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        RelativePath::encode(path).map(|_| ())
    }
    fn lookup(&self, path: &Path) -> Option<&Receipt> {
        self.index
            .get(path)
            .and_then(|position| self.document.receipts.get(*position))
    }
    pub(super) fn validate_reviewed_entry(
        &self,
        path: &Path,
        metadata: &Metadata,
    ) -> io::Result<()> {
        if let Some(inventory) = &self.reviewed {
            inventory.check(path, metadata)?;
        }
        Ok(())
    }
    pub(super) fn root(
        &mut self,
        source: &Arc<PinnedDirectory>,
        parent: &Arc<PinnedDirectory>,
        name: &OsStr,
    ) -> io::Result<Arc<PinnedDirectory>> {
        self.validate()?;
        if directory_identity(source)? != self.document.source_identity
            || directory_identity(parent)? != self.document.destination_parent_identity
        {
            return Err(changed(
                "root identity changed or boot-bound identity expired; reselect folders and review conflicts",
            ));
        }
        if let Some(inventory) = &self.reviewed {
            inventory.validate(source)?;
        }
        if self.resume {
            if self.lookup(Path::new("")).is_none() {
                return Err(changed(
                    "root creation was interrupted; existing output requires review",
                ));
            }
            let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
            let directory = parent.open_child(name, &metadata)?;
            if self.parent.contains_identity(&source.identity)
                || self.parent.contains_identity(&directory.identity)
            {
                return Err(changed(
                    "journal must remain outside both copied trees; move it back to private application data",
                ));
            }
            self.check_directory(Path::new(""), source, &directory)?;
            Ok(directory)
        } else {
            let directory = parent.create_child(name)?;
            self.checkpoint(
                Path::new(""),
                PersistenceBoundary::OutputPublicationUncertain,
                |journal| {
                    #[cfg(test)]
                    persistence_injection::fail_at("root_publication_flush")?;
                    durability::sync_directory(&directory.file)?;
                    durability::sync_directory(&parent.file)?;
                    #[cfg(test)]
                    crash_injection::boundary("root_published");
                    journal.enter_directory(Path::new(""), source, &directory)
                },
            )?;
            Ok(directory)
        }
    }
    fn check_directory(
        &self,
        path: &Path,
        source: &PinnedDirectory,
        destination: &PinnedDirectory,
    ) -> io::Result<()> {
        destination.validate_destination()?;
        destination_security::check_private_file(&destination.file)?;
        match self.lookup(path) {
            Some(Receipt::Directory {
                source: s,
                destination: d,
                modified,
                ..
            }) if *s == directory_identity(source)?
                && *d == directory_identity(destination)?
                && *modified == Metadata::from_file(&source.file)?.modified()?.into_std() =>
            {
                Ok(())
            }
            _ => Err(changed(
                "directory conflict; reselect and review existing output",
            )),
        }
    }
    pub(super) fn enter_directory(
        &mut self,
        path: &Path,
        source: &PinnedDirectory,
        destination: &PinnedDirectory,
    ) -> io::Result<()> {
        if self.lookup(path).is_some() {
            self.check_directory(path, source, destination)?;
        } else {
            self.checkpoint(path, PersistenceBoundary::OutputUnrecorded, |journal| {
                journal.save_receipt(Receipt::Directory {
                    path: RelativePath::encode(path)?,
                    source: directory_identity(source)?,
                    destination: directory_identity(destination)?,
                    accessed: Metadata::from_file(&source.file)?.accessed()?.into_std(),
                    modified: Metadata::from_file(&source.file)?.modified()?.into_std(),
                    completed: false,
                })
            })?;
        }
        self.seen.insert(path.to_path_buf());
        Ok(())
    }
    pub(super) fn directory(
        &mut self,
        path: &Path,
        source: &Arc<PinnedDirectory>,
        destination: &Arc<PinnedDirectory>,
        name: &OsStr,
        metadata: &Metadata,
    ) -> io::Result<Arc<PinnedDirectory>> {
        let child_source = source.open_child(name, metadata)?;
        if self.lookup(path).is_some() {
            let expected = metadata_nofollow(&destination.file, Path::new(name))?;
            let child = destination.open_child(name, &expected)?;
            self.check_directory(path, &child_source, &child)?;
            Ok(child)
        } else {
            let child = destination.create_child(name)?;
            self.checkpoint(
                path,
                PersistenceBoundary::OutputPublicationUncertain,
                |journal| {
                    #[cfg(test)]
                    persistence_injection::fail_at("directory_publication_flush")?;
                    durability::sync_directory(&child.file)?;
                    durability::sync_directory(&destination.file)?;
                    journal.enter_directory(path, &child_source, &child)
                },
            )?;
            Ok(child)
        }
    }
    pub(super) fn directory_times(&self, path: &Path) -> io::Result<(SystemTime, SystemTime)> {
        match self.lookup(path) {
            Some(Receipt::Directory {
                accessed, modified, ..
            }) => Ok((*accessed, *modified)),
            _ => Err(changed("directory receipt missing")),
        }
    }
    pub(super) fn complete_directory(
        &mut self,
        path: &Path,
        destination: &PinnedDirectory,
    ) -> io::Result<()> {
        self.checkpoint(
            path,
            PersistenceBoundary::OutputPublicationUncertain,
            |journal| {
                #[cfg(test)]
                persistence_injection::fail_at("directory_completion_flush")?;
                durability::sync_directory(&destination.file)?;
                let mut receipt = journal
                    .lookup(path)
                    .cloned()
                    .ok_or_else(|| changed("directory receipt missing"))?;
                if let Receipt::Directory { completed, .. } = &mut receipt {
                    *completed = true;
                }
                journal.save_receipt(receipt)
            },
        )
    }
    pub(super) fn record_file(&mut self, path: &Path, facts: FileReceipt) -> io::Result<()> {
        self.checkpoint(path, PersistenceBoundary::OutputUnrecorded, |journal| {
            journal.save_receipt(Receipt::File {
                path: RelativePath::encode(path)?,
                facts,
            })?;
            journal.seen.insert(path.to_path_buf());
            Ok(())
        })
    }
    pub(super) fn revalidate_file(
        &mut self,
        path: &Path,
        source: &PinnedDirectory,
        destination: &PinnedDirectory,
        name: &OsStr,
        cancelled: &AtomicBool,
    ) -> io::Result<Option<u64>> {
        let Some(receipt) = self.lookup(path) else {
            return Ok(None);
        };
        let Receipt::File { facts, .. } = receipt else {
            return Err(changed("source type changed; conflict requires review"));
        };
        for (parent, identity, is_source) in [
            (source, &facts.source_identity, true),
            (destination, &facts.destination_identity, false),
        ] {
            parent.validate()?;
            let expected = metadata_nofollow(&parent.file, Path::new(name))?;
            migration::reject_windows_placeholder(&expected)?;
            if !expected.is_file() || expected.is_symlink() {
                return Err(changed(
                    "recorded file missing or changed type; conflict requires review",
                ));
            }
            #[cfg(test)]
            race_injection::before_open();
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No).nonblock(true);
            let mut file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
            let before = Metadata::from_file(&file)?;
            if !migration::same_snapshot(&expected, &before)
                || identity.as_ref() != Some(&durability::identity(&file)?)
                || (is_source && Snapshot::new(&before)? != facts.source)
            {
                return Err(changed(
                    "recorded file identity or source facts changed; conflict requires review",
                ));
            }
            if migration::stream(&mut file, None, facts.bytes, cancelled)?
                != (facts.bytes, facts.digest)
                || !migration::same_snapshot(&before, &Metadata::from_file(&file)?)
                || !migration::same_snapshot(
                    &before,
                    &metadata_nofollow(&parent.file, Path::new(name))?,
                )
            {
                return Err(changed(
                    "recorded content changed; conflict requires review",
                ));
            }
            parent.validate()?;
            if !is_source {
                destination.validate_destination()?;
                destination_security::check_private_file(&file)?;
                if before.modified()?.into_std() != facts.source.modified {
                    return Err(changed(
                        "destination timestamp changed; conflict requires review",
                    ));
                }
                migration::preserve_time_values(&file, facts.accessed, facts.source.modified)?;
                persistence_result(
                    durability::sync_file(&file),
                    PersistenceBoundary::OutputPublicationUncertain,
                )?;
            }
        }
        let bytes = facts.bytes;
        self.seen.insert(path.to_path_buf());
        Ok(Some(bytes))
    }
    pub(super) fn finish(&mut self, report: &mut CopyReport) -> io::Result<()> {
        if let Some(inventory) = &self.reviewed {
            let validation = absolute(&self.document.source)
                .and_then(|path| PinnedDirectory::open_absolute(&path))
                .and_then(|root| inventory.validate(&root));
            if let Err(error) = validation {
                report.scan_completed = false;
                report.failed.push((PathBuf::new(), error.to_string()));
            }
        }

        self.checkpoint(
            Path::new(""),
            PersistenceBoundary::PreviousCheckpoint,
            |journal| journal.finish_inner(report),
        )
    }
    fn finish_inner(&mut self, report: &mut CopyReport) -> io::Result<()> {
        report.metadata_limitations.push("Automatic resume across boots requires stable filesystem identities. If only a boot-bound identity is available, reselect the folders and review conflicts after reboot.".into());
        for receipt in &self.document.receipts {
            let path = receipt.path().decode()?;
            if !self.seen.contains(&path) && !report.cancelled {
                report.failed.push((path,"recorded source missing or no longer safely reachable; conflict requires review".into()));
            }
        }
        let outcome = Outcome {
            verified: Vec::new(),
            bytes: report.verified_bytes,
            skipped: report
                .skipped
                .iter()
                .map(|(p, s)| Ok((RelativePath::encode(p)?, s.clone())))
                .collect::<io::Result<_>>()?,
            failed: report
                .failed
                .iter()
                .map(|(p, s)| Ok((RelativePath::encode(p)?, s.clone())))
                .collect::<io::Result<_>>()?,
            cancelled: report.cancelled,
            scan_completed: report.scan_completed,
            metadata_limitations: report.metadata_limitations.clone(),
        };
        self.append(|file, sequence| Self::write_outcome(file, sequence, &outcome))?;
        self.document.outcome = Some(outcome);
        Ok(())
    }
}

/// Start a fresh durable operation. The caller supplies a private application-data
/// journal directory outside both copied trees. Existing journal/output names
/// are conflicts. Windows currently refuses before creating any files because
/// unprivileged directory-entry persistence is not yet established.
///
/// # Errors
/// Returns an error if grants, journal identity, or initial persistence cannot be validated.
pub fn copy_folder_with_journal(
    source: &Path,
    destination: &Path,
    journal_path: &Path,
    cancelled: &AtomicBool,
    progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, String> {
    if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(CopyReport {
            cancelled: true,
            ..CopyReport::default()
        });
    }
    let journal =
        MigrationJournal::create(source, destination, journal_path).map_err(|e| e.to_string())?;
    migration::copy_folder_internal(source, destination, cancelled, progress, Some(journal))
}
/// Same engine, with reviewed identities checked on the actual pinned roots
/// before the fresh journal is published. Resume also compares the existing
/// receipt identities, so an old journal can never waive the reviewed binding.
pub(super) fn copy_reviewed(
    source: &Path,
    destination: &Path,
    journal_path: &Path,
    reviewed: &(Identity, Identity, [u8; 32]),
    resume: bool,
    cancelled: &AtomicBool,
    progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, String> {
    let journal = if resume {
        let journal = MigrationJournal::open(journal_path).map_err(|e| e.to_string())?;
        if journal.document.reviewed_revision != Some(reviewed.2)
            || journal.document.source_identity != reviewed.0
            || journal.document.destination_parent_identity != reviewed.1
            || absolute(&journal.document.source).map_err(|e| e.to_string())? != source
            || absolute(&journal.document.destination).map_err(|e| e.to_string())? != destination
        {
            return Err("saved journal does not match the reviewed endpoints".into());
        }
        journal
    } else {
        MigrationJournal::create_reviewed(source, destination, journal_path, Some(reviewed))
            .map_err(|e| e.to_string())?
    };
    migration::copy_folder_internal(source, destination, cancelled, progress, Some(journal))
}

/// Reopen a private journal and revalidate native roots, source facts and output
/// bytes. Changed or unrecorded output is retained as a conflict requiring review.
/// Linux without a persistent filesystem UUID is restricted to the same boot.
///
/// # Errors
/// Returns an error for corrupt receipts, changed roots, or unavailable durable storage.
pub fn resume_folder(
    journal_path: &Path,
    cancelled: &AtomicBool,
    progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, String> {
    let journal = MigrationJournal::open(journal_path).map_err(|e| e.to_string())?;
    let source = absolute(&journal.document.source).map_err(|e| e.to_string())?;
    let destination = absolute(&journal.document.destination).map_err(|e| e.to_string())?;
    migration::copy_folder_internal(&source, &destination, cancelled, progress, Some(journal))
}
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf, sync::atomic::Ordering};

    fn decoded_document<T: serde::de::DeserializeOwned>(path: &Path) -> T {
        let journal = MigrationJournal::open(path).unwrap();
        let mut document = journal.document;
        document.version = 1;
        serde_json::from_value(serde_json::to_value(document).unwrap()).unwrap()
    }

    /// Torn tails, recovery failure, legacy upgrades and malformed journals.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn journal_recovery_repairs_torn_tails_and_refuses_corruption() {
        isolated(
            "torn_tail_is_repaired_but_complete_corruption_is_refused",
            || {
                let (_temp, _source, output, journal) = completed_fixture();
                let clean = fs::read(&journal).unwrap();
                for tail in [vec![1, 2, 3], {
                    let mut bytes = 100_u32.to_le_bytes().to_vec();
                    let sequence = MigrationJournal::open(&journal).unwrap().sequence;
                    bytes.extend_from_slice(&sequence.to_le_bytes());
                    bytes.extend_from_slice(&[0; 32]);
                    bytes.extend_from_slice(b"incomplete");
                    bytes
                }] {
                    let mut torn = clean.clone();
                    torn.extend(tail);
                    fs::write(&journal, torn).unwrap();
                    let reopened = MigrationJournal::open(&journal).unwrap();
                    assert_eq!(fs::read(&journal).unwrap(), clean);
                    drop(reopened);
                }
                let mut corrupt = clean;
                corrupt[60] ^= 1;
                fs::write(&journal, &corrupt).unwrap();
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                assert_eq!(fs::read(&journal).unwrap(), corrupt);
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
            },
        );

        isolated("recovery_failure_never_traverses_or_publishes", || {
            let (_temp, source, output, journal) = fixture();
            let cancelled = AtomicBool::new(false);
            copy_folder_with_journal(&source, &output, &journal, &cancelled, |p| {
                if p.verified_files == 1 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            })
            .unwrap();
            let mut bytes = fs::read(&journal).unwrap();
            bytes.extend_from_slice(b"torn");
            fs::write(&journal, &bytes).unwrap();
            let failure = persistence_injection::Failure::at("before_recover_tail", 1);
            assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
            assert_eq!(fs::read(&journal).unwrap(), bytes);
            assert!(!output.join("b").exists());
            drop(failure);
            assert!(
                resume_folder(&journal, &AtomicBool::new(false), |_| {})
                    .unwrap()
                    .complete()
            );
        });

        isolated(
            "legacy_upgrade_failure_preserves_original_and_native_conflict",
            || {
                let (_temp, _source, output, journal) = completed_fixture();
                let legacy: Document = decoded_document(&journal);
                let bytes = serde_json::to_vec(&legacy).unwrap();
                fs::write(&journal, &bytes).unwrap();
                let failure = persistence_injection::Failure::at("legacy_before_replace", 1);
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                assert_eq!(fs::read(&journal).unwrap(), bytes);
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                drop(failure);
                fs::write(output.join("a"), b"other").unwrap();
                let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(!report.complete());
                assert_eq!(fs::read(output.join("a")).unwrap(), b"other");
                assert!(fs::read(&journal).unwrap().starts_with(MAGIC));
            },
        );

        // Phase `legacy_journal_moved_into_output_is_refused_without_conversion`.
        // Even migration/repair of the journal must not mutate a copied tree.
        isolated(
            "legacy_journal_moved_into_output_is_refused_without_conversion",
            || {
                let (_temp, _source, output, journal) = completed_fixture();
                let legacy: Document = decoded_document(&journal);
                let bytes = serde_json::to_vec(&legacy).unwrap();
                let moved = output.join("moved.journal");
                fs::write(&journal, &bytes).unwrap();
                fs::rename(&journal, &moved).unwrap();
                assert!(resume_folder(&moved, &AtomicBool::new(false), |_| {}).is_err());
                assert_eq!(fs::read(&moved).unwrap(), bytes);
            },
        );

        isolated(
            "malformed_schema_and_traversal_are_refused_before_writes",
            || {
                let (_temp, _source, output, journal) = completed_fixture();
                let original: serde_json::Value = decoded_document(&journal);
                let mut cases = vec![b"not json".to_vec()];
                let mut unknown = original.clone();
                unknown["unknown"] = true.into();
                cases.push(serde_json::to_vec(&unknown).unwrap());
                let mut version = original.clone();
                version["version"] = 99.into();
                cases.push(serde_json::to_vec(&version).unwrap());
                for units in [
                    vec![46, 46],
                    vec![47],
                    vec![97, 47, 98],
                    vec![0],
                    vec![97; 256],
                ] {
                    let mut bad = original.clone();
                    bad["receipts"][1]["path"]["components"][0] =
                        serde_json::json!({"platform":"Unix","units":units});
                    cases.push(serde_json::to_vec(&bad).unwrap());
                }
                for bytes in cases {
                    fs::write(&journal, bytes).unwrap();
                    assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                    assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                }
            },
        );
    }

    /// Receipt checkpoints, restart, names, directories and timestamps.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn journal_restart_resumes_from_the_durable_prefix_losslessly() {
        // Phase `receipt_checkpoint_preserves_the_durable_prefix`.
        // A receipt must append bounded bytes, not rewrite all prior checkpoints.
        isolated("receipt_checkpoint_preserves_the_durable_prefix", || {
            let (_temp, source, output, journal) = fixture();
            let cancel = AtomicBool::new(false);
            let mut prefix = Vec::new();
            copy_folder_with_journal(&source, &output, &journal, &cancel, |p| {
                if p.verified_files == 1 && prefix.is_empty() {
                    prefix = fs::read(&journal).unwrap();
                }
            })
            .unwrap();
            let finished = fs::read(&journal).unwrap();
            assert!(
                finished.starts_with(&prefix),
                "checkpoint rewrote earlier durable receipts"
            );
            assert!(resume_folder(&journal, &cancel, |_| {}).unwrap().complete());
        });

        // Phase `restarts_after_one_durable_file_and_preserves_originals`.
        // Removing durable receipts or skipping output revalidation breaks restart.
        isolated(
            "restarts_after_one_durable_file_and_preserves_originals",
            || {
                let (_temp, source, output, journal) = fixture();
                let cancelled = AtomicBool::new(false);
                let report =
                    copy_folder_with_journal(&source, &output, &journal, &cancelled, |p| {
                        if p.verified_files == 1 {
                            cancelled.store(true, Ordering::Relaxed);
                        }
                    })
                    .unwrap();
                assert!(report.cancelled);
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                assert!(!output.join("b").exists());
                let resumed = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(resumed.complete(), "{resumed:?}");
                assert_eq!(resumed.verified_files.len(), 2);
                for base in [&source, &output] {
                    assert_eq!(fs::read(base.join("a")).unwrap(), b"hello");
                    assert_eq!(fs::read(base.join("b")).unwrap(), b"world");
                }
            },
        );

        // Phase `unix_non_utf8_names_survive_restart_losslessly`.
        #[cfg(target_os = "linux")]
        isolated("unix_non_utf8_names_survive_restart_losslessly", || {
            use std::os::unix::ffi::OsStringExt;
            let (_temp, source, output, journal) = fixture();
            let name = std::ffi::OsString::from_vec(vec![b'n', 255]);
            fs::write(source.join(&name), b"native").unwrap();
            assert!(
                copy_folder_with_journal(
                    &source,
                    &output,
                    &journal,
                    &AtomicBool::new(false),
                    |_| {}
                )
                .unwrap()
                .complete()
            );
            assert!(
                resume_folder(&journal, &AtomicBool::new(false), |_| {})
                    .unwrap()
                    .complete()
            );
            assert_eq!(fs::read(output.join(&name)).unwrap(), b"native");
        });

        // Phase `resumed_files_and_directories_retain_original_timestamps`.
        // Revalidation reads must not destroy the timestamp preservation contract.
        isolated(
            "resumed_files_and_directories_retain_original_timestamps",
            || {
                let (_temp, source, output, journal) = fixture();
                fs::create_dir(source.join("empty")).unwrap();
                let time = ui::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
                for p in [&source, &source.join("a"), &source.join("empty")] {
                    File::open(p)
                        .unwrap()
                        .set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
                        .unwrap();
                }
                assert!(
                    copy_folder_with_journal(
                        &source,
                        &output,
                        &journal,
                        &AtomicBool::new(false),
                        |_| {}
                    )
                    .unwrap()
                    .complete()
                );
                // Simulate atime advancement caused by enumeration/content rereads on
                // mounts where access-time updates are enabled (APFS may suppress it).
                for p in [&source, &source.join("empty"), &output.join("a")] {
                    File::open(p)
                        .unwrap()
                        .set_times(
                            fs::FileTimes::new()
                                .set_accessed(time + std::time::Duration::from_secs(60)),
                        )
                        .unwrap();
                }
                assert!(
                    resume_folder(&journal, &AtomicBool::new(false), |_| {})
                        .unwrap()
                        .complete()
                );
                for p in [&output, &output.join("a"), &output.join("empty")] {
                    let m = fs::metadata(p).unwrap();
                    assert_eq!(m.modified().unwrap(), time);
                    assert_eq!(m.accessed().unwrap(), time);
                }
            },
        );

        isolated(
            "nested_empty_directory_and_outcomes_survive_restart",
            || {
                let (_temp, source, output, journal) = fixture();
                fs::create_dir_all(source.join("nested/empty")).unwrap();
                fs::write(source.join("nested/content"), b"nested").unwrap();
                let cancel = AtomicBool::new(false);
                let report = copy_folder_with_journal(&source, &output, &journal, &cancel, |p| {
                    if p.verified_files == 3 {
                        cancel.store(true, Ordering::Relaxed);
                    }
                })
                .unwrap();
                assert!(report.cancelled);
                let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(report.complete(), "{report:?}");
                assert!(output.join("nested/empty").is_dir());
                let document: serde_json::Value = decoded_document(&journal);
                assert_eq!(document["outcome"]["scan_completed"], true);
                assert_eq!(
                    document["receipts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| r["kind"] == "File")
                        .count(),
                    3
                );
                assert!(
                    !document["outcome"]["metadata_limitations"]
                        .as_array()
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(
                    document["receipts"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter(|r| r["kind"] == "Directory" && r["completed"] == true)
                        .count(),
                    3
                );
            },
        );
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a"), b"hello").unwrap();
        fs::write(source.join("b"), b"world").unwrap();
        let private = base.join("journal");
        fs::create_dir(&private).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
        }
        (
            temp,
            source,
            base.join("output"),
            private.join("receipt.json"),
        )
    }

    fn completed_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let f = fixture();
        assert!(
            copy_folder_with_journal(&f.1, &f.2, &f.3, &AtomicBool::new(false), |_| {})
                .unwrap()
                .complete()
        );
        f
    }
    /// Modified sources/destinations, missing sources and replaced roots or journals.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn journal_conflicts_and_replacements_never_authorize_overwrite() {
        // Phase `modified_destination_remains_a_conflict`.
        // A receipt must not conceal an edit or replacement of either version.
        isolated("modified_destination_remains_a_conflict", || {
            let (_temp, source, output, journal) = completed_fixture();
            fs::write(output.join("a"), b"other").unwrap();
            let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(!report.complete());
            assert_eq!(fs::read(output.join("a")).unwrap(), b"other");
            assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
            assert!(!report.verified_files.contains(&PathBuf::from("a")));
        });

        isolated(
            "modified_source_requires_choice_and_preserves_both_versions",
            || {
                let (_temp, source, output, journal) = completed_fixture();
                fs::write(source.join("a"), b"newer").unwrap();
                let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(!report.complete());
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                assert_eq!(fs::read(source.join("a")).unwrap(), b"newer");
            },
        );

        isolated("missing_source_never_authorizes_removal_of_output", || {
            let (_temp, source, output, journal) = completed_fixture();
            fs::remove_file(source.join("a")).unwrap();
            let result = resume_folder(&journal, &AtomicBool::new(false), |_| {});
            assert!(result.is_err() || !result.unwrap().complete());
            assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        });

        isolated(
            "replaces_neither_existing_journal_nor_replaced_roots",
            || {
                let (_temp, source, output, journal) = completed_fixture();
                let original_journal = fs::read(&journal).unwrap();
                assert!(
                    copy_folder_with_journal(
                        &source,
                        &output.with_extension("new"),
                        &journal,
                        &AtomicBool::new(false),
                        |_| {}
                    )
                    .is_err()
                );
                assert_eq!(fs::read(&journal).unwrap(), original_journal);
                fs::rename(&output, output.with_extension("old")).unwrap();
                fs::create_dir(&output).unwrap();
                fs::write(output.join("sentinel"), b"keep").unwrap();
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
                assert!(!output.join("a").exists());
            },
        );

        isolated(
            "replaced_journal_is_never_overwritten_by_a_running_copy",
            || {
                let (_temp, source, output, journal) = fixture();
                let mut replaced = false;
                let result = copy_folder_with_journal(
                    &source,
                    &output,
                    &journal,
                    &AtomicBool::new(false),
                    |p| {
                        if p.verified_files == 1 && !replaced {
                            replaced = true;
                            fs::rename(&journal, journal.with_extension("old")).unwrap();
                            fs::write(&journal, b"replacement").unwrap();
                        }
                    },
                );
                let report = result.expect("persistence failures retain the incomplete report");
                assert!(!report.complete());
                assert_eq!(report.verified_files, [PathBuf::from("a")]);
                assert!(
                    report
                        .failed
                        .iter()
                        .any(|(_, message)| message.contains("journal replaced"))
                );
                assert!(
                    !report.failed.iter().any(
                        |(_, message)| message.contains("previous journal checkpoint retained")
                    )
                );
                assert!(replaced);
                assert_eq!(fs::read(&journal).unwrap(), b"replacement");
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
            },
        );

        isolated("replaced_source_root_is_rejected", || {
            let (_temp, source, output, journal) = completed_fixture();
            fs::rename(&source, source.with_extension("old")).unwrap();
            fs::create_dir(&source).unwrap();
            fs::write(source.join("a"), b"other").unwrap();
            assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
            assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        });
    }
    #[test]
    fn interruption_worker() {
        let Some(base) = std::env::var_os("TILO_JOURNAL_FIXTURE") else {
            return;
        };
        let base = PathBuf::from(base);
        copy_folder_with_journal(
            &base.join("source"),
            &base.join("output"),
            &base.join("journal/receipt.json"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    }
    fn worker_name(name: &str) -> String {
        format!("{}::{name}", module_path!().split_once("::").unwrap().1)
    }
    fn interrupted(boundary: &str, count: &str) -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let f = fixture();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &worker_name("interruption_worker"),
                "--nocapture",
            ])
            .env("TILO_JOURNAL_FIXTURE", f.1.parent().unwrap())
            .env("TILO_JOURNAL_BOUNDARY", boundary)
            .env("TILO_JOURNAL_BOUNDARY_COUNT", count)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(73),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        f
    }
    /// Crashes around publication and receipt replacement, cancelled start.
    #[test]
    fn crash_boundaries_keep_valid_journals_and_revalidate_output() {
        isolated(
            "crash_after_publication_before_receipt_preserves_unjournaled_conflict",
            || {
                let (_temp, source, output, journal) = interrupted("file_published", "1");
                let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(!report.complete());
                assert!(!report.verified_files.contains(&PathBuf::from("a")));
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
                assert_eq!(fs::read(output.join("b")).unwrap(), b"world");
            },
        );

        isolated(
            "crash_before_receipt_replace_keeps_old_valid_journal_and_output",
            || {
                let (_temp, source, output, journal) = interrupted("journal_before_replace", "2");
                let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(!report.complete());
                assert!(!report.verified_files.contains(&PathBuf::from("a")));
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
            },
        );

        isolated(
            "crash_after_receipt_replace_revalidates_published_output",
            || {
                let (_temp, source, output, journal) = interrupted("journal_after_replace", "2");
                let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(report.complete(), "{report:?}");
                assert_eq!(report.verified_files.len(), 2);
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
            },
        );

        isolated(
            "crash_after_root_publication_requires_review_of_unrecorded_root",
            || {
                let (_temp, source, output, journal) = interrupted("root_published", "1");
                assert!(output.is_dir());
                fs::write(output.join("sentinel"), b"keep").unwrap();
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
                assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
            },
        );

        isolated("cancelled_start_does_not_create_receipts_or_output", || {
            let (_temp, source, output, journal) = fixture();
            let report = copy_folder_with_journal(
                &source,
                &output,
                &journal,
                &AtomicBool::new(true),
                |_| {},
            )
            .unwrap();
            assert!(report.cancelled);
            assert!(!journal.exists());
            assert!(!output.exists());
        });
    }
    /// Journal permissions, identity mismatch, moved journals, digests and FIFO swaps.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn resume_revalidates_journal_identity_and_source_content() {
        // Phase `private_journal_permissions_and_nofollow_are_enforced`.
        #[cfg(unix)]
        isolated(
            "private_journal_permissions_and_nofollow_are_enforced",
            || {
                use std::os::unix::fs::PermissionsExt;
                let (_temp, _source, output, journal) = completed_fixture();
                assert_eq!(
                    fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                fs::set_permissions(&journal, fs::Permissions::from_mode(0o644)).unwrap();
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                fs::rename(&journal, journal.with_extension("old")).unwrap();
                std::os::unix::fs::symlink(journal.with_extension("old"), &journal).unwrap();
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
            },
        );

        isolated(
            "platform_and_boot_identity_mismatch_require_reselection",
            || {
                let (_temp, _source, output, journal) = completed_fixture();
                let original: serde_json::Value = decoded_document(&journal);
                let mut foreign = original.clone();
                foreign["source"]["platform"] = "Windows".into();
                fs::write(&journal, serde_json::to_vec(&foreign).unwrap()).unwrap();
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                let mut changed = original;
                changed["source_identity"]["volume"] = serde_json::json!([2, 0, 0, 0]);
                fs::write(&journal, serde_json::to_vec(&changed).unwrap()).unwrap();
                assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
            },
        );

        isolated("resume_refuses_a_journal_moved_into_a_copied_tree", || {
            let (_temp, _source, output, journal) = completed_fixture();
            let moved = output.join("receipt.json");
            fs::rename(&journal, &moved).unwrap();
            let bytes = fs::read(&moved).unwrap();
            assert!(resume_folder(&moved, &AtomicBool::new(false), |_| {}).is_err());
            assert_eq!(fs::read(&moved).unwrap(), bytes);
            assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        });

        isolated(
            "revalidation_checks_digest_even_with_matching_size_and_mtime",
            || {
                let (_temp, _source, output, journal) = completed_fixture();
                let path = output.join("a");
                let modified = fs::metadata(&path).unwrap().modified().unwrap();
                fs::write(&path, b"other").unwrap();
                File::open(&path)
                    .unwrap()
                    .set_times(fs::FileTimes::new().set_modified(modified))
                    .unwrap();
                let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                assert!(!report.complete());
                assert!(!report.verified_files.contains(&PathBuf::from("a")));
                assert_eq!(fs::read(&path).unwrap(), b"other");
            },
        );

        isolated("source_swapped_to_fifo_during_resume_never_blocks", || {
            let (_temp, source, _output, _journal) = fixture();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &worker_name("fifo_race_worker")])
                .env("TILO_FIFO_RACE_WORKER", source.parent().unwrap())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    assert!(source.parent().unwrap().join("fifo-rejected").is_file());
                    break;
                }
                if ui::time::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("resume blocked opening a swapped FIFO");
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
    }
    #[test]
    fn fifo_race_worker() {
        if std::env::var_os("TILO_FIFO_RACE_WORKER").is_none() {
            return;
        }
        let base = PathBuf::from(std::env::var_os("TILO_FIFO_RACE_WORKER").unwrap());
        let source = base.join("source");
        let output = base.join("output");
        let journal = base.join("journal/receipt.json");
        assert!(
            copy_folder_with_journal(&source, &output, &journal, &AtomicBool::new(false), |_| {})
                .unwrap()
                .complete()
        );
        race_injection::BEFORE_OPEN.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                use std::os::unix::ffi::OsStrExt;
                let path = source.join("a");
                fs::rename(&path, source.join("original-a")).unwrap();
                let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: NUL-terminated path in this test's disposable directory.
                assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
            }));
        });
        let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!report.complete());
        fs::write(base.join("fifo-rejected"), b"checked").unwrap();
    }
    /// File, directory, outcome, root and post-publication receipt failures.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn failed_receipts_stop_traversal_and_retain_the_prior_checkpoint() {
        isolated(
            "failed_file_receipt_stops_before_later_files_or_directories",
            || {
                for boundary in ["before_encode", "before_replace", "after_replace"] {
                    let (_temp, source, output, journal) = fixture();
                    fs::create_dir(source.join("later-directory")).unwrap();
                    let failure = persistence_injection::Failure::at(boundary, 2);
                    let result = copy_folder_with_journal(
                        &source,
                        &output,
                        &journal,
                        &AtomicBool::new(false),
                        |_| {},
                    );
                    assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                    assert!(
                        !output.join("b").exists(),
                        "later file published after {boundary} failure"
                    );
                    assert!(!output.join("later-directory").exists());
                    let report =
                        result.expect("post-publication failure must retain the incomplete report");
                    assert!(!report.complete());
                    assert!(!report.scan_completed);
                    assert!(
                        report.verified_files.is_empty(),
                        "unsaved receipt counted as persisted"
                    );
                    assert!(report.failed.iter().any(|(p, _)| p == Path::new("a")));
                    let diagnostic = &report
                        .failed
                        .iter()
                        .find(|(p, _)| p == Path::new("a"))
                        .unwrap()
                        .1;
                    assert!(diagnostic.contains(if boundary == "after_replace" {
                        "replacement is visible but its persistence is uncertain"
                    } else {
                        "previous journal checkpoint retained"
                    }));
                    assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
                    assert_eq!(fs::read(source.join("b")).unwrap(), b"world");
                    drop(failure);
                    let checkpoint: Document = decoded_document(&journal);
                    if boundary != "after_replace" {
                        assert_eq!(
                            checkpoint.receipts.len(),
                            1,
                            "checkpoint advanced after persistence failed"
                        );
                    }
                    let resumed = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                    if boundary == "after_replace" {
                        assert!(resumed.complete(), "{resumed:?}");
                    } else {
                        assert!(!resumed.complete());
                        assert!(!resumed.verified_files.contains(&PathBuf::from("a")));
                    }
                    assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                }
            },
        );

        isolated(
            "failed_directory_receipt_or_completion_stops_parent_traversal",
            || {
                for (boundary, save) in [
                    ("before_replace", 2),
                    ("before_replace", 3),
                    ("directory_publication_flush", 1),
                    ("directory_completion_flush", 1),
                ] {
                    let (_temp, source, output, journal) = fixture();
                    fs::create_dir(source.join("0-directory")).unwrap();
                    let failure = persistence_injection::Failure::at(boundary, save);
                    let result = copy_folder_with_journal(
                        &source,
                        &output,
                        &journal,
                        &AtomicBool::new(false),
                        |_| {},
                    );
                    assert!(output.join("0-directory").is_dir());
                    assert!(
                        !output.join("a").exists(),
                        "parent traversal continued after directory checkpoint failure"
                    );
                    assert!(!output.join("b").exists());
                    let report = result.expect("directory persistence failure must retain report");
                    assert!(!report.complete());
                    assert!(!report.scan_completed);
                    assert!(
                        report
                            .failed
                            .iter()
                            .any(|(p, _)| p == Path::new("0-directory"))
                    );
                    drop(failure);
                    assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
                    let resumed = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                    if (boundary == "before_replace" && save == 2)
                        || boundary == "directory_publication_flush"
                    {
                        assert!(!resumed.complete());
                    } else {
                        assert!(resumed.complete(), "{resumed:?}");
                    }
                    assert!(output.join("0-directory").is_dir());
                }
            },
        );

        isolated(
            "failed_final_outcome_save_retains_verified_report_and_prior_checkpoint",
            || {
                for boundary in ["before_encode", "before_replace", "after_replace"] {
                    let (_temp, source, output, journal) = fixture();
                    let failure = persistence_injection::Failure::at(boundary, 5);
                    let report = copy_folder_with_journal(
                        &source,
                        &output,
                        &journal,
                        &AtomicBool::new(false),
                        |_| {},
                    )
                    .expect("final persistence failure must retain accumulated results");
                    assert!(!report.complete());
                    assert_eq!(report.verified_files.len(), 2);
                    assert_eq!(report.verified_bytes, 10);
                    assert_eq!(report.failed.len(), 1);
                    assert!(report.failed[0].1.contains(if boundary == "after_replace" {
                        "replacement is visible but its persistence is uncertain"
                    } else {
                        "previous journal checkpoint retained"
                    }));
                    let checkpoint: Document = decoded_document(&journal);
                    if boundary != "after_replace" {
                        assert!(checkpoint.outcome.is_none());
                    }
                    drop(failure);
                    assert!(
                        resume_folder(&journal, &AtomicBool::new(false), |_| {})
                            .unwrap()
                            .complete()
                    );
                    for base in [&source, &output] {
                        assert_eq!(fs::read(base.join("a")).unwrap(), b"hello");
                        assert_eq!(fs::read(base.join("b")).unwrap(), b"world");
                    }
                }
            },
        );

        isolated(
            "failed_initial_root_receipt_returns_incomplete_report_without_traversal",
            || {
                for boundary in ["root_publication_flush", "before_replace"] {
                    let (_temp, source, output, journal) = fixture();
                    let failure = persistence_injection::Failure::at(boundary, 1);
                    let report = copy_folder_with_journal(
                        &source,
                        &output,
                        &journal,
                        &AtomicBool::new(false),
                        |_| {},
                    )
                    .expect("root receipt failed after mkdir; retain report");
                    assert!(!report.complete());
                    assert!(output.is_dir());
                    assert!(!output.join("a").exists());
                    assert!(!report.failed.is_empty());
                    drop(failure);
                    assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
                    assert!(output.is_dir());
                    assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
                }
            },
        );

        isolated(
            "failed_post_publication_flush_or_receipt_construction_stops_later_entries",
            || {
                for boundary in ["file_publication_flush", "file_receipt_construct"] {
                    let (_temp, source, output, journal) = fixture();
                    let failure = persistence_injection::Failure::at(boundary, 1);
                    let report = copy_folder_with_journal(
                        &source,
                        &output,
                        &journal,
                        &AtomicBool::new(false),
                        |_| {},
                    )
                    .unwrap();
                    assert!(!report.complete());
                    assert!(!report.scan_completed);
                    assert!(report.verified_files.is_empty());
                    assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                    assert!(!output.join("b").exists());
                    assert!(report.failed.iter().any(|(path, _)| path == Path::new("a")));
                    let checkpoint: Document = decoded_document(&journal);
                    assert_eq!(checkpoint.receipts.len(), 1);
                    drop(failure);
                    let resumed = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
                    assert!(!resumed.complete());
                    assert!(!resumed.verified_files.contains(&PathBuf::from("a")));
                    assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
                    assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
                    assert_eq!(fs::read(source.join("b")).unwrap(), b"world");
                }
            },
        );
    }

    /// Runs one merged phase on a fresh thread so the thread-local race and
    /// persistence injection hooks start clean, exactly as when the phase was
    /// its own test; a failing phase re-raises its original panic payload.
    fn isolated(phase: &str, body: impl FnOnce() + Send) {
        std::thread::scope(|scope| {
            let handle = std::thread::Builder::new()
                .name(phase.to_owned())
                .spawn_scoped(scope, body)
                .expect("spawn isolated test phase");
            if let Err(panic) = handle.join() {
                std::panic::resume_unwind(panic);
            }
        });
    }
}

#[cfg(test)]
pub(super) mod crash_injection {
    use std::cell::Cell;
    thread_local! { static COUNT: Cell<usize> = const { Cell::new(0) }; }
    // Intentional abrupt child exit exercises process-crash recovery boundaries.
    #[allow(
        clippy::exit,
        reason = "Crash-recovery tests deliberately exit the child at a selected journal boundary without running destructors."
    )]
    pub(crate) fn boundary(name: &str) {
        if std::env::var("TILO_JOURNAL_BOUNDARY").ok().as_deref() != Some(name) {
            return;
        }
        let nth = std::env::var("TILO_JOURNAL_BOUNDARY_COUNT")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        COUNT.with(|count| {
            count.set(count.get() + 1);
            if count.get() == nth {
                std::process::exit(73);
            }
        });
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    #[test]
    fn journaled_copy_refuses_before_mutation_until_native_durability_is_available() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("output");
        let journal = temp.path().join("journal");
        let result = super::copy_folder_with_journal(
            &source,
            &destination,
            &journal,
            &std::sync::atomic::AtomicBool::new(false),
            |_| {},
        );
        assert!(result.is_err());
        assert!(!destination.exists());
        assert!(!journal.exists());
    }
}

#[cfg(test)]
mod race_injection {
    use std::cell::RefCell;
    thread_local! { pub(super) static BEFORE_OPEN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) }; }
    pub(super) fn before_open() {
        BEFORE_OPEN.with(|hook| {
            if let Some(action) = hook.borrow_mut().take() {
                action();
            }
        });
    }
}

#[cfg(test)]
pub(super) mod persistence_injection {
    use std::{cell::RefCell, io};
    thread_local! { static FAILURE: RefCell<Option<(&'static str,usize)>> = const { RefCell::new(None) }; }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub(in crate::setup) struct Failure;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl Failure {
        pub(in crate::setup) fn at(boundary: &'static str, nth: usize) -> Self {
            FAILURE.with(|f| *f.borrow_mut() = Some((boundary, nth)));
            Self
        }
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl Drop for Failure {
        fn drop(&mut self) {
            FAILURE.with(|f| *f.borrow_mut() = None);
        }
    }
    pub(crate) fn fail_at(boundary: &str) -> io::Result<()> {
        FAILURE.with(|f| {
            let mut f = f.borrow_mut();
            if let Some((target, nth)) = &mut *f
                && *target == boundary
            {
                *nth -= 1;
                if *nth == 0 {
                    *f = None;
                    return Err(io::Error::new(
                        if boundary == "before_encode" {
                            io::ErrorKind::InvalidData
                        } else {
                            io::ErrorKind::WriteZero
                        },
                        "injected checkpoint failure",
                    ));
                }
            }
            Ok(())
        })
    }
}
