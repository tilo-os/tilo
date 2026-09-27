//! Renaming without replacing an existing destination.

use std::io;
use std::path::Path;

/// Rename `src` to `dest`, failing with `AlreadyExists` instead of replacing
/// an existing `dest`, atomically where the host supports it.
///
/// # Errors
///
/// Returns the host's error; `Unsupported` where the host has no such rename.
pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
    imp::rename_noreplace(src, dest)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
        let src = std::ffi::CString::new(src.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let dest = std::ffi::CString::new(dest.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: both pointers refer to live, NUL-terminated path buffers.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                src.as_ptr(),
                libc::AT_FDCWD,
                dest.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    unsafe extern "C" {
        fn renamex_np(
            from: *const libc::c_char,
            to: *const libc::c_char,
            flags: u32,
        ) -> libc::c_int;
    }
    const RENAME_EXCL: u32 = 0x0000_0004;

    pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
        let src = std::ffi::CString::new(src.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let dest = std::ffi::CString::new(dest.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: both pointers refer to live, NUL-terminated path buffers.
        let result = unsafe { renamex_np(src.as_ptr(), dest.as_ptr(), RENAME_EXCL) };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileW(existing: *const u16, new: *const u16) -> i32;
    }

    pub fn rename_noreplace(src: &Path, dest: &Path) -> io::Result<()> {
        let src: Vec<u16> = src.as_os_str().encode_wide().chain(Some(0)).collect();
        let dest: Vec<u16> = dest.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: both pointers refer to live, NUL-terminated UTF-16 path buffers.
        let result = unsafe { MoveFileW(src.as_ptr(), dest.as_ptr()) };
        if result != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use std::io;
    use std::path::Path;

    pub fn rename_noreplace(_src: &Path, _dest: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
