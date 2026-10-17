//! Thin Win32 wrappers for the Windows staging adapter. Every decision is
//! made by the pure modules; this file only moves bytes across the FFI edge.

use std::ffi::OsString;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr;
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ENVVAR_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
    ERROR_NO_MORE_FILES, ERROR_NOT_ALL_ASSIGNED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE, LUID,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_BEGIN, FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH,
    FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FindFirstVolumeW,
    FindNextVolumeW, FindVolumeClose, FlushFileBuffers, GetDiskFreeSpaceExW,
    GetVolumePathNamesForVolumeNameW, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING,
    ReadFile, SetFilePointerEx, WriteFile,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DISK_EXTENT, DISK_GEOMETRY, DISK_GEOMETRY_EX, DRIVE_LAYOUT_INFORMATION_EX,
    DRIVE_LAYOUT_INFORMATION_GPT, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME,
    IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, PARTITION_INFORMATION_EX,
    PARTITION_INFORMATION_GPT, VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::System::Memory::{
    MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAlloc, VirtualFree,
};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
use windows_sys::Win32::System::SystemInformation::{
    FIRMWARE_TYPE, FirmwareTypeUefi, GetFirmwareType, GetSystemWindowsDirectoryW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::System::WindowsProgramming::{
    GetFirmwareEnvironmentVariableExW, SetFirmwareEnvironmentVariableExW,
};

use super::super::platform::Disk;
use super::facts::PowerStatus;
use super::layout::{self, VolumeExtent};
use super::scripts::{self, DriveLetter};

// The pure decoders in `layout` hard-code the 64-bit structure layout; prove
// it against the SDK definitions for whatever target this compiles for.
const _: () = {
    assert!(offset_of!(DRIVE_LAYOUT_INFORMATION_EX, PartitionEntry) == layout::LAYOUT_HEADER_BYTES);
    assert!(
        offset_of!(DRIVE_LAYOUT_INFORMATION_EX, Anonymous)
            + offset_of!(DRIVE_LAYOUT_INFORMATION_GPT, DiskId)
            == layout::LAYOUT_GPT_DISK_ID
    );
    assert!(
        offset_of!(DRIVE_LAYOUT_INFORMATION_EX, Anonymous)
            + offset_of!(DRIVE_LAYOUT_INFORMATION_GPT, StartingUsableOffset)
            == layout::LAYOUT_GPT_STARTING_USABLE
    );
    assert!(
        offset_of!(DRIVE_LAYOUT_INFORMATION_EX, Anonymous)
            + offset_of!(DRIVE_LAYOUT_INFORMATION_GPT, UsableLength)
            == layout::LAYOUT_GPT_USABLE_LENGTH
    );
    assert!(size_of::<PARTITION_INFORMATION_EX>() == layout::ENTRY_BYTES);
    assert!(offset_of!(PARTITION_INFORMATION_EX, PartitionStyle) == layout::ENTRY_STYLE);
    assert!(offset_of!(PARTITION_INFORMATION_EX, StartingOffset) == layout::ENTRY_OFFSET);
    assert!(offset_of!(PARTITION_INFORMATION_EX, PartitionLength) == layout::ENTRY_LENGTH);
    assert!(offset_of!(PARTITION_INFORMATION_EX, PartitionNumber) == layout::ENTRY_NUMBER);
    assert!(
        offset_of!(PARTITION_INFORMATION_EX, Anonymous)
            + offset_of!(PARTITION_INFORMATION_GPT, PartitionType)
            == layout::ENTRY_GPT_TYPE
    );
    assert!(
        offset_of!(PARTITION_INFORMATION_EX, Anonymous)
            + offset_of!(PARTITION_INFORMATION_GPT, PartitionId)
            == layout::ENTRY_GPT_ID
    );
    assert!(
        offset_of!(PARTITION_INFORMATION_EX, Anonymous)
            + offset_of!(PARTITION_INFORMATION_GPT, Name)
            == layout::ENTRY_GPT_NAME
    );
    assert!(offset_of!(DISK_GEOMETRY, BytesPerSector) == layout::GEOMETRY_BYTES_PER_SECTOR);
    assert!(offset_of!(DISK_GEOMETRY_EX, DiskSize) == layout::GEOMETRY_DISK_SIZE);
    assert!(offset_of!(VOLUME_DISK_EXTENTS, Extents) == layout::EXTENTS_FIRST);
    assert!(size_of::<DISK_EXTENT>() == layout::EXTENT_BYTES);
    assert!(offset_of!(DISK_EXTENT, DiskNumber) == layout::EXTENT_DISK_NUMBER);
    assert!(offset_of!(DISK_EXTENT, StartingOffset) == layout::EXTENT_OFFSET);
    assert!(offset_of!(DISK_EXTENT, ExtentLength) == layout::EXTENT_LENGTH);
};

/// Relative to the Windows directory; never resolved through `PATH`.
pub const POWERSHELL: &str = r"System32\WindowsPowerShell\v1.0\powershell.exe";
pub const SHUTDOWN: &str = r"System32\shutdown.exe";

/// `EFI_GLOBAL_VARIABLE` in the braced form the Win32 API takes.
const EFI_GLOBAL_GUID: &str = "{8BE4DF61-93CA-11D2-AA0D-00E098032B8C}";
/// `NON_VOLATILE | BOOTSERVICE_ACCESS | RUNTIME_ACCESS`.
pub const EFI_ATTRIBUTES: u32 = 0x07;
const MAX_EFI_VAR_BYTES: usize = 64 * 1024;
const MAX_IOCTL_BYTES: usize = 1 << 20;
const PS_OUTPUT_LIMIT: usize = 1 << 20;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(buf.get(..end).unwrap_or_default())
        .to_string_lossy()
        .into_owned()
}

