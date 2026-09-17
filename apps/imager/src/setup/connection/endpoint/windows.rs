//! Windows handle and volume evidence; no drive-letter-only identity, reparse
//! traversal, hydration, encryption unlock or unproved transaction enablement.
use super::super::model::{EndpointIdentity, NamePolicy};
use super::{Capabilities, Capability, EndpointError, NativeEvidence, digest};
use std::fs::File;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::ptr;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_FILES, GetLastError};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_BASIC_INFO, FILE_CASE_SENSITIVE_INFO, FILE_ID_INFO, FileBasicInfo, FileCaseSensitiveInfo,
    FileIdInfo, FileStreamInfo, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose,
    GetFileInformationByHandleEx, GetFinalPathNameByHandleW, GetVolumeInformationByHandleW,
    GetVolumeInformationW, VOLUME_NAME_GUID,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

fn info<T>(file: &File, class: i32) -> Result<T, EndpointError> {
    let mut result = MaybeUninit::<T>::uninit();
    // SAFETY: private callers pair the SDK info class with its exact SDK struct.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            result.as_mut_ptr().cast(),
            size_of::<T>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful SDK call initialized the complete requested struct.
    Ok(unsafe { result.assume_init() })
}
fn id(file: &File) -> Result<FILE_ID_INFO, EndpointError> {
    info(file, FileIdInfo)
}
fn volume_path(file: &File) -> Result<Vec<u16>, EndpointError> {
    let mut path = vec![0_u16; 32768];
    // SAFETY: live handle and writable UTF-16 buffer bounded by its length.
    let count = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            VOLUME_NAME_GUID,
        )
    } as usize;
    if count == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if count >= path.len() {
        return Err(EndpointError::LimitExceeded);
    }
    path.truncate(count);
    // Native volume GUID form: \\?\Volume{GUID}\... . UNC/network paths lack
    // local media/dirty-state evidence and are refused.
    let prefix: Vec<_> = "\\\\?\\Volume{".encode_utf16().collect();
    if !path.starts_with(&prefix) {
        return Err(EndpointError::Unsupported);
    }
    let end = path
        .iter()
        .position(|u| *u == u16::from(b'}'))
        .ok_or(EndpointError::Unsupported)?;
    let mut volume = path.get(..=end).ok_or(EndpointError::Unsupported)?.to_vec();
    volume.push(u16::from(b'\\'));
    Ok(volume)
}
fn flags(file: &File) -> Result<(u32, u32), EndpointError> {
    let mut serial = 0;
    let mut flags = 0;
    // SAFETY: nullable unused outputs, correctly typed writable scalar outputs.
    if unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            ptr::null_mut(),
            0,
            &raw mut serial,
            ptr::null_mut(),
            &raw mut flags,
            ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // FILE_SUPPORTS_OPEN_BY_FILE_ID is native semantic evidence, not a name.
    if flags & 0x0100_0000 == 0 {
        return Err(EndpointError::Unsupported);
    }
    Ok((serial, flags))
}
fn volume_device(volume: &[u16]) -> Result<File, EndpointError> {
    let mut path = volume.to_vec();
    if path.pop() != Some(u16::from(b'\\')) {
        return Err(EndpointError::Unsupported);
    }
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(std::ffi::OsString::from_wide(&path))
        .map_err(Into::into)
}
fn word(bytes: &[u8], offset: usize) -> Result<u32, EndpointError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(EndpointError::Unsupported)?
            .try_into()
            .map_err(|_| EndpointError::Unsupported)?,
    ))
}
fn device_identity(volume: &[u16]) -> Result<Vec<u8>, EndpointError> {
    use windows_sys::Win32::System::Ioctl::{
        IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
        StorageDeviceIdProperty, VOLUME_DISK_EXTENTS,
    };
    let volume = volume_device(volume)?;
    let mut extents = VOLUME_DISK_EXTENTS::default();
    let mut returned = 0;
    // SAFETY: exact SDK fixed one-extent structure and read-only volume ioctl.
    if unsafe {
        DeviceIoControl(
            volume.as_raw_handle(),
            windows_sys::Win32::Storage::FileSystem::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            ptr::null(),
            0,
            (&raw mut extents).cast(),
            size_of::<VOLUME_DISK_EXTENTS>() as u32,
            &raw mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if extents.NumberOfDiskExtents != 1 || returned as usize != size_of::<VOLUME_DISK_EXTENTS>() {
        return Err(EndpointError::Unsupported);
    }
    let extent = extents.Extents.first().ok_or(EndpointError::Unsupported)?;
    let disk = std::fs::OpenOptions::new()
        .access_mode(0)
        .share_mode(1 | 2)
        .open(format!("\\\\.\\PhysicalDrive{}", extent.DiskNumber))?;
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceIdProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut bytes = [0_u8; 4096];
    // SAFETY: SDK query struct and bounded descriptor byte buffer. QUERY only.
    if unsafe {
        DeviceIoControl(
            disk.as_raw_handle(),
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&raw const query).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            bytes.as_mut_ptr().cast(),
            bytes.len() as u32,
            &raw mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let bytes = bytes
        .get(..returned as usize)
        .ok_or(EndpointError::Unsupported)?;
    if word(bytes, 4)? as usize != bytes.len() {
        return Err(EndpointError::Unsupported);
    }
    let count = word(bytes, 8)?;
    if count == 0 || count > 64 {
        return Err(EndpointError::Unsupported);
    }
    let mut offset = 12;
    let mut ids = std::collections::BTreeSet::new();
    for index in 0..count {
        let code = word(bytes, offset)?;
        let kind = word(bytes, offset + 4)?;
        let sizes = word(bytes, offset + 8)?;
        let length = (sizes & 0xffff) as usize;
        let next = (sizes >> 16) as usize;
        let association = word(bytes, offset + 12)?;
        let value = bytes
            .get(offset + 16..offset + 16 + length)
            .ok_or(EndpointError::Unsupported)?;
        // Device-associated EUI-64, NAA/FCPH or SCSI name identifiers. Port and
        // target IDs, mutable friendly names and OS-assigned disk numbers are
        // not persistent hardware identities.
        if association == 0
            && matches!(kind, 2 | 3 | 8)
            && matches!(code, 1..=3)
            && !value.is_empty()
            && value.iter().any(|b| *b != 0)
        {
            let mut id = kind.to_le_bytes().to_vec();
            id.extend_from_slice(value);
            ids.insert(id);
        }
        if index + 1 < count && next < 16 + length {
            return Err(EndpointError::Unsupported);
        }
        offset += next;
    }
    if ids.is_empty() {
        return Err(EndpointError::Unsupported);
    }
    let mut identity = extent.StartingOffset.to_le_bytes().to_vec();
    for id in ids {
        identity.extend_from_slice(&(id.len() as u32).to_le_bytes());
        identity.extend_from_slice(&id);
    }
    Ok(identity)
}
fn clean_volume(volume: &[u16]) -> Result<(), EndpointError> {
    let mut device = volume.to_vec();
    if device.pop() != Some(u16::from(b'\\')) {
        return Err(EndpointError::Unsupported);
    }
    let path = std::ffi::OsString::from_wide(&device);
    // Never ask for write, unlock or change the volume state. Sharing permits
    // the current mount; inability to query dirty state is an explicit refusal.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(path)?;
    let mut dirty = 0_u32;
    let mut returned = 0_u32;
    // SAFETY: FSCTL_IS_VOLUME_DIRTY has no input and one DWORD output.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x0009_0078,
            ptr::null(),
            0,
            (&raw mut dirty).cast(),
            4,
            &raw mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if returned != 4 || dirty != 0 {
        return Err(EndpointError::UnsafeVolume);
    }
    // FSCTL_GET_NTFS_VOLUME_DATA proves the native NTFS backend and its file-ID
    // semantics. ReFS/FAT cannot be admitted by a matching label or flag alone.
    let mut ntfs = [0_u64; 12];
    // SAFETY: exact 96-byte NTFS_VOLUME_DATA_BUFFER; read-only control query.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x0009_0064,
            ptr::null(),
            0,
            ntfs.as_mut_ptr().cast(),
            96,
            &raw mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if returned < 96 {
        return Err(EndpointError::Unsupported);
    }
    Ok(())
}
struct VolumeEnumeration(windows_sys::Win32::Foundation::HANDLE);
impl Drop for VolumeEnumeration {
    fn drop(&mut self) {
        // SAFETY: owns the successful FindFirstVolumeW search handle.
        unsafe {
            FindVolumeClose(self.0);
        }
    }
}
fn check_duplicates(selected: &[u16], expected_serial: u32) -> Result<(), EndpointError> {
    let mut name = [0_u16; 128];
    // SAFETY: fixed writable buffer with matching length.
    let handle = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    let volumes = VolumeEnumeration(handle);
    let mut count = 0;
    loop {
        count += 1;
        if count > 128 {
            return Err(EndpointError::LimitExceeded);
        }
        let end = name
            .iter()
            .position(|c| *c == 0)
            .ok_or(EndpointError::Unsupported)?;
        if name.get(..end) != Some(selected) {
            let mut serial = 0;
            // SAFETY: terminated volume path and writable serial output.
            let success = unsafe {
                GetVolumeInformationW(
                    name.as_ptr(),
                    ptr::null_mut(),
                    0,
                    &raw mut serial,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                )
            };
            if success != 0 && serial == expected_serial {
                return Err(EndpointError::AmbiguousIdentity);
            }
            if success == 0 {
                let error = std::io::Error::last_os_error();
                // An inaccessible/locked volume could contain a cloned identity.
                // Empty removable slots (NOT_READY) do not name accessible media.
                if error.raw_os_error() != Some(21) {
                    return Err(EndpointError::AmbiguousIdentity);
                }
            }
        }
        // SAFETY: live search handle; correctly sized writable output.
        if unsafe { FindNextVolumeW(volumes.0, name.as_mut_ptr(), name.len() as u32) } == 0 {
            // SAFETY: reading the calling thread's last Win32 error.
            if unsafe { GetLastError() } != ERROR_NO_MORE_FILES {
                return Err(std::io::Error::last_os_error().into());
            }
            break;
        }
    }
    Ok(())
}
pub(super) fn inspect(file: &File) -> Result<NativeEvidence, EndpointError> {
    let identity = id(file)?;
    let volume = volume_path(file)?;
    let (serial, flags) = flags(file)?;
    clean_volume(&volume)?;
    check_duplicates(&volume, serial)?;
    let mut stable_volume = device_identity(&volume)?;
    for unit in &volume {
        stable_volume.extend_from_slice(&unit.to_le_bytes());
    }
    stable_volume.extend_from_slice(&identity.VolumeSerialNumber.to_le_bytes());
    let write = if flags & 0x0008_0000 != 0 {
        EndpointError::ReadOnly
    } else {
        EndpointError::Unverified
    };
    // Case-sensitive directories are exact; default Windows semantics require
    // native canonical-key evidence for every candidate, supplied by a future
    // mapping adapter. An empty map makes the pure reconciler keep them pending.
    let case: FILE_CASE_SENSITIVE_INFO = info(file, FileCaseSensitiveInfo)?;
    let names = if case.Flags & 1 != 0 {
        NamePolicy::Exact
    } else {
        NamePolicy::Canonical(std::collections::BTreeMap::new())
    };
    Ok(NativeEvidence {
        identity: EndpointIdentity {
            volume: digest(&stable_volume),
            root: digest(&identity.FileId.Identifier),
        },
        mount: stable_volume,
        capabilities: Capabilities {
            safe_read: Capability::Available,
            safe_write: Capability::Refused(write),
            durable_transactions: Capability::Refused(EndpointError::Unverified),
        },
        names,
    })
}
pub(super) fn same_mount(a: &File, b: &File) -> Result<(), EndpointError> {
    if id(a)?.VolumeSerialNumber != id(b)?.VolumeSerialNumber || volume_path(a)? != volume_path(b)?
    {
        return Err(EndpointError::IdentityChanged);
    }
    Ok(())
}
pub(super) fn check_metadata(file: &File, directory: bool) -> Result<bool, EndpointError> {
    let basic: FILE_BASIC_INFO = info(file, FileBasicInfo)?;
    if basic.FileAttributes & (0x400 | 0x1000 | 0x4000 | 0x40000 | 0x0040_0000) != 0 {
        return Err(EndpointError::Unsupported);
    }
    if directory {
        // Mixed case-sensitive descendants invalidate whole-endpoint key policy.
        let case: FILE_CASE_SENSITIVE_INFO = info(file, FileCaseSensitiveInfo)?;
        if case.Flags != 0 {
            return Err(EndpointError::Unsupported);
        }
    } else {
        // FILE_STREAM_INFO list: next DWORD, name-length DWORD, two LARGE_INTEGERs,
        // UTF-16 name. A second/nondefault stream cannot be represented by model.
        let mut stream = [0_u64; 128];
        // SAFETY: aligned 1024-byte output buffer, native FileStreamInfo class.
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStreamInfo,
                stream.as_mut_ptr().cast(),
                1024,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: byte view of initialized integer storage, exactly its length.
        let bytes = unsafe { std::slice::from_raw_parts(stream.as_ptr().cast::<u8>(), 1024) };
        let header = bytes.get(..8).ok_or(EndpointError::Unsupported)?;
        let expected = "::$DATA"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        if header.get(..4) != Some(&[0, 0, 0, 0])
            || header.get(4..8) != Some(&14_u32.to_le_bytes())
            || bytes.get(24..38) != Some(expected.as_slice())
        {
            return Err(EndpointError::Unsupported);
        }
    }
    Ok(false)
}

pub(super) fn change_token(file: &File) -> Result<Vec<u8>, EndpointError> {
    let basic: FILE_BASIC_INFO = info(file, FileBasicInfo)?;
    Ok(basic.ChangeTime.to_le_bytes().to_vec())
}
