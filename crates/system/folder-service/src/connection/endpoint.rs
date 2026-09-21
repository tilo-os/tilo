//! Native, handle-pinned endpoint discovery. Probing is strictly read-only.
//!
//! A locator is only a bounded hint. Opening always attests the stored volume
//! and root identity again. Native evidence is not a filesystem-label promise.
use super::super::destination_security;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cap_fs_ext::{MetadataExt, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};
use cap_primitives::fs::{FollowSymlinks, Metadata, OpenOptions};
use cap_std::fs::Dir;
use sha2::{Digest, Sha256};

use super::model::{
    Encoding, Endpoint, EndpointIdentity, EntryState, MAX_COMPONENTS, MAX_ENTRIES, NamePolicy,
    Observation, ObservedTree, PendingReason, RelativePath, ScopeIssue,
};
#[cfg(target_os = "linux")]
#[path = "endpoint/linux.rs"]
mod native;
#[cfg(windows)]
#[path = "endpoint/windows.rs"]
mod native;
#[cfg(not(any(target_os = "linux", windows)))]
#[path = "endpoint/unsupported.rs"]
mod native;

#[cfg(any(target_os = "linux", test))]
fn reject_clone_candidate(
    volume: &[u8; 16],
    candidate: Result<[u8; 16], EndpointError>,
) -> Result<(), EndpointError> {
    match candidate {
        Ok(uuid) if uuid != *volume => Ok(()),
        Ok(_) | Err(_) => Err(EndpointError::AmbiguousIdentity),
    }
}
#[cfg(any(target_os = "linux", test))]
fn serial_identity(
    serial: &[u8],
    vendor: Option<&[u8]>,
    model: Option<&[u8]>,
) -> Result<Vec<u8>, EndpointError> {
    let vendor = vendor.ok_or(EndpointError::Unsupported)?;
    let model = model.ok_or(EndpointError::Unsupported)?;
    let mut identity = Vec::new();
    for (label, value) in [
        (b"serial".as_slice(), serial),
        (b"vendor".as_slice(), vendor),
        (b"model".as_slice(), model),
    ] {
        if value.is_empty() || value.contains(&0) || value.len() > 4096 {
            return Err(EndpointError::Unsupported);
        }
        identity.extend_from_slice(label);
        identity.push(0);
        identity.extend_from_slice(&(value.len() as u32).to_le_bytes());
        identity.extend_from_slice(value);
    }
    Ok(identity)
}

#[cfg(any(windows, test))]
fn windows_metadata_policy(
    directory: bool,
    links: u32,
    named_stream: bool,
    baseline_acl: bool,
) -> Result<(), EndpointError> {
    if (!directory && links != 1) || named_stream || !baseline_acl {
        return Err(EndpointError::Unsupported);
    }
    Ok(())
}

#[cfg(all(target_os = "linux", test))]
#[path = "endpoint/linux/ntfs.rs"]
pub mod ntfs;

#[cfg(test)]
pub enum ReadEndpoint {
    Local(PinnedEndpoint),
    #[cfg(target_os = "linux")]
    Ntfs(Arc<ntfs::NtfsReadEndpoint>),
}

#[cfg(test)]
pub fn observe_read(endpoint: &ReadEndpoint, cancelled: &AtomicBool) -> ObservedTree {
    match endpoint {
        ReadEndpoint::Local(local) => observe(local, cancelled),
        #[cfg(target_os = "linux")]
        ReadEndpoint::Ntfs(native) => native.observe(cancelled),
    }
}

const MAX_ENDPOINTS: usize = 128;
static NEXT_PROBE: AtomicU64 = AtomicU64::new(0);
const MAX_HASH_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointError {
    Offline,
    PermissionDenied,
    IdentityChanged,
    AmbiguousIdentity,
    Unsupported,
    UnsafeVolume,
    ReadOnly,
    Unverified,
    Unstable,
    LimitExceeded,
    Cancelled,
}
impl EndpointError {
    fn reason(self) -> PendingReason {
        match self {
            Self::Offline => PendingReason::Offline,
            Self::PermissionDenied => PendingReason::PermissionDenied,
            Self::IdentityChanged | Self::AmbiguousIdentity => PendingReason::IdentityChanged,
            Self::Unstable => PendingReason::Unstable,
            Self::LimitExceeded | Self::Cancelled => PendingReason::IncompleteScope,
            _ => PendingReason::Unsupported,
        }
    }
}
impl From<io::Error> for EndpointError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotConnected => Self::Offline,
            io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            io::ErrorKind::ReadOnlyFilesystem => Self::ReadOnly,
            // Device loss and stale handles are unavailability, never absence.
            _ if matches!(error.raw_os_error(), Some(5 | 6 | 19 | 116)) => Self::Offline,
            _ => Self::Unsupported,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    Available,
    Refused(EndpointError),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub safe_read: Capability,
    pub safe_write: Capability,
    pub durable_transactions: Capability,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointLocator(PathBuf);
impl EndpointLocator {
    /// A stored locator confers no authority or identity.
    ///
    /// # Errors
    /// Refuses nonabsolute, traversal-containing and excessively long paths.
    pub fn new(path: &Path) -> Result<Self, EndpointError> {
        if !path.is_absolute() || path.as_os_str().len() > 32_768 {
            return Err(EndpointError::Unsupported);
        }
        let components: Vec<_> = path.components().collect();
        if components.len() > MAX_COMPONENTS
            || components
                .iter()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(EndpointError::LimitExceeded);
        }
        Ok(Self(path.to_path_buf()))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}
#[derive(Clone, Debug)]
pub struct EndpointProbe {
    pub identity: EndpointIdentity,
    pub locator: EndpointLocator,
    #[cfg(test)]
    pub capabilities: Capabilities,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    #[cfg(test)]
    Read,
    Write,
}

