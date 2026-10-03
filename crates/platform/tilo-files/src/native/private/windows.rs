//! Windows ownership and DACL checks for identity secrets.
use super::{File, Path, io};
use std::ffi::c_void;
use std::os::windows::{
    ffi::OsStrExt,
    fs::MetadataExt,
    io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_NO_TOKEN, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE,
        GetLastError, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT, SetSecurityInfo,
        },
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorDacl,
        GetTokenInformation, IsValidSid, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_ALWAYS, OPEN_EXISTING, READ_CONTROL, WRITE_DAC,
    },
    System::Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken},
};

struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: all instances own a LocalAlloc-family API result.
        unsafe {
            LocalFree(self.0);
        }
    }
}

struct UserSid(Vec<usize>);
impl UserSid {
    fn current() -> io::Result<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: output pointer is valid and token access is query-only.
        unsafe {
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut raw) == 0
                && (GetLastError() != ERROR_NO_TOKEN
                    || OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw) == 0)
            {
                return Err(io::Error::last_os_error());
            }
            let token = OwnedHandle::from_raw_handle(raw);
            let mut size = 0;
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &raw mut size,
            );
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut buffer = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
            if GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &raw mut size,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(buffer))
        }
    }
    fn ptr(&self) -> PSID {
        // SAFETY: GetTokenInformation initialized an aligned TOKEN_USER and
        // its SID inside this retained allocation.
        unsafe { (*self.0.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
    fn descriptor(&self) -> io::Result<LocalAllocation> {
        let mut sid_string = std::ptr::null_mut();
        // SAFETY: self owns a valid SID; API allocations are freed by guards.
        unsafe {
            if ConvertSidToStringSidW(self.ptr(), &raw mut sid_string) == 0 {
                return Err(io::Error::last_os_error());
            }
            let _sid = LocalAllocation(sid_string.cast());
            let mut length = 0;
            while *sid_string.add(length) != 0 {
                length = length
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("SID string exceeds addressable length"))?;
            }
            let sid = String::from_utf16(std::slice::from_raw_parts(sid_string, length))
                .map_err(|_| io::Error::other("invalid account SID"))?;
            // Explicit owner, protected DACL, only this account gets access.
            // OI/CI make descendants private from the moment they are created.
            let sddl: Vec<_> = format!("O:{sid}D:P(A;OICI;FA;;;{sid})")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(LocalAllocation(descriptor))
        }
    }
}

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

fn validate(file: &File, user: &UserSid, directory: bool, check_acl: bool) -> io::Result<()> {
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
    // SAFETY: live handle and initialized output pointers. The returned
    // owner/DACL/ACE pointers live inside the guarded security descriptor.
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
        if owner.is_null() || EqualSid(owner, user.ptr()) == 0 {
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
            // Standard deny ACEs add no authority; unknown/object/callback
            // ACE types fail closed rather than being interpreted partially.
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
            // SID's fixed header is eight bytes, followed by SubAuthorityCount
            // u32s. Bound the whole SID inside its ACE before any SID API reads it.
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
            if IsValidSid(sid) == 0 || EqualSid(sid, user.ptr()) == 0 {
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
    let user = UserSid::current()?;
    let descriptor = user.descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| io::Error::other("security attributes exceed native length"))?,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
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
    let user = UserSid::current()?;
    let descriptor = user.descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
            .map_err(|_| io::Error::other("security attributes exceed native length"))?,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let name = wide(path)?;
    let access = GENERIC_READ | READ_CONTROL | if write { GENERIC_WRITE } else { 0 };
    let disposition = if exclusive {
        CREATE_NEW
    } else if create {
        OPEN_ALWAYS
    } else {
        OPEN_EXISTING
    };
    // SAFETY: all pointers reference live buffers; success transfers the
    // returned owned handle into File. Reparse points are never followed.
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

pub(super) const fn owner_only(_metadata: &std::fs::Metadata) -> bool {
    true
}
