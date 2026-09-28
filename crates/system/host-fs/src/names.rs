//! File name rules that only some hosts impose.

/// Whether the host refuses `name` as a file name for reasons beyond path
/// syntax. Windows rejects `:` (alternate data streams) and a trailing `.`;
/// other hosts accept both.
#[inline]
#[must_use]
pub fn rejected_by_host(name: &str) -> bool {
    cfg!(windows) && (name.contains(':') || name.ends_with('.'))
}

/// The exact bytes of `path` on hosts whose paths are byte strings (Unix),
/// for protocols that carry Unix paths verbatim. `None` elsewhere.
#[inline]
#[must_use]
pub fn unix_path_bytes(path: &std::path::Path) -> Option<&[u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
