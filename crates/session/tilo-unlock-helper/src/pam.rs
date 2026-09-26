//! Minimal PAM `authenticate` driver, loaded at runtime via `dlopen`.
//!
//! # Why dlopen instead of linking `-lpam`
//!
//! Linking libpam at build time needs the `libpam.so` dev symlink
//! (pam-devel). Resolving `libpam.so.0` at runtime with `dlopen` needs only
//! the shared object that ships on every PAM system. This keeps the helper
//! buildable on hosts without pam-devel and adds no compile-time system
//! dependency to the OS image.
//!
//! # What it does
//!
//! [`authenticate`] runs the standard PAM `pam_start` → `pam_authenticate`
//! → (`pam_acct_mgmt`) → `pam_end` sequence for `user` against the named
//! PAM `service`, answering the conversation's password prompt(s) with the
//! supplied `password`. It returns a coarse [`Outcome`] — never a raw PAM
//! string — so the caller (and, one layer up, the lock screen) can keep its
//! messaging non-leaky.
//!
//! `pam_unix` delegates the shadow check to the setuid `unix_chkpwd` helper,
//! so this authenticates the *invoking* user's own password without the
//! helper itself being privileged.

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;

/// The result of a verification attempt. Deliberately coarse: no PAM text
/// crosses this boundary, so nothing leaks upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The credential authenticated (and the account is valid).
    Ok,
    /// PAM rejected the credential (wrong password / unknown user /
    /// disabled account). Maps to the non-leaky "incorrect" message.
    Denied,
    /// The check could not be carried out (library missing, symbols
    /// unresolved, conversation failure). Fail-closed: the caller must
    /// treat this as "stay locked".
    Error,
}

// PAM constants (Linux-PAM). Stable ABI values.
const PAM_SUCCESS: c_int = 0;
const PAM_CONV_ERR: c_int = 19;
/// `PAM_PROMPT_ECHO_OFF` — a hidden prompt (the password).
const PAM_PROMPT_ECHO_OFF: c_int = 1;
/// `PAM_PROMPT_ECHO_ON` — a visible prompt (some stacks echo the username).
const PAM_PROMPT_ECHO_ON: c_int = 2;
const RTLD_NOW: c_int = 0x0002;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: Option<
        extern "C" fn(
            num_msg: c_int,
            msg: *mut *const PamMessage,
            resp: *mut *mut PamResponse,
            appdata_ptr: *mut c_void,
        ) -> c_int,
    >,
    appdata_ptr: *mut c_void,
}

type PamHandle = *mut c_void;

// Function-pointer types for the symbols we resolve from libpam.
type PamStartFn = unsafe extern "C" fn(
    service: *const c_char,
    user: *const c_char,
    conv: *const PamConv,
    pamh: *mut PamHandle,
) -> c_int;
type PamAuthenticateFn = unsafe extern "C" fn(pamh: PamHandle, flags: c_int) -> c_int;
type PamAcctMgmtFn = unsafe extern "C" fn(pamh: PamHandle, flags: c_int) -> c_int;
type PamEndFn = unsafe extern "C" fn(pamh: PamHandle, status: c_int) -> c_int;

