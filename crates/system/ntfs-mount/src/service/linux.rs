//! The service's hands on a real Linux machine.
//!
//! - Discovery reads sysfs, udev's database and each candidate's boot
//!   sector; nothing here loads or uses a file-system driver.
//! - The preflight runs in confined helpers: this program's `check-volume`
//!   for the disk-check flag, then `ntfs-3g.probe` for hibernation, Fast
//!   Startup and an unfinished journal.
//! - "Open" loads `ntfs3` explicitly (filesystem autoloading of it is
//!   blacklisted) and mounts it here.
//! - "Open safely" mounts a FUSE connection here and gives it to `ntfs-3g`,
//!   which runs confined (see [`super::sandbox`]). `ntfs-3g` always mounts
//!   for itself, which a confined process cannot do, so a small preloaded
//!   library answers its `mount` call: it reports that the volume opened,
//!   waits for this service to mount the connection, and puts that
//!   connection where `ntfs-3g` expects its own. Nothing is mounted for a
//!   volume `ntfs-3g` could not open.
//!
//! Every mount is `nosuid,nodev,noexec` and reachable only by the person who
//! opened it.

use std::collections::BTreeSet;
use std::ffi::{CString, OsString};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{
    DirBuilderExt, FileExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::peer::PeerPolicy;
use super::sandbox::{self, Account, Confinement, Device};
use super::{Backend, Candidate, Health, MountRequest};
use crate::boot::{self, BootDisks};
use crate::identity::{BootSector, Identity, UdevProperties};
use crate::policy::DriveFacts;
use crate::protocol::{Mode, ReadOnlyReason};
use crate::topology;

/// The helper's descriptors for the FUSE connection and the handoff socket,
/// which the preloaded library expects.
pub const HANDOFF_FUSE_FD: i32 = 4;
pub const HANDOFF_SOCKET_FD: i32 = 5;

/// How long `ntfs-3g` may take to open a volume before it is given up on.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a preflight helper may run.
const CHECK_TIMEOUT: Duration = Duration::from_secs(60);
/// How long the driver may take to flush and exit after its unmount.
const EXIT_TIMEOUT: Duration = Duration::from_secs(20);

/// Exit codes of `check-volume`.
pub const CHECK_CLEAN: i32 = 0;
pub const CHECK_DIRTY: i32 = 10;
pub const CHECK_MALFORMED: i32 = 11;

/// Where everything is.
#[derive(Clone, Debug)]
pub struct Config {
    pub sys_root: PathBuf,
    pub udev_data: PathBuf,
    pub dev_root: PathBuf,
    pub boot_disks: PathBuf,
    pub media_root: PathBuf,
    pub sandbox_dir: PathBuf,
    /// Open mounts, so a restarted service can tidy up after its
    /// predecessor.
    pub ledger: PathBuf,
    pub self_exe: PathBuf,
    pub ntfs_3g: PathBuf,
    pub ntfs_3g_probe: PathBuf,
    pub fuse_handoff: PathBuf,
    pub modprobe: PathBuf,
    pub helper_account: String,
    pub peer: PeerPolicy,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sys_root: PathBuf::from("/sys"),
            udev_data: PathBuf::from("/run/udev/data"),
            dev_root: PathBuf::from("/dev"),
            boot_disks: PathBuf::from("/run/tilo-ntfs-mount/boot-disks.json"),
            media_root: PathBuf::from("/run/media"),
            sandbox_dir: PathBuf::from("/run/tilo-ntfs-mount/sandbox"),
            ledger: PathBuf::from("/run/tilo-ntfs-mount/mounts"),
            self_exe: PathBuf::from("/usr/libexec/tilo-ntfs-mount"),
            ntfs_3g: PathBuf::from("/usr/bin/ntfs-3g"),
            ntfs_3g_probe: PathBuf::from("/usr/bin/ntfs-3g.probe"),
            fuse_handoff: PathBuf::from("/usr/lib/tilo/ntfs-mount/fuse-handoff.so"),
            modprobe: PathBuf::from("/usr/sbin/modprobe"),
            helper_account: "tilo-ntfs".into(),
            peer: PeerPolicy::default(),
        }
    }
}

/// What an open drive keeps alive.
pub enum Mount {
    Kernel,
    Safe { driver: Child },
}

pub struct LinuxBackend {
    config: Config,
    install_disks: BTreeSet<String>,
}

fn message(context: &str, error: &io::Error) -> String {
    format!("{context}: {error}")
}

