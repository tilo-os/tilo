//! [`WindowsPlatform`]: the Win32 + Storage-cmdlet implementation of
//! [`Platform`], and the unprivileged [`probe_facts`].

use std::cell::{Cell, RefCell};
use std::time::Duration;

use super::super::platform::{Disk, IoResult, NewPartition, Platform, SystemVolume};
use super::super::probe::HostFacts;
use super::facts::{self, Observed};
use super::scripts::{self, DriveLetter};
use super::storage::{self, Snapshot};
use super::sys::{self, AlignedBuf, DriveAccess, Handle};
use super::{layout, ops};

/// `Get-PartitionSupportedSize` analyses the volume; allow for a slow disk.
const PROBE_TIMEOUT: Duration = Duration::from_secs(180);
const PARTITION_TIMEOUT: Duration = Duration::from_secs(180);
/// An NTFS shrink relocates files and can take a long time on a full volume.
const RESIZE_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
/// Raw transfers are split so the aligned bounce buffer stays small.
const IO_CHUNK_BYTES: usize = 8 << 20;
/// Upper bound for the GPT entry array read (1024 entries of 1 KiB).
const MAX_GPT_ARRAY_BYTES: u64 = 1 << 20;
const SYSTEM_ENVIRONMENT_PRIVILEGE: &str = "SeSystemEnvironmentPrivilege";

fn io_err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn take_snapshot(drive: DriveLetter) -> Result<Snapshot, String> {
    parse_snapshot_program(&scripts::snapshot(drive))
}

fn parse_snapshot_program(script: &str) -> Result<Snapshot, String> {
    let out = sys::powershell(script, PROBE_TIMEOUT)?;
    let json: serde_json::Value =
        serde_json::from_slice(&out).map_err(|e| format!("snapshot JSON: {e}"))?;
    storage::parse_snapshot(&json)
}

fn system_layout(drive: DriveLetter) -> Result<Disk, String> {
    let extent = sys::system_volume_extent(drive).map_err(io_err)?;
    sys::layout_disk(extent.disk_number)
}

fn observe() -> Observed {
    let uefi = sys::firmware_is_uefi();
    let secure_boot_registry = sys::hklm_dword(
        r"SYSTEM\CurrentControlSet\Control\SecureBoot\State",
        "UEFISecureBootEnabled",
    );
    let (hiberboot_enabled, hibernate_enabled, hiberfile_present) = sys::hibernation_observation();
    let power = sys::power_status();
    let (snapshot, layout_disk, volume_space) = match sys::windows_drive() {
        Ok(drive) => (
            take_snapshot(drive),
            system_layout(drive),
            sys::volume_space(drive),
        ),
        Err(error) => (Err(error.to_string()), Err(error.to_string()), None),
    };
    Observed {
        uefi,
        secure_boot_registry,
        hiberboot_enabled,
        hibernate_enabled,
        hiberfile_present,
        power,
        snapshot,
        layout_disk,
        volume_space,
    }
}

/// Probe this Windows host (design spec §5). Never mutates and needs no
/// elevation: the layout comes from metadata-only IOCTLs, everything else from
/// the registry, `GetFirmwareType`, `GetSystemPowerStatus` and one read-only
/// PowerShell Storage snapshot. Boot variables are left empty; see
/// [`facts::basis_firmware`].
pub fn probe_facts() -> HostFacts {
    facts::assemble(&observe())
}

fn disk_number(disk: &Disk) -> IoResult<u32> {
    disk.id
        .parse()
        .map_err(|_| format!("invalid Windows disk number {:?}", disk.id))
}

fn usize_of(value: u64) -> IoResult<usize> {
    usize::try_from(value).map_err(|_| "size exceeds the address space".to_string())
}

/// The open raw target of partition I/O: the physical drive plus locks on
/// any (unmounted) volume Windows created inside the partition.
struct RawTarget {
    disk_id: String,
    partition_guid: String,
    start: u64,
    len: u64,
    sector: u64,
    handle: Handle,
    _volume_locks: Vec<Handle>,
    bounce: Option<AlignedBuf>,
}

