//! Verified, one-time folder copying into a fresh destination.
//!
//! Grants must be native absolute paths without symlink components. Directory
//! handles, not estimates or queued absolute paths, authorize traversal. No
//! source writes, merges, automatic retries, or executable launches occur here.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cap_fs_ext::OpenOptionsFollowExt;
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use cap_std::fs::Dir;
use sha2::{Digest, Sha256};

use super::destination_security;
use super::migration_durability;
use super::migration_journal::{MigrationJournal, PersistenceBoundary, persistence_result};
pub use super::migration_journal::{copy_folder_with_journal, resume_folder};
use super::migration_source::{CopyFileSource, CopyPass, LocalCopySource, SourcePass};
use super::nofollow::{metadata_nofollow, open_directory_nofollow, same_directory_identity};

const BUFFER_SIZE: usize = 64 * 1024;
const MAX_DEPTH: usize = 128;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Default)]
pub struct CopyProgress {
    pub verified_files: u64,
    pub verified_bytes: u64,
    pub current_relative_path: PathBuf,
}

/// Paths are relative to the selected source. Verified output is retained on
/// cancellation/failure. General fidelity disclosures do not assert metadata
/// equivalence; errors preserving supported timestamps make `complete` false.
#[derive(Clone, Debug, Default)]
pub struct CopyReport {
    pub verified_files: Vec<PathBuf>,
    pub verified_bytes: u64,
    pub skipped: Vec<(PathBuf, String)>,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
    pub scan_completed: bool,
    pub metadata_limitations: Vec<String>,
}

impl CopyReport {
    pub fn complete(&self) -> bool {
        self.scan_completed && !self.cancelled && self.skipped.is_empty() && self.failed.is_empty()
    }
}

pub(super) fn changed(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn check_cancel(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "copy cancelled"))
    } else {
        Ok(())
    }
}

/// Each edge retains both the parent capability and the observed child identity.
/// Sharing ancestors bounds descriptors to the depth of the active traversal.
pub(super) struct PinnedDirectory {
    pub(super) file: File,
    pub(super) identity: Metadata,
    parent: Option<(Arc<Self>, OsString)>,
    created: bool,
}

impl PinnedDirectory {
    pub(super) fn open_absolute(path: &Path) -> io::Result<Arc<Self>> {
        if !path.is_absolute() {
            return Err(changed("folder grant must be a native absolute path"));
        }
        let mut anchor = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir if names.is_empty() => {
                    anchor.push(component);
                }
                Component::Normal(name) => names.push(name),
                _ => return Err(changed("folder grant contains a non-normal component")),
            }
        }
        let file =
            cap_primitives::fs::open_ambient_dir(&anchor, cap_primitives::ambient_authority())?;
        let identity = Metadata::from_file(&file)?;
        let mut directory = Arc::new(Self {
            file,
            identity,
            parent: None,
            created: false,
        });
        for name in names {
            let metadata = metadata_nofollow(&directory.file, Path::new(name))?;
            directory = directory.open_child(name, &metadata)?;
        }
        Ok(directory)
    }

    pub(super) fn open_child(
        self: &Arc<Self>,
        name: &OsStr,
        expected: &Metadata,
    ) -> io::Result<Arc<Self>> {
        reject_windows_placeholder(expected)?;
        let file = open_directory_nofollow(&self.file, Path::new(name), expected)?;
        Ok(Arc::new(Self {
            identity: Metadata::from_file(&file)?,
            file,
            parent: Some((Arc::clone(self), name.to_os_string())),
            created: false,
        }))
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        let mut current = self;
        while let Some((parent, name)) = &current.parent {
            let metadata = metadata_nofollow(&parent.file, Path::new(name))?;
            if metadata.is_symlink()
                || !metadata.is_dir()
                || !same_directory_identity(&metadata, &current.identity)
            {
                return Err(changed(
                    "directory identity changed; select the folder again",
                ));
            }
            reject_windows_placeholder(&metadata)?;
            current = parent;
        }
        Ok(())
    }

    pub(super) fn contains_identity(&self, identity: &Metadata) -> bool {
        let mut current = self;
        loop {
            if same_directory_identity(&current.identity, identity) {
                return true;
            }
            match &current.parent {
                Some((parent, _)) => current = parent,
                None => return false,
            }
        }
    }

    pub(super) fn validate_destination(&self) -> io::Result<()> {
        self.validate()?;
        let mut current = self;
        let mut child = None;
        loop {
            destination_security::check_directory(&current.file, child.as_ref(), current.created)?;
            child = Some(Metadata::from_file(&current.file)?);
            match &current.parent {
                Some((parent, _)) => current = parent,
                None => break,
            }
        }
        Ok(())
    }

    pub(super) fn create_child(self: &Arc<Self>, name: &OsStr) -> io::Result<Arc<Self>> {
        // Establish namespace protection BEFORE mkdir. Observation after an
        // unprotected mkdir could adopt another account's replacement.
        self.validate_destination()?;
        destination_security::create_directory(&self.file, name)?;
        let expected = metadata_nofollow(&self.file, Path::new(name))?;
        let file = open_directory_nofollow(&self.file, Path::new(name), &expected)?;
        destination_security::check_private_file(&file)?;
        let child = Arc::new(Self {
            identity: Metadata::from_file(&file)?,
            file,
            parent: Some((Arc::clone(self), name.to_os_string())),
            created: true,
        });
        child.validate_destination()?;
        Ok(child)
    }
}

