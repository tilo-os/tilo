//! Linux [`Platform`]: `sfdisk`/`partx` for the partition map, raw
//! `O_DIRECT|O_EXCL` I/O through `/dev/disk/by-partuuid/<guid>`, and
//! efivarfs for firmware variables. Plus the unprivileged Linux probe.
//!
//! Test harnesses (loop disk, fake efivarfs directory) may point the adapter
//! elsewhere with `TILO_STAGING_TEST_DISK` and `TILO_STAGING_TEST_EFIVARS`,
//! honoured only when [`super::helper::test_overrides_allowed`].

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use ui::time::Duration;

use super::efi::GLOBAL_GUID;
use super::linux_parse::{self, MountSource, PowerSupply};
use super::platform::{Disk, IoResult, NewPartition, Platform, SystemVolume};
use super::probe::{Blocker, HostFacts};

const EFIVARS: &str = "/sys/firmware/efi/efivars";
/// `_IO(0x12, 97)`: flush and invalidate the block device buffer cache.
const BLKFLSBUF: libc::c_ulong = 0x1261;
/// `_IOR(0x12, 114, size_t)`.
const BLKGETSIZE64: libc::c_ulong = 0x8008_1272;
const FS_IMMUTABLE_FL: libc::c_int = 0x10;
const DIRECT_ALIGN: usize = 4096;

pub struct LinuxPlatform {
    /// Whole-disk node, e.g. `/dev/nvme0n1`.
    disk: PathBuf,
    efivars: PathBuf,
    /// Partitions are looked up by PARTUUID below this directory.
    by_partuuid: PathBuf,
    /// Where block nodes are made when neither udev nor devtmpfs provides one.
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

/// Kernel name (`nvme0n1p3`, `dm-0`) of a mount source.
fn source_name(source: &MountSource) -> Option<String> {
    let link = match source {
        MountSource::Path(p) => std::fs::canonicalize(p).ok()?,
        MountSource::Number(major, minor) => {
            std::fs::canonicalize(format!("/sys/dev/block/{major}:{minor}")).ok()?
        }
    };
    Some(link.file_name()?.to_string_lossy().into_owned())
}

/// The whole disk holding the running OS: every one of `/sysroot`, `/`,
/// `/boot` and `/boot/efi` that is a block mount must resolve (through
/// LUKS, LVM or other dm layers) to the same single disk.
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

impl LinuxPlatform {
    /// The adapter for this host's system disk.
    pub fn open() -> Result<Self, String> {
        let overrides = super::helper::test_overrides_allowed();
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
        Ok(Self {
            disk,
            efivars: env("TILO_STAGING_TEST_EFIVARS").unwrap_or_else(|| PathBuf::from(EFIVARS)),
            by_partuuid: PathBuf::from("/dev/disk/by-partuuid"),
            // Below /dev: temporary filesystems are often mounted nodev.
            private_nodes: PathBuf::from(format!("/dev/.tilo-staging-{}", std::process::id())),
        })
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
        Ok(sectors * 512)
    }

    fn logical_sector(&self) -> IoResult<u64> {
        let name = self.disk_name()?;
        read_trimmed(&PathBuf::from(format!(
            "/sys/class/block/{name}/queue/logical_block_size"
        )))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "logical sector size unreadable".into())
    }

    /// Unprivileged fallback: lsblk (udev database) plus sysfs extents.
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

    fn lock_disk(&self) -> IoResult<advisory_lock::FileLock> {
        let file = File::open(&self.disk).map_err(|e| e.to_string())?;
        advisory_lock::FileLock::exclusive(file).map_err(|e| e.to_string())
    }

