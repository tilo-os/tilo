//! Opening identity artifacts without following any symlink.
//!
//! `openat2` with `RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS` does this in
//! one call, but it is not always callable: systemd's `RestrictSUIDSGID=`
//! answers `openat2` with `ENOSYS`, because seccomp cannot inspect the mode
//! inside its `open_how` struct. The identity units set that option, so an
//! `ENOSYS` falls back to walking the path one component at a time with
//! `O_NOFOLLOW`, which refuses the same symlinks and magic links.

use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};

const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const RESOLVE_BENEATH: u64 = 0x08;

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// Open `path` relative to `dir` (the working directory when `None`)
/// without following a symlink in any component. With `beneath`, `path`
/// must also stay inside `dir`: an absolute path is refused, and so is `..`
/// when the kernel's `openat2` is unavailable.
///
/// # Errors
///
/// The `openat2`/`openat` error: `ELOOP` or `ENOTDIR` for a symlink, and
/// `EXDEV` for a path that would leave `dir` under `beneath`.
pub fn open(
    dir: Option<&File>,
    path: &CStr,
    flags: libc::c_int,
    mode: libc::mode_t,
    beneath: bool,
) -> io::Result<File> {
    match openat2(dir, path, flags, mode, beneath) {
        Err(error) if error.raw_os_error() == Some(libc::ENOSYS) => {
            walk(dir, path, flags, mode, beneath)
        }
        result => result,
    }
}

fn openat2(
    dir: Option<&File>,
    path: &CStr,
    flags: libc::c_int,
    mode: libc::mode_t,
    beneath: bool,
) -> io::Result<File> {
    let how = OpenHow {
        flags: u64::try_from(flags)
            .map_err(|_| io::Error::other("open flags are outside the Linux u64 ABI"))?,
        mode: u64::from(mode),
        resolve: RESOLVE_NO_MAGICLINKS
            | RESOLVE_NO_SYMLINKS
            | if beneath { RESOLVE_BENEATH } else { 0 },
    };
    let directory = dir.map_or(libc::AT_FDCWD, File::as_raw_fd);
    // SAFETY: path and open_how remain live for this syscall. A successful
    // descriptor is uniquely transferred into File below.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory,
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = RawFd::try_from(descriptor)
        .map_err(|_| io::Error::other("openat2 returned an invalid descriptor"))?;
    // SAFETY: openat2 returned this owned descriptor and no other File owns it.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

fn openat(dir: RawFd, name: &CStr, flags: libc::c_int, mode: libc::mode_t) -> io::Result<OwnedFd> {
    // SAFETY: name is a live NUL-terminated string for this call.
    let descriptor = unsafe { libc::openat(dir, name.as_ptr(), flags, libc::c_uint::from(mode)) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned this owned descriptor and nothing else owns it.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn walk(
    dir: Option<&File>,
    path: &CStr,
    flags: libc::c_int,
    mode: libc::mode_t,
    beneath: bool,
) -> io::Result<File> {
    let bytes = path.to_bytes();
    let absolute = bytes.first() == Some(&b'/');
    let components: Vec<&[u8]> = bytes
        .split(|&b| b == b'/')
        .filter(|component| !component.is_empty() && *component != b".")
        .collect();
    if beneath && (absolute || components.contains(&b"..".as_slice())) {
        return Err(io::Error::from_raw_os_error(libc::EXDEV));
    }
    let directory_flags = libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut current = if absolute {
        Some(openat(libc::AT_FDCWD, c"/", directory_flags, 0)?)
    } else {
        None
    };
    let base = dir.map_or(libc::AT_FDCWD, File::as_raw_fd);
    let Some((last, parents)) = components.split_last() else {
        let at = current.as_ref().map_or(base, OwnedFd::as_raw_fd);
        return openat(at, c".", flags, mode).map(File::from);
    };
    for component in parents {
        let name =
            CString::new(*component).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
        let at = current.as_ref().map_or(base, OwnedFd::as_raw_fd);
        current = Some(openat(at, &name, directory_flags, 0)?);
    }
    let name = CString::new(*last).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    let at = current.as_ref().map_or(base, OwnedFd::as_raw_fd);
    openat(at, &name, flags | libc::O_NOFOLLOW, mode).map(File::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use std::os::unix::ffi::OsStrExt as _;

    const READ: libc::c_int = libc::O_RDONLY | libc::O_CLOEXEC;

    fn c(path: &std::path::Path) -> CString {
        CString::new(path.as_os_str().as_bytes()).unwrap()
    }

    #[test]
    fn the_fallback_walk_refuses_what_openat2_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().canonicalize().unwrap();
        let real = top.join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("key"), b"seed").unwrap();
        std::os::unix::fs::symlink(&real, top.join("linked-dir")).unwrap();
        std::os::unix::fs::symlink(real.join("key"), real.join("linked-key")).unwrap();
        let root = File::open(&top).unwrap();

        let opened = [
            (None, c(&real.join("key")), false),
            (Some(&root), c("real/key".as_ref()), false),
            (Some(&root), c("./real//key".as_ref()), true),
        ];
        for (dir, path, beneath) in &opened {
            for opener in [openat2, walk] {
                let mut body = String::new();
                opener(*dir, path, READ, 0, *beneath)
                    .unwrap_or_else(|e| panic!("{path:?} beneath={beneath}: {e}"))
                    .read_to_string(&mut body)
                    .unwrap();
                assert_eq!(body, "seed");
            }
        }

        let refused = [
            (None, c(&top.join("linked-dir/key")), false),
            (None, c(&real.join("linked-key")), false),
            (None, c("/proc/self/root/etc/hostname".as_ref()), false),
            (Some(&root), c("linked-dir/key".as_ref()), true),
            (Some(&root), c("real/linked-key".as_ref()), true),
            (Some(&root), c(&real.join("key")), true),
        ];
        for (dir, path, beneath) in &refused {
            assert!(
                openat2(*dir, path, READ, 0, *beneath).is_err(),
                "openat2 accepted {path:?}"
            );
            assert!(
                walk(*dir, path, READ, 0, *beneath).is_err(),
                "the fallback walk accepted {path:?}, which openat2 refuses"
            );
        }
        assert!(
            walk(Some(&root), c"real/../real/key", READ, 0, true).is_err(),
            "without openat2 the walk cannot prove `..` stays beneath, so it refuses it"
        );
    }

    #[test]
    fn the_fallback_walk_creates_the_final_component_with_its_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let root = File::open(tmp.path()).unwrap();
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC;
        drop(walk(Some(&root), c"lock", flags, 0o600, true).unwrap());
        let mode = std::fs::metadata(tmp.path().join("lock"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "created with {mode:o}");
    }
}
