//! Fixed restricted ioctls on the genuine held Stage. These bytes select a
//! publication purpose; only the receiving filesystem's real grants authorize it.
use std::{fs::File, io, os::fd::AsRawFd};
pub const SIZE: usize = 264;
pub const PROBE: u32 = 0x8004_7451;
pub const ARM: u32 = 0x4108_7452;
pub const MAGIC: [u8; 4] = *b"TAS1";

/// A closed raw destination component; no record ID or authority is carried.
/// # Errors
/// Refuses empty, escaped or oversized native components.
pub fn encode(target: &[u8]) -> io::Result<[u8; SIZE]> {
    if target.is_empty()
        || target.len() > 255
        || target == b"."
        || target == b".."
        || target.contains(&0)
        || target.contains(&b'/')
    {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let len = u16::try_from(target.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let end = 8usize
        .checked_add(target.len())
        .ok_or(io::ErrorKind::InvalidInput)?;
    let mut out = [0; SIZE];
    out.get_mut(..4)
        .ok_or(io::ErrorKind::InvalidInput)?
        .copy_from_slice(&MAGIC);
    out.get_mut(4..6)
        .ok_or(io::ErrorKind::InvalidInput)?
        .copy_from_slice(&len.to_le_bytes());
    out.get_mut(8..end)
        .ok_or(io::ErrorKind::InvalidInput)?
        .copy_from_slice(target);
    Ok(out)
}
/// Rejects unknown versions, extensions, padding and escaped components.
pub fn decode(bytes: &[u8]) -> io::Result<&[u8]> {
    if bytes.len() != SIZE
        || bytes.get(..4) != Some(MAGIC.as_slice())
        || bytes.get(6..8) != Some([0; 2].as_slice())
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let length_bytes = bytes
        .get(4..6)
        .ok_or(io::ErrorKind::InvalidData)?
        .try_into()
        .map_err(|_| io::ErrorKind::InvalidData)?;
    let len = usize::from(u16::from_le_bytes(length_bytes));
    let end = 8usize.checked_add(len).ok_or(io::ErrorKind::InvalidData)?;
    if len == 0
        || len > 255
        || bytes
            .get(end..)
            .ok_or(io::ErrorKind::InvalidData)?
            .iter()
            .any(|v| *v != 0)
    {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let target = bytes.get(8..end).ok_or(io::ErrorKind::InvalidData)?;
    if encode(target)?.as_slice() != bytes {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(target)
}
/// Probe the exact held Stage. Unsupported legacy filesystems retain their
/// ordinary publication path. Once support is observed, every ARM failure is
/// terminal before rename; refusal can never downgrade to a generic rename.
pub(crate) fn arm(file: &File, target: &[u8]) -> io::Result<()> {
    let mut magic = [0u8; 4];
    // SAFETY: restricted fixed-size output points to four writable live bytes.
    let result = unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            libc::c_ulong::from(PROBE),
            magic.as_mut_ptr(),
        )
    };
    if result < 0 {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::ENOTTY | libc::ENOSYS)) {
            return Ok(());
        }
        return Err(error);
    }
    if result != 0 || magic != MAGIC {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let input = encode(target)?;
    // SAFETY: restricted command's encoded size is exactly SIZE live input bytes.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), libc::c_ulong::from(ARM), input.as_ptr()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_marker_codec_is_closed_and_lossless() {
        let input = encode(&[0xff, b'x']).unwrap();
        assert_eq!(decode(&input).unwrap(), &[0xff, b'x']);
        for index in [0, 4, 6, 263] {
            let mut changed = input;
            changed[index] = 99;
            assert!(decode(&changed).is_err());
        }
        for target in [b"..".as_slice(), b"a/b", b"", b"x\0"] {
            assert!(encode(target).is_err());
        }
    }
    #[test]
    fn marker_layout_and_component_limits_match_protocol() {
        let mut expected = [0u8; SIZE];
        expected[..8].copy_from_slice(b"TAS1\x02\x00\x00\x00");
        expected[8..10].copy_from_slice(&[0xff, b'x']);
        assert_eq!(encode(&[0xff, b'x']).unwrap(), expected);

        let target = [0xff; 255];
        let encoded = encode(&target).unwrap();
        assert_eq!(&encoded[..8], b"TAS1\xff\x00\x00\x00");
        assert_eq!(encoded[263], 0);
        assert_eq!(decode(&encoded).unwrap(), target);
        assert!(encode(&[b'x'; 256]).is_err());
        assert!(encode(b".").is_err());

        for length in [0u16, 256, u16::MAX] {
            let mut changed = encoded;
            changed[4..6].copy_from_slice(&length.to_le_bytes());
            assert_eq!(
                decode(&changed).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        for length in 0..SIZE {
            assert!(decode(&encoded[..length]).is_err());
        }
        let mut extended = encoded.to_vec();
        extended.push(0);
        assert!(decode(&extended).is_err());
    }

    #[test]
    fn ordinary_native_stage_unsupported_probe_preserves_existing_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("stage");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let result = arm(&file, b"document");
        result.unwrap();
    }
}
