//! Bounded, read-only observations of an active device-mapper table.
//!
//! Status mode never requests table parameters (in particular crypt keys).
//! Only linear and crypt mappings are supported; their dependency graph must
//! still be checked by the caller down to independently witnessed block media.

use std::{fs::File, io, os::fd::AsRawFd};

const BUFFER_BYTES: usize = 64 * 1024;
const MAX_TARGETS: usize = 128;
const MAX_DEPENDENCIES: usize = 128;
const HEADER_BYTES: usize = 312;
const SPEC_BYTES: usize = 40;
const ACTIVE: u32 = 1 << 5;
const REJECT_FLAGS: u32 =
    (1 << 0) | (1 << 1) | (1 << 6) | (1 << 8) | (1 << 12) | (1 << 17) | (1 << 18);

// Linux dm-ioctl.h: _IOWR(0xfd, command, struct dm_ioctl).
const TABLE_DEPS: libc::c_ulong = 0xc138_fd0b;
const TABLE_STATUS: libc::c_ulong = 0xc138_fd0c;

#[repr(C)]
#[derive(Clone, Copy)]
struct Header {
    version: [u32; 3],
    data_size: u32,
    data_start: u32,
    target_count: u32,
    open_count: i32,
    flags: u32,
    event_nr: u32,
    padding: u32,
    dev: u64,
    name: [u8; 128],
    uuid: [u8; 129],
    data: [u8; 7],
}

const _: () = assert!(std::mem::size_of::<Header>() == HEADER_BYTES);

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct DmState {
    pub(super) dependencies: Vec<u64>,
    uuid: Vec<u8>,
    event_nr: u32,
    targets: Vec<Target>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
struct Target {
    kind: Vec<u8>,
    sector_start: u64,
    length: u64,
}

pub(super) fn inspect(control: &File, device: u64) -> io::Result<DmState> {
    let encoded = encode_device(device)?;
    let first = status(control, encoded)?;
    let (dependencies, header) = dependencies(control, encoded)?;
    if header.event_nr != first.event_nr
        || terminated(&header.uuid)? != first.uuid
        || header.target_count as usize != first.targets.len()
    {
        return Err(refused());
    }
    let last = status(control, encoded)?;
    if first != last {
        return Err(refused());
    }
    Ok(DmState {
        dependencies,
        ..first
    })
}

fn status(control: &File, device: u64) -> io::Result<DmState> {
    let (buffer, header) = query(control, device, TABLE_STATUS)?;
    parse_status(&buffer, &header)
}

fn parse_status(buffer: &[u64], header: &Header) -> io::Result<DmState> {
    let bytes = bytes(buffer);
    let start = header.data_start as usize;
    let end = header.data_size as usize;
    let count = header.target_count as usize;
    if count == 0 || count > MAX_TARGETS || start > end || end > bytes.len() {
        return Err(refused());
    }
    let aligned_end = end.checked_add(7).ok_or_else(refused)? & !7;
    let mut targets = Vec::with_capacity(count);
    let mut relative = 0_usize;
    let mut expected_sector = 0_u64;
    for index in 0..count {
        let offset = start.checked_add(relative).ok_or_else(refused)?;
        let spec_end = offset.checked_add(SPEC_BYTES).ok_or_else(refused)?;
        if !relative.is_multiple_of(8) || spec_end > end {
            return Err(refused());
        }
        let spec = &bytes[offset..spec_end];
        let sector_start = u64::from_ne_bytes(spec[0..8].try_into().map_err(|_| refused())?);
        let length = u64::from_ne_bytes(spec[8..16].try_into().map_err(|_| refused())?);
        let target_status = i32::from_ne_bytes(spec[16..20].try_into().map_err(|_| refused())?);
        let next = u32::from_ne_bytes(spec[20..24].try_into().map_err(|_| refused())?) as usize;
        let next_offset = start.checked_add(next).ok_or_else(refused)?;
        // retrieve_status reports the final used byte before alignment in
        // data_size, while spec.next includes the final alignment padding.
        let limit = if index + 1 == count { aligned_end } else { end };
        let kind = terminated(&spec[24..40])?;
        if !matches!(kind, b"linear" | b"crypt")
            || target_status != 0
            || sector_start != expected_sector
            || length == 0
            || next < relative.checked_add(SPEC_BYTES + 1).ok_or_else(refused)?
            || !next.is_multiple_of(8)
            || next_offset > limit
            || next_offset > bytes.len()
        {
            return Err(refused());
        }
        // Validate the bounded status string but never retain or expose it.
        terminated(&bytes[spec_end..next_offset.min(end)])?;
        expected_sector = sector_start.checked_add(length).ok_or_else(refused)?;
        targets.push(Target {
            kind: kind.to_vec(),
            sector_start,
            length,
        });
        relative = next;
    }
    let uuid = terminated(&header.uuid)?;
    if uuid.is_empty() {
        return Err(refused());
    }
    Ok(DmState {
        dependencies: Vec::new(),
        uuid: uuid.to_vec(),
        event_nr: header.event_nr,
        targets,
    })
}

fn dependencies(control: &File, device: u64) -> io::Result<(Vec<u64>, Header)> {
    let (buffer, header) = query(control, device, TABLE_DEPS)?;
    let bytes = bytes(&buffer);
    let start = header.data_start as usize;
    let end = header.data_size as usize;
    if start.checked_add(8).ok_or_else(refused)? > end {
        return Err(refused());
    }
    let count =
        u32::from_ne_bytes(bytes[start..start + 4].try_into().map_err(|_| refused())?) as usize;
    if count == 0
        || count > MAX_DEPENDENCIES
        || start.checked_add(8 + count * 8).ok_or_else(refused)? > end
    {
        return Err(refused());
    }
    let mut dependencies = Vec::with_capacity(count);
    for index in 0..count {
        let offset = start + 8 + index * 8;
        let encoded = u64::from_ne_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .map_err(|_| refused())?,
        );
        let decoded = decode_device(encoded)?;
        if encoded == device || dependencies.contains(&decoded) {
            return Err(refused());
        }
        dependencies.push(decoded);
    }
    dependencies.sort_unstable();
    Ok((dependencies, header))
}