/// Explicit process-owned lookup of persisted hints. No global ambient registry.
#[derive(Default)]
pub struct EndpointContext {
    locators: BTreeMap<([u8; 32], [u8; 32]), EndpointLocator>,
    #[cfg(all(target_os = "linux", test))]
    ntfs: BTreeMap<([u8; 32], [u8; 32]), Arc<ntfs::NtfsReadEndpoint>>,
}
impl EndpointContext {
    /// Register one authenticated administrative session without granting public safe-read support.
    /// # Errors
    /// Refuses identity mismatch, duplicate live authorities and resource exhaustion.
    #[cfg(all(target_os = "linux", test))]
    pub fn register_ntfs_session(
        &mut self,
        client: Arc<std::sync::Mutex<qos_ntfs_protocol::client::Client>>,
        expected: EndpointIdentity,
    ) -> Result<(), EndpointError> {
        let key = (expected.volume, expected.root);
        if self.locators.contains_key(&key) {
            return Err(EndpointError::AmbiguousIdentity);
        }
        if let Some(previous) = self.ntfs.get(&key) {
            return if previous.same_session(&client) {
                Ok(())
            } else {
                Err(EndpointError::AmbiguousIdentity)
            };
        }
        if self.locators.len().saturating_add(self.ntfs.len()) >= MAX_ENDPOINTS {
            return Err(EndpointError::LimitExceeded);
        }
        self.ntfs.insert(
            key,
            Arc::new(ntfs::NtfsReadEndpoint::new(client, expected)?),
        );
        Ok(())
    }
    /// # Errors
    /// Refuses missing or invalidated local identities; native operations reattest through their retained broker.
    #[cfg(test)]
    pub fn open_read_endpoint(
        &self,
        expected: &EndpointIdentity,
    ) -> Result<ReadEndpoint, EndpointError> {
        #[cfg(all(target_os = "linux", test))]
        if let Some(endpoint) = self.ntfs.get(&(expected.volume, expected.root)) {
            return Ok(ReadEndpoint::Ntfs(Arc::clone(endpoint)));
        }
        self.open_endpoint(expected, Access::Read)
            .map(ReadEndpoint::Local)
    }

    /// Register a selected or persisted hint, validating its expected identity.
    ///
    /// # Errors
    /// Refuses stale, unavailable, ambiguous or unsupported endpoint identities.
    pub fn register(
        &mut self,
        locator: EndpointLocator,
        expected: EndpointIdentity,
    ) -> Result<(), EndpointError> {
        let key = (expected.volume, expected.root);
        #[cfg(all(target_os = "linux", test))]
        if self.ntfs.contains_key(&key) {
            return Err(EndpointError::AmbiguousIdentity);
        }
        #[cfg(all(target_os = "linux", test))]
        if self.locators.len().saturating_add(self.ntfs.len()) >= MAX_ENDPOINTS
            && !self.locators.contains_key(&key)
        {
            return Err(EndpointError::LimitExceeded);
        }
        if self.locators.len() >= MAX_ENDPOINTS && !self.locators.contains_key(&key) {
            return Err(EndpointError::LimitExceeded);
        }
        let probe = probe_endpoint(locator.path())?;
        if probe.identity != expected {
            return Err(EndpointError::IdentityChanged);
        }
        if let Some(previous) = self.locators.get(&key) {
            let a = Pin::open(previous.path())?;
            let b = Pin::open(locator.path())?;
            if !same_object(&a.metadata, &b.metadata) {
                return Err(EndpointError::AmbiguousIdentity);
            }
        }
        self.locators.insert(key, locator);
        Ok(())
    }
    /// Reopen a previously selected endpoint with fresh mount and identity evidence.
    ///
    /// # Errors
    /// Refuses identity changes, media loss and access without native proof.
    pub fn open_endpoint(
        &self,
        expected: &EndpointIdentity,
        access: Access,
    ) -> Result<PinnedEndpoint, EndpointError> {
        #[cfg(all(target_os = "linux", test))]
        if self.ntfs.contains_key(&(expected.volume, expected.root)) {
            return Err(if access == Access::Write {
                EndpointError::ReadOnly
            } else {
                EndpointError::Unverified
            });
        }
        let locator = self
            .locators
            .get(&(expected.volume, expected.root))
            .ok_or(EndpointError::Offline)?;
        let root = Pin::open(locator.path())?;
        let evidence = native::inspect(&root.file)?;
        if evidence.identity != *expected {
            return Err(EndpointError::IdentityChanged);
        }
        root.validate()?;
        let endpoint = PinnedEndpoint {
            root,
            locator: locator.clone(),
            evidence,
            invalidated: AtomicBool::new(false),
            write_verified: AtomicBool::new(false),
        };
        endpoint.revalidate()?;
        if access == Access::Write {
            verify_selected_write(&endpoint)?;
            // Windows runtime evidence is still absent; a diagnostic trial
            // cannot make it a supported production write configuration.
            #[cfg(windows)]
            return Err(EndpointError::Unverified);
        }
        Ok(endpoint)
    }
}