/// A page-aligned bounce buffer of at least `len` bytes, allocated once and
/// reused for every chunk.
fn bounce(slot: &mut Option<AlignedBuf>, len: usize) -> IoResult<&mut [u8]> {
    if slot.as_ref().is_none_or(|b| b.len() < len) {
        *slot = Some(AlignedBuf::new(len.max(IO_CHUNK_BYTES)).map_err(io_err)?);
    }
    slot.as_mut()
        .ok_or("bounce buffer unavailable")?
        .as_mut_slice()
        .get_mut(..len)
        .ok_or_else(|| "bounce buffer too small".to_string())
}

/// The Windows staging adapter.
///
/// [`WindowsPlatform::read_only`] serves the unprivileged UI: every mutation
/// and raw access is refused. [`WindowsPlatform::elevated`] serves the
/// staging helper; it additionally reads the on-disk GPT on every
/// [`Platform::system_disk`] and requires it to agree with Windows.
pub struct WindowsPlatform {
    elevated: bool,
    geometry_only: bool,
    drive: DriveLetter,
    efi_ready: Cell<bool>,
    target: RefCell<Option<RawTarget>>,
}

impl WindowsPlatform {
    /// Read only the installed ESP named by its GPT PARTUUID. This bypasses
    /// the staging writer's volume locks: a final ESP may have a Windows
    /// volume object, but firmware registration must never write to it.
    pub fn read_installed_esp(
        &self,
        disk: &Disk,
        esp_guid: &str,
        offset: u64,
        data: &mut [u8],
    ) -> IoResult<()> {
        self.require_elevated()?;
        let (current, _) = self.current_for(disk)?;
        if &current != disk {
            return Err("installed ESP geometry changed during readback".into());
        }
        let part = ops::find(&current, esp_guid)?;
        if !super::super::same_guid(&part.type_guid, super::super::ESP_TYPE) {
            return Err("final loader read requires an EFI System Partition".into());
        }
        let (start, len) = ops::partition_bytes(&current, part)?;
        let absolute = ops::io_offset(start, len, current.sector_bytes, offset, data.len() as u64)?;
        let handle =
            sys::open_physical_drive(disk_number(&current)?, DriveAccess::Read).map_err(io_err)?;
        let mut aligned = AlignedBuf::new(data.len()).map_err(io_err)?;
        sys::read_exact_at(&handle, absolute, aligned.as_mut_slice()).map_err(io_err)?;
        data.copy_from_slice(aligned.as_mut_slice());
        Ok(())
    }

    /// Hash a loader with one GPT cross-check and one read-only physical
    /// drive handle for the entire FAT traversal. The next pass reopens and
    /// revalidates before changing firmware.
    pub fn installed_esp_loader_sha256(
        &self,
        disk: &Disk,
        esp_guid: &str,
        path: &str,
    ) -> IoResult<String> {
        self.require_elevated()?;
        let (current, _) = self.current_for(disk)?;
        if &current != disk {
            return Err("installed ESP geometry changed during readback".into());
        }
        let part = ops::find(&current, esp_guid)?;
        if !super::super::same_guid(&part.type_guid, super::super::ESP_TYPE) {
            return Err("final loader read requires an EFI System Partition".into());
        }
        let (start, len) = ops::partition_bytes(&current, part)?;
        let sector = current.sector_bytes;
        if sector == 0 || !sector.is_power_of_two() || sector > 4096 {
            return Err("invalid disk sector size".into());
        }
        let handle =
            sys::open_physical_drive(disk_number(&current)?, DriveAccess::Read).map_err(io_err)?;
        let mut read = |offset: u64, data: &mut [u8]| -> IoResult<()> {
            let end = offset
                .checked_add(data.len() as u64)
                .ok_or("ESP offset overflow")?;
            if end > len {
                return Err("read exceeds installed ESP".into());
            }
            let aligned_start = offset / sector * sector;
            let aligned_end = end
                .div_ceil(sector)
                .checked_mul(sector)
                .ok_or("ESP alignment overflow")?;
            let aligned_len = usize_of(aligned_end - aligned_start)?;
            let mut buffer = AlignedBuf::new(aligned_len).map_err(io_err)?;
            sys::read_exact_at(&handle, start + aligned_start, buffer.as_mut_slice())
                .map_err(io_err)?;
            let within = usize_of(offset - aligned_start)?;
            data.copy_from_slice(&buffer.as_mut_slice()[within..within + data.len()]);
            Ok(())
        };
        super::super::final_bootnext::hash_esp_file(&mut read, len, path)
    }

