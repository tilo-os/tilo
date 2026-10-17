//! Actual full-filesystem scope for a protected per-volume participant Store.
//! No payload-authored record locator or synthetic Home/owner Plan is accepted.
use super::super::EntryBinding;
use super::*;
mod owned;

pub struct ParticipantNamespace {
    root: Arc<PinnedDirectory>,
    point: PathBuf,
    mount: u64,
    volume: SourceVolume,
    read_only: bool,
    owned: Option<owned::OwnedParticipantMount>,
}
impl ParticipantNamespace {
    fn facts(&self) -> io::Result<Facts> {
        let mut facts = Facts::read()?;
        if let Some(owned) = &self.owned {
            owned.bind_facts(&mut facts)?;
        }
        Ok(facts)
    }

    pub fn qualification_owned_root(&self) -> io::Result<()> {
        self.validate()?;
        if !self.read_only || self.owned.is_none() {
            return Err(refusal("Qualification participant is writable"));
        }
        kernel_read_only(&self.root)?;
        super::super::super::home_capture::consumer_qualification::check_source_root_marker(
            &self.root.file,
        )?;
        self.validate()
    }

    pub fn admit(path: &Path, selected: &fs::File, entry: &EntryBinding) -> io::Result<Self> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if capture_mounted_entry(path, selected, entry.kind())? != *entry {
            return Err(refusal(
                "Participant entry differs from actual platform grant",
            ));
        }
        let facts = Facts::read()?;
        // Resolving the partition separately rejects duplicate PARTUUIDs even
        // if the selected path currently resolves one apparent filesystem.
        facts.matching_volume(entry.volume())?;
        let candidates: Vec<_> = facts
            .mounts
            .iter()
            .filter(|mount| {
                mount.root == Path::new("/")
                    && facts
                        .volume(mount)
                        .is_ok_and(|(volume, _)| &volume == entry.volume())
            })
            .collect();
        let [mount] = candidates.as_slice() else {
            return Err(unsupported_source(
                "Mounted participant requires one actual full-filesystem mount",
            ));
        };
        let root = PinnedDirectory::open_absolute(&mount.point)?;
        let held = tilo_files::native::atomic::StorageDirectory::from_file(
            super::super::super::migration_durability::readable_directory(&root.file)?,
        )?;
        held.validate_namespace_control()?;
        if root.file.metadata()?.uid() != 0
            || fs_native::snapshot_lease::mount_id(&root.file)? != mount.id
        {
            return Err(refusal(
                "Participant filesystem root lacks protected custody",
            ));
        }
        let result = Self {
            root,
            point: mount.point.clone(),
            mount: mount.id,
            volume: entry.volume().clone(),
            read_only: false,
            owned: None,
        };
        result.validate()?;
        if capture_mounted_entry(path, selected, entry.kind())? != *entry {
            return Err(refusal("Participant source changed during root admission"));
        }
        Ok(result)
    }
    pub fn root(&self) -> &Arc<PinnedDirectory> {
        &self.root
    }
    pub fn point(&self) -> &Path {
        &self.point
    }
    pub fn validate(&self) -> io::Result<()> {
        self.validate_with(&self.facts()?)
    }
    fn validate_with(&self, facts: &Facts) -> io::Result<()> {
        self.root.validate()?;
        let current = PinnedDirectory::open_absolute(&self.point)?;
        if !super::super::super::nofollow::same_directory_identity(
            &current.identity,
            &self.root.identity,
        ) || fs_native::snapshot_lease::mount_id(&current.file)? != self.mount
            || fs_native::snapshot_lease::mount_id(&self.root.file)? != self.mount
        {
            return Err(refusal("Participant full-filesystem mount changed"));
        }
        facts.matching_volume(&self.volume)?;
        let mount = facts.covering(&self.point)?;
        if mount.point != self.point
            || mount.root != Path::new("/")
            || mount.id != self.mount
            || facts.volume(mount)?.0 != self.volume
        {
            return Err(refusal("Participant disk or filesystem geometry changed"));
        }
        if self.read_only {
            kernel_read_only(&self.root)?;
            if !mount.read_only
                || facts.mounts.iter().any(|alias| {
                    (alias.source == mount.source || alias.number == mount.number)
                        && !alias.read_only
                })
            {
                return Err(refusal("Participant source also has a writable mount"));
            }
        }
        let held = tilo_files::native::atomic::StorageDirectory::from_file(
            super::super::super::migration_durability::readable_directory(&self.root.file)?,
        )?;
        held.validate_namespace_control()
    }
}