#[cfg_attr(
    not(windows),
    allow(
        clippy::unnecessary_wraps,
        reason = "Windows rejects reparse points and cloud placeholders; other platforms retain the same fallible validation interface."
    )
)]
pub(super) fn reject_windows_placeholder(metadata: &Metadata) -> io::Result<()> {
    #[cfg(windows)]
    {
        use cap_primitives::fs::MetadataExt;
        // All reparse points (including cloud placeholders/junctions), offline,
        // recall-on-open and recall-on-data-access require a separate safe path.
        if metadata.file_attributes() & (0x400 | 0x1000 | 0x0004_0000 | 0x0040_0000) != 0 {
            return Err(changed("reparse point or cloud placeholder is unsupported"));
        }
    }
    #[cfg(not(windows))]
    let _ = metadata;
    Ok(())
}

pub(super) fn same_snapshot(before: &Metadata, after: &Metadata) -> bool {
    if !same_directory_identity(before, after)
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
        || before.permissions() != after.permissions()
        || before.is_file() != after.is_file()
        || before.is_dir() != after.is_dir()
        || after.is_symlink()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use cap_primitives::fs::MetadataExt;
        if before.ctime() != after.ctime() || before.ctime_nsec() != after.ctime_nsec() {
            return false;
        }
    }
    #[cfg(windows)]
    {
        use cap_primitives::fs::MetadataExt;
        if before.file_attributes() != after.file_attributes() {
            return false;
        }
    }
    true
}

// Linux scanner handles may be O_PATH and Windows scanner handles lack
// FILE_WRITE_ATTRIBUTES. Reopen only the fresh destination using its pinned
// parent, no-follow lookup and identity comparison before setting timestamps.
fn preserve_directory_times(directory: &PinnedDirectory, original: &Metadata) -> io::Result<()> {
    preserve_directory_times_values(
        directory,
        original.accessed()?.into_std(),
        original.modified()?.into_std(),
    )
}

pub(super) fn preserve_directory_times_values(
    directory: &PinnedDirectory,
    accessed: ui::time::SystemTime,
    modified: ui::time::SystemTime,
) -> io::Result<()> {
    use cap_fs_ext::OpenOptionsMaybeDirExt;
    directory.validate_destination()?;
    let (parent, name) = directory
        .parent
        .as_ref()
        .ok_or_else(|| changed("destination has no pinned parent"))?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .maybe_dir(true)
        .follow(FollowSymlinks::No);
    #[cfg(windows)]
    {
        use cap_primitives::fs::OpenOptionsExt;
        options.access_mode(0x80 | 0x100); // FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES
    }
    let writable = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    let observed = Metadata::from_file(&writable)?;
    if !observed.is_dir() || !same_directory_identity(&directory.identity, &observed) {
        return Err(changed("destination changed before timestamp preservation"));
    }
    preserve_time_values(&writable, accessed, modified)
}

pub(super) fn preserve_time_values(
    file: &File,
    accessed: ui::time::SystemTime,
    modified: ui::time::SystemTime,
) -> io::Result<()> {
    file.set_times(
        fs::FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )?;
    let actual = file.metadata()?;
    if actual.modified()? != modified || actual.accessed()? != accessed {
        return Err(changed(
            "destination cannot preserve source timestamps exactly",
        ));
    }
    Ok(())
}

/// The only removable entry is an exclusive temporary file whose current
/// no-follow identity still matches the handle created by this operation.
pub(super) struct OwnedTemporary {
    parent: Arc<PinnedDirectory>,
    pub(super) name: OsString,
    pub(super) file: File,
    pub(super) identity: Metadata,
}