/// The PAM conversation callback. PAM hands us the prompt(s); for every
/// password/echo prompt we hand back a freshly `strdup`'d copy of the stored
/// password (PAM takes ownership and `free`s it). Informational and error
/// prompts get a NULL response. `appdata_ptr` points at the NUL-terminated
/// password bytes.
extern "C" fn converse(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() {
        return PAM_CONV_ERR;
    }
    // Allocate the response array PAM expects (`num_msg` entries). PAM frees
    // this array and each `resp` with `free`, so it must come from the same
    // C allocator.
    // `num_msg <= 0` already returned above, so `num_msg` is provably positive here.
    #[allow(
        clippy::cast_sign_loss,
        reason = "num_msg > 0 is checked just above; the sign can't be lost"
    )]
    let n = num_msg as usize;
    let arr = unsafe { libc::calloc(n, std::mem::size_of::<PamResponse>()).cast::<PamResponse>() };
    if arr.is_null() {
        return PAM_CONV_ERR;
    }
    for i in 0..n {
        let m = unsafe { *msg.add(i) };
        if m.is_null() {
            continue;
        }
        let style = unsafe { (*m).msg_style };
        let slot = unsafe { &mut *arr.add(i) };
        slot.resp_retcode = 0;
        if style == PAM_PROMPT_ECHO_OFF || style == PAM_PROMPT_ECHO_ON {
            // Duplicate the password with the C allocator so PAM can free it.
            let dup = unsafe { libc::strdup(appdata_ptr as *const c_char) };
            if dup.is_null() {
                // Roll back: free anything we set, then the array.
                for j in 0..i {
                    let s = unsafe { &mut *arr.add(j) };
                    if !s.resp.is_null() {
                        unsafe { libc::free(s.resp.cast::<c_void>()) };
                    }
                }
                unsafe { libc::free(arr.cast::<c_void>()) };
                return PAM_CONV_ERR;
            }
            slot.resp = dup;
        } else {
            slot.resp = ptr::null_mut();
        }
    }
    unsafe { *resp = arr };
    PAM_SUCCESS
}

/// Resolved libpam entry points.
struct Pam {
    handle: *mut c_void,
    start: PamStartFn,
    authenticate: PamAuthenticateFn,
    acct_mgmt: PamAcctMgmtFn,
    end: PamEndFn,
}

impl Pam {
    /// dlopen `lib` and resolve the four symbols we use. `None` if the
    /// library or any symbol is missing (fail-closed at the call site).
    fn load(lib: &str) -> Option<Self> {
        let libname = CString::new(lib).ok()?;
        let handle = unsafe { libc::dlopen(libname.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: transmuting a resolved symbol to its known C signature.
        unsafe {
            let start = sym(handle, b"pam_start\0")?;
            let authenticate = sym(handle, b"pam_authenticate\0")?;
            let acct_mgmt = sym(handle, b"pam_acct_mgmt\0")?;
            let end = sym(handle, b"pam_end\0")?;
            Some(Pam {
                handle,
                start: std::mem::transmute::<*mut c_void, PamStartFn>(start),
                authenticate: std::mem::transmute::<*mut c_void, PamAuthenticateFn>(authenticate),
                acct_mgmt: std::mem::transmute::<*mut c_void, PamAcctMgmtFn>(acct_mgmt),
                end: std::mem::transmute::<*mut c_void, PamEndFn>(end),
            })
        }
    }
}

impl Drop for Pam {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { libc::dlclose(self.handle) };
        }
    }
}

/// Resolve one non-null symbol or `None`.
unsafe fn sym(handle: *mut c_void, name: &[u8]) -> Option<*mut c_void> {
    let p = unsafe { libc::dlsym(handle, name.as_ptr().cast::<c_char>()) };
    if p.is_null() { None } else { Some(p) }
}

