use super::{information, refused};
use std::{ffi::c_void, fs::File, io, mem::size_of, os::windows::io::AsRawHandle, ptr};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_NO_TOKEN, HANDLE, LocalFree},
    Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SE_FILE_OBJECT,
        },
        DACL_SECURITY_INFORMATION, GetAce, GetLengthSid, GetSecurityDescriptorControl,
        GetTokenInformation, IsValidAcl, IsValidSid, OWNER_SECURITY_INFORMATION, PSID,
        SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY,
    System::Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken},
};

pub(super) struct Descriptor(*mut c_void);
impl Descriptor {
    pub(super) fn as_ptr(&self) -> *mut c_void {
        self.0
    }
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut raw = ptr::null_mut();
        // SAFETY: terminated SDDL, SDK revision 1 and writable pointer output.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &raw mut raw,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(raw))
    }
}
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: owns one SDK LocalAlloc-family allocation.
        unsafe {
            LocalFree(self.0);
        }
    }
}
struct Token(HANDLE);
impl Drop for Token {
    fn drop(&mut self) {
        // SAFETY: owns one native token handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

unsafe fn sid_text(sid: PSID) -> io::Result<String> {
    // SAFETY: callers establish SID storage bounds in a live SDK allocation.
    if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
        return Err(refused("invalid security principal"));
    }
    let mut raw = ptr::null_mut();
    // SAFETY: valid SID; writable native pointer output.
    if unsafe { ConvertSidToStringSidW(sid, &raw mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocation = Descriptor(raw.cast());
    let mut len = 0;
    // SAFETY: SDK returns a terminated SID string within its allocation.
    while unsafe { *raw.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: length just measured in live returned buffer.
    String::from_utf16(unsafe { std::slice::from_raw_parts(raw, len) })
        .map_err(|_| refused("invalid SID text"))
}

pub(super) struct Principal(String);
impl Principal {
    #[cfg(test)]
    pub(super) fn sid_text(&self) -> &str {
        &self.0
    }
    pub(super) fn current() -> io::Result<Self> {
        let mut raw = ptr::null_mut();
        // SAFETY: pseudo thread handle and writable token output. OpenAsSelf
        // avoids an impersonated principal affecting the token query's access.
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut raw) } != 0 {
            let _token = Token(raw);
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "private storage does not support thread impersonation",
            ));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_NO_TOKEN as i32) {
            return Err(error);
        }
        // SAFETY: pseudo process handle and writable native token output.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = Token(raw);
        let mut bytes = 0;
        // SAFETY: zero-capacity token sizing query.
        let sized =
            unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &raw mut bytes) };
        if sized != 0
            || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
            || !(size_of::<TOKEN_USER>()..=65536).contains(&(bytes as usize))
        {
            return Err(refused("cannot size process security token"));
        }
        let capacity = bytes;
        let mut buffer = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        // SAFETY: aligned storage sized by SDK and a live process token.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                capacity,
                &raw mut bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if bytes > capacity || (bytes as usize) < size_of::<TOKEN_USER>() {
            return Err(refused("invalid token size"));
        }
        // SAFETY: successful query initialized aligned TOKEN_USER.
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let start = buffer.as_ptr() as usize;
        let end = start + bytes as usize;
        let address = sid as usize;
        if address < start || address.checked_add(8).is_none_or(|v| v > end) {
            return Err(refused("token SID outside buffer"));
        }
        // SID's byte 1 is the subauthority count. Bound it before SDK validation.
        // SAFETY: the fixed eight-byte SID header is within the token allocation.
        let size = 8 + usize::from(unsafe { *sid.cast::<u8>().add(1) }) * 4;
        if address.checked_add(size).is_none_or(|v| v > end) {
            return Err(refused("token SID exceeds buffer"));
        }
        // SAFETY: full SID storage has been bounded and remains allocated.
        Ok(Self(unsafe { sid_text(sid)? }))
    }
    pub(super) fn private_descriptor(&self) -> io::Result<Descriptor> {
        Descriptor::from_sddl(&format!("O:{}D:P(A;;FA;;;{})", self.0, self.0))
    }
}

