//! `pam_tilo`: every sign-in surface asks `tilo-authd`, which checks the secret
//! by unwrapping the person's user key (docs/security/sign-in.md §5.2).
//!
//! Module arguments:
//! - `socket=PATH`: the daemon's socket, for tests.
//! - `boot_sign_in=SERVICE`: when the PAM service is SERVICE (the greeter's),
//!   start the session of the person who signed in at boot once, without asking
//!   again. Any other service steps aside.
//! - `fingerprint`: no secret; succeed when tilo-authd holds the person's key for
//!   a session of theirs running now, the one time a fingerprint may confirm
//!   them (sign-in.md §4.3), and step aside otherwise.
//! - `use_first_pass`, `try_first_pass`, `use_authtok`: as for every module;
//!   libpam's `pam_get_authtok` reads them.
//! - `seats=PATH`: this computer's daemon's native endpoint, which the session
//!   hooks tell who signed in and who left ([`seat`]), for tests.
//!
//! When the daemon cannot be reached the module answers `PAM_AUTHINFO_UNAVAIL`
//! without prompting, and the image's stack ends there: the daemon is the only
//! check of a secret, and checks the old password itself for people not yet moved
//! off it. A migrated person's old hash may still be in `/etc/shadow` for an older
//! image that can boot, so nothing on this one may check it.

mod ffi;
mod seat;

use ffi::{Handle, Status};
use std::ffi::{CStr, c_char, c_int};
use std::path::PathBuf;
use tilo_auth::client::Client;
use tilo_auth::protocol::{Refusal, Reply, Request};

struct Args {
    socket: PathBuf,
    seats: PathBuf,
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
            seats: std::path::Path::new(tilo_paths::machine::DAEMON_SOCKETS)
                .join(tilo_paths::machine::DAEMON_NATIVE),
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
            } else if let Some(path) = arg.strip_prefix("seats=") {
                parsed.seats = PathBuf::from(path);
            } else if let Some(service) = arg.strip_prefix("boot_sign_in=") {
                parsed.boot_sign_in = Some(service.to_owned());
            } else if arg == "fingerprint" {
                parsed.fingerprint = true;
            }
        }
        parsed
    }
}

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
        return Status::AuthinfoUnavail;
    };
    // Someone still on the old password changes it there.
    match enrolled(&mut client, &user) {
        Some(true) => {}
        Some(false) => return Status::Ignore,
        None => return Status::AuthinfoUnavail,
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

/// Where the session finds the one-use secret it redeems for the link its shell is admitted by:
/// the variable the daemon's own shells read (`runtime_collab::wire::shell::SHELL_TOKEN_ENV`).
const SHELL_TOKEN_ENV: &str = "TILO_SHELL_TOKEN";

/// Hand `token` on to the session the application starts, named in its environment: only that
/// session's processes read it, and it admits one link.
fn hand_on(handle: &Handle, token: &str) -> Result<(), String> {
    handle.put_env(SHELL_TOKEN_ENV, token)
}

/// A change of who is signed in, told to the daemon at `socket` for an account: the secret it
/// answers for the person's shell, if any.
type Change = fn(&std::path::Path, &str, Option<&str>) -> Result<Option<String>, String>;

/// Tell this computer's daemon who signed in, or who left, when it runs: never a reason to refuse
/// the session, whatever it answers. A link it answers for the person's shell is handed on to the
/// session.
fn seat_changed(
    pamh: *mut ffi::PamHandle,
    arg_count: c_int,
    arg_values: *const *const c_char,
    change: Change,
) -> c_int {
    guarded(|| {
        // SAFETY: libpam passes its argument vector.
        let options = unsafe { Args::parse(arg_count, arg_values) };
        // SAFETY: libpam passes a live handle for the duration of the call.
        let Some(handle) = (unsafe { Handle::new(pamh) }) else {
            return Status::Success;
        };
        if !options.seats.exists() {
            return Status::Success;
        }
        let Some(user) = handle.user() else {
            return Status::Success;
        };
        let changed = change(
            &options.seats,
            &user,
            handle.env(c"XDG_STATE_HOME").as_deref(),
        )
        .and_then(|token| token.map_or(Ok(()), |token| hand_on(&handle, &token)));
        if let Err(why) = changed {
            handle.log_warning(&why);
        }
        Status::Success
    })
}

/// # Safety
///
/// Called by libpam with a live handle and its module arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_open_session(
    pamh: *mut ffi::PamHandle,
    _flags: c_int,
    arg_count: c_int,
    arg_values: *const *const c_char,
) -> c_int {
    seat_changed(pamh, arg_count, arg_values, seat::signed_in)
}

/// # Safety
///
/// Called by libpam with a live handle and its module arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pam_sm_close_session(
    pamh: *mut ffi::PamHandle,
    _flags: c_int,
    arg_count: c_int,
    arg_values: *const *const c_char,
) -> c_int {
    seat_changed(pamh, arg_count, arg_values, seat::left)
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
