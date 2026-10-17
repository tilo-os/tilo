//! No-replay mounts owned by authenticated root source roles. Ordinary user
//! UDisks mounts and writable retained mounts do not enter this backend.
use super::*;
use std::{
    ffi::CString,
    os::{
        fd::{AsFd, AsRawFd},
        unix::fs::FileTypeExt,
    },
    sync::Mutex,
};

pub(super) struct RootReadOnlyMount(Mutex<Option<Custody>>);
struct Custody {
    parent: PathBuf,
    parent_pin: fs::File,
    point: PathBuf,
    underlying: fs::File,
    block: fs::File,
    block_stamp: (u64, u64, u64),
    namespace: fs::File,
    volume: SourceVolume,
    device: String,
    witness: Option<OwnedMountWitness>,
    root: Option<fs::File>,
}
fn denied(message: &'static str) -> io::Error {
    refusal(message)
}
fn cpath(path: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}
fn stamp(file: &fs::File) -> io::Result<(u64, u64, u64)> {
    let m = file.metadata()?;
    Ok((m.dev(), m.ino(), m.rdev()))
}
fn readable(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn validate_selected_block(
    facts: &Facts,
    volume: &SourceVolume,
    device: &str,
    rdev: u64,
) -> io::Result<()> {
    if facts.matching_volume(volume)? != device {
        return Err(denied("Selected source GPT identity changed"));
    }
    let expected = format!("{}:{}", libc::major(rdev), libc::minor(rdev));
    let partitions: Vec<_> = facts
        .devices
        .iter()
        .flat_map(|disk| disk.children.iter())
        .filter(|part| part.name == device)
        .collect();
    let [partition] = partitions.as_slice() else {
        return Err(denied("Selected source device is ambiguous"));
    };
    let named = fs::symlink_metadata(device)?;
    if partition.maj_min.as_deref() != Some(expected.as_str())
        || !named.file_type().is_block_device()
        || named.rdev() != rdev
    {
        return Err(denied(
            "Selected source named/kernel device differs from held authorized block",
        ));
    }
    Ok(())
}
impl Custody {
    fn namespace(&self) -> io::Result<(u64, u64)> {
        let held = self.namespace.metadata()?;
        let current = fs::metadata("/proc/self/ns/mnt")?;
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Err(denied("Root readonly source namespace changed"));
        }
        Ok((held.dev(), held.ino()))
    }
    fn validate_parent(&self) -> io::Result<()> {
        let actual = readable(&self.parent)?;
        let held = self.parent_pin.metadata()?;
        let named = actual.metadata()?;
        if (held.dev(), held.ino()) != (named.dev(), named.ino())
            || held.uid() != 0
            || held.mode() & 0o777 != 0o700
        {
            return Err(denied("Root readonly source mount parent changed"));
        }
        Ok(())
    }
    fn bind(&self, facts: &mut Facts) -> io::Result<()> {
        self.validate_parent()?;
        let namespace = self.namespace()?;
        let witness = self
            .witness
            .as_ref()
            .ok_or_else(|| denied("Root readonly source has no initial mount witness"))?;
        witness.validate(facts, namespace)?;
        if facts.matching_volume(&self.volume)? != self.device
            || stamp(&self.block)? != self.block_stamp
        {
            return Err(denied("Root readonly held source device changed"));
        }
        let block = self.block.metadata()?;
        validate_selected_block(facts, &self.volume, &self.device, block.rdev())?;
        if !block.file_type().is_block_device() {
            return Err(denied("Root readonly source is not a held block device"));
        }
        let root = self
            .root
            .as_ref()
            .ok_or_else(|| denied("Root readonly mount root is unavailable"))?;
        let named = readable(&self.point)?;
        if stamp(root)? != stamp(&named)?
            || fs_native::snapshot_lease::mount_id(root)? != witness.id
            || fs_native::snapshot_lease::mount_id(&named)? != witness.id
        {
            return Err(denied("Root readonly mount root changed"));
        }
        let mount = facts
            .mounts
            .iter()
            .find(|m| m.id == witness.id)
            .ok_or_else(|| denied("Root readonly source mount absent"))?;
        let fsname = match self.volume.filesystem {
            Filesystem::Ext4 => "ext4",
            Filesystem::Xfs => "xfs",
            Filesystem::Btrfs => "btrfs",
        };
        if !mount.read_only || mount.root != Path::new("/") || mount.filesystem != fsname {
            return Err(denied(
                "Root readonly source mount flags or filesystem changed",
            ));
        }
        let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: held readable directory and complete kernel output storage.
        if unsafe { libc::fstatvfs(root.as_raw_fd(), status.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatvfs initialized the output.
        let flags = unsafe { status.assume_init() }.f_flag;
        let required =
            libc::ST_RDONLY | libc::ST_NODEV | libc::ST_NOSUID | libc::ST_NOEXEC | libc::ST_NOATIME;
        if flags & required != required {
            return Err(denied("Kernel readonly source protections changed"));
        }
        if self.volume.filesystem == Filesystem::Btrfs {
            let physical = fs_native::btrfs::single_device_identity(root.as_fd())?;
            if physical.rdev != block.rdev()
                || uuid::Uuid::from_bytes(physical.filesystem).to_string()
                    != self.volume.filesystem_uuid.to_lowercase()
                || fs_native::btrfs::subvolume_identity(root.as_fd())?.id != 5
            {
                return Err(denied(
                    "Root readonly Btrfs mount differs from held selected device",
                ));
            }
            // Btrfs subvolumes can have different anonymous device numbers.
            // Inspect actual kernel membership, never cached mount source text.
            for alias in &facts.mounts {
                if alias.filesystem == "btrfs" && alias.id != witness.id {
                    let directory = readable(&alias.point)?;
                    if fs_native::snapshot_lease::mount_id(&directory)? != alias.id {
                        return Err(denied(
                            "Btrfs alias mount changed during source observation",
                        ));
                    }
                    let alias_filesystem =
                        fs_native::btrfs::subvolume_identity(directory.as_fd())?.filesystem;
                    if alias_filesystem != physical.filesystem {
                        continue;
                    }
                    if fs_native::btrfs::single_device_identity(directory.as_fd())?.rdev
                        == block.rdev()
                    {
                        return Err(denied(
                            "Root readonly Btrfs source has another mounted alias",
                        ));
                    }
                }
            }
        } else {
            let expected = format!(
                "{}:{}",
                libc::major(block.rdev()),
                libc::minor(block.rdev())
            );
            if mount.number != expected {
                return Err(denied("Root readonly mount has another physical device"));
            }
        }
        facts.owned_bindings.push((witness.id, self.device.clone()));
        Ok(())
    }
}
impl RootReadOnlyMount {
    pub(super) fn mount(
        volume: &SourceVolume,
        facts: &Facts,
    ) -> io::Result<(PathBuf, Arc<MountedVolume>)> {
        // This is selected explicitly only by root authenticated source roles.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let device = facts.matching_volume(volume)?;
        if facts
            .mounts
            .iter()
            .any(|m| facts.volume(m).is_ok_and(|(v, _)| v == *volume))
        {
            return Err(denied(
                "Source is already mounted; grant its verified existing mount",
            ));
        }
        let service = block_devices::udisks::Udisks::system().map_err(io::Error::other)?;
        let block = service.open_read_only(&device).map_err(io::Error::other)?;
        let metadata = block.metadata()?;
        let partition = facts
            .devices
            .iter()
            .flat_map(|disk| disk.children.iter())
            .find(|part| part.name == device)
            .ok_or_else(|| denied("Selected source partition disappeared"))?;
        let number = format!(
            "{}:{}",
            libc::major(metadata.rdev()),
            libc::minor(metadata.rdev())
        );
        // SAFETY: F_GETFL reads flags of the live held authorized descriptor.
        let access = unsafe { libc::fcntl(block.as_raw_fd(), libc::F_GETFL) };
        if !metadata.file_type().is_block_device()
            || partition.maj_min.as_deref() != Some(number.as_str())
            || access < 0
            || access & libc::O_ACCMODE != libc::O_RDONLY
        {
            return Err(denied(
                "Authorized readonly source descriptor differs from selected GPT partition",
            ));
        }
        validate_selected_block(&Facts::read()?, volume, &device, metadata.rdev())?;
        if volume.filesystem == Filesystem::Btrfs {
            for mount in &Facts::read()?.mounts {
                if mount.filesystem != "btrfs" {
                    continue;
                }
                let root = readable(&mount.point)?;
                if fs_native::snapshot_lease::mount_id(&root)? != mount.id {
                    return Err(denied("Btrfs source mount changed before kernel mount"));
                }
                if uuid::Uuid::from_bytes(
                    fs_native::btrfs::subvolume_identity(root.as_fd())?.filesystem,
                )
                .to_string()
                    == volume.filesystem_uuid.to_lowercase()
                    && fs_native::btrfs::single_device_identity(root.as_fd())?.rdev
                        == metadata.rdev()
                {
                    return Err(denied("Selected Btrfs source is already mounted"));
                }
            }
        }
        let namespace = fs::File::open("/proc/self/ns/mnt")?;
        if !Facts::read()?.covering(Path::new("/run"))?.private {
            return Err(denied(
                "Authenticated readonly source helper requires private mount propagation",
            ));
        }
        let parent_directory = tempfile::Builder::new()
            .prefix("tilo-readonly-source-")
            .tempdir_in("/run")?;
        let parent = parent_directory.path().to_owned();
        // Explicit mode defeats both caller umask and ambient helper defaults.
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))?;
        let parent_pin = readable(&parent)?;
        let point = parent.join("source");
        fs::create_dir(&point)?;
        let underlying = readable(&point)?;
        let custody = Custody {
            parent,
            parent_pin,
            point: point.clone(),
            underlying,
            block_stamp: stamp(&block)?,
            block,
            namespace,
            volume: volume.clone(),
            device: device.clone(),
            witness: None,
            root: None,
        };
        custody.validate_parent()?;
        let source = CString::new(format!("/proc/self/fd/{}", custody.block.as_raw_fd()))
            .map_err(io::Error::other)?;
        let target = cpath(&point)?;
        let (filesystem, options) = match volume.filesystem {
            Filesystem::Ext4 => (c"ext4", c"noload"),
            Filesystem::Xfs => (c"xfs", c"norecovery"),
            Filesystem::Btrfs => (c"btrfs", c"nologreplay,subvolid=5"),
        };
        let flags =
            libc::MS_RDONLY | libc::MS_NODEV | libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NOATIME;
        let _kept_parent = parent_directory.keep();
        // SAFETY: all strings are controlled NUL-terminated values; source is
        // this still-held authorized block FD, target a root-private directory.
        if unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                filesystem.as_ptr(),
                flags,
                options.as_ptr().cast(),
            )
        } != 0
        {
            let error = io::Error::last_os_error();
            drop(custody);
            fs::remove_dir(&point)?;
            fs::remove_dir(
                point
                    .parent()
                    .ok_or_else(|| denied("Source parent missing"))?,
            )?;
            return Err(error);
        }
        // Store actual custody immediately after a successful kernel effect.
        // Failed observation is quarantined by backend Drop, never unmounted
        // against an invented witness or traversed by TempDir cleanup.
        let backend = RootReadOnlyMount(Mutex::new(Some(custody)));
        let observed = (|| {
            let mut current = Facts::read()?;
            let mut held = backend
                .0
                .lock()
                .map_err(|_| denied("Root readonly custody poisoned"))?;
            let custody = held
                .as_mut()
                .ok_or_else(|| denied("Root readonly custody absent"))?;
            custody.witness = Some(OwnedMountWitness::capture(
                &current,
                &point,
                custody.namespace()?,
            )?);
            custody.root = Some(readable(&point)?);
            custody.bind(&mut current)
        })();
        observed?;
        let owned = Arc::new(MountedVolume {
            service: Box::new(backend),
            device,
            // Construction has observed this exact readonly mount, namespace,
            // underlying point and kernel witness. The last lease must retire
            // it on constructor refusal as well as explicit success; release
            // revalidates custody and quarantines any actual uncertainty.
            automatic_release: true,
        });
        Ok((point, owned))
    }
}
impl MountRelease for RootReadOnlyMount {
    fn authenticated_readonly(&self) -> bool {
        true
    }
    fn bind_facts(&self, facts: &mut Facts) -> io::Result<()> {
        self.0
            .lock()
            .map_err(|_| denied("Root readonly custody poisoned"))?
            .as_ref()
            .ok_or_else(|| denied("Root readonly custody retired"))?
            .bind(facts)
    }
    fn release(&self, device: &str) -> io::Result<()> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| denied("Root readonly custody poisoned"))?;
        let custody = guard
            .as_mut()
            .ok_or_else(|| denied("Root readonly custody retired"))?;
        if device != custody.device {
            return Err(denied("Root readonly release device differs"));
        }
        let mut facts = Facts::read()?;
        custody.bind(&mut facts)?;
        let id = custody
            .witness
            .as_ref()
            .ok_or_else(|| denied("Root readonly witness absent"))?
            .id;
        // Close our own active mount reference before ordinary unmount. The
        // root-private named point and underlying directory remain pinned.
        drop(custody.root.take());
        custody.validate_parent()?;
        let latest = Facts::read()?;
        custody
            .witness
            .as_ref()
            .ok_or_else(|| denied("Root readonly mount witness absent"))?
            .validate(&latest, custody.namespace()?)?;
        let named = readable(&custody.point)?;
        if fs_native::snapshot_lease::mount_id(&named)? != id {
            custody.root = Some(named);
            return Err(denied("Root readonly named mount changed before unmount"));
        }
        drop(named);
        let target = cpath(&custody.point)?;
        // SAFETY: exact root-private previously attested point; no force/lazy.
        if unsafe { libc::umount2(target.as_ptr(), 0) } != 0 {
            let error = io::Error::last_os_error();
            custody.root = readable(&custody.point).ok();
            return Err(error);
        }
        let after = Facts::read()?;
        if after
            .mounts
            .iter()
            .any(|m| m.id == id || m.point == custody.point)
            || stamp(&readable(&custody.point)?)? != stamp(&custody.underlying)?
        {
            return Err(denied("Root readonly source retirement cannot be observed"));
        }
        let number = format!(
            "{}:{}",
            libc::major(custody.block_stamp.2),
            libc::minor(custody.block_stamp.2)
        );
        for alias in &after.mounts {
            if custody.volume.filesystem == Filesystem::Btrfs && alias.filesystem == "btrfs" {
                let directory = readable(&alias.point)?;
                if fs_native::snapshot_lease::mount_id(&directory)? != alias.id {
                    return Err(denied("Btrfs alias changed during source retirement"));
                }
                if uuid::Uuid::from_bytes(
                    fs_native::btrfs::subvolume_identity(directory.as_fd())?.filesystem,
                )
                .to_string()
                    == custody.volume.filesystem_uuid.to_lowercase()
                    && fs_native::btrfs::single_device_identity(directory.as_fd())?.rdev
                        == custody.block_stamp.2
                {
                    return Err(denied(
                        "Retired readonly Btrfs source still has a mounted alias",
                    ));
                }
            } else if alias.number == number {
                return Err(denied("Retired readonly source still has a mounted alias"));
            }
        }
        custody.validate_parent()?;
        fs::remove_dir(&custody.point)?;
        fs::remove_dir(&custody.parent)?;
        drop(guard.take());
        Ok(())
    }
}
impl Drop for RootReadOnlyMount {
    fn drop(&mut self) {
        let custody = match self.0.get_mut() {
            Ok(held) => held.take(),
            Err(poison) => poison.into_inner().take(),
        };
        if let Some(custody) = custody {
            static UNKNOWN: Mutex<Vec<Custody>> = Mutex::new(Vec::new());
            match UNKNOWN.lock() {
                Ok(mut held) if held.len() < 64 => held.push(custody),
                _ => std::mem::forget(custody),
            }
        }
    }
}
