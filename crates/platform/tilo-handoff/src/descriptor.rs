//! Reading the public descriptor from the account's private state directory.
//!
//! The descriptor is public content, but its location is not: a same-name
//! symlink, a directory, a file owned by another account or a world-writable
//! carrier must not be treated as the handoff this login will connect to.

use super::{DESCRIPTOR_FILE, MAX_DESCRIPTOR, PublicHandoff, invalid};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read as _},
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    },
    path::Path,
};

fn current_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn open_child(directory: &File, name: &str, flags: i32) -> io::Result<File> {
    let name = CString::new(name).map_err(|_| invalid("Other-computer handoff is invalid"))?;
    // SAFETY: the name is terminated and `directory` is a live directory
    // descriptor; openat returns a newly owned descriptor on success.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful openat returned a newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn owned_directory(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.uid() != current_uid() || metadata.mode() & 0o022 != 0 {
        return Err(invalid(
            "The other-computer handoff directory is not private",
        ));
    }
    Ok(())
}

/// Read `home/.local/state/tilo/other-device.json` through held directory
/// descriptors. A missing descriptor is `None`; a malformed, linked,
/// outer-owned or public object is an error.
///
/// # Errors
///
/// Refuses a relative home, a linked or outer-owned state path, a non-regular
/// or publicly writable descriptor, an oversized file and invalid metadata.
pub fn read_descriptor(home: &Path) -> io::Result<Option<PublicHandoff>> {
    if !home.is_absolute() {
        return Err(invalid("The other-computer handoff location is invalid"));
    }
    let home = home.canonicalize()?;
    let mut directory = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&home)?;
    owned_directory(&directory)?;
    for segment in [".local", "state", "tilo"] {
        directory = match open_child(&directory, segment, libc::O_RDONLY | libc::O_DIRECTORY) {
            Ok(next) => next,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        owned_directory(&directory)?;
    }
    let file = match open_child(&directory, DESCRIPTOR_FILE, libc::O_RDONLY) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != current_uid()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_DESCRIPTOR
    {
        return Err(invalid("The other-computer handoff is not private"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_DESCRIPTOR + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DESCRIPTOR {
        return Err(invalid("Other-computer handoff is too large"));
    }
    PublicHandoff::decode(&bytes).map(Some)
}
