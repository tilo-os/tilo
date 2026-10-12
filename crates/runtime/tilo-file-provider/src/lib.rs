//! The C bridge the macOS File Provider extension (`macos/`) reads the
//! standalone runtime's workspace files through.
//!
//! The extension is a sandboxed Swift process; the workspace is the runtime
//! daemon's, served read only on its profile's `files.sock`
//! (`runtime_fs::workspace_server`). A session keeps one connection, on a
//! thread of its own because the Cap'n Proto client is single-threaded, and
//! reconnects when the runtime restarts. Items are named by
//! workspace-relative path, the root being `""`.
//!
//! Every call answers JSON, `{"ok": …}` or `{"error": "…"}`, as a string
//! the caller frees with [`tilo_fp_string_free`]; `macos/TiloFileProvider.h`
//! declares the functions.

mod session;

use serde::Serialize;
use session::{Request, Session};
use std::{
    ffi::{CStr, CString, c_char},
    os::unix::ffi::OsStrExt as _,
    path::PathBuf,
};

/// Where the runtime serves the workspace for the user running this
/// process: its default profile's `files.sock`. The home directory comes
/// from the user database, since a sandboxed process's `HOME` is its
/// container.
fn default_socket() -> Option<PathBuf> {
    let home = home_directory()?;
    Some(home.join("Library/Application Support/Tilo/runtime/files.sock"))
}

fn home_directory() -> Option<PathBuf> {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0 as c_char; 4096];
    let mut found = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call, `buffer` outlives the
    // strings `entry` points into, and `found` is only read afterwards.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 || found.is_null() {
        return None;
    }
    // SAFETY: `getpwuid_r` succeeded, so `entry` is initialised and its
    // `pw_dir` is a NUL-terminated string inside `buffer`.
    let dir = unsafe { CStr::from_ptr(entry.assume_init().pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Reply<T> {
    Ok(T),
    Error(String),
}

fn answer<T: Serialize>(reply: &Reply<T>) -> *mut c_char {
    let json = serde_json::to_string(reply)
        .unwrap_or_else(|_| r#"{"error":"the answer could not be encoded"}"#.to_owned());
    // JSON escapes every control character, so it holds no NUL.
    CString::new(json).map_or(std::ptr::null_mut(), CString::into_raw)
}

fn answer_with<T: Serialize>(result: Result<T, String>) -> *mut c_char {
    match result {
        Ok(value) => answer(&Reply::Ok(value)),
        Err(error) => answer(&Reply::<()>::Error(error)),
    }
}

/// # Safety
/// `text` is null or a valid NUL-terminated string.
unsafe fn text(text: *const c_char) -> Result<String, String> {
    if text.is_null() {
        return Err("a required string was null".into());
    }
    // SAFETY: the caller guarantees a NUL-terminated string.
    unsafe { CStr::from_ptr(text) }
        .to_str()
        .map(str::to_owned)
        .map_err(|_| "a string was not UTF-8".into())
}

/// # Safety
/// `session` is null or came from [`tilo_fp_open`] and was not closed.
unsafe fn call(session: *const Session, request: Result<Request, String>) -> *mut c_char {
    // SAFETY: the caller guarantees `session` is live or null.
    let Some(session) = (unsafe { session.as_ref() }) else {
        return answer(&Reply::<()>::Error("no session".into()));
    };
    answer_with(request.and_then(|request| session.call(request)))
}

/// Opens a session with the workspace served at `socket`, or at the
/// default profile's socket when `socket` is null. Nothing is connected
/// until the first call. Null when the session cannot start.
///
/// # Safety
/// `socket` is null or a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_fp_open(socket: *const c_char) -> *mut Session {
    let socket = if socket.is_null() {
        default_socket()
    } else {
        // SAFETY: forwarded from the caller.
        unsafe { text(socket) }.ok().map(PathBuf::from)
    };
    socket
        .and_then(|socket| Session::open(socket).ok())
        .map_or(std::ptr::null_mut(), |session| {
            Box::into_raw(Box::new(session))
        })
}

/// Closes a session from [`tilo_fp_open`].
///
/// # Safety
/// `session` is null or came from [`tilo_fp_open`] and is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_fp_close(session: *mut Session) {
    if !session.is_null() {
        // SAFETY: the caller hands back ownership exactly once.
        drop(unsafe { Box::from_raw(session) });
    }
}

/// The entries of the directory at `path`: `{"ok": [item, …]}`, each item
/// `{"name", "kind", "size", "modified"}` with `kind` one of `file`,
/// `directory`, `symlink`, `other` and `modified` in Unix seconds.
///
/// # Safety
/// `session` as for [`tilo_fp_close`] but not closed; `path` a valid
/// NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_fp_list(session: *const Session, path: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller.
    unsafe { call(session, text(path).map(Request::List)) }
}

/// The item at `path`: `{"ok": item}`, its `name` the last component.
///
/// # Safety
/// As [`tilo_fp_list`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_fp_stat(session: *const Session, path: *const c_char) -> *mut c_char {
    // SAFETY: forwarded from the caller.
    unsafe { call(session, text(path).map(Request::Stat)) }
}

/// Hydrates the file at `path` into a new file at `destination`:
/// `{"ok": {"size": bytes}}`.
///
/// # Safety
/// As [`tilo_fp_list`], and `destination` a valid NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_fp_fetch(
    session: *const Session,
    path: *const c_char,
    destination: *const c_char,
) -> *mut c_char {
    // SAFETY: forwarded from the caller.
    let request = unsafe { text(path) }.and_then(|path| {
        // SAFETY: forwarded from the caller.
        let destination = unsafe { text(destination) }?;
        Ok(Request::Fetch {
            path,
            destination: PathBuf::from(destination),
        })
    });
    // SAFETY: forwarded from the caller.
    unsafe { call(session, request) }
}

/// Frees a string this library returned.
///
/// # Safety
/// `string` is null or came from this library and is not used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn tilo_fp_string_free(string: *mut c_char) {
    if !string.is_null() {
        // SAFETY: the caller hands back ownership exactly once.
        drop(unsafe { CString::from_raw(string) });
    }
}

#[cfg(test)]
mod tests;
