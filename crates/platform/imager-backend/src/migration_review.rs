//! Bounded metadata inventory binding a review to the copy engine's snapshots.
//! This is change detection, not a content snapshot or new copy authority.
use super::migration::{PinnedDirectory, changed};
use super::migration_journal::Snapshot;
use super::migration_store::NativePath;
use super::nofollow::metadata_nofollow;
use cap_primitives::fs::Metadata;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

/// Socket inodes are live IPC endpoints, not persistent user file content.
/// Never classify by a filename or directory: keys/config next to a socket
/// remain ordinary transferable data.
pub fn runtime_socket(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use cap_primitives::fs::FileTypeExt;
        metadata.file_type().is_socket()
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

#[derive(Debug)]
struct ReviewedSourceChanged(&'static str);
impl std::fmt::Display for ReviewedSourceChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for ReviewedSourceChanged {}
pub fn source_changed(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|error| error.is::<ReviewedSourceChanged>())
}
fn changed_source(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, ReviewedSourceChanged(message))
}

/// A nested mount needs an independently approved source and transfer mapping.
/// Keep this distinct from metadata incompatibility and source substitution.
#[derive(Debug)]
struct NestedHomeMount(#[cfg(target_os = "linux")] Option<Arc<MountedHomeLocations>>);
impl std::fmt::Display for NestedHomeMount {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Whole-home transfer across mounted entries is not supported")
    }
}
impl std::error::Error for NestedHomeMount {}
pub fn nested_home_mount(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|error| error.is::<NestedHomeMount>())
}
fn unsupported_home_mount() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        NestedHomeMount(
            #[cfg(target_os = "linux")]
            None,
        ),
    )
}

#[cfg(target_os = "linux")]
mod mounted_locations;
#[cfg(all(target_os = "linux", feature = "test-support"))]
pub use mounted_locations::DiscoveryObserver;
#[cfg(target_os = "linux")]
pub use mounted_locations::MountedHomeLocations;
#[cfg(target_os = "linux")]
pub use mounted_locations::root_set::{
    GrantedHomeRoots, GrantedRoot, HomeRootSet, MountedRootRequest, MountedRootSelection, RootKind,
};
#[cfg(target_os = "linux")]
pub fn mounted_home_locations(error: &io::Error) -> Option<Arc<MountedHomeLocations>> {
    error
        .get_ref()?
        .downcast_ref::<NestedHomeMount>()?
        .0
        .clone()
}

#[cfg(target_os = "linux")]
mod capture_custody;
#[cfg(target_os = "linux")]
pub use capture_custody::CaptureCustody;
#[cfg(target_os = "linux")]
mod original_custody;
#[cfg(target_os = "linux")]
pub use original_custody::{OriginalOmission, OriginalPreparation};

pub mod storage;
#[cfg(target_os = "linux")]
pub use storage::scratch::InventoryScratch;

/// Retained preparation authority, rechecked whenever working storage is used.
pub trait InventoryScratchCustody: Send + Sync {
    fn check(&self) -> io::Result<()>;
}

/// Working storage cannot be constructed on hosts without native admission.
#[cfg(not(target_os = "linux"))]
pub struct InventoryScratch {
    _never: std::convert::Infallible,
}
const MAX_ENTRIES: usize = 100_000;

#[derive(PartialEq, Eq, Serialize, Deserialize)]
struct Fact {
    kind: u8,
    device: u64,
    snapshot: Option<Snapshot>,
    #[cfg(unix)]
    #[serde(skip_serializing_if = "Option::is_none")]
    stable: Option<(u64, u32, u32, u32)>,
}
impl Fact {
    fn capture(metadata: &Metadata) -> io::Result<Self> {
        use cap_fs_ext::MetadataExt;
        Ok(Self {
            kind: if metadata.is_symlink() {
                0
            } else if metadata.is_dir() {
                1
            } else if metadata.is_file() {
                2
            } else if runtime_socket(metadata) {
                4
            } else {
                3
            },
            device: metadata.dev(),
            snapshot: Some(Snapshot::new(metadata)?),
            #[cfg(unix)]
            stable: None,
        })
    }
}

