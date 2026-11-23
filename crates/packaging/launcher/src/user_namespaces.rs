//! Whether this computer lets a process make a user namespace: what every app's sandbox is built
//! in. A computer that allows none cannot start any app, and the launcher says so by its exit code,
//! so whoever started it can name the cause.

/// The exit code of a launch whose sandbox could not be made because this computer allows no user
/// namespaces. Next to [`crate::services::EXIT_REQUIRED_SERVICE_MISSING`]: a cause the launcher
/// knows, which the session names in the person's words.
pub const EXIT_NO_USER_NAMESPACES: i32 = 71;

/// Whether a process started here may make a user namespace of its own. A child tries it, so a
/// kernel setting, a namespace limit and a container's filter all answer the same way.
///
/// # Errors
///
/// The probe itself could not run: the answer is unknown.
#[cfg(target_os = "linux")]
pub fn allowed() -> std::io::Result<bool> {
    use std::os::unix::process::CommandExt as _;
    let mut probe = std::process::Command::new("/usr/bin/true");
    // SAFETY: the closure runs in the forked child before exec and only makes one syscall.
    unsafe {
        probe.pre_exec(|| {
            if libc::unshare(libc::CLONE_NEWUSER) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    match probe.status() {
        Ok(status) => Ok(status.success()),
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(libc::EPERM | libc::ENOSPC | libc::EUSERS | libc::EINVAL)
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}