impl OwnedTemporary {
    pub(super) fn create(parent: &Arc<PinnedDirectory>, excluded: &OsStr) -> io::Result<Self> {
        parent.validate_destination()?;
        destination_security::check_private_file(&parent.file)?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_primitives::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        for _ in 0..128 {
            let name = OsString::from(format!(
                ".tilo-copy-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            if name == excluded {
                continue;
            }
            match cap_primitives::fs::open(&parent.file, Path::new(&name), &options) {
                Ok(file) => {
                    destination_security::check_private_file(&file)?;
                    let identity = Metadata::from_file(&file)?;
                    return Ok(Self {
                        parent: Arc::clone(parent),
                        name,
                        file,
                        identity,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(changed("cannot reserve an exclusive temporary filename"))
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        self.parent.validate_destination()?;
        destination_security::check_private_file(&self.parent.file)?;
        destination_security::check_private_file(&self.file)?;
        let metadata = metadata_nofollow(&self.parent.file, Path::new(&self.name))?;
        if !metadata.is_file()
            || metadata.is_symlink()
            || !same_directory_identity(&metadata, &self.identity)
        {
            return Err(changed("temporary destination was replaced"));
        }
        Ok(())
    }

    fn remove_owned(&self) -> io::Result<()> {
        self.validate()?;
        cap_primitives::fs::remove_file(&self.parent.file, Path::new(&self.name))
    }
}

impl Drop for OwnedTemporary {
    fn drop(&mut self) {
        // Never recursively clean a destination or unlink a replacement.
        let _ = self.remove_owned();
    }
}

pub(super) struct StagedFile<S: CopyFileSource = LocalCopySource> {
    name: OsString,
    source: S,
    temporary: OwnedTemporary,
    bytes: u64,
    digest: [u8; 32],
}
impl StagedFile<LocalCopySource> {
    fn copy(
        source_parent: &Arc<PinnedDirectory>,
        destination: &Arc<PinnedDirectory>,
        name: &OsStr,
        expected: &Metadata,
        cancelled: &AtomicBool,
    ) -> io::Result<Self> {
        check_cancel(cancelled)?;
        Self::copy_source(
            LocalCopySource::open(source_parent, name, expected)?,
            destination,
            name,
            cancelled,
        )
    }
    fn verify_and_publish(self, cancelled: &AtomicBool) -> io::Result<u64> {
        self.publish(cancelled, false).map(|receipt| receipt.bytes)
    }
}
impl<S: CopyFileSource> StagedFile<S> {
    pub(super) fn copy_source(
        mut source: S,
        destination: &Arc<PinnedDirectory>,
        name: &OsStr,
        cancelled: &AtomicBool,
    ) -> io::Result<Self> {
        check_cancel(cancelled)?;
        source.times()?;
        source.begin_pass(CopyPass::Initial)?;
        let expected = source.expected_len();
        let mut temporary = OwnedTemporary::create(destination, name)?;
        let (bytes, digest) = stream(
            &mut SourcePass(&mut source),
            Some(&mut temporary.file),
            expected,
            cancelled,
        )?;
        temporary.file.flush()?;
        temporary.file.sync_all()?;
        Ok(Self {
            name: name.to_os_string(),
            source,
            temporary,
            bytes,
            digest,
        })
    }
    pub(super) fn publish(
        mut self,
        cancelled: &AtomicBool,
        durable: bool,
    ) -> io::Result<S::Receipt> {
        check_cancel(cancelled)?;
        self.temporary.validate()?;
        self.temporary.file.seek(SeekFrom::Start(0))?;
        let destination = stream(&mut self.temporary.file, None, self.bytes, cancelled)?;
        if destination != (self.bytes, self.digest) {
            return Err(changed("destination content verification failed"));
        }
        self.source.begin_pass(CopyPass::IndependentReread)?;
        let reread = stream(
            &mut SourcePass(&mut self.source),
            None,
            self.bytes,
            cancelled,
        )?;
        if reread != (self.bytes, self.digest) {
            return Err(changed(
                "source content changed during copy; retry required",
            ));
        }
        let times = self.source.times()?;
        preserve_time_values(&self.temporary.file, times.accessed, times.modified)?;
        if durable {
            persistence_result(
                migration_durability::sync_file(&self.temporary.file),
                PersistenceBoundary::OutputUnpublished,
            )?;
        } else {
            self.temporary.file.sync_all()?;
        }
        self.source.attest(self.bytes, self.digest, cancelled)?;
        check_cancel(cancelled)?;
        self.temporary.validate()?;
        // Hard-link publication is atomic and fails if the destination exists.
        // Filesystems without hard links are explicitly unsupported, never
        // downgraded to an overwriting rename or a partially visible copy.
        cap_primitives::fs::hard_link(
            &self.temporary.parent.file,
            Path::new(&self.temporary.name),
            &self.temporary.parent.file,
            Path::new(&self.name),
        )?;
        let mut boundary = PersistenceBoundary::OutputPublicationUncertain;
        let receipt = (|| {
            let published = metadata_nofollow(&self.temporary.parent.file, Path::new(&self.name))?;
            if !published.is_file()
                || !same_directory_identity(&published, &self.temporary.identity)
            {
                return Err(changed("published destination identity changed"));
            }
            self.temporary.remove_owned()?;
            if durable {
                #[cfg(test)]
                super::migration_journal::persistence_injection::fail_at("file_publication_flush")?;
                migration_durability::sync_directory(&self.temporary.parent.file)?;
                boundary = PersistenceBoundary::OutputUnrecorded;
            }
            #[cfg(test)]
            if durable {
                super::migration_journal::crash_injection::boundary("file_published");
            }
            self.source
                .receipt(&self.temporary.file, self.bytes, self.digest, durable)
        })();
        if durable {
            persistence_result(receipt, boundary)
        } else {
            receipt
        }
    }
}

/// Bounded streaming, also used for the independent source/destination rereads.
/// The expected length bounds work even if another process keeps appending.
pub(super) fn stream(
    source: &mut impl Read,
    mut destination: Option<&mut File>,
    expected: u64,
    cancelled: &AtomicBool,
) -> io::Result<(u64, [u8; 32])> {
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    loop {
        check_cancel(cancelled)?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| changed("file length overflow"))?;
        if total > expected {
            return Err(changed("file length changed during copy"));
        }
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| changed("invalid read length"))?;
        digest.update(chunk);
        if let Some(output) = &mut destination {
            output.write_all(chunk)?;
        }
    }
    if total != expected {
        return Err(changed("file length changed during copy"));
    }
    Ok((total, digest.finalize().into()))
}

struct Copier<'a, P> {
    cancelled: &'a AtomicBool,
    progress: P,
    report: CopyReport,
    journal: Option<MigrationJournal>,
}

impl<P: FnMut(CopyProgress)> Copier<'_, P> {
    fn notify(&mut self, path: &Path) {
        (self.progress)(CopyProgress {
            verified_files: self.report.verified_files.len() as u64,
            verified_bytes: self.report.verified_bytes,
            current_relative_path: path.to_path_buf(),
        });
    }

    fn walk(
        &mut self,
        source: &Arc<PinnedDirectory>,
        destination: &Arc<PinnedDirectory>,
        relative: &Path,
        depth: usize,
    ) -> io::Result<()> {
        check_cancel(self.cancelled)?;
        source.validate()?;
        destination.validate_destination()?;
        if depth > MAX_DEPTH {
            return Err(changed("folder nesting exceeds supported depth"));
        }
        let before = Metadata::from_file(&source.file)?;
        if let Some(journal) = &mut self.journal {
            journal.enter_directory(relative, source, destination)?;
        }
        let directory = Dir::from_std_file(source.file.try_clone()?);
        let mut names = Vec::new();
        for entry in directory.entries()? {
            check_cancel(self.cancelled)?;
            match entry {
                Ok(entry) => names.push(entry.file_name()),
                Err(error) => self
                    .report
                    .failed
                    .push((relative.to_path_buf(), error.to_string())),
            }
        }
        names.sort();
        for name in names {
            let path = relative.join(&name);
            self.notify(&path);
            check_cancel(self.cancelled)?;
            source.validate()?;
            destination.validate_destination()?;
            if let Err(error) = self.copy_entry(source, destination, &name, &path, depth) {
                if let Some(journal) = &mut self.journal {
                    journal.observe_persistence_failure(&path, &error);
                }
                if self.cancelled.load(Ordering::Relaxed)
                    || self
                        .journal
                        .as_ref()
                        .is_some_and(MigrationJournal::has_failed)
                {
                    return Err(error);
                }
                self.report.failed.push((path, error.to_string()));
            }
        }
        source.validate()?;
        if !same_snapshot(&before, &Metadata::from_file(&source.file)?) {
            return Err(changed(
                "source directory changed during enumeration; retry required",
            ));
        }
        destination.validate_destination()?;
        if let Some(journal) = &self.journal {
            let (accessed, modified) = journal.directory_times(relative)?;
            preserve_directory_times_values(destination, accessed, modified)?;
        } else {
            preserve_directory_times(destination, &before)?;
        }
        if let Some(journal) = &mut self.journal {
            journal.complete_directory(relative, destination)?;
        }
        Ok(())
    }

    fn copy_entry(
        &mut self,
        source: &Arc<PinnedDirectory>,
        destination: &Arc<PinnedDirectory>,
        name: &OsStr,
        relative: &Path,
        depth: usize,
    ) -> io::Result<()> {
        if let Some(journal) = &self.journal {
            journal.validate_relative(relative)?;
        }
        let metadata = metadata_nofollow(&source.file, Path::new(name))?;
        if let Some(journal) = &self.journal {
            journal.validate_reviewed_entry(relative, &metadata)?;
        }
        if metadata.is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            self.report.skipped.push((
                relative.to_path_buf(),
                if metadata.is_symlink() {
                    "A link, which is not copied".to_string()
                } else {
                    "A special file, which is not copied".to_string()
                },
            ));
            return Ok(());
        }
        reject_windows_placeholder(&metadata)?;
        if metadata.is_dir() {
            let source_child = source.open_child(name, &metadata)?;
            let destination_child = if let Some(journal) = &mut self.journal {
                journal.directory(relative, source, destination, name, &metadata)?
            } else {
                destination.create_child(name)?
            };
            self.walk(
                &source_child,
                &destination_child,
                relative,
                depth.saturating_add(1),
            )
        } else {
            let bytes = if let Some(journal) = &mut self.journal {
                if let Some(bytes) =
                    journal.revalidate_file(relative, source, destination, name, self.cancelled)?
                {
                    bytes
                } else {
                    let staged =
                        StagedFile::copy(source, destination, name, &metadata, self.cancelled)?;
                    let receipt = staged.publish(self.cancelled, true)?;
                    let bytes = receipt.bytes;
                    journal.record_file(relative, receipt)?;
                    bytes
                }
            } else {
                let staged =
                    StagedFile::copy(source, destination, name, &metadata, self.cancelled)?;
                staged.verify_and_publish(self.cancelled)?
            };
            self.report.verified_bytes = self
                .report
                .verified_bytes
                .checked_add(bytes)
                .ok_or_else(|| changed("verified byte count overflow"))?;
            self.report.verified_files.push(relative.to_path_buf());
            self.notify(relative);
            Ok(())
        }
    }
}

/// Copy the explicitly granted source to the exact, new import directory.
///
/// `complete()` covers all regular-file bytes, directory entries and supported
/// timestamps, with other fidelity limitations disclosed separately. Progress
/// counts only published, verified files. An initial progress event occurs after
/// pinning grants but before creating the destination.
///
/// # Errors
/// Returns an error for invalid/overlapping grants, an existing destination, or
/// failure to create the fresh root. Failures after creation are per-path report
/// entries; verified outputs remain available for later explicit resumption.
pub fn copy_folder(
    source: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
    progress: impl FnMut(CopyProgress),
) -> Result<CopyReport, String> {
    copy_folder_internal(source, destination, cancelled, progress, None)
}

pub(super) fn copy_folder_internal(
    source: &Path,
    destination: &Path,
    cancelled: &AtomicBool,
    progress: impl FnMut(CopyProgress),
    journal: Option<MigrationJournal>,
) -> Result<CopyReport, String> {
    let mut copier = Copier {
        journal,
        cancelled,
        progress,
        report: CopyReport {
            metadata_limitations: vec!["Copies regular-file contents and access/modification times. Ownership, original permissions, ACLs, extended attributes, alternate streams, creation times, sparse layout and hard-link relationships are not preserved. New files use private user-owned permissions on Unix and use an explicit protected private directory ACL on Windows. Destination namespace protection excludes hostile same-account or administrator processes.".to_string()],
            ..CopyReport::default()
        },
    };
    if cancelled.load(Ordering::Relaxed) {
        copier.report.cancelled = true;
        return Ok(copier.report);
    }
    if !destination.is_absolute() {
        return Err("destination must be a native absolute path".to_string());
    }
    let name = destination
        .file_name()
        .ok_or_else(|| "destination must name a new directory".to_string())?;
    let parent = destination
        .parent()
        .ok_or_else(|| "destination has no parent".to_string())?;
    let source = PinnedDirectory::open_absolute(source)
        .map_err(|error| format!("cannot open source: {error}"))?;
    let parent = PinnedDirectory::open_absolute(parent)
        .map_err(|error| format!("cannot open destination parent: {error}"))?;
    if parent.contains_identity(&source.identity) {
        return Err("source and destination overlap".to_string());
    }
    copier.notify(Path::new(""));
    if cancelled.load(Ordering::Relaxed) {
        copier.report.cancelled = true;
        return Ok(copier.report);
    }
    source.validate().map_err(|error| error.to_string())?;
    let destination = if let Some(journal) = &mut copier.journal {
        match journal.root(&source, &parent, name) {
            Ok(destination) => destination,
            Err(_) if journal.has_failed() => {
                journal.retain_failure_in(&mut copier.report);
                return Ok(copier.report);
            }
            Err(error) => return Err(error.to_string()),
        }
    } else {
        parent
            .create_child(name)
            .map_err(|error| format!("cannot create fresh destination: {error}"))?
    };
    match copier.walk(&source, &destination, Path::new(""), 0) {
        Ok(()) => copier.report.scan_completed = true,
        Err(error) => {
            if let Some(journal) = &mut copier.journal {
                journal.observe_persistence_failure(Path::new(""), &error);
            }
            if !copier
                .journal
                .as_ref()
                .is_some_and(MigrationJournal::has_failed)
            {
                copier
                    .report
                    .failed
                    .push((PathBuf::new(), error.to_string()));
            }
        }
    }
    copier.report.cancelled = cancelled.load(Ordering::Relaxed);
    if let Some(journal) = &mut copier.journal {
        if !journal.has_failed() {
            let _ = journal.finish(&mut copier.report);
        }
        journal.retain_failure_in(&mut copier.report);
    }
    Ok(copier.report)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture() -> (TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let source = base.join("source");
        fs::create_dir(&source).unwrap();
        (temp, source, base.join("destination"))
    }

    /// Sealed sources, staged corruption, changed sources and conflicting destinations.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn publication_requires_verified_unchanged_bytes_and_preserves_conflicts() {
        // Phase `sealed_source_requires_independent_bytes_and_late_attestation`.
        // A native-like reader must get a second real pass and postflight before linking.
        {
            use super::super::migration_source::{CopyFileSource, CopyPass, CopyTimes, sealed};
            struct Source {
                pass: usize,
                offset: usize,
                mutate: bool,
                cancel: Arc<AtomicBool>,
            }
            impl sealed::Sealed for Source {}
            impl CopyFileSource for Source {
                type Receipt = (u64, [u8; 32]);
                fn expected_len(&self) -> u64 {
                    3
                }
                fn times(&self) -> io::Result<CopyTimes> {
                    Ok(CopyTimes {
                        accessed: ui::time::UNIX_EPOCH,
                        modified: ui::time::UNIX_EPOCH,
                    })
                }
                fn begin_pass(&mut self, _: CopyPass) -> io::Result<()> {
                    self.pass += 1;
                    self.offset = 0;
                    Ok(())
                }
                fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                    if self.offset == 3 {
                        return Ok(0);
                    }
                    let data = if self.mutate && self.pass == 2 {
                        b"bad"
                    } else {
                        b"abc"
                    };
                    let n = out.len().min(3 - self.offset);
                    out[..n].copy_from_slice(&data[self.offset..self.offset + n]);
                    self.offset += n;
                    Ok(n)
                }
                fn attest(&mut self, _: u64, _: [u8; 32], _: &AtomicBool) -> io::Result<()> {
                    if self.pass != 2 {
                        return Err(changed("test source missing independent pass"));
                    }
                    self.cancel.store(true, Ordering::Relaxed);
                    Ok(())
                }
                fn receipt(
                    &self,
                    _: &File,
                    bytes: u64,
                    digest: [u8; 32],
                    _: bool,
                ) -> io::Result<Self::Receipt> {
                    Ok((bytes, digest))
                }
            }
            let (_temp, source, _) = fixture();
            let parent = PinnedDirectory::open_absolute(&source).unwrap();
            let destination = parent.create_child(OsStr::new("private")).unwrap();
            let source = source.join("private");
            for mutate in [true, false] {
                let cancel = Arc::new(AtomicBool::new(false));
                let reader = Source {
                    pass: 0,
                    offset: 0,
                    mutate,
                    cancel: cancel.clone(),
                };
                let staged =
                    StagedFile::copy_source(reader, &destination, OsStr::new("native"), &cancel)
                        .unwrap();
                assert!(staged.publish(&cancel, true).is_err());
                assert!(!source.join("native").exists());
            }
        }