pub(super) fn validate_private(file: &File, principal: &Principal) -> io::Result<()> {
    let info = information(file)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 || info.nNumberOfLinks != 1 {
        return Err(refused(
            "private storage requires a regular file with one link",
        ));
    }
    validate_private_security(file, principal)
}

// Share the security admission without relaxing the regular-file kind/link gate.
pub(super) fn validate_private_security(file: &File, principal: &Principal) -> io::Result<()> {
    let mut raw = ptr::null_mut();
    let mut owner = ptr::null_mut();
    let mut dacl: *mut ACL = ptr::null_mut();
    // SAFETY: retained handle, writable SDK outputs; allocation owns returned pointers.
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &raw mut owner,
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut raw,
        )
    };
    let descriptor = Descriptor(raw);
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    if raw.is_null() || owner.is_null() || dacl.is_null() {
        return Err(refused(
            "private file has absent owner or unrestricted DACL",
        ));
    }
    // SAFETY: OS-produced owner and ACL in the live security descriptor.
    if unsafe { sid_text(owner)? } != principal.0 || unsafe { IsValidAcl(dacl) } == 0 {
        return Err(refused("private file owner or DACL is untrusted"));
    }
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: live native descriptor and correctly sized native outputs.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &raw mut control, &raw mut revision) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(refused(
            "private file DACL is not protected from inheritance",
        ));
    }
    // SAFETY: SDK validated the ACL and returned its owned storage.
    for index in 0..unsafe { (*dacl).AceCount } {
        let mut ace = ptr::null_mut();
        // SAFETY: valid ACL and index, writable output.
        if unsafe { GetAce(dacl, u32::from(index), &raw mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetAce returns an ACE_HEADER in the validated ACL.
        let header = unsafe { &*ace.cast::<ACE_HEADER>() };
        if header.AceType == 1 {
            continue;
        } // ordinary deny cannot widen access
        if header.AceType != 0 || header.AceSize < 16 {
            return Err(refused("unsupported private-file ACL entry"));
        }
        // SAFETY: ordinary allow layout, with at least header/mask/SID header.
        let allow = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        let sid = ptr::addr_of!(allow.SidStart).cast_mut().cast::<c_void>();
        // SAFETY: fixed SID header fits; bound subauthorities before SDK calls.
        let sid_size = 8 + u32::from(unsafe { *sid.cast::<u8>().add(1) }) * 4;
        if sid_size > u32::from(header.AceSize) - 8 {
            return Err(refused("ACL SID exceeds entry"));
        }
        // SAFETY: full SID bounded in live validated ACL.
        if unsafe { IsValidSid(sid) } == 0 || unsafe { GetLengthSid(sid) } != sid_size {
            return Err(refused("invalid ACL principal"));
        }
        // Reject all other-principal allows, including inheritance-only ACEs;
        // no administrator/system exceptions or caller-supplied principal proof.
        if unsafe { sid_text(sid)? } != principal.0 {
            return Err(refused("private file ACL grants another principal access"));
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::{os::windows::fs::OpenOptionsExt, path::Path};
    use windows_sys::Win32::{
        Security::{
            Authorization::SetSecurityInfo, GetSecurityDescriptorDacl,
            PROTECTED_DACL_SECURITY_INFORMATION,
        },
        Storage::FileSystem::{FILE_FLAG_BACKUP_SEMANTICS, READ_CONTROL, WRITE_DAC},
    };
    pub(in super::super) fn set_dacl(path: &Path, sddl: &str) {
        let file = File::options()
            .access_mode(WRITE_DAC | READ_CONTROL)
            .share_mode(7)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap();
        let descriptor = Descriptor::from_sddl(sddl).unwrap();
        let mut dacl = ptr::null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        // SAFETY: live fixture descriptor and exact native outputs.
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(
                    descriptor.0,
                    &raw mut present,
                    &raw mut dacl,
                    &raw mut defaulted,
                )
            },
            0
        );
        // SAFETY: fixture-owned handle with WRITE_DAC and live DACL allocation.
        assert_eq!(
            unsafe {
                SetSecurityInfo(
                    file.as_raw_handle(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    dacl,
                    ptr::null_mut(),
                )
            },
            0
        );
    }
}
