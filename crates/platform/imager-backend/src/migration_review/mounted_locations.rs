//! Metadata-only candidates beneath an approved Home. A candidate is not a grant.
use super::*;
use std::{ffi::OsString, fs::File};

pub mod root_set;

const MAX_LOCATIONS: usize = 256;
const MAX_VISITED: usize = 1_000_000;
struct Candidate {
    relative: PathBuf,
    parent: Arc<PinnedDirectory>,
    name: OsString,
    entry: File,
    fact: Fact,
    mount: u64,
}
struct Directory {
    pin: Arc<PinnedDirectory>,
    fact: Fact,
    mount: u64,
}
/// Pins only entries and their granted ancestry. Never opens mounted contents.
pub struct MountedHomeLocations {
    root: Arc<PinnedDirectory>,
    home: PathBuf,
    directories: Vec<Directory>,
    candidates: Vec<Candidate>,
    policy: SourceInventory,
}
impl std::fmt::Debug for MountedHomeLocations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MountedHomeLocations")
            .field("locations", &self.display_names())
            .finish()
    }
}
impl MountedHomeLocations {
    pub fn discover(
        root: &Arc<PinnedDirectory>,
        home: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        let policy = SourceInventory::capture_home_policy(root, InstallerCustody::review(home)?)?;
        Self::discover_with_policy(root, home, cancel, policy)
    }
    pub fn discover_original(
        root: &Arc<PinnedDirectory>,
        home: &Path,
        values: &[OriginalOmission],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        let policy = SourceInventory::original_home_policy(root, values)?;
        Self::discover_with_policy(root, home, cancel, policy)
    }
    fn discover_with_policy(
        root: &Arc<PinnedDirectory>,
        home: &Path,
        cancel: &AtomicBool,
        policy: SourceInventory,
    ) -> io::Result<Arc<Self>> {
        super::super::migration::check_cancel(cancel)?;
        root.validate()?;
        let mut result = Self {
            root: root.clone(),
            home: home.to_path_buf(),
            directories: Vec::new(),
            candidates: Vec::new(),
            policy,
        };
        let mount = fs_native::snapshot_lease::mount_id(&root.file)?;
        let mut visited = 0;
        result.walk(root, Path::new(""), mount, &mut visited, cancel)?;
        result.validate(cancel)?;
        result
            .candidates
            .sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(Arc::new(result))
    }
    fn walk(
        &mut self,
        directory: &Arc<PinnedDirectory>,
        relative: &Path,
        mount: u64,
        visited: &mut usize,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        super::super::migration::check_cancel(cancel)?;
        directory.validate()?;
        if fs_native::snapshot_lease::mount_id(&directory.file)? != mount {
            return Err(changed_source(
                "Approved Home mount changed before directory discovery",
            ));
        }
        if relative.components().count() > 128 {
            return Err(changed("Mounted Home discovery exceeds depth bound"));
        }
        let fact = Fact::capture(&Metadata::from_file(&directory.file)?)?;
        let candidate_start = self.candidates.len();
        let opened = cap_std::fs::Dir::from_std_file(directory.file.try_clone()?);
        for entry in opened.entries()? {
            super::super::migration::check_cancel(cancel)?;
            *visited += 1;
            if *visited > MAX_VISITED {
                return Err(changed("Mounted Home discovery exceeds entry bound"));
            }
            let name = entry?.file_name();
            let path = relative.join(&name);
            if self.policy.excluded(&path)? {
                continue;
            }
            let metadata = metadata_nofollow(&directory.file, Path::new(&name))?;
            let held = open_mount_entry(directory, &name, &metadata)?;
            let observed = fs_native::snapshot_lease::mount_id(&held)?;
            if observed != mount {
                if self.candidates.len() == MAX_LOCATIONS {
                    return Err(changed("Mounted Home discovery exceeds location bound"));
                }
                self.candidates.push(Candidate {
                    relative: path,
                    parent: directory.clone(),
                    name,
                    entry: held,
                    fact: Fact::capture(&metadata)?,
                    mount: observed,
                });
                // O_PATH is the only access to the mounted entry. No readdir,
                // read, open_child or recursive scan occurs beyond this fence.
            } else if metadata.is_dir() && !metadata.is_symlink() {
                #[cfg(any(test, feature = "test-support"))]
                observe_directory_open(&path);
                self.walk(
                    &directory.open_child(&name, &metadata)?,
                    &path,
                    mount,
                    visited,
                    cancel,
                )?;
            }
        }
        directory.validate()?;
        if fs_native::snapshot_lease::mount_id(&directory.file)? != mount
            || Fact::capture(&Metadata::from_file(&directory.file)?)? != fact
        {
            return Err(changed_source(
                "Approved Home directory changed during mounted-location discovery",
            ));
        }
        // Keep the exact ancestry of candidates, not a descriptor for every
        // directory in a large Home. Ordinary traversal owns only its depth.
        if relative.as_os_str().is_empty() || self.candidates.len() != candidate_start {
            self.directories.push(Directory {
                pin: directory.clone(),
                fact,
                mount,
            });
        }
        Ok(())
    }
    pub fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        super::super::migration::check_cancel(cancel)?;
        self.root.validate()?;
        self.policy.excluded(Path::new(""))?;
        for directory in &self.directories {
            super::super::migration::check_cancel(cancel)?;
            directory.pin.validate()?;
            if fs_native::snapshot_lease::mount_id(&directory.pin.file)? != directory.mount
                || Fact::capture(&Metadata::from_file(&directory.pin.file)?)? != directory.fact
            {
                return Err(changed_source(
                    "Approved Home ancestry changed during mounted-location discovery",
                ));
            }
        }
        for candidate in &self.candidates {
            super::super::migration::check_cancel(cancel)?;
            candidate.parent.validate()?;
            let current = metadata_nofollow(&candidate.parent.file, Path::new(&candidate.name))?;
            let opened = open_mount_entry(&candidate.parent, &candidate.name, &current)?;
            if Fact::capture(&current)? != candidate.fact
                || Fact::capture(&Metadata::from_file(&candidate.entry)?)? != candidate.fact
                || fs_native::snapshot_lease::mount_id(&opened)? != candidate.mount
                || fs_native::snapshot_lease::mount_id(&candidate.entry)? != candidate.mount
            {
                return Err(changed_source(
                    "Mounted Home location changed after discovery",
                ));
            }
        }
        self.root.validate()
    }
    pub fn display_names(&self) -> Vec<String> {
        self.candidates
            .iter()
            .map(|candidate| candidate.relative.display().to_string())
            .collect()
    }
    pub fn require_single_root(self: &Arc<Self>, cancel: &AtomicBool) -> io::Result<()> {
        self.validate(cancel)?;
        if self.candidates.is_empty() {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            NestedHomeMount(Some(self.clone())),
        ))
    }
}

