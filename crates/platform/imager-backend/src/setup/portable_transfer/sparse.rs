//! GNU PAX sparse 0.1: the map is metadata; member bytes concatenate its extents.
//! https://www.gnu.org/software/tar/manual/html_node/PAX-0.html
use super::{changed, check_cancel};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    os::{fd::AsRawFd, unix::fs::FileExt},
    sync::atomic::AtomicBool,
};

const MAX_EXTENTS: usize = 65_536;
const MAX_MAP: usize = 1024 * 1024;
// Unsupported/coarse filesystem hole reporting must not cause unbounded scans.
const MAX_ZERO_READ: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Sparse {
    pub logical: u64,
    pub extents: Vec<(u64, u64)>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct Witness {
    pub sparse: Sparse,
    pub hashes: Vec<[u8; 32]>,
}

fn number(bytes: &[u8]) -> io::Result<u64> {
    if bytes.is_empty() || bytes.len() > 20 || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(changed("Invalid sparse decimal value"));
    }
    bytes.iter().try_fold(0u64, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|v| {
                digit
                    .checked_sub(b'0')
                    .and_then(|digit| v.checked_add(u64::from(digit)))
            })
            .ok_or_else(|| changed("Sparse decimal value overflows"))
    })
}

pub(super) fn parse(keys: &BTreeMap<Vec<u8>, Vec<u8>>, stored: u64) -> io::Result<Option<Sparse>> {
    let sparse_keys = keys
        .keys()
        .filter(|key| key.starts_with(b"GNU.sparse."))
        .collect::<Vec<_>>();
    if sparse_keys.is_empty() {
        return Ok(None);
    }
    if sparse_keys.len() != 4
        || sparse_keys.iter().any(|key| {
            !matches!(
                key.as_slice(),
                b"GNU.sparse.size"
                    | b"GNU.sparse.numblocks"
                    | b"GNU.sparse.map"
                    | b"GNU.sparse.name"
            )
        })
    {
        return Err(changed("Unsupported or mixed sparse format"));
    }
    let field = |key: &[u8]| {
        keys.get(key)
            .ok_or_else(|| changed("Incomplete sparse metadata"))
    };
    let logical = number(field(b"GNU.sparse.size")?)?;
    let count = number(field(b"GNU.sparse.numblocks")?)?;
    let map = field(b"GNU.sparse.map")?;
    let name = field(b"GNU.sparse.name")?;
    if logical > i64::MAX as u64
        || count > MAX_EXTENTS as u64
        || map.len() > MAX_MAP
        || name.is_empty()
        || name.len() > 4096
        || name.contains(&0)
    {
        return Err(changed("Sparse metadata exceeds bounds"));
    }
    let mut fields = map.split(|byte| *byte == b',');
    let mut extents = Vec::with_capacity(count as usize);
    let mut previous = 0u64;
    let mut total = 0u64;
    for index in 0..count {
        let offset = number(
            fields
                .next()
                .ok_or_else(|| changed("Truncated sparse map"))?,
        )?;
        let length = number(
            fields
                .next()
                .ok_or_else(|| changed("Truncated sparse map"))?,
        )?;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| changed("Sparse extent overflows"))?;
        // GNU tar uses a final (logical_size, 0) marker, including all-hole files.
        if offset < previous
            || end > logical
            || (length == 0 && (index.checked_add(1) != Some(count) || offset != logical))
        {
            return Err(changed("Sparse extents overlap or leave logical bounds"));
        }
        total = total
            .checked_add(length)
            .ok_or_else(|| changed("Sparse stored size overflows"))?;
        previous = end;
        extents.push((offset, length));
    }
    if (count == 0 && !map.is_empty()) || (count != 0 && fields.next().is_some()) || total != stored
    {
        return Err(changed("Sparse map differs from stored member size"));
    }
    Ok(Some(Sparse { logical, extents }))
}