    pub fn read_only() -> IoResult<Self> {
        Ok(Self {
            elevated: false,
            geometry_only: false,
            drive: sys::windows_drive().map_err(io_err)?,
            efi_ready: Cell::new(false),
            target: RefCell::new(None),
        })
    }

    /// Fails unless this process is an elevated administrator (proved by
    /// enabling `SeSystemEnvironmentPrivilege`, which only elevated
    /// administrator tokens hold).
    pub fn elevated() -> IoResult<Self> {
        let platform = Self {
            elevated: true,
            geometry_only: false,
            drive: sys::windows_drive().map_err(io_err)?,
            efi_ready: Cell::new(false),
            target: RefCell::new(None),
        };
        platform.ensure_efi_privilege()?;
        Ok(platform)
    }

    /// Final installed-OS firmware handoff needs fresh disk geometry, not
    /// shrink capacity. Retain Windows/native/GPT cross-checks on every read.
    pub fn elevated_final_boot() -> IoResult<Self> {
        let mut platform = Self::elevated()?;
        platform.geometry_only = true;
        Ok(platform)
    }

    fn require_elevated(&self) -> IoResult<()> {
        if self.elevated {
            Ok(())
        } else {
            Err("this operation needs the elevated staging helper".into())
        }
    }

    fn ensure_efi_privilege(&self) -> IoResult<()> {
        if !self.efi_ready.get() {
            sys::enable_privilege(SYSTEM_ENVIRONMENT_PRIVILEGE)
                .map_err(|e| format!("{SYSTEM_ENVIRONMENT_PRIVILEGE}: {e}"))?;
            self.efi_ready.set(true);
        }
        Ok(())
    }

    fn invalidate(&self) {
        self.target.replace(None);
    }

    /// The system disk as the native layout reports it, cross-checked against
    /// the Storage cmdlets (and, elevated, against the on-disk GPT).
    fn current(&self) -> IoResult<(Disk, Snapshot)> {
        let snapshot = if self.geometry_only {
            parse_snapshot_program(&scripts::geometry_snapshot(self.drive))?
        } else {
            take_snapshot(self.drive)?
        };
        let disk = system_layout(self.drive)?;
        storage::cross_check(&disk, &snapshot)?;
        if self.elevated {
            verify_gpt(&disk)?;
        }
        Ok((disk, snapshot))
    }

    fn current_for(&self, planned: &Disk) -> IoResult<(Disk, Snapshot)> {
        let (disk, snapshot) = self.current()?;
        ops::same_disk(planned, &disk)?;
        Ok((disk, snapshot))
    }