fn query(control: &File, device: u64, request: libc::c_ulong) -> io::Result<(Vec<u64>, Header)> {
    // u64 storage gives the UAPI header and each target its required alignment.
    let mut buffer = vec![0_u64; BUFFER_BYTES / 8];
    let header = Header {
        version: [4, 0, 0],
        data_size: BUFFER_BYTES as u32,
        data_start: HEADER_BYTES as u32,
        target_count: 0,
        open_count: 0,
        flags: 0, // active status, never DM_STATUS_TABLE_FLAG
        event_nr: 0,
        padding: 0,
        dev: device,
        name: [0; 128],
        uuid: [0; 129],
        data: [0; 7],
    };
    // SAFETY: the bounded allocation is aligned and larger than Header.
    unsafe { buffer.as_mut_ptr().cast::<Header>().write(header) };
    // SAFETY: these read-only DM commands receive the correctly sized buffer.
    if unsafe { libc::ioctl(control.as_raw_fd(), request, buffer.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Header remains inside the aligned live allocation.
    let header = unsafe { buffer.as_ptr().cast::<Header>().read() };
    if header.version[0] != 4
        || header.dev != device
        || header.flags & ACTIVE == 0
        || header.flags & REJECT_FLAGS != 0
        || header.data_size as usize > BUFFER_BYTES
        || (header.data_start as usize) < HEADER_BYTES
        || header.data_start > header.data_size
        || header.data_start % 8 != 0
    {
        return Err(refused());
    }
    Ok((buffer, header))
}

fn bytes(buffer: &[u64]) -> &[u8] {
    // SAFETY: bytes have alignment one and cover exactly the initialized buffer.
    unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast(), std::mem::size_of_val(buffer)) }
}

fn terminated(bytes: &[u8]) -> io::Result<&[u8]> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(refused)?;
    Ok(&bytes[..end])
}

fn encode_device(device: u64) -> io::Result<u64> {
    let major = libc::major(device as libc::dev_t);
    let minor = libc::minor(device as libc::dev_t);
    if major > 0xfff || minor > 0xfffff || major == 0 {
        return Err(refused());
    }
    Ok(u64::from(
        (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12),
    ))
}

fn decode_device(encoded: u64) -> io::Result<u64> {
    if encoded > u64::from(u32::MAX) {
        return Err(refused());
    }
    let value = encoded as u32;
    let major = (value >> 8) & 0xfff;
    let minor = (value & 0xff) | ((value >> 12) & 0xfff00);
    if major == 0 {
        return Err(refused());
    }
    Ok(libc::makedev(major, minor) as u64)
}

fn refused() -> io::Error {
    io::Error::other("inventory scratch device-mapper backing could not be verified")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_dependencies_use_kernel_encoding_not_glibc_minor_layout() {
        let device = libc::makedev(253, 0x12345) as u64;
        let encoded = encode_device(device).expect("supported block device");
        assert_eq!(encoded, 0x1230fd45);
        assert_eq!(decode_device(encoded).expect("kernel dependency"), device);
        assert!(decode_device(1_u64 << 32).is_err());
        assert!(decode_device(0).is_err());
    }

    #[test]
    fn status_strings_must_terminate_within_the_bounded_record() {
        assert_eq!(terminated(b"crypt\0ignored").expect("terminated"), b"crypt");
        assert!(terminated(b"crypt").is_err());
        assert!(terminated(&[]).is_err());
    }

    #[test]
    fn final_status_record_can_end_before_its_aligned_next_offset() {
        let mut buffer = vec![0_u64; BUFFER_BYTES / 8];
        // SAFETY: Header contains only integer values and byte arrays.
        let mut header: Header = unsafe { std::mem::zeroed() };
        header.data_start = HEADER_BYTES as u32;
        header.data_size = (HEADER_BYTES + SPEC_BYTES + 1) as u32;
        header.target_count = 1;
        header.uuid[..4].copy_from_slice(b"test");
        // SAFETY: byte view covers the initialized u64 allocation exactly.
        let output = unsafe {
            std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), BUFFER_BYTES)
        };
        let spec = &mut output[HEADER_BYTES..HEADER_BYTES + SPEC_BYTES];
        spec[8..16].copy_from_slice(&1_u64.to_ne_bytes());
        spec[20..24].copy_from_slice(&48_u32.to_ne_bytes());
        spec[24..29].copy_from_slice(b"crypt");
        let state = parse_status(&buffer, &header).expect("unaligned final empty status");
        assert_eq!(state.targets.len(), 1);
        assert_eq!(state.targets[0].length, 1);
        // The NUL must belong to the reported used bytes, not padding.
        header.data_size -= 1;
        assert!(parse_status(&buffer, &header).is_err());
        // The same gap cannot precede an intermediate target.
        header.data_size += 1;
        header.target_count = 2;
        assert!(parse_status(&buffer, &header).is_err());
    }
}
