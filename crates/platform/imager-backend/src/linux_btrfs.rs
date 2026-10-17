//! Held single-device Btrfs root used only by the canonical staging helper.
use super::{LinuxPlatform, run};
use crate::platform::{IoResult, LinuxBtrfsVolume, Platform};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::Command;
const RESERVE: u64 = 4 << 30;

pub(super) struct Held {
    root: File,
    namespace: File,
    mount_id: u64,
    target: String,
    device: File,
    device_path: PathBuf,
    guid: String,
    first_lba: u64,
    sector: u64,
    fsid: String,
    disk_guid: String,
    disk: File,
    disk_path: PathBuf,
    _lock: advisory_lock::FileLock,
}

fn partition_bytes(partition: &crate::platform::Partition, sector: u64) -> IoResult<u64> {
    partition
        .last_lba
        .checked_sub(partition.first_lba)
        .and_then(|size| size.checked_add(1))
        .and_then(|size| size.checked_mul(sector))
        .ok_or_else(|| "Btrfs partition geometry overflow".into())
}

fn mount_line(target: &str) -> IoResult<(u64, String)> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|_| "Btrfs mount table unavailable")?;
    let mut matching = text.lines().filter_map(|line| {
        let (left, right) = line.split_once(" - ")?;
        let fields: Vec<_> = left.split_whitespace().collect();
        let after: Vec<_> = right.split_whitespace().collect();
        (fields.get(4) == Some(&target)
            && after.first() == Some(&"btrfs")
            && fields.get(5)?.split(',').any(|value| value == "rw"))
        .then(|| Some((fields.first()?.parse().ok()?, after.get(1)?.to_string())))?
    });
    let result = matching.next().ok_or("no supported writable Btrfs root")?;
    if matching.next().is_some() {
        return Err("ambiguous Btrfs root mount".into());
    }
    Ok(result)
}

fn show(output: &str) -> IoResult<(String, u64)> {
    if output.len() > 16384 {
        return Err("Btrfs device response too large".into());
    }
    let mut fsid = None;
    let mut size = None;
    let mut devices = 0_u64;
    for line in output.lines() {
        if let Some((_, uuid)) = line.rsplit_once("uuid: ") {
            if fsid.is_some() || !crate::is_guid(uuid.trim()) {
                return Err("Btrfs filesystem identity invalid".into());
            }
            fsid = Some(uuid.trim().to_ascii_lowercase());
        }
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first() == Some(&"devid") {
            devices = devices.saturating_add(1);
            if fields.get(1) != Some(&"1") || fields.get(2) != Some(&"size") {
                return Err("Btrfs multi-device resize unsupported".into());
            }
            size = fields.get(3).and_then(|value| value.parse::<u64>().ok());
        }
        if fields.starts_with(&["Total", "devices"]) && fields.get(2) != Some(&"1") {
            return Err("Btrfs multi-device resize unsupported".into());
        }
    }
    if devices != 1 {
        return Err("Btrfs device inventory unsupported".into());
    }
    Ok((
        fsid.ok_or("Btrfs identity missing")?,
        size.ok_or("Btrfs device size missing")?,
    ))
}

