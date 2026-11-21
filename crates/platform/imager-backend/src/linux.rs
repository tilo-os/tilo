//! Linux [`Platform`]: `sfdisk`/`partx` for the partition map, raw `O_DIRECT|O_EXCL` I/O through
//! `/dev/disk/by-partuuid/<guid>`, and efivarfs for firmware variables.
//!
//! Test harnesses may point the adapter at a loop disk and a fake efivarfs directory through
//! `TILO_STAGING_TEST_DISK` and `TILO_STAGING_TEST_EFIVARS`, honoured only when
//! [`super::firmware::test_overrides_allowed`].

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tilo_clock::time::Duration;

use super::efi::GLOBAL_GUID;
use super::linux_parse::{self, MountSource, PowerSupply};
use super::model::{Blocker, HostFacts};
use super::platform::{Disk, IoResult, NewPartition, Platform, SystemVolume};

#[path = "linux_btrfs.rs"]
mod btrfs;

const EFIVARS: &str = "/sys/firmware/efi/efivars";
/// `_IO(0x12, 97)`: flush and invalidate the block device buffer cache.
const BLKFLSBUF: libc::c_ulong = 0x1261;
const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;
const FS_IMMUTABLE_FL: libc::c_int = 0x10;
const DIRECT_ALIGN: usize = 4096;

pub struct LinuxPlatform {
    btrfs: Option<btrfs::Held>,
    /// Whole-disk node, e.g. `/dev/nvme0n1`.
    disk: PathBuf,
    efivars: PathBuf,
    /// Partitions are looked up by PARTUUID below this directory.
    by_partuuid: PathBuf,
    private_nodes: PathBuf,
}

impl Drop for LinuxPlatform {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.private_nodes);
    }
}

fn run(command: &mut Command) -> IoResult<String> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", command.get_program().display()))?;
    if !output.status.success() {
        return Err(format!(
            "{} failed: {}",
            command.get_program().display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn source_name(source: &MountSource) -> Option<String> {
    let link = match source {
        MountSource::Path(p) => std::fs::canonicalize(p).ok()?,
        MountSource::Number(major, minor) => {
            std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).ok()?
        }
    };
    Some(link.file_name()?.to_string_lossy().into_owned())
}

pub fn resolve_system_disk() -> Result<PathBuf, Blocker> {
    let info =
        std::fs::read_to_string("/proc/self/mountinfo").map_err(|_| Blocker::DiskUnsupported)?;
    let mut disks: Vec<String> = Vec::new();
    for point in ["/sysroot", "/", "/boot", "/boot/efi"] {
        let Some(source) = linux_parse::mount_source(&info, point) else {
            continue;
        };
        let Some(name) = source_name(&source) else {
            continue;
        };
        let json = run(Command::new("lsblk")
            .args(["-J", "-s", "-o", "NAME,TYPE"])
            .arg(format!("/dev/{name}")))
        .map_err(|_| Blocker::DiskUnsupported)?;
        for disk in linux_parse::backing_disks(&json).map_err(|_| Blocker::DiskUnsupported)? {
            if !disks.contains(&disk) {
                disks.push(disk);
            }
        }
    }
    match disks.as_slice() {
        [one] => Ok(PathBuf::from(format!("/dev/{one}"))),
        _ => Err(Blocker::DiskUnsupported),
    }
}

/// The whole disks that back this namespace's mounts: a named staging target
/// whose disk is among them is in use.
#[cfg(target_os = "linux")]
fn mounted_backing_disks() -> Result<Vec<String>, String> {
    let info = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| format!("/proc/self/mountinfo: {e}"))?;
    let mut resolve = |name: &str| -> Result<Vec<String>, String> {
        // `mounted_disks` hands a `/dev/` source without its prefix, or the
        // mount's device number when the source is not a path.
        let node = match name.split_once(':').and_then(|(major, minor)| {
            Some((major.parse::<u32>().ok()?, minor.parse::<u32>().ok()?))
        }) {
            Some((major, minor)) => {
                let sys = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
                let real =
                    std::fs::canonicalize(&sys).map_err(|e| format!("{}: {e}", sys.display()))?;
                let device = real
                    .file_name()
                    .ok_or_else(|| format!("{} has no device name", sys.display()))?;
                PathBuf::from("/dev").join(device)
            }
            None => PathBuf::from("/dev").join(name),
        };
        backing_disks_of(&node)
    };
    crate::staging::target::mounted_disks(&info, &mut resolve)
}

#[cfg(target_os = "linux")]
fn backing_disks_of(node: &Path) -> Result<Vec<String>, String> {
    let json = match Command::new("lsblk")
        .args(["-J", "-s", "-o", "NAME,TYPE"])
        .arg(node)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        // A device that vanished from this namespace backs no disk of ours.
        _ => return Ok(Vec::new()),
    };
    match linux_parse::backing_disks(&json) {
        Ok(disks) => Ok(disks),
        // RAID and multipath mounts cannot be attributed to one whole disk;
        // they never make a plain disk look free, so they resolve to nothing.
        Err(_) => Ok(Vec::new()),
    }
}

