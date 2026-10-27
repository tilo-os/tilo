//! Kernel-derived filesystem coordinates for scratch/source overlap refusal.
//! Metadata reopens confer no source-read or parent-directory permission.
use super::{AsRawFd, File, OpenOptionsExt, Path, PathBuf, identity, io, refuse};
use std::{
    io::Read,
    os::{fd::FromRawFd, unix::ffi::OsStrExt},
    path::Component,
};
const TABLE_MAX: usize = 4 * 1024 * 1024;
#[derive(PartialEq, Eq)]
pub(super) struct Coordinate {
    device: (u32, u32),
    path: PathBuf,
}
impl Coordinate {
    pub(super) fn overlaps(&self, other: &Self) -> bool {
        if self.device != other.device {
            return false;
        }
        let contains_other = self.path.starts_with(&other.path);
        contains_other || other.path.starts_with(&self.path)
    }
}
#[derive(PartialEq, Eq)]
struct Mount {
    device: (u32, u32),
    root: PathBuf,
    point: PathBuf,
    filesystem: Vec<u8>,
}
pub(super) fn of(file: &File) -> io::Result<Coordinate> {
    let before = identity(file)?;
    let name = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    literal(&name)?;
    let mount = selected_mount(before.2)?;
    let filesystem = filesystem_type(file)?;
    // Btrfs getattr returns the subvolume's anonymous device, whereas
    // mountinfo reports the superblock device. Both remain independently
    // attested: object identity above and filesystem coordinates below.
    let btrfs = filesystem == libc::BTRFS_SUPER_MAGIC;
    if btrfs != (mount.filesystem == b"btrfs") {
        return Err(refuse("Held source filesystem type differs"));
    }
    if !btrfs && mount.device != (libc::major(before.0), libc::minor(before.0)) {
        return Err(refuse("Held source mount device differs"));
    }
    let relative = name
        .strip_prefix(&mount.point)
        .map_err(|_| refuse("Held source path lies outside its kernel mount"))?;
    let named = reopen(&name)?;
    if identity(&named)? != before || filesystem_type(&named)? != filesystem {
        return Err(refuse("Held source namespace differs from its descriptor"));
    }
    // For a regular-file root, attest the real containing directory and exact
    // child. The parent is used only for this metadata proof; it is never
    // added to the mounted source table or worker's readable policy.
    let parent = if file.metadata()?.is_file() {
        let parent_name = name
            .parent()
            .ok_or_else(|| refuse("Held file parent missing"))?;
        let leaf = name
            .file_name()
            .ok_or_else(|| refuse("Held file name missing"))?;
        let parent = reopen(parent_name)?;
        if !parent.metadata()?.is_dir() {
            return Err(refuse("Held file containing directory differs"));
        }
        let leaf = std::ffi::CString::new(leaf.as_bytes()).map_err(io::Error::other)?;
        let raw = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let child = unsafe { File::from_raw_fd(raw) };
        if identity(&child)? != before {
            return Err(refuse("Held file containing entry differs"));
        }
        Some((parent_name.to_owned(), parent))
    } else {
        None
    };
    let path = mount.root.join(relative);
    literal(&path)?;
    if filesystem_type(file)? != filesystem
        || filesystem_type(&named)? != filesystem
        || identity(file)? != before
        || identity(&reopen(&name)?)? != before
        || std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))? != name
        || selected_mount(before.2)? != mount
    {
        return Err(refuse("Held source coordinate changed during review"));
    }
    if let Some((name, parent)) = parent
        && identity(&reopen(&name)?)? != identity(&parent)?
    {
        return Err(refuse("Held file containing directory changed"));
    }
    Ok(Coordinate {
        device: mount.device,
        path,
    })
}
fn filesystem_type(file: &File) -> io::Result<libc::c_long> {
    let mut value = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs initializes the output on success; file owns a live FD.
    if unsafe { libc::fstatfs(file.as_raw_fd(), value.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { value.assume_init() }.f_type)
}
fn reopen(path: &Path) -> io::Result<File> {
    File::options()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn literal(path: &Path) -> io::Result<()> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute()
        || bytes.len() > 65536
        || bytes.contains(&0)
        || bytes.ends_with(b" (deleted)")
        || path
            .components()
            .any(|component| !matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(refuse("Held source coordinate is not literal and present"));
    }
    Ok(())
}
fn selected_mount(wanted: u64) -> io::Result<Mount> {
    let mut bytes = Vec::new();
    File::open("/proc/self/mountinfo")?
        .take((TABLE_MAX + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > TABLE_MAX {
        return Err(refuse("Kernel mount table exceeds bound"));
    }
    let mut result = None;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let fields = line.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() < 10 {
            return Err(refuse("Kernel mount table malformed"));
        }
        let id = number(fields.first().ok_or(io::ErrorKind::InvalidData)?)?;
        if id != wanted {
            continue;
        }
        if result.is_some() {
            return Err(refuse("Kernel mount identifier repeated"));
        }
        let device = fields
            .get(2)
            .ok_or(io::ErrorKind::InvalidData)?
            .split(|byte| *byte == b':')
            .collect::<Vec<_>>();
        if device.len() != 2 {
            return Err(refuse("Kernel mount device malformed"));
        }
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(|| refuse("Kernel mount filesystem missing"))?;
        if separator < 6
            || fields.len() != separator.checked_add(4).ok_or(io::ErrorKind::InvalidData)?
        {
            return Err(refuse("Kernel mount filesystem malformed"));
        }
        let filesystem = fields
            .get(separator.checked_add(1).ok_or(io::ErrorKind::InvalidData)?)
            .ok_or(io::ErrorKind::InvalidData)?
            .to_vec();
        let root = decode(fields.get(3).ok_or(io::ErrorKind::InvalidData)?)?;
        let point = decode(fields.get(4).ok_or(io::ErrorKind::InvalidData)?)?;
        literal(&root)?;
        literal(&point)?;
        result = Some(Mount {
            device: (
                u32::try_from(number(device.first().ok_or(io::ErrorKind::InvalidData)?)?)
                    .map_err(io::Error::other)?,
                u32::try_from(number(device.get(1).ok_or(io::ErrorKind::InvalidData)?)?)
                    .map_err(io::Error::other)?,
            ),
            root,
            point,
            filesystem,
        });
    }
    result.ok_or_else(|| refuse("Held source mount is absent from current namespace"))
}
fn number(bytes: &[u8]) -> io::Result<u64> {
    std::str::from_utf8(bytes)
        .map_err(io::Error::other)?
        .parse()
        .map_err(io::Error::other)
}
fn decode(bytes: &[u8]) -> io::Result<PathBuf> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some((&byte, rest)) = remaining.split_first() {
        if byte != b'\\' {
            decoded.push(byte);
            remaining = rest;
            continue;
        }
        let (encoded, rest) = rest
            .split_at_checked(3)
            .ok_or_else(|| refuse("Kernel mount escape truncated"))?;
        decoded.push(match encoded {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return Err(refuse("Kernel mount escape unsupported")),
        });
        remaining = rest;
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&decoded)))
}

