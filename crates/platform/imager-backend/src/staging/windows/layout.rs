//! Pure decoders for the native disk structures the Windows adapter reads:
//! `IOCTL_DISK_GET_DRIVE_LAYOUT_EX`, `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`,
//! `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS` output buffers, and the on-disk GPT
//! header and entry array. Compiled on every platform so fixtures run on any
//! development host; the Win32 side (`sys.rs`) statically asserts that the
//! offsets below match the `windows-sys` definitions for the target.

use super::super::platform::{Disk, Partition};

/// `PARTITION_STYLE_*`.
pub const STYLE_MBR: u32 = 0;
pub const STYLE_GPT: u32 = 1;

// `DRIVE_LAYOUT_INFORMATION_EX` on 64-bit Windows.
pub const LAYOUT_HEADER_BYTES: usize = 48;
pub const LAYOUT_GPT_DISK_ID: usize = 8;
pub const LAYOUT_GPT_STARTING_USABLE: usize = 24;
pub const LAYOUT_GPT_USABLE_LENGTH: usize = 32;
// `PARTITION_INFORMATION_EX` on 64-bit Windows.
pub const ENTRY_BYTES: usize = 144;
pub const ENTRY_STYLE: usize = 0;
pub const ENTRY_OFFSET: usize = 8;
pub const ENTRY_LENGTH: usize = 16;
pub const ENTRY_NUMBER: usize = 24;
pub const ENTRY_GPT_TYPE: usize = 32;
pub const ENTRY_GPT_ID: usize = 48;
pub const ENTRY_GPT_NAME: usize = 72;
pub const ENTRY_GPT_NAME_UNITS: usize = 36;
// `DISK_GEOMETRY_EX`.
pub const GEOMETRY_BYTES_PER_SECTOR: usize = 20;
pub const GEOMETRY_DISK_SIZE: usize = 24;
pub const GEOMETRY_MIN_BYTES: usize = 32;
// `VOLUME_DISK_EXTENTS` / `DISK_EXTENT`.
pub const EXTENTS_FIRST: usize = 8;
pub const EXTENT_BYTES: usize = 24;
pub const EXTENT_DISK_NUMBER: usize = 0;
pub const EXTENT_OFFSET: usize = 8;
pub const EXTENT_LENGTH: usize = 16;

pub type ParseResult<T> = Result<T, String>;

fn bytes<const N: usize>(buf: &[u8], at: usize) -> ParseResult<[u8; N]> {
    let end = at
        .checked_add(N)
        .ok_or_else(|| "structure offset overflow".to_string())?;
    buf.get(at..end)
        .and_then(|slice| <[u8; N]>::try_from(slice).ok())
        .ok_or_else(|| format!("structure truncated at byte {at}"))
}

pub fn u16_at(buf: &[u8], at: usize) -> ParseResult<u16> {
    bytes::<2>(buf, at).map(u16::from_le_bytes)
}

pub fn u32_at(buf: &[u8], at: usize) -> ParseResult<u32> {
    bytes::<4>(buf, at).map(u32::from_le_bytes)
}

pub fn u64_at(buf: &[u8], at: usize) -> ParseResult<u64> {
    bytes::<8>(buf, at).map(u64::from_le_bytes)
}

/// A signed 64-bit Win32 byte count that must not be negative.
fn nonneg_at(buf: &[u8], at: usize) -> ParseResult<u64> {
    let value = bytes::<8>(buf, at).map(i64::from_le_bytes)?;
    u64::try_from(value).map_err(|_| format!("negative byte count at byte {at}"))
}

/// Format a little-endian (in-memory `GUID` / on-disk GPT) GUID as the
/// lowercase hyphenated form the staging contract uses.
pub fn guid_from_le(raw: [u8; 16]) -> String {
    uuid::Uuid::from_bytes_le(raw).to_string()
}

fn guid_at(buf: &[u8], at: usize) -> ParseResult<String> {
    bytes::<16>(buf, at).map(guid_from_le)
}

/// Accept `{XXXXXXXX-...}` or bare, any case; return lowercase bare or `None`
/// when the text is not exactly one GUID.
pub fn normalize_guid(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let bare = trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
        .unwrap_or(trimmed);
    let groups: Vec<&str> = bare.split('-').collect();
    let lengths = [8, 4, 4, 4, 12];
    let valid = groups.len() == lengths.len()
        && groups
            .iter()
            .zip(lengths)
            .all(|(group, len)| group.len() == len && group.bytes().all(|b| b.is_ascii_hexdigit()));
    valid.then(|| bare.to_ascii_lowercase())
}

fn is_zero_guid(guid: &str) -> bool {
    guid.bytes().all(|b| b == b'0' || b == b'-')
}