/// Probe the disk a request names, without write authority.
#[cfg(target_os = "linux")]
pub fn probe_target(target: &crate::staging::target::TargetDisk) -> Result<HostFacts, String> {
    let platform = LinuxPlatform::open_target(target)?;
    if removable(&platform.disk) {
        return Err(format!("{target} is removable"));
    }
    if !Path::new("/sys/firmware/efi").is_dir() && !platform.efivars.is_dir() {
        return Err("firmware is not UEFI".into());
    }
    let disk = match platform.system_disk() {
        Ok(disk) => disk,
        Err(e) if e == "not-gpt" => return Err(format!("{target} has no GPT partition table")),
        Err(e) => return Err(format!("{target}: {e}")),
    };
    let firmware = super::firmware::read_firmware(&platform)?;
    let mut facts = HostFacts {
        basis: Some(super::plan::Basis {
            disk,
            system_volume: None,
            linux_btrfs_volume: None,
            firmware,
        }),
        bitlocker_protected: false,
        blockers: Vec::new(),
    };
    if !facts
        .basis
        .as_ref()
        .is_some_and(|basis| basis.firmware.uefi)
    {
        facts.block(Blocker::NotUefi);
    }
    if linux_parse::on_low_battery(&power_supplies()) {
        facts.block(Blocker::OnBattery);
    }
    Ok(facts)
}

impl LinuxPlatform {
    /// The adapter for this host's system disk: errors when the disk or retained filesystem cannot
    /// be opened.
    pub fn open() -> Result<Self, String> {
        let overrides = super::firmware::test_overrides_allowed();
        let env = |k: &str| {
            overrides
                .then(|| std::env::var_os(k))
                .flatten()
                .map(PathBuf::from)
        };
        let disk = match env("TILO_STAGING_TEST_DISK") {
            Some(d) => d,
            None => resolve_system_disk()
                .map_err(|b| format!("{b:?}: the system disk is unsupported"))?,
        };
        let mut platform = Self::at(disk);
        platform.btrfs = btrfs::Held::discover(&platform)?;
        Ok(platform)
    }

    /// The adapter for the disk a staging request names by its identity
    /// (`serial:<serial>` or `wwid:<wwn>`): the unallocated part of that disk,
    /// never this machine's own system disk unless the request named it.
    ///
    /// A disk with a mounted partition is refused: staging never writes beside
    /// contents in use.
    pub fn open_target(target: &crate::staging::target::TargetDisk) -> Result<Self, String> {
        let disk = match Self::test_disk() {
            Some(disk) => disk,
            None => {
                crate::staging::target::resolve(target, &crate::staging::target::Roots::default())?
            }
        };
        let platform = Self::at(disk);
        let name = platform.disk_name()?;
        let in_use = mounted_backing_disks()
            .map_err(|e| format!("{target}: the mount table is unreadable: {e}"))?;
        if in_use.contains(&name) {
            return Err(format!(
                "{target} ({name}) has a mounted partition; unmount it before staging"
            ));
        }
        Ok(platform)
    }

