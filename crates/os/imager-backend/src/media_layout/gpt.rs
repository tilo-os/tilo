use super::{
    DATA_PARTITION_NAME, DATA_PARTITION_TYPE, File, MediaPartitionBinding, SECTOR, invalid, io,
};
use std::os::unix::fs::FileExt;
const ARRAY_MAX: usize = 512 * 1024;
fn bytes(data: &[u8], start: usize, count: usize) -> io::Result<&[u8]> {
    data.get(
        start
            ..start
                .checked_add(count)
                .ok_or_else(|| invalid("GPT bounds overflow"))?,
    )
    .ok_or_else(|| invalid("GPT field exceeds bounds"))
}
fn u32_at(data: &[u8], at: usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(
        bytes(data, at, 4)?
            .try_into()
            .map_err(|_| invalid("GPT integer"))?,
    ))
}
fn u64_at(data: &[u8], at: usize) -> io::Result<u64> {
    Ok(u64::from_le_bytes(
        bytes(data, at, 8)?
            .try_into()
            .map_err(|_| invalid("GPT integer"))?,
    ))
}
fn put(data: &mut [u8], at: usize, value: &[u8]) -> io::Result<()> {
    data.get_mut(
        at..at
            .checked_add(value.len())
            .ok_or_else(|| invalid("GPT bounds overflow"))?,
    )
    .ok_or_else(|| invalid("GPT field exceeds bounds"))?
    .copy_from_slice(value);
    Ok(())
}
fn add_lba(left: u64, right: u64) -> io::Result<u64> {
    left.checked_add(right)
        .ok_or_else(|| invalid("GPT LBA addition overflows"))
}
fn sub_lba(left: u64, right: u64) -> io::Result<u64> {
    left.checked_sub(right)
        .ok_or_else(|| invalid("GPT LBA subtraction underflows"))
}
fn offset(lba: u64) -> io::Result<u64> {
    lba.checked_mul(SECTOR)
        .ok_or_else(|| invalid("GPT offset overflows"))
}
fn read(file: &File, at: u64, length: usize) -> io::Result<Vec<u8>> {
    let mut value = vec![0; length];
    file.read_exact_at(&mut value, at)?;
    Ok(value)
}
#[derive(Clone)]
struct Header {
    raw: Vec<u8>,
    current: u64,
    alternate: u64,
    first: u64,
    last: u64,
    array_lba: u64,
    count: usize,
    array: Vec<u8>,
    disk: String,
}
impl Header {
    fn read(file: &File, lba: u64, total: u64) -> io::Result<Self> {
        if lba == 0 || lba >= total {
            return Err(invalid("GPT header is outside device"));
        }
        let raw = read(file, offset(lba)?, 512)?;
        let size = usize::try_from(u32_at(&raw, 12)?).map_err(io::Error::other)?;
        if bytes(&raw, 0, 8)? != b"EFI PART"
            || u32_at(&raw, 8)? != 0x0001_0000
            || !(92..=512).contains(&size)
            || u32_at(&raw, 20)? != 0
        {
            return Err(invalid("Invalid native GPT header"));
        }
        let mut checksum = bytes(&raw, 0, size)?.to_vec();
        put(&mut checksum, 16, &[0; 4])?;
        if crc32fast::hash(&checksum) != u32_at(&raw, 16)? {
            return Err(invalid("GPT header checksum differs"));
        }
        let current = u64_at(&raw, 24)?;
        let alternate = u64_at(&raw, 32)?;
        let first = u64_at(&raw, 40)?;
        let last = u64_at(&raw, 48)?;
        let array_lba = u64_at(&raw, 72)?;
        let count = usize::try_from(u32_at(&raw, 80)?).map_err(io::Error::other)?;
        let array_bytes = count
            .checked_mul(128)
            .ok_or_else(|| invalid("GPT array overflows"))?;
        if current != lba
            || alternate == lba
            || alternate >= total
            || first > last
            || last >= total
            || count == 0
            || array_bytes > ARRAY_MAX
            || u32_at(&raw, 84)? != 128
            || array_lba < 2
            || array_lba
                .checked_add((array_bytes as u64).div_ceil(SECTOR))
                .is_none_or(|end| end > total)
        {
            return Err(invalid("GPT geometry exceeds supported bounds"));
        }
        let array = read(file, offset(array_lba)?, array_bytes)?;
        if crc32fast::hash(&array) != u32_at(&raw, 88)? {
            return Err(invalid("GPT partition array checksum differs"));
        }
        let disk = uuid::Uuid::from_bytes_le(
            bytes(&raw, 56, 16)?
                .try_into()
                .map_err(|_| invalid("GPT GUID"))?,
        )
        .to_string();
        Ok(Self {
            raw,
            current,
            alternate,
            first,
            last,
            array_lba,
            count,
            array,
            disk,
        })
    }
    fn encode(&mut self) -> io::Result<()> {
        put(&mut self.raw, 24, &self.current.to_le_bytes())?;
        put(&mut self.raw, 32, &self.alternate.to_le_bytes())?;
        put(&mut self.raw, 40, &self.first.to_le_bytes())?;
        put(&mut self.raw, 48, &self.last.to_le_bytes())?;
        put(&mut self.raw, 72, &self.array_lba.to_le_bytes())?;
        put(
            &mut self.raw,
            88,
            &crc32fast::hash(&self.array).to_le_bytes(),
        )?;
        put(&mut self.raw, 16, &[0; 4])?;
        let size = usize::try_from(u32_at(&self.raw, 12)?).map_err(io::Error::other)?;
        let crc = crc32fast::hash(bytes(&self.raw, 0, size)?);
        put(&mut self.raw, 16, &crc.to_le_bytes())
    }
}
fn pair(file: &File, total: u64) -> io::Result<(Header, Header)> {
    let primary = Header::read(file, 1, total)?;
    let backup = Header::read(file, primary.alternate, total)?;
    if backup.alternate != 1
        || primary.disk != backup.disk
        || primary.first != backup.first
        || primary.last != backup.last
        || primary.count != backup.count
        || primary.array != backup.array
        || primary.array_lba != 2
        || primary.first < add_lba(2, (primary.array.len() as u64).div_ceil(SECTOR))?
        || add_lba(
            backup.array_lba,
            (backup.array.len() as u64).div_ceil(SECTOR),
        )? != backup.current
        || primary.last >= backup.array_lba
    {
        return Err(invalid("GPT copies are inconsistent"));
    }
    validate_partitions(&primary)?;
    Ok((primary, backup))
}
fn validate_partitions(header: &Header) -> io::Result<()> {
    let mut regions = Vec::new();
    let mut identifiers = std::collections::BTreeSet::new();
    for entry in header.array.chunks_exact(128) {
        if bytes(entry, 0, 16)?.iter().all(|value| *value == 0) {
            continue;
        }
        let start = u64_at(entry, 32)?;
        let end = u64_at(entry, 40)?;
        if start < header.first || end > header.last || start > end {
            return Err(invalid("GPT partition exceeds usable bounds"));
        }
        if bytes(entry, 16, 16)?.iter().all(|value| *value == 0) {
            return Err(invalid("GPT partition GUID is missing"));
        }
        if !identifiers.insert(bytes(entry, 16, 16)?.to_vec()) {
            return Err(invalid("GPT partition GUIDs are duplicated"));
        }
        regions.push((start, end));
    }
    regions.sort_unstable();
    if regions
        .windows(2)
        .any(|window| matches!(window, [a,b] if a.1>=b.0))
    {
        return Err(invalid("GPT partitions overlap"));
    }
    Ok(())
}
fn mbr(file: &File) -> io::Result<Vec<u8>> {
    let value = read(file, 0, 512)?;
    if bytes(&value, 510, 2)? != [0x55, 0xaa]
        || bytes(&value, 450, 1)? != [0xee]
        || u32_at(&value, 454)? != 1
        || bytes(&value, 462, 48)?.iter().any(|byte| *byte != 0)
    {
        return Err(invalid(
            "Migration provisioning refuses missing protective or hybrid MBR",
        ));
    }
    Ok(value)
}
fn append_partition(
    primary: &mut Header,
    first: u64,
    last: u64,
    partition_guid: uuid::Uuid,
) -> io::Result<()> {
    let slot = primary
        .array
        .chunks_exact(128)
        .position(|entry| entry.iter().all(|byte| *byte == 0))
        .ok_or_else(|| invalid("Image GPT has no unused partition slot"))?;
    let mut entry = vec![0; 128];
    put(
        &mut entry,
        0,
        &uuid::Uuid::parse_str(DATA_PARTITION_TYPE)
            .map_err(io::Error::other)?
            .to_bytes_le(),
    )?;
    put(&mut entry, 16, &partition_guid.to_bytes_le())?;
    put(&mut entry, 32, &first.to_le_bytes())?;
    put(&mut entry, 40, &last.to_le_bytes())?;
    for (index, value) in DATA_PARTITION_NAME.encode_utf16().enumerate() {
        let at = index
            .checked_mul(2)
            .and_then(|at| at.checked_add(56))
            .ok_or_else(|| invalid("GPT name offset overflows"))?;
        put(&mut entry, at, &value.to_le_bytes())?;
    }
    let entry_offset = slot
        .checked_mul(128)
        .ok_or_else(|| invalid("GPT slot offset overflows"))?;
    put(&mut primary.array, entry_offset, &entry)?;
    Ok(())
}
pub struct Layout {
    primary: Header,
    backup: Header,
    protective: Vec<u8>,
    original: Header,
    original_backup: Header,
}
impl Layout {
    /// # Errors
    /// Rejects inconsistent planned geometry, an invalid profile gap, or missing/ambiguous migration partition markers.
    pub fn planned_binding(
        &self,
        target_bytes: u64,
        stable: &str,
    ) -> io::Result<MediaPartitionBinding> {
        binding_headers(&self.primary, &self.backup, target_bytes, stable)
    }
    /// # Errors
    /// Rejects unsupported image/target geometry, inconsistent GPT copies, occupied migration slots, insufficient space, or native read failures.
    pub fn plan(
        file: &File,
        image_bytes: u64,
        target_bytes: u64,
        stable: &str,
    ) -> io::Result<Self> {
        Self::plan_with_guid(
            file,
            image_bytes,
            target_bytes,
            stable,
            uuid::Uuid::new_v4(),
        )
    }
    /// # Errors
    /// Rejects a nil partition GUID and all geometry, GPT consistency, and capacity failures reported by [`Self::plan`].
    pub fn plan_with_guid(
        file: &File,
        image_bytes: u64,
        target_bytes: u64,
        stable: &str,
        partition_guid: uuid::Uuid,
    ) -> io::Result<Self> {
        if partition_guid.is_nil() {
            return Err(invalid("Migration partition GUID must be nonzero"));
        }
        if image_bytes < 3 * SECTOR
            || !image_bytes.is_multiple_of(SECTOR)
            || !target_bytes.is_multiple_of(SECTOR)
            || target_bytes <= image_bytes
        {
            return Err(invalid(
                "Media needs unused capacity beyond the verified installer image",
            ));
        }
        let total = target_bytes / SECTOR;
        let (mut primary, mut backup) = pair(file, total)?;
        if primary.alternate != sub_lba(image_bytes / SECTOR, 1)? {
            return Err(invalid(
                "Verified image GPT backup does not match its exact image boundary",
            ));
        }
        let marker = uuid::Uuid::parse_str(DATA_PARTITION_TYPE)
            .map_err(io::Error::other)?
            .to_bytes_le();
        if primary
            .array
            .chunks_exact(128)
            .any(|entry| entry.get(..16) == Some(marker.as_slice()))
        {
            return Err(invalid("Installer image already contains migration layout"));
        }
        let original = primary.clone();
        let original_backup = backup.clone();
        let array_sectors = (primary.array.len() as u64).div_ceil(SECTOR);
        let backup_array = total
            .checked_sub(add_lba(1, array_sectors)?)
            .ok_or_else(|| invalid("Media geometry is too small"))?;
        let last_usable = backup_array
            .checked_sub(add_lba(
                crate::media_model::PROFILE_BLOCK_SIZE as u64 / SECTOR,
                1,
            )?)
            .ok_or_else(|| invalid("Media cannot hold backup GPT and profile gap"))?;
        let first = (image_bytes / SECTOR)
            .div_ceil(2048)
            .checked_mul(2048)
            .ok_or_else(|| invalid("Media alignment overflows"))?;
        let aligned_end = (add_lba(last_usable, 1)? / 2048)
            .checked_mul(2048)
            .ok_or_else(|| invalid("Media alignment overflows"))?;
        let last = sub_lba(aligned_end, 1)?;
        if last < first || add_lba(sub_lba(last, first)?, 1)? < 32_768 || original.last >= first {
            return Err(invalid(
                "Media has insufficient unused capacity for a migration data filesystem",
            ));
        }
        append_partition(&mut primary, first, last, partition_guid)?;
        primary.alternate = sub_lba(total, 1)?;
        primary.last = last_usable;
        backup.current = sub_lba(total, 1)?;
        backup.alternate = 1;
        backup.last = last_usable;
        backup.array_lba = backup_array;
        backup.array.clone_from(&primary.array);
        primary.encode()?;
        backup.encode()?;
        let mut protective = mbr(file)?;
        put(
            &mut protective,
            458,
            &u32::try_from(sub_lba(total, 1)?)
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        )?;
        let result = Self {
            primary,
            backup,
            protective,
            original,
            original_backup,
        };
        let _ = stable;
        Ok(result)
    }
    /// # Errors
    /// Returns the native write or synchronization error if the backup GPT cannot be durably stored.
    pub fn stage_backup(&self, file: &File) -> io::Result<()> {
        file.write_all_at(&self.backup.array, offset(self.backup.array_lba)?)?;
        file.write_all_at(&self.backup.raw, offset(self.backup.current)?)?;
        file.sync_all()
    }
    /// # Errors
    /// Rejects changed GPT metadata, cancellation, native I/O failures, or inconsistent committed read-back.
    pub fn commit(&self, file: &File, cancelled: &impl Fn() -> bool) -> io::Result<()> {
        let total = add_lba(self.backup.current, 1)?;
        let old = Header::read(file, self.original_backup.current, total)?;
        if old.raw != self.original_backup.raw || old.array != self.original_backup.array {
            return Err(invalid("Held original backup GPT changed after planning"));
        }
        let unchanged = Header::read(file, 1, total).is_ok_and(|current| {
            current.raw == self.original.raw && current.array == self.original.array
        });
        if !unchanged {
            // A retained approved plan can repair only its own durable backup.
            // The original image backup must still independently match as well.
            let staged = Header::read(file, self.backup.current, total)?;
            if staged.raw != self.backup.raw || staged.array != self.backup.array {
                return Err(invalid("Held target GPT changed after planning"));
            }
        }
        if cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        self.stage_backup(file)?;
        // Finish this tiny transaction once backup is durable; cancellation
        // never leaves a success claim with mutually inconsistent GPT copies.
        file.write_all_at(&self.primary.array, offset(self.primary.array_lba)?)?;
        file.write_all_at(&self.primary.raw, offset(1)?)?;
        file.write_all_at(&self.protective, 0)?;
        file.sync_all()?;
        let (actual, backup) = pair(file, total)?;
        if actual.raw != self.primary.raw
            || actual.array != self.primary.array
            || backup.raw != self.backup.raw
        {
            return Err(invalid("Provisioned GPT read-back differs"));
        }
        Ok(())
    }
}
/// # Errors
/// Rejects invalid protective MBRs, inconsistent GPT copies, unsupported profile gaps, or missing/ambiguous migration markers.
pub fn binding(file: &File, target_bytes: u64, stable: &str) -> io::Result<MediaPartitionBinding> {
    let total = target_bytes / SECTOR;
    let (primary, backup) = pair(file, total)?;
    mbr(file)?;
    binding_headers(&primary, &backup, target_bytes, stable)
}
fn binding_headers(
    primary: &Header,
    backup: &Header,
    target_bytes: u64,
    stable: &str,
) -> io::Result<MediaPartitionBinding> {
    let total = target_bytes / SECTOR;
    if primary.alternate != sub_lba(total, 1)?
        || add_lba(
            add_lba(primary.last, 1)?,
            crate::media_model::PROFILE_BLOCK_SIZE as u64 / SECTOR,
        )? != backup.array_lba
    {
        return Err(invalid("Provisioned GPT profile gap is invalid"));
    }
    let expected = uuid::Uuid::parse_str(DATA_PARTITION_TYPE)
        .map_err(io::Error::other)?
        .to_bytes_le();
    let mut found = None;
    for (slot, entry) in primary.array.chunks_exact(128).enumerate() {
        if bytes(entry, 0, 16)? != expected {
            continue;
        }
        let mut label = Vec::new();
        for part in bytes(entry, 56, 72)?.chunks_exact(2) {
            let value = u16::from_le_bytes(part.try_into().map_err(|_| invalid("GPT name"))?);
            if value == 0 {
                break;
            }
            label.push(value);
        }
        if String::from_utf16(&label).map_err(io::Error::other)? != DATA_PARTITION_NAME
            || found.is_some()
        {
            return Err(invalid("Migration data partition is ambiguous"));
        }
        found = Some(MediaPartitionBinding {
            schema: 1,
            target_stable_id: stable.into(),
            disk_guid: primary.disk.clone(),
            partition_guid: uuid::Uuid::from_bytes_le(
                bytes(entry, 16, 16)?
                    .try_into()
                    .map_err(|_| invalid("GPT GUID"))?,
            )
            .to_string(),
            number: u32::try_from(
                slot.checked_add(1)
                    .ok_or_else(|| invalid("GPT slot overflows"))?,
            )
            .map_err(io::Error::other)?,
            first_lba: u64_at(entry, 32)?,
            last_lba: u64_at(entry, 40)?,
            sector_bytes: SECTOR,
            profile_offset: offset(add_lba(primary.last, 1)?)?,
        });
    }
    found.ok_or_else(|| invalid("Migration data partition marker is missing"))
}