impl Sparse {
    fn validate(&self) -> io::Result<()> {
        if self.logical > i64::MAX as u64 || self.extents.len() > MAX_EXTENTS {
            return Err(changed("Sparse witness exceeds bounds"));
        }
        let mut previous = 0;
        for (index, &(offset, length)) in self.extents.iter().enumerate() {
            let end = offset
                .checked_add(length)
                .ok_or_else(|| changed("Sparse witness overflows"))?;
            if offset < previous
                || end > self.logical
                || (length == 0
                    && (index.checked_add(1) != Some(self.extents.len()) || offset != self.logical))
            {
                return Err(changed("Sparse witness has invalid extents"));
            }
            previous = end;
        }
        Ok(())
    }

    /// Allocation coverage, not logical length or concatenated stored bytes.
    /// Multiple selected extents sharing a filesystem block count it once.
    pub(super) fn allocation_bytes(&self, block: u64) -> io::Result<u64> {
        self.validate()?;
        if block == 0 {
            return Err(changed("Sparse allocation block missing"));
        }
        let mut previous_end = 0u64;
        let mut blocks = 0u64;
        for &(offset, length) in &self.extents {
            if length == 0 {
                continue;
            }
            let start = offset / block;
            let end = offset
                .checked_add(length)
                .and_then(|n| n.checked_add(block - 1))
                .map(|n| n / block)
                .ok_or_else(|| changed("Sparse allocation range overflows"))?;
            let covered = end
                .checked_sub(start.max(previous_end))
                .ok_or_else(|| changed("Sparse allocation range regressed"))?;
            blocks = blocks
                .checked_add(covered)
                .ok_or_else(|| changed("Sparse allocation coverage overflows"))?;
            previous_end = end;
        }
        blocks
            .checked_mul(block)
            .ok_or_else(|| changed("Sparse allocation bytes overflow"))
    }

    pub(super) fn hash(&self, reader: &mut impl Read, cancel: &AtomicBool) -> io::Result<Witness> {
        self.validate()?;
        check_cancel(cancel)?;
        let mut hashes = Vec::with_capacity(self.extents.len());
        let mut buffer = [0; 8192];
        for &(_, length) in &self.extents {
            let mut remaining = length;
            let mut digest = Sha256::new();
            while remaining != 0 {
                check_cancel(cancel)?;
                let count = remaining.min(buffer.len() as u64) as usize;
                let bytes = buffer
                    .get_mut(..count)
                    .ok_or_else(|| changed("Sparse read exceeds buffer"))?;
                reader.read_exact(bytes)?;
                digest.update(bytes);
                remaining = remaining
                    .checked_sub(count as u64)
                    .ok_or_else(|| changed("Sparse read exceeds extent"))?;
            }
            hashes.push(digest.finalize().into());
        }
        let mut trailing = [0; 1];
        if reader.read(&mut trailing)? != 0 {
            return Err(changed("Sparse member contains unaccounted data"));
        }
        Ok(Witness {
            sparse: self.clone(),
            hashes,
        })
    }
}

