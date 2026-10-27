//! Fresh, nonrecursive detached copies of already authenticated held roots.
//! Neither caller paths nor serialized records can construct this custody.
use super::{
    AsRawFd, Component, File, FromRawFd, Inputs, MetadataExt, OpenOptionsExt, OsStrExt, Path,
    PathBuf, Projection, RawFd, check_input, identity, io, refused, root, validate,
};
use sha2::{Digest, Sha256};

pub(in crate::owned_service::capture::original) struct DetachedRoots {
    files: Vec<File>,
    projections: Vec<Projection>,
    originals: Vec<File>,
    topologies: Vec<Topology>,
}
impl Inputs {
    pub(in crate::owned_service::capture::original) fn prepare_launch(
        &self,
    ) -> io::Result<DetachedRoots> {
        self.check()?;
        let mut result = DetachedRoots {
            files: Vec::with_capacity(self.files.len()),
            projections: Vec::with_capacity(self.projections.len()),
            originals: Vec::with_capacity(self.files.len()),
            topologies: Vec::with_capacity(self.files.len()),
        };
        for (source, expected) in self.files.iter().zip(&self.projections) {
            check_input(source)?;
            if identity(source)? != expected.identity
                || super::super::super::inventory_scratch::identity(source)?.2 != expected.mount
            {
                return Err(refused("Approved root changed before detached copy"));
            }
            let topology = Topology::capture(source)?;
            // SAFETY: empty lookup refers exclusively to the held authenticated
            // descriptor. No AT_RECURSIVE: descendant mounts are not delegated.
            let raw = unsafe {
                libc::syscall(
                    libc::SYS_open_tree,
                    source.as_raw_fd(),
                    c"".as_ptr(),
                    libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC | libc::AT_EMPTY_PATH as u32,
                )
            };
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // Successful open_tree descriptors are kernel int values.
            let raw = raw as RawFd;
            // SAFETY: open_tree returned this new uniquely owned mount descriptor.
            let file = unsafe { File::from_raw_fd(raw) };
            check_input(&file)?;
            topology.recheck(source)?;
            let mount = super::super::super::inventory_scratch::identity(&file)?.2;
            if identity(&file)? != expected.identity || mount == 0 || mount == expected.mount {
                return Err(refused("Detached approved root identity differs"));
            }
            let mut projection = expected.clone();
            projection.mount = mount;
            result.files.push(file);
            result.projections.push(projection);
            result.originals.push(source.try_clone()?);
            result.topologies.push(topology);
            // Recheck live original descriptors; cloning never replaces their custody.
            self.check()?;
        }
        result.check()?;
        self.check()?;
        Ok(result)
    }
}
impl DetachedRoots {
    pub(in crate::owned_service::capture::original) fn projections(&self) -> Vec<Projection> {
        self.projections.clone()
    }
    pub(in crate::owned_service::capture::original) fn pairs(&self) -> Vec<(RawFd, RawFd)> {
        self.files
            .iter()
            .zip(&self.projections)
            .map(|(file, projection)| (file.as_raw_fd(), projection.descriptor))
            .collect()
    }
    pub(in crate::owned_service::capture::original) fn check(&self) -> io::Result<()> {
        root()?;
        validate(&self.projections)?;
        if self.files.len() != self.projections.len()
            || self.files.is_empty()
            || self.originals.len() != self.files.len()
            || self.topologies.len() != self.files.len()
        {
            return Err(refused("Detached approved root descriptors incomplete"));
        }
        for ((file, expected), (original, topology)) in self
            .files
            .iter()
            .zip(&self.projections)
            .zip(self.originals.iter().zip(&self.topologies))
        {
            check_input(file)?;
            check_input(original)?;
            if identity(original)? != expected.identity {
                return Err(refused(
                    "Original approved root identity changed before launch",
                ));
            }
            topology.recheck(original)?;
            if identity(file)? != expected.identity
                || super::super::super::inventory_scratch::identity(file)?.2 != expected.mount
            {
                return Err(refused("Detached approved root descriptor changed"));
            }
        }
        Ok(())
    }
}

