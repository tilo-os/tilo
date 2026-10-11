//! The existing launcher registration gate, consumed before native RPC startup.
use std::io::{self, Read as _};
use std::os::fd::{FromRawFd as _, RawFd};
use std::sync::OnceLock;

pub const ENV: &str = "TILO_INTERNAL_LAUNCH_REGISTRATION_FD";

/// Consume the launcher-owned inherited read end exactly once.
/// # Errors
/// Rejects invalid descriptors, a closed gate or a non-release byte.
pub fn wait_fd(fd: RawFd) -> io::Result<()> {
    if fd < 3 {
        return Err(io::Error::other("invalid launch-registration descriptor"));
    }
    // SAFETY: the caller transfers the one inherited registration descriptor.
    let mut gate = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut release = [0];
    gate.read_exact(&mut release)?;
    if release != [1] {
        return Err(io::Error::other("launch-registration gate refused"));
    }
    Ok(())
}

/// Wait once per executing process, before its shared native connection starts.
/// # Errors
/// Rejects a missing release or invalid inherited descriptor; never grants authority.
pub fn wait_inherited() -> io::Result<()> {
    static WAITED: OnceLock<Result<(), String>> = OnceLock::new();
    WAITED
        .get_or_init(|| {
            let Some(raw) = std::env::var_os(ENV) else {
                return Ok(());
            };
            let fd = raw
                .to_str()
                .and_then(|raw| raw.parse::<RawFd>().ok())
                .ok_or("invalid launch-registration descriptor")?;
            wait_fd(fd).map_err(|error| error.to_string())
        })
        .clone()
        .map_err(io::Error::other)
}
