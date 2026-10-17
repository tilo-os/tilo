//! Native capacity and durability on a retained directory descriptor.
use std::io;

#[cfg(unix)]
pub fn capacity(directory: &std::fs::File, needed: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: live pinned descriptor and writable native output storage.
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initializes the structure.
    let status = unsafe { status.assume_init() };
    if u128::from(status.f_flag) & u128::from(libc::ST_RDONLY) != 0 {
        return Err(io::Error::other("Selected recovery directory is read-only"));
    }
    let available = u128::from(status.f_bavail) * u128::from(status.f_frsize);
    // Leave room for directory entries and durable journal overhead. Never use
    // privileged reserved blocks as space available to the person copying.
    let required = needed
        .checked_add(16 << 20)
        .ok_or_else(|| io::Error::other("Recovery capacity overflow"))?;
    if available < u128::from(required) {
        return Err(io::Error::other(
            "Selected recovery directory has insufficient free space",
        ));
    }
    crate::migration_durability::readable_directory(directory)?.sync_all()
}
#[cfg(not(unix))]
pub fn capacity(_: &std::fs::File, _: u64) -> io::Result<()> {
    Err(io::Error::other(
        "Native recovery capacity and durability validation is unavailable",
    ))
}
