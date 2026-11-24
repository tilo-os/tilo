//! Windows ownership and DACL checks for identity secrets.
use super::{File, Path, io};
use std::os::windows::{
    ffi::OsStrExt,
    fs::MetadataExt,
    io::{AsRawHandle, FromRawHandle},
};
use tilo_socket::account::{Account, LocalAllocation, attributes};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, GetLastError,
        INVALID_HANDLE_VALUE,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER,
        Authorization::{GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo},
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorDacl, IsValidSid,
        IsWellKnownSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        WinLocalSystemSid,
    },
    Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    },
};

/// Explicit owner, a protected DACL, and only this account gets access; OI/CI make descendants
/// private from the moment they are made.
const PRIVATE_SDDL: &str = "O:{sid}D:P(A;OICI;FA;;;{sid})";

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "identity storage must be private, user-owned, and not a reparse point",
    )
}
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in identity path",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn validate(file: &File, user: &Account, directory: bool, check_acl: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        }
    {
        return Err(denied());
    }
    let mut owner = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: live handle and initialized output pointers. The returned owner/DACL/ACE pointers live
    // inside the guarded security descriptor.
    unsafe {
        let result = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            std::ptr::null_mut(),
            &raw mut dacl,
            std::ptr::null_mut(),
            &raw mut descriptor,
        );
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result.cast_signed()));
        }
        let _descriptor = LocalAllocation(descriptor);
        if owner.is_null() || EqualSid(owner, user.sid()) == 0 {
            return Err(denied());
        }
        if !check_acl {
            return Ok(());
        }
        if dacl.is_null() {
            return Err(denied());
        }
        for index in 0..u32::from((*dacl).AceCount) {
            let mut ace = std::ptr::null_mut();
            if GetAce(dacl, index, &raw mut ace) == 0 {
                return Err(io::Error::last_os_error());
            }
            let header = &*ace.cast::<ACE_HEADER>();
            // Standard deny ACEs add no authority; unknown/object/callback ACE types fail closed
            // rather than being interpreted partially.
            if header.AceType == 1 {
                continue;
            }
            if header.AceType != 0 || usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
            {
                return Err(denied());
            }
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            let available = usize::from(header.AceSize)
                .checked_sub(sid_offset)
                .ok_or_else(denied)?;
            // SID's fixed header is eight bytes, followed by SubAuthorityCount u32s. Bound the whole
            // SID inside its ACE before any SID API reads it.
            if available < 8 {
                return Err(denied());
            }
            let sid_bytes = ace.cast::<u8>().add(sid_offset);
            let required = usize::from(*sid_bytes.add(1))
                .checked_mul(4)
                .and_then(|bytes| bytes.checked_add(8))
                .ok_or_else(denied)?;
            if required > available {
                return Err(denied());
            }
            let sid = sid_bytes.cast();
            // The operating system itself may hold an entry: it can take any file regardless,
            // so its entry grants nobody anything (Wine writes one on every file it describes).
            if IsValidSid(sid) == 0
                || (EqualSid(sid, user.sid()) == 0 && IsWellKnownSid(sid, WinLocalSystemSid) == 0)
            {
                return Err(denied());
            }
        }
    }
    Ok(())
}

pub(super) fn directory(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let user = Account::current()?;
    let descriptor = user.descriptor(PRIVATE_SDDL)?;
    let attributes = attributes(&descriptor);
    let name = wide(path)?;
    // SAFETY: terminated path and valid security descriptor live across calls.
    unsafe {
        if CreateDirectoryW(name.as_ptr(), &raw const attributes) == 0
            && GetLastError() != ERROR_ALREADY_EXISTS
        {
            return Err(io::Error::last_os_error());
        }
        let raw = CreateFileW(
            name.as_ptr(),
            READ_CONTROL | WRITE_DAC | GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = File::from_raw_handle(raw);
        validate(&file, &user, true, false)?;
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl = std::ptr::null_mut();
        if GetSecurityDescriptorDacl(
            descriptor.0,
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        ) == 0
            || present == 0
            || dacl.is_null()
        {
            return Err(denied());
        }
        let result = SetSecurityInfo(
            raw,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        );
        if result != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(result.cast_signed()));
        }
        validate(&file, &user, true, true)
    }
}

pub(super) fn open(path: &Path, write: bool, create: bool, exclusive: bool) -> io::Result<File> {
    let user = Account::current()?;
    let descriptor = user.descriptor(PRIVATE_SDDL)?;
    let attributes = attributes(&descriptor);
    let name = wide(path)?;
    let access = GENERIC_READ | READ_CONTROL | if write { GENERIC_WRITE } else { 0 };
    let disposition = if exclusive {
        CREATE_NEW
    } else if create {
        OPEN_ALWAYS
    } else {
        OPEN_EXISTING
    };
    // SAFETY: all pointers reference live buffers; success transfers the returned owned handle into
    // File. Reparse points are never followed.
    unsafe {
        let raw = CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &raw const attributes,
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        );
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let file = File::from_raw_handle(raw);
        validate(&file, &user, false, true)?;
        Ok(file)
    }
}
