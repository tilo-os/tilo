//! `pam_tilo`: every sign-in surface asks `tilo-authd`, which checks the
//! secret by unwrapping the person's user key (docs/security/sign-in.md §5.2).
//!
//! Module arguments:
//! - `socket=PATH`: the daemon's socket, for tests.
//! - `boot_sign_in=SERVICE`: when the PAM service is SERVICE (the greeter's),
//!   start the session of the person who signed in at boot without asking
//!   again, once. Any other service steps aside.
//! - `fingerprint`: no secret; succeed when tilo-authd holds the person's key
//!   for a session of theirs running now, the one time a fingerprint may
//!   confirm them (sign-in.md §4.3), and step aside otherwise. The service
//!   puts the fingerprint module behind this step.
//! - `use_first_pass`, `try_first_pass`, `use_authtok`: as for every module;
//!   libpam's `pam_get_authtok` reads them.
//!
//! When the daemon cannot be reached the module answers
//! `PAM_AUTHINFO_UNAVAIL` without prompting, so the stack's old password
//! check still works for people not yet moved off it; theirs is the only
//! password hash left to check.

mod ffi;

use ffi::{Handle, Status};
use std::ffi::{CStr, c_char, c_int};
use std::path::PathBuf;
use tilo_auth::client::Client;
use tilo_auth::protocol::{Refusal, Reply, Request};

struct Args {
    socket: PathBuf,
    boot_sign_in: Option<String>,
    fingerprint: bool,
}

impl Args {
    /// # Safety
    ///
    /// `arg_values` points at `arg_count` NUL-terminated strings, as libpam passes them.
    unsafe fn parse(arg_count: c_int, arg_values: *const *const c_char) -> Self {
        let mut parsed = Self {
            socket: PathBuf::from(tilo_auth::SOCKET),
            boot_sign_in: None,
            fingerprint: false,
        };
        let count = usize::try_from(arg_count).unwrap_or(0);
        for i in 0..count {
            // SAFETY: the caller guarantees `arg_count` valid entries.
            let ptr = unsafe { *arg_values.add(i) };
            if ptr.is_null() {
                continue;
            }
            // SAFETY: each entry is a NUL-terminated string libpam owns.
            let arg = unsafe { CStr::from_ptr(ptr) }.to_string_lossy();
            if let Some(path) = arg.strip_prefix("socket=") {
                parsed.socket = PathBuf::from(path);
            } else if let Some(service) = arg.strip_prefix("boot_sign_in=") {
                parsed.boot_sign_in = Some(service.to_owned());
            } else if arg == "fingerprint" {
                parsed.fingerprint = true;
            }
        }
        parsed
    }
}

/// What a refusal means to the stack.
fn status_of(refusal: Option<Refusal>) -> Status {
    match refusal {
        // Not a local account: some other module may know them.
        Some(Refusal::NotEnrolled) => Status::Ignore,
        Some(Refusal::Unavailable) => Status::AuthinfoUnavail,
        Some(Refusal::LockedOut) => Status::MaxTries,
        Some(Refusal::NotPermitted) => Status::PermDenied,
        _ => Status::AuthErr,
    }
}

/// What the screen says about a refused secret.
fn message(reply: &Reply) -> Option<String> {
    let wait = reply.retry_in_s.map(|s| s.div_ceil(60).max(1));
    match reply.error? {
        Refusal::Wrong => Some(match (reply.tries_left, wait) {
            (Some(0), Some(minutes)) => {
                format!("That isn't it. The next try is possible in {minutes} min.")
            }
            (Some(1), _) => "That isn't it. 1 try left before a wait.".to_owned(),
            (Some(left), _) => format!("That isn't it. {left} tries left before a wait."),
            (None, _) => "That isn't it.".to_owned(),
        }),
        Refusal::LockedOut => Some(match wait {
            Some(minutes) => format!(
                "Too many tries. The next try is possible in {minutes} min, or use the \
                 recovery key."
            ),
            None => "Too many tries. Wait a while, or use the recovery key.".to_owned(),
        }),
        Refusal::WeakSecret => Some(
            "This device has no security chip to slow down guessing, so the new password \
             has to be longer."
                .to_owned(),
        ),
        _ => None,
    }
}