pub struct SourceInventory {
    facts: storage::Storage,
    same_device: Option<u64>,
    #[cfg(target_os = "linux")]
    same_mount: Option<u64>,
    #[cfg(target_os = "linux")]
    projected_roots: Option<Arc<launcher::owned_service::VerifiedOriginalMountedRoots>>,
    max_entries: usize,
    #[cfg(target_os = "linux")]
    custody: Option<Arc<InstallerCustody>>,
    #[cfg(target_os = "linux")]
    original_custody: Vec<Arc<original_custody::OriginalCustody>>,
    /// Nested, root-relative subtrees the person excluded from this review.
    exclusions: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct InventorySummary {
    pub files: u64,
    pub bytes: u64,
    pub links: u64,
    pub special_files: u64,
    #[serde(default)]
    pub runtime_sockets: u64,
}
impl SourceInventory {
    /// Reviewed entries as path, entry kind and logical size. Crate-internal:
    /// a source review derives its labels and counts from the same facts the
    /// copy engine enforces, instead of walking the source again.
    pub(crate) fn entries(
        &self,
    ) -> impl Iterator<Item = io::Result<(PathBuf, u8, Option<u64>)>> + '_ {
        self.facts.iter().map(|item| {
            item.map(|(path, fact)| (path, fact.kind, fact.snapshot.as_ref().map(Snapshot::size)))
        })
    }