impl Held {
    pub(super) fn discover(platform: &LinuxPlatform) -> IoResult<Option<Self>> {
        // SAFETY: geteuid has no arguments or memory effects.
        if rustix::process::geteuid().as_raw() != 0 {
            return Ok(None);
        }
        let candidate = ["/sysroot", "/"]
            .into_iter()
            .find_map(|target| mount_line(target).ok().map(|facts| (target, facts)));
        let Some((target, (mount_id, source))) = candidate else {
            return Ok(None);
        };
        let path = std::fs::canonicalize(&source).map_err(|_| "Btrfs source device unavailable")?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Btrfs device name invalid")?;
        let sys = PathBuf::from("/sys/class/block").join(name);
        if !sys.join("partition").is_file() || sys.join("dm").exists() || sys.join("md").exists() {
            return Err("Btrfs mapped devices are unsupported".into());
        }
        let parent =
            std::fs::canonicalize(&sys).map_err(|_| "Btrfs device ancestry unavailable")?;
        if parent.parent().and_then(|p| p.file_name()) != platform.disk.file_name() {
            return Err("Btrfs root not on selected system disk".into());
        }
        let held_disk = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&platform.disk)
            .map_err(|_| "Btrfs held disk unavailable")?;
        if !held_disk
            .metadata()
            .map_err(|_| "Btrfs held disk unavailable")?
            .file_type()
            .is_block_device()
        {
            return Err("Btrfs selected disk is not a block device".into());
        }
        let lock = advisory_lock::FileLock::exclusive(
            held_disk
                .try_clone()
                .map_err(|_| "Btrfs disk lock unavailable")?,
        )
        .map_err(|_| "Btrfs disk already has an installer operation")?;
        let root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(target)
            .map_err(|_| "Btrfs mount unavailable")?;
        let device = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|_| "Btrfs device unavailable")?;
        if !device
            .metadata()
            .map_err(|_| "Btrfs device metadata unavailable")?
            .file_type()
            .is_block_device()
        {
            return Err("Btrfs selected device is not a block device".into());
        }
        let disk = platform.system_disk()?;
        let number = std::fs::read_to_string(sys.join("partition"))
            .map_err(|_| "Btrfs partition number unavailable")?
            .trim()
            .parse::<u32>()
            .map_err(|_| "Btrfs partition number invalid")?;
        let partition = disk
            .partitions
            .iter()
            .find(|p| p.number == number)
            .ok_or("Btrfs partition absent from selected GPT")?;
        let argument = format!("/proc/{}/fd/{}", std::process::id(), root.as_raw_fd());
        let (fsid, bytes) = show(&run(Command::new("/usr/bin/btrfs")
            .args(["filesystem", "show", "--raw"])
            .arg(&argument))?)?;
        if bytes > partition_bytes(partition, disk.sector_bytes)? {
            return Err("Btrfs filesystem and GPT original sizes differ".into());
        }
        Ok(Some(Self {
            _lock: lock,
            root,
            namespace: File::open("/proc/self/ns/mnt")
                .map_err(|_| "Btrfs namespace unavailable")?,
            mount_id,
            target: target.into(),
            device,
            device_path: path,
            guid: partition.guid.clone(),
            first_lba: partition.first_lba,
            sector: disk.sector_bytes,
            fsid,
            disk_guid: disk.guid.clone(),
            disk: held_disk,
            disk_path: platform.disk.clone(),
        }))
    }

    fn argument(&self) -> String {
        format!("/proc/{}/fd/{}", std::process::id(), self.root.as_raw_fd())
    }
    fn disk_argument(&self) -> String {
        format!("/proc/{}/fd/{}", std::process::id(), self.disk.as_raw_fd())
    }
    fn check_disk(&self) -> IoResult<()> {
        let held = self
            .disk
            .metadata()
            .map_err(|_| "Btrfs held disk unavailable")?;
        let named = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.disk_path)
            .and_then(|f| f.metadata())
            .map_err(|_| "Btrfs named disk unavailable")?;
        if !named.file_type().is_block_device()
            || (held.dev(), held.ino(), held.rdev()) != (named.dev(), named.ino(), named.rdev())
        {
            return Err("Btrfs whole disk identity changed".into());
        }
        Ok(())
    }
    pub(super) fn facts(&self, platform: &LinuxPlatform) -> IoResult<LinuxBtrfsVolume> {
        self.check_disk()?;
        let namespace =
            File::open("/proc/self/ns/mnt").map_err(|_| "Btrfs namespace unavailable")?;
        let current_ns = namespace
            .metadata()
            .map_err(|_| "Btrfs namespace unavailable")?;
        let held_ns = self
            .namespace
            .metadata()
            .map_err(|_| "Btrfs namespace unavailable")?;
        let (mount_id, mount_source) = mount_line(&self.target)?;
        let current_root = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&self.target)
            .and_then(|file| file.metadata())
            .map_err(|_| "Btrfs mount unavailable")?;
        let held_root = self
            .root
            .metadata()
            .map_err(|_| "Btrfs mount unavailable")?;
        let current_device = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.device_path)
            .and_then(|file| file.metadata())
            .map_err(|_| "Btrfs device unavailable")?;
        let held_device = self
            .device
            .metadata()
            .map_err(|_| "Btrfs device unavailable")?;
        if (current_ns.dev(), current_ns.ino()) != (held_ns.dev(), held_ns.ino())
            || mount_id != self.mount_id
            || (current_root.dev(), current_root.ino()) != (held_root.dev(), held_root.ino())
            || !current_device.file_type().is_block_device()
            || (
                current_device.dev(),
                current_device.ino(),
                current_device.rdev(),
            ) != (held_device.dev(), held_device.ino(), held_device.rdev())
            || std::fs::canonicalize(mount_source).map_err(|_| "Btrfs mount source unavailable")?
                != self.device_path
        {
            return Err("Btrfs held mount or device replaced".into());
        }
        let disk = platform.system_disk()?;
        if disk.guid != self.disk_guid {
            return Err("Btrfs selected disk identity changed".into());
        }
        let partition = disk
            .partitions
            .iter()
            .find(|p| p.guid == self.guid)
            .ok_or("Btrfs retained partition disappeared")?;
        if (partition.first_lba, disk.sector_bytes) != (self.first_lba, self.sector) {
            return Err("Btrfs retained partition moved".into());
        }
        let argument = self.argument();
        let (fsid, filesystem_bytes) = show(&run(Command::new("/usr/bin/btrfs")
            .args(["filesystem", "show", "--raw"])
            .arg(&argument))?)?;
        if fsid != self.fsid {
            return Err("Btrfs filesystem identity changed".into());
        }
        let minimum = self.minimum_bytes(&argument)?;
        let mut kernel_partition_bytes = 0_u64;
        // SAFETY: BLKGETSIZE64 writes precisely one u64 to a valid output pointer.
        if unsafe {
            libc::ioctl(
                self.device.as_raw_fd(),
                super::BLKGETSIZE64,
                &mut kernel_partition_bytes,
            )
        } != 0
        {
            return Err("Btrfs kernel device size unavailable".into());
        }
        Ok(LinuxBtrfsVolume {
            partition_guid: self.guid.clone(),
            filesystem_uuid: self.fsid.clone(),
            device_id: 1,
            mount_id: self.mount_id,
            first_lba: self.first_lba,
            sector_bytes: self.sector,
            size_bytes: partition_bytes(partition, self.sector)?,
            filesystem_bytes,
            kernel_partition_bytes,
            supported_min_bytes: minimum,
            reserve_bytes: RESERVE,
        })
    }

    fn minimum_bytes(&self, argument: &str) -> IoResult<u64> {
        let exclusive = std::fs::read_to_string(
            PathBuf::from("/sys/fs/btrfs")
                .join(&self.fsid)
                .join("exclusive_operation"),
        )
        .map_err(|_| "Btrfs exclusive operation status unavailable")?;
        if exclusive.trim() != "none" {
            return Err("Btrfs exclusive operation requires completion".into());
        }
        let scrub = run(Command::new("/usr/bin/btrfs")
            .env("LC_ALL", "C")
            .args(["scrub", "status"])
            .arg(argument))?;
        if !scrub.lines().any(|line| {
            line.trim() == "no stats available"
                || line.split_whitespace().collect::<Vec<_>>() == ["Status:", "finished"]
                || line.split_whitespace().collect::<Vec<_>>() == ["Status:", "aborted"]
        }) {
            return Err("Btrfs scrub must finish before resize".into());
        }
        let minimum = run(Command::new("/usr/bin/btrfs")
            .args(["inspect-internal", "min-dev-size", "--id", "1"])
            .arg(argument))?;
        let minimum_fields: Vec<_> = minimum.split_whitespace().collect();
        if minimum_fields.get(1) != Some(&"bytes") {
            return Err("Btrfs minimum size format unavailable".into());
        }
        let minimum = minimum_fields
            .first()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or("Btrfs minimum size unavailable")?;
        Ok(minimum)
    }

    pub(super) fn resize_filesystem(
        &self,
        platform: &LinuxPlatform,
        guid: &str,
        bytes: u64,
    ) -> IoResult<()> {
        let facts = self.facts(platform)?;
        if guid != self.guid
            || bytes > facts.kernel_partition_bytes
            || bytes
                < facts
                    .supported_min_bytes
                    .checked_add(RESERVE)
                    .ok_or("Btrfs reserve overflow")?
        {
            return Err("Btrfs filesystem resize outside reviewed bounds".into());
        }
        let argument = self.argument();
        run(Command::new("/usr/bin/btrfs")
            .args(["filesystem", "resize"])
            .arg(format!("1:{bytes}"))
            .arg(&argument))?;
        run(Command::new("/usr/bin/btrfs")
            .args(["filesystem", "sync"])
            .arg(&argument))?;
        if self.facts(platform)?.filesystem_bytes != bytes {
            return Err("Btrfs filesystem resize readback failed".into());
        }
        Ok(())
    }

    pub(super) fn resize_partition(
        &self,
        platform: &LinuxPlatform,
        guid: &str,
        bytes: u64,
    ) -> IoResult<()> {
        let facts = self.facts(platform)?;
        if guid != self.guid || bytes < facts.filesystem_bytes || !bytes.is_multiple_of(self.sector)
        {
            return Err("Btrfs GPT resize outside filesystem bounds".into());
        }
        let before = platform.system_disk()?;
        let disk_argument = self.disk_argument();
        let mut exact_before: serde_json::Value =
            serde_json::from_str(&run(Command::new("sfdisk")
                .arg("--json")
                .arg(&disk_argument))?)
            .map_err(|_| "Btrfs complete GPT readback invalid")?;
        let selected = before
            .partitions
            .iter()
            .find(|p| p.guid == self.guid)
            .ok_or("Btrfs retained partition unavailable")?;
        let sectors = bytes
            .checked_div(self.sector)
            .ok_or("Btrfs zero sector size")?;
        let last = self
            .first_lba
            .checked_add(sectors)
            .and_then(|value| value.checked_sub(1))
            .ok_or("Btrfs GPT extent overflow")?;
        if last > before.last_usable_lba
            || before
                .partitions
                .iter()
                .any(|p| p.guid != self.guid && p.first_lba <= last && p.last_lba >= self.first_lba)
        {
            return Err("Btrfs GPT resize overlaps another partition".into());
        }
        let number = selected.number.to_string();
        let partitions = exact_before
            .get_mut("partitiontable")
            .and_then(|t| t.get_mut("partitions"))
            .and_then(|p| p.as_array_mut())
            .ok_or("Btrfs complete GPT partition table missing")?;
        let matches: Vec<_> = partitions
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                p.get("uuid")
                    .and_then(|v| v.as_str())
                    .filter(|id| id.eq_ignore_ascii_case(&self.guid))
                    .map(|_| i)
            })
            .collect();
        let [index] = matches.as_slice() else {
            return Err("Btrfs complete GPT selected UUID differs".into());
        };
        partitions
            .get_mut(*index)
            .ok_or("Btrfs GPT entry missing")?
            .as_object_mut()
            .ok_or("Btrfs GPT entry invalid")?
            .insert("size".into(), serde_json::Value::from(sectors));
        // --part-size changes only size; every other field is read back below.
        run(Command::new("sfdisk")
            .args(["--no-reread", "--no-tell-kernel", "--part-size"])
            .arg(&disk_argument)
            .arg(&number)
            .arg(sectors.to_string()))?;
        self.disk.sync_all().map_err(|_| "Btrfs GPT sync failed")?;
        self.check_disk()?;
        let exact_after: serde_json::Value = serde_json::from_str(&run(Command::new("sfdisk")
            .arg("--json")
            .arg(&disk_argument))?)
        .map_err(|_| "Btrfs complete GPT readback invalid")?;
        if exact_after != exact_before {
            return Err("Btrfs GPT resize changed unreviewed attributes or names".into());
        }
        let after = platform.system_disk()?;
        let mut expected = before;
        expected
            .partitions
            .iter_mut()
            .find(|p| p.guid == self.guid)
            .ok_or("Btrfs retained partition unavailable")?
            .last_lba = last;
        if after != expected {
            return Err("Btrfs GPT resize changed another reviewed field".into());
        }
        run(Command::new("partx")
            .args(["--update", "--nr"])
            .arg(number)
            .arg(&disk_argument))?;
        let after = self.facts(platform)?;
        if after.size_bytes != bytes || after.kernel_partition_bytes != bytes {
            return Err("Btrfs GPT or kernel resize readback failed".into());
        }
        Ok(())
    }
}