fn authenticate(pamh: &Handle, args: &Args) -> Status {
    if let Some(greeter) = &args.boot_sign_in
        && pamh
            .item_text(ffi::PAM_SERVICE)
            .as_deref()
            .map(String::as_str)
            != Some(greeter.as_str())
    {
        return Status::Ignore;
    }
    let Some(user) = pamh.user() else {
        return Status::UserUnknown;
    };
    let Ok(mut client) = Client::connect(&args.socket) else {
        pamh.log_warning("tilo-authd is not running; leaving the check to the rest of the stack");
        return if args.boot_sign_in.is_some() || args.fingerprint {
            Status::Ignore
        } else {
            Status::AuthinfoUnavail
        };
    };
    if args.boot_sign_in.is_some() {
        return match client.call(&Request::ClaimBootSignIn { user }) {
            Ok(reply) if reply.ok => Status::Success,
            _ => Status::Ignore,
        };
    }
    if args.fingerprint {
        return match client.call(&Request::KeyHeld { user }) {
            Ok(reply) if reply.ok && reply.key_held == Some(true) => Status::Success,
            _ => Status::Ignore,
        };
    }
    let secret = match pamh.authtok(ffi::PAM_AUTHTOK, None) {
        Ok(secret) => secret,
        Err(status) => return status,
    };
    match client.call(&Request::Verify { user, secret }) {
        Ok(reply) if reply.ok => Status::Success,
        Ok(reply) => {
            if let Some(text) = message(&reply) {
                pamh.error_message(&text);
            }
            status_of(reply.error)
        }
        Err(e) => {
            pamh.log_warning(&format!("tilo-authd did not answer: {e}"));
            Status::AuthinfoUnavail
        }
    }
}

/// Whether the daemon checks this person's secret; `None` when it cannot say.
fn enrolled(client: &mut Client, user: &str) -> Option<bool> {
    let reply = client
        .call(&Request::Status {
            user: user.to_owned(),
        })
        .ok()?;
    reply.ok.then_some(reply.enrolled.unwrap_or(false))
}

fn change_authtok(pamh: &Handle, args: &Args, flags: c_int) -> Status {
    let Some(user) = pamh.user() else {
        return Status::UserUnknown;
    };
    let Ok(mut client) = Client::connect(&args.socket) else {
        return Status::Ignore;
    };
    // Someone still on the old password changes it there.
    if enrolled(&mut client, &user) != Some(true) {
        return Status::Ignore;
    }
    if flags & ffi::PAM_PRELIM_CHECK != 0 {
        return match pamh.authtok(ffi::PAM_OLDAUTHTOK, None) {
            Ok(_) => Status::Success,
            Err(status) => status,
        };
    }
    if flags & ffi::PAM_UPDATE_AUTHTOK == 0 {
        return Status::ServiceErr;
    }
    let Some(current) = pamh.item_text(ffi::PAM_OLDAUTHTOK) else {
        return Status::AuthtokRecoveryErr;
    };
    let new = match pamh.authtok(ffi::PAM_AUTHTOK, None) {
        Ok(new) => new,
        Err(status) => return status,
    };
    match client.call(&Request::SetSecret {
        user,
        current: Some(current),
        recovery: None,
        new,
    }) {
        Ok(reply) if reply.ok => Status::Success,
        Ok(reply) => {
            if let Some(text) = message(&reply) {
                pamh.error_message(&text);
            }
            match reply.error {
                Some(Refusal::Wrong | Refusal::LockedOut) => Status::AuthtokRecoveryErr,
                _ => Status::AuthtokErr,
            }
        }
        Err(_) => Status::AuthtokErr,
    }
}

/// Run `body`, turning a panic into a service error rather than unwinding
/// into libpam.
fn guarded(body: impl FnOnce() -> Status) -> c_int {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or(Status::ServiceErr)
        .code()
}

/// # Safety
///
/// Called by libpam with a live handle and its module arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_authenticate(
    pamh: *mut ffi::PamHandle,
    _flags: c_int,
    arg_count: c_int,
    arg_values: *const *const c_char,
) -> c_int {
    guarded(|| {
        // SAFETY: libpam passes its argument vector.
        let options = unsafe { Args::parse(arg_count, arg_values) };
        // SAFETY: libpam passes a live handle for the duration of the call.
        let Some(handle) = (unsafe { Handle::new(pamh) }) else {
            return Status::ServiceErr;
        };
        authenticate(&handle, &options)
    })
}

/// # Safety
///
/// Called by libpam.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_setcred(
    _pamh: *mut ffi::PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    Status::Success.code()
}

/// Accounts stay with the rest of the stack.
///
/// # Safety
///
/// Called by libpam.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_acct_mgmt(
    _pamh: *mut ffi::PamHandle,
    _flags: c_int,
    _argc: c_int,
    _argv: *const *const c_char,
) -> c_int {
    Status::Ignore.code()
}

/// # Safety
///
/// Called by libpam with a live handle and its module arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_chauthtok(
    pamh: *mut ffi::PamHandle,
    flags: c_int,
    arg_count: c_int,
    arg_values: *const *const c_char,
) -> c_int {
    guarded(|| {
        // SAFETY: libpam passes its argument vector.
        let options = unsafe { Args::parse(arg_count, arg_values) };
        // SAFETY: libpam passes a live handle for the duration of the call.
        let Some(handle) = (unsafe { Handle::new(pamh) }) else {
            return Status::ServiceErr;
        };
        change_authtok(&handle, &options, flags)
    })
}