    /// The adapter for the disk a request's plan names: its recorded identity
    /// when it was staged onto a named disk, its device path otherwise.
    pub fn open_on(id: &str) -> Result<Self, String> {
        if id.is_empty() {
            return Err("the plan names no disk".into());
        }
        if let Ok(target) = crate::staging::target::TargetDisk::parse(id) {
            Self::open_target(&target)
        } else {
            let disk = match Self::test_disk() {
                Some(disk) => disk,
                None => PathBuf::from(id),
            };
            let mut platform = Self::at(disk);
            platform.btrfs = btrfs::Held::discover(&platform)?;
            Ok(platform)
        }
    }

    fn test_disk() -> Option<PathBuf> {
        super::firmware::test_overrides_allowed()
            .then(|| std::env::var_os("TILO_STAGING_TEST_DISK"))
            .flatten()
            .map(PathBuf::from)
    }

    fn at(disk: PathBuf) -> Self {
        let efivars = super::firmware::test_overrides_allowed()
            .then(|| std::env::var_os("TILO_STAGING_TEST_EFIVARS"))
            .flatten()
            .map_or_else(|| PathBuf::from(EFIVARS), PathBuf::from);
        Self {
            btrfs: None,
            disk,
            efivars,
            by_partuuid: PathBuf::from("/dev/disk/by-partuuid"),
            private_nodes: PathBuf::from(format!("/dev/.tilo-staging-{}", std::process::id())),
        }
    }

    fn disk_name(&self) -> IoResult<String> {
        let real = std::fs::canonicalize(&self.disk).map_err(|e| e.to_string())?;
        Ok(real
            .file_name()
            .ok_or("disk has no name")?
            .to_string_lossy()
            .into_owned())
    }

    fn size_bytes(&self) -> IoResult<u64> {
        let name = self.disk_name()?;
        let sectors: u64 = read_trimmed(&PathBuf::from(format!("/sys/class/block/{name}/size")))
            .and_then(|s| s.parse().ok())
            .ok_or("disk size unreadable")?;
        sectors
            .checked_mul(512)
            .ok_or_else(|| "disk size overflow".into())
    }