    fn resize_system(&mut self, partition_guid: &str, to_bytes: u64, grow: bool) -> IoResult<()> {
        self.require_elevated()?;
        self.invalidate();
        let (disk, snapshot) = self.current()?;
        let system = ops::find(&disk, partition_guid)?;
        if snapshot.system.guid.as_deref() != Some(system.guid.as_str()) {
            return Err("only the Windows volume can be resized".into());
        }
        let (_, size) = ops::partition_bytes(&disk, system)?;
        let direction_ok = if grow {
            to_bytes > size
        } else {
            to_bytes < size
        };
        if !direction_ok || to_bytes.checked_rem(disk.sector_bytes) != Some(0) {
            return Err(format!(
                "refusing to resize the Windows volume from {size} to {to_bytes} bytes"
            ));
        }
        let number = disk_number(&disk)?;
        let script = scripts::resize(number, system.number, &system.guid, to_bytes)
            .ok_or("invalid partition GUID")?;
        let out = sys::powershell(&script, RESIZE_TIMEOUT)?;
        let json = serde_json::from_slice(&out).map_err(|e| format!("resize JSON: {e}"))?;
        ops::parse_resized(&json, &system.guid, to_bytes)?;
        let guid = system.guid.clone();
        let (after, _) = self.current_for(&disk)?;
        let (_, now) = ops::partition_bytes(&after, ops::find(&after, &guid)?)?;
        if now != to_bytes {
            return Err(format!("the Windows volume is {now} bytes, not {to_bytes}"));
        }
        Ok(())
    }

    /// Open (or reuse) the raw target for partition I/O after proving the
    /// partition is a staging partition with no mounted volume.
    fn with_target<R>(
        &self,
        disk: &Disk,
        partition_guid: &str,
        io: impl FnOnce(&mut RawTarget) -> IoResult<R>,
    ) -> IoResult<R> {
        self.require_elevated()?;
        let guid = super::layout::normalize_guid(partition_guid).ok_or("not a partition GUID")?;
        let mut slot = self.target.borrow_mut();
        let reusable = slot
            .as_ref()
            .is_some_and(|t| t.disk_id == disk.id && t.partition_guid == guid);
        if !reusable {
            *slot = None;
            *slot = Some(self.open_target(disk, &guid)?);
        }
        let target = slot.as_mut().ok_or("raw target unavailable")?;
        io(target)
    }

    fn open_target(&self, disk: &Disk, guid: &str) -> IoResult<RawTarget> {
        let (current, snapshot) = self.current_for(disk)?;
        let partition = ops::find(&current, guid)?;
        if !ops::is_staging_type(&partition.type_guid) {
            return Err("refusing raw I/O on a partition that is not a staging partition".into());
        }
        if storage::protected_partitions(&snapshot).contains(&partition.guid) {
            return Err("refusing raw I/O on a Windows boot, system or lettered partition".into());
        }
        let number = disk_number(&current)?;
        let (start, len) = ops::partition_bytes(&current, partition)?;
        let mut locks = Vec::new();
        for volume in sys::volumes().map_err(io_err)? {
            let device = volume.trim_end_matches('\\');
            // A volume whose extents cannot be read (an empty card reader,
            // an offline disk) is not on the target disk.
            let Ok(extents) = sys::volume_extents(device) else {
                continue;
            };
            let mine: Vec<_> = extents.iter().filter(|e| e.disk_number == number).collect();
            let mut inside = false;
            for extent in mine {
                match ops::volume_extent_allowed(start, len, extent.offset, extent.length) {
                    Some(true) => {
                        inside |= extent.offset < start.saturating_add(len)
                            && start < extent.offset.saturating_add(extent.length);
                    }
                    _ => return Err("a Windows volume straddles the staging partition".into()),
                }
            }
            if !inside {
                continue;
            }
            if !sys::mount_points(&volume).map_err(io_err)?.is_empty() {
                return Err("the staging partition's volume is mounted".into());
            }
            locks.push(sys::lock_volume(device).map_err(|e| format!("locking {device}: {e}"))?);
        }
        let handle = sys::open_physical_drive(number, DriveAccess::ReadWrite).map_err(io_err)?;
        Ok(RawTarget {
            disk_id: current.id.clone(),
            partition_guid: partition.guid.clone(),
            start,
            len,
            sector: current.sector_bytes,
            handle,
            _volume_locks: locks,
            bounce: None,
        })
    }
}