fn user(uid: u32) -> io::Result<(String, u32)> {
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: `passwd` is plain old data; all-zero is a valid value.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live, correctly sized storage.
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if rc != 0 || found.is_null() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "no such account"));
    }
    // SAFETY: getpwuid_r succeeded, so pw_name points into `buffer`.
    let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }
        .to_string_lossy()
        .into_owned();
    Ok((name, entry.pw_gid))
}

fn account(name: &str) -> io::Result<Account> {
    let name = CString::new(name).map_err(io::Error::other)?;
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: `passwd` is plain old data; all-zero is a valid value.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live, correctly sized storage.
    let rc = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &raw mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut found,
        )
    };
    if rc != 0 || found.is_null() || entry.pw_uid == 0 || entry.pw_gid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the helper account is missing",
        ));
    }
    Ok(Account {
        uid: entry.pw_uid,
        gid: entry.pw_gid,
    })
}

fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)
}

fn umount(path: &Path, detach: bool) -> io::Result<()> {
    let target = c_path(path)?;
    let flags = if detach { libc::MNT_DETACH } else { 0 };
    // SAFETY: a NUL-terminated path.
    if unsafe { libc::umount2(target.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn mount(
    source: &Path,
    target: &Path,
    fstype: &str,
    flags: libc::c_ulong,
    data: &str,
) -> io::Result<()> {
    let (source, target) = (c_path(source)?, c_path(target)?);
    let fstype = CString::new(fstype).map_err(io::Error::other)?;
    let data = CString::new(data).map_err(io::Error::other)?;
    // SAFETY: NUL-terminated strings that outlive the call.
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            flags,
            data.as_ptr().cast(),
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A directory name for a label: printable, no separators, never hidden.
pub fn directory_name(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| if c == '/' || c.is_control() { '_' } else { c })
        .take(64)
        .collect();
    let trimmed = cleaned.trim().trim_start_matches('.').trim();
    if trimmed.is_empty() {
        "NTFS".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Wait for `child` to exit, up to `timeout`.
fn wait_for(child: &mut Child, timeout: Duration) -> io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What `ntfs-3g`'s exit code says about the volume.
fn driver_refusal(code: Option<i32>) -> String {
    match code {
        Some(14) => "Windows is hibernated or uses Fast Startup on this drive".into(),
        Some(15) => "Windows was not shut down properly on this drive".into(),
        Some(12) => "This drive does not hold an NTFS file system".into(),
        Some(13) => "This drive's file system is damaged. Check it in Windows.".into(),
        Some(16) => "This drive is in use".into(),
        Some(17) => "This drive is part of a RAID set, which cannot be opened here".into(),
        _ => "This drive could not be opened".into(),
    }
}

impl LinuxBackend {
    /// Set up the backend and tidy up after a predecessor that stopped
    /// with drives still mounted: their drivers stopped with it.
    pub fn new(config: Config) -> Self {
        let install_disks = fs::read_to_string("/proc/self/mountinfo")
            .ok()
            .and_then(|mountinfo| {
                ["/sysroot", "/"]
                    .iter()
                    .find_map(|target| boot::mount_source(&mountinfo, target).map(str::to_owned))
            })
            .and_then(|source| fs::canonicalize(source).ok())
            .and_then(|device| {
                device
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .map(|name| boot::physical_disks(&config.sys_root, &name))
            .unwrap_or_default();
        if let Ok(ledger) = fs::read_to_string(&config.ledger) {
            for path in ledger.lines().map(Path::new) {
                let _ = umount(path, true);
                let _ = fs::remove_dir(path);
            }
        }
        let _ = fs::remove_file(&config.ledger);
        Self {
            config,
            install_disks,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    fn ledger(&self, path: &Path, add: bool) {
        let mut paths: Vec<String> = fs::read_to_string(&self.config.ledger)
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or_default();
        let entry = path.to_string_lossy().into_owned();
        paths.retain(|existing| *existing != entry);
        if add {
            paths.push(entry);
        }
        let _ = fs::write(&self.config.ledger, paths.join("\n"));
    }

    fn candidate(&self, name: &str, boot_disks: &BootDisks) -> Option<Candidate> {
        let sys = &self.config.sys_root;
        let major_minor =
            fs::read_to_string(sys.join("class/block").join(name).join("dev")).ok()?;
        let props = UdevProperties::read(&self.config.udev_data, major_minor.trim()).ok()?;
        if props.fs_type() != Some("ntfs") {
            return None;
        }
        let topology = topology::read(sys, name).ok()?;
        if topology.virtual_device {
            return None;
        }
        let device = self.open_device(name, major_minor.trim(), false).ok()?;
        let mut sector = [0_u8; 512];
        device.read_exact_at(&mut sector, 0).ok()?;
        let boot_sector = BootSector::parse(&sector)?;
        let seq = boot::diskseq(sys, &topology.disk);
        let sectors: u64 = fs::read_to_string(sys.join("class/block").join(name).join("size"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .unwrap_or(0);
        let facts = DriveFacts {
            present_at_boot: boot_disks.contains(&topology.disk, seq),
            on_install_disk: self.install_disks.contains(&topology.disk),
            identity: Identity {
                drive: props.drive(),
                partition: props.partition(),
                volume: Some(boot_sector.volume_serial),
            },
            topology,
        };
        Some(Candidate {
            id: format!("{name}:{}", seq.unwrap_or(0)),
            name: name.to_owned(),
            label: props.label().unwrap_or_default().to_owned(),
            size_bytes: sectors.saturating_mul(512),
            facts,
        })
    }

    /// Open `/dev/<name>` and check it is the device sysfs reports.
    fn open_device(&self, name: &str, major_minor: &str, writable: bool) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(self.config.dev_root.join(name))?;
        let metadata = file.metadata()?;
        let rdev = metadata.rdev();
        let expected = major_minor
            .split_once(':')
            .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
            .map(|(major, minor): (u32, u32)| libc::makedev(major, minor));
        if metadata.file_type().is_block_device() && Some(rdev) == expected {
            Ok(file)
        } else {
            Err(io::Error::other("the device node does not match sysfs"))
        }
    }

    fn device_for(&self, candidate: &Candidate, writable: bool) -> io::Result<File> {
        let major_minor = fs::read_to_string(
            self.config
                .sys_root
                .join("class/block")
                .join(&candidate.name)
                .join("dev"),
        )?;
        self.open_device(&candidate.name, major_minor.trim(), writable)
    }

    fn helper(&self) -> Result<Account, String> {
        account(&self.config.helper_account)
            .map_err(|error| message("Opening safely is unavailable", &error))
    }

    fn run_check(child: io::Result<Child>) -> Result<Option<i32>, String> {
        let mut child = child.map_err(|error| message("The drive could not be checked", &error))?;
        if let Ok(Some(status)) = wait_for(&mut child, CHECK_TIMEOUT) {
            Ok(status.code())
        } else {
            let _ = child.kill();
            let _ = child.wait();
            Err("Checking the drive took too long".into())
        }
    }

    /// Make `/run/media/<user>/<name>` for `uid`, readable only by them.
    fn mount_point(&self, uid: u32, label: &str) -> io::Result<PathBuf> {
        let (name, gid) = user(uid)?;
        fs::create_dir_all(&self.config.media_root)?;
        let base = self.config.media_root.join(name);
        match fs::DirBuilder::new().mode(0o750).create(&base) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(&base)?;
        if !metadata.is_dir() || metadata.uid() != 0 {
            return Err(io::Error::other("the media directory is not the system's"));
        }
        std::os::unix::fs::chown(&base, Some(0), Some(gid))?;
        fs::set_permissions(&base, fs::Permissions::from_mode(0o750))?;
        let stem = directory_name(label);
        for attempt in 0_u32..100 {
            let name = if attempt == 0 {
                stem.clone()
            } else {
                format!("{stem} ({})", attempt.saturating_add(1))
            };
            let path = base.join(name);
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("no free mount point"))
    }

    fn mount_kernel(
        &self,
        request: &MountRequest<'_>,
        target: &Path,
        flags: libc::c_ulong,
    ) -> Result<Mount, String> {
        let status = Command::new(&self.config.modprobe)
            .arg("ntfs3")
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| message("The NTFS driver could not be loaded", &error))?;
        if !status.success() {
            return Err("The NTFS driver could not be loaded".into());
        }
        let (_, gid) = user(request.uid).map_err(|error| message("Unknown account", &error))?;
        let options = format!("uid={},gid={gid},umask=077,windows_names", request.uid);
        mount(
            &self.config.dev_root.join(&request.candidate.name),
            target,
            "ntfs3",
            flags,
            &options,
        )
        .map_err(|error| message("The drive could not be opened", &error))?;
        Ok(Mount::Kernel)
    }

    fn mount_safe(
        &self,
        request: &MountRequest<'_>,
        target: &Path,
        flags: libc::c_ulong,
    ) -> Result<Mount, String> {
        let helper = self.helper()?;
        let (_, gid) = user(request.uid).map_err(|error| message("Unknown account", &error))?;
        let device = self
            .device_for(request.candidate, false)
            .map_err(|error| message("The drive could not be opened", &error))?;
        let fuse: OwnedFd = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC)
            .open("/dev/fuse")
            .map_err(|error| message("Opening safely is unavailable", &error))?
            .into();
        let (mut ours, theirs) =
            UnixStream::pair().map_err(|error| message("The drive could not be opened", &error))?;
        let mut options = format!(
            "no_detach,uid={},gid={gid},umask=077,windows_names",
            request.uid
        );
        if request.read_only {
            options.push_str(",ro");
        }
        let args: Vec<OsString> = vec![
            "-o".into(),
            options.into(),
            self.config.sandbox_dir.join(sandbox::NODE).into(),
            self.config.sandbox_dir.join(sandbox::MOUNT_DIR).into(),
        ];
        let confinement = Confinement {
            sandbox_dir: &self.config.sandbox_dir,
            account: helper,
            device: &device,
            access: Device::Node {
                writable: !request.read_only,
            },
            inherit: vec![
                (fuse.as_raw_fd(), HANDOFF_FUSE_FD),
                (theirs.as_raw_fd(), HANDOFF_SOCKET_FD),
            ],
            read_paths: vec![self.config.fuse_handoff.clone()],
        };
        let mut driver = sandbox::spawn(
            &confinement,
            &self.config.ntfs_3g,
            &args,
            &[(
                "LD_PRELOAD",
                self.config.fuse_handoff.clone().into_os_string(),
            )],
        )
        .map_err(|error| message("Opening safely is unavailable", &error))?;
        drop(theirs);
        let refuse = |driver: &mut Child, reason: String| {
            let _ = driver.kill();
            let _ = driver.wait();
            reason
        };
        // The library writes one byte once ntfs-3g has opened the volume and
        // reached its mount call; an exit before that is a refusal.
        let _ = ours.set_read_timeout(Some(OPEN_TIMEOUT));
        let mut ready = [0_u8; 1];
        if ours.read_exact(&mut ready).is_err() || ready != *b"R" {
            let code = wait_for(&mut driver, Duration::from_secs(2))
                .ok()
                .flatten()
                .and_then(|status| status.code());
            return Err(refuse(&mut driver, driver_refusal(code)));
        }
        let data = format!(
            "fd={},rootmode=40000,user_id={},group_id={gid},default_permissions",
            fuse.as_raw_fd(),
            request.uid
        );
        if let Err(error) = mount(
            &self.config.dev_root.join(&request.candidate.name),
            target,
            "fuseblk",
            flags,
            &data,
        ) {
            return Err(refuse(
                &mut driver,
                message("The drive could not be opened", &error),
            ));
        }
        if ours.write_all(b"G").is_err() {
            let _ = umount(target, true);
            return Err(refuse(&mut driver, "The drive could not be opened".into()));
        }
        Ok(Mount::Safe { driver })
    }
}

impl Backend for LinuxBackend {
    type Mount = Mount;

    fn drives(&mut self) -> Result<Vec<Candidate>, String> {
        let boot_disks = BootDisks::load(&self.config.boot_disks);
        let entries = fs::read_dir(self.config.sys_root.join("class/block"))
            .map_err(|error| message("Drives cannot be listed", &error))?;
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        Ok(names
            .iter()
            .filter_map(|name| self.candidate(name, &boot_disks))
            .collect())
    }

    fn preflight(&mut self, candidate: &Candidate) -> Result<Health, String> {
        let helper = self.helper()?;
        let device = self
            .device_for(candidate, false)
            .map_err(|error| message("The drive could not be opened", &error))?;
        let check = |access: Device, program: &Path, args: &[OsString]| {
            sandbox::spawn(
                &Confinement {
                    sandbox_dir: &self.config.sandbox_dir,
                    account: helper,
                    device: &device,
                    access,
                    inherit: Vec::new(),
                    read_paths: Vec::new(),
                },
                program,
                args,
                &[],
            )
        };
        let dirty = match Self::run_check(check(
            Device::Fd,
            &self.config.self_exe,
            &["check-volume".into()],
        ))? {
            Some(CHECK_CLEAN) => false,
            Some(CHECK_DIRTY) => true,
            Some(CHECK_MALFORMED) => return Err(driver_refusal(Some(13))),
            _ => return Err("The drive could not be checked".into()),
        };
        // `--readwrite` is the mode in which ntfs-3g looks for hibernation
        // and an unfinished journal; the helper's node is writable for it.
        let probe = Self::run_check(check(
            Device::Node { writable: true },
            &self.config.ntfs_3g_probe,
            &[
                "--readwrite".into(),
                self.config.sandbox_dir.join(sandbox::NODE).into(),
            ],
        ))?;
        let read_only = match probe {
            Some(0) if dirty => Some(ReadOnlyReason::NeedsCheck),
            Some(0) => None,
            Some(14) => Some(ReadOnlyReason::WindowsHibernated),
            Some(15) => Some(ReadOnlyReason::WindowsNotShutDown),
            other => return Err(driver_refusal(other)),
        };
        Ok(Health { read_only })
    }

    fn mount(&mut self, request: &MountRequest<'_>) -> Result<(PathBuf, Mount), String> {
        let target = self
            .mount_point(request.uid, &request.candidate.label)
            .map_err(|error| message("No place to open the drive", &error))?;
        let mut flags = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
        if request.read_only {
            flags |= libc::MS_RDONLY;
        }
        let mounted = match request.mode {
            Mode::Open => self.mount_kernel(request, &target, flags),
            Mode::OpenSafely => self.mount_safe(request, &target, flags),
        };
        match mounted {
            Ok(handle) => {
                self.ledger(&target, true);
                Ok((target, handle))
            }
            Err(reason) => {
                let _ = fs::remove_dir(&target);
                Err(reason)
            }
        }
    }

    fn unmount(&mut self, path: &Path, mount: &mut Mount, gone: bool) -> Result<(), String> {
        match umount(path, gone) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(libc::EINVAL) => {}
            Err(error) if error.raw_os_error() == Some(libc::EBUSY) => {
                return Err("Files on this drive are still open".into());
            }
            Err(error) => return Err(message("The drive could not be closed", &error)),
        }
        if let Mount::Safe { driver } = mount {
            // The unmount asked ntfs-3g to write everything out; it exits
            // when done.
            if !matches!(wait_for(driver, EXIT_TIMEOUT), Ok(Some(_))) {
                let _ = driver.kill();
                let _ = driver.wait();
            }
        }
        let _ = fs::remove_dir(path);
        self.ledger(path, false);
        Ok(())
    }

    fn alive(&mut self, mount: &mut Mount) -> bool {
        match mount {
            Mount::Kernel => true,
            Mount::Safe { driver } => matches!(driver.try_wait(), Ok(None)),
        }
    }

    fn owner_present(&self, uid: u32) -> bool {
        self.config.peer.has_session(uid)
    }

    fn new_grant_id(&mut self) -> String {
        let mut bytes = [0_u8; 16];
        let filled =
            File::open("/dev/urandom").and_then(|mut random| random.read_exact(&mut bytes));
        if filled.is_err() {
            // Unique enough within one person's list when randomness fails.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            bytes = now.to_le_bytes();
        }
        bytes.iter().fold(String::new(), |mut id, byte| {
            let _ = write!(id, "{byte:02x}");
            id
        })
    }
}

/// `check-volume`: decide the disk-check flag of the volume on descriptor
/// [`sandbox::DEVICE_FD`], inside the confined helper.
pub fn check_volume() -> i32 {
    use std::os::fd::FromRawFd;
    // SAFETY: the service hands the volume over as this descriptor.
    let device = unsafe { File::from_raw_fd(sandbox::DEVICE_FD) };
    match crate::health::volume_is_dirty(|offset, buffer| device.read_exact_at(buffer, offset)) {
        Ok(false) => CHECK_CLEAN,
        Ok(true) => CHECK_DIRTY,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => CHECK_MALFORMED,
        Err(_) => 12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_are_named_after_labels_without_escaping_their_directory() {
        assert_eq!(directory_name("Windows"), "Windows");
        assert_eq!(directory_name("../etc"), "_etc");
        assert_eq!(directory_name("a/b"), "a_b");
        assert_eq!(directory_name("  "), "NTFS");
        assert_eq!(directory_name(".hidden"), "hidden");
        assert_eq!(directory_name("x\ny"), "x_y");
        assert_eq!(directory_name(&"L".repeat(300)).len(), 64);
    }

    #[test]
    fn driver_exit_codes_become_reasons() {
        assert!(driver_refusal(Some(14)).contains("hibernated"));
        assert!(driver_refusal(Some(15)).contains("shut down"));
        assert_eq!(driver_refusal(None), "This drive could not be opened");
    }
}
