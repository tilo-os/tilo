//! Fixed restricted ioctls on the genuine held Stage. These bytes select a
//! publication purpose; only the receiving filesystem's real grants authorize it.
use std::{fs::File, io, os::fd::AsRawFd};
pub const SIZE: usize = 264;
pub const PROBE: u32 = 0x8004_7451;
pub const ARM: u32 = 0x4108_7452;
pub const MAGIC: [u8; 4] = *b"TAS1";

/// A closed raw destination component; no record ID or authority is carried.
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
/// An ioctl request code as the C library types it: glibc's `unsigned long`, musl's `int`, which
/// carries the same 32 bits (both codes have the top bit of an `int` set).
#[cfg(not(target_env = "musl"))]
fn request(code: u32) -> libc::c_ulong {
    libc::c_ulong::from(code)
}
#[cfg(target_env = "musl")]
fn request(code: u32) -> libc::c_int {
    libc::c_int::from_ne_bytes(code.to_ne_bytes())
}
/// Probe the exact held Stage. Unsupported legacy filesystems retain their
/// ordinary publication path. Once support is observed, every ARM failure is
/// terminal before rename; refusal can never downgrade to a generic rename.
pub(crate) fn arm(file: &File, target: &[u8]) -> io::Result<()> {
    let mut magic = [0u8; 4];
    // SAFETY: restricted fixed-size output points to four writable live bytes.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request(PROBE), magic.as_mut_ptr()) };
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
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request(ARM), input.as_ptr()) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    if result != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(())
}