/// Authenticate `user`'s `password` against the PAM `service`, loading libpam
/// from `lib` (typically `"libpam.so.0"`). Returns a coarse [`Outcome`]; no
/// PAM diagnostic text is ever surfaced.
///
/// Fail-closed: any inability to run the check (missing library, unresolved
/// symbol, invalid C strings, conversation failure) yields [`Outcome::Error`],
/// never [`Outcome::Ok`].
pub fn authenticate(lib: &str, service: &str, user: &str, password: &str) -> Outcome {
    let Some(pam) = Pam::load(lib) else {
        return Outcome::Error;
    };
    // The password must be a valid C string (no interior NUL). A NUL in a
    // password can never match a real credential, so reject as denied rather
    // than error (it is a bad credential, not a system failure).
    let (Ok(c_service), Ok(c_user), Ok(c_pass)) = (
        CString::new(service),
        CString::new(user),
        CString::new(password),
    ) else {
        return Outcome::Denied;
    };

    // Keep the password buffer alive for the whole conversation; PAM reads
    // `appdata_ptr` during `pam_authenticate`.
    let conv = PamConv {
        conv: Some(converse),
        appdata_ptr: c_pass.as_ptr().cast_mut().cast::<c_void>(),
    };
    let mut pamh: PamHandle = ptr::null_mut();
    let rc = unsafe {
        (pam.start)(
            c_service.as_ptr(),
            c_user.as_ptr(),
            &raw const conv,
            &raw mut pamh,
        )
    };
    if rc != PAM_SUCCESS || pamh.is_null() {
        return Outcome::Error;
    }

    let auth_rc = unsafe { (pam.authenticate)(pamh, 0) };
    let outcome = if auth_rc == PAM_SUCCESS {
        // Authenticated — also require the account to be valid (not expired
        // / disabled). A non-success here is still "denied", not "ok".
        let acct_rc = unsafe { (pam.acct_mgmt)(pamh, 0) };
        if acct_rc == PAM_SUCCESS {
            Outcome::Ok
        } else {
            Outcome::Denied
        }
    } else {
        Outcome::Denied
    };
    unsafe { (pam.end)(pamh, auth_rc) };
    // Ensure the password C string outlives the conversation above.
    drop(c_pass);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bad inputs are fail-closed: never `Ok`.
    #[test]
    fn bad_library_or_password_never_authenticates() {
        // A missing library is fail-closed: never `Ok`, always `Error`.
        let out = authenticate(
            "libdefinitely-not-a-real-pam.so.999",
            "login",
            "nobody",
            "x",
        );
        assert_eq!(out, Outcome::Error);

        // A password containing an interior NUL cannot be a valid credential;
        // it is rejected as denied (bad credential), never accepted.
        // We can only reach the CString check if the library loads; if
        // libpam.so.0 is unavailable this returns Error, which is also
        // never Ok — either way the invariant "never Ok for a bad input"
        // holds.
        let out = authenticate("libpam.so.0", "login", "nobody", "a\0b");
        assert_ne!(out, Outcome::Ok);
    }

    /// Full PAM path, opt-in: authenticate a **guaranteed-unknown** user
    /// and assert PAM denies it. This drives dlopen + the conversation
    /// callback + `pam_authenticate` against the live PAM stack.
    ///
    /// An unknown user is used deliberately: a *known* local account may be
    /// passwordless (`passwd -S` reports `NP`), in which case `pam_unix`'s
    /// `nullok` legitimately returns success regardless of the typed password
    /// — that is PAM policy, not a helper fault. An unknown user has no
    /// entry to match, so any sane stack denies it, giving a stable deny
    /// oracle across environments.
    ///
    /// `#[ignore]` by default: a failed auth trips `pam_unix`'s ~2s fail
    /// delay and can feed `pam_faillock`, so it stays out of the default
    /// suite. Opt in with:
    ///   `cargo test -p tilo-unlock-helper -- --ignored real_pam_denies_unknown_user`
    #[test]
    #[ignore = "runs a real PAM auth (fail delay + faillock side effects)"]
    fn real_pam_denies_unknown_user() {
        let service =
            std::env::var("TILO_UNLOCK_PAM_SERVICE").unwrap_or_else(|_| "login".to_string());
        let out = authenticate(
            "libpam.so.0",
            &service,
            "tilo-nonexistent-user-8f3a2b1c",
            "irrelevant-password",
        );
        // An unknown user must never authenticate. Depending on whether the
        // unprivileged test process may run the stack this is Denied (the
        // normal outcome) or Error (stack unavailable) — never Ok.
        assert_ne!(out, Outcome::Ok, "an unknown user must never authenticate");
    }
}