fn utf16_name(buf: &[u8], at: usize, units: usize) -> ParseResult<String> {
    let mut out = Vec::with_capacity(units);
    for unit in 0..units {
        let offset = unit
            .checked_mul(2)
            .and_then(|delta| at.checked_add(delta))
            .ok_or_else(|| "name offset overflow".to_string())?;
        let value = u16_at(buf, offset)?;
        if value == 0 {
            break;
        }
        out.push(value);
    }
    Ok(String::from_utf16_lossy(&out))
}

/// One partition as `IOCTL_DISK_GET_DRIVE_LAYOUT_EX` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutPartition {
    pub number: u32,
    pub offset: u64,
    pub length: u64,
    /// `None` for MBR entries.
    pub type_guid: Option<String>,
    pub guid: Option<String>,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveLayout {
    pub style: u32,
    /// GPT only.
    pub disk_guid: Option<String>,
    pub starting_usable_offset: u64,
    pub usable_length: u64,
    pub partitions: Vec<LayoutPartition>,
}

/// Decode a `DRIVE_LAYOUT_INFORMATION_EX` buffer. Unused MBR slots and
/// zero-length entries are dropped.
pub fn parse_drive_layout(buf: &[u8]) -> ParseResult<DriveLayout> {
    let style = u32_at(buf, 0)?;
    let count = u32_at(buf, 4)?;
    let (disk_guid, starting_usable_offset, usable_length) = if style == STYLE_GPT {
        (
            Some(guid_at(buf, LAYOUT_GPT_DISK_ID)?),
            nonneg_at(buf, LAYOUT_GPT_STARTING_USABLE)?,
            nonneg_at(buf, LAYOUT_GPT_USABLE_LENGTH)?,
        )
    } else {
        (None, 0, 0)
    };
    let count = usize::try_from(count).map_err(|_| "partition count overflow".to_string())?;
    if count > 1024 {
        return Err(format!("implausible partition count {count}"));
    }
    let mut partitions = Vec::with_capacity(count);
    for index in 0..count {
        let base = index
            .checked_mul(ENTRY_BYTES)
            .and_then(|delta| LAYOUT_HEADER_BYTES.checked_add(delta))
            .ok_or_else(|| "partition entry offset overflow".to_string())?;
        let at = |field: usize| {
            base.checked_add(field)
                .ok_or_else(|| "partition field offset overflow".to_string())
        };
        let entry_style = u32_at(buf, at(ENTRY_STYLE)?)?;
        let offset = nonneg_at(buf, at(ENTRY_OFFSET)?)?;
        let length = nonneg_at(buf, at(ENTRY_LENGTH)?)?;
        let number = u32_at(buf, at(ENTRY_NUMBER)?)?;
        // The entry must be fully present even when it is skipped below.
        bytes::<1>(buf, at(ENTRY_BYTES.saturating_sub(1))?)?;
        if length == 0 {
            continue;
        }
        let (type_guid, guid, name) = if entry_style == STYLE_GPT {
            let type_guid = guid_at(buf, at(ENTRY_GPT_TYPE)?)?;
            if is_zero_guid(&type_guid) {
                continue;
            }
            (
                Some(type_guid),
                Some(guid_at(buf, at(ENTRY_GPT_ID)?)?),
                utf16_name(buf, at(ENTRY_GPT_NAME)?, ENTRY_GPT_NAME_UNITS)?,
            )
        } else {
            (None, None, String::new())
        };
        partitions.push(LayoutPartition {
            number,
            offset,
            length,
            type_guid,
            guid,
            name,
        });
    }
    Ok(DriveLayout {
        style,
        disk_guid,
        starting_usable_offset,
        usable_length,
        partitions,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub disk_size: u64,
    pub bytes_per_sector: u64,
}

/// Decode a `DISK_GEOMETRY_EX` buffer.
pub fn parse_geometry(buf: &[u8]) -> ParseResult<Geometry> {
    if buf.len() < GEOMETRY_MIN_BYTES {
        return Err("disk geometry truncated".into());
    }
    Ok(Geometry {
        disk_size: nonneg_at(buf, GEOMETRY_DISK_SIZE)?,
        bytes_per_sector: u64::from(u32_at(buf, GEOMETRY_BYTES_PER_SECTOR)?),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VolumeExtent {
    pub disk_number: u32,
    pub offset: u64,
    pub length: u64,
}

/// Decode a `VOLUME_DISK_EXTENTS` buffer.
pub fn parse_volume_extents(buf: &[u8]) -> ParseResult<Vec<VolumeExtent>> {
    let count = usize::try_from(u32_at(buf, 0)?).map_err(|_| "extent count overflow")?;
    if count > 256 {
        return Err(format!("implausible volume extent count {count}"));
    }
    (0..count)
        .map(|index| {
            let base = index
                .checked_mul(EXTENT_BYTES)
                .and_then(|delta| EXTENTS_FIRST.checked_add(delta))
                .ok_or_else(|| "extent offset overflow".to_string())?;
            let field = |delta: usize| {
                base.checked_add(delta)
                    .ok_or_else(|| "extent field overflow".to_string())
            };
            Ok(VolumeExtent {
                disk_number: u32_at(buf, field(EXTENT_DISK_NUMBER)?)?,
                offset: nonneg_at(buf, field(EXTENT_OFFSET)?)?,
                length: nonneg_at(buf, field(EXTENT_LENGTH)?)?,
            })
        })
        .collect()
}

fn valid_sector(sector: u64) -> bool {
    (512..=65536).contains(&sector) && sector.is_power_of_two()
}

fn lba_span(offset: u64, length: u64, sector: u64) -> ParseResult<(u64, u64)> {
    if !offset.is_multiple_of(sector) || !length.is_multiple_of(sector) || length == 0 {
        return Err(format!(
            "extent {offset}+{length} is not aligned to {sector}-byte sectors"
        ));
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| "extent overflow".to_string())?;
    let last = (end / sector)
        .checked_sub(1)
        .ok_or_else(|| "extent underflow".to_string())?;
    Ok((offset / sector, last))
}

/// Combine the native layout and geometry of disk `number` into the staging
/// [`Disk`]. Usable LBA bounds come straight from the GPT header values the
/// layout IOCTL reports, so no raw read (and no elevation) is required.
pub fn disk_from_layout(
    number: u32,
    geometry: Geometry,
    layout: &DriveLayout,
) -> ParseResult<Disk> {
    if layout.style != STYLE_GPT {
        return Err("the system disk is not GPT".into());
    }
    let sector = geometry.bytes_per_sector;
    if !valid_sector(sector) {
        return Err(format!("unsupported logical sector size {sector}"));
    }
    let guid = layout
        .disk_guid
        .clone()
        .filter(|guid| !is_zero_guid(guid))
        .ok_or_else(|| "the GPT disk has no disk GUID".to_string())?;
    let (first_usable_lba, last_usable_lba) =
        lba_span(layout.starting_usable_offset, layout.usable_length, sector)?;
    if last_usable_lba
        .checked_mul(sector)
        .is_none_or(|end| end >= geometry.disk_size)
    {
        return Err("GPT usable range exceeds the disk".into());
    }
    let mut partitions = Vec::with_capacity(layout.partitions.len());
    for entry in &layout.partitions {
        let (first_lba, last_lba) = lba_span(entry.offset, entry.length, sector)?;
        if first_lba < first_usable_lba || last_lba > last_usable_lba {
            return Err(format!(
                "partition {} lies outside the GPT usable range",
                entry.number
            ));
        }
        let (Some(type_guid), Some(guid)) = (entry.type_guid.clone(), entry.guid.clone()) else {
            return Err(format!("partition {} has no GPT identity", entry.number));
        };
        if entry.number == 0 {
            return Err("a GPT partition has no partition number".into());
        }
        partitions.push(Partition {
            number: entry.number,
            guid,
            type_guid,
            first_lba,
            last_lba,
        });
    }
    partitions.sort_by_key(|partition| partition.first_lba);
    if partitions
        .windows(2)
        .any(|pair| matches!(pair, [a, b] if a.last_lba >= b.first_lba))
    {
        return Err("the partition map has overlapping entries".into());
    }
    Ok(Disk {
        id: number.to_string(),
        guid,
        size_bytes: geometry.disk_size,
        sector_bytes: sector,
        first_usable_lba,
        last_usable_lba,
        partitions,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GptHeader {
    pub disk_guid: String,
    pub first_usable_lba: u64,
    pub last_usable_lba: u64,
    pub entries_lba: u64,
    pub entry_count: u32,
    pub entry_bytes: u32,
    pub entries_crc: u32,
}

impl GptHeader {
    /// Bytes to read at `entries_lba` for the whole entry array, rounded up
    /// to whole sectors.
    pub fn entries_read_bytes(&self, sector: u64) -> ParseResult<u64> {
        let raw = u64::from(self.entry_count)
            .checked_mul(u64::from(self.entry_bytes))
            .ok_or_else(|| "entry array overflow".to_string())?;
        raw.div_ceil(sector)
            .checked_mul(sector)
            .ok_or_else(|| "entry array overflow".to_string())
    }
}

/// Decode and CRC-check the primary GPT header (the sector at LBA 1).
pub fn parse_gpt_header(sector_buf: &[u8]) -> ParseResult<GptHeader> {
    if sector_buf.get(0..8) != Some(b"EFI PART".as_slice()) {
        return Err("LBA 1 does not hold a GPT header".into());
    }
    let header_bytes = usize::try_from(u32_at(sector_buf, 12)?).map_err(|_| "header size")?;
    if header_bytes < 92 || header_bytes > sector_buf.len() {
        return Err(format!("implausible GPT header size {header_bytes}"));
    }
    let stored_crc = u32_at(sector_buf, 16)?;
    let mut header = sector_buf
        .get(..header_bytes)
        .ok_or_else(|| "GPT header truncated".to_string())?
        .to_vec();
    if let Some(field) = header.get_mut(16..20) {
        field.fill(0);
    }
    if crc32fast::hash(&header) != stored_crc {
        return Err("GPT header checksum mismatch".into());
    }
    if u64_at(sector_buf, 24)? != 1 {
        return Err("the GPT header at LBA 1 is not the primary header".into());
    }
    let entry_bytes = u32_at(sector_buf, 84)?;
    if entry_bytes < 128 || entry_bytes % 8 != 0 || entry_bytes > 4096 {
        return Err(format!("implausible GPT entry size {entry_bytes}"));
    }
    let entry_count = u32_at(sector_buf, 80)?;
    if entry_count == 0 || entry_count > 1024 {
        return Err(format!("implausible GPT entry count {entry_count}"));
    }
    Ok(GptHeader {
        disk_guid: guid_at(sector_buf, 56)?,
        first_usable_lba: u64_at(sector_buf, 40)?,
        last_usable_lba: u64_at(sector_buf, 48)?,
        entries_lba: u64_at(sector_buf, 72)?,
        entry_count,
        entry_bytes,
        entries_crc: u32_at(sector_buf, 88)?,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GptEntry {
    /// 1-based index in the entry array: the EFI `HD()` partition number.
    pub index: u32,
    pub type_guid: String,
    pub guid: String,
    pub first_lba: u64,
    pub last_lba: u64,
}

/// Decode and CRC-check the GPT entry array; unused entries are dropped.
pub fn parse_gpt_entries(buf: &[u8], header: &GptHeader) -> ParseResult<Vec<GptEntry>> {
    let entry_bytes =
        usize::try_from(header.entry_bytes).map_err(|_| "entry size overflow".to_string())?;
    let count =
        usize::try_from(header.entry_count).map_err(|_| "entry count overflow".to_string())?;
    let total = entry_bytes
        .checked_mul(count)
        .ok_or_else(|| "entry array overflow".to_string())?;
    let array = buf
        .get(..total)
        .ok_or_else(|| "GPT entry array truncated".to_string())?;
    if crc32fast::hash(array) != header.entries_crc {
        return Err("GPT entry array checksum mismatch".into());
    }
    let mut entries = Vec::new();
    for (slot, raw) in array.chunks_exact(entry_bytes).enumerate() {
        let type_guid = guid_at(raw, 0)?;
        if is_zero_guid(&type_guid) {
            continue;
        }
        let index = u32::try_from(slot)
            .ok()
            .and_then(|slot| slot.checked_add(1))
            .ok_or_else(|| "entry index overflow".to_string())?;
        entries.push(GptEntry {
            index,
            type_guid,
            guid: guid_at(raw, 16)?,
            first_lba: u64_at(raw, 32)?,
            last_lba: u64_at(raw, 40)?,
        });
    }
    Ok(entries)
}

/// The elevated helper reads the GPT itself and requires it to agree exactly
/// with what Windows reports, including that Windows' partition numbers are
/// the GPT entry indices the EFI device path needs.
pub fn cross_check_gpt(disk: &Disk, header: &GptHeader, entries: &[GptEntry]) -> ParseResult<()> {
    if header.disk_guid != disk.guid {
        return Err("GPT header disk GUID differs from the Windows layout".into());
    }
    if header.first_usable_lba != disk.first_usable_lba
        || header.last_usable_lba != disk.last_usable_lba
    {
        return Err("GPT header usable range differs from the Windows layout".into());
    }
    if entries.len() != disk.partitions.len() {
        return Err("GPT entry array and Windows layout list different partitions".into());
    }
    for partition in &disk.partitions {
        let entry = entries
            .iter()
            .find(|entry| entry.guid == partition.guid)
            .ok_or_else(|| format!("partition {} is missing from the GPT", partition.guid))?;
        if entry.type_guid != partition.type_guid
            || entry.first_lba != partition.first_lba
            || entry.last_lba != partition.last_lba
        {
            return Err(format!(
                "partition {} differs between the GPT and Windows",
                partition.guid
            ));
        }
        if entry.index != partition.number {
            return Err(format!(
                "Windows partition number {} is GPT entry {}; refusing an ambiguous device path",
                partition.number, entry.index
            ));
        }
    }
    Ok(())
}