    fn settle(&self) {
        let _ = Command::new("udevadm")
            .args(["settle", "--timeout=10"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// The node for partition `number` of our disk: udev's by-partuuid link
    /// when present, else `/dev/<name>` found through sysfs, else a node made
    /// from sysfs `dev` in a private directory (containers without udev or a
    /// live devtmpfs). `open_partition` proves the identity either way.
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
            // SAFETY: valid NUL-terminated path; makedev is pure arithmetic.
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

    /// Open a staging partition's node and prove it is the expected
    /// partition of our disk before any byte moves.
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
            || start != Some(p.first_lba * per)
            || rc != 0
            || size != (p.last_lba - p.first_lba + 1) * disk.sector_bytes
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
                // Plain directories (test doubles) do not support flags.
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

/// A heap buffer aligned for `O_DIRECT`.
struct Aligned {
    storage: Vec<u8>,
    at: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> Self {
        let storage = vec![0_u8; len + DIRECT_ALIGN];
        let at = storage.as_ptr().align_offset(DIRECT_ALIGN);
        Self { storage, at, len }
    }
    fn bytes(&mut self) -> &mut [u8] {
        &mut self.storage[self.at..self.at + self.len]
    }
}

impl Platform for LinuxPlatform {
    fn system_disk(&self) -> IoResult<Disk> {
        let size = self.size_bytes()?;
        let sector = self.logical_sector()?;
        // Root (and `disk` group members) read the real GPT header;
        // everyone else gets the udev view with standard usable bounds.
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
            spec.last_lba - spec.first_lba + 1,
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
        let deadline = ui::time::now() + Duration::from_secs(10);
        let mut added = false;
        while self.partition_node(&guid, p.number).is_err() {
            if !added {
                let _ = run(Command::new("partx")
                    .args(["-a", "--nr", &p.number.to_string()])
                    .arg(&self.disk));
                added = true;
            }
            self.settle();
            if ui::time::now() > deadline {
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
        // An exclusive open fails with EBUSY while anything mounts or holds
        // the partition; deleting its GPT entry then would leave a live
        // filesystem over unallocated space.
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
        self.settle();
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
        let mut buf = Aligned::new(data.len());
        buf.bytes().copy_from_slice(data);
        file.write_all_at(buf.bytes(), offset)
            .map_err(|e| format!("write {partition_guid}: {e}"))?;
        file.sync_data().map_err(|e| e.to_string())
    }

    fn read_partition(
        &self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> IoResult<()> {
        let file = self.open_partition(disk, partition_guid, false)?;
        let mut aligned = Aligned::new(buf.len());
        file.read_exact_at(aligned.bytes(), offset)
            .map_err(|e| format!("read {partition_guid}: {e}"))?;
        buf.copy_from_slice(aligned.bytes());
        Ok(())
    }

    fn sync_disk(&mut self, _disk: &Disk) -> IoResult<()> {
        let file = File::open(&self.disk).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        // SAFETY: BLKFLSBUF takes no argument beyond the descriptor.
        if unsafe { libc::ioctl(file.as_raw_fd(), BLKFLSBUF as _, 0) } != 0 {
            let error = std::io::Error::last_os_error();
            // A regular-file test disk has no buffer cache ioctl.
            if error.raw_os_error() != Some(libc::ENOTTY) {
                return Err(error.to_string());
            }
        }
        Ok(())
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
        // A regular test directory keeps stale tail bytes on overwrite.
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

/// The unprivileged Linux probe (spec §5).
pub fn probe_facts() -> HostFacts {
    let platform = match LinuxPlatform::open() {
        Ok(p) => p,
        Err(_) if !Path::new("/sys/firmware/efi").is_dir() => {
            return HostFacts::unsupported(Blocker::NotUefi);
        }
        Err(_) => return HostFacts::unsupported(Blocker::DiskUnsupported),
    };
    // A test efivars directory (overrides only) stands in for the firmware.
    let efivars_overridden = platform.efivars != Path::new(EFIVARS);
    if !Path::new("/sys/firmware/efi").is_dir()
        && !(efivars_overridden && platform.efivars.is_dir())
    {
        return HostFacts::unsupported(Blocker::NotUefi);
    }
    if removable(&platform.disk) {
        return HostFacts::unsupported(Blocker::DiskUnsupported);
    }
    let disk = match platform.system_disk() {
        Ok(d) => d,
        Err(e) if e == "not-gpt" => return HostFacts::unsupported(Blocker::NotGpt),
        Err(_) => return HostFacts::unsupported(Blocker::DiskUnsupported),
    };
    let Ok(firmware) = super::helper::read_firmware(&platform) else {
        return HostFacts::unsupported(Blocker::NotUefi);
    };
    let mut facts = HostFacts {
        basis: Some(super::plan::Basis {
            disk,
            system_volume: None,
            firmware,
        }),
        bitlocker_protected: false,
        blockers: Vec::new(),
    };
    if linux_parse::on_low_battery(&power_supplies()) {
        facts.block(Blocker::OnBattery);
    }
    facts
}