#[cfg(all(test, feature = "managed-tree-test"))]
mod tests {
    use super::*;
    use std::{
        ffi::CString,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    const SELECTOR: &str = "owned_service::capture::inventory_scratch::coordinate::tests::actual_bind_alias_coordinates_refuse_scratch_overlap_and_preserve_siblings";
    struct Binding(CString);
    impl Binding {
        fn new(source: &Path, target: &Path) -> Self {
            let source = CString::new(source.as_os_str().as_bytes()).unwrap();
            let target = CString::new(target.as_os_str().as_bytes()).unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        source.as_ptr(),
                        target.as_ptr(),
                        std::ptr::null(),
                        libc::MS_BIND,
                        std::ptr::null(),
                    )
                },
                0,
                "actual native bind prerequisite"
            );
            Self(target)
        }
    }
    impl Drop for Binding {
        fn drop(&mut self) {
            assert_eq!(
                unsafe { libc::umount2(self.0.as_ptr(), 0) },
                0,
                "held source descriptors must close before retirement"
            );
        }
    }
    #[test]
    fn actual_bind_alias_coordinates_refuse_scratch_overlap_and_preserve_siblings() {
        const CHILD: &str = "TILO_SCRATCH_COORDINATE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let witness = tempfile::tempdir().unwrap();
            std::fs::set_permissions(witness.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
            let marker = witness.path().join("complete");
            let mut child = std::process::Command::new("/usr/bin/unshare")
                .args(["--mount", "--propagation", "private"])
                .arg(std::env::current_exe().unwrap())
                .args(["--exact", SELECTOR, "--nocapture"])
                .env(CHILD, &marker)
                .spawn()
                .unwrap();
            let deadline = tilo_test_env::hang_deadline();
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success());
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("actual bind-coordinate qualification timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                std::fs::read(marker).unwrap(),
                b"real aliases refused; disjoint siblings retained; mounts retired"
            );
            return;
        }
        assert_eq!(
            rustix::process::geteuid().as_raw(),
            0,
            "isolated native root prerequisite"
        );
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let scratch_path = source.join("scratch");
        let sibling = root.path().join("sibling");
        let alias = root.path().join("source-alias");
        let scratch_alias = root.path().join("scratch-alias");
        for path in [&source, &scratch_path, &sibling, &alias, &scratch_alias] {
            std::fs::create_dir(path).unwrap();
        }
        let file = scratch_path.join("selected-file");
        std::fs::write(&file, b"original mounted file").unwrap();
        let file_alias = root.path().join("file-alias");
        std::fs::write(&file_alias, b"underlying mountpoint").unwrap();
        let source_bind = Binding::new(&source, &alias);
        let scratch_bind = Binding::new(&scratch_path, &scratch_alias);
        let file_bind = Binding::new(&file, &file_alias);
        {
            let directory = File::open(&scratch_alias).unwrap();
            let scratch = Scratch {
                identity: identity(&directory).unwrap(),
                directory,
                allocation_bytes: 1,
            };
            for path in [&source, &alias, &file, &file_alias] {
                assert_eq!(
                    scratch
                        .refuse_mounted_overlap(&reopen(path).unwrap())
                        .unwrap_err()
                        .to_string(),
                    "Inventory scratch overlaps its mounted migration source"
                );
            }
            scratch
                .refuse_mounted_overlap(&reopen(&sibling).unwrap())
                .unwrap();
            let sibling_file = sibling.join("document");
            std::fs::write(&sibling_file, b"disjoint selected bytes").unwrap();
            scratch
                .refuse_mounted_overlap(&reopen(&sibling_file).unwrap())
                .unwrap();
            let held = reopen(&sibling_file).unwrap();
            std::fs::rename(&sibling_file, sibling.join("old-document")).unwrap();
            std::fs::write(&sibling_file, b"replacement").unwrap();
            // The held descriptor remains named old-document; prove its actual
            // coordinate rather than mistaking the replacement for the grant.
            scratch.refuse_mounted_overlap(&held).unwrap();
            std::fs::remove_file(sibling.join("old-document")).unwrap();
            assert!(scratch.refuse_mounted_overlap(&held).is_err());
        }
        drop(file_bind);
        drop(scratch_bind);
        drop(source_bind);
        assert_eq!(std::fs::read(&file).unwrap(), b"original mounted file");
        assert_eq!(
            std::fs::read(&file_alias).unwrap(),
            b"underlying mountpoint"
        );
        root.close().unwrap();
        std::fs::write(
            std::env::var_os(CHILD).unwrap(),
            b"real aliases refused; disjoint siblings retained; mounts retired",
        )
        .unwrap();
    }
    struct Subvolume {
        parent: File,
        name: CString,
        path: PathBuf,
        created: bool,
    }
    impl Subvolume {
        fn new(parent: &Path, name: &str) -> Self {
            let mut value = Self {
                parent: File::open(parent).unwrap(),
                name: CString::new(name).unwrap(),
                path: parent.join(name),
                created: false,
            };
            value.operation(0x5000_940e).unwrap();
            value.created = true; // BTRFS_IOC_SUBVOL_CREATE
            value
        }
        fn operation(&self, request: libc::c_ulong) -> io::Result<()> {
            // Linux UAPI btrfs_ioctl_vol_args: i64 fd, char name[4088].
            let mut args = [0u8; 4096];
            let name = self.name.as_bytes_with_nul();
            args[8..8 + name.len()].copy_from_slice(name);
            if unsafe { libc::ioctl(self.parent.as_raw_fd(), request, args.as_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
    impl Subvolume {
        fn retire(&mut self) -> io::Result<()> {
            if !self.created {
                return Ok(());
            }
            match std::fs::remove_dir_all(self.path.join("scratch")) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            self.operation(0x5000_940f)?; // BTRFS_IOC_SNAP_DESTROY
            self.created = false;
            Ok(())
        }
    }
    impl Drop for Subvolume {
        fn drop(&mut self) {
            if self.retire().is_err() {
                if std::thread::panicking() {
                    eprintln!("owned Btrfs fixture retirement failed during unwind");
                } else {
                    panic!("owned Btrfs fixture retirement failed");
                }
            }
        }
    }
    struct OwnedChild(Option<std::process::Child>);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                // Keep exact task custody until a successful wait observes
                // retirement. The outer native runner provides the deadline.
                loop {
                    let _ = child.kill();
                    match child.wait() {
                        Ok(_) => return,
                        Err(_) => {
                            eprintln!("Btrfs fixture child retirement remains unobserved");
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn actual_btrfs_subvolume_coordinates_refuse_ancestor_and_alias_overlap() {
        const CHILD: &str = "TILO_BTRFS_COORDINATE_CHILD";
        const EXACT: &str = "owned_service::capture::inventory_scratch::coordinate::tests::actual_btrfs_subvolume_coordinates_refuse_ancestor_and_alias_overlap";
        if std::env::var_os(CHILD).is_none() {
            let witness = tempfile::tempdir().unwrap();
            let marker = witness.path().join("complete");
            let executable = File::open("/proc/self/exe").unwrap();
            let executable_path =
                format!("/proc/{}/fd/{}", std::process::id(), executable.as_raw_fd());
            let mut child = OwnedChild(Some(
                std::process::Command::new("/usr/bin/unshare")
                    .args(["--mount", "--propagation", "private"])
                    .arg(executable_path)
                    .args(["--exact", EXACT, "--nocapture"])
                    .env(CHILD, &marker)
                    .spawn()
                    .unwrap(),
            ));
            let deadline = tilo_test_env::hang_deadline();
            loop {
                if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
                    child.0.take(); // try_wait observed and reaped this exact child.
                    assert!(status.success());
                    break;
                }
                if Instant::now() >= deadline {
                    // OwnedChild kills/reaps the exact child while unwinding.
                    panic!("Btrfs coordinate fixture timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(std::fs::read(marker).unwrap(), b"actual subvolume ancestors and aliases refused; sibling retained; owned mounts and subvolumes retired");
            return;
        }
        assert_eq!(rustix::process::geteuid().as_raw(), 0);
        // The native runner supplies only this private writable Btrfs witness,
        // independently from the readonly signed artifacts and SDK closure.
        let witness = Path::new("/run/tilo/btrfs-coordinate-witness");
        let held = reopen(witness).unwrap();
        let facts = held.metadata().unwrap();
        assert!(facts.is_dir());
        assert_eq!(facts.uid(), 0);
        assert_eq!(facts.gid(), 0);
        assert_eq!(facts.mode() & 0o7777, 0o700);
        let root = tempfile::tempdir_in(witness).unwrap();
        assert_eq!(
            identity(&reopen(witness).unwrap()).unwrap(),
            identity(&held).unwrap()
        );
        assert_eq!(
            filesystem_type(&File::open(root.path()).unwrap()).unwrap(),
            libc::BTRFS_SUPER_MAGIC,
            "genuine Btrfs native prerequisite must be observable"
        );
        // This fixture runs rootless: its namespace CAP_SYS_ADMIN does not
        // satisfy Btrfs ioctl's initial-user-namespace capable() check.
        // Require the actual superblock option before creating any subvolume.
        let mount_id = identity(&held).unwrap().2;
        let mut table = Vec::new();
        File::open("/proc/self/mountinfo")
            .unwrap()
            .take((TABLE_MAX + 1) as u64)
            .read_to_end(&mut table)
            .unwrap();
        assert!(table.len() <= TABLE_MAX);
        let rows: Vec<_> = table
            .split(|byte| *byte == b'\n')
            .filter(|line| {
                line.split(|byte| *byte == b' ')
                    .next()
                    .is_some_and(|id| number(id).is_ok_and(|id| id == mount_id))
            })
            .collect();
        assert_eq!(rows.len(), 1);
        let fields: Vec<_> = rows[0].split(|byte| *byte == b' ').collect();
        let separator = fields.iter().position(|field| *field == b"-").unwrap();
        assert_eq!(fields.len(), separator + 4);
        assert_eq!(fields[separator + 1], b"btrfs");
        assert!(
            fields[separator + 3]
                .split(|byte| *byte == b',')
                .any(|option| option == b"user_subvol_rm_allowed"),
            "rootless Btrfs destroy prerequisite absent; no subvolume created"
        );
        assert_eq!(
            identity(&reopen(witness).unwrap()).unwrap(),
            identity(&held).unwrap()
        );
        let mut source = Subvolume::new(root.path(), "source");
        let mut sibling = Subvolume::new(root.path(), "sibling");
        std::fs::create_dir(source.path.join("scratch")).unwrap();
        std::fs::create_dir(sibling.path.join("scratch")).unwrap();
        let mut nested = Subvolume::new(&source.path, "nested");
        let scratch_path = nested.path.join("scratch");
        std::fs::create_dir(&scratch_path).unwrap();
        let alias = root.path().join("alias");
        std::fs::create_dir(&alias).unwrap();
        let binding = Binding::new(&scratch_path, &alias);
        {
            let directory = File::open(&alias).unwrap();
            let scratch = Scratch {
                identity: identity(&directory).unwrap(),
                directory,
                allocation_bytes: 1,
            };
            let ancestor = reopen(root.path()).unwrap();
            let original = reopen(&scratch_path).unwrap();
            assert_ne!(
                ancestor.metadata().unwrap().dev(),
                original.metadata().unwrap().dev(),
                "real subvolume anonymous devices must exercise cross-device ancestry"
            );
            for path in [
                root.path(),
                &source.path,
                &nested.path,
                &scratch_path,
                &alias,
            ] {
                assert_eq!(
                    scratch
                        .refuse_mounted_overlap(&reopen(path).unwrap())
                        .unwrap_err()
                        .to_string(),
                    "Inventory scratch overlaps its mounted migration source"
                );
            }
            scratch
                .refuse_mounted_overlap(&reopen(&sibling.path).unwrap())
                .unwrap();
        }
        drop(binding);
        nested.retire().unwrap();
        sibling.retire().unwrap();
        source.retire().unwrap();
        drop(nested);
        drop(sibling);
        drop(source);
        root.close().unwrap();
        std::fs::write(std::env::var_os(CHILD).unwrap(), b"actual subvolume ancestors and aliases refused; sibling retained; owned mounts and subvolumes retired").unwrap();
    }
}
