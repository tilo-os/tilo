//! Private staging directories.

use std::io;
use std::path::Path;

/// Create the directory `path` (its parent must exist, and `path` must not)
/// so that only its owner can use it: mode `0700` on Unix. On Windows, where
/// staging is not access-restricted this way, it is created hidden instead.
///
/// # Errors
///
/// Returns the creation error; on Windows a failure to hide it removes the
/// new directory again.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700).create(path)
    }
    #[cfg(windows)]
    {
        std::fs::create_dir(path)?;
        if let Err(error) = mark_private(path) {
            let _ = std::fs::remove_dir(path);
            return Err(error);
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::create_dir(path)
    }
}

/// Keep a private staging path out of casual view where the host has no
/// owner-only permission for it: the Hidden attribute on Windows, nothing
/// elsewhere.
///
/// # Errors
///
/// Returns the attribute change's error.
pub fn mark_private(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn GetFileAttributesW(path: *const u16) -> u32;
            fn SetFileAttributesW(path: *const u16, attributes: u32) -> i32;
        }
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
        const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is a live, NUL-terminated UTF-16 path buffer for both
        // calls. Existing attributes are retained while adding Hidden.
        let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if attributes == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        if unsafe { SetFileAttributesW(wide.as_ptr(), attributes | FILE_ATTRIBUTE_HIDDEN) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(())
    }
}