impl ParticipantNamespace {
    /// Existing readonly full-root mount only. No payload path, implicit mount,
    /// synthetic directory Home or cleanup authority is introduced.
    pub fn reopen_read_only(entry: &EntryBinding) -> io::Result<Self> {
        entry.validate()?;
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let facts = Facts::read()?;
        Self::reopen_read_only_with(entry, &facts)
    }
    fn reopen_read_only_with(entry: &EntryBinding, facts: &Facts) -> io::Result<Self> {
        entry.validate()?;
        facts.matching_volume(entry.volume())?;
        let candidates: Vec<_> = facts
            .mounts
            .iter()
            .filter(|mount| {
                mount.root == Path::new("/")
                    && facts
                        .volume(mount)
                        .is_ok_and(|(volume, _)| &volume == entry.volume())
            })
            .collect();
        let [mount] = candidates.as_slice() else {
            return Err(unsupported_source(
                "Participant requires its unique existing readonly full-filesystem mount",
            ));
        };
        if !mount.read_only {
            return Err(refusal("Participant source is not readonly"));
        }
        let root = PinnedDirectory::open_absolute(&mount.point)?;
        kernel_read_only(&root)?;
        let result = Self {
            root,
            point: mount.point.clone(),
            mount: mount.id,
            volume: entry.volume().clone(),
            read_only: true,
            owned: None,
        };
        result.validate_with(facts)?;
        Ok(result)
    }
    pub fn open_record(&self, operation: &str) -> io::Result<fs::File> {
        if !super::super::operation_id(operation) {
            return Err(refusal("Participant operation is not canonical"));
        }
        self.validate()?;
        kernel_read_only(&self.root)?;
        let mut directory = self.root.clone();
        for name in ["var", "lib", "tilo-imager", "home-capture", "records"] {
            let metadata =
                super::super::super::nofollow::metadata_nofollow(&directory.file, Path::new(name))?;
            directory = directory.open_child(std::ffi::OsStr::new(name), &metadata)?;
            if fs_native::snapshot_lease::mount_id(&directory.file)? != self.mount {
                return Err(refusal(
                    "Participant record namespace crosses an undeclared mount",
                ));
            }
            let held = tilo_files::native::atomic::StorageDirectory::from_file(
                super::super::super::migration_durability::readable_directory(&directory.file)?,
            )?;
            held.validate_namespace_control()?;
            if matches!(name, "home-capture" | "records") {
                held.validate_privacy(tilo_files::native::atomic::Privacy::OwnerOnly)?;
            }
        }
        let held = tilo_files::native::atomic::StorageDirectory::from_file(
            super::super::super::migration_durability::readable_directory(&directory.file)?,
        )?;
        let file = held.open_read(
            std::ffi::OsStr::new(&format!("{operation}.mounted-participant.json")),
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )?;
        directory.validate()?;
        self.validate()?;
        kernel_read_only(&self.root)?;
        Ok(file)
    }
    pub fn open_entry(&self, entry: &EntryBinding) -> io::Result<fs::File> {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        entry.validate()?;
        self.validate()?;
        kernel_read_only(&self.root)?;
        if entry.volume() != &self.volume {
            return Err(refusal("Participant entry belongs to another volume"));
        }
        let path = entry.home.relative.decode().map_err(refusal)?;
        let mut directory = self.root.clone();
        let mut names = path.components().peekable();
        let file = loop {
            let Some(std::path::Component::Normal(name)) = names.next() else {
                break directory.file.try_clone()?;
            };
            let metadata =
                super::super::super::nofollow::metadata_nofollow(&directory.file, Path::new(name))?;
            if names.peek().is_some() {
                directory = directory.open_child(name, &metadata)?;
                if fs_native::snapshot_lease::mount_id(&directory.file)? != self.mount {
                    return Err(refusal("Participant locator crosses ungranted mount"));
                }
                continue;
            }
            let name = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
            let raw = unsafe {
                libc::openat(
                    directory.file.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            break unsafe { fs::File::from_raw_fd(raw) };
        };
        let metadata = file.metadata()?;
        if (
            metadata.ino(),
            metadata.uid(),
            metadata.gid(),
            metadata.mode(),
        ) != (entry.home.inode, entry.owner(), entry.group, entry.mode)
            || metadata.file_type().is_symlink()
            || fs_native::snapshot_lease::mount_id(&file)? != self.mount
        {
            return Err(refusal("Protected participant source replaced"));
        }
        if let Some(expected) = &entry.home.subvolume {
            let query = fs::File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
            if super::super::super::migration_durability::identity(&query)? != *expected {
                return Err(refusal("Participant Btrfs inode identity changed"));
            }
        }
        directory.validate()?;
        self.validate()?;
        kernel_read_only(&self.root)?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn participant_namespace_never_promotes_a_different_actual_descriptor_to_source_authority() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::write(&first, b"selected bytes remain").unwrap();
        fs::write(&second, b"unselected bytes remain").unwrap();
        let sample = super::super::super::tests::sample();
        let binding = EntryBinding {
            home: sample.home,
            kind: super::super::super::EntryKind::File,
            group: 1002,
            mode: libc::S_IFREG | 0o640,
        };
        let selected = fs::File::open(&second).unwrap();
        let error = ParticipantNamespace::admit(&first, &selected, &binding)
            .err()
            .expect("mismatched actual descriptor must refuse before creating any namespace");
        if rustix::process::geteuid().as_raw() == 0 {
            assert_eq!(
                error.to_string(),
                "Mounted entry differs from actual selected descriptor"
            );
        } else {
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        assert!(!temp.path().join("var").exists());
        assert_eq!(fs::read(first).unwrap(), b"selected bytes remain");
        assert_eq!(fs::read(second).unwrap(), b"unselected bytes remain");
    }
}