/// An owned kernel handle.
#[derive(Debug)]
pub struct Handle(HANDLE);

impl Handle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: owns a handle returned by CreateFileW/OpenProcessToken.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// SAFETY: kernel handles are process-wide and may be used from any thread.
unsafe impl Send for Handle {}

/// Open a device or volume path. `access` 0 permits metadata IOCTLs without
/// elevation.
pub fn open(path: &str, access: u32, flags: FILE_FLAGS_AND_ATTRIBUTES) -> io::Result<Handle> {
    let path = wide(path);
    // SAFETY: nul-terminated path; null security attributes and template.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            flags,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(handle))
}

pub fn physical_drive_path(number: u32) -> String {
    format!(r"\\.\PhysicalDrive{number}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveAccess {
    /// No data access: geometry and layout IOCTLs only; needs no elevation.
    Metadata,
    /// Unbuffered raw reads (administrators only).
    Read,
    /// Unbuffered, write-through raw reads and writes (administrators only).
    ReadWrite,
}

pub fn open_physical_drive(number: u32, access: DriveAccess) -> io::Result<Handle> {
    let path = physical_drive_path(number);
    match access {
        DriveAccess::Metadata => open(&path, 0, 0),
        DriveAccess::Read => open(&path, GENERIC_READ, FILE_FLAG_NO_BUFFERING),
        DriveAccess::ReadWrite => open(
            &path,
            GENERIC_READ | GENERIC_WRITE,
            FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH,
        ),
    }
}

/// Run an output-only IOCTL, growing the buffer until the driver fits.
pub fn ioctl_read(handle: &Handle, code: u32, initial: usize) -> io::Result<Vec<u8>> {
    let mut words = initial.div_ceil(8).max(8);
    loop {
        // u64 storage keeps the output 8-byte aligned for the driver.
        let mut buf = vec![0u64; words];
        let bytes = words
            .checked_mul(8)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| io::Error::other("IOCTL buffer overflow"))?;
        let mut returned = 0u32;
        // SAFETY: `buf` is writable for `bytes`; no input buffer; synchronous.
        let ok = unsafe {
            DeviceIoControl(
                handle.raw(),
                code,
                ptr::null(),
                0,
                buf.as_mut_ptr().cast(),
                bytes,
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        if ok != 0 {
            let len = usize::try_from(returned).map_err(|_| io::Error::other("IOCTL size"))?;
            let out: Vec<u8> = buf.iter().flat_map(|w| w.to_ne_bytes()).take(len).collect();
            return Ok(out);
        }
        // SAFETY: reads this thread's last-error value.
        let error = unsafe { GetLastError() };
        let grown = words.saturating_mul(2);
        if (error == ERROR_INSUFFICIENT_BUFFER || error == ERROR_MORE_DATA)
            && grown.saturating_mul(8) <= MAX_IOCTL_BYTES
        {
            words = grown;
            continue;
        }
        return Err(io::Error::from_raw_os_error(
            i32::try_from(error).unwrap_or(i32::MAX),
        ));
    }
}

/// Send a no-buffer control code (lock, dismount).
fn ioctl_plain(handle: &Handle, code: u32) -> io::Result<()> {
    let mut returned = 0u32;
    // SAFETY: no input or output buffers; synchronous handle.
    let ok = unsafe {
        DeviceIoControl(
            handle.raw(),
            code,
            ptr::null(),
            0,
            ptr::null_mut(),
            0,
            &raw mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn windows_directory() -> io::Result<String> {
    let mut buf = vec![0u16; 1024];
    let cap = u32::try_from(buf.len()).map_err(|_| io::Error::other("buffer size"))?;
    // SAFETY: writable buffer of `cap` UTF-16 units.
    let len = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), cap) };
    if len == 0 || len >= cap {
        return Err(io::Error::last_os_error());
    }
    Ok(from_wide(&buf))
}

pub fn windows_drive() -> io::Result<DriveLetter> {
    DriveLetter::of_path(&windows_directory()?)
        .ok_or_else(|| io::Error::other("Windows is not installed on a lettered volume"))
}

pub fn system_executable(relative: &str) -> Result<PathBuf, String> {
    let dir = windows_directory().map_err(|e| e.to_string())?;
    Ok(PathBuf::from(dir).join(relative))
}

pub fn volume_extents(volume_path: &str) -> io::Result<Vec<VolumeExtent>> {
    let handle = open(volume_path, 0, 0)?;
    let buf = ioctl_read(&handle, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, 64)?;
    layout::parse_volume_extents(&buf).map_err(io::Error::other)
}

/// The disk number and extent of the Windows volume. A volume spanning more
/// than one extent (dynamic or Storage Spaces) is refused.
pub fn system_volume_extent(drive: DriveLetter) -> io::Result<VolumeExtent> {
    let extents = volume_extents(&format!(r"\\.\{}:", drive.letter()))?;
    match extents.as_slice() {
        [one] => Ok(*one),
        _ => Err(io::Error::other(
            "the Windows volume spans several disk extents",
        )),
    }
}

pub fn layout_disk(number: u32) -> Result<Disk, String> {
    let handle = open_physical_drive(number, DriveAccess::Metadata).map_err(|e| e.to_string())?;
    let geometry = ioctl_read(&handle, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, 256)
        .map_err(|e| format!("disk geometry: {e}"))?;
    let drive_layout = ioctl_read(&handle, IOCTL_DISK_GET_DRIVE_LAYOUT_EX, 16 * 1024)
        .map_err(|e| format!("disk layout: {e}"))?;
    layout::disk_from_layout(
        number,
        layout::parse_geometry(&geometry)?,
        &layout::parse_drive_layout(&drive_layout)?,
    )
}

pub fn firmware_is_uefi() -> Option<bool> {
    let mut kind: FIRMWARE_TYPE = 0;
    // SAFETY: writes one FIRMWARE_TYPE.
    (unsafe { GetFirmwareType(&raw mut kind) } != 0).then_some(kind == FirmwareTypeUefi)
}

/// A `REG_DWORD` under `HKLM`, or `None` when absent or unreadable.
pub fn hklm_dword(subkey: &str, value: &str) -> Option<u32> {
    let subkey = wide(subkey);
    let value = wide(value);
    let mut data = 0u32;
    let mut size = u32::try_from(size_of::<u32>()).ok()?;
    // SAFETY: nul-terminated names; `data` holds exactly `size` bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            ptr::null_mut(),
            (&raw mut data).cast(),
            &raw mut size,
        )
    };
    (status == 0).then_some(data)
}