/// Read the primary GPT and its entry array from the raw disk and require
/// both to match `disk` exactly (elevated only).
fn verify_gpt(disk: &Disk) -> IoResult<()> {
    let number = disk_number(disk)?;
    let handle = sys::open_physical_drive(number, DriveAccess::Read).map_err(io_err)?;
    let sector = disk.sector_bytes;
    let mut header = AlignedBuf::new(usize_of(sector)?).map_err(io_err)?;
    sys::read_exact_at(&handle, sector, header.as_mut_slice()).map_err(io_err)?;
    let header = layout::parse_gpt_header(header.as_mut_slice())?;
    let bytes = header.entries_read_bytes(sector)?;
    if bytes > MAX_GPT_ARRAY_BYTES {
        return Err("GPT entry array is implausibly large".into());
    }
    let mut entries = AlignedBuf::new(usize_of(bytes)?).map_err(io_err)?;
    let at = header
        .entries_lba
        .checked_mul(sector)
        .ok_or("entry array offset overflow")?;
    sys::read_exact_at(&handle, at, entries.as_mut_slice()).map_err(io_err)?;
    let entries = layout::parse_gpt_entries(entries.as_mut_slice(), &header)?;
    layout::cross_check_gpt(disk, &header, &entries)
}

impl Platform for WindowsPlatform {
    fn system_disk(&self) -> IoResult<Disk> {
        self.current().map(|(disk, _)| disk)
    }

    fn system_volume(&self) -> IoResult<Option<SystemVolume>> {
        let (disk, snapshot) = self.current()?;
        facts::resolve_system_volume(&disk, &snapshot, sys::volume_space(self.drive)).map(Some)
    }

    fn shrink_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()> {
        self.resize_system(partition_guid, to_bytes, false)
    }

    fn grow_system_volume(&mut self, partition_guid: &str, to_bytes: u64) -> IoResult<()> {
        self.resize_system(partition_guid, to_bytes, true)
    }

    fn create_partition(&mut self, disk: &Disk, spec: &NewPartition) -> IoResult<String> {
        self.require_elevated()?;
        self.invalidate();
        let (current, _) = self.current_for(disk)?;
        let (offset, size) = ops::create_extent(&current, spec)?;
        let number = disk_number(&current)?;
        let script = scripts::create(number, offset, size, &spec.type_guid)
            .ok_or("invalid partition type")?;
        let out = sys::powershell(&script, PARTITION_TIMEOUT)?;
        let json = serde_json::from_slice(&out).map_err(|e| format!("create JSON: {e}"))?;
        let guid = ops::parse_created(&json, offset, size, &spec.type_guid)?;
        // `New-Partition` cannot set the GPT name; names are informational
        // only (never identity), so the partition keeps an empty name.
        let (after, _) = self.current_for(disk)?;
        let created = ops::find(&after, &guid)?;
        if created.first_lba != spec.first_lba
            || created.last_lba != spec.last_lba
            || super::layout::normalize_guid(&spec.type_guid).as_deref()
                != Some(created.type_guid.as_str())
        {
            return Err("the created partition does not match the requested extent".into());
        }
        Ok(guid)
    }

    fn delete_partition(&mut self, disk: &Disk, partition_guid: &str) -> IoResult<()> {
        self.require_elevated()?;
        self.invalidate();
        let (current, snapshot) = self.current_for(disk)?;
        let partition = ops::find(&current, partition_guid)?;
        if !ops::is_staging_type(&partition.type_guid)
            || storage::protected_partitions(&snapshot).contains(&partition.guid)
        {
            return Err("refusing to delete a partition that is not a staging partition".into());
        }
        let (offset, size) = ops::partition_bytes(&current, partition)?;
        let number = disk_number(&current)?;
        let script = scripts::delete(number, partition.number, &partition.guid, offset, size)
            .ok_or("invalid partition GUID")?;
        sys::powershell(&script, PARTITION_TIMEOUT)?;
        let guid = partition.guid.clone();
        let (after, _) = self.current_for(disk)?;
        if after.partitions.iter().any(|p| p.guid == guid) {
            return Err("Windows still lists the deleted partition".into());
        }
        Ok(())
    }