    fn logical_sector(&self) -> IoResult<u64> {
        let name = self.disk_name()?;
        read_trimmed(&PathBuf::from(format!(
            "/sys/class/block/{name}/queue/logical_block_size"
        )))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "logical sector size unreadable".into())
    }

    fn lsblk_disk(&self) -> IoResult<Disk> {
        let json = run(Command::new("lsblk")
            .args([
                "-J",
                "-b",
                "-o",
                "NAME,PATH,TYPE,SIZE,LOG-SEC,RM,PTUUID,PTTYPE,PARTUUID,PARTTYPE,PARTN",
            ])
            .arg(&self.disk))?;
        let l = linux_parse::lsblk_disk(&json)?;
        let disk = l.name.clone();
        linux_parse::disk_from_lsblk(&l, &|part| {
            let base = PathBuf::from(format!("/sys/class/block/{disk}/{part}"));
            let start = read_trimmed(&base.join("start"))?.parse().ok()?;
            let size = read_trimmed(&base.join("size"))?.parse().ok()?;
            Some((start, size))
        })
    }

    fn lock_disk(&self) -> IoResult<Option<advisory_lock::FileLock>> {
        if self.btrfs.is_some() {
            return Ok(None);
        }
        let file = File::open(&self.disk).map_err(|e| e.to_string())?;
        advisory_lock::FileLock::exclusive(file)
            .map(Some)
            .map_err(|e| e.to_string())
    }

    fn settle() {
        let _ = Command::new("udevadm")
            .args(["settle", "--timeout=10"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// udev's by-partuuid link, else `/dev/<name>` from sysfs, else a node made in a private
    /// directory; `open_partition` proves the identity either way.
    fn partition_node(&self, guid: &str, number: u32) -> IoResult<PathBuf> {
        let link = self.by_partuuid.join(guid.to_ascii_lowercase());
        if link.exists() {
            return Ok(link);
        }
        let disk = self.disk_name()?;
        let entries = std::fs::read_dir(format!("/sys/class/block/{disk}"))
            .map_err(|e| format!("{disk}: {e}"))?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if read_trimmed(&path.join("partition")) != Some(number.to_string()) {
                continue;
            }
            let name = entry.file_name();
            let dev = PathBuf::from("/dev").join(&name);
            if dev.exists() {
                return Ok(dev);
            }
            let (major, minor) = read_trimmed(&path.join("dev"))
                .and_then(|d| {
                    let (a, b) = d.split_once(':')?;
                    Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?))
                })
                .ok_or("partition device number unreadable")?;
            std::fs::create_dir_all(&self.private_nodes).map_err(|e| e.to_string())?;
            std::fs::set_permissions(
                &self.private_nodes,
                std::os::unix::fs::PermissionsExt::from_mode(0o700),
            )
            .map_err(|e| e.to_string())?;
            let node = self.private_nodes.join(&name);
            let _ = std::fs::remove_file(&node);
            let c_node = std::ffi::CString::new(node.as_os_str().as_encoded_bytes())
                .map_err(|e| e.to_string())?;
            let rc = unsafe {
                libc::mknod(
                    c_node.as_ptr(),
                    libc::S_IFBLK | 0o600,
                    libc::makedev(major, minor),
                )
            };
            if rc != 0 {
                return Err(format!(
                    "mknod {}: {}",
                    node.display(),
                    std::io::Error::last_os_error()
                ));
            }
            return Ok(node);
        }
        Err(format!(
            "the kernel does not expose partition {number} of {disk}"
        ))
    }

    /// Proves it is the expected partition of our disk before any byte moves.
    fn open_partition(&self, disk: &Disk, guid: &str, write: bool) -> IoResult<File> {
        let p = disk
            .partitions
            .iter()
            .find(|p| super::same_guid(&p.guid, guid))
            .ok_or_else(|| format!("no partition {guid}"))?;
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(write)
            .custom_flags(libc::O_EXCL | libc::O_DIRECT | libc::O_CLOEXEC);
        let file = options
            .open(self.partition_node(guid, p.number)?)
            .map_err(|e| format!("open partition {guid}: {e}"))?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        let rdev = meta.rdev();
        let (major, minor) = (libc::major(rdev), libc::minor(rdev));
        let sys = PathBuf::from(format!("/sys/dev/block/{major}:{minor}"));
        let parent = std::fs::canonicalize(&sys)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
        let start: Option<u64> = read_trimmed(&sys.join("start")).and_then(|s| s.parse().ok());
        let per = disk.sector_bytes / 512;
        let mut size = 0_u64;
        // SAFETY: BLKGETSIZE64 writes one u64 through the valid pointer.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), BLKGETSIZE64 as _, &mut size) };
        if parent.as_deref() != Some(self.disk_name()?.as_str())
            || start
                != Some(
                    p.first_lba
                        .checked_mul(per)
                        .ok_or("partition start overflow")?,
                )
            || rc != 0
            || size
                != p.last_lba
                    .checked_sub(p.first_lba)
                    .and_then(|size| size.checked_add(1))
                    .and_then(|size| size.checked_mul(disk.sector_bytes))
                    .ok_or("partition size overflow")?
        {
            return Err(format!("{guid} does not resolve to the planned partition"));
        }
        Ok(file)
    }

    fn efivar_path(&self, name: &str) -> IoResult<PathBuf> {
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!("invalid EFI variable name {name:?}"));
        }
        Ok(self.efivars.join(format!("{name}-{GLOBAL_GUID}")))
    }

    /// efivarfs marks most variables immutable; clear it as efibootmgr does.
    fn make_mutable(path: &Path) -> IoResult<()> {
        let file = match File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.to_string()),
        };
        let mut flags: libc::c_int = 0;
        // SAFETY: GETFLAGS/SETFLAGS read and write one int through valid pointers.
        unsafe {
            if libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETFLAGS as _, &mut flags) != 0 {
                return Ok(());
            }
            if flags & FS_IMMUTABLE_FL != 0 {
                flags &= !FS_IMMUTABLE_FL;
                if libc::ioctl(file.as_raw_fd(), libc::FS_IOC_SETFLAGS as _, &flags) != 0 {
                    return Err(std::io::Error::last_os_error().to_string());
                }
            }
        }
        Ok(())
    }
}

