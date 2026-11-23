//! Native capacity and durability on a retained directory descriptor.
use std::io;

#[cfg(unix)]
pub fn capacity(directory: &std::fs::File, needed: u64) -> io::Result<()> {
    let status = rustix::fs::fstatvfs(directory)?;
    if status
        .f_flag
        .contains(rustix::fs::StatVfsMountFlags::RDONLY)
    {
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