    fn write_partition(
        &mut self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        data: &[u8],
    ) -> IoResult<()> {
        self.with_target(disk, partition_guid, |target| {
            let len = u64::try_from(data.len()).map_err(io_err)?;
            let mut at = ops::io_offset(target.start, target.len, target.sector, offset, len)?;
            for chunk in data.chunks(IO_CHUNK_BYTES) {
                let staged = bounce(&mut target.bounce, chunk.len())?;
                staged.copy_from_slice(chunk);
                sys::write_all_at(&target.handle, at, staged).map_err(io_err)?;
                at = at
                    .checked_add(u64::try_from(chunk.len()).map_err(io_err)?)
                    .ok_or("offset overflow")?;
            }
            Ok(())
        })
    }

    fn read_partition(
        &self,
        disk: &Disk,
        partition_guid: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> IoResult<()> {
        self.with_target(disk, partition_guid, |target| {
            let len = u64::try_from(buf.len()).map_err(io_err)?;
            let mut at = ops::io_offset(target.start, target.len, target.sector, offset, len)?;
            for chunk in buf.chunks_mut(IO_CHUNK_BYTES) {
                let staged = bounce(&mut target.bounce, chunk.len())?;
                sys::read_exact_at(&target.handle, at, staged).map_err(io_err)?;
                chunk.copy_from_slice(staged);
                at = at
                    .checked_add(u64::try_from(chunk.len()).map_err(io_err)?)
                    .ok_or("offset overflow")?;
            }
            Ok(())
        })
    }

    fn sync_disk(&mut self, disk: &Disk) -> IoResult<()> {
        self.require_elevated()?;
        if let Some(target) = self.target.borrow().as_ref()
            && target.disk_id == disk.id
        {
            return sys::flush(&target.handle).map_err(io_err);
        }
        let handle =
            sys::open_physical_drive(disk_number(disk)?, DriveAccess::ReadWrite).map_err(io_err)?;
        sys::flush(&handle).map_err(io_err)
    }

    fn get_efi_var(&self, name: &str) -> IoResult<Option<Vec<u8>>> {
        if !ops::valid_efi_name(name) {
            return Err(format!("invalid EFI variable name {name:?}"));
        }
        self.ensure_efi_privilege()?;
        sys::efi_get(name).map_err(|e| format!("reading {name}: {e}"))
    }

    fn set_efi_var(&mut self, name: &str, data: &[u8]) -> IoResult<()> {
        self.require_elevated()?;
        if data.is_empty() {
            return Err("refusing to write an empty EFI variable (that deletes it)".into());
        }
        if !ops::valid_efi_name(name) {
            return Err(format!("invalid EFI variable name {name:?}"));
        }
        self.ensure_efi_privilege()?;
        sys::efi_set(name, data).map_err(|e| format!("writing {name}: {e}"))?;
        if sys::efi_get(name).map_err(io_err)?.as_deref() != Some(data) {
            return Err(format!("{name} did not read back as written"));
        }
        Ok(())
    }

    fn delete_efi_var(&mut self, name: &str) -> IoResult<()> {
        self.require_elevated()?;
        if !ops::valid_efi_name(name) {
            return Err(format!("invalid EFI variable name {name:?}"));
        }
        self.ensure_efi_privilege()?;
        if sys::efi_get(name).map_err(io_err)?.is_none() {
            return Ok(());
        }
        sys::efi_set(name, &[]).map_err(|e| format!("deleting {name}: {e}"))?;
        if sys::efi_get(name).map_err(io_err)?.is_some() {
            return Err(format!("{name} still exists after deletion"));
        }
        Ok(())
    }
}