pub struct PinnedEndpoint {
    locator: EndpointLocator,
    root: Arc<Pin>,
    evidence: NativeEvidence,
    invalidated: AtomicBool,
    write_verified: AtomicBool,
}
impl PinnedEndpoint {
    pub fn capabilities(&self) -> Capabilities {
        let mut capabilities = self.evidence.capabilities;
        if cfg!(target_os = "linux")
            && self.write_verified.load(Ordering::Acquire)
            && !self.invalidated.load(Ordering::Acquire)
        {
            capabilities.safe_write = Capability::Available;
            capabilities.durable_transactions = Capability::Available;
        }
        capabilities
    }
    pub fn identity(&self) -> EndpointIdentity {
        self.evidence.identity
    }
    /// Revalidate a previously tested write session immediately before each
    /// mutation. Namespace permission changes revoke the session as well.
    ///
    /// # Errors
    /// Refuses untested writes, identity changes, unsafe mounts or permissions.
    pub fn revalidate_for_write(&self) -> Result<(), EndpointError> {
        if !self.write_verified.load(Ordering::Acquire) {
            return Err(EndpointError::Unverified);
        }
        self.revalidate()?;
        if let Err(error) = validate_write_namespace(&self.root) {
            self.invalidate();
            return Err(error);
        }
        #[cfg(windows)]
        return Err(EndpointError::Unverified);
        #[cfg(not(windows))]
        Ok(())
    }
    pub fn invalidate(&self) {
        self.invalidated.store(true, Ordering::Release);
    }
    /// Recheck retained handles, current mount state and persistent identity.
    /// Any failure permanently invalidates this session handle.
    ///
    /// # Errors
    /// Returns an explicit unavailable, unsafe or changed-identity reason.
    pub fn revalidate(&self) -> Result<(), EndpointError> {
        if self.invalidated.load(Ordering::Acquire) {
            return Err(EndpointError::IdentityChanged);
        }
        let result = self
            .root
            .validate()
            .and_then(|()| {
                let current_pin = Pin::open(self.locator.path())?;
                native::same_mount(&self.root.file, &current_pin.file)?;
                native::inspect(&self.root.file)
            })
            .and_then(|current| {
                if current == self.evidence {
                    Ok(())
                } else {
                    Err(EndpointError::IdentityChanged)
                }
            });
        if result.is_err() {
            self.invalidate();
        }
        result
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeEvidence {
    identity: EndpointIdentity,
    // Session mount token, deliberately absent from persistent identity.
    mount: Vec<u8>,
    capabilities: Capabilities,
    names: NamePolicy,
}
/// Read-only capability discovery; never writes a probe file, unlocks or mounts.
///
/// # Errors
/// Refuses unsafe traversal, unavailable media or missing native identity proof.
pub fn probe_endpoint(path: &Path) -> Result<EndpointProbe, EndpointError> {
    let locator = EndpointLocator::new(path)?;
    let pin = Pin::open(locator.path())?;
    let evidence = native::inspect(&pin.file)?;
    pin.validate()?;
    Ok(EndpointProbe {
        identity: evidence.identity,
        locator,
        #[cfg(test)]
        capabilities: evidence.capabilities,
    })
}
#[cfg(any(target_os = "linux", windows))]
fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn same_object(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn metadata(file: &File) -> Result<Metadata, EndpointError> {
    Metadata::from_file(file).map_err(Into::into)
}
fn stat(parent: &File, name: &Path) -> Result<Metadata, EndpointError> {
    cap_primitives::fs::stat(parent, name, FollowSymlinks::No).map_err(Into::into)
}
fn reject_special(meta: &Metadata) -> Result<(), EndpointError> {
    if meta.is_symlink() || (!meta.is_dir() && !meta.is_file()) {
        return Err(EndpointError::Unsupported);
    }
    #[cfg(windows)]
    {
        use cap_primitives::fs::MetadataExt;
        // Reparse, offline, encrypted, recall-on-open/data; never hydrate.
        if meta.file_attributes() & (0x400 | 0x1000 | 0x4000 | 0x40000 | 0x0040_0000) != 0 {
            return Err(EndpointError::Unsupported);
        }
    }
    Ok(())
}
struct Pin {
    file: File,
    metadata: Metadata,
    parent: Option<(Arc<Self>, OsString)>,
}
impl Pin {
    fn open(path: &Path) -> Result<Arc<Self>, EndpointError> {
        EndpointLocator::new(path)?;
        let mut anchor = PathBuf::new();
        let mut names = Vec::new();
        for c in path.components() {
            match c {
                Component::Prefix(_) | Component::RootDir if names.is_empty() => anchor.push(c),
                Component::Normal(name) => names.push(name),
                _ => return Err(EndpointError::Unsupported),
            }
        }
        let file =
            cap_primitives::fs::open_ambient_dir(&anchor, cap_primitives::ambient_authority())?;
        let mut pin = Arc::new(Self {
            metadata: metadata(&file)?,
            file,
            parent: None,
        });
        for name in names {
            pin = pin.child(name)?;
        }
        pin.validate()?;
        Ok(pin)
    }
    fn child(self: &Arc<Self>, name: &std::ffi::OsStr) -> Result<Arc<Self>, EndpointError> {
        let before = stat(&self.file, Path::new(name))?;
        reject_special(&before)?;
        if !before.is_dir() {
            return Err(EndpointError::Unsupported);
        }
        let file = cap_primitives::fs::open_dir_nofollow(&self.file, Path::new(name))?;
        let after = metadata(&file)?;
        reject_special(&after)?;
        if !same_object(&before, &after) {
            return Err(EndpointError::IdentityChanged);
        }
        Ok(Arc::new(Self {
            file,
            metadata: after,
            parent: Some((Arc::clone(self), name.to_os_string())),
        }))
    }
    fn validate(&self) -> Result<(), EndpointError> {
        let mut current = self;
        loop {
            reject_special(&metadata(&current.file)?)?;
            let Some((parent, name)) = &current.parent else {
                break;
            };
            let observed = stat(&parent.file, Path::new(name))?;
            reject_special(&observed)?;
            if !observed.is_dir() || !same_object(&observed, &current.metadata) {
                return Err(EndpointError::IdentityChanged);
            }
            current = parent;
        }
        Ok(())
    }
}
fn readable_directory(file: &File) -> Result<File, EndpointError> {
    let mut options = OpenOptions::new();
    options.read(true).maybe_dir(true);
    cap_primitives::fs::open(file, Path::new("."), &options).map_err(Into::into)
}
fn relative(path: &Path) -> Result<RelativePath, EndpointError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        RelativePath::unix(path.as_os_str().as_bytes()).map_err(|_| EndpointError::LimitExceeded)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        RelativePath::windows(&path.as_os_str().encode_wide().collect::<Vec<_>>())
            .map_err(|_| EndpointError::LimitExceeded)
    }
}
fn encoding() -> Encoding {
    if cfg!(windows) {
        Encoding::Windows
    } else {
        Encoding::Unix
    }
}
fn same_snapshot(a: &Metadata, b: &Metadata) -> bool {
    if !same_object(a, b)
        || a.len() != b.len()
        || a.modified().ok() != b.modified().ok()
        || a.permissions() != b.permissions()
        || a.is_dir() != b.is_dir()
        || a.is_file() != b.is_file()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use cap_primitives::fs::MetadataExt;
        if a.ctime() != b.ctime() || a.ctime_nsec() != b.ctime_nsec() {
            return false;
        }
    }
    true
}
struct Snapshot {
    path: PathBuf,
    metadata: Metadata,
    token: Vec<u8>,
}
fn change_token(file: &File) -> Result<Vec<u8>, EndpointError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file.metadata()?;
        let mut token = meta.ctime().to_le_bytes().to_vec();
        token.extend_from_slice(&meta.ctime_nsec().to_le_bytes());
        Ok(token)
    }
    #[cfg(windows)]
    {
        native::change_token(file)
    }
}
fn validate_snapshot(root: &Arc<Pin>, snapshot: &Snapshot) -> Result<(), EndpointError> {
    let mut parent = Arc::clone(root);
    let mut components = snapshot.path.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(EndpointError::Unsupported);
        };
        if components.peek().is_some() || snapshot.metadata.is_dir() {
            let child = parent.child(name)?;
            #[cfg(any(target_os = "linux", windows))]
            native::same_mount(&parent.file, &child.file)?;
            parent = child;
        } else {
            reject_special(&stat(&parent.file, Path::new(name))?)?;
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_primitives::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NONBLOCK);
            }
            let file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
            let current = metadata(&file)?;
            reject_special(&current)?;
            parent.validate()?;
            return if same_snapshot(&snapshot.metadata, &current)
                && change_token(&file)? == snapshot.token
            {
                Ok(())
            } else {
                Err(EndpointError::Unstable)
            };
        }
    }
    parent.validate()?;
    if same_snapshot(&snapshot.metadata, &metadata(&parent.file)?)
        && change_token(&parent.file)? == snapshot.token
    {
        Ok(())
    } else {
        Err(EndpointError::Unstable)
    }
}
struct Scan<'a> {
    #[cfg(target_os = "linux")]
    owned: Option<&'a super::transaction::native::NativeOwnedObservation<'a>>,
    cancelled: &'a AtomicBool,
    entries: BTreeMap<RelativePath, EntryState>,
    issues: Vec<ScopeIssue>,
    bytes: u64,
    visited: usize,
    snapshots: Vec<Snapshot>,
}
impl Scan<'_> {
    fn check(&self) -> Result<(), EndpointError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(EndpointError::Cancelled);
        }
        if self.visited >= MAX_ENTRIES {
            return Err(EndpointError::LimitExceeded);
        }
        Ok(())
    }
    fn issue(&mut self, path: Option<RelativePath>, error: EndpointError) {
        if self.issues.len() < MAX_ENTRIES {
            self.issues.push(ScopeIssue {
                path,
                reason: error.reason(),
            });
        } else {
            self.issues.clear();
            self.issues.push(ScopeIssue {
                path: None,
                reason: PendingReason::IncompleteScope,
            });
        }
    }
    fn directory(
        &mut self,
        dir: &Arc<Pin>,
        path: &Path,
        depth: usize,
    ) -> Result<(), EndpointError> {
        self.check()?;
        if depth >= MAX_COMPONENTS {
            return Err(EndpointError::LimitExceeded);
        }
        let before = metadata(&dir.file)?;
        let token = change_token(&dir.file)?;
        native::check_metadata(&readable_directory(&dir.file)?, true)?;
        let iterator = Dir::from_std_file(readable_directory(&dir.file)?).entries()?;
        for entry in iterator {
            self.check()?;
            self.visited += 1;
            let entry = entry?;
            let name = entry.file_name();
            let child_path = path.join(&name);
            let key = relative(&child_path)?;
            #[cfg(target_os = "linux")]
            if depth == 0 {
                if let Some(owned) = self.owned {
                    match owned.excludes(&NativeDirectory(Arc::clone(dir)), &name) {
                        Ok(true) => continue,
                        Ok(false) => {}
                        Err(error) => self.issue(Some(key.clone()), error),
                    }
                }
            }

            let result = (|| {
                dir.validate()?;
                let meta = stat(&dir.file, Path::new(&name))?;
                reject_special(&meta)?;
                if meta.is_dir() {
                    let child = dir.child(&name)?;
                    native::same_mount(&dir.file, &child.file)?;
                    self.entries.insert(key.clone(), EntryState::Directory);
                    self.directory(&child, &child_path, depth + 1)
                } else {
                    let state = self.file(dir, &name, &meta, &child_path)?;
                    self.entries.insert(key.clone(), state);
                    Ok(())
                }
            })();
            if let Err(error) = result {
                self.issue(Some(key), error);
            }
        }
        dir.validate()?;
        if !same_snapshot(&before, &metadata(&dir.file)?) {
            return Err(EndpointError::Unstable);
        }
        self.snapshots.push(Snapshot {
            path: path.to_path_buf(),
            metadata: before,
            token,
        });
        Ok(())
    }
    fn file(
        &mut self,
        parent: &Arc<Pin>,
        name: &std::ffi::OsStr,
        expected: &Metadata,
        path: &Path,
    ) -> Result<EntryState, EndpointError> {
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_primitives::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use cap_primitives::fs::OpenOptionsExt;
            options.share_mode(1);
        }
        let mut file = cap_primitives::fs::open(&parent.file, Path::new(name), &options)?;
        let before = metadata(&file)?;
        reject_special(&before)?;
        if !before.is_file() || !same_snapshot(expected, &before) {
            return Err(EndpointError::Unstable);
        }
        native::same_mount(&parent.file, &file)?;
        let executable = native::check_metadata(&file, false)?;
        if before.len() > MAX_HASH_BYTES.saturating_sub(self.bytes) {
            return Err(EndpointError::LimitExceeded);
        }
        let token = change_token(&file)?;
        let mut hash = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(EndpointError::Cancelled);
            }
            if self.bytes == MAX_HASH_BYTES {
                break;
            }
            let remaining = usize::try_from(MAX_HASH_BYTES.saturating_sub(self.bytes))
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            #[cfg(target_os = "linux")]
            if let Some(owned) = self.owned {
                owned.charge(remaining as u64)?;
            }
            let count = file.read(
                buffer
                    .get_mut(..remaining)
                    .ok_or(EndpointError::LimitExceeded)?,
            )?;
            if count == 0 {
                break;
            }
            bytes += count as u64;
            self.bytes += count as u64;
            if self.bytes > MAX_HASH_BYTES {
                return Err(EndpointError::LimitExceeded);
            }
            let chunk = buffer.get(..count).ok_or(EndpointError::Unstable)?;
            hash.update(chunk);
        }
        if bytes != before.len()
            || !same_snapshot(&before, &metadata(&file)?)
            || !same_snapshot(&before, &stat(&parent.file, Path::new(name))?)
        {
            return Err(EndpointError::Unstable);
        }
        if change_token(&file)? != token {
            return Err(EndpointError::Unstable);
        }
        parent.validate()?;
        self.snapshots.push(Snapshot {
            path: path.to_path_buf(),
            metadata: before,
            token,
        });
        Ok(EntryState::File {
            sha256: hash.finalize().into(),
            bytes,
            executable,
        })
    }
}
/// Observe bounded native entries without executing or hydrating anything.
/// Incomplete enumeration, cancellation and access failure are never deletion.
pub fn observe(endpoint: &PinnedEndpoint, cancelled: &AtomicBool) -> ObservedTree {
    observe_inner(
        endpoint,
        cancelled,
        #[cfg(target_os = "linux")]
        None,
    )
}
/// Exact registered namespaces only; failed or substituted ownership stays
/// visible and makes this Connected observation incomplete.
#[cfg(target_os = "linux")]
pub(super) fn observe_connected(
    endpoint: &PinnedEndpoint,
    owned: &super::transaction::native::NativeOwnedNames,
    cancelled: &AtomicBool,
) -> ObservedTree {
    observe_inner(endpoint, cancelled, Some(owned))
}
fn observe_inner(
    endpoint: &PinnedEndpoint,
    cancelled: &AtomicBool,
    #[cfg(target_os = "linux")] owned: Option<&super::transaction::native::NativeOwnedNames>,
) -> ObservedTree {
    let model = Endpoint {
        identity: endpoint.identity(),
        encoding: encoding(),
        names: endpoint.evidence.names.clone(),
    };
    if let Err(error) = endpoint.revalidate() {
        return ObservedTree {
            endpoint: model,
            entries: BTreeMap::new(),
            observation: Observation::Unavailable(error.reason()),
        };
    }
    #[cfg(target_os = "linux")]
    let ownership = owned.map(super::transaction::native::NativeOwnedNames::observation);
    #[cfg(target_os = "linux")]
    let owned = ownership.as_ref();
    let mut scan = Scan {
        #[cfg(target_os = "linux")]
        owned,
        cancelled,
        entries: BTreeMap::new(),
        issues: Vec::new(),
        bytes: 0,
        visited: 0,
        snapshots: Vec::new(),
    };
    #[cfg(target_os = "linux")]
    if let Some(owned) = owned {
        if let Err(error) = owned.validate_all(&NativeDirectory(Arc::clone(&endpoint.root))) {
            scan.issue(None, error);
        }
    }
    if let Err(error) = scan.directory(&endpoint.root, Path::new(""), 0) {
        scan.issue(None, error);
    }
    // Recheck early files and directories after later siblings have been
    // processed. Otherwise a late write could escape per-file hash checks.
    for snapshot in std::mem::take(&mut scan.snapshots) {
        if cancelled.load(Ordering::Relaxed) {
            scan.issue(None, EndpointError::Cancelled);
            break;
        }
        if let Err(error) = validate_snapshot(&endpoint.root, &snapshot) {
            scan.issue(relative(&snapshot.path).ok(), error);
        }
    }
    if let Err(error) = endpoint.revalidate() {
        return ObservedTree {
            endpoint: model,
            entries: BTreeMap::new(),
            observation: Observation::Unavailable(error.reason()),
        };
    }
    #[cfg(target_os = "linux")]
    if let Some(owned) = owned {
        if let Err(error) = owned.validate_all(&NativeDirectory(Arc::clone(&endpoint.root))) {
            scan.issue(None, error);
        }
    }
    let observation = if scan.issues.is_empty() {
        Observation::Complete
    } else {
        Observation::Incomplete(scan.issues)
    };
    // Bounds and ancestor entries are maintained together by Scan. No dropped
    // ModelError can turn an invalid tree into a complete empty observation.
    ObservedTree {
        endpoint: model,
        entries: scan.entries,
        observation,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pin_detects_replaced_directory_and_never_adopts_new_contents() {
        let temp = tempfile::tempdir().unwrap();
        let selected = temp.path().canonicalize().unwrap().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let pin = Pin::open(&selected).unwrap();
        std::fs::rename(&selected, temp.path().join("old")).unwrap();
        std::fs::create_dir(&selected).unwrap();
        assert_eq!(pin.validate(), Err(EndpointError::IdentityChanged));
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn diagnostic_write_cannot_promote_unverified_platform_capabilities() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let refused = Capabilities {
            safe_read: Capability::Available,
            safe_write: Capability::Refused(EndpointError::Unverified),
            durable_transactions: Capability::Refused(EndpointError::Unverified),
        };
        let endpoint = PinnedEndpoint {
            locator: EndpointLocator::new(&path).unwrap(),
            root: Pin::open(&path).unwrap(),
            evidence: NativeEvidence {
                identity: EndpointIdentity {
                    volume: [1; 32],
                    root: [2; 32],
                },
                mount: vec![],
                capabilities: refused,
                names: NamePolicy::Exact,
            },
            invalidated: AtomicBool::new(false),
            write_verified: AtomicBool::new(true),
        };
        assert_eq!(endpoint.capabilities(), refused);
    }
    #[test]
    fn unresolved_clone_evidence_requires_reselection() {
        for error in [
            EndpointError::Unsupported,
            EndpointError::Offline,
            EndpointError::UnsafeVolume,
            EndpointError::PermissionDenied,
            EndpointError::IdentityChanged,
        ] {
            assert_eq!(
                reject_clone_candidate(&[1; 16], Err(error)),
                Err(EndpointError::AmbiguousIdentity),
                "{error:?}"
            );
        }
        assert_eq!(
            reject_clone_candidate(&[1; 16], Ok([1; 16])),
            Err(EndpointError::AmbiguousIdentity)
        );
        assert_eq!(reject_clone_candidate(&[1; 16], Ok([2; 16])), Ok(()));
    }
    #[test]
    fn serial_without_manufacturer_and_model_is_not_identity() {
        for (vendor, model) in [
            (None, None),
            (Some(b"vendor".as_slice()), None),
            (None, Some(b"model".as_slice())),
            (Some(b"".as_slice()), Some(b"model".as_slice())),
        ] {
            assert_eq!(
                serial_identity(b"123", vendor, model),
                Err(EndpointError::Unsupported)
            );
        }
        assert_ne!(
            serial_identity(b"123", Some(b"AB"), Some(b"C")).unwrap(),
            serial_identity(b"123", Some(b"A"), Some(b"BC")).unwrap()
        );
    }
    #[test]
    fn windows_portability_policy_refuses_links_directory_streams_and_custom_acl() {
        assert_eq!(windows_metadata_policy(false, 1, false, true), Ok(()));
        assert_eq!(windows_metadata_policy(true, 1, false, true), Ok(()));
        for (directory, links, stream, acl) in [
            (false, 2, false, true),
            (true, 1, true, true),
            (false, 1, false, false),
            (true, 1, false, false),
        ] {
            assert_eq!(
                windows_metadata_policy(directory, links, stream, acl),
                Err(EndpointError::Unsupported)
            );
        }
    }
    #[test]
    fn validation_detects_a_previously_hashed_file_changed_later() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let path = base.join("early");
        std::fs::write(&path, b"old").unwrap();
        let file = File::open(&path).unwrap();
        let snapshot = Snapshot {
            path: PathBuf::from("early"),
            metadata: metadata(&file).unwrap(),
            token: change_token(&file).unwrap(),
        };
        std::fs::write(&path, b"newer").unwrap();
        assert_eq!(
            validate_snapshot(&Pin::open(&base).unwrap(), &snapshot),
            Err(EndpointError::Unstable)
        );
    }
    #[cfg(unix)]
    #[test]
    fn pin_refuses_symlink_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("real")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("real"), temp.path().join("link")).unwrap();
        assert!(Pin::open(&temp.path().join("link")).is_err());
    }
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod native_tests {
    use super::*;
    fn fixture() -> tempfile::TempDir {
        let root = std::env::var_os("QOS_ENDPOINT_FIXTURE")
            .expect("set explicitly selected disposable native fixture root");
        tempfile::tempdir_in(root).unwrap()
    }
    #[test]
    #[ignore = "controller-provided disposable native volume required"]
    fn native_identity_observation_and_session_invalidation() {
        let fixture = fixture();
        std::fs::write(fixture.path().join("hello"), b"hello").unwrap();
        let probe = probe_endpoint(fixture.path()).expect("native read-only probe");
        assert_eq!(probe.capabilities.safe_read, Capability::Available);
        let mut context = EndpointContext::default();
        context
            .register(probe.locator.clone(), probe.identity)
            .unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        let tree = observe(&endpoint, &AtomicBool::new(false));
        assert_eq!(tree.observation(), &Observation::Complete, "{tree:?}");
        let key = if cfg!(windows) {
            RelativePath::windows(&[104, 101, 108, 108, 111]).unwrap()
        } else {
            RelativePath::unix(b"hello").unwrap()
        };
        assert!(matches!(
            tree.entries().get(&key),
            Some(EntryState::File { bytes: 5, .. })
        ));
        assert_eq!(
            tree.entries().get(&key),
            Some(&EntryState::File {
                sha256: [
                    44, 242, 77, 186, 95, 176, 163, 14, 38, 232, 59, 42, 197, 185, 226, 158, 27,
                    22, 30, 92, 31, 167, 66, 94, 115, 4, 51, 98, 147, 139, 152, 36
                ],
                bytes: 5,
                executable: false,
            })
        );
        endpoint.invalidate();
        assert!(matches!(
            observe(&endpoint, &AtomicBool::new(false)).observation(),
            Observation::Unavailable(PendingReason::IdentityChanged)
        ));
        let mut restarted = EndpointContext::default();
        restarted.register(probe.locator, probe.identity).unwrap();
        assert!(
            restarted
                .open_endpoint(&probe.identity, Access::Read)
                .is_ok()
        );
    }
    #[test]
    #[ignore = "controller-provided disposable native volume required"]
    fn native_cancelled_and_symlink_scopes_are_incomplete() {
        let fixture = fixture();
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing", fixture.path().join("link")).unwrap();
        let probe = probe_endpoint(fixture.path()).unwrap();
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        assert!(matches!(
            observe(&endpoint, &AtomicBool::new(true)).observation(),
            Observation::Incomplete(_)
        ));
        #[cfg(unix)]
        assert!(matches!(
            observe(&endpoint, &AtomicBool::new(false)).observation(),
            Observation::Incomplete(_)
        ));
    }
    #[test]
    #[ignore = "controller-provided disposable native volume required"]
    fn native_replaced_or_offline_root_never_becomes_empty_complete() {
        let fixture = fixture();
        let selected = fixture.path().join("selected");
        std::fs::create_dir(&selected).unwrap();
        let probe = probe_endpoint(&selected).unwrap();
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        std::fs::rename(&selected, fixture.path().join("old")).unwrap();
        assert!(matches!(
            observe(&endpoint, &AtomicBool::new(false)).observation(),
            Observation::Unavailable(_)
        ));
        std::fs::create_dir(&selected).unwrap();
        assert!(matches!(
            context.open_endpoint(&probe.identity, Access::Read),
            Err(EndpointError::IdentityChanged)
        ));
    }
}

