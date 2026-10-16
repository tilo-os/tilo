//! Closed macOS 64-bit SDK security layout; no libc ACL or membership objects.
use std::{ffi::c_void, fs::File, io, os::fd::AsRawFd};
const RETURNED: u32 = 0x8000_0000;
const SECURITY: u32 = 0x0040_0000;
const VOL_CAPS: u32 = 0x0002_0000;
const EXTENDED: u32 = 0x400;
const MAGIC: u32 = 0x012c_c16d;
const MAX_ENTRIES: usize = 128;
const CAP_BYTES: usize = 4 + 20 + 32;
const ACL_BYTES: usize = 4 + 20 + 8 + 44 + MAX_ENTRIES * 24;
#[repr(C)]
struct Attributes {
    count: u16,
    reserved: u16,
    common: u32,
    volume: u32,
    directory: u32,
    file: u32,
    fork: u32,
}
unsafe extern "C" {
    fn fsetattrlist(
        fd: i32,
        attrs: *const Attributes,
        buffer: *const c_void,
        size: usize,
        options: u32,
    ) -> i32;
    fn fgetattrlist(
        fd: i32,
        attrs: *const Attributes,
        buffer: *mut c_void,
        size: usize,
        options: u32,
    ) -> i32;
}
fn query(file: &File, common: u32, volume: u32, output: &mut [u8]) -> io::Result<()> {
    let attrs = Attributes {
        count: 5,
        reserved: 0,
        common,
        volume,
        directory: 0,
        file: 0,
        fork: 0,
    };
    // SAFETY: live exact FD, initialized SDK structure and complete writable buffer.
    if unsafe {
        fgetattrlist(
            file.as_raw_fd(),
            &raw const attrs,
            output.as_mut_ptr().cast(),
            output.len(),
            0,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn word(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_ne_bytes(
        bytes
            .get(offset..offset.checked_add(4).ok_or_else(invalid)?)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
/// Offset of ACL entry `i`'s flags word within the attribute buffer.
fn entry_offset(i: usize) -> io::Result<usize> {
    i.checked_mul(24)
        .and_then(|n| n.checked_add(92))
        .ok_or_else(invalid)
}
fn capabilities(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() != CAP_BYTES
        || word(bytes, 0)? as usize != CAP_BYTES
        || word(bytes, 4)? != RETURNED
        || word(bytes, 8)? != VOL_CAPS
        || (12..24)
            .step_by(4)
            .any(|i| word(bytes, i).unwrap_or(u32::MAX) != 0)
    {
        return Err(invalid());
    }
    // Two four-u32 arrays, interfaces index1: capabilities at28, valid at44.
    if word(bytes, 28)? & EXTENDED == 0 || word(bytes, 44)? & EXTENDED == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    Ok(())
}
fn layout(bytes: &[u8]) -> io::Result<Option<(usize, u32)>> {
    let total = word(bytes, 0)? as usize;
    if total < 24 || total > bytes.len() {
        return Err(invalid());
    }
    if word(bytes, 4)? & !(RETURNED | SECURITY) != 0
        || word(bytes, 4)? & RETURNED == 0
        || (8..24)
            .step_by(4)
            .any(|i| word(bytes, i).unwrap_or(u32::MAX) != 0)
    {
        return Err(invalid());
    }
    // The local supported profile omits the ACL returned bit but leaves a
    // zeroed fixed slot. An affirmative volume capability check is mandatory.
    if word(bytes, 4)? & SECURITY == 0 {
        return if total == 32 && bytes.get(24..32) == Some(&[0u8; 8]) {
            Ok(None)
        } else {
            Err(invalid())
        };
    }
    if total < 32 || word(bytes, 24)? != 8 {
        return Err(invalid());
    }
    let length = word(bytes, 28)? as usize;
    if length < 44 || 32usize.checked_add(length) != Some(total) || word(bytes, 32)? != MAGIC {
        return Err(invalid());
    }
    let count = word(bytes, 68)?;
    let flags = word(bytes, 72)?;
    if flags & !0x0003_ffff != 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    if count == u32::MAX {
        return if length == 44 {
            Ok(Some((0, flags)))
        } else {
            Err(invalid())
        };
    }
    let count = count as usize;
    if count > MAX_ENTRIES || Some(length) != count.checked_mul(24).and_then(|n| n.checked_add(44))
    {
        return Err(invalid());
    }
    for i in 0..count {
        let flags = word(bytes, entry_offset(i)?)?;
        if !matches!(flags & 0xf, 1 | 2) || flags & !0x1ff != 0 {
            return Err(io::ErrorKind::Unsupported.into());
        }
    }
    Ok(Some((count, flags)))
}
fn security(bytes: &[u8]) -> io::Result<()> {
    if let Some((count, flags)) = layout(bytes)? {
        // Deferred inheritance may change access on publication.
        if flags & (1 << 16) != 0 {
            return Err(io::ErrorKind::Unsupported.into());
        }
        for i in 0..count {
            if word(bytes, entry_offset(i)?)? & 0xf != 2 {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
    }
    Ok(())
}
pub(crate) fn validate(file: &File) -> io::Result<()> {
    let mut caps = [0u8; CAP_BYTES];
    query(file, RETURNED, RETURNED | VOL_CAPS, &mut caps)?;
    capabilities(&caps)?;
    let mut acl = [0u8; ACL_BYTES];
    query(file, RETURNED | SECURITY, 0, &mut acl)?;
    security(&acl)
}

pub(crate) fn validate_private(file: &File, metadata: &std::fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !crate::native::private::private_to_me(&metadata) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    validate(file)
}

fn empty(file: &File) -> io::Result<()> {
    let attrs = Attributes {
        count: 5,
        reserved: 0,
        common: RETURNED,
        volume: 0,
        directory: 2,
        file: 0,
        fork: 0,
    };
    let mut result = [0u8; 28];
    // SAFETY: exact live directory FD and complete fixed SDK output buffer.
    if unsafe {
        fgetattrlist(
            file.as_raw_fd(),
            &raw const attrs,
            result.as_mut_ptr().cast(),
            result.len(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if word(&result, 0)? != 28 || word(&result, 12)? & 2 == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    if word(&result, 24)? != 0 {
        return Err(io::ErrorKind::DirectoryNotEmpty.into());
    }
    Ok(())
}
/// Only the private exclusive-mkdir custody may call this; never an adopted FD.
pub(crate) fn normalize_fresh_empty(file: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || !crate::native::private::owned_by_me(&metadata) {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut caps = [0u8; CAP_BYTES];
    query(file, RETURNED, RETURNED | VOL_CAPS, &mut caps)?;
    capabilities(&caps)?;
    empty(file)?;
    let mut current = [0u8; ACL_BYTES];
    query(file, RETURNED | SECURITY, 0, &mut current)?;
    let input = normalized_input(&current)?;
    let attrs = Attributes {
        count: 5,
        reserved: 0,
        common: SECURITY,
        volume: 0,
        directory: 0,
        file: 0,
        fork: 0,
    };
    // SAFETY: retained exclusively created empty child and exact checked layout.
    if unsafe {
        fsetattrlist(
            file.as_raw_fd(),
            &raw const attrs,
            input.as_ptr().cast(),
            input.len(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    rustix::fs::fchmod(file, rustix::fs::Mode::from_raw_mode(0o700))?;
    validate_private(file, &file.metadata()?)?;
    empty(file)
}
#[cfg(test)]
mod tests;

fn normalized_input(current: &[u8]) -> io::Result<[u8; 52]> {
    let private_flags = layout(current)?.map_or(0, |(_, flags)| flags & 0xffff);
    let mut input = [0u8; 52];
    for (offset, value) in [
        (0_usize, 8u32),
        (4, 44),
        (8, MAGIC),
        (44, 0),
        (48, 0x0002_0000 | private_flags),
    ] {
        input
            .get_mut(offset..offset.saturating_add(4))
            .ok_or_else(invalid)?
            .copy_from_slice(&value.to_ne_bytes());
    }
    Ok(input)
}