const TOPOLOGY_MAX: usize = 4 * 1024 * 1024;
struct Topology {
    namespace: File,
    namespace_id: (u64, u64),
    source: (u64, u64, u64),
    name: PathBuf,
    table_sha256: [u8; 32],
}
impl Topology {
    fn capture(source: &File) -> io::Result<Self> {
        use std::io::Read;
        let namespace = File::open("/proc/self/ns/mnt")?;
        let namespace_id = (namespace.metadata()?.dev(), namespace.metadata()?.ino());
        let held = super::super::super::inventory_scratch::identity(source)?;
        let name = std::fs::read_link(format!("/proc/self/fd/{}", source.as_raw_fd()))?;
        literal(&name)?;
        let mut table = Vec::new();
        File::open("/proc/self/mountinfo")?
            .take((TOPOLOGY_MAX + 1) as u64)
            .read_to_end(&mut table)?;
        if table.len() > TOPOLOGY_MAX {
            return Err(refused("Approved root mount topology exceeds bound"));
        }
        let mut selected = false;
        let mut ids = std::collections::BTreeSet::new();
        for line in table
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let fields = line.split(|byte| *byte == b' ').collect::<Vec<_>>();
            if fields.len() < 10 || !fields.contains(&b"-".as_slice()) {
                return Err(refused("Approved root mount topology is malformed"));
            }
            let id = number(fields.first().ok_or(io::ErrorKind::InvalidData)?)?;
            if id == 0 || !ids.insert(id) {
                return Err(refused("Approved root mount identifier differs"));
            }
            number(fields.get(1).ok_or(io::ErrorKind::InvalidData)?)?;
            let point = decode(fields.get(4).ok_or(io::ErrorKind::InvalidData)?)?;
            literal(&point)?;
            if id != held.2 && point.starts_with(&name) {
                return Err(refused(
                    "Approved root contains an unapproved descendant mount",
                ));
            }
            if id == held.2 {
                let device = fields
                    .get(2)
                    .ok_or(io::ErrorKind::InvalidData)?
                    .split(|byte| *byte == b':')
                    .collect::<Vec<_>>();
                if device.len() != 2
                    || (
                        number(device.first().ok_or(io::ErrorKind::InvalidData)?)?,
                        number(device.get(1).ok_or(io::ErrorKind::InvalidData)?)?,
                    ) != (
                        u64::from(libc::major(held.0)),
                        u64::from(libc::minor(held.0)),
                    )
                    || !name.starts_with(&point)
                {
                    return Err(refused("Approved root kernel mount coordinate differs"));
                }
                literal(&decode(fields.get(3).ok_or(io::ErrorKind::InvalidData)?)?)?;
                selected = true;
            }
        }
        if !selected {
            return Err(refused("Approved root mount is absent from namespace"));
        }
        let result = Self {
            namespace,
            namespace_id,
            source: held,
            name,
            table_sha256: Sha256::digest(&table).into(),
        };
        result.check_descriptor(source)?;
        Ok(result)
    }
    fn check_descriptor(&self, source: &File) -> io::Result<()> {
        let current_ns = File::open("/proc/self/ns/mnt")?;
        let named = File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.name)?;
        if (current_ns.metadata()?.dev(), current_ns.metadata()?.ino()) != self.namespace_id
            || (
                self.namespace.metadata()?.dev(),
                self.namespace.metadata()?.ino(),
            ) != self.namespace_id
            || super::super::super::inventory_scratch::identity(source)? != self.source
            || super::super::super::inventory_scratch::identity(&named)? != self.source
            || std::fs::read_link(format!("/proc/self/fd/{}", source.as_raw_fd()))? != self.name
        {
            return Err(refused("Approved root topology descriptor changed"));
        }
        Ok(())
    }
    fn recheck(&self, source: &File) -> io::Result<()> {
        self.check_descriptor(source)?;
        let current = Self::capture(source)?;
        if current.source != self.source
            || current.namespace_id != self.namespace_id
            || current.name != self.name
            || current.table_sha256 != self.table_sha256
        {
            return Err(refused(
                "Approved root mount topology changed during detached copy",
            ));
        }
        Ok(())
    }
}
fn number(bytes: &[u8]) -> io::Result<u64> {
    std::str::from_utf8(bytes)
        .map_err(io::Error::other)?
        .parse()
        .map_err(io::Error::other)
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
        return Err(refused("Approved root topology coordinate is not literal"));
    }
    Ok(())
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
            .ok_or_else(|| refused("Approved root mount escape is truncated"))?;
        decoded.push(match encoded {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return Err(refused("Approved root mount escape is unsupported")),
        });
        remaining = rest;
    }
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&decoded)))
}