/// Test requested write semantics inside a newly created private disposable
/// directory. Calling this explicitly means the user selected the connection.
/// The probe function never calls this operation.
///
/// # Errors
/// Refuses unsafe namespaces, failed identity checks and unsupported operations.
pub fn verify_selected_write(endpoint: &PinnedEndpoint) -> Result<Capabilities, EndpointError> {
    endpoint.revalidate()?;
    if endpoint.evidence.capabilities.safe_write == Capability::Refused(EndpointError::ReadOnly) {
        return Err(EndpointError::ReadOnly);
    }
    validate_write_namespace(&endpoint.root)?;
    let name = format!(
        ".qos-endpoint-probe-{}-{}",
        std::process::id(),
        NEXT_PROBE.fetch_add(1, Ordering::Relaxed)
    );
    // Exclusive creation with private permissions at creation time. Never adopt
    // or clean an existing name. Refusal may leave our private diagnostic files.
    destination_security::create_directory(&endpoint.root.file, std::ffi::OsStr::new(&name))?;
    let private = endpoint.root.child(std::ffi::OsStr::new(&name))?;
    destination_security::check_private_file(&private.file)?;
    let directory = Dir::from_std_file(readable_directory(&private.file)?);
    let guard = || -> Result<(), EndpointError> {
        endpoint.revalidate()?;
        private.validate()?;
        validate_write_namespace(&private)
    };
    for (name, bytes) in [
        ("live", b"old".as_slice()),
        ("staged", b"new".as_slice()),
        ("replace", b"replacement".as_slice()),
    ] {
        guard()?;
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
        let mut file = cap_primitives::fs::open(&private.file, Path::new(name), &options)?;
        destination_security::check_private_file(&file)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    sync_trial_directory(&private.file)?;
    let old = directory.open("live")?;
    guard()?;
    directory.rename("live", &directory, "recovered")?;
    sync_trial_directory(&private.file)?;
    guard()?;
    directory.rename("staged", &directory, "live")?;
    sync_trial_directory(&private.file)?;
    assert_trial_bytes(old.into_std(), b"old")?;
    assert_trial_bytes(directory.open("recovered")?.into_std(), b"old")?;
    let before_replace = directory.open("live")?;
    guard()?;
    directory.rename("replace", &directory, "live")?;
    sync_trial_directory(&private.file)?;
    assert_trial_bytes(before_replace.into_std(), b"new")?;
    assert_trial_bytes(directory.open("live")?.into_std(), b"replacement")?;
    // Cleanup is descriptor-relative and only names created exclusively above.
    for name in ["live", "recovered"] {
        guard()?;
        directory.remove_file(name)?;
    }
    sync_trial_directory(&private.file)?;
    guard()?;
    let root_directory = Dir::from_std_file(readable_directory(&endpoint.root.file)?);
    root_directory.remove_dir(&name)?;
    sync_trial_directory(&endpoint.root.file)?;
    endpoint.revalidate()?;
    endpoint.write_verified.store(true, Ordering::Release);
    Ok(endpoint.capabilities())
}
fn assert_trial_bytes(file: File, expected: &[u8]) -> Result<(), EndpointError> {
    let mut observed = Vec::new();
    file.take(32).read_to_end(&mut observed)?;
    if observed == expected {
        Ok(())
    } else {
        Err(EndpointError::Unstable)
    }
}
fn sync_trial_directory(file: &File) -> Result<(), EndpointError> {
    #[cfg(not(windows))]
    {
        readable_directory(file)?.sync_all()?;
    }
    // Windows has no reviewed directory-flush proof. File flush/rename trials
    // can establish read/write semantics but never durable_transactions.
    #[cfg(windows)]
    if !metadata(file)?.is_dir() {
        return Err(EndpointError::IdentityChanged);
    }
    Ok(())
}
fn validate_write_namespace(pin: &Pin) -> Result<(), EndpointError> {
    pin.validate()?;
    let mut current = pin;
    let mut child = None;
    loop {
        destination_security::check_directory(&current.file, child.as_ref(), false)?;
        child = Some(metadata(&current.file)?);
        let Some((parent, _)) = &current.parent else {
            break;
        };
        current = parent;
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod write_native_tests {
    use super::*;
    #[test]
    #[ignore = "explicitly selected disposable volume; creates only private probe files"]
    fn native_private_write_replace_recovery_and_persistence() {
        let root = std::env::var_os("QOS_ENDPOINT_FIXTURE").unwrap();
        let fixture = tempfile::tempdir_in(root).unwrap();
        let probe = probe_endpoint(fixture.path()).unwrap();
        assert!(
            std::fs::read_dir(fixture.path()).unwrap().next().is_none(),
            "probe must be read-only"
        );
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        let capabilities = verify_selected_write(&endpoint).unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(capabilities.safe_write, Capability::Available);
        #[cfg(windows)]
        assert_eq!(
            capabilities.safe_write,
            Capability::Refused(EndpointError::Unverified)
        );
        #[cfg(target_os = "linux")]
        assert_eq!(capabilities.durable_transactions, Capability::Available);
        #[cfg(windows)]
        assert_eq!(
            capabilities.durable_transactions,
            Capability::Refused(EndpointError::Unverified)
        );
        assert!(
            std::fs::read_dir(fixture.path()).unwrap().next().is_none(),
            "private trial cleans only its own entries"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_fixture_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn open(path: &Path) -> PinnedEndpoint {
        let probe = probe_endpoint(path).unwrap();
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap()
    }
    #[test]
    #[ignore = "ordinary-user disposable native volume required"]
    fn native_permission_denial_and_custom_xattrs_are_not_absence() {
        let root = std::env::var_os("QOS_ENDPOINT_FIXTURE").unwrap();
        let fixture = tempfile::tempdir_in(root).unwrap();
        let denied = fixture.path().join("denied");
        std::fs::create_dir(&denied).unwrap();
        std::fs::write(denied.join("private"), b"secret").unwrap();
        let endpoint = open(fixture.path());
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0)).unwrap();
        let result = observe(&endpoint, &AtomicBool::new(false));
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            matches!(result.observation(), Observation::Incomplete(issues) if issues.iter().any(|i| i.reason == PendingReason::PermissionDenied))
        );
        let file = File::open(denied.join("private")).unwrap();
        use std::os::fd::AsRawFd;
        // SAFETY: selected private fixture file, valid fixed C name and bytes.
        assert_eq!(
            unsafe {
                libc::fsetxattr(
                    file.as_raw_fd(),
                    c"user.qos-fixture".as_ptr(),
                    b"yes".as_ptr().cast(),
                    3,
                    0,
                )
            },
            0
        );
        let result = observe(&endpoint, &AtomicBool::new(false));
        assert!(
            matches!(result.observation(), Observation::Incomplete(issues) if issues.iter().any(|i| i.reason == PendingReason::Unsupported))
        );
    }
    #[test]
    #[ignore = "controller mounts a disposable volume read-only before test"]
    fn native_readonly_mount_preserves_safe_read_without_write() {
        let path = PathBuf::from(std::env::var_os("QOS_ENDPOINT_READONLY").unwrap());
        let probe = probe_endpoint(&path).unwrap();
        assert_eq!(probe.capabilities.safe_read, Capability::Available);
        assert_eq!(
            probe.capabilities.safe_write,
            Capability::Refused(EndpointError::ReadOnly)
        );
        assert_eq!(
            verify_selected_write(&open(&path)),
            Err(EndpointError::ReadOnly)
        );
    }
    #[test]
    #[ignore = "controller mounts two independently backed clones of one filesystem UUID"]
    fn native_cloned_filesystem_requires_reselection() {
        let path = PathBuf::from(std::env::var_os("QOS_ENDPOINT_CLONE_MAIN").unwrap());
        assert!(matches!(
            probe_endpoint(&path),
            Err(EndpointError::AmbiguousIdentity)
        ));
    }
    #[test]
    #[ignore = "controller coordinates only disposable-volume unmount/remount via external barriers"]
    fn native_detach_remount_invalidates_old_session_and_preserves_identity() {
        let root = std::env::var_os("QOS_ENDPOINT_FIXTURE").unwrap();
        let barriers = PathBuf::from(std::env::var_os("QOS_ENDPOINT_BARRIERS").unwrap());
        let fixture = tempfile::tempdir_in(root).unwrap();
        std::fs::write(fixture.path().join("retained"), b"retained").unwrap();
        let probe = probe_endpoint(fixture.path()).unwrap();
        let endpoint = open(fixture.path());
        let wait = |name: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            while !barriers.join(name).exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "controller barrier timeout: {name}"
                );
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        };
        std::fs::write(barriers.join("ready-unmount"), b"ready").unwrap();
        wait("unmounted");
        assert!(matches!(
            observe(&endpoint, &AtomicBool::new(false)).observation(),
            Observation::Unavailable(_)
        ));
        std::fs::write(barriers.join("ready-remount"), b"ready").unwrap();
        wait("remounted");
        let restored = probe_endpoint(fixture.path()).unwrap();
        assert_eq!(restored.identity, probe.identity);
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let fresh = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        assert_eq!(
            observe(&fresh, &AtomicBool::new(false)).observation(),
            &Observation::Complete
        );
        assert!(matches!(
            observe(&endpoint, &AtomicBool::new(false)).observation(),
            Observation::Unavailable(_)
        ));
    }
}