#[cfg(any(test, feature = "test-support"))]
type DirectoryObserver = Box<dyn FnMut(&Path)>;
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static DIRECTORY_OBSERVER: std::cell::RefCell<Option<DirectoryObserver>> = const { std::cell::RefCell::new(None) };
}
/// Scoped observer for real mount-race qualification fixtures.
#[cfg(feature = "test-support")]
pub struct DiscoveryObserver;
#[cfg(feature = "test-support")]
impl DiscoveryObserver {
    pub fn install(observer: impl FnMut(&Path) + 'static) -> Self {
        DIRECTORY_OBSERVER.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(observer));
        });
        Self
    }
}
#[cfg(feature = "test-support")]
impl Drop for DiscoveryObserver {
    fn drop(&mut self) {
        DIRECTORY_OBSERVER.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
#[cfg(any(test, feature = "test-support"))]
fn observe_directory_open(path: &Path) {
    DIRECTORY_OBSERVER.with(|slot| {
        if let Some(observer) = slot.borrow_mut().as_mut() {
            observer(path);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_never_follows_outside_symlinks_and_refuses_replaced_home() {
        let temporary = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let home = root.join("home");
        let outside = root.join("outside");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("private"), b"outside bytes").unwrap();
        std::os::unix::fs::symlink(&outside, home.join("literal-link")).unwrap();
        std::fs::write(home.join(".hidden"), b"retained bytes").unwrap();
        let cancel = AtomicBool::new(false);
        let pin = PinnedDirectory::open_absolute(&home).unwrap();
        let discovered = MountedHomeLocations::discover(&pin, &home, &cancel).unwrap();
        assert!(discovered.display_names().is_empty());
        discovered.require_single_root(&cancel).unwrap();
        std::fs::rename(&home, root.join("retained-home")).unwrap();
        std::fs::create_dir(&home).unwrap();
        assert!(discovered.validate(&cancel).is_err());
        assert_eq!(
            std::fs::read(outside.join("private")).unwrap(),
            b"outside bytes"
        );
        assert_eq!(
            std::fs::read(root.join("retained-home/.hidden")).unwrap(),
            b"retained bytes"
        );
    }
    #[test]
    fn cancelled_discovery_does_not_return_a_location_review() {
        let temporary = tempfile::tempdir().unwrap();
        let home = std::fs::canonicalize(temporary.path()).unwrap();
        let pin = PinnedDirectory::open_absolute(&home).unwrap();
        let error =
            MountedHomeLocations::discover(&pin, &home, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!nested_home_mount(&error));
    }
}
