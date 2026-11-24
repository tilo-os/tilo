//! The Windows account a process or thread acts as: what a named pipe and a
//! private file are kept to, and what a pipe's peer is checked against where
//! Unix compares a uid.
use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_TOKEN, GetLastError, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        EqualSid, GetTokenInformation, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenIsAppContainer, TokenSessionId, TokenUser,
    },
    System::Threading::{
        GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    },
};

/// A `LocalAlloc`-family allocation a Windows API handed back, freed on drop.
pub struct LocalAllocation(pub *mut c_void);

// SAFETY: the allocation is immutable once made (a SID string, a security descriptor), and
// LocalFree may run on any thread.
unsafe impl Send for LocalAllocation {}
// SAFETY: as above; nothing writes through the pointer.
unsafe impl Sync for LocalAllocation {}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: every instance owns a LocalAlloc-family API result.
        unsafe {
            LocalFree(self.0);
        }
    }
}

/// The account a token acts as (its `TOKEN_USER`), and whether it is an
/// `AppContainer`'s.
pub struct Account {
    user: Vec<usize>,
    app_container: bool,
    session: u32,
}

impl Account {
    /// The account this thread acts as: its impersonation token's, or the
    /// process's.
    ///
    /// # Errors
    /// The token cannot be read.
    pub fn current() -> io::Result<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: the output pointer is valid and the access is query-only.
        unsafe {
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut raw) == 0
                && (GetLastError() != ERROR_NO_TOKEN
                    || OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw) == 0)
            {
                return Err(io::Error::last_os_error());
            }
            Self::of_token(&OwnedHandle::from_raw_handle(raw))
        }
    }

    /// The account process `pid` runs as.
    ///
    /// # Errors
    /// The process is gone, or its token cannot be read.
    pub fn of_process(pid: u32) -> io::Result<Self> {
        // SAFETY: query-only access; each handle is owned as soon as it is opened.
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            let process = OwnedHandle::from_raw_handle(process);
            let mut raw = std::ptr::null_mut();
            if OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &raw mut raw) == 0 {
                return Err(io::Error::last_os_error());
            }
            Self::of_token(&OwnedHandle::from_raw_handle(raw))
        }
    }

    /// The account the process `process` names runs as: the one opened, whatever its id names
    /// by now.
    ///
    /// # Errors
    /// The handle lacks query access, or the token cannot be read.
    pub fn of_process_handle(process: &impl AsHandle) -> io::Result<Self> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: a live process handle; the token is owned as soon as it is opened.
        unsafe {
            if OpenProcessToken(
                process.as_handle().as_raw_handle(),
                TOKEN_QUERY,
                &raw mut raw,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Self::of_token(&OwnedHandle::from_raw_handle(raw))
        }
    }

    fn of_token(token: &OwnedHandle) -> io::Result<Self> {
        let handle: RawHandle = token.as_raw_handle();
        let mut size = 0;
        // SAFETY: the first call only measures; the buffer is then sized and aligned for it.
        unsafe {
            GetTokenInformation(handle, TokenUser, std::ptr::null_mut(), 0, &raw mut size);
            if size == 0 {
                return Err(io::Error::last_os_error());
            }
            let mut user = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
            if GetTokenInformation(
                handle,
                TokenUser,
                user.as_mut_ptr().cast(),
                size,
                &raw mut size,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut app_container = 0_u32;
            let mut length = 0;
            if GetTokenInformation(
                handle,
                TokenIsAppContainer,
                (&raw mut app_container).cast(),
                4,
                &raw mut length,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut session = 0_u32;
            if GetTokenInformation(
                handle,
                TokenSessionId,
                (&raw mut session).cast(),
                4,
                &raw mut length,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                user,
                app_container: app_container != 0,
                session,
            })
        }
    }

    /// The account's SID, valid for as long as `self` is.
    #[must_use]
    pub fn sid(&self) -> PSID {
        // SAFETY: GetTokenInformation initialized an aligned TOKEN_USER and its SID inside this
        // retained allocation.
        unsafe { (*self.user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    /// Whether `other` is the same account.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        // SAFETY: both SIDs live inside their accounts' allocations.
        unsafe { EqualSid(self.sid(), other.sid()) != 0 }
    }

    /// Whether the token is an `AppContainer`'s: an app this account started,
    /// confined, rather than the account's own programs.
    #[must_use]
    pub fn is_app_container(&self) -> bool {
        self.app_container
    }

    /// The sign-in session the token belongs to: each person signed in at once has their own.
    #[must_use]
    pub const fn session(&self) -> u32 {
        self.session
    }

    /// The SID in its string form (`S-1-5-21-…`).
    ///
    /// # Errors
    /// The conversion failed.
    pub fn sid_string(&self) -> io::Result<String> {
        let mut text = std::ptr::null_mut();
        // SAFETY: the SID is valid; the API allocation is freed by its guard.
        unsafe {
            if ConvertSidToStringSidW(self.sid(), &raw mut text) == 0 {
                return Err(io::Error::last_os_error());
            }
            let _text = LocalAllocation(text.cast());
            let mut length = 0;
            while *text.add(length) != 0 {
                length = length
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("SID too long"))?;
            }
            String::from_utf16(std::slice::from_raw_parts(text, length))
                .map_err(|_| io::Error::other("invalid account SID"))
        }
    }

    /// The security descriptor `sddl` names, with `{sid}` standing for this
    /// account's SID.
    ///
    /// # Errors
    /// The SID or the descriptor could not be made.
    pub fn descriptor(&self, sddl: &str) -> io::Result<LocalAllocation> {
        let sddl: Vec<u16> = sddl
            .replace("{sid}", &self.sid_string()?)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = std::ptr::null_mut();
        // SAFETY: the string is NUL-terminated; the allocation is owned by the returned guard.
        unsafe {
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(LocalAllocation(descriptor))
    }
}

/// Security attributes creating an object with `descriptor`, never inherited.
#[must_use]
pub fn attributes(descriptor: &LocalAllocation) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    }
}