fn read_at(file: &File, mut offset: u64, mut bytes: &mut [u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        match file.read_at(bytes, offset) {
            Ok(0) => return Err(changed("Sparse file became truncated")),
            Ok(count) => {
                offset = offset
                    .checked_add(count as u64)
                    .ok_or_else(|| changed("Sparse read offset overflows"))?;
                bytes = bytes
                    .get_mut(count..)
                    .ok_or_else(|| changed("Sparse read exceeds buffer"))?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn seek(file: &File, offset: u64, kind: i32) -> io::Result<Option<u64>> {
    // Save the shared open-description offset: positional readers and callers
    // must observe exactly the same position after sparse inspection.
    // SAFETY: live descriptor; SEEK_CUR does not alter its position.
    let original = unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_CUR) };
    if original < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: live descriptor, bounded offset checked by sparse parser.
    let result = unsafe { libc::lseek(file.as_raw_fd(), offset as libc::off_t, kind) };
    let error = (result < 0).then(io::Error::last_os_error);
    // SAFETY: live descriptor; original came from a successful seek.
    if unsafe { libc::lseek(file.as_raw_fd(), original, libc::SEEK_SET) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if result >= 0 {
        return Ok(Some(result as u64));
    }
    let error = error.ok_or_else(|| changed("Sparse seek lost its failure"))?;
    if error.raw_os_error() == Some(libc::ENXIO) {
        Ok(None)
    } else {
        Err(error)
    }
}

/// Upper bound matching GNU tar's seek-based sparse producer. This deliberately
/// uses reported data ranges rather than allocation blocks (which may compress).
pub(super) fn stored_size(file: &File) -> io::Result<u64> {
    let metadata = file.metadata()?;
    let logical = metadata.len();
    if !metadata.is_file() || logical > i64::MAX as u64 {
        return Err(changed(
            "Sparse reservation requires a bounded regular file",
        ));
    }
    let mut position = 0;
    let mut stored = 0u64;
    let mut count = 0usize;
    while position < logical {
        let data = match seek(file, position, libc::SEEK_DATA) {
            Ok(None) => break,
            Ok(Some(data)) if data >= position && data <= logical => data,
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) => {
                return Ok(logical);
            }
            _ => {
                return Err(changed(
                    "Filesystem sparse reservation data range is invalid",
                ));
            }
        };
        if data == logical {
            break;
        }
        let hole = match seek(file, data, libc::SEEK_HOLE) {
            Ok(Some(hole)) if hole > data && hole <= logical => hole,
            Ok(None) => logical,
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) => {
                return Ok(logical);
            }
            _ => {
                return Err(changed(
                    "Filesystem sparse reservation hole range is invalid",
                ));
            }
        };
        stored = stored
            .checked_add(
                hole.checked_sub(data)
                    .ok_or_else(|| changed("Sparse reservation regressed"))?,
            )
            .filter(|size| *size <= logical)
            .ok_or_else(|| changed("Sparse reservation overflows"))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| changed("Sparse reservation extent count overflows"))?;
        if count > MAX_EXTENTS {
            return Err(changed("Sparse reservation exceeds extent bounds"));
        }
        position = hole;
    }
    Ok(stored)
}

fn zero_gap(
    file: &File,
    start: u64,
    end: u64,
    budget: &mut u64,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut position = start;
    let mut buffer = [0; 8192];
    while position < end {
        check_cancel(cancel)?;
        let data = match seek(file, position, libc::SEEK_DATA) {
            Ok(None) => return Ok(()),
            Ok(Some(data)) if data >= position => data.min(end),
            Ok(Some(_)) => return Err(changed("Filesystem sparse data position regressed")),
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) => {
                position
            }
            Err(error) => return Err(error),
        };
        if data == end {
            return Ok(());
        }
        let hole = match seek(file, data, libc::SEEK_HOLE) {
            Ok(Some(hole)) if hole > data => hole.min(end),
            Ok(None) => end,
            Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) => end,
            _ => return Err(changed("Filesystem sparse hole position is invalid")),
        };
        position = data;
        while position < hole {
            check_cancel(cancel)?;
            let count = hole
                .checked_sub(position)
                .ok_or_else(|| changed("Sparse gap position regressed"))?
                .min(buffer.len() as u64);
            *budget = budget.checked_sub(count).ok_or_else(|| {
                changed("Filesystem cannot verify sparse holes within bounded storage")
            })?;
            let bytes = buffer
                .get_mut(..count as usize)
                .ok_or_else(|| changed("Sparse gap exceeds buffer"))?;
            read_at(file, position, bytes)?;
            if bytes.iter().any(|byte| *byte != 0) {
                return Err(changed(
                    "Sparse file contains data outside authenticated extents",
                ));
            }
            position = position
                .checked_add(count)
                .ok_or_else(|| changed("Sparse gap position overflows"))?;
        }
    }
    Ok(())
}

