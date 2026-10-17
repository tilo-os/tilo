//! Whether Windows flagged a volume for a disk check: the `VOLUME_IS_DIRTY`
//! bit of `$Volume`'s volume-information attribute.
//!
//! The other unclean states (hibernation, Fast Startup, an unfinished
//! journal) are what `ntfs-3g.probe` reports; it ignores this bit, so the
//! service reads it here. The bytes come from the drive being checked, so this
//! runs only in the confined preflight process, and every offset is checked.

use std::io;

/// `$Volume` is record 3 of the MFT.
const VOLUME_RECORD: u64 = 3;
/// NTFS protects each 512-byte stride of a record with an update sequence.
const FIXUP_STRIDE: usize = 512;
const ATTR_VOLUME_INFORMATION: u32 = 0x70;
const ATTR_END: u32 = 0xFFFF_FFFF;
const VOLUME_IS_DIRTY: u16 = 0x0001;
const MAX_RECORD_SIZE: u64 = 64 * 1024;

/// Why a volume could not be read far enough to decide.
#[derive(Debug, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Malformed> {
    let end = at.checked_add(2).ok_or(Malformed("offset overflow"))?;
    bytes
        .get(at..end)
        .and_then(|slice| slice.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(Malformed("field past the end"))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Malformed> {
    let end = at.checked_add(4).ok_or(Malformed("offset overflow"))?;
    bytes
        .get(at..end)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(Malformed("field past the end"))
}

fn u64_at(bytes: &[u8], at: usize) -> Result<u64, Malformed> {
    let end = at.checked_add(8).ok_or(Malformed("offset overflow"))?;
    bytes
        .get(at..end)
        .and_then(|slice| slice.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or(Malformed("field past the end"))
}

/// The byte offset and size of `$Volume`'s MFT record, from the boot sector.
///
/// # Errors
///
/// Returns [`Malformed`] for geometry no NTFS volume has.
pub fn volume_record_location(boot: &[u8]) -> Result<(u64, usize), Malformed> {
    let bytes_per_sector = u64::from(u16_at(boot, 0x0B)?);
    if !(512..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() {
        return Err(Malformed("sector size"));
    }
    let raw_spc = *boot.get(0x0D).ok_or(Malformed("boot sector"))?;
    // Values above 0x80 encode 2^(256 - value) sectors per cluster.
    let sectors_per_cluster = if raw_spc > 0x80 {
        1_u64
            .checked_shl(256_u32.saturating_sub(u32::from(raw_spc)))
            .ok_or(Malformed("cluster size"))?
    } else {
        u64::from(raw_spc)
    };
    let cluster = bytes_per_sector
        .checked_mul(sectors_per_cluster)
        .filter(|size| *size > 0 && *size <= 2 * 1024 * 1024)
        .ok_or(Malformed("cluster size"))?;
    let raw_record = i8::from_le_bytes([*boot.get(0x40).ok_or(Malformed("boot sector"))?]);
    let record = if raw_record < 0 {
        1_u64
            .checked_shl(u32::from(raw_record.unsigned_abs()))
            .ok_or(Malformed("record size"))?
    } else {
        cluster
            .checked_mul(u64::from(raw_record.unsigned_abs()))
            .ok_or(Malformed("record size"))?
    };
    if !(1024..=MAX_RECORD_SIZE).contains(&record) || !record.is_power_of_two() {
        return Err(Malformed("record size"));
    }
    let mft = u64_at(boot, 0x30)?
        .checked_mul(cluster)
        .ok_or(Malformed("MFT location"))?;
    let offset = record
        .checked_mul(VOLUME_RECORD)
        .and_then(|into| mft.checked_add(into))
        .ok_or(Malformed("MFT location"))?;
    let size = usize::try_from(record).map_err(|_| Malformed("record size"))?;
    Ok((offset, size))
}

/// Undo the update sequence on a record read from disk.
fn apply_fixups(record: &mut [u8]) -> Result<(), Malformed> {
    if record.get(0..4) != Some(b"FILE".as_slice()) {
        return Err(Malformed("not an MFT record"));
    }
    let usa_offset = usize::from(u16_at(record, 4)?);
    let usa_count = usize::from(u16_at(record, 6)?);
    let strides = record.len() / FIXUP_STRIDE;
    if usa_count != strides.saturating_add(1) {
        return Err(Malformed("update sequence"));
    }
    let usn = u16_at(record, usa_offset)?;
    for stride in 1..usa_count {
        let replacement_at = usa_offset
            .checked_add(stride.checked_mul(2).ok_or(Malformed("update sequence"))?)
            .ok_or(Malformed("update sequence"))?;
        let replacement = u16_at(record, replacement_at)?;
        let tail = stride
            .checked_mul(FIXUP_STRIDE)
            .and_then(|end| end.checked_sub(2))
            .ok_or(Malformed("update sequence"))?;
        if u16_at(record, tail)? != usn {
            return Err(Malformed("torn record"));
        }
        let slot = record
            .get_mut(tail..tail.saturating_add(2))
            .ok_or(Malformed("update sequence"))?;
        slot.copy_from_slice(&replacement.to_le_bytes());
    }
    Ok(())
}

/// Whether the `$Volume` record (as read from disk) carries the dirty bit.
///
/// # Errors
///
/// Returns [`Malformed`] if the record or its attributes do not parse.
pub fn record_is_dirty(record: &mut [u8]) -> Result<bool, Malformed> {
    apply_fixups(record)?;
    let in_use = usize::try_from(u32_at(record, 0x18)?).map_err(|_| Malformed("record"))?;
    let end = in_use.min(record.len());
    let mut at = usize::from(u16_at(record, 0x14)?);
    while at < end {
        let kind = u32_at(record, at)?;
        if kind == ATTR_END {
            break;
        }
        let length = usize::try_from(u32_at(record, at.saturating_add(4))?)
            .map_err(|_| Malformed("attribute"))?;
        if length < 0x18 || at.checked_add(length).is_none_or(|next| next > end) {
            return Err(Malformed("attribute length"));
        }
        let resident = *record
            .get(at.saturating_add(8))
            .ok_or(Malformed("attribute"))?
            == 0;
        if kind == ATTR_VOLUME_INFORMATION && resident {
            let value_length = usize::try_from(u32_at(record, at.saturating_add(0x10))?)
                .map_err(|_| Malformed("attribute"))?;
            let value = at
                .checked_add(usize::from(u16_at(record, at.saturating_add(0x14))?))
                .ok_or(Malformed("attribute"))?;
            if value_length < 12 || value.saturating_add(value_length) > at.saturating_add(length) {
                return Err(Malformed("volume information"));
            }
            return Ok(u16_at(record, value.saturating_add(0x0A))? & VOLUME_IS_DIRTY != 0);
        }
        at = at.saturating_add(length);
    }
    Err(Malformed("no volume information"))
}

/// Read the dirty bit through `read_at(offset, buffer)`.
///
/// # Errors
///
/// Returns the read error, or an `InvalidData` error naming what did not
/// parse.
pub fn volume_is_dirty(
    mut read_at: impl FnMut(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<bool> {
    let invalid = |Malformed(what): Malformed| io::Error::new(io::ErrorKind::InvalidData, what);
    let mut boot = [0_u8; 512];
    read_at(0, &mut boot)?;
    let (offset, size) = volume_record_location(&boot).map_err(invalid)?;
    let mut record = vec![0_u8; size];
    read_at(offset, &mut record)?;
    record_is_dirty(&mut record).map_err(invalid)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "a fixed test image whose offsets are constants"
)]
pub(crate) mod image {
    //! A minimal NTFS image: a boot sector and a `$Volume` record, enough
    //! for the dirty-bit check.

    /// Bytes of a volume whose MFT starts at cluster 4 with 4 KiB clusters
    /// and 1 KiB records, with `flags` in `$Volume`.
    pub fn volume(flags: u16) -> Vec<u8> {
        let cluster = 4096;
        let record = 1024;
        let mft = 4 * cluster;
        let mut image = vec![0_u8; mft + 4 * record];
        image[3..11].copy_from_slice(b"NTFS    ");
        image[0x0B..0x0D].copy_from_slice(&512_u16.to_le_bytes());
        image[0x0D] = 8;
        image[0x30..0x38].copy_from_slice(&4_u64.to_le_bytes());
        image[0x40] = 0xF6; // -10: 2^10-byte records
        image[0x48..0x50].copy_from_slice(&0x1234_u64.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xAA;

        let at = mft + 3 * record;
        let rec = &mut image[at..at + record];
        rec[0..4].copy_from_slice(b"FILE");
        rec[4..6].copy_from_slice(&0x30_u16.to_le_bytes()); // update sequence offset
        rec[6..8].copy_from_slice(&3_u16.to_le_bytes()); // 1 + two strides
        rec[0x14..0x16].copy_from_slice(&0x38_u16.to_le_bytes()); // first attribute
        rec[0x16..0x18].copy_from_slice(&1_u16.to_le_bytes()); // in use
        rec[0x18..0x1C].copy_from_slice(&0x100_u32.to_le_bytes()); // bytes in use
        // $VOLUME_NAME (0x60), empty resident value, to be skipped.
        let name = 0x38;
        rec[name..name + 4].copy_from_slice(&0x60_u32.to_le_bytes());
        rec[name + 4..name + 8].copy_from_slice(&0x18_u32.to_le_bytes());
        rec[name + 0x14..name + 0x16].copy_from_slice(&0x18_u16.to_le_bytes());
        // $VOLUME_INFORMATION (0x70): 12-byte resident value.
        let info = name + 0x18;
        rec[info..info + 4].copy_from_slice(&0x70_u32.to_le_bytes());
        rec[info + 4..info + 8].copy_from_slice(&0x28_u32.to_le_bytes());
        rec[info + 0x10..info + 0x14].copy_from_slice(&12_u32.to_le_bytes());
        rec[info + 0x14..info + 0x16].copy_from_slice(&0x18_u16.to_le_bytes());
        rec[info + 0x18 + 8] = 3; // NTFS 3.1
        rec[info + 0x18 + 9] = 1;
        rec[info + 0x18 + 0x0A..info + 0x18 + 0x0C].copy_from_slice(&flags.to_le_bytes());
        let end = info + 0x28;
        rec[end..end + 4].copy_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
        // Apply the update sequence the way Windows writes it.
        let usn = 0x0007_u16.to_le_bytes();
        rec[0x30..0x32].copy_from_slice(&usn);
        for stride in 1..=2 {
            let tail = stride * 512 - 2;
            let original = [rec[tail], rec[tail + 1]];
            rec[0x30 + stride * 2..0x32 + stride * 2].copy_from_slice(&original);
            rec[tail..tail + 2].copy_from_slice(&usn);
        }
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(image: &[u8]) -> io::Result<bool> {
        volume_is_dirty(|offset, buffer| {
            let start = usize::try_from(offset).unwrap();
            let bytes = image
                .get(start..start.saturating_add(buffer.len()))
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
            buffer.copy_from_slice(bytes);
            Ok(())
        })
    }

    #[test]
    fn the_dirty_bit_is_read_through_the_update_sequence() {
        assert!(!check(&image::volume(0)).unwrap());
        assert!(check(&image::volume(VOLUME_IS_DIRTY)).unwrap());
    }

    #[test]
    fn torn_or_truncated_records_refuse_rather_than_guess() {
        let mut torn = image::volume(0);
        let at = 4 * 4096 + 3 * 1024 + 510;
        torn[at] ^= 0xFF;
        assert_eq!(check(&torn).unwrap_err().kind(), io::ErrorKind::InvalidData);

        let mut overlong = image::volume(0);
        let attribute = 4 * 4096 + 3 * 1024 + 0x38 + 4;
        overlong[attribute..attribute + 4].copy_from_slice(&0x0010_0000_u32.to_le_bytes());
        assert_eq!(
            check(&overlong).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let image = image::volume(0);
        assert!(check(&image[..4 * 4096]).is_err());
    }

    #[test]
    fn impossible_geometry_is_refused() {
        let mut boot = image::volume(0)[..512].to_vec();
        boot[0x0B..0x0D].copy_from_slice(&100_u16.to_le_bytes());
        assert_eq!(volume_record_location(&boot), Err(Malformed("sector size")));
        let mut boot = image::volume(0)[..512].to_vec();
        boot[0x40] = 0x90; // 2^112-byte records
        assert!(volume_record_location(&boot).is_err());
    }
}
