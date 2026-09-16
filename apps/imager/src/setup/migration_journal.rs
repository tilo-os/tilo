//! Versioned private receipts. A receipt is a hint to revalidate, never overwrite authority.
use super::migration::{self, CopyProgress, CopyReport, OwnedTemporary, PinnedDirectory, changed};
use super::{
    destination_security,
    migration_durability::{self as durability, Identity},
    nofollow::{metadata_nofollow, same_directory_identity},
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::File,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::SystemTime,
};

const MAX_JOURNAL: u64 = 16 * 1024 * 1024;

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
            .is_some_and(|error| error.is::<PersistenceError>())
        {
            cause
        } else {
            io::Error::other(PersistenceError { boundary, cause })
        }
    })
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
struct Snapshot {
    size: u64,
    modified: SystemTime,
    created: Option<SystemTime>,
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}
impl Snapshot {
    fn new(m: &Metadata) -> io::Result<Self> {
        use cap_fs_ext::MetadataExt;
        Ok(Self {
            size: m.len(),
            modified: m.modified()?.into_std(),
            created: m.created().ok().map(|t| t.into_std()),
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
#[derive(Serialize, Deserialize)]
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
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    operation: String,
    source: NativeName,
    destination: NativeName,
    source_identity: Identity,
    destination_parent_identity: Identity,
    receipts: Vec<Receipt>,
    outcome: Option<Outcome>,
}
/// In-memory owner of one exclusively created, versioned private journal.
/// All persisted paths are native code units; no permissions or grants are serialized.
pub struct MigrationJournal {
    document: Document,
    parent: Arc<PinnedDirectory>,
    name: OsString,
    file: File,
    identity: Metadata,
    resume: bool,
    seen: BTreeSet<PathBuf>,
    failure: Option<JournalFailure>,
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
    if !path.is_absolute() {
        return Err(changed("journal path must be absolute"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| changed("journal filename missing"))?
        .to_os_string();
    RelativePath::encode(Path::new(&name))?;
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| changed("journal parent missing"))?,
    )?;
    parent.validate_destination()?;
    destination_security::check_private_file(&parent.file)?;
    Ok((parent, name))
}
impl MigrationJournal {
    fn create(
        source_path: &Path,
        destination_path: &Path,
        journal_path: &Path,
    ) -> io::Result<Self> {
        durability::supported()?;
        let source = PinnedDirectory::open_absolute(source_path)?;
        let destination_parent = PinnedDirectory::open_absolute(
            destination_path
                .parent()
                .ok_or_else(|| changed("destination parent missing"))?,
        )?;
        destination_parent.validate_destination()?;
        let (parent, name) = journal_parent(journal_path)?;
        if parent.contains_identity(&source.identity)
            || source.contains_identity(&parent.identity)
            || destination_path.starts_with(journal_path.parent().unwrap())
        {
            return Err(changed(
                "journal must be outside both copied trees in a separate private directory",
            ));
        }
        let document = Document {
            version: 1,
            operation: format!("{}-{:?}", std::process::id(), SystemTime::now()),
            source: NativeName::encode(source_path.as_os_str()),
            destination: NativeName::encode(destination_path.as_os_str()),
            source_identity: directory_identity(&source)?,
            destination_parent_identity: directory_identity(&destination_parent)?,
            receipts: Vec::new(),
            outcome: None,
        };
        absolute(&document.source)?;
        absolute(&document.destination)?;
        let mut temporary = OwnedTemporary::create(&parent, &name)?;
        temporary.file.write_all(&serde_json::to_vec(&document)?)?;
        durability::sync_file(&temporary.file)?;
        parent.validate_destination()?;
        cap_primitives::fs::hard_link(
            &parent.file,
            Path::new(&temporary.name),
            &parent.file,
            Path::new(&name),
        )?;
        durability::sync_directory(&parent.file)?;
        let file = temporary.file.try_clone()?;
        let identity = Metadata::from_file(&file)?;
        Ok(Self {
            document,
            parent,
            name,
            file,
            identity,
            resume: false,
            seen: BTreeSet::new(),
            failure: None,
        })
    }
    fn open(path: &Path) -> io::Result<Self> {
        durability::supported()?;
        let (parent, name) = journal_parent(path)?;
        let expected = metadata_nofollow(&parent.file, Path::new(&name))?;
        if !expected.is_file() || expected.is_symlink() || expected.len() > MAX_JOURNAL {
            return Err(changed("journal is not a bounded regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let mut file = cap_primitives::fs::open(&parent.file, Path::new(&name), &options)?;
        destination_security::check_private_file(&file)?;
        let identity = Metadata::from_file(&file)?;
        if !migration::same_snapshot(&expected, &identity) {
            return Err(changed("journal replaced while opening"));
        }
        let mut data = Vec::new();
        (&mut file).take(MAX_JOURNAL + 1).read_to_end(&mut data)?;
        if data.len() as u64 > MAX_JOURNAL {
            return Err(changed("journal exceeds size bound"));
        }
        let document: Document = serde_json::from_slice(&data)?;
        if document.version != 1 || document.operation.is_empty() || document.operation.len() > 128
        {
            return Err(changed("unsupported journal schema or operation"));
        }
        absolute(&document.source)?;
        absolute(&document.destination)?;
        let mut paths = BTreeSet::new();
        for receipt in &document.receipts {
            if !paths.insert(receipt.path().decode()?) {
                return Err(changed("duplicate journal receipt"));
            }
        }
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
        let result = Self {
            document,
            parent,
            name,
            file,
            identity,
            resume: true,
            seen: BTreeSet::new(),
            failure: None,
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> io::Result<()> {
        self.parent.validate_destination()?;
        destination_security::check_private_file(&self.parent.file)?;
        destination_security::check_private_file(&self.file)?;
        let current = metadata_nofollow(&self.parent.file, Path::new(&self.name))?;
        if !current.is_file()
            || current.is_symlink()
            || !same_directory_identity(&current, &self.identity)
        {
            return Err(changed("journal replaced; reselect and review operation"));
        }
        Ok(())
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

    fn save(&mut self) -> io::Result<()> {
        let mut boundary = PersistenceBoundary::PreviousCheckpoint;
        let result = (|| {
            persistence_result(self.validate(), PersistenceBoundary::JournalUnavailable)?;
            #[cfg(test)]
            persistence_injection::fail_at("before_encode")?;
            let bytes = serde_json::to_vec(&self.document)?;
            if bytes.len() as u64 > MAX_JOURNAL {
                return Err(changed("journal capacity exceeded"));
            }
            let mut temporary = OwnedTemporary::create(&self.parent, &self.name)?;
            temporary.file.write_all(&bytes)?;
            durability::sync_file(&temporary.file)?;
            persistence_result(self.validate(), PersistenceBoundary::JournalUnavailable)?;
            #[cfg(test)]
            crash_injection::boundary("journal_before_replace");
            #[cfg(test)]
            persistence_injection::fail_at("before_replace")?;
            cap_primitives::fs::rename(
                &self.parent.file,
                Path::new(&temporary.name),
                &self.parent.file,
                Path::new(&self.name),
            )?;
            boundary = PersistenceBoundary::JournalReplacementUncertain;
            #[cfg(test)]
            crash_injection::boundary("journal_after_replace");
            #[cfg(test)]
            persistence_injection::fail_at("after_replace")?;
            self.file = temporary.file.try_clone()?;
            self.identity = Metadata::from_file(&self.file)?;
            durability::sync_directory(&self.parent.file)
        })();
        persistence_result(result, boundary)
    }
    pub(super) fn validate_relative(&self, path: &Path) -> io::Result<()> {
        RelativePath::encode(path).map(|_| ())
    }
    fn lookup(&self, path: &Path) -> Option<&Receipt> {
        self.document
            .receipts
            .iter()
            .find(|r| r.path().decode().is_ok_and(|p| p == path))
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
                journal.document.receipts.push(Receipt::Directory {
                    path: RelativePath::encode(path)?,
                    source: directory_identity(source)?,
                    destination: directory_identity(destination)?,
                    accessed: Metadata::from_file(&source.file)?.accessed()?.into_std(),
                    modified: Metadata::from_file(&source.file)?.modified()?.into_std(),
                    completed: false,
                });
                journal.save()
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
                for receipt in &mut journal.document.receipts {
                    if let Receipt::Directory {
                        path: p, completed, ..
                    } = receipt
                        && p.decode()? == path
                    {
                        *completed = true;
                    }
                }
                journal.save()
            },
        )
    }
    pub(super) fn record_file(&mut self, path: &Path, facts: FileReceipt) -> io::Result<()> {
        self.checkpoint(path, PersistenceBoundary::OutputUnrecorded, |journal| {
            journal.document.receipts.push(Receipt::File {
                path: RelativePath::encode(path)?,
                facts,
            });
            journal.save()?;
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
        self.document.outcome = Some(Outcome {
            verified: report
                .verified_files
                .iter()
                .map(|p| RelativePath::encode(p))
                .collect::<io::Result<_>>()?,
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
        });
        self.save()
    }
}

/// Start a fresh durable operation. The caller supplies a private application-data
/// journal directory outside both copied trees. Existing journal/output names
/// are conflicts. Windows currently refuses before creating any files because
/// unprivileged directory-entry persistence is not yet established.
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
/// Reopen a private journal and revalidate native roots, source facts and output
/// bytes. Changed or unrecorded output is retained as a conflict requiring review.
/// Linux without a persistent filesystem UUID is restricted to the same boot.
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

    // Removing durable receipts or skipping output revalidation breaks restart.
    #[test]
    fn restarts_after_one_durable_file_and_preserves_originals() {
        let (_temp, source, output, journal) = fixture();
        let cancelled = AtomicBool::new(false);
        let report = copy_folder_with_journal(&source, &output, &journal, &cancelled, |p| {
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
    // A receipt must not conceal an edit or replacement of either version.
    #[test]
    fn modified_destination_remains_a_conflict() {
        let (_temp, source, output, journal) = completed_fixture();
        fs::write(output.join("a"), b"other").unwrap();
        let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!report.complete());
        assert_eq!(fs::read(output.join("a")).unwrap(), b"other");
        assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
        assert!(!report.verified_files.contains(&PathBuf::from("a")));
    }
    #[test]
    fn modified_source_requires_choice_and_preserves_both_versions() {
        let (_temp, source, output, journal) = completed_fixture();
        fs::write(source.join("a"), b"newer").unwrap();
        let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!report.complete());
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        assert_eq!(fs::read(source.join("a")).unwrap(), b"newer");
    }
    #[test]
    fn missing_source_never_authorizes_removal_of_output() {
        let (_temp, source, output, journal) = completed_fixture();
        fs::remove_file(source.join("a")).unwrap();
        let result = resume_folder(&journal, &AtomicBool::new(false), |_| {});
        assert!(result.is_err() || !result.unwrap().complete());
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
    }
    #[test]
    fn replaces_neither_existing_journal_nor_replaced_roots() {
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
    }
    #[test]
    fn malformed_schema_and_traversal_are_refused_before_writes() {
        let (_temp, _source, output, journal) = completed_fixture();
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
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
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn unix_non_utf8_names_survive_restart_losslessly() {
        use std::os::unix::ffi::OsStringExt;
        let (_temp, source, output, journal) = fixture();
        let name = std::ffi::OsString::from_vec(vec![b'n', 255]);
        fs::write(source.join(&name), b"native").unwrap();
        assert!(
            copy_folder_with_journal(&source, &output, &journal, &AtomicBool::new(false), |_| {})
                .unwrap()
                .complete()
        );
        assert!(
            resume_folder(&journal, &AtomicBool::new(false), |_| {})
                .unwrap()
                .complete()
        );
        assert_eq!(fs::read(output.join(&name)).unwrap(), b"native");
    }
    // Revalidation reads must not destroy the timestamp preservation contract.
    #[test]
    fn resumed_files_and_directories_retain_original_timestamps() {
        let (_temp, source, output, journal) = fixture();
        fs::create_dir(source.join("empty")).unwrap();
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
        for p in [&source, &source.join("a"), &source.join("empty")] {
            File::open(p)
                .unwrap()
                .set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
                .unwrap();
        }
        assert!(
            copy_folder_with_journal(&source, &output, &journal, &AtomicBool::new(false), |_| {})
                .unwrap()
                .complete()
        );
        // Simulate atime advancement caused by enumeration/content rereads on
        // mounts where access-time updates are enabled (APFS may suppress it).
        for p in [&source, &source.join("empty"), &output.join("a")] {
            File::open(p)
                .unwrap()
                .set_times(
                    fs::FileTimes::new().set_accessed(time + std::time::Duration::from_secs(60)),
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
    }

    #[test]
    fn interruption_worker() {
        let Some(base) = std::env::var_os("QOS_JOURNAL_FIXTURE") else {
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
            .env("QOS_JOURNAL_FIXTURE", f.1.parent().unwrap())
            .env("QOS_JOURNAL_BOUNDARY", boundary)
            .env("QOS_JOURNAL_BOUNDARY_COUNT", count)
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
    #[test]
    fn crash_after_publication_before_receipt_preserves_unjournaled_conflict() {
        let (_temp, source, output, journal) = interrupted("file_published", "1");
        let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!report.complete());
        assert!(!report.verified_files.contains(&PathBuf::from("a")));
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
        assert_eq!(fs::read(output.join("b")).unwrap(), b"world");
    }
    #[test]
    fn crash_before_receipt_replace_keeps_old_valid_journal_and_output() {
        let (_temp, source, output, journal) = interrupted("journal_before_replace", "2");
        let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!report.complete());
        assert!(!report.verified_files.contains(&PathBuf::from("a")));
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
    }
    #[test]
    fn crash_after_receipt_replace_revalidates_published_output() {
        let (_temp, source, output, journal) = interrupted("journal_after_replace", "2");
        let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(report.complete(), "{report:?}");
        assert_eq!(report.verified_files.len(), 2);
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
    }
    #[test]
    fn crash_after_root_publication_requires_review_of_unrecorded_root() {
        let (_temp, source, output, journal) = interrupted("root_published", "1");
        assert!(output.is_dir());
        fs::write(output.join("sentinel"), b"keep").unwrap();
        assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
        assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
    }
    #[test]
    fn cancelled_start_does_not_create_receipts_or_output() {
        let (_temp, source, output, journal) = fixture();
        let report =
            copy_folder_with_journal(&source, &output, &journal, &AtomicBool::new(true), |_| {})
                .unwrap();
        assert!(report.cancelled);
        assert!(!journal.exists());
        assert!(!output.exists());
    }
    #[test]
    fn replaced_journal_is_never_overwritten_by_a_running_copy() {
        let (_temp, source, output, journal) = fixture();
        let mut replaced = false;
        let result =
            copy_folder_with_journal(&source, &output, &journal, &AtomicBool::new(false), |p| {
                if p.verified_files == 1 && !replaced {
                    replaced = true;
                    fs::rename(&journal, journal.with_extension("old")).unwrap();
                    fs::write(&journal, b"replacement").unwrap();
                }
            });
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
            !report
                .failed
                .iter()
                .any(|(_, message)| message.contains("previous journal checkpoint retained"))
        );
        assert!(replaced);
        assert_eq!(fs::read(&journal).unwrap(), b"replacement");
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
        assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
    }
    #[test]
    fn replaced_source_root_is_rejected() {
        let (_temp, source, output, journal) = completed_fixture();
        fs::rename(&source, source.with_extension("old")).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a"), b"other").unwrap();
        assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
    }
    #[cfg(unix)]
    #[test]
    fn private_journal_permissions_and_nofollow_are_enforced() {
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
    }
    #[test]
    fn nested_empty_directory_and_outcomes_survive_restart() {
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
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
        assert_eq!(document["outcome"]["scan_completed"], true);
        assert_eq!(document["outcome"]["verified"].as_array().unwrap().len(), 3);
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
    }
    #[test]
    fn platform_and_boot_identity_mismatch_require_reselection() {
        let (_temp, _source, output, journal) = completed_fixture();
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
        let mut foreign = original.clone();
        foreign["source"]["platform"] = "Windows".into();
        fs::write(&journal, serde_json::to_vec(&foreign).unwrap()).unwrap();
        assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
        let mut changed = original;
        changed["source_identity"]["volume"] = serde_json::json!([2, 0, 0, 0]);
        fs::write(&journal, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(resume_folder(&journal, &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
    }
    #[test]
    fn resume_refuses_a_journal_moved_into_a_copied_tree() {
        let (_temp, _source, output, journal) = completed_fixture();
        let moved = output.join("receipt.json");
        fs::rename(&journal, &moved).unwrap();
        let bytes = fs::read(&moved).unwrap();
        assert!(resume_folder(&moved, &AtomicBool::new(false), |_| {}).is_err());
        assert_eq!(fs::read(&moved).unwrap(), bytes);
        assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
    }
    #[test]
    fn revalidation_checks_digest_even_with_matching_size_and_mtime() {
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
    }

    #[test]
    fn fifo_race_worker() {
        if std::env::var_os("QOS_FIFO_RACE_WORKER").is_none() {
            return;
        }
        let base = PathBuf::from(std::env::var_os("QOS_FIFO_RACE_WORKER").unwrap());
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
            }))
        });
        let report = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
        assert!(!report.complete());
        fs::write(base.join("fifo-rejected"), b"checked").unwrap();
    }
    #[test]
    fn source_swapped_to_fifo_during_resume_never_blocks() {
        let (_temp, source, _output, _journal) = fixture();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &worker_name("fifo_race_worker")])
            .env("QOS_FIFO_RACE_WORKER", source.parent().unwrap())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                assert!(source.parent().unwrap().join("fifo-rejected").is_file());
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("resume blocked opening a swapped FIFO");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    fn failed_file_receipt_stops_before_later_files_or_directories() {
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
            let checkpoint: Document =
                serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
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
    }
    #[test]
    fn failed_directory_receipt_or_completion_stops_parent_traversal() {
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
    }
    #[test]
    fn failed_final_outcome_save_retains_verified_report_and_prior_checkpoint() {
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
            let checkpoint: Document =
                serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
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
    }
    #[test]
    fn failed_initial_root_receipt_returns_incomplete_report_without_traversal() {
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
    }
    #[test]
    fn failed_post_publication_flush_or_receipt_construction_stops_later_entries() {
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
            let checkpoint: Document =
                serde_json::from_slice(&fs::read(&journal).unwrap()).unwrap();
            assert_eq!(checkpoint.receipts.len(), 1);
            drop(failure);
            let resumed = resume_folder(&journal, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(!resumed.complete());
            assert!(!resumed.verified_files.contains(&PathBuf::from("a")));
            assert_eq!(fs::read(output.join("a")).unwrap(), b"hello");
            assert_eq!(fs::read(source.join("a")).unwrap(), b"hello");
            assert_eq!(fs::read(source.join("b")).unwrap(), b"world");
        }
    }
}

#[cfg(test)]
pub(super) mod crash_injection {
    use std::cell::Cell;
    thread_local! { static COUNT: Cell<usize> = const { Cell::new(0) }; }
    pub(crate) fn boundary(name: &str) {
        if std::env::var("QOS_JOURNAL_BOUNDARY").ok().as_deref() != Some(name) {
            return;
        }
        let nth = std::env::var("QOS_JOURNAL_BOUNDARY_COUNT")
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
    pub(super) struct Failure;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl Failure {
        pub(super) fn at(boundary: &'static str, nth: usize) -> Self {
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