pub fn power_status() -> Option<PowerStatus> {
    let mut status = SYSTEM_POWER_STATUS {
        ACLineStatus: 0,
        BatteryFlag: 0,
        BatteryLifePercent: 0,
        SystemStatusFlag: 0,
        BatteryLifeTime: 0,
        BatteryFullLifeTime: 0,
    };
    // SAFETY: writes one SYSTEM_POWER_STATUS.
    (unsafe { GetSystemPowerStatus(&raw mut status) } != 0).then_some(PowerStatus {
        ac_line: status.ACLineStatus,
        battery_flag: status.BatteryFlag,
        percent: status.BatteryLifePercent,
    })
}

/// (total, free) bytes of the volume rooted at `drive`.
pub fn volume_space(drive: DriveLetter) -> Option<(u64, u64)> {
    let root = wide(&format!(r"{}:\", drive.letter()));
    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: nul-terminated root; three writable u64 outputs.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &raw mut available,
            &raw mut total,
            &raw mut free,
        )
    };
    (ok != 0).then_some((total, free))
}

/// `hiberfil.sys` exists. Only a definite "not found" counts as absent.
pub fn hiberfile_present(drive: DriveLetter) -> bool {
    match std::fs::metadata(format!(r"{}:\hiberfil.sys", drive.letter())) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

/// (`HiberbootEnabled`, `HibernateEnabled`, hibernation file present).
pub fn hibernation_observation() -> (Option<u32>, Option<u32>, bool) {
    let hiberfile = windows_drive().map_or(true, hiberfile_present);
    (
        hklm_dword(
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Power",
            "HiberbootEnabled",
        ),
        hklm_dword(
            r"SYSTEM\CurrentControlSet\Control\Power",
            "HibernateEnabled",
        ),
        hiberfile,
    )
}

/// Run a PowerShell program through `-EncodedCommand`; stdout on success,
/// the bounded stderr text otherwise.
pub fn powershell(script: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let exe = system_executable(POWERSHELL)?;
    let out = bounded_command::run_os(
        &exe,
        &scripts::powershell_args(script),
        None,
        PS_OUTPUT_LIMIT,
        timeout,
    )
    .map_err(|error| format!("PowerShell: {error}"))?;
    if out.status.success() {
        return Ok(out.stdout);
    }
    let detail = String::from_utf8_lossy(&out.stderr);
    let detail: String = detail.trim().chars().take(600).collect();
    Err(format!("PowerShell failed ({}): {detail}", out.status))
}

/// Enable one privilege already present in this process token.
pub fn enable_privilege(name: &str) -> io::Result<()> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: pseudo-handle of this process; writes one token handle.
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &raw mut token,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let name = wide(name);
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    // SAFETY: local system; nul-terminated privilege name; one LUID output.
    if unsafe { LookupPrivilegeValueW(ptr::null(), name.as_ptr(), &raw mut luid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    // SAFETY: valid token opened for adjustment; previous state not requested.
    let ok = unsafe {
        AdjustTokenPrivileges(
            token.raw(),
            0,
            &raw const privileges,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    // AdjustTokenPrivileges succeeds without assigning a privilege the token
    // lacks; only GetLastError tells.
    // SAFETY: reads this thread's last-error value.
    let error = unsafe { GetLastError() };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if error == ERROR_NOT_ALL_ASSIGNED {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this process does not hold the privilege (not elevated)",
        ));
    }
    Ok(())
}

pub fn efi_get(name: &str) -> io::Result<Option<Vec<u8>>> {
    let name = wide(name);
    let guid = wide(EFI_GLOBAL_GUID);
    let mut capacity = 4096usize;
    loop {
        let mut buf = vec![0u8; capacity];
        let size = u32::try_from(capacity).map_err(|_| io::Error::other("EFI buffer size"))?;
        let mut attributes = 0u32;
        // SAFETY: nul-terminated name and GUID; `buf` writable for `size`.
        let len = unsafe {
            GetFirmwareEnvironmentVariableExW(
                name.as_ptr(),
                guid.as_ptr(),
                buf.as_mut_ptr().cast(),
                size,
                &raw mut attributes,
            )
        };
        if len != 0 {
            buf.truncate(usize::try_from(len).map_err(|_| io::Error::other("EFI size"))?);
            return Ok(Some(buf));
        }
        // SAFETY: reads this thread's last-error value.
        let error = unsafe { GetLastError() };
        if error == ERROR_ENVVAR_NOT_FOUND {
            return Ok(None);
        }
        if error == ERROR_INSUFFICIENT_BUFFER && capacity < MAX_EFI_VAR_BYTES {
            capacity = capacity.saturating_mul(2).min(MAX_EFI_VAR_BYTES);
            continue;
        }
        return Err(io::Error::from_raw_os_error(
            i32::try_from(error).unwrap_or(i32::MAX),
        ));
    }
}

/// Write (or, with empty `data`, delete) a global EFI variable.
pub fn efi_set(name: &str, data: &[u8]) -> io::Result<()> {
    let name = wide(name);
    let guid = wide(EFI_GLOBAL_GUID);
    let size = u32::try_from(data.len()).map_err(|_| io::Error::other("EFI variable too large"))?;
    let value = if data.is_empty() {
        ptr::null()
    } else {
        data.as_ptr().cast()
    };
    // SAFETY: nul-terminated name and GUID; `data` readable for `size`.
    let ok = unsafe {
        SetFirmwareEnvironmentVariableExW(name.as_ptr(), guid.as_ptr(), value, size, EFI_ATTRIBUTES)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A page-aligned buffer suitable for `FILE_FLAG_NO_BUFFERING` I/O.
#[derive(Debug)]
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}

// SAFETY: exclusively owned allocation.
unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> io::Result<Self> {
        // SAFETY: fresh committed read/write allocation, page aligned.
        let ptr = unsafe {
            VirtualAlloc(
                ptr::null(),
                len.max(1),
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if ptr.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            ptr: ptr.cast(),
            len,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `len` committed bytes owned by self.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: releases the whole VirtualAlloc reservation.
        unsafe {
            VirtualFree(self.ptr.cast(), 0, MEM_RELEASE);
        }
    }
}

fn seek(handle: &Handle, offset: u64) -> io::Result<()> {
    let distance = i64::try_from(offset).map_err(|_| io::Error::other("offset overflow"))?;
    // SAFETY: valid handle; new position not requested.
    if unsafe { SetFilePointerEx(handle.raw(), distance, ptr::null_mut(), FILE_BEGIN) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Read exactly `buf.len()` bytes at `offset`. Alignment is the caller's.
pub fn read_exact_at(handle: &Handle, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    seek(handle, offset)?;
    let len = u32::try_from(buf.len()).map_err(|_| io::Error::other("read too large"))?;
    let mut done = 0u32;
    // SAFETY: `buf` writable for `len`; synchronous handle.
    if unsafe {
        ReadFile(
            handle.raw(),
            buf.as_mut_ptr(),
            len,
            &raw mut done,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if done != len {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short read"));
    }
    Ok(())
}

/// Write exactly `buf.len()` bytes at `offset`. Alignment is the caller's.
pub fn write_all_at(handle: &Handle, offset: u64, buf: &[u8]) -> io::Result<()> {
    seek(handle, offset)?;
    let len = u32::try_from(buf.len()).map_err(|_| io::Error::other("write too large"))?;
    let mut done = 0u32;
    // SAFETY: `buf` readable for `len`; synchronous handle.
    if unsafe {
        WriteFile(
            handle.raw(),
            buf.as_ptr(),
            len,
            &raw mut done,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if done != len {
        return Err(io::Error::new(io::ErrorKind::WriteZero, "short write"));
    }
    Ok(())
}

pub fn flush(handle: &Handle) -> io::Result<()> {
    // SAFETY: valid handle.
    if unsafe { FlushFileBuffers(handle.raw()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Every volume GUID path (`\\?\Volume{...}\`).
pub fn volumes() -> io::Result<Vec<String>> {
    let mut buf = vec![0u16; 64];
    let cap = u32::try_from(buf.len()).map_err(|_| io::Error::other("buffer size"))?;
    // SAFETY: writable buffer of `cap` units.
    let find = unsafe { FindFirstVolumeW(buf.as_mut_ptr(), cap) };
    if find == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut out = vec![from_wide(&buf)];
    let result = loop {
        buf.fill(0);
        // SAFETY: live find handle; writable buffer.
        if unsafe { FindNextVolumeW(find, buf.as_mut_ptr(), cap) } == 0 {
            // SAFETY: reads this thread's last-error value.
            let error = unsafe { GetLastError() };
            break if error == ERROR_NO_MORE_FILES {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(
                    i32::try_from(error).unwrap_or(i32::MAX),
                ))
            };
        }
        out.push(from_wide(&buf));
    };
    // SAFETY: closes the find handle opened above.
    unsafe {
        FindVolumeClose(find);
    }
    result.map(|()| out)
}

/// Drive letters and folder mount points of a volume GUID path.
pub fn mount_points(volume: &str) -> io::Result<Vec<String>> {
    let name = wide(volume);
    let mut buf = vec![0u16; 1024];
    let cap = u32::try_from(buf.len()).map_err(|_| io::Error::other("buffer size"))?;
    let mut needed = 0u32;
    // SAFETY: nul-terminated volume name; writable multi-string buffer.
    if unsafe {
        GetVolumePathNamesForVolumeNameW(name.as_ptr(), buf.as_mut_ptr(), cap, &raw mut needed)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(buf
        .split(|&c| c == 0)
        .filter(|part| !part.is_empty())
        .map(String::from_utf16_lossy)
        .collect())
}

/// Lock and dismount a volume; the lock lasts as long as the handle.
pub fn lock_volume(volume_device: &str) -> io::Result<Handle> {
    let handle = open(volume_device, GENERIC_READ | GENERIC_WRITE, 0)?;
    ioctl_plain(&handle, FSCTL_LOCK_VOLUME)?;
    ioctl_plain(&handle, FSCTL_DISMOUNT_VOLUME)?;
    Ok(handle)
}
