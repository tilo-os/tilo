//! The few libpam calls a module makes.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr::NonNull;
use zeroize::Zeroizing;

/// libpam's opaque `pam_handle_t`.
#[repr(C)]
pub struct PamHandle {
    _private: [u8; 0],
}

pub const PAM_SERVICE: c_int = 1;
pub const PAM_AUTHTOK: c_int = 6;
pub const PAM_OLDAUTHTOK: c_int = 7;
pub const PAM_PRELIM_CHECK: c_int = 0x4000;
pub const PAM_UPDATE_AUTHTOK: c_int = 0x2000;
const PAM_ERROR_MSG: c_int = 3;
const LOG_WARNING: c_int = 4;

// By its soname, which every PAM system has, rather than the `libpam.so`
// development link: the module then names libpam as a dependency and loads
// even into a process that opened libpam privately, as tilo-unlock-helper does.
#[link(name = "libpam.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn pam_get_user(pamh: *mut PamHandle, user: *mut *const c_char, prompt: *const c_char)
    -> c_int;
    fn pam_get_authtok(
        pamh: *mut PamHandle,
        item: c_int,
        authtok: *mut *const c_char,
        prompt: *const c_char,
    ) -> c_int;
    fn pam_get_item(pamh: *const PamHandle, item_type: c_int, item: *mut *const c_void) -> c_int;
    fn pam_prompt(
        pamh: *mut PamHandle,
        style: c_int,
        response: *mut *mut c_char,
        fmt: *const c_char,
        ...
    ) -> c_int;
    fn pam_syslog(pamh: *const PamHandle, priority: c_int, fmt: *const c_char, ...);
}

/// The PAM return codes this module gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Success,
    ServiceErr,
    PermDenied,
    AuthErr,
    AuthinfoUnavail,
    UserUnknown,
    MaxTries,
    AuthtokErr,
    AuthtokRecoveryErr,
    Ignore,
}

impl Status {
    pub fn code(self) -> c_int {
        match self {
            Self::Success => 0,
            Self::ServiceErr => 3,
            Self::PermDenied => 6,
            Self::AuthErr => 7,
            Self::AuthinfoUnavail => 9,
            Self::UserUnknown => 10,
            Self::MaxTries => 11,
            Self::AuthtokErr => 20,
            Self::AuthtokRecoveryErr => 21,
            Self::Ignore => 25,
        }
    }

    /// A libpam result handed back unchanged.
    fn from_code(code: c_int) -> Self {
        match code {
            0 => Self::Success,
            6 => Self::PermDenied,
            7 => Self::AuthErr,
            9 => Self::AuthinfoUnavail,
            10 => Self::UserUnknown,
            11 => Self::MaxTries,
            20 => Self::AuthtokErr,
            21 => Self::AuthtokRecoveryErr,
            25 => Self::Ignore,
            _ => Self::ServiceErr,
        }
    }
}

/// A live handle, for the duration of one module call.
pub struct Handle(NonNull<PamHandle>);

impl Handle {
    /// # Safety
    ///
    /// `pamh` is the handle libpam passed to the current module call, and the
    /// result does not outlive that call.
    pub unsafe fn new(pamh: *mut PamHandle) -> Option<Self> {
        NonNull::new(pamh).map(Self)
    }

    fn text(ptr: *const c_char) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        // SAFETY: libpam returns NUL-terminated strings it owns, valid until
        // the item changes, and they are copied out at once.
        unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .ok()
            .map(str::to_owned)
    }

    pub fn user(&self) -> Option<String> {
        let mut user: *const c_char = std::ptr::null();
        // SAFETY: a live handle and a valid out-pointer; a NULL prompt is
        // libpam's default.
        let rc = unsafe { pam_get_user(self.0.as_ptr(), &raw mut user, std::ptr::null()) };
        if rc != 0 {
            return None;
        }
        Self::text(user).filter(|u| !u.is_empty())
    }

    /// The item, asking for it if no earlier module did, with the
    /// `use_first_pass`/`try_first_pass`/`use_authtok` arguments honoured.
    pub fn authtok(&self, item: c_int, prompt: Option<&CStr>) -> Result<Zeroizing<String>, Status> {
        let mut token: *const c_char = std::ptr::null();
        let prompt = prompt.map_or(std::ptr::null(), CStr::as_ptr);
        // SAFETY: a live handle, a valid out-pointer and a NUL-terminated or
        // NULL prompt.
        let rc = unsafe { pam_get_authtok(self.0.as_ptr(), item, &raw mut token, prompt) };
        if rc != 0 {
            return Err(Status::from_code(rc));
        }
        Self::text(token).map(Zeroizing::new).ok_or(Status::AuthErr)
    }

    /// An item an earlier call left, without asking.
    pub fn item_text(&self, item: c_int) -> Option<Zeroizing<String>> {
        let mut value: *const c_void = std::ptr::null();
        // SAFETY: a live handle and a valid out-pointer.
        let rc = unsafe { pam_get_item(self.0.as_ptr(), item, &raw mut value) };
        if rc != 0 {
            return None;
        }
        Self::text(value.cast::<c_char>()).map(Zeroizing::new)
    }

    /// Show `text` as an error through the application's conversation.
    pub fn error_message(&self, text: &str) {
        let Ok(text) = CString::new(text) else { return };
        // SAFETY: a live handle; "%s" with one NUL-terminated argument; a
        // NULL response pointer asks libpam to discard any answer.
        unsafe {
            pam_prompt(
                self.0.as_ptr(),
                PAM_ERROR_MSG,
                std::ptr::null_mut(),
                c"%s".as_ptr(),
                text.as_ptr(),
            );
        }
    }

    pub fn log_warning(&self, text: &str) {
        let Ok(text) = CString::new(text) else { return };
        // SAFETY: a live handle; "%s" with one NUL-terminated argument.
        unsafe {
            pam_syslog(self.0.as_ptr(), LOG_WARNING, c"%s".as_ptr(), text.as_ptr());
        }
    }
}