    #[cfg(target_os = "linux")]
    pub fn regular_files(&self) -> impl Iterator<Item = io::Result<PathBuf>> + '_ {
        self.paths_of_kind(2)
    }
    fn paths_of_kind(&self, kind: u8) -> impl Iterator<Item = io::Result<PathBuf>> + '_ {
        self.facts.iter().filter_map(move |item| match item {
            Ok((path, fact)) if fact.kind == kind => Some(Ok(path)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
    }
    pub fn entry_count(&self) -> usize {
        self.facts.len()
    }
    pub fn summary(&self) -> io::Result<InventorySummary> {
        let mut summary = InventorySummary::default();
        for item in self.facts.iter() {
            let (_, fact) = item?;
            match fact.kind {
                0 => summary.links += 1,
                2 => {
                    summary.files += 1;
                    summary.bytes = summary
                        .bytes
                        .checked_add(
                            fact.snapshot
                                .as_ref()
                                .ok_or_else(|| changed("File snapshot missing"))?
                                .size(),
                        )
                        .ok_or_else(|| changed("source byte count overflow"))?;
                }
                3 => summary.special_files += 1,
                4 => summary.runtime_sockets += 1,
                _ => {}
            }
        }
        Ok(summary)
    }
    pub fn capture(root: &Arc<PinnedDirectory>) -> io::Result<Self> {
        let mut inventory = Self {
            facts: storage::Storage::new(),
            same_device: None,
            #[cfg(target_os = "linux")]
            same_mount: None,
            #[cfg(target_os = "linux")]
            projected_roots: None,
            max_entries: MAX_ENTRIES,
            #[cfg(target_os = "linux")]
            custody: None,
            #[cfg(target_os = "linux")]
            original_custody: Vec::new(),
            exclusions: Vec::new(),
        };
        inventory.walk(root, Path::new(""), 0)?;
        Ok(inventory)
    }

    /// Whole-home review never descends through bind mounts or another volume.
    /// Such a grant needs an explicit separate transfer, not ambient traversal.
    pub fn capture_home(root: &Arc<PinnedDirectory>) -> io::Result<Self> {
        use cap_fs_ext::MetadataExt;
        let mut inventory = Self {
            facts: storage::Storage::new(),
            same_device: Some(root.identity.dev()),
            #[cfg(target_os = "linux")]
            same_mount: Some(fs_native::snapshot_lease::mount_id(&root.file)?),
            #[cfg(target_os = "linux")]
            projected_roots: None,
            max_entries: 1_000_000,
            #[cfg(target_os = "linux")]
            custody: None,
            #[cfg(target_os = "linux")]
            original_custody: Vec::new(),
            exclusions: Vec::new(),
        };
        inventory.walk(root, Path::new(""), 0)?;
        Ok(inventory)
    }

    /// The only permitted portable omission is the installer's own held,
    /// private preparation namespace. No caller supplies a traversal filter.
    #[cfg(target_os = "linux")]
    pub fn capture_portable_home(root: &Arc<PinnedDirectory>, home: &Path) -> io::Result<Self> {
        let mut inventory = Self::capture_home_policy(root, InstallerCustody::review(home)?)?;
        inventory.walk(root, Path::new(""), 0)?;
        Ok(inventory)
    }
    #[cfg(target_os = "linux")]
    pub fn capture_portable_home_with_storage(
        root: &Arc<PinnedDirectory>,
        home: &Path,
        scratch: Arc<InventoryScratch>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let custody = InstallerCustody::review(home)?;
        if let Err(disjoint) = scratch.validate_source(root) {
            let admitted = custody.as_ref().ok_or(disjoint)?;
            admitted.validate()?;
            scratch.validate_excluded_source(root, &admitted.pin)?;
        }
        let mut inventory = Self::capture_home_policy(root, custody)?;
        inventory.facts = storage::Storage::with_scratch(scratch);
        inventory.walk_with_cancel(root, Path::new(""), 0, cancel)?;
        inventory.facts.finish(cancel)?;
        Ok(inventory)
    }
    #[cfg(target_os = "linux")]
    pub fn working_storage(&self) -> Option<Arc<InventoryScratch>> {
        self.facts.working_storage()
    }
    #[cfg(target_os = "linux")]
    fn capture_home_policy(
        root: &Arc<PinnedDirectory>,
        custody: Option<Arc<InstallerCustody>>,
    ) -> io::Result<Self> {
        use cap_fs_ext::MetadataExt;
        Ok(Self {
            facts: storage::Storage::new(),
            same_device: Some(root.identity.dev()),
            same_mount: Some(fs_native::snapshot_lease::mount_id(&root.file)?),
            projected_roots: None,
            max_entries: 1_000_000,
            custody,
            original_custody: Vec::new(),
            exclusions: Vec::new(),
        })
    }
    pub fn excluded(&self, path: &Path) -> io::Result<bool> {
        if self
            .exclusions
            .iter()
            .any(|excluded| path.starts_with(excluded))
        {
            return Ok(true);
        }
        #[cfg(target_os = "linux")]
        if let Some(custody) = &self.custody {
            custody.validate()?;
            return Ok(path == custody.relative);
        }
        #[cfg(target_os = "linux")]
        for custody in &self.original_custody {
            custody.validate()?;
            if path == custody.relative {
                return Ok(true);
            }
        }
        let _ = path;
        Ok(false)
    }
    pub fn mutable_directory(&self, path: &Path) -> bool {
        #[cfg(target_os = "linux")]
        if let Some(custody) = &self.custody {
            return custody.mutable_ancestors
                && path != custody.relative
                && custody.relative.starts_with(path);
        }
        let _ = path;
        false
    }
    pub fn excluded_paths(&self) -> Vec<&Path> {
        #[cfg(target_os = "linux")]
        let custody = self
            .custody
            .iter()
            .map(|c| c.relative.as_path())
            .chain(self.original_custody.iter().map(|c| c.relative.as_path()));
        #[cfg(not(target_os = "linux"))]
        let custody = std::iter::empty::<&Path>();
        custody
            .chain(self.exclusions.iter().map(PathBuf::as_path))
            .collect()
    }

    /// Apply nested, root-relative exclusions from an explicit source selection.
    /// Shape is checked here; existence is checked against the pinned source by
    /// the caller, so a stale selection refuses instead of silently widening.
    ///
    /// # Errors
    /// Refuses unbounded or non-relative exclusion paths.
    pub fn set_exclusions(&mut self, exclusions: &[PathBuf]) -> io::Result<()> {
        crate::setup::source_tree::exclusion_paths(
            &exclusions
                .iter()
                .map(|path| NativePath::encode(path))
                .collect::<Vec<_>>(),
        )?;
        self.exclusions = exclusions.to_vec();
        Ok(())
    }

    pub fn exclusions(&self) -> &[PathBuf] {
        &self.exclusions
    }

    /// Re-walk this review with nested exclusions applied. The walk is the same
    /// canonical producer, so every downstream consumer (archive, verification,
    /// totals and the installed import) sees one consistent inventory.
    ///
    /// # Errors
    /// Refuses exclusions that are missing, linked or outside the source.
    pub(crate) fn with_exclusions(
        mut self,
        root: &Arc<PinnedDirectory>,
        exclusions: &[PathBuf],
    ) -> io::Result<Self> {
        if exclusions.is_empty() {
            return Ok(self);
        }
        crate::setup::source_tree::validate_exclusions_under(&root.file, exclusions)?;
        self.set_exclusions(exclusions)?;
        self.facts = self.facts.empty_like();
        self.walk(root, Path::new(""), 0)?;
        Ok(self)
    }
    fn fact(&self, path: &Path, metadata: &Metadata) -> io::Result<Fact> {
        let mut fact = Fact::capture(metadata)?;
        if metadata.is_dir() && self.mutable_directory(path) {
            fact.snapshot = None;
            #[cfg(unix)]
            {
                use cap_fs_ext::MetadataExt;
                fact.stable = Some((
                    metadata.ino(),
                    cap_primitives::fs::MetadataExt::mode(metadata),
                    cap_primitives::fs::MetadataExt::uid(metadata),
                    cap_primitives::fs::MetadataExt::gid(metadata),
                ));
            }
        }
        Ok(fact)
    }

    fn walk(
        &mut self,
        directory: &Arc<PinnedDirectory>,
        relative: &Path,
        depth: usize,
    ) -> io::Result<()> {
        let cancel = AtomicBool::new(false);
        self.walk_with_cancel(directory, relative, depth, &cancel)?;
        self.facts.finish(&cancel)
    }
    fn walk_with_cancel(
        &mut self,
        directory: &Arc<PinnedDirectory>,
        relative: &Path,
        depth: usize,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        super::migration::check_cancel(cancel)?;
        directory.validate()?;
        #[cfg(target_os = "linux")]
        if let Some(expected) = self.same_mount.filter(|_| self.projected_roots.is_none()) {
            if fs_native::snapshot_lease::mount_id(&directory.file)? != expected {
                return Err(changed_source(
                    "Reviewed Home mount changed; review the folder again.",
                ));
            }
        }
        #[cfg(target_os = "linux")]
        self.check_projected_root(relative, &directory.file)?;
        let before = Metadata::from_file(&directory.file)?;
        self.insert(relative, &before, cancel)?;
        // Read the listing before descending so each level keeps only its
        // pinned directory open. Holding the listing's descriptors across the
        // recursion exhausts macOS's default limit of 256 well before the
        // depth limit below can refuse the selection.
        let names = cap_std::fs::Dir::from_std_file(directory.file.try_clone()?)
            .entries()?
            // One more than the inventory admits, so an oversized folder is
            // refused by `insert` without buffering its whole listing.
            .take(MAX_ENTRIES + 1)
            .map(|entry| {
                super::migration::check_cancel(cancel)?;
                entry.map(|entry| entry.file_name())
            })
            .collect::<io::Result<Vec<_>>>()?;
        for name in names {
            super::migration::check_cancel(cancel)?;
            let path = relative.join(&name);
            if self.excluded(&path)? {
                continue;
            }
            let metadata = metadata_nofollow(&directory.file, Path::new(&name))?;
            #[cfg(target_os = "linux")]
            if let Some(expected) = self.same_mount.filter(|_| self.projected_roots.is_none()) {
                check_entry_mount(directory, &name, &metadata, expected)?;
            }
            #[cfg(target_os = "linux")]
            if self.projected_roots.is_some() {
                let held = open_mount_entry(directory, &name, &metadata)?;
                self.check_projected_root(&path, &held)?;
            }
            if metadata.is_dir() && !metadata.is_symlink() {
                use cap_fs_ext::MetadataExt;
                if self
                    .same_device
                    .filter(|_| {
                        #[cfg(target_os = "linux")]
                        {
                            self.projected_roots.is_none()
                        }
                        #[cfg(not(target_os = "linux"))]
                        {
                            true
                        }
                    })
                    .is_some_and(|device| device != metadata.dev())
                {
                    return Err(unsupported_home_mount());
                }
                if depth >= 128 {
                    return Err(changed(
                        "Source review exceeds depth 128; select a smaller folder.",
                    ));
                }
                self.walk_with_cancel(
                    &directory.open_child(&name, &metadata)?,
                    &path,
                    depth + 1,
                    cancel,
                )?;
            } else {
                self.insert(&path, &metadata, cancel)?;
            }
        }
        directory.validate()?;
        #[cfg(target_os = "linux")]
        self.check_projected_root(relative, &directory.file)?;
        if self.fact(relative, &before)?
            != self.fact(relative, &Metadata::from_file(&directory.file)?)?
        {
            return Err(changed_source(
                "Source changed during review; review the folder again.",
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn check_projected_root(&self, path: &Path, held: &std::fs::File) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let Some(roots) = &self.projected_roots else {
            return Ok(());
        };
        if path.as_os_str().is_empty() {
            roots.check()?;
        }
        let expected = roots
            .scope(path)
            .or_else(|| self.same_device.zip(self.same_mount))
            .ok_or_else(|| changed_source("Original source projection has no mount scope"))?;
        if held.metadata()?.dev() != expected.0
            || fs_native::snapshot_lease::mount_id(held)? != expected.1
        {
            return Err(unsupported_home_mount());
        }
        Ok(())
    }

    fn insert(&mut self, path: &Path, metadata: &Metadata, cancel: &AtomicBool) -> io::Result<()> {
        if self.facts.len() >= self.max_entries || path.as_os_str().len() > 4096 {
            return Err(changed(
                "Source review exceeds its entry bound or path length 4096; select a smaller folder.",
            ));
        }
        let fact = self.fact(path, metadata)?;
        self.facts.insert(path.to_owned(), fact, cancel)
    }

    pub fn revision(&self) -> io::Result<[u8; 32]> {
        self.revision_with_cancel(&AtomicBool::new(false))
    }
    fn revision_with_cancel(&self, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
        let mut digest = Sha256::new();
        for item in self.facts.iter() {
            super::migration::check_cancel(cancel)?;
            let (path, fact) = item?;
            let bytes = serde_json::to_vec(&(NativePath::encode(&path), fact))?;
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        Ok(digest.finalize().into())
    }

    pub fn check(&self, path: &Path, metadata: &Metadata) -> io::Result<()> {
        if self.facts.get(path)? != Some(self.fact(path, metadata)?) {
            return Err(changed_source(
                "Source contents changed after review; explicitly review the folder again.",
            ));
        }
        Ok(())
    }

    pub fn validate(&self, root: &Arc<PinnedDirectory>) -> io::Result<()> {
        self.validate_with_cancel(root, &AtomicBool::new(false))
    }
    pub fn validate_with_cancel(
        &self,
        root: &Arc<PinnedDirectory>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let mut current = Self {
            facts: self.facts.empty_like(),
            same_device: self.same_device,
            #[cfg(target_os = "linux")]
            same_mount: self.same_mount,
            #[cfg(target_os = "linux")]
            projected_roots: self.projected_roots.clone(),
            max_entries: self.max_entries,
            #[cfg(target_os = "linux")]
            custody: self.custody.clone(),
            #[cfg(target_os = "linux")]
            original_custody: self.original_custody.clone(),
            exclusions: self.exclusions.clone(),
        };
        current.walk_with_cancel(root, Path::new(""), 0, cancel)?;
        current.facts.finish(cancel)?;
        if current.revision_with_cancel(cancel)? != self.revision_with_cancel(cancel)? {
            return Err(changed_source(
                "Source contents changed after review; explicitly review the folder again.",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn open_mount_entry(
    parent: &PinnedDirectory,
    name: &std::ffi::OsStr,
    expected: &Metadata,
) -> io::Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };
    let name_c = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
    // SAFETY: retained parent and single entry name; O_PATH neither reads file
    // contents nor opens devices/FIFOs, and O_NOFOLLOW pins literal symlinks.
    let raw = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name_c.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned one new owned descriptor.
    let file = unsafe { std::fs::File::from_raw_fd(raw) };
    if Fact::capture(&Metadata::from_file(&file)?)? != Fact::capture(expected)? {
        return Err(changed_source("Source entry changed during mount review."));
    }
    if Fact::capture(&metadata_nofollow(&parent.file, Path::new(name))?)?
        != Fact::capture(expected)?
    {
        return Err(changed_source("Source entry changed during mount review."));
    }
    Ok(file)
}
#[cfg(target_os = "linux")]
fn check_entry_mount(
    parent: &PinnedDirectory,
    name: &std::ffi::OsStr,
    expected: &Metadata,
    mount: u64,
) -> io::Result<()> {
    let entry = open_mount_entry(parent, name, expected)?;
    if fs_native::snapshot_lease::mount_id(&entry)? != mount {
        return Err(unsupported_home_mount());
    }
    Ok(())
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    #[cfg(all(target_os = "linux", feature = "native-capture-qualification"))]
    #[test]
    fn live_inventory_working_storage_uses_only_exact_existing_installer_exclusion() {
        use std::os::unix::fs::PermissionsExt;
        let native = std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("explicitly owned supported native scratch filesystem required");
        let owned = tempfile::Builder::new()
            .prefix("live-inventory-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(native)
            .unwrap();
        let home_path = owned.path().join("home");
        std::fs::create_dir(&home_path).unwrap();
        std::fs::set_permissions(&home_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &home_path);
        environment.set("XDG_STATE_HOME", home_path.join(".local/state"));
        let namespace = crate::session::preparation_namespace().unwrap();
        let home = PinnedDirectory::open_absolute(&home_path).unwrap();
        let mut custody = home.clone();
        for component in namespace.strip_prefix(&home_path).unwrap().components() {
            let std::path::Component::Normal(name) = component else {
                panic!("canonical preparation namespace must be beneath fixture home");
            };
            custody = custody.create_child(name).unwrap();
        }
        let scratch_pin = custody
            .create_child(std::ffi::OsStr::new("working-storage"))
            .unwrap();
        let scratch = InventoryScratch::review(scratch_pin, 32 * 1024 * 1024).unwrap();
        let document = home_path.join("document");
        std::fs::write(&document, b"ordinary original document").unwrap();
        let cancel = AtomicBool::new(false);
        assert!(scratch.validate_source(&home).is_err());
        let unrelated = home
            .create_child(std::ffi::OsStr::new("user-documents"))
            .unwrap();
        let unadmitted = InventoryScratch::review(unrelated, 32 * 1024 * 1024).unwrap();
        assert!(
            SourceInventory::capture_portable_home_with_storage(
                &home, &home_path, unadmitted, &cancel
            )
            .is_err()
        );
        let inventory = SourceInventory::capture_portable_home_with_storage(
            &home, &home_path, scratch, &cancel,
        )
        .unwrap();
        assert_eq!(inventory.summary().unwrap().files, 1);
        inventory.validate_with_cancel(&home, &cancel).unwrap();
        std::fs::write(&document, b"changed original document").unwrap();
        assert!(inventory.validate_with_cancel(&home, &cancel).is_err());
        assert_eq!(
            std::fs::read(document).unwrap(),
            b"changed original document"
        );
    }
    #[test]
    fn review_depth_limit_refuses_without_creating_copy_output() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut path = root.clone();
        for _ in 0..129 {
            path.push("d");
            std::fs::create_dir(&path).unwrap();
        }
        let error = match SourceInventory::capture(&PinnedDirectory::open_absolute(&root).unwrap())
        {
            Ok(_) => panic!("deep inventory must require a smaller selection"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("select a smaller folder"),
            "unexpected refusal: {error}"
        );
    }
}

/// Exact installer namespace grant, unavailable for arbitrary exclusions.
#[cfg(target_os = "linux")]
struct InstallerCustody {
    mutable_ancestors: bool,
    pin: Arc<PinnedDirectory>,
    relative: PathBuf,
    stable: (u64, u32, u32, u32),
}
#[cfg(target_os = "linux")]
impl InstallerCustody {
    fn review(home: &Path) -> io::Result<Option<Arc<Self>>> {
        use cap_fs_ext::MetadataExt;
        let Some(namespace) = crate::session::preparation_namespace() else {
            return Err(changed("Installer preparation namespace unavailable"));
        };
        let Ok(relative) = namespace.strip_prefix(home) else {
            return Ok(None);
        };
        if relative.as_os_str().is_empty() {
            return Err(changed("Home cannot be installer custody"));
        }
        match std::fs::symlink_metadata(&namespace) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
            Ok(_) => {}
        }
        let pin = PinnedDirectory::open_absolute(&namespace)?;
        super::destination_security::check_private_file(&pin.file)?;
        let m = Metadata::from_file(&pin.file)?;
        Ok(Some(Arc::new(Self {
            mutable_ancestors: true,
            pin,
            relative: relative.to_owned(),
            stable: (
                m.ino(),
                cap_primitives::fs::MetadataExt::mode(&m),
                cap_primitives::fs::MetadataExt::uid(&m),
                cap_primitives::fs::MetadataExt::gid(&m),
            ),
        })))
    }
    fn validate(&self) -> io::Result<()> {
        use cap_fs_ext::MetadataExt;
        self.pin.validate()?;
        super::destination_security::check_private_file(&self.pin.file)?;
        let m = Metadata::from_file(&self.pin.file)?;
        if (
            m.ino(),
            cap_primitives::fs::MetadataExt::mode(&m),
            cap_primitives::fs::MetadataExt::uid(&m),
            cap_primitives::fs::MetadataExt::gid(&m),
        ) != self.stable
        {
            return Err(changed("Installer preparation namespace changed"));
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
pub use mounted_locations::root_set::TransferredHomeRoots;
