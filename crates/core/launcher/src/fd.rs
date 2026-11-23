//! Descriptor helpers for callers that keep owning a directory hierarchy by holding
//! descriptors rather than paths, and the small kernel-descriptor observations they all need.
use rustix::fs::{AtFlags, Statx, StatxFlags};
use std::{
    ffi::CStr,
    fs::File,
    io::{self, Read, Seek, Write},
    os::{
        fd::{AsFd, AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileExt, MetadataExt, OpenOptionsExt},
        },
    },
};

/// Seals that make a written memfd immutable: no write, grow, shrink or further sealing.
pub(crate) const SEALS: i32 =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

/// Open the directory `name` directly beneath `parent`, refusing symlinks and anything but a directory,
/// and keeping the descriptor out of `exec`.
pub(crate) fn open_beneath(parent: &File, name: &CStr) -> io::Result<File> {
    // SAFETY: both descriptors and the NUL-terminated name stay live for the call.
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `openat` returned a new descriptor whose ownership moves here once.
    Ok(unsafe { File::from_raw_fd(raw) })
}

/// Open the root-owned ELF at `path`, refusing symlinks on the way, any ancestor writable by group
/// or other, and any entry that is not an executable, single-linked, protected root file starting
/// with the ELF magic.
pub(crate) fn protected_executable(ancestors: &[&str], path: &str) -> io::Result<File> {
    for ancestor in ancestors {
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(ancestor)?;
        let facts = directory.metadata()?;
        if facts.uid() != 0 || facts.mode() & 0o022 != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let facts = file.metadata()?;
    if !facts.is_file()
        || facts.uid() != 0
        || facts.nlink() != 1
        || facts.mode() & 0o7022 != 0
        || facts.mode() & 0o111 == 0
        || facts.len() == 0
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut magic = [0; 4];
    file.read_exact_at(&mut magic, 0)?;
    if &magic != b"\x7fELF" {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}

/// The device and inode that identify `file`, to notice a path being swapped for another object between
/// two looks.
pub(crate) fn identity(file: &File) -> io::Result<(u64, u64)> {
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

/// Extended metadata of the held object itself: the empty path names `file`, so no name is resolved and
/// nothing can be swapped in between.
pub(crate) fn statx(file: &File, flags: AtFlags, mask: StatxFlags) -> io::Result<Statx> {
    rustix::fs::statx(file.as_fd(), c"", flags | AtFlags::EMPTY_PATH, mask).map_err(io::Error::from)
}

/// The mount the held object lives on.
pub(crate) fn mount_id(file: &File, flags: AtFlags, unavailable: &'static str) -> io::Result<u64> {
    let facts = statx(file, flags, StatxFlags::MNT_ID)?;
    if facts.stx_mask & StatxFlags::MNT_ID.bits() == 0 || facts.stx_mnt_id == 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, unavailable));
    }
    Ok(facts.stx_mnt_id)
}

/// Create a private, sealable memfd named `name`, write `bytes` and seal it immutable. The
/// file is positioned at the start so a published descriptor reads from offset zero.
pub(crate) fn sealed_memfd(name: &CStr, bytes: &[u8]) -> io::Result<File> {
    sealed_memfd_mode(name, bytes, None)
}

/// [`sealed_memfd`] with an exact file mode, for descriptors published as executables or secrets.
/// The mode is applied before sealing, as callers did.
pub(crate) fn sealed_memfd_mode(name: &CStr, bytes: &[u8], mode: Option<u32>) -> io::Result<File> {
    // SAFETY: literal name and fixed flags create a private sealable descriptor.
    let raw =
        unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `memfd_create` returned a new uniquely owned descriptor.
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(bytes)?;
    file.rewind()?;
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

/// Move a sealed descriptor onto the fixed number `target` a child inherits: a no-op move that
/// clears `CLOEXEC` when it already sits there, otherwise `dup3`.
pub(crate) fn publish_at_fd(file: File, target: i32) -> io::Result<()> {
    let raw = file.as_raw_fd();
    if raw == target {
        if unsafe { libc::fcntl(raw, libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        std::mem::forget(file);
    } else if unsafe { libc::dup3(raw, target, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Verify a sealed, root-owned memfd without reading it: a regular file, owned by root, held only
/// by descriptors (`nlink` 0), at most `max` bytes, carrying every seal, and — when
/// `name_prefix` is given — named under `/proc/self/fd`. Strict in every dimension callers
/// checked separately.
pub(crate) fn check_sealed(file: &File, name_prefix: Option<&[u8]>, max: u64) -> io::Result<()> {
    let metadata = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let named = match name_prefix {
        Some(prefix) => std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?
            .as_os_str()
            .as_bytes()
            .starts_with(prefix),
        None => true,
    };
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.nlink() != 0
        || metadata.len() > max
        || seals < 0
        || seals & SEALS != SEALS
        || !named
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "descriptor lacks sealed root ownership",
        ));
    }
    Ok(())
}

/// [`check_sealed`] then read every byte; a sealed, non-empty receipt or token.
pub(crate) fn read_sealed(
    file: &File,
    name_prefix: Option<&[u8]>,
    max: u64,
) -> io::Result<Vec<u8>> {
    check_sealed(file, name_prefix, max)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = vec![0; usize::try_from(len).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    Ok(bytes)
}

/// One numeric `key:` field from a descriptor's `/proc/self/fdinfo`, strict and bounded: the
/// whole file must fit `bound`, the key must appear at most once, and its value must be all
/// ASCII digits. `None` means the key was absent.
pub(crate) fn fdinfo_u64(fd: &impl AsRawFd, key: &[u8], bound: usize) -> io::Result<Option<u64>> {
    let mut bytes = Vec::new();
    File::open(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))?
        .take(bound.saturating_add(1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > bound {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut found = None;
    for row in bytes.split(|byte| *byte == b'\n') {
        let Some(value) = row.strip_prefix(key) else {
            continue;
        };
        if found.is_some() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let value = value.trim_ascii();
        if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
            return Err(io::ErrorKind::InvalidData.into());
        }
        found = Some(
            std::str::from_utf8(value)
                .map_err(io::Error::other)?
                .parse()
                .map_err(io::Error::other)?,
        );
    }
    Ok(found)
}

/// The device/inode identity of each namespace of the current process that `names` lists, in
/// order, for comparison against a child's or a proof's namespace set.
pub(crate) fn namespace_identities(names: &[&str]) -> io::Result<Vec<(u64, u64)>> {
    names
        .iter()
        .map(|name| {
            let metadata = std::fs::metadata(format!("/proc/self/ns/{name}"))?;
            Ok((metadata.dev(), metadata.ino()))
        })
        .collect()
}

/// Whether a pidfd has signalled: `POLLIN`. A poll failure or `POLLERR`/`POLLNVAL` is refused
/// rather than read as a live process.
pub(crate) fn pidfd_exited(fd: &impl AsRawFd) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&raw mut descriptor, 1, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if descriptor.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(descriptor.revents & libc::POLLIN != 0)
}