#[cfg(all(test, feature = "managed-tree-test"))]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};
    const MODE: &str = "TILO_DETACHED_ROOT_NATIVE_PROBE";
    const SELECTOR: &str = "owned_service::capture::original::roots::detached::tests::fresh_launch_mounts_refuse_unapproved_children_and_preserve_base";
    struct OwnedChild(Option<Child>);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    struct Mounted(CString);
    impl Mounted {
        fn tmpfs(path: &Path) -> Self {
            let target = CString::new(path.as_os_str().as_bytes()).unwrap();
            // SAFETY: all syscall inputs are live literals or fixture pathnames.
            assert_eq!(
                unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        target.as_ptr(),
                        c"tmpfs".as_ptr(),
                        libc::MS_NOSUID | libc::MS_NODEV,
                        c"size=16m,mode=0755".as_ptr().cast(),
                    )
                },
                0,
                "actual fixture tmpfs mount: {}",
                io::Error::last_os_error()
            );
            Self(target)
        }
        fn readonly(&self) {
            // SAFETY: the owned fixture mount is the literal remount target.
            assert_eq!(
                unsafe {
                    libc::mount(
                        std::ptr::null(),
                        self.0.as_ptr(),
                        std::ptr::null(),
                        libc::MS_REMOUNT
                            | libc::MS_RDONLY
                            | libc::MS_NOSUID
                            | libc::MS_NODEV
                            | libc::MS_NOEXEC,
                        std::ptr::null(),
                    )
                },
                0,
                "actual fixture readonly mount: {}",
                io::Error::last_os_error()
            );
        }
    }
    impl Drop for Mounted {
        fn drop(&mut self) {
            // SAFETY: only the exact owned fixture mount is retired.
            assert_eq!(
                unsafe { libc::umount2(self.0.as_ptr(), 0) },
                0,
                "owned detached fixture retirement: {}",
                io::Error::last_os_error()
            );
        }
    }
    fn child() {
        assert_eq!(
            rustix::process::geteuid().as_raw(),
            0,
            "native root prerequisite required"
        );
        let directory = tempfile::tempdir().unwrap();
        let source_mount = Mounted::tmpfs(directory.path());
        let nested_path = directory.path().join("child");
        std::fs::create_dir(&nested_path).unwrap();
        std::fs::write(nested_path.join("under"), b"underlying approved data").unwrap();
        let nested_mount = Mounted::tmpfs(&nested_path);
        std::fs::write(nested_path.join("over"), b"unapproved mounted data").unwrap();
        source_mount.readonly();
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.path())
            .unwrap();
        let device = file.metadata().unwrap().dev();
        let inputs = Inputs::admit(
            vec![OriginalMountedInput::from_reviewed("selected".into(), file).unwrap()],
            device,
        )
        .unwrap();
        let original = inputs.projections();
        assert_eq!(
            inputs
                .prepare_launch()
                .err()
                .expect("actual descendant mount must refuse before cloning")
                .to_string(),
            "Approved root contains an unapproved descendant mount"
        );
        assert_eq!(inputs.projections(), original);
        drop(nested_mount);
        let first = inputs.prepare_launch().unwrap();
        let second = inputs.prepare_launch().unwrap();
        assert_eq!(
            inputs.projections(),
            original,
            "base custody must remain unchanged"
        );
        assert_ne!(first.projections()[0].mount, second.projections()[0].mount);
        for lease in [&first, &second] {
            lease.check().unwrap();
            assert_eq!(lease.projections()[0].identity, original[0].identity);
            let (source, slot) = lease.pairs()[0];
            assert_eq!(slot, FIRST_FD);
            assert_eq!(
                unsafe { libc::fcntl(source, libc::F_GETFD) } & libc::FD_CLOEXEC,
                libc::FD_CLOEXEC
            );
            // SAFETY: openat names fixed leaves of the actual held detached root.
            let raw = unsafe {
                libc::openat(
                    source,
                    c"child/under".as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC,
                )
            };
            assert!(
                raw >= 0,
                "approved child bytes are readable only after owned nested mount retirement"
            );
            let mut contents = String::new();
            unsafe { File::from_raw_fd(raw) }
                .read_to_string(&mut contents)
                .unwrap();
            assert_eq!(contents, "underlying approved data");
            let absent = unsafe {
                libc::openat(
                    source,
                    c"child/over".as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC,
                )
            };
            if absent >= 0 {
                drop(unsafe { File::from_raw_fd(absent) });
            }
            assert_eq!(
                absent, -1,
                "retired unapproved child mount bytes must remain absent"
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ENOENT)
            );
        }
        drop((first, second, inputs));
        drop(source_mount);
    }
    #[test]
    fn fresh_launch_mounts_refuse_unapproved_children_and_preserve_base() {
        if std::env::var_os(MODE).is_some() {
            child();
            return;
        }
        let mut command = Command::new("/usr/bin/unshare");
        command
            .args(["--mount", "--propagation", "private"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", SELECTOR, "--nocapture", "--test-threads=1"])
            .env(MODE, "1")
            .stdin(Stdio::null());
        let parent = unsafe { libc::getpid() };
        // SAFETY: child setup contains only async-signal-safe syscalls.
        unsafe {
            command.pre_exec(move || {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                Ok(())
            });
        }
        let mut owned = OwnedChild(Some(command.spawn().unwrap()));
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(status) = owned.0.as_mut().unwrap().try_wait().unwrap() {
                owned.0.take();
                assert!(
                    status.success(),
                    "actual detached mount probe failed: {status}"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "owned detached mount probe timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