#[cfg(all(test, windows))]
mod windows_fixture_tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    #[test]
    #[ignore = "disposable native Windows volume with accessible clean-volume evidence required"]
    fn native_windows_locked_file_and_offline_placeholder_never_complete() {
        let fixture =
            tempfile::tempdir_in(std::env::var_os("QOS_ENDPOINT_FIXTURE").unwrap()).unwrap();
        let path = fixture.path().join("file");
        std::fs::write(&path, b"private").unwrap();
        let probe = probe_endpoint(fixture.path()).unwrap();
        let mut context = EndpointContext::default();
        context.register(probe.locator, probe.identity).unwrap();
        let endpoint = context
            .open_endpoint(&probe.identity, Access::Read)
            .unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        assert!(matches!(
            observe(&endpoint, &AtomicBool::new(false)).observation(),
            Observation::Incomplete(_)
        ));
        drop(locked);
        let path: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: valid terminated path to our disposable file; only OFFLINE bit.
        assert_ne!(
            unsafe {
                windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(path.as_ptr(), 0x1000)
            },
            0
        );
        let tree = observe(&endpoint, &AtomicBool::new(false));
        // SAFETY: restore only the selected disposable file's attributes.
        unsafe {
            windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(path.as_ptr(), 0x80);
        }
        assert!(matches!(tree.observation(), Observation::Incomplete(_)));
        assert!(tree.entries().is_empty());
    }
    #[test]
    #[ignore = "controller-provided disposable reparse/junction path; never follows or hydrates it"]
    fn native_windows_reparse_selection_is_refused() {
        let path = PathBuf::from(std::env::var_os("QOS_ENDPOINT_REPARSE").unwrap());
        assert!(probe_endpoint(&path).is_err());
    }
}