        // Phase `corruption_of_staged_bytes_is_not_published`.
        // Catches publication without destination digest verification.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("hello"), b"hello").unwrap();
            fs::create_dir(&destination).unwrap();
            let mut staged = stage_fixture(&source, &destination);
            staged.temporary.file.seek(SeekFrom::Start(0)).unwrap();
            staged.temporary.file.write_all(b"wrong").unwrap();
            assert!(staged.verify_and_publish(&AtomicBool::new(false)).is_err());
            assert!(!destination.join("hello").exists());
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
            assert_eq!(fs::read(source.join("hello")).unwrap(), b"hello");
        }

        // Phase `source_content_changed_with_restored_mtime_is_not_published`.
        // Catches trusting size/mtime without a second source content read.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("hello"), b"hello").unwrap();
            fs::create_dir(&destination).unwrap();
            let staged = stage_fixture(&source, &destination);
            let time = fs::metadata(source.join("hello"))
                .unwrap()
                .modified()
                .unwrap();
            fs::write(source.join("hello"), b"world").unwrap();
            fs::File::options()
                .write(true)
                .open(source.join("hello"))
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(time))
                .unwrap();
            assert!(staged.verify_and_publish(&AtomicBool::new(false)).is_err());
            assert!(!destination.join("hello").exists());
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
            assert_eq!(fs::read(source.join("hello")).unwrap(), b"world");
        }

        // Phase `publication_preserves_conflicting_destination`.
        // Catches overwriting a conflict created after exclusive staging.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("hello"), b"hello").unwrap();
            fs::create_dir(&destination).unwrap();
            let staged = stage_fixture(&source, &destination);
            fs::write(destination.join("hello"), b"sentinel").unwrap();
            assert!(staged.verify_and_publish(&AtomicBool::new(false)).is_err());
            assert_eq!(fs::read(destination.join("hello")).unwrap(), b"sentinel");
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
        }

        // Phase `replacing_a_temporary_file_preserves_the_replacement`.
        // Catches unlinking an entry we no longer own during error cleanup.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("hello"), b"hello").unwrap();
            fs::create_dir(&destination).unwrap();
            let staged = stage_fixture(&source, &destination);
            let temporary = destination.join(&staged.temporary.name);
            fs::remove_file(&temporary).unwrap();
            fs::write(&temporary, b"sentinel").unwrap();
            assert!(staged.verify_and_publish(&AtomicBool::new(false)).is_err());
            assert_eq!(fs::read(temporary).unwrap(), b"sentinel");
            assert!(!destination.join("hello").exists());
        }
    }

    /// Copying trees, refusing existing destinations, links and unsupported entries, metadata limits.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn copy_reproduces_supported_trees_and_refuses_unsupported_entries() {
        // Phase `copies_nested_files_and_empty_directories_without_changing_originals`.
        // Catches missing/corrupted copies and dropped empty directories.
        {
            let (_temp, source, destination) = fixture();
            fs::create_dir(source.join("nested")).unwrap();
            fs::create_dir(source.join("empty")).unwrap();
            fs::write(source.join("nested/hello.txt"), b"hello").unwrap();
            let mut progress = Vec::new();
            let report = copy_folder(&source, &destination, &AtomicBool::new(false), |p| {
                progress.push(p);
            })
            .unwrap();
            assert!(report.complete(), "{report:?}");
            assert_eq!(
                fs::read(destination.join("nested/hello.txt")).unwrap(),
                b"hello"
            );
            assert!(destination.join("empty").is_dir());
            assert_eq!(fs::read(source.join("nested/hello.txt")).unwrap(), b"hello");
            assert_eq!(report.verified_bytes, 5);
            assert_eq!(report.verified_files, [PathBuf::from("nested/hello.txt")]);
            assert_eq!(progress.last().unwrap().verified_bytes, 5);
        }

        // Phase `refuses_existing_destination_and_overlapping_grants`.
        // Catches overwrite/merge or recursive self-copy.
        {
            let (_temp, source, destination) = fixture();
            fs::create_dir(&destination).unwrap();
            fs::write(destination.join("sentinel"), b"keep").unwrap();
            for target in [&destination, &source, &source.join("inside")] {
                assert!(copy_folder(&source, target, &AtomicBool::new(false), |_| {}).is_err());
            }
            assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"keep");
            assert!(!source.join("inside").exists());
        }

        // Phase `refuses_symbolic_link_source`.
        #[cfg(unix)]
        {
            let (temp, source, destination) = fixture();
            fs::write(source.join("sentinel"), b"keep").unwrap();
            let link = temp.path().canonicalize().unwrap().join("link");
            std::os::unix::fs::symlink(&source, &link).unwrap();
            assert!(copy_folder(&link, &destination, &AtomicBool::new(false), |_| {}).is_err());
            assert!(!destination.exists());
            assert_eq!(fs::read(source.join("sentinel")).unwrap(), b"keep");
        }

        // Phase `unsupported_entries_are_explicit_and_never_followed`.
        // Catches silently following links or claiming a partial copy complete.
        #[cfg(unix)]
        {
            let (temp, source, destination) = fixture();
            let outside = temp.path().canonicalize().unwrap().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("sentinel"), b"keep").unwrap();
            std::os::unix::fs::symlink(&outside, source.join("link")).unwrap();
            let _socket = std::os::unix::net::UnixListener::bind(source.join("socket")).unwrap();
            let report =
                copy_folder(&source, &destination, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(!report.complete());
            assert_eq!(report.skipped.len(), 2);
            assert!(
                report
                    .skipped
                    .iter()
                    .any(|(path, _)| path == Path::new("link"))
            );
            assert!(
                report
                    .skipped
                    .iter()
                    .any(|(path, _)| path == Path::new("socket"))
            );
            assert!(!destination.join("link").exists());
            assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
        }

        // Phase `large_file_and_non_ascii_name_cross_chunk_boundaries`.
        {
            let (_temp, source, destination) = fixture();
            let bytes = vec![43; 200_003];
            fs::write(source.join("café 📁.txt"), &bytes).unwrap();
            let report =
                copy_folder(&source, &destination, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(report.complete(), "{report:?}");
            assert_eq!(report.verified_bytes, 200_003);
            assert_eq!(fs::read(destination.join("café 📁.txt")).unwrap(), bytes);
        }

        // Phase `preserves_supported_timestamps_and_discloses_other_metadata_limits`.
        // Catches loss of timestamps and accidentally copying permissive modes.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("hello"), b"hello").unwrap();
            let time = ui::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
            fs::File::options()
                .write(true)
                .open(source.join("hello"))
                .unwrap()
                .set_times(fs::FileTimes::new().set_accessed(time).set_modified(time))
                .unwrap();
            let report =
                copy_folder(&source, &destination, &AtomicBool::new(false), |_| {}).unwrap();
            assert!(report.complete(), "{report:?}");
            assert_eq!(
                fs::metadata(destination.join("hello"))
                    .unwrap()
                    .modified()
                    .unwrap(),
                time
            );
            assert_eq!(
                fs::metadata(destination.join("hello"))
                    .unwrap()
                    .accessed()
                    .unwrap(),
                time
            );
            assert!(!report.metadata_limitations.is_empty());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&destination).unwrap().permissions().mode() & 0o077,
                    0
                );
                assert_eq!(
                    fs::metadata(destination.join("hello"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o077,
                    0
                );
            }
        }
    }

    /// Cancellation before, during and after staging.
    #[test]
    fn migration_cancellation_keeps_verified_files_and_cleans_only_temps() {
        // Phase `cancellation_creates_no_destination_and_reports_incomplete`.
        // Catches work performed despite an already-cancelled grant.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("sentinel"), b"keep").unwrap();
            let report =
                copy_folder(&source, &destination, &AtomicBool::new(true), |_| {}).unwrap();
            assert!(report.cancelled);
            assert!(!report.complete());
            assert!(!destination.exists());
            assert_eq!(fs::read(source.join("sentinel")).unwrap(), b"keep");
        }

        // Phase `cancellation_retains_verified_files_and_stops_before_next_entry`.
        // Catches completing a cancelled operation or deleting verified work.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("a"), b"hello").unwrap();
            fs::write(source.join("b"), b"world").unwrap();
            let cancelled = AtomicBool::new(false);
            let report = copy_folder(&source, &destination, &cancelled, |p| {
                if p.verified_files == 1 {
                    cancelled.store(true, Ordering::Relaxed);
                }
            })
            .unwrap();
            assert!(report.cancelled);
            assert!(!report.complete());
            assert_eq!(report.verified_files, [PathBuf::from("a")]);
            assert_eq!(report.verified_bytes, 5);
            assert_eq!(fs::read(destination.join("a")).unwrap(), b"hello");
            assert!(!destination.join("b").exists());
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
        }

        // Phase `cancellation_after_staging_cleans_only_the_unpublished_temp`.
        {
            let (_temp, source, destination) = fixture();
            fs::write(source.join("hello"), b"hello").unwrap();
            fs::create_dir(&destination).unwrap();
            let staged = stage_fixture(&source, &destination);
            assert!(staged.verify_and_publish(&AtomicBool::new(true)).is_err());
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        }
    }

    /// Destination parent replacement, descendant swaps and Windows pinning.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn pinned_paths_resist_parent_and_descendant_swaps() {
        // Phase `refuses_a_destination_parent_replaced_after_pinning`.
        // Catches a check-then-open destination parent redirect.
        #[cfg(unix)]
        {
            let (temp, source, _) = fixture();
            let base = temp.path().canonicalize().unwrap();
            let parent = base.join("parent");
            fs::create_dir(&parent).unwrap();
            fs::write(source.join("hello"), b"hello").unwrap();
            let mut replaced = false;
            let result = copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {
                    if !replaced {
                        replaced = true;
                        fs::rename(&parent, base.join("old-parent")).unwrap();
                        fs::create_dir(&parent).unwrap();
                        fs::write(parent.join("sentinel"), b"keep").unwrap();
                    }
                },
            );
            assert!(replaced);
            assert!(result.is_err() || !result.unwrap().complete());
            assert!(!parent.join("copy").exists());
            assert!(!base.join("old-parent/copy").exists());
            assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"keep");
        }

        // Phase `swapped_descendant_is_never_followed_outside_grant`.
        #[cfg(unix)]
        {
            let (temp, source, destination) = fixture();
            fs::create_dir(source.join("child")).unwrap();
            fs::write(source.join("child/hello"), b"hello").unwrap();
            let outside = temp.path().canonicalize().unwrap().join("outside");
            fs::create_dir(&outside).unwrap();
            fs::write(outside.join("sentinel"), b"keep").unwrap();
            let mut swapped = false;
            let report = copy_folder(&source, &destination, &AtomicBool::new(false), |p| {
                if p.current_relative_path == Path::new("child") && !swapped {
                    swapped = true;
                    fs::rename(source.join("child"), source.join("old-child")).unwrap();
                    std::os::unix::fs::symlink(&outside, source.join("child")).unwrap();
                }
            })
            .unwrap();
            assert!(swapped);
            assert!(!report.complete());
            assert!(!destination.join("child").exists());
            assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
        }

        // Phase `pinned_destination_parent_cannot_be_renamed_on_windows`.
        #[cfg(windows)]
        {
            let (temp, source, _) = fixture();
            let base = temp.path().canonicalize().unwrap();
            let parent = base.join("parent");
            fs::create_dir(&parent).unwrap();
            fs::write(source.join("hello"), b"hello").unwrap();
            let mut attempted = false;
            let report = copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {
                    if !attempted {
                        attempted = true;
                        assert!(fs::rename(&parent, base.join("renamed")).is_err());
                    }
                },
            )
            .unwrap();
            assert!(attempted);
            assert!(report.complete(), "{report:?}");
            assert_eq!(fs::read(parent.join("copy/hello")).unwrap(), b"hello");
        }
    }

    fn stage_fixture(source: &Path, destination: &Path) -> StagedFile {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(destination, fs::Permissions::from_mode(0o700)).unwrap();
        }
        #[cfg(windows)]
        {
            // Staging requires an actually private directory, including its
            // inherited child ACL; use the same native creation as production.
            fs::remove_dir(destination).unwrap();
            let parent = PinnedDirectory::open_absolute(destination.parent().unwrap()).unwrap();
            parent
                .create_child(destination.file_name().unwrap())
                .unwrap();
        }
        let source = PinnedDirectory::open_absolute(source).unwrap();
        let destination = PinnedDirectory::open_absolute(destination).unwrap();
        let expected = metadata_nofollow(&source.file, Path::new("hello")).unwrap();
        StagedFile::copy(
            &source,
            &destination,
            OsStr::new("hello"),
            &expected,
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    /// Writable parents, ancestor rights, sticky temporaries and changed permissions.
    #[test]
    #[cfg(unix)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn namespace_permissions_gate_creation_and_publication() {
        // Phase `namespace_refuses_other_writable_parent_before_creating_anything`.
        // A private child does not prevent another account replacing it through
        // an unsafe parent. Refusal must happen before mkdir, not after adoption.
        {
            use std::os::unix::fs::PermissionsExt;
            let (temp, source, _) = fixture();
            let parent = temp.path().canonicalize().unwrap().join("public");
            fs::create_dir(&parent).unwrap();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
            fs::write(parent.join("sentinel"), b"keep").unwrap();
            let result = copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {},
            );
            assert!(result.is_err());
            assert!(!parent.join("copy").exists());
            assert_eq!(fs::read(parent.join("sentinel")).unwrap(), b"keep");
        }

        // Phase `namespace_checks_ancestor_replacement_rights`.
        {
            use std::os::unix::fs::PermissionsExt;
            let (temp, source, _) = fixture();
            let ancestor = temp.path().canonicalize().unwrap().join("public");
            fs::create_dir(&ancestor).unwrap();
            fs::create_dir(ancestor.join("private")).unwrap();
            fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o777)).unwrap();
            fs::set_permissions(ancestor.join("private"), fs::Permissions::from_mode(0o700))
                .unwrap();
            assert!(
                copy_folder(
                    &source,
                    &ancestor.join("private/copy"),
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
            );
            assert!(!ancestor.join("private/copy").exists());
        }

        // Phase `namespace_preserves_safe_sticky_temporary_paths`.
        {
            use std::os::unix::fs::PermissionsExt;
            let (temp, source, _) = fixture();
            let parent = temp.path().canonicalize().unwrap().join("sticky");
            fs::create_dir(&parent).unwrap();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o1777)).unwrap();
            fs::write(source.join("hello"), b"hello").unwrap();
            let report = copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            assert!(report.complete(), "{report:?}");
            assert_eq!(fs::read(parent.join("copy/hello")).unwrap(), b"hello");
            assert_eq!(
                fs::metadata(parent.join("copy"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }

        // Phase `namespace_changed_permissions_stop_publication_and_cleanup`.
        // Once the private namespace is opened to other writers, neither publish
        // nor cleanup is allowed to perform its name-based mutation.
        {
            use std::os::unix::fs::PermissionsExt;
            let (_temp, source, destination) = fixture();
            fs::create_dir(&destination).unwrap();
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
            fs::write(source.join("hello"), b"hello").unwrap();
            let staged = stage_fixture(&source, &destination);
            let temporary = destination.join(&staged.temporary.name);
            fs::set_permissions(&destination, fs::Permissions::from_mode(0o777)).unwrap();
            assert!(staged.verify_and_publish(&AtomicBool::new(false)).is_err());
            assert!(!destination.join("hello").exists());
            assert_eq!(fs::read(&temporary).unwrap(), b"hello");
        }
    }

    /// macOS ACL write grants, inherit-only ACLs and read-only ACLs.
    #[test]
    #[cfg(target_os = "macos")]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn namespace_macos_acls_are_honoured_before_copy() {
        // Phase `namespace_macos_acl_write_grant_is_not_hidden_by_private_mode`.
        {
            use std::os::unix::fs::PermissionsExt;
            let (temp, source, _) = fixture();
            let parent = temp.path().canonicalize().unwrap().join("acl-parent");
            fs::create_dir(&parent).unwrap();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                std::process::Command::new("chmod")
                    .args([
                        "+a",
                        "everyone allow add_file,add_subdirectory,delete_child"
                    ])
                    .arg(&parent)
                    .status()
                    .unwrap()
                    .success()
            );
            let result = copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {},
            );
            assert!(result.is_err());
            assert!(!parent.join("copy").exists());
        }

        // Phase `namespace_macos_inherit_only_write_acl_is_refused_before_mkdir`.
        {
            let (temp, source, _) = fixture();
            let parent = temp.path().canonicalize().unwrap().join("acl-parent");
            fs::create_dir(&parent).unwrap();
            assert!(std::process::Command::new("chmod").args(["+a", "everyone allow write,append,delete_child,file_inherit,directory_inherit,only_inherit"]).arg(&parent).status().unwrap().success());
            assert!(
                copy_folder(
                    &source,
                    &parent.join("copy"),
                    &AtomicBool::new(false),
                    |_| {}
                )
                .is_err()
            );
            assert!(!parent.join("copy").exists());
        }

        // Phase `namespace_macos_read_only_acl_does_not_block_safe_copy`.
        {
            let (temp, source, _) = fixture();
            let parent = temp.path().canonicalize().unwrap().join("acl-parent");
            fs::create_dir(&parent).unwrap();
            fs::write(source.join("hello"), b"hello").unwrap();
            assert!(
                std::process::Command::new("chmod")
                    .args([
                        "+a",
                        "everyone allow read,readattr,readextattr,readsecurity"
                    ])
                    .arg(&parent)
                    .status()
                    .unwrap()
                    .success()
            );
            let report = copy_folder(
                &source,
                &parent.join("copy"),
                &AtomicBool::new(false),
                |_| {},
            )
            .unwrap();
            assert!(report.complete(), "{report:?}");
            assert_eq!(fs::read(parent.join("copy/hello")).unwrap(), b"hello");
        }
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "requires root only to create a disposable foreign-owned child"]
    fn namespace_sticky_parent_does_not_trust_a_foreign_owned_child() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::PermissionsExt;
        // ownership of a newly created empty directory in its own fixture.
        assert_eq!(tilo_process::effective_uid(), 0);
        let (temp, source, _) = fixture();
        let parent = temp.path().canonicalize().unwrap().join("sticky");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o1777)).unwrap();
        let child = parent.join("foreign");
        fs::create_dir(&child).unwrap();
        let file = fs::File::open(&child).unwrap();
        // SAFETY: valid fixture-only descriptor; uid/gid65534 is untrusted by
        // a uid0 worker, independently of whether it names a local account.
        assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 65_534, 65_534) }, 0);
        assert!(
            copy_folder(
                &source,
                &child.join("copy"),
                &AtomicBool::new(false),
                |_| {}
            )
            .is_err()
        );
        assert!(!child.join("copy").exists());
    }
}
