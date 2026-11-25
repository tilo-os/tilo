//! This process's one connection to its device's daemon, as the launcher handed it.
//!
//! The launcher connects a program it starts to the daemon before its sandbox closes, names the
//! connection in [`NATIVE_FD_ENV`] and the workspace it acts in in [`WORKSPACE_ENV`], and the
//! daemon knows every call on it as that program's. A descriptor has one owner, so the connection
//! is opened once, here, and shared: a program's calls, a window's page logins and an app's
//! secrets all travel on the same line.

use std::sync::{Arc, OnceLock};

use super::LineClient;

/// The connection the launcher handed this process: a descriptor on Unix, an inherited handle
/// on Windows.
pub const NATIVE_FD_ENV: &str = "TILO_SVC_NATIVE_FD";
/// The workspace the launcher started this process in: what its connection acts in.
pub const WORKSPACE_ENV: &str = "TILO_WORKSPACE_ID";

/// This process's connection to the daemon, and the workspace it acts in.
#[derive(Clone)]
pub struct Handed {
    pub line: Arc<LineClient>,
    pub workspace: String,
}

/// The connection the launcher handed this process, opened on first use: `None` for a process
/// it handed none, or one that names no workspace.
#[must_use]
pub fn connection() -> Option<Handed> {
    static HANDED: OnceLock<Option<Handed>> = OnceLock::new();
    HANDED.get_or_init(open).clone()
}

/// Whether the launcher handed this process a connection at all, opened or not.
#[must_use]
pub fn was_handed() -> bool {
    std::env::var_os(NATIVE_FD_ENV).is_some_and(|value| !value.is_empty())
}

fn open() -> Option<Handed> {
    let value = std::env::var(NATIVE_FD_ENV).ok()?;
    let value = value.trim().parse::<usize>().ok()?;
    let workspace = std::env::var(WORKSPACE_ENV)
        .ok()
        .filter(|workspace| !workspace.is_empty())?;
    let line = LineClient::of_stream(inherited(value)?).ok()?;
    Some(Handed {
        line: Arc::new(line),
        workspace,
    })
}

/// The stream the launcher opened for this process and named by `value`. Called once, from
/// [`connection`]'s `OnceLock`, so this is its one owner.
#[expect(
    unsafe_code,
    reason = "the launcher hands the connection as a descriptor"
)]
fn inherited(value: usize) -> Option<tilo_socket::Stream> {
    #[cfg(unix)]
    {
        let fd = std::os::fd::RawFd::try_from(value).ok()?;
        // SAFETY: the launcher opened `fd` for this process alone, and this is its one owner.
        Some(unsafe { <std::os::unix::net::UnixStream as std::os::fd::FromRawFd>::from_raw_fd(fd) })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle as _, OwnedHandle, RawHandle};
        // SAFETY: the launcher made this handle inheritable for this process alone, and this is
        // its one owner.
        let handle = unsafe { OwnedHandle::from_raw_handle(value as RawHandle) };
        tilo_socket::Stream::from_handle(handle).ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = value;
        None
    }
}
