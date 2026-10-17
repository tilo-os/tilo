//! Pure preconditions and read-back checks around each Windows mutation.
//! The adapter calls these before and after every PowerShell or raw-disk
//! operation; nothing here performs I/O.

use serde_json::Value;

use super::super::platform::{Disk, NewPartition, Partition};
use super::super::{ESP_TYPE, STAGING_ROOT_TYPE};
use super::layout::normalize_guid;

pub type OpResult<T> = Result<T, String>;

/// Only the two staging partition types may be created, written or deleted.
pub fn is_staging_type(type_guid: &str) -> bool {
    normalize_guid(type_guid).is_some_and(|t| t == ESP_TYPE || t == STAGING_ROOT_TYPE)
}

/// The disk the caller planned against must still be the disk Windows reports.
pub fn same_disk(planned: &Disk, current: &Disk) -> OpResult<()> {
    if planned.id != current.id
        || planned.guid != current.guid
        || planned.size_bytes != current.size_bytes
        || planned.sector_bytes != current.sector_bytes
    {
        return Err("the system disk changed identity".into());
    }
    Ok(())
}

/// Byte offset and size for `spec`, after proving it lies inside the usable
/// range and overlaps nothing on `current`.
pub fn create_extent(current: &Disk, spec: &NewPartition) -> OpResult<(u64, u64)> {
    if spec.guid.is_some() {
        return Err("Windows cannot choose a partition GUID; plan with `guid: None`".into());
    }
    if !is_staging_type(&spec.type_guid) {
        return Err(format!(
            "refusing to create a partition of type {}",
            spec.type_guid
        ));
    }
    if spec.first_lba > spec.last_lba
        || spec.first_lba < current.first_usable_lba
        || spec.last_lba > current.last_usable_lba
    {
        return Err("the new partition lies outside the GPT usable range".into());
    }
    if current
        .partitions
        .iter()
        .any(|p| p.first_lba <= spec.last_lba && spec.first_lba <= p.last_lba)
    {
        return Err("the new partition overlaps an existing partition".into());
    }
    let sectors = spec
        .last_lba
        .checked_sub(spec.first_lba)
        .and_then(|n| n.checked_add(1))
        .ok_or("extent overflow")?;
    let offset = spec
        .first_lba
        .checked_mul(current.sector_bytes)
        .ok_or("offset overflow")?;
    let size = sectors
        .checked_mul(current.sector_bytes)
        .ok_or("size overflow")?;
    Ok((offset, size))
}

/// Decode the create program's read-back and return the new partition GUID.
/// Exact offset, size and type are required; any drive letter or folder mount
/// point (anything but the `\\?\Volume{...}\` GUID path) is a failure.
pub fn parse_created(json: &Value, offset: u64, size: u64, type_guid: &str) -> OpResult<String> {
    let number = |key: &str| json.get(key).and_then(Value::as_u64);
    if number("Offset") != Some(offset) || number("Size") != Some(size) {
        return Err("Windows created the partition at a different extent".into());
    }
    let created_type = json
        .get("GptType")
        .and_then(Value::as_str)
        .and_then(normalize_guid);
    if created_type != normalize_guid(type_guid) {
        return Err("Windows created the partition with a different type".into());
    }
    let letter = json
        .get("DriveLetter")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if letter.chars().any(|c| c.is_ascii_alphabetic()) {
        return Err("Windows assigned a drive letter to the new partition".into());
    }
    let paths = match json.get("AccessPaths") {
        Some(Value::Array(paths)) => paths.iter().collect::<Vec<_>>(),
        Some(path @ Value::String(_)) => vec![path],
        _ => Vec::new(),
    };
    if paths.iter().any(|path| {
        !path
            .as_str()
            .is_some_and(|p| p.is_empty() || p.starts_with(r"\\?\Volume{"))
    }) {
        return Err("the new partition has a mount point".into());
    }
    json.get("Guid")
        .and_then(Value::as_str)
        .and_then(normalize_guid)
        .ok_or_else(|| "Windows did not report the new partition's GUID".into())
}

/// Decode the resize program's read-back.
pub fn parse_resized(json: &Value, guid: &str, size: u64) -> OpResult<()> {
    let reported = json
        .get("Guid")
        .and_then(Value::as_str)
        .and_then(normalize_guid);
    if reported != normalize_guid(guid) {
        return Err("the resized partition changed identity".into());
    }
    if json.get("Size").and_then(Value::as_u64) != Some(size) {
        return Err("Windows did not resize the partition to the exact size".into());
    }
    Ok(())
}

/// The partition with `guid` on `disk`.
pub fn find<'a>(disk: &'a Disk, guid: &str) -> OpResult<&'a Partition> {
    let guid = normalize_guid(guid).ok_or("not a partition GUID")?;
    disk.partitions
        .iter()
        .find(|p| p.guid == guid)
        .ok_or_else(|| format!("partition {guid} is not on the system disk"))
}

/// Byte offset and length of `partition` on its disk.
pub fn partition_bytes(disk: &Disk, partition: &Partition) -> OpResult<(u64, u64)> {
    let start = partition
        .first_lba
        .checked_mul(disk.sector_bytes)
        .ok_or("offset overflow")?;
    let len = partition
        .last_lba
        .checked_sub(partition.first_lba)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| n.checked_mul(disk.sector_bytes))
        .ok_or("length overflow")?;
    Ok((start, len))
}

/// Absolute disk offset of `[offset, offset + len)` inside a partition of
/// `part_len` bytes at `part_start`, with sector alignment enforced.
pub fn io_offset(
    part_start: u64,
    part_len: u64,
    sector: u64,
    offset: u64,
    len: u64,
) -> OpResult<u64> {
    if offset.checked_rem(sector) != Some(0) || len.checked_rem(sector) != Some(0) {
        return Err("partition I/O must be sector aligned".into());
    }
    let end = offset.checked_add(len).ok_or("I/O range overflow")?;
    if end > part_len {
        return Err("partition I/O runs past the end of the partition".into());
    }
    part_start
        .checked_add(offset)
        .ok_or_else(|| "I/O offset overflow".into())
}

/// A volume extent on the target disk may only intersect the partition when
/// it lies entirely inside it (then it is the partition's own, unmounted
/// volume, which the adapter locks and dismounts).
pub fn volume_extent_allowed(
    part_start: u64,
    part_len: u64,
    offset: u64,
    len: u64,
) -> Option<bool> {
    let part_end = part_start.checked_add(part_len)?;
    let end = offset.checked_add(len)?;
    let intersects = offset < part_end && part_start < end;
    Some(!intersects || (offset >= part_start && end <= part_end))
}

/// EFI variable names the adapter accepts: ASCII alphanumerics only.
pub fn valid_efi_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `BootCurrent` / `BootNext`: one little-endian `u16`, formatted `%04X`.
pub fn decode_boot_number(bytes: &[u8]) -> OpResult<String> {
    match *bytes {
        [lo, hi] => Ok(format!("{:04X}", u16::from_le_bytes([lo, hi]))),
        _ => Err(format!("boot number variable has {} bytes", bytes.len())),
    }
}

/// `BootOrder`: an array of little-endian `u16`.
pub fn decode_boot_order(bytes: &[u8]) -> OpResult<Vec<String>> {
    if !bytes.len().is_multiple_of(2) {
        return Err("BootOrder has an odd length".into());
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| match *pair {
            [lo, hi] => format!("{:04X}", u16::from_le_bytes([lo, hi])),
            _ => String::new(),
        })
        .collect())
}
