//! Metadata-only capture from the exact approved entry; no root-owned data read.
use super::super::{EntryBinding, EntryKind};
use super::*;
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::ffi::OsStrExt,
};

impl ResolvedSource {
    /// Reopen a protected same-volume binding as O_PATH, never a root-readable
    /// file handed to the original-user worker. The owning source context must
    /// retain its actual readonly mount until the worker has retired.
    pub fn open_original_mounted_entry(&self, binding: &EntryBinding) -> io::Result<fs::File> {
        binding.validate()?;
        if binding.volume() != &self.plan.home.volume {
            return Err(refusal("Mounted entry lacks same-volume source custody"));
        }
        self.pin.validate()?;
        kernel_read_only(&self.pin)?;
        let facts = Facts::read()?;
        let renewed = resolve_mode_with(
            &self.plan,
            &self.mountpoint,
            &facts,
            true,
            AccessMode::ReadOnly,
        )?;
        if !super::super::super::nofollow::same_directory_identity(
            &renewed.pin.identity,
            &self.pin.identity,
        ) {
            return Err(refusal("Mounted entry primary source changed"));
        }
        let mount = facts
            .mounts
            .iter()
            .rev()
            .find(|mount| mount.point == self.mountpoint)
            .ok_or_else(|| refusal("Mounted entry source mount disappeared"))?;
        let relative = binding.home.relative.decode().map_err(refusal)?;
        let suffix = relative
            .strip_prefix(
                mount
                    .root
                    .strip_prefix("/")
                    .map_err(|_| refusal("Mounted filesystem root must be absolute"))?,
            )
            .map_err(|_| unsupported_source("Mounted entry is outside held source mount"))?;
        let mut directory = PinnedDirectory::open_absolute(&self.mountpoint)?;
        let mount_id = fs_native::snapshot_lease::mount_id(&directory.file)?;
        if mount_id != mount.id {
            return Err(refusal("Mounted entry source mount identity changed"));
        }
        let mut names = suffix.components().peekable();
        let file = loop {
            let Some(component) = names.next() else {
                break directory.file.try_clone()?;
            };
            let std::path::Component::Normal(name) = component else {
                return Err(refusal("Mounted entry locator is not normalized"));
            };
            let metadata =
                super::super::super::nofollow::metadata_nofollow(&directory.file, Path::new(name))?;
            if names.peek().is_some() {
                directory = directory.open_child(name, &metadata)?;
                if fs_native::snapshot_lease::mount_id(&directory.file)? != mount_id {
                    return Err(refusal("Mounted entry locator crosses an unapproved mount"));
                }
                continue;
            }
            let name = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
            // Held directory and a single literal component, with no symlink
            // or file data read. Final mount identity is checked before use.
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
        ) != (
            binding.home.inode,
            binding.home.original_uid,
            binding.group,
            binding.mode,
        ) || fs_native::snapshot_lease::mount_id(&file)? != mount_id
            || metadata.file_type().is_symlink()
        {
            return Err(refusal("Protected mounted entry was replaced"));
        }
        // Query Btrfs durable inode metadata only. This temporary descriptor is
        // closed here; the handed descriptor remains O_PATH and confers no DAC
        // bypass. The worker opens data only after original credentials drop.
        if let Some(expected) = &binding.home.subvolume {
            let query = fs::File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
            let actual = super::super::super::migration_durability::identity(&query)?;
            if actual != *expected {
                return Err(refusal("Mounted entry Btrfs identity changed"));
            }
        }
        directory.validate()?;
        self.pin.validate()?;
        kernel_read_only(&self.pin)?;
        if !same(&metadata, &file.metadata()?)
            || fs_native::snapshot_lease::mount_id(&file)? != mount_id
        {
            return Err(refusal("Mounted entry changed during readonly reopen"));
        }
        Ok(file)
    }
}

