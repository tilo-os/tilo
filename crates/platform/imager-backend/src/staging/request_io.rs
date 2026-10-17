//! Bounded no-follow request and release inputs for the elevated transaction.

use std::io::Read;
use std::path::Path;
const MAX_REQUEST_BYTES: u64 = 1 << 20;

pub fn open_input(path: &str) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a final symlink or block on a FIFO with root authority.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| format!("{path}: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    Ok(file)
}

pub fn read_input(path: &str, limit: u64) -> Result<Vec<u8>, String> {
    let file = open_input(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("{path} is unexpectedly large"));
    }
    Ok(bytes)
}

/// Read the request with the caller's authority checks: a regular file, not
/// a symlink, writable only by root or the authenticated caller, in a
/// directory nobody else can swap it out of.
pub fn read_request(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let caller = ["PKEXEC_UID", "SUDO_UID"]
            .iter()
            .find_map(|k| std::env::var(k).ok())
            .map(|v| v.parse::<u32>().map_err(|_| "invalid caller UID"))
            .transpose()?
            .unwrap_or(0);
        let trusted = |m: &std::fs::Metadata| m.uid() == 0 || m.uid() == caller;
        let parent = path.parent().ok_or("request has no directory")?;
        let dir = std::fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
        let mode = dir.permissions().mode();
        if !dir.is_dir() || !trusted(&dir) || (mode & 0o022 != 0 && mode & 0o1000 == 0) {
            return Err("the request directory is writable by others".into());
        }
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_file() || !trusted(&meta) || meta.permissions().mode() & 0o022 != 0 {
            return Err("the request file is not owned safely".into());
        }
    }
    read_input(&path.to_string_lossy(), MAX_REQUEST_BYTES)
}

pub fn create_progress(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    // create_new refuses a pre-planted file or symlink.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}
