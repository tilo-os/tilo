//! Private, retained namespace. Every destructive store operation revalidates it.
use super::super::super::{
    destination_security, migration_durability as durability, nofollow::metadata_nofollow,
};
#[cfg(not(windows))]
use super::super::super::{
    migration::{OwnedTemporary, PinnedDirectory},
    migration_store::ensure_private_root,
    nofollow::same_directory_identity,
};
use super::super::model::EndpointIdentity;
use super::{StoreError, format};
#[cfg(not(windows))]
use cap_fs_ext::{MetadataExt, OpenOptionsFollowExt};
#[cfg(not(windows))]
pub(super) use cap_primitives::fs::Metadata;
#[cfg(not(windows))]
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
#[cfg(not(windows))]
pub(super) type Directory = PinnedDirectory;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(super) use super::super::windows_native::WindowsDirectory as Directory;
use serde::{Deserialize, Serialize};
#[cfg(all(test, target_os = "macos"))]
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
#[cfg(windows)]
pub(super) use windows::{
    Metadata, file_identity, open_file, read_file, regular, same_snapshot, validate_file,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Identity {
    pub volume: [u8; 32],
    pub root: [u8; 32],
}
impl From<EndpointIdentity> for Identity {
    fn from(value: EndpointIdentity) -> Self {
        Self {
            volume: value.volume,
            root: value.root,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema: u32,
    identity: Identity,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Current {
    pub schema: u32,
    pub identity: Identity,
    pub generation: u64,
    pub root: Option<[u8; 32]>,
    pub sequence: u64,
    pub digest: [u8; 32],
    pub log: u64,
}
type ReadBudgetSlot =
    Arc<std::sync::Mutex<Option<std::sync::Weak<std::sync::Mutex<ReviewReadBudget>>>>>;
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ReviewReadStats {
    pub bytes: u64,
    pub pages: u64,
    pub rows: u64,
    pub work: u64,
}
struct ReviewReadBudget {
    used: ReviewReadStats,
    limits: ReviewReadStats,
    started: std::time::Instant,
    deadline: std::time::Instant,
}
/// Move-only finite store lease. It authorizes no event or endpoint access.
pub(in crate::connection) struct ReadLease {
    slot: ReadBudgetSlot,
    budget: Arc<std::sync::Mutex<ReviewReadBudget>>,
}
// The slot is weak: the final owning/nested lease's drop ends the budget. An
// earlier owner's drop cannot detach a still-live nested acceptance lease.
impl ReadLease {
    pub(in crate::connection) fn check(&self) -> Result<(), StoreError> {
        if !self
            .slot
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|active| Arc::ptr_eq(&active, &self.budget))
        {
            return Err(StoreError::StaleRevision);
        }
        let budget = self
            .budget
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?;
        if std::time::Instant::now() >= budget.deadline
            || budget.used.bytes > budget.limits.bytes
            || budget.used.pages > budget.limits.pages
            || budget.used.rows > budget.limits.rows
            || budget.used.work > budget.limits.work
        {
            return Err(StoreError::LimitExceeded);
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn limits_for_test(&self, limits: ReviewReadStats) {
        self.budget.lock().unwrap().limits = limits;
    }
    #[cfg(test)]
    pub(super) fn expire_for_test(&self) {
        self.budget.lock().unwrap().deadline = std::time::Instant::now();
    }
    #[cfg(test)]
    pub(super) fn stats(&self) -> ReviewReadStats {
        self.budget.lock().unwrap().used
    }
}

/// Preserve the store refusal outside io::Error: the shared framing reader may
/// treat UnexpectedEof as a torn last frame, but never a refused read allowance.
struct LogRead<'a, R> {
    storage: &'a Storage,
    reader: &'a mut R,
    refusal: Option<StoreError>,
}
impl<R: Read> Read for LogRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.refusal.is_some() {
            return Err(std::io::Error::other("bounded log read refused"));
        }
        if let Err(error) = self.storage.review_charge(buffer.len() as u64, 0, 0, 1) {
            self.refusal = Some(error);
            return Err(std::io::Error::other("bounded log read refused"));
        }
        self.reader.read(buffer)
    }
}
impl<R> LogRead<'_, R> {
    fn complete<T>(self, result: Result<T, StoreError>) -> Result<T, StoreError> {
        if let Some(error) = self.refusal {
            return Err(error);
        }
        let value = result?;
        // A final successful or EOF read may itself have crossed the deadline.
        self.storage.review_charge(0, 0, 0, 0)?;
        Ok(value)
    }
}

pub(super) struct Storage {
    read_budget: ReadBudgetSlot,
    pub root: Arc<Directory>,
    pub pages: Arc<Directory>,
    pub recovery: Arc<Directory>,
    pub quarantine: Arc<Directory>,
    pub path: PathBuf,
    pub identity: Identity,
    lock: File,
    lock_identity: Metadata,
}

fn attest(path: &Path) -> Result<Identity, StoreError> {
    #[cfg(test)]
    if super::tests::identity_unavailable() {
        return Err(StoreError::UnsupportedDurability);
    }
    #[cfg(all(test, target_os = "macos"))]
    {
        // Host source tests only: real volume UUID/inode, never runtime admission.
        let pin = PinnedDirectory::open_absolute(path)?;
        let bytes = serde_json::to_vec(&durability::identity(&pin.file)?)
            .map_err(|_| StoreError::Corrupt)?;
        Ok(Identity {
            volume: Sha256::digest(b"host-test-only").into(),
            root: Sha256::digest(bytes).into(),
        })
    }
    #[cfg(not(all(test, target_os = "macos")))]
    {
        super::super::endpoint::probe_endpoint(path)
            .map(|probe| probe.identity.into())
            .map_err(|_| StoreError::UnsupportedDurability)
    }
}
fn admitted() -> Result<(), StoreError> {
    durability::supported().map_err(|_| StoreError::UnsupportedDurability)?;
    #[cfg(not(any(target_os = "linux", all(test, target_os = "macos"))))]
    return Err(StoreError::UnsupportedDurability);
    #[cfg(any(target_os = "linux", all(test, target_os = "macos")))]
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn regular(file: &File) -> Result<Metadata, StoreError> {
    destination_security::check_private_file(file)?;
    let metadata = Metadata::from_file(file)?;
    super::super::super::migration::reject_windows_placeholder(&metadata)?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(StoreError::IdentityChanged);
    }
    Ok(metadata)
}
#[cfg(not(windows))]
pub(super) fn open_file(
    parent: &Arc<Directory>,
    name: &str,
    create: bool,
) -> Result<File, StoreError> {
    parent.validate_destination()?;
    destination_security::check_private_file(&parent.file)?;
    let before = if create {
        None
    } else {
        Some(metadata_nofollow(&parent.file, Path::new(name))?)
    };
    if before
        .as_ref()
        .is_some_and(|m| !m.is_file() || m.is_symlink() || m.nlink() != 1)
    {
        return Err(StoreError::IdentityChanged);
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(create)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_primitives::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
    let after = regular(&file)?;
    if before
        .as_ref()
        .is_some_and(|m| !same_directory_identity(m, &after))
    {
        return Err(StoreError::IdentityChanged);
    }
    validate_file(parent, name, &file, &after)?;
    Ok(file)
}
#[cfg(not(windows))]
pub(super) fn validate_file(
    parent: &Arc<Directory>,
    name: &str,
    file: &File,
    expected: &Metadata,
) -> Result<(), StoreError> {
    parent.validate_destination()?;
    let actual = regular(file)?;
    let named = metadata_nofollow(&parent.file, Path::new(name))?;
    if !named.is_file()
        || named.is_symlink()
        || named.nlink() != 1
        || !same_directory_identity(&named, expected)
        || !same_directory_identity(&actual, expected)
    {
        return Err(StoreError::IdentityChanged);
    }
    Ok(())
}
fn child(parent: &Arc<Directory>, name: &str, create: bool) -> Result<Arc<Directory>, StoreError> {
    let directory = if create {
        parent.create_child(OsStr::new(name))?
    } else {
        let m = metadata_nofollow(&parent.file, Path::new(name))?;
        parent.open_child(OsStr::new(name), &m)?
    };
    directory.validate_destination()?;
    destination_security::check_private_file(&directory.file)?;
    if create {
        sync_directory(&directory)?;
        sync_directory(parent)?;
    }
    Ok(directory)
}
impl Storage {
    #[cfg(not(windows))]
    pub fn open(path: &Path) -> Result<(Self, Current), StoreError> {
        admitted()?;
        let mut ancestor = path.to_path_buf();
        while !ancestor.try_exists()? {
            ancestor = ancestor.parent().ok_or(StoreError::Corrupt)?.to_path_buf();
        }
        let anchor = PinnedDirectory::open_absolute(&ancestor)?;
        anchor.validate_destination()?;
        let ancestor_id = attest(&ancestor)?;
        let fresh = ancestor != path;
        if fresh {
            // Validate the retained ancestor before allowing the existing helper to create.
            anchor.validate_destination()?;
            if attest(&ancestor)? != ancestor_id {
                return Err(StoreError::IdentityChanged);
            }
        }
        let root = ensure_private_root(path)?;
        anchor.validate_destination()?;
        if attest(&ancestor)? != ancestor_id {
            return Err(StoreError::IdentityChanged);
        }
        let identity = attest(path)?;
        root.validate_destination()?;
        if fresh {
            // Sync each newly published child edge, including the first ancestor edge.
            let mut directory = path.to_path_buf();
            loop {
                let pin = PinnedDirectory::open_absolute(&directory)?;
                pin.validate_destination()?;
                sync_directory(&pin)?;
                if directory == ancestor {
                    break;
                }
                directory = directory.parent().ok_or(StoreError::Corrupt)?.to_path_buf();
            }
        }
        let lock = open_file(&root, "ownership.lock", fresh)?;
        let lock_identity = regular(&lock)?;
        lock.try_lock().map_err(|_| StoreError::Busy)?;
        validate_file(&root, "ownership.lock", &lock, &lock_identity)?;
        if fresh {
            sync_file(&lock)?;
            sync_directory(&root)?;
        }
        let pages = child(&root, "pages", fresh)?;
        let recovery = child(&root, "recovery", fresh)?;
        let quarantine = child(&root, "quarantine", fresh)?;
        let storage = Self {
            read_budget: Arc::new(std::sync::Mutex::new(None)),
            root,
            pages,
            recovery,
            quarantine,
            path: path.to_path_buf(),
            identity,
            lock,
            lock_identity,
        };
        storage.validate()?;
        let current = if fresh {
            storage.initialize()?
        } else {
            let header: Header =
                serde_json::from_slice(&storage.read_bytes(&storage.root, "store-header", 4096)?)
                    .map_err(|_| StoreError::Corrupt)?;
            if header.schema != 1 || header.identity != storage.identity {
                return Err(StoreError::IdentityChanged);
            }
            let current: Current =
                serde_json::from_slice(&storage.read_bytes(&storage.root, "CURRENT", 4096)?)
                    .map_err(|_| StoreError::Corrupt)?;
            if current.schema != 1 || current.identity != storage.identity {
                return Err(StoreError::IdentityChanged);
            }
            current
        };
        Ok((storage, current))
    }
    fn initialize(&self) -> Result<Current, StoreError> {
        let header = Header {
            schema: 1,
            identity: self.identity.clone(),
        };
        self.create_bytes(&self.root, "store-header", &format::encode(&header)?)?;
        let current = Current {
            schema: 1,
            identity: self.identity.clone(),
            generation: 0,
            root: None,
            sequence: 0,
            digest: [0; 32],
            log: 0,
        };
        let mut log = open_file(&self.root, "log-0", true)?;
        log.write_all(b"QOSCON1\n")?;
        let header = format::LogHeader {
            schema: 1,
            identity: self.identity.clone(),
            generation: 0,
            first_sequence: 1,
            predecessor: [0; 32],
        };
        format::encode(&header)?;
        super::super::super::migration_journal_log::write_record(&mut log, &mut 0, &header)?;
        sync_file(&log)?;
        sync_directory(&self.root)?;
        self.create_bytes(&self.root, "CURRENT", &format::encode(&current)?)?;
        Ok(current)
    }
    pub fn validate(&self) -> Result<(), StoreError> {
        self.review_charge(0, 0, 0, 1)?;
        #[cfg(windows)]
        {
            destination_security::require_private_protected(&self.root.file)?;
            self.root.validate_authority()?;
        }
        self.root
            .validate_destination()
            .map_err(|_| StoreError::IdentityChanged)?;
        if attest(&self.path)? != self.identity {
            return Err(StoreError::IdentityChanged);
        }
        validate_file(
            &self.root,
            "ownership.lock",
            &self.lock,
            &self.lock_identity,
        )?;
        Ok(())
    }
    pub(super) fn read_lease(&self, consent: bool) -> Result<ReadLease, StoreError> {
        let mut slot = self
            .read_budget
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?;
        let budget = if let Some(active) = slot.as_ref().and_then(std::sync::Weak::upgrade) {
            active
        } else {
            let (limits, seconds) = if consent {
                (
                    ReviewReadStats {
                        bytes: 512 * 1024 * 1024,
                        pages: 65_536,
                        rows: 1_048_576,
                        work: 4_194_304,
                    },
                    30,
                )
            } else {
                (
                    ReviewReadStats {
                        bytes: 1024 * 1024 * 1024,
                        pages: 131_072,
                        rows: 4_456_448,
                        work: 8_388_608,
                    },
                    60,
                )
            };
            let started = std::time::Instant::now();
            let budget = Arc::new(std::sync::Mutex::new(ReviewReadBudget {
                used: ReviewReadStats::default(),
                limits,
                started,
                deadline: started
                    .checked_add(std::time::Duration::from_secs(seconds))
                    .ok_or(StoreError::LimitExceeded)?,
            }));
            *slot = Some(Arc::downgrade(&budget));
            budget
        };
        drop(slot);
        if consent {
            // A nested acceptance tightens the existing lease; it cannot reset
            // counters or borrow the larger replay allowance/deadline.
            let mut active = budget.lock().map_err(|_| StoreError::RecoveryRequired)?;
            active.limits.bytes = active.limits.bytes.min(512 * 1024 * 1024);
            active.limits.pages = active.limits.pages.min(65_536);
            active.limits.rows = active.limits.rows.min(1_048_576);
            active.limits.work = active.limits.work.min(4_194_304);
            active.deadline = active.deadline.min(
                active
                    .started
                    .checked_add(std::time::Duration::from_secs(30))
                    .ok_or(StoreError::LimitExceeded)?,
            );
        }
        let lease = ReadLease {
            slot: Arc::clone(&self.read_budget),
            budget,
        };
        lease.check()?;
        Ok(lease)
    }
    pub(super) fn review_charge(
        &self,
        bytes: u64,
        pages: u64,
        rows: u64,
        work: u64,
    ) -> Result<(), StoreError> {
        let active = self
            .read_budget
            .lock()
            .map_err(|_| StoreError::RecoveryRequired)?
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        if let Some(active) = active {
            let mut b = active.lock().map_err(|_| StoreError::RecoveryRequired)?;
            if std::time::Instant::now() >= b.deadline {
                return Err(StoreError::LimitExceeded);
            }
            let add = |used: u64, amount: u64, max: u64| {
                used.checked_add(amount)
                    .filter(|n| *n <= max)
                    .ok_or(StoreError::LimitExceeded)
            };
            b.used = ReviewReadStats {
                bytes: add(b.used.bytes, bytes, b.limits.bytes)?,
                pages: add(b.used.pages, pages, b.limits.pages)?,
                rows: add(b.used.rows, rows, b.limits.rows)?,
                work: add(b.used.work, work, b.limits.work)?,
            };
        }
        Ok(())
    }
    pub(super) fn read_log_record<T: serde::de::DeserializeOwned>(
        &self,
        reader: &mut impl Read,
        sequence: u64,
    ) -> Result<Option<T>, StoreError> {
        let mut metered = LogRead {
            storage: self,
            reader,
            refusal: None,
        };
        let result =
            super::framing::read_record(&mut metered, sequence).map_err(|_| StoreError::Corrupt);
        metered.complete(result)
    }
    pub(super) fn read_log_magic(&self, reader: &mut impl Read) -> Result<(), StoreError> {
        let mut metered = LogRead {
            storage: self,
            reader,
            refusal: None,
        };
        let mut magic = [0; 8];
        let result = metered.read_exact(&mut magic).map_err(StoreError::from);
        metered.complete(result)?;
        if &magic != b"QOSCON1\n" {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
    pub fn read_bytes(
        &self,
        parent: &Arc<Directory>,
        name: &str,
        max: u64,
    ) -> Result<Vec<u8>, StoreError> {
        self.validate()?;
        let mut file = read_file(parent, name)?;
        let before = regular(&file)?;
        if before.len() > max {
            return Err(StoreError::LimitExceeded);
        }
        let bytes = read_metered(&mut file, max, &|requested| {
            self.review_charge(requested as u64, 0, 0, 0)
        })?;
        if bytes.len() as u64 > max {
            return Err(StoreError::LimitExceeded);
        }
        validate_file(parent, name, &file, &before)?;
        if !same_snapshot(&before, &regular(&file)?) {
            return Err(StoreError::IdentityChanged);
        }
        Ok(bytes)
    }
    pub fn create_bytes(
        &self,
        parent: &Arc<Directory>,
        name: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.validate()?;
        let mut file = open_file(parent, name, true)?;
        let identity = regular(&file)?;
        file.write_all(bytes)?;
        sync_file(&file)?;
        self.validate()?;
        validate_file(parent, name, &file, &identity)?;
        sync_directory(parent)?;
        Ok(())
    }
    pub(super) fn validate_current(
        &self,
        expected: &Current,
        identity: &Metadata,
    ) -> Result<(), StoreError> {
        let pointer = read_file(&self.root, "CURRENT")?;
        validate_file(&self.root, "CURRENT", &pointer, identity)?;
        if self.read_bytes(&self.root, "CURRENT", 4096)? != format::encode(expected)? {
            return Err(StoreError::IdentityChanged);
        }
        Ok(())
    }
}

impl super::ConnectionStore {
    // Kept on the locked store so callers cannot supply substitute evidence for
    // its retained CURRENT and committed active-log snapshot.
    #[cfg(not(windows))]
    pub(super) fn publish_current(
        &self,
        next: &Current,
        next_log: &File,
        next_log_identity: &Metadata,
    ) -> Result<Metadata, StoreError> {
        self.validate()?;
        let mut temporary = OwnedTemporary::create(&self.storage.root, OsStr::new("CURRENT"))?;
        temporary.file.write_all(&format::encode(next)?)?;
        let identity = regular(&temporary.file)?;
        sync_file(&temporary.file)?;
        #[cfg(test)]
        {
            super::tests::fault()?;
            super::tests::checkpoint_boundary(super::tests::CheckpointBoundary::Prepared)?;
        }
        temporary.validate()?;
        if !same_snapshot(&identity, &regular(&temporary.file)?) {
            return Err(StoreError::IdentityChanged);
        }
        self.validate_next_log(next, next_log, next_log_identity)?;
        // This must follow every potentially blocking preparation stage. The
        // old pointer and old log still define the only committed generation.
        self.validate()?;
        cap_primitives::fs::rename(
            &self.storage.root.file,
            Path::new(&temporary.name),
            &self.storage.root.file,
            Path::new("CURRENT"),
        )?;
        #[cfg(test)]
        super::tests::fault()?;
        sync_directory(&self.storage.root)?;
        #[cfg(test)]
        super::tests::checkpoint_boundary(super::tests::CheckpointBoundary::Published)?;
        // Keep old dependencies and poison on uncertainty after replacement too.
        self.storage.validate()?;
        self.storage.validate_current(next, &identity)?;
        self.validate_next_log(next, next_log, next_log_identity)?;
        self.validate_log()?;
        Ok(identity)
    }
    fn validate_next_log(
        &self,
        next: &Current,
        log: &File,
        identity: &Metadata,
    ) -> Result<(), StoreError> {
        validate_file(
            &self.storage.root,
            &format!("log-{}", next.log),
            log,
            identity,
        )?;
        if !same_snapshot(identity, &regular(log)?) {
            return Err(StoreError::IdentityChanged);
        }
        Ok(())
    }
}

pub(super) const NATIVE_PAGE_REQUEST: usize = 4096;
pub(super) const NATIVE_PAGE_CAP: u64 = format::MAX_BYTES as u64 + 1 + NATIVE_PAGE_REQUEST as u64;
pub(super) fn read_metered(
    reader: &mut impl Read,
    max: u64,
    before_read: &dyn Fn(usize) -> Result<(), StoreError>,
) -> Result<Vec<u8>, StoreError> {
    let mut bytes = Vec::new();
    let mut requested = 0_u64;
    let limit = max.checked_add(1).ok_or(StoreError::LimitExceeded)?;
    let mut buffer = [0_u8; NATIVE_PAGE_REQUEST];
    loop {
        let remaining = limit
            .checked_sub(u64::try_from(bytes.len()).map_err(|_| StoreError::LimitExceeded)?)
            .ok_or(StoreError::LimitExceeded)?;
        let count = usize::try_from(remaining.min(NATIVE_PAGE_REQUEST as u64))
            .map_err(|_| StoreError::LimitExceeded)?;
        if count == 0 {
            return Err(StoreError::LimitExceeded);
        }
        requested = requested
            .checked_add(count as u64)
            .filter(|total| *total <= NATIVE_PAGE_CAP)
            .ok_or(StoreError::LimitExceeded)?;
        before_read(count)?;
        let target = buffer.get_mut(..count).ok_or(StoreError::LimitExceeded)?;
        match reader.read(target) {
            Ok(0) => return Ok(bytes),
            Ok(read) => {
                bytes.extend_from_slice(target.get(..read).ok_or(StoreError::Corrupt)?);
                if bytes.len() as u64 > max {
                    return Err(StoreError::LimitExceeded);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(error.into()),
        }
    }
}

impl Storage {
    pub fn page_metered(
        &self,
        hash: [u8; 32],
        before_read: &dyn Fn(usize) -> Result<(), StoreError>,
    ) -> Result<format::PageNode, StoreError> {
        self.review_charge(0, 1, 0, 1)?;
        let bytes = self.read_bytes_metered(
            &self.pages,
            &format::hex(&hash),
            format::MAX_BYTES as u64,
            before_read,
        )?;
        if format::digest(&bytes) != hash {
            return Err(StoreError::Corrupt);
        }
        let page: format::PageNode =
            serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt)?;
        self.review_charge(
            0,
            0,
            match &page.node {
                format::Node::Leaf(rows) => rows.len() as u64,
                _ => 0,
            },
            match &page.node {
                format::Node::Leaf(rows) => rows.len() as u64,
                format::Node::Index(children) => children.len() as u64,
            },
        )?;
        page.reference(hash)?;
        Ok(page)
    }
    pub fn read_bytes_metered(
        &self,
        parent: &Arc<Directory>,
        name: &str,
        max: u64,
        before_read: &dyn Fn(usize) -> Result<(), StoreError>,
    ) -> Result<Vec<u8>, StoreError> {
        self.validate()?;
        let mut file = read_file(parent, name)?;
        let before = regular(&file)?;
        if before.len() > max {
            return Err(StoreError::LimitExceeded);
        }
        let bytes = read_metered(&mut file, max, &|requested| {
            self.review_charge(requested as u64, 0, 0, 0)?;
            before_read(requested)
        })?;
        validate_file(parent, name, &file, &before)?;
        if !same_snapshot(&before, &regular(&file)?) {
            return Err(StoreError::IdentityChanged);
        }
        Ok(bytes)
    }
    pub fn page(&self, hash: [u8; 32]) -> Result<format::PageNode, StoreError> {
        self.review_charge(0, 1, 0, 1)?;
        let bytes = self.read_bytes(&self.pages, &format::hex(&hash), format::MAX_BYTES as u64)?;
        if format::digest(&bytes) != hash {
            return Err(StoreError::Corrupt);
        }
        let page: format::PageNode =
            serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt)?;
        self.review_charge(
            0,
            0,
            match &page.node {
                format::Node::Leaf(rows) => rows.len() as u64,
                _ => 0,
            },
            match &page.node {
                format::Node::Leaf(rows) => rows.len() as u64,
                format::Node::Index(children) => children.len() as u64,
            },
        )?;
        page.reference(hash)?;
        Ok(page)
    }
    pub fn save_page_with(
        &self,
        page: &format::PageNode,
        meter: Option<&dyn Fn(usize) -> Result<(), StoreError>>,
    ) -> Result<format::Child, StoreError> {
        self.review_charge(0, 0, 0, 1)?;
        let bytes = format::encode(page)?;
        let hash = format::digest(&bytes);
        let name = format::hex(&hash);
        let reference = page.reference(hash)?;
        match self.create_bytes(&self.pages, &name, &bytes) {
            Ok(()) => (),
            Err(StoreError::Persistence(error))
                if error.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                let actual = if let Some(meter) = meter {
                    self.read_bytes_metered(&self.pages, &name, format::MAX_BYTES as u64, meter)?
                } else {
                    self.read_bytes(&self.pages, &name, format::MAX_BYTES as u64)?
                };
                if actual != bytes {
                    return Err(StoreError::Corrupt);
                }
                #[cfg(not(windows))]
                {
                    let file = open_file(&self.pages, &name, false)?;
                    sync_file(&file)?;
                }
                sync_directory(&self.pages)?;
            }
            Err(error) => return Err(error),
        }
        Ok(reference)
    }
    pub fn find(
        &self,
        root: Option<[u8; 32]>,
        key: &[u8],
    ) -> Result<Option<format::Row>, StoreError> {
        let Some(mut hash) = root else {
            return Ok(None);
        };
        let mut expected = None;
        for _ in 0..8 {
            let page = self.page(hash)?;
            if expected
                .as_ref()
                .is_some_and(|e| page.reference(hash).as_ref().ok() != Some(e))
            {
                return Err(StoreError::Corrupt);
            }
            match page.node {
                format::Node::Leaf(rows) => {
                    return Ok(rows.into_iter().find(|row| row.key() == key));
                }
                format::Node::Index(children) => {
                    let Some(child) = children.into_iter().find(|child| {
                        child.first.as_slice() <= key && key <= child.last.as_slice()
                    }) else {
                        return Ok(None);
                    };
                    hash = child.hash;
                    expected = Some(child);
                }
            }
        }
        Err(StoreError::LimitExceeded)
    }
}
pub(super) struct TreeIter<'a> {
    storage: &'a Storage,
    stack: Vec<format::Child>,
    rows: std::vec::IntoIter<format::Row>,
    failed: bool,
    visited: u64,
    page_limit: u64,
    meter: Option<&'a dyn Fn(usize) -> Result<(), StoreError>>,
}
impl<'a> TreeIter<'a> {
    pub fn new(storage: &'a Storage, root: Option<[u8; 32]>) -> Result<Self, StoreError> {
        let mut stack = Vec::new();
        if let Some(hash) = root {
            stack.push(storage.page(hash)?.reference(hash)?);
        }
        Ok(Self {
            storage,
            stack,
            rows: Vec::new().into_iter(),
            failed: false,
            visited: 0,
            page_limit: 2_097_152,
            meter: None,
        })
    }
    pub(super) fn new_review(
        storage: &'a Storage,
        root: [u8; 32],
        count: u32,
    ) -> Result<Self, StoreError> {
        if count == 0 || count > 4096 {
            return Err(StoreError::LimitExceeded);
        }
        let mut iter = Self::new(storage, Some(root))?;
        iter.page_limit = u64::from(count)
            .checked_mul(8)
            .and_then(|n| n.checked_add(1))
            .ok_or(StoreError::LimitExceeded)?;
        Ok(iter)
    }
    pub fn new_metered_baseline(
        storage: &'a Storage,
        root: Option<[u8; 32]>,
        count: u32,
        meter: &'a dyn Fn(usize) -> Result<(), StoreError>,
    ) -> Result<Self, StoreError> {
        if count > 4096 || (count == 0) != root.is_none() {
            return Err(StoreError::LimitExceeded);
        }
        let mut stack = Vec::new();
        let mut visited = 0;
        if let Some(hash) = root {
            let child = storage.page_metered(hash, meter)?.reference(hash)?;
            if child.count != u64::from(count) {
                return Err(StoreError::Corrupt);
            }
            stack.push(child);
            visited = 1;
        }
        Ok(Self {
            storage,
            stack,
            rows: Vec::new().into_iter(),
            failed: false,
            visited,
            page_limit: if count == 0 {
                0
            } else {
                u64::from(count) * 8 + 1
            },
            meter: Some(meter),
        })
    }
}
impl Iterator for TreeIter<'_> {
    type Item = Result<format::Row, StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        loop {
            if let Some(row) = self.rows.next() {
                return Some(self.storage.review_charge(0, 0, 1, 1).map(|()| row));
            }
            let child = self.stack.pop()?;
            self.visited = self.visited.saturating_add(1);
            let result = (|| {
                if self.visited > self.page_limit {
                    return Err(StoreError::LimitExceeded);
                }
                let page = if let Some(meter) = self.meter {
                    self.storage.page_metered(child.hash, meter)?
                } else {
                    self.storage.page(child.hash)?
                };
                if page.reference(child.hash)? != child {
                    return Err(StoreError::Corrupt);
                }
                match page.node {
                    format::Node::Leaf(rows) => self.rows = rows.into_iter(),
                    format::Node::Index(children) => self.stack.extend(children.into_iter().rev()),
                }
                Ok(())
            })();
            if let Err(error) = result {
                self.failed = true;
                return Some(Err(error));
            }
        }
    }
}
pub(super) fn build_pages(
    storage: &Storage,
    rows: impl Iterator<Item = Result<format::Row, StoreError>>,
) -> Result<Option<[u8; 32]>, StoreError> {
    build_pages_with(storage, rows, None)
}
pub(super) fn build_pages_with(
    storage: &Storage,
    rows: impl Iterator<Item = Result<format::Row, StoreError>>,
    meter: Option<&dyn Fn(usize) -> Result<(), StoreError>>,
) -> Result<Option<[u8; 32]>, StoreError> {
    fn feed(
        storage: &Storage,
        levels: &mut [Vec<format::Child>],
        child: format::Child,
        meter: Option<&dyn Fn(usize) -> Result<(), StoreError>>,
    ) -> Result<(), StoreError> {
        let index = usize::from(child.level);
        let level = levels.get_mut(index).ok_or(StoreError::LimitExceeded)?;
        level.push(child);
        if level.len() == 32 {
            let page = format::PageNode {
                schema: 1,
                node: format::Node::Index(std::mem::take(level)),
            };
            let parent = storage.save_page_with(&page, meter)?;
            feed(storage, levels, parent, meter)?;
        }
        Ok(())
    }
    let mut levels: Vec<Vec<format::Child>> = (0..8).map(|_| Vec::new()).collect();
    let mut leaf = Vec::new();
    let mut previous = None;
    for result in rows {
        let row = result?;
        storage.review_charge(0, 0, 1, 1)?;
        let key = row.key();
        if previous.as_ref().is_some_and(|p| p >= &key) {
            return Err(StoreError::Corrupt);
        }
        previous = Some(key);
        leaf.push(row);
        let page = format::PageNode {
            schema: 1,
            node: format::Node::Leaf(leaf.clone()),
        };
        if leaf.len() > 128 || format::encode(&page).is_err() {
            let last = leaf.pop().ok_or(StoreError::Corrupt)?;
            let page = format::PageNode {
                schema: 1,
                node: format::Node::Leaf(std::mem::take(&mut leaf)),
            };
            feed(
                storage,
                &mut levels,
                storage.save_page_with(&page, meter)?,
                meter,
            )?;
            leaf.push(last);
        }
    }
    if !leaf.is_empty() {
        feed(
            storage,
            &mut levels,
            storage.save_page_with(
                &format::PageNode {
                    schema: 1,
                    node: format::Node::Leaf(leaf),
                },
                meter,
            )?,
            meter,
        )?;
    }
    for level in 0..8 {
        let children = std::mem::take(levels.get_mut(level).ok_or(StoreError::LimitExceeded)?);
        if children.is_empty() {
            continue;
        }
        if children.len() == 1 && levels.iter().all(Vec::is_empty) {
            return Ok(children.first().map(|child| child.hash));
        }
        feed(
            storage,
            &mut levels,
            storage.save_page_with(
                &format::PageNode {
                    schema: 1,
                    node: format::Node::Index(children),
                },
                meter,
            )?,
            meter,
        )?;
    }
    if levels.iter().all(Vec::is_empty) {
        Ok(None)
    } else {
        Err(StoreError::LimitExceeded)
    }
}