fn same(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    (left.dev(), left.ino(), left.uid(), left.gid(), left.mode())
        == (
            right.dev(),
            right.ino(),
            right.uid(),
            right.gid(),
            right.mode(),
        )
}
pub fn capture_mounted_entry(
    path: &Path,
    selected: &fs::File,
    kind: EntryKind,
) -> io::Result<EntryBinding> {
    let parent = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| refusal("Mounted entry parent missing"))?,
    )?;
    let name = path
        .file_name()
        .ok_or_else(|| refusal("Mounted entry name missing"))?;
    let name = std::ffi::CString::new(name.as_bytes()).map_err(io::Error::other)?;
    // SAFETY: held literal parent and single terminated filename. No data is
    // opened as root; the bound entry is retained only with O_PATH.
    let raw = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let named = unsafe { fs::File::from_raw_fd(raw) };
    let metadata = selected.metadata()?;
    if !same(&metadata, &named.metadata()?)
        || metadata.file_type().is_symlink()
        || (kind == EntryKind::Directory && !metadata.is_dir())
        || (kind == EntryKind::File && !metadata.is_file())
    {
        return Err(refusal(
            "Mounted entry differs from actual selected descriptor",
        ));
    }
    let facts = Facts::read()?;
    let mount = facts.covering(path)?;
    if fs_native::snapshot_lease::mount_id(selected)? != mount.id
        || fs_native::snapshot_lease::mount_id(&named)? != mount.id
    {
        return Err(refusal("Mounted entry namespace changed"));
    }
    let (volume, _) = facts.volume(mount)?;
    let relative = mount
        .root
        .strip_prefix("/")
        .map_err(|_| refusal("Invalid mounted filesystem root"))?
        .join(
            path.strip_prefix(&mount.point)
                .map_err(|_| refusal("Entry escaped covering mount"))?,
        );
    // EntryBinding validates actual directory roots (including filesystem
    // root) separately from regular-file locators.
    // Selected descriptors already carry the platform grant. Identity queries
    // inspect only metadata; an O_PATH entry is never promoted to root READ.
    let subvolume = if volume.filesystem == Filesystem::Btrfs {
        let file = if metadata.is_dir() {
            super::super::super::migration_durability::readable_directory(selected)?
        } else {
            selected.try_clone()?
        };
        Some(super::super::super::migration_durability::identity(&file)?)
    } else {
        None
    };
    let result = EntryBinding {
        home: Home {
            volume,
            relative: NativePath::encode(&relative),
            inode: metadata.ino(),
            original_uid: metadata.uid(),
            subvolume,
        },
        kind,
        group: metadata.gid(),
        mode: metadata.mode(),
    };
    result.validate()?;
    parent.validate()?;
    if !same(&metadata, &selected.metadata()?)
        || !same(&metadata, &named.metadata()?)
        || fs_native::snapshot_lease::mount_id(selected)? != mount.id
        || fs_native::snapshot_lease::mount_id(&named)? != mount.id
    {
        return Err(refusal("Mounted entry changed during registration"));
    }
    let raw = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let current_named = unsafe { fs::File::from_raw_fd(raw) };
    if !same(&metadata, &current_named.metadata()?)
        || fs_native::snapshot_lease::mount_id(&current_named)? != mount.id
    {
        return Err(refusal(
            "Mounted entry name was substituted during registration",
        ));
    }
    let renewed = Facts::read()?;
    let current = renewed.covering(path)?;
    if current.id != mount.id || renewed.volume(current)?.0 != result.home.volume {
        return Err(refusal("Mounted entry volume changed during registration"));
    }
    Ok(result)
}

pub fn mounted_record_namespace(
    entry: &EntryBinding,
    records: &Path,
    held: &Arc<PinnedDirectory>,
) -> io::Result<PathBuf> {
    entry.validate()?;
    if !records.ends_with("var/lib/tilo-imager/home-capture/records") {
        return Err(refusal("Expected fixed broker records"));
    }
    let current = PinnedDirectory::open_absolute(records)?;
    current.validate_destination()?;
    held.validate_destination()?;
    if !super::super::super::nofollow::same_directory_identity(&current.identity, &held.identity) {
        return Err(refusal("Mounted entry records were replaced"));
    }
    let facts = Facts::read()?;
    let mount = facts.covering(records)?;
    if facts.volume(mount)?.0 != entry.home.volume
        || fs_native::snapshot_lease::mount_id(&held.file)? != mount.id
    {
        return Err(unsupported_source(
            "Each mounted source needs protected records on its actual filesystem",
        ));
    }
    let relative = mount
        .root
        .strip_prefix("/")
        .map_err(|_| refusal("Invalid records mount root"))?
        .join(
            records
                .strip_prefix(&mount.point)
                .map_err(|_| refusal("Records escaped source mount"))?,
        );
    super::super::relative(&relative)?;
    current.validate_destination()?;
    held.validate_destination()?;
    Ok(relative)
}