impl Witness {
    pub(super) fn verify(&self, file: &File, cancel: &AtomicBool) -> io::Result<()> {
        self.sparse.validate()?;
        check_cancel(cancel)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.len() != self.sparse.logical
            || self.hashes.len() != self.sparse.extents.len()
        {
            return Err(changed("Sparse file logical size differs"));
        }
        let mut previous = 0;
        let mut budget = MAX_ZERO_READ;
        let mut buffer = [0; 8192];
        for (&(offset, length), expected) in self.sparse.extents.iter().zip(&self.hashes) {
            zero_gap(file, previous, offset, &mut budget, cancel)?;
            let mut digest = Sha256::new();
            let mut position = offset;
            let end = offset
                .checked_add(length)
                .ok_or_else(|| changed("Sparse extent overflows"))?;
            while position < end {
                check_cancel(cancel)?;
                let count = end
                    .checked_sub(position)
                    .ok_or_else(|| changed("Sparse extent position regressed"))?
                    .min(buffer.len() as u64) as usize;
                let bytes = buffer
                    .get_mut(..count)
                    .ok_or_else(|| changed("Sparse extent exceeds buffer"))?;
                read_at(file, position, bytes)?;
                digest.update(bytes);
                position = position
                    .checked_add(count as u64)
                    .ok_or_else(|| changed("Sparse extent position overflows"))?;
            }
            if <[u8; 32]>::from(digest.finalize()) != *expected {
                return Err(changed("Sparse extent content differs"));
            }
            previous = end;
        }
        zero_gap(file, previous, self.sparse.logical, &mut budget, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(size: &str, blocks: &str, map: &str) -> BTreeMap<Vec<u8>, Vec<u8>> {
        [
            ("GNU.sparse.size", size),
            ("GNU.sparse.numblocks", blocks),
            ("GNU.sparse.map", map),
            ("GNU.sparse.name", "data"),
        ]
        .into_iter()
        .map(|(key, value)| (key.as_bytes().to_vec(), value.as_bytes().to_vec()))
        .collect()
    }
    #[test]
    fn sparse_map_bounds_and_terminal_marker() {
        assert!(parse(&keys("100", "2", "10,5,100,0"), 5).unwrap().is_some());
        assert!(parse(&keys("100", "1", "100,0"), 0).unwrap().is_some());
        for (size, count, map, stored) in [
            ("100", "2", "10,5,14,2", 7),
            ("100", "1", "99,2", 2),
            ("100", "1", "100,1", 1),
            ("100", "1", "99,0", 0),
            ("100", "2", "1,0,100,0", 0),
            ("100", "1", "1,2", 3),
            ("100", "1", "1,2,3", 2),
            ("100", "1", "1", 0),
            ("100", "1", "18446744073709551615,2", 2),
            ("9223372036854775808", "0", "", 0),
            ("100", "65537", "", 0),
        ] {
            assert!(parse(&keys(size, count, map), stored).is_err(), "{map}");
        }
        let mut metadata = keys("100", "1", "100,0");
        metadata.insert(b"GNU.sparse.major".to_vec(), b"1".to_vec());
        assert!(parse(&metadata, 0).is_err());
        metadata.remove(b"GNU.sparse.major".as_slice());
        metadata.remove(b"GNU.sparse.name".as_slice());
        assert!(parse(&metadata, 0).is_err());
    }
    #[test]
    fn actual_sparse_file_verifies_extents_and_rejects_gap_or_size_changes() {
        let directory = tempfile::tempdir().unwrap();
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.path().join("data"))
            .unwrap();
        let logical = 64 * 1024 * 1024;
        file.set_len(logical).unwrap();
        file.write_all_at(b"data", 4096).unwrap();
        use std::io::{Seek, SeekFrom};
        let mut position_check = &file;
        position_check.seek(SeekFrom::Start(123)).unwrap();
        assert!(stored_size(&file).unwrap() < logical);
        assert_eq!(position_check.stream_position().unwrap(), 123);
        let sparse = parse(
            &keys(&logical.to_string(), "2", &format!("4096,4,{logical},0")),
            4,
        )
        .unwrap()
        .unwrap();
        let cancel = AtomicBool::new(false);
        let witness = sparse.hash(&mut b"data".as_slice(), &cancel).unwrap();
        witness.verify(&file, &cancel).unwrap();
        file.write_all_at(b"x", 32 * 1024 * 1024).unwrap();
        assert!(witness.verify(&file, &cancel).is_err());
        file.write_all_at(&[0], 32 * 1024 * 1024).unwrap();
        file.write_all_at(b"bad!", 4096).unwrap();
        assert!(witness.verify(&file, &cancel).is_err());
        file.write_all_at(b"data", 4096).unwrap();
        file.set_len(logical.checked_sub(1).unwrap()).unwrap();
        assert!(witness.verify(&file, &cancel).is_err());
    }
}