// Fault hooks run only in source tests, after a real host durability operation.
pub(super) fn sync_file(file: &File) -> Result<(), StoreError> {
    #[cfg(not(windows))]
    durability::sync_file(file)?;
    #[cfg(windows)]
    super::super::windows_native::flush_file(file)?;
    #[cfg(test)]
    super::tests::fault()?;
    Ok(())
}
pub(super) fn sync_directory(directory: &Arc<Directory>) -> Result<(), StoreError> {
    #[cfg(not(windows))]
    durability::sync_directory(&directory.file)?;
    #[cfg(windows)]
    directory.sync()?;
    #[cfg(test)]
    super::tests::fault()?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "GcWire", into = "GcWire")]
struct GcIntent {
    schema: u32,
    identity: Identity,
    generation: u64,
    sequence: u64,
    page: bool,
    name: String,
    file_id: format::FileIdentity,
    length: u64,
    digest: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GcWire {
    schema: u32,
    identity: Identity,
    generation: u64,
    sequence: u64,
    page: bool,
    name: String,
    #[serde(
        default,
        deserialize_with = "format::present",
        skip_serializing_if = "Option::is_none"
    )]
    inode: Option<u64>,
    #[serde(
        default,
        deserialize_with = "format::present",
        skip_serializing_if = "Option::is_none"
    )]
    windows_file_id_v1: Option<format::WindowsFileId>,
    length: u64,
    digest: [u8; 32],
}
impl TryFrom<GcWire> for GcIntent {
    type Error = &'static str;
    fn try_from(w: GcWire) -> Result<Self, Self::Error> {
        let file_id = match (w.inode, w.windows_file_id_v1) {
            (Some(inode), None) => format::FileIdentity::Unix(inode),
            (None, Some(id)) => format::FileIdentity::Windows(id),
            _ => return Err("exactly one native identity required"),
        };
        file_id
            .require_platform()
            .map_err(|_| "wrong native platform")?;
        Ok(Self {
            schema: w.schema,
            identity: w.identity,
            generation: w.generation,
            sequence: w.sequence,
            page: w.page,
            name: w.name,
            file_id,
            length: w.length,
            digest: w.digest,
        })
    }
}
impl From<GcIntent> for GcWire {
    fn from(w: GcIntent) -> Self {
        let (inode, windows_file_id_v1) = match w.file_id {
            format::FileIdentity::Unix(n) => (Some(n), None),
            format::FileIdentity::Windows(id) => (None, Some(id)),
        };
        Self {
            schema: w.schema,
            identity: w.identity,
            generation: w.generation,
            sequence: w.sequence,
            page: w.page,
            name: w.name,
            inode,
            windows_file_id_v1,
            length: w.length,
            digest: w.digest,
        }
    }
}
pub(super) struct GcScan {
    pages: cap_std::fs::ReadDir,
    logs: cap_std::fs::ReadDir,
    phase: u8,
}
fn component(name: &str) -> Result<std::ffi::CString, StoreError> {
    if name.is_empty()
        || name.len() > 80
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(StoreError::Corrupt);
    }
    std::ffi::CString::new(name).map_err(|_| StoreError::Corrupt)
}
/// Atomically capture whatever occupies the source name without clobbering a
/// quarantine occupant. Identity is checked on the captured object afterwards.
#[cfg(not(windows))]
pub(super) fn capture_no_replace(
    parent: &Arc<Directory>,
    name: &str,
    quarantine: &Arc<Directory>,
) -> Result<(), StoreError> {
    parent.validate_destination()?;
    quarantine.validate_destination()?;
    destination_security::check_private_file(&parent.file)?;
    destination_security::check_private_file(&quarantine.file)?;
    let from = component(name)?;
    let to = component("captured")?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        #[cfg(test)]
        super::tests::before_capture(parent, name)?;
        #[cfg(target_os = "linux")]
        // SAFETY: retained live directory descriptors and validated single C names.
        let result = unsafe {
            libc::renameat2(
                parent.file.as_raw_fd(),
                from.as_ptr(),
                quarantine.file.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        // SAFETY: same descriptor/name contract; EXCL never replaces the destination.
        let result = unsafe {
            libc::renameatx_np(
                parent.file.as_raw_fd(),
                from.as_ptr(),
                quarantine.file.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result != 0 {
            return Err(StoreError::Persistence(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (from, to);
        Err(StoreError::UnsupportedDurability)
    }
}
fn file_digest(
    file: &mut File,
    max: u64,
    before_read: &dyn Fn(usize) -> Result<(), StoreError>,
) -> Result<([u8; 32], u64), StoreError> {
    use sha2::{Digest, Sha256};
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    loop {
        before_read(buffer.len())?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .ok_or(StoreError::LimitExceeded)?;
        if length > max {
            return Err(StoreError::LimitExceeded);
        }
        hash.update(buffer.get(..count).ok_or(StoreError::Corrupt)?);
    }
    Ok((hash.finalize().into(), length))
}
fn child_fingerprint(child: &format::Child) -> Result<[u8; 32], StoreError> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(child.hash);
    hash.update(child.count.to_be_bytes());
    hash.update([child.level]);
    for boundary in [&child.first, &child.last] {
        hash.update(
            u64::try_from(boundary.len())
                .map_err(|_| StoreError::LimitExceeded)?
                .to_be_bytes(),
        );
        hash.update(boundary);
    }
    Ok(hash.finalize().into())
}
fn parse_hash(name: &str) -> Option<[u8; 32]> {
    if name.len() != 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut hash = [0; 32];
    for (out, pair) in hash.iter_mut().zip(name.as_bytes().chunks_exact(2)) {
        let text = std::str::from_utf8(pair).ok()?;
        *out = u8::from_str_radix(text, 16).ok()?;
    }
    Some(hash)
}
impl Storage {
    // Every incoming Child is compared before cached membership can skip a
    // descent. This preserves range/count validation for duplicate references
    // without retaining their unbounded keys in memory.
    fn collect_reachable_child(
        &self,
        child: format::Child,
        baselines: bool,
        visited_root: &mut u64,
        visited_total: &mut u64,
        seen: &mut std::collections::BTreeMap<[u8; 32], ([u8; 32], Option<bool>)>,
        reachable: &mut std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<(), StoreError> {
        *visited_root = visited_root
            .checked_add(1)
            .ok_or(StoreError::LimitExceeded)?;
        if *visited_root > 2_097_152 {
            return Err(StoreError::LimitExceeded);
        }
        *visited_total = visited_total
            .checked_add(1)
            .filter(|count| *count <= 2_097_152)
            .ok_or(StoreError::LimitExceeded)?;
        let fingerprint = child_fingerprint(&child)?;
        let upgrade = match seen.get(&child.hash) {
            Some((canonical, _)) if *canonical != fingerprint => {
                return Err(StoreError::Corrupt);
            }
            Some((_, None)) => return Err(StoreError::Corrupt),
            Some((_, Some(true))) => return Ok(()),
            Some((_, Some(false))) if !baselines => return Ok(()),
            Some((_, Some(false))) => true,
            None => {
                seen.insert(child.hash, (fingerprint, None));
                false
            }
        };
        if upgrade {
            let Some((_, complete)) = seen.get_mut(&child.hash) else {
                return Err(StoreError::Corrupt);
            };
            *complete = None;
        }
        let page = self.page(child.hash)?;
        if page.reference(child.hash)? != child {
            return Err(StoreError::Corrupt);
        }
        reachable.insert(child.hash);
        match page.node {
            format::Node::Index(children) => {
                for child in children {
                    self.collect_reachable_child(
                        child,
                        baselines,
                        visited_root,
                        visited_total,
                        seen,
                        reachable,
                    )?;
                }
            }
            format::Node::Leaf(rows) if baselines => {
                for row in rows {
                    let root = match row {
                        format::Row::Connection(connection) => connection.baseline_root,
                        format::Row::InitialChainReview(review) => Some(review.root),
                        _ => None,
                    };
                    self.collect_reachable_root(root, false, visited_total, seen, reachable)?;
                }
            }
            format::Node::Leaf(_) => (),
        }
        let Some((_, complete)) = seen.get_mut(&child.hash) else {
            return Err(StoreError::Corrupt);
        };
        *complete = Some(baselines);
        Ok(())
    }
    fn collect_reachable_root(
        &self,
        root: Option<[u8; 32]>,
        baselines: bool,
        visited_total: &mut u64,
        seen: &mut std::collections::BTreeMap<[u8; 32], ([u8; 32], Option<bool>)>,
        reachable: &mut std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<(), StoreError> {
        let Some(hash) = root else { return Ok(()) };
        let child = self.page(hash)?.reference(hash)?;
        let mut visited_root = 0_u64;
        self.collect_reachable_child(
            child,
            baselines,
            &mut visited_root,
            visited_total,
            seen,
            reachable,
        )
    }
    fn contains_page(
        &self,
        root: Option<[u8; 32]>,
        wanted: [u8; 32],
        baselines: bool,
    ) -> Result<bool, StoreError> {
        let Some(hash) = root else { return Ok(false) };
        let mut stack = vec![self.page(hash)?.reference(hash)?];
        let mut visited = 0_u64;
        while let Some(child) = stack.pop() {
            visited = visited.checked_add(1).ok_or(StoreError::LimitExceeded)?;
            if visited > 2_097_152 {
                return Err(StoreError::LimitExceeded);
            }
            let page = self.page(child.hash)?;
            if page.reference(child.hash)? != child {
                return Err(StoreError::Corrupt);
            }
            if child.hash == wanted {
                return Ok(true);
            }
            match page.node {
                format::Node::Index(children) => stack.extend(children.into_iter().rev()),
                format::Node::Leaf(rows) if baselines => {
                    for row in rows {
                        let root = match row {
                            format::Row::Connection(connection) => connection.baseline_root,
                            format::Row::InitialChainReview(review) => Some(review.root),
                            _ => None,
                        };
                        if self.contains_page(root, wanted, false)? {
                            return Ok(true);
                        }
                    }
                }
                format::Node::Leaf(_) => (),
            }
        }
        Ok(false)
    }
}
impl super::ConnectionStore {
    pub(super) fn gc_reachable_pages(
        &self,
    ) -> Result<std::collections::BTreeSet<[u8; 32]>, StoreError> {
        use std::io::{Seek, SeekFrom};
        let mut reachable = std::collections::BTreeSet::new();
        let mut seen = std::collections::BTreeMap::new();
        let mut visited = 0_u64;
        self.storage.collect_reachable_root(
            self.current.root,
            true,
            &mut visited,
            &mut seen,
            &mut reachable,
        )?;
        self.storage.collect_reachable_root(
            self.candidate_pin,
            false,
            &mut visited,
            &mut seen,
            &mut reachable,
        )?;
        // Earlier candidate roots in this active log remain replay dependencies,
        // even if a later baseline event superseded the connection's pointer.
        let mut log = reread_file(&self.storage.root, &format!("log-{}", self.current.log))?;
        let length = log.metadata()?.len();
        if length > 16 * 1024 * 1024 {
            return Err(StoreError::LimitExceeded);
        }
        log.seek(SeekFrom::Start(8))?;
        let _: format::LogHeader = self
            .storage
            .read_log_record(&mut log, 0)?
            .ok_or(StoreError::Corrupt)?;
        let mut sequence = format::next(self.current.sequence)?;
        while let Some(event) = self
            .storage
            .read_log_record::<format::Event>(&mut log, sequence)?
        {
            let root = match event.change {
                format::Change::BaselineCommit { root, .. } => root,
                format::Change::InitialReviewAccepted { review, .. } => Some(review.root),
                _ => None,
            };
            self.storage.collect_reachable_root(
                root,
                false,
                &mut visited,
                &mut seen,
                &mut reachable,
            )?;
            sequence = format::next(sequence)?;
        }
        Ok(reachable)
    }
    pub(super) fn page_referenced(&self, wanted: [u8; 32]) -> Result<bool, StoreError> {
        use std::io::{Seek, SeekFrom};
        if self
            .storage
            .contains_page(self.candidate_pin, wanted, false)?
        {
            return Ok(true);
        }
        if self
            .storage
            .contains_page(self.current.root, wanted, true)?
        {
            return Ok(true);
        }
        // Earlier candidate roots in this active log remain replay dependencies,
        // even if a later baseline event superseded the connection's pointer.
        let mut log = reread_file(&self.storage.root, &format!("log-{}", self.current.log))?;
        let length = log.metadata()?.len();
        if length > 16 * 1024 * 1024 {
            return Err(StoreError::LimitExceeded);
        }
        log.seek(SeekFrom::Start(8))?;
        let _: format::LogHeader = self
            .storage
            .read_log_record(&mut log, 0)?
            .ok_or(StoreError::Corrupt)?;
        let mut sequence = format::next(self.current.sequence)?;
        while let Some(event) = self
            .storage
            .read_log_record::<format::Event>(&mut log, sequence)?
        {
            let root = match event.change {
                format::Change::BaselineCommit { root, .. } => root,
                format::Change::InitialReviewAccepted { review, .. } => Some(review.root),
                _ => None,
            };
            if self.storage.contains_page(root, wanted, false)? {
                return Ok(true);
            }
            sequence = format::next(sequence)?;
        }
        Ok(false)
    }
    fn retired_log(&self, file: &mut File, generation: u64) -> Result<bool, StoreError> {
        use std::io::{Seek, SeekFrom};
        if generation >= self.current.log {
            return Ok(false);
        }
        if file.metadata()?.len() > 16 * 1024 * 1024 {
            return Err(StoreError::LimitExceeded);
        }
        file.seek(SeekFrom::Start(0))?;
        self.storage.read_log_magic(file)?;
        let header: format::LogHeader = self
            .storage
            .read_log_record(file, 0)?
            .ok_or(StoreError::Corrupt)?;
        if header.schema != 1
            || header.identity != self.storage.identity
            || header.generation != generation
            || header.generation >= self.current.generation
        {
            return Err(StoreError::Corrupt);
        }
        let mut sequence = header.first_sequence;
        let mut predecessor = header.predecessor;
        loop {
            let offset = file.stream_position()?;
            let Some(event) = self
                .storage
                .read_log_record::<format::Event>(file, sequence)?
            else {
                if offset != file.metadata()?.len() {
                    return Err(StoreError::RecoveryRequired);
                }
                break;
            };
            if event.schema != 1
                || event.predecessor != predecessor
                || sequence > self.current.sequence
            {
                return Err(StoreError::Corrupt);
            }
            predecessor = format::digest(&format::encode(&event)?);
            sequence = format::next(sequence)?;
        }
        Ok(true)
    }
    pub(super) fn collect_garbage(&mut self) -> Result<usize, StoreError> {
        let _read_lease = self.storage.read_lease(false)?;
        self.resume_gc()?;
        let reachable = self.gc_reachable_pages()?;
        let mut scan = match self.gc_scan.take() {
            Some(scan) => scan,
            None => GcScan {
                pages: cap_std::fs::Dir::from_std_file(enumeration_file(&self.storage.pages)?)
                    .entries()?,
                logs: cap_std::fs::Dir::from_std_file(enumeration_file(&self.storage.root)?)
                    .entries()?,
                phase: 0,
            },
        };
        let mut removed = 0;
        for _ in 0..32 {
            let next = if scan.phase == 0 {
                scan.pages.next()
            } else {
                scan.logs.next()
            };
            let Some(next) = next else {
                if scan.phase == 0 {
                    scan.phase = 1;
                    continue;
                }
                self.gc_scan = None;
                return Ok(removed);
            };
            let name = next?.file_name();
            let Some(name) = name.to_str() else { continue };
            let page = scan.phase == 0;
            let parent = if page {
                Arc::clone(&self.storage.pages)
            } else {
                Arc::clone(&self.storage.root)
            };
            if page {
                let Some(hash) = parse_hash(name) else {
                    continue;
                };
                if reachable.contains(&hash) {
                    continue;
                }
                self.storage.page(hash)?;
            } else {
                let Some(generation) = name
                    .strip_prefix("log-")
                    .and_then(|value| value.parse::<u64>().ok())
                else {
                    continue;
                };
                if name != format!("log-{generation}") || generation >= self.current.log {
                    continue;
                }
                let mut file = open_file(&parent, name, false)?;
                if !self.retired_log(&mut file, generation)? {
                    continue;
                }
            }
            let mut file = open_file(&parent, name, false)?;
            let metadata = regular(&file)?;
            let (digest, length) = file_digest(
                &mut file,
                if page {
                    format::MAX_BYTES as u64
                } else {
                    16 * 1024 * 1024
                },
                &|requested| self.storage.review_charge(requested as u64, 0, 0, 1),
            )?;
            validate_file(&parent, name, &file, &metadata)?;
            self.validate()?;
            let intent = GcIntent {
                schema: 1,
                identity: self.storage.identity.clone(),
                generation: self.current.generation,
                sequence: self.current.sequence,
                page,
                name: name.to_string(),
                file_id: file_identity(&metadata),
                length,
                digest,
            };
            self.storage.create_bytes(
                &self.storage.quarantine,
                "intent",
                &format::encode(&intent)?,
            )?;
            #[cfg(windows)]
            drop(file);
            self.finish_gc(&intent)?;
            removed += 1;
        }
        self.gc_scan = Some(scan);
        Ok(removed)
    }
    pub(super) fn resume_gc(&self) -> Result<(), StoreError> {
        let _read_lease = self.storage.read_lease(false)?;
        match metadata_nofollow(&self.storage.quarantine.file, Path::new("intent")) {
            Ok(_) => {
                let bytes = self
                    .storage
                    .read_bytes(&self.storage.quarantine, "intent", 4096)?;
                let intent: GcIntent =
                    serde_json::from_slice(&bytes).map_err(|_| StoreError::RecoveryRequired)?;
                self.finish_gc(&intent)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match metadata_nofollow(&self.storage.quarantine.file, Path::new("captured")) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    _ => Err(StoreError::RecoveryRequired),
                }
            }
            Err(error) => Err(error.into()),
        }
    }
    fn finish_gc(&self, intent: &GcIntent) -> Result<(), StoreError> {
        self.validate()?;
        component(&intent.name)?;
        if intent.schema != 1
            || intent.identity != self.storage.identity
            || intent.generation > self.current.generation
            || intent.sequence > self.current.sequence
        {
            return Err(StoreError::RecoveryRequired);
        }
        let parent = if intent.page {
            Arc::clone(&self.storage.pages)
        } else {
            Arc::clone(&self.storage.root)
        };
        if intent.page {
            let hash = parse_hash(&intent.name).ok_or(StoreError::Corrupt)?;
            if hash != intent.digest || self.page_referenced(hash)? {
                return Err(StoreError::RecoveryRequired);
            }
        } else {
            let generation = intent
                .name
                .strip_prefix("log-")
                .and_then(|name| name.parse::<u64>().ok())
                .ok_or(StoreError::Corrupt)?;
            if generation >= self.current.log || intent.name != format!("log-{generation}") {
                return Err(StoreError::RecoveryRequired);
            }
        }
        let quarantine = Arc::clone(&self.storage.quarantine);
        let intent_file = mutation_file(&quarantine, "intent")?;
        let intent_identity = regular(&intent_file)?;
        let captured = metadata_nofollow(&quarantine.file, Path::new("captured"));
        if captured
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            match metadata_nofollow(&parent.file, Path::new(&intent.name)) {
                Ok(_) => {
                    let source = mutation_file(&parent, &intent.name)?;
                    #[cfg(windows)]
                    let mut source = source;
                    let metadata = regular(&source)?;
                    if file_identity(&metadata) != intent.file_id || metadata.len() != intent.length
                    {
                        return Err(StoreError::RecoveryRequired);
                    }
                    validate_file(&parent, &intent.name, &source, &metadata)?;
                    #[cfg(windows)]
                    {
                        let (digest, length) =
                            file_digest(&mut source, intent.length, &|requested| {
                                self.storage.review_charge(requested as u64, 0, 0, 1)
                            })?;
                        if digest != intent.digest || length != intent.length {
                            return Err(StoreError::RecoveryRequired);
                        }
                    }
                    #[cfg(not(windows))]
                    capture_no_replace(&parent, &intent.name, &quarantine)?;
                    #[cfg(windows)]
                    windows::capture(&source, &parent, &intent.name, &quarantine)?;
                    sync_directory(&parent)?;
                    sync_directory(&quarantine)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {} // Interrupted after an already-safe unlink.
                Err(error) => return Err(error.into()),
            }
        } else if let Err(error) = captured {
            return Err(error.into());
        }
        #[cfg(windows)]
        windows::require_absent(&parent, &intent.name)?;
        match metadata_nofollow(&quarantine.file, Path::new("captured")) {
            Ok(_) => {
                let mut captured = mutation_file(&quarantine, "captured")?;
                let metadata = regular(&captured)?;
                if file_identity(&metadata) != intent.file_id || metadata.len() != intent.length {
                    return Err(StoreError::RecoveryRequired);
                }
                let (digest, length) = file_digest(&mut captured, intent.length, &|requested| {
                    self.storage.review_charge(requested as u64, 0, 0, 1)
                })?;
                if digest != intent.digest || length != intent.length {
                    return Err(StoreError::RecoveryRequired);
                }
                self.validate()?;
                validate_file(&quarantine, "captured", &captured, &metadata)?;
                #[cfg(not(windows))]
                cap_primitives::fs::remove_file(&quarantine.file, Path::new("captured"))?;
                #[cfg(windows)]
                windows::delete(captured, self, "captured", &metadata, intent)?;
                sync_directory(&quarantine)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.validate()?;
        validate_file(&quarantine, "intent", &intent_file, &intent_identity)?;
        #[cfg(not(windows))]
        cap_primitives::fs::remove_file(&quarantine.file, Path::new("intent"))?;
        #[cfg(windows)]
        windows::delete(intent_file, self, "intent", &intent_identity, intent)?;
        sync_directory(&quarantine)?;
        Ok(())
    }
}

#[cfg(not(windows))]
pub(super) fn file_identity(metadata: &Metadata) -> format::FileIdentity {
    format::FileIdentity::Unix(metadata.ino())
}

#[cfg(not(windows))]
pub(super) use super::super::super::migration::same_snapshot;
#[cfg(not(windows))]
pub(super) fn read_file(parent: &Arc<Directory>, name: &str) -> Result<File, StoreError> {
    open_file(parent, name, false)
}
pub(super) fn reread_file(parent: &Arc<Directory>, name: &str) -> Result<File, StoreError> {
    #[cfg(not(windows))]
    {
        read_file(parent, name)
    }
    #[cfg(windows)]
    {
        windows::reread_file(parent, name)
    }
}
fn enumeration_file(parent: &Arc<Directory>) -> Result<File, StoreError> {
    #[cfg(not(windows))]
    {
        Ok(parent.file.try_clone()?)
    }
    #[cfg(windows)]
    {
        Ok(parent.entries_file()?)
    }
}

fn mutation_file(parent: &Arc<Directory>, name: &str) -> Result<File, StoreError> {
    #[cfg(not(windows))]
    {
        open_file(parent, name, false)
    }
    #[cfg(windows)]
    {
        windows::mutation_file(parent, name)
    }
}
#[cfg(all(windows, test))]
pub(super) fn capture_no_replace(
    parent: &Arc<Directory>,
    name: &str,
    quarantine: &Arc<Directory>,
) -> Result<(), StoreError> {
    let file = mutation_file(parent, name)?;
    windows::capture(&file, parent, name, quarantine)
}

#[cfg(windows)]
pub(in crate::connection) use windows::{CurrentReplacement, GcDeletion};

// Unix keeps its exact File interface and open behavior. Windows readers carry
// their sharing guard and never expose an unguarded inspection File.
#[cfg(not(windows))]
pub(super) type RecoveryReader = File;
#[cfg(windows)]
pub(super) use windows::{
    RecoveryReader, RecoveryWriterGuard, guarded_reader, preserved_reader, recovery_metadata,
    validate_recovery_reader,
};
#[cfg(not(windows))]
pub(super) fn recovery_metadata(reader: &RecoveryReader) -> Result<Metadata, StoreError> {
    regular(reader)
}
#[cfg(not(windows))]
pub(super) fn validate_recovery_reader(
    parent: &Arc<Directory>,
    name: &str,
    reader: &RecoveryReader,
    expected: &Metadata,
) -> Result<(), StoreError> {
    validate_file(parent, name, reader, expected)
}

#[cfg(test)]
mod identity_wire_tests {
    use super::*;
    #[test]
    fn gc_identity_preserves_unix_shape_and_rejects_mixed_duplicate_and_platform() {
        let identity = Identity {
            volume: [1; 32],
            root: [2; 32],
        };
        let legacy = format!(
            r#"{{"schema":1,"identity":{},"generation":3,"sequence":4,"page":true,"name":"old","inode":42,"length":2,"digest":{}}}"#,
            serde_json::to_string(&identity).unwrap(),
            serde_json::to_string(&[5_u8; 32]).unwrap()
        );
        if cfg!(windows) {
            assert!(serde_json::from_str::<GcIntent>(&legacy).is_err());
        } else {
            let decoded: GcIntent = serde_json::from_str(&legacy).unwrap();
            assert_eq!(format::encode(&decoded).unwrap(), legacy.as_bytes());
        }
        for bad in [
            legacy.replace("\"inode\":42", "\"inode\":42,\"inode\":42"),
            legacy.replace("\"inode\":42", "\"inode\":42,\"windows_file_id_v1\":null"),
            legacy.replace("\"inode\":42", "\"inode\":null"),
        ] {
            assert!(serde_json::from_str::<GcIntent>(&bad).is_err());
        }
        let native = legacy.replace("\"inode\":42", r#""windows_file_id_v1":{"volume_serial":7,"file_id":[0,0,0,0,0,0,0,0,1,0,0,0,0,0,0,0]}"#);
        let wire: GcWire = serde_json::from_str(&native).unwrap();
        assert_eq!(serde_json::to_string(&wire).unwrap(), native);
        assert_eq!(
            serde_json::from_str::<GcIntent>(&native).is_ok(),
            cfg!(windows)
        );
    }
}