struct Aligned {
    storage: Vec<u8>,
    at: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> IoResult<Self> {
        let capacity = len
            .checked_add(DIRECT_ALIGN)
            .ok_or("aligned buffer size overflow")?;
        let storage = vec![0_u8; capacity];
        let at = storage.as_ptr().align_offset(DIRECT_ALIGN);
        if at >= DIRECT_ALIGN {
            return Err("buffer alignment unavailable".into());
        }
        Ok(Self { storage, at, len })
    }
    fn bytes(&mut self) -> IoResult<&mut [u8]> {
        self.storage
            .get_mut(self.at..)
            .and_then(|rest| rest.get_mut(..self.len))
            .ok_or_else(|| "aligned buffer bounds invalid".into())
    }
}

/// The path a staged root's file is placed at: a bounded, relative one, so a
/// request can never write outside the mounted filesystem.
fn staging_file_path(path: &str) -> IoResult<PathBuf> {
    let relative = path.trim_start_matches('/');
    if relative.is_empty()
        || path.len() > 256
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.bytes().any(|b| b.is_ascii_control())
    {
        return Err(format!("{path:?} is not a place inside a staged root"));
    }
    Ok(PathBuf::from(relative))
}

impl LinuxPlatform {
    /// Mount the staged partition `guid` (read-only when asked) long enough to
    /// place or read a file inside it, in the adapter's own private directory,
    /// which its `Drop` removes.
    fn with_staging_mount<T>(
        &self,
        disk: &Disk,
        guid: &str,
        read_only: bool,
        body: impl FnOnce(&Path) -> IoResult<T>,
    ) -> IoResult<T> {
        let partition = disk
            .partitions
            .iter()
            .find(|p| super::same_guid(&p.guid, guid))
            .cloned()
            .ok_or_else(|| format!("no partition {guid}"))?;
        let node = self.partition_node(guid, partition.number)?;
        std::fs::create_dir_all(&self.private_nodes).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&self.private_nodes, PermissionsExt::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        let dir = self
            .private_nodes
            .join(format!("mount-{}", guid.to_ascii_lowercase()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let options = if read_only {
            "ro,nosuid,nodev,noexec"
        } else {
            "rw,nosuid,nodev"
        };
        let mounted = Command::new("mount")
            .args(["-o", options])
            .arg(&node)
            .arg(&dir)
            .output();
        let mounted = match mounted {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = mounted {
            let _ = std::fs::remove_dir(&dir);
            return Err(format!(
                "the staged root cannot be mounted to place its enrollment token: {error}"
            ));
        }
        let outcome = body(&dir);
        let unmounted = Command::new("umount")
            .arg(&dir)
            .output()
            .map_err(|e| e.to_string())
            .and_then(|output| {
                output
                    .status
                    .success()
                    .then_some(())
                    .ok_or_else(|| String::from_utf8_lossy(&output.stderr).trim().to_owned())
            });
        let _ = std::fs::remove_dir(&dir);
        match (outcome, unmounted) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(format!("the staged root did not unmount: {error}")),
            (Ok(value), Ok(_)) => Ok(value),
        }
    }
}

impl Platform for LinuxPlatform {
    fn system_disk(&self) -> IoResult<Disk> {
        let size = self.size_bytes()?;
        let sector = self.logical_sector()?;
        let mut disk = match run(Command::new("sfdisk").arg("--json").arg(&self.disk)) {
            Ok(json) => linux_parse::disk_from_sfdisk(&json, size, sector)?,
            Err(_) => self.lsblk_disk()?,
        };
        disk.id = self.disk.to_string_lossy().into_owned();
        Ok(disk)
    }

    fn system_volume(&self) -> IoResult<Option<SystemVolume>> {
        Ok(None)
    }

    fn linux_btrfs_volume(&self) -> IoResult<Option<super::platform::LinuxBtrfsVolume>> {
        self.btrfs.as_ref().map(|held| held.facts(self)).transpose()
    }
    fn resize_linux_btrfs_filesystem(&mut self, guid: &str, bytes: u64) -> IoResult<()> {
        self.btrfs
            .as_ref()
            .ok_or("no admitted Btrfs root")?
            .resize_filesystem(self, guid, bytes)
    }
    fn resize_linux_btrfs_partition(&mut self, guid: &str, bytes: u64) -> IoResult<()> {
        self.btrfs
            .as_ref()
            .ok_or("no admitted Btrfs root")?
            .resize_partition(self, guid, bytes)
    }

    fn shrink_system_volume(&mut self, _: &str, _: u64) -> IoResult<()> {
        Err("Linux hosts use existing free space only".into())
    }

    fn grow_system_volume(&mut self, _: &str, _: u64) -> IoResult<()> {
        Err("Linux hosts use existing free space only".into())
    }

    fn create_partition(&mut self, disk: &Disk, spec: &NewPartition) -> IoResult<String> {
        let guid = spec
            .guid
            .clone()
            .ok_or("Linux partitions are created with a planned GUID")?
            .to_ascii_lowercase();
        if !super::is_guid(&guid) || !super::is_guid(&spec.type_guid.to_ascii_lowercase()) {
            return Err("invalid GUID".into());
        }
        if !spec
            .name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b' ')
        {
            return Err("invalid partition name".into());
        }
        let _lock = self.lock_disk()?;
        let current = self.system_disk()?;
        if !super::same_guid(&current.guid, &disk.guid) {
            return Err("the disk changed identity".into());
        }
        let line = format!(
            "start={}, size={}, type={}, uuid={}, name=\"{}\"\n",
            spec.first_lba,
            spec.last_lba
                .checked_sub(spec.first_lba)
                .and_then(|size| size.checked_add(1))
                .ok_or("invalid partition extent")?,
            spec.type_guid.to_ascii_uppercase(),
            guid.to_ascii_uppercase(),
            spec.name
        );
        let mut child = Command::new("sfdisk")
            .args(["--append", "--no-reread", "--wipe=never"])
            .arg(&self.disk)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("sfdisk: {e}"))?;
        child
            .stdin
            .take()
            .ok_or("sfdisk stdin")?
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(format!(
                "sfdisk --append failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let created = self.system_disk()?;
        let p = created
            .partitions
            .iter()
            .find(|p| super::same_guid(&p.guid, &guid))
            .ok_or("the new partition did not appear in the table")?;
        let _ = run(Command::new("partx").arg("-u").arg(&self.disk));
        let deadline = tilo_clock::now()
            .checked_add(Duration::from_secs(10))
            .ok_or("partition wait deadline overflow")?;
        let mut added = false;
        while self.partition_node(&guid, p.number).is_err() {
            if !added {
                let _ = run(Command::new("partx")
                    .args(["-a", "--nr", &p.number.to_string()])
                    .arg(&self.disk));
                added = true;
            }
            Self::settle();
            if tilo_clock::now() > deadline {
                return Err("the kernel did not expose the new partition".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(guid)
    }

    fn delete_partition(&mut self, disk: &Disk, partition_guid: &str) -> IoResult<()> {
        let _lock = self.lock_disk()?;
        let current = self.system_disk()?;
        if !super::same_guid(&current.guid, &disk.guid) {
            return Err("the disk changed identity".into());
        }
        let p = current
            .partitions
            .iter()
            .find(|p| super::same_guid(&p.guid, partition_guid))
            .ok_or("partition already gone")?;
        drop(
            self.open_partition(&current, partition_guid, false)
                .map_err(|e| format!("staging partition is in use; unmount it first ({e})"))?,
        );
        let number = p.number.to_string();
        run(Command::new("sfdisk")
            .args(["--no-reread", "--delete"])
            .arg(&self.disk)
            .arg(&number))?;
        let _ = run(Command::new("partx")
            .args(["-d", "--nr", &number])
            .arg(&self.disk));
        Self::settle();
        Ok(())
    }

    fn write_partition(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        data: &[u8],
    ) -> IoResult<()> {
        let file = self.open_partition(disk, partition_guid, true)?;
        let mut buf = Aligned::new(data.len())?;
        buf.bytes()?.copy_from_slice(data);
        // Durable once the caller syncs the disk (`sync_disk`), as every step does before it is
        // recorded: a flush per chunk made a component thousands of device flushes, each one a
        // flush of the whole image file under a virtual machine.
        file.write_all_at(buf.bytes()?, offset)
            .map_err(|e| format!("write {partition_guid}: {e}"))
    }

    fn write_staging_file(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        contents: &[u8],
        mode: u32,
    ) -> IoResult<()> {
        let relative = staging_file_path(path)?;
        self.with_staging_mount(disk, partition_guid, false, |dir| {
            let file_path = dir.join(&relative);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true).mode(mode);
            let mut file = options
                .open(&file_path)
                .map_err(|e| format!("{}: {e}", file_path.display()))?;
            file.write_all(contents).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::set_permissions(&file_path, PermissionsExt::from_mode(mode))
                .map_err(|e| e.to_string())?;
            if let Some(parent) = file_path.parent() {
                File::open(parent)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    }

    fn read_staging_file(
        &self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
    ) -> IoResult<Option<Vec<u8>>> {
        let relative = staging_file_path(path)?;
        self.with_staging_mount(disk, partition_guid, true, |dir| {
            let file_path = dir.join(&relative);
            match std::fs::read(&file_path) {
                Ok(bytes) if bytes.len() <= 64 * 1024 => Ok(Some(bytes)),
                Ok(_) => Err("the staged root's enrollment token is unexpectedly large".into()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(format!("{}: {error}", file_path.display())),
            }
        })
    }

    fn write_staging_file_from(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        source: &std::path::Path,
        mode: u32,
    ) -> IoResult<u64> {
        let relative = staging_file_path(path)?;
        let mut input = super::staging::request_io::open_input(&source.to_string_lossy())?;
        self.with_staging_mount(disk, partition_guid, false, |dir| {
            let file_path = dir.join(&relative);
            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true).mode(mode);
            let mut file = options
                .open(&file_path)
                .map_err(|e| format!("{}: {e}", file_path.display()))?;
            let copied = std::io::copy(&mut input, &mut file).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            std::fs::set_permissions(&file_path, PermissionsExt::from_mode(mode))
                .map_err(|e| e.to_string())?;
            if let Some(parent) = file_path.parent() {
                File::open(parent)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|e| e.to_string())?;
            }
            Ok(copied)
        })
    }

    fn staging_file_matches(
        &self,
        disk: &Disk,
        partition_guid: &str,
        path: &str,
        source: &std::path::Path,
    ) -> IoResult<bool> {
        const CHUNK: usize = 1 << 20;
        let relative = staging_file_path(path)?;
        let mut input = super::staging::request_io::open_input(&source.to_string_lossy())?;
        let expected = input
            .metadata()
            .map_err(|e| format!("{}: {e}", source.display()))?;
        self.with_staging_mount(disk, partition_guid, true, |dir| {
            let file_path = dir.join(&relative);
            let mut placed = match File::open(&file_path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(format!("{}: {error}", file_path.display())),
            };
            let size = placed.metadata().map_err(|e| e.to_string())?.len();
            if size != expected.len() {
                return Ok(false);
            }
            let mut wanted = vec![0_u8; CHUNK];
            let mut found = vec![0_u8; CHUNK];
            loop {
                let n = input.read(&mut wanted).map_err(|e| e.to_string())?;
                if n == 0 {
                    return Ok(true);
                }
                let mut done = 0;
                while done < n {
                    let got = placed
                        .read(&mut found[done..n])
                        .map_err(|e| e.to_string())?;
                    if got == 0 || found[done..done + got] != wanted[done..done + got] {
                        return Ok(false);
                    }
                    done += got;
                }
            }
        })
    }

    fn read_partition(
        &self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> IoResult<()> {
        let file = self.open_partition(disk, partition_guid, false)?;
        let mut aligned = Aligned::new(buf.len())?;
        file.read_exact_at(aligned.bytes()?, offset)
            .map_err(|e| format!("read {partition_guid}: {e}"))?;
        buf.copy_from_slice(aligned.bytes()?);
        Ok(())
    }

    fn sync_disk(&mut self, _disk: &Disk) -> IoResult<()> {
        let file = File::open(&self.disk).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        // SAFETY: BLKFLSBUF takes no argument beyond the descriptor.
        if unsafe { libc::ioctl(file.as_raw_fd(), BLKFLSBUF as _, 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ENOTTY) {
                return Err(error.to_string());
            }
        }
        Ok(())
    }

    fn has_uefi(&self) -> bool {
        Path::new("/sys/firmware/efi").is_dir()
            || (self.efivars != Path::new(EFIVARS) && self.efivars.is_dir())
    }

    fn get_efi_var(&self, name: &str) -> IoResult<Option<Vec<u8>>> {
        let path = self.efivar_path(name)?;
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{name}: {e}")),
        };
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(64 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        linux_parse::efivar_payload(&bytes).map(Some)
    }

    fn set_efi_var(&mut self, name: &str, data: &[u8]) -> IoResult<()> {
        let path = self.efivar_path(name)?;
        Self::make_mutable(&path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| format!("{name}: {e}"))?;
        // efivarfs requires attributes + data in a single write().
        let bytes = linux_parse::efivar_file(data);
        let written = file.write(&bytes).map_err(|e| format!("{name}: {e}"))?;
        if written != bytes.len() {
            return Err(format!("{name}: short efivarfs write"));
        }
        let _ = file.set_len(bytes.len() as u64);
        Ok(())
    }

    fn delete_efi_var(&mut self, name: &str) -> IoResult<()> {
        let path = self.efivar_path(name)?;
        Self::make_mutable(&path)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{name}: {e}")),
        }
    }
}

fn power_supplies() -> Vec<PowerSupply> {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|e| {
            let p = e.path();
            PowerSupply {
                kind: read_trimmed(&p.join("type")).unwrap_or_default(),
                online: read_trimmed(&p.join("online")).map(|s| s == "1"),
                status: read_trimmed(&p.join("status")),
                capacity: read_trimmed(&p.join("capacity")).and_then(|s| s.parse().ok()),
            }
        })
        .collect()
}

fn removable(disk: &Path) -> bool {
    let Some(name) = std::fs::canonicalize(disk)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
    else {
        return true;
    };
    read_trimmed(&PathBuf::from(format!("/sys/class/block/{name}/removable"))).as_deref()
        == Some("1")
}

/// The unprivileged Linux probe.
pub fn probe_facts() -> HostFacts {
    let platform = match LinuxPlatform::open() {
        Ok(p) => p,
        Err(_) if !Path::new("/sys/firmware/efi").is_dir() => {
            return HostFacts::unsupported(Blocker::NotUefi);
        }
        Err(_) => return HostFacts::unsupported(Blocker::DiskUnsupported),
    };
    if removable(&platform.disk) {
        return HostFacts::unsupported(Blocker::DiskUnsupported);
    }
    let disk = match platform.system_disk() {
        Ok(d) => d,
        Err(e) if e == "not-gpt" => return HostFacts::unsupported(Blocker::NotGpt),
        Err(_) => return HostFacts::unsupported(Blocker::DiskUnsupported),
    };
    let Ok(firmware) = super::firmware::read_firmware(&platform) else {
        return HostFacts::unsupported(Blocker::NotUefi);
    };
    let mut facts = HostFacts {
        basis: Some(super::plan::Basis {
            disk,
            system_volume: None,
            linux_btrfs_volume: match platform.linux_btrfs_volume() {
                Ok(volume) => volume,
                Err(_) => return HostFacts::unsupported(Blocker::DiskUnsupported),
            },
            firmware,
        }),
        bitlocker_protected: false,
        blockers: Vec::new(),
    };
    if facts
        .basis
        .as_ref()
        .is_some_and(|basis| !basis.firmware.uefi)
    {
        facts.block(Blocker::NotUefi);
    }
    if linux_parse::on_low_battery(&power_supplies()) {
        facts.block(Blocker::OnBattery);
    }
    facts
}