// Narrow retained-handle bridge for the private Connected native executor. A
// caller cannot manufacture this capability from a path or serialized identity.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(super) struct NativeDirectory(Arc<Pin>);
#[cfg(target_os = "linux")]
impl NativeDirectory {
    pub(super) fn file(&self) -> &File {
        &self.0.file
    }
    pub(super) fn validate(&self, writable: bool) -> Result<(), EndpointError> {
        self.0.validate()?;
        if writable {
            validate_write_namespace(&self.0)?;
        }
        native::check_metadata(&readable_directory(&self.0.file)?, true)?;
        Ok(())
    }
    pub(super) fn child(
        &self,
        name: &std::ffi::OsStr,
        writable: bool,
    ) -> Result<Self, EndpointError> {
        let child = Self(self.0.child(name)?);
        native::same_mount(&self.0.file, &child.0.file)?;
        child.validate(writable)?;
        Ok(child)
    }
}
#[cfg(target_os = "linux")]
impl PinnedEndpoint {
    pub(super) fn native_directory(
        &self,
        writable: bool,
    ) -> Result<NativeDirectory, EndpointError> {
        if writable {
            self.revalidate_for_write()?;
        } else {
            self.revalidate()?;
        }
        let directory = NativeDirectory(Arc::clone(&self.root));
        directory.validate(writable)?;
        Ok(directory)
    }
}
/// Metadata only; the Directory owner retains and validates its own name edges.
#[cfg(target_os = "linux")]
pub(super) fn native_directory_metadata(file: &File) -> Result<(), EndpointError> {
    native::check_metadata(file, true).map(|_| ())
}
#[cfg(target_os = "linux")]
pub(super) fn native_regular_metadata(file: &File) -> Result<bool, EndpointError> {
    native::check_metadata(file, false)
}
#[cfg(target_os = "linux")]
pub(super) fn native_same_mount(a: &File, b: &File) -> Result<(), EndpointError> {
    native::same_mount(a, b)
}
#[cfg(target_os = "linux")]
pub(super) fn native_object_generation(file: &File) -> Result<Vec<u8>, EndpointError> {
    native::object_generation(file)
}

#[cfg(windows)]
pub(super) use native::{
    connected_metadata, connected_root_binding, connected_volume_binding, connected_volume_path,
};
