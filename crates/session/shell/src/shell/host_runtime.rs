//! Facts about host processes and private files in the session's runtime
//! directory, read by the launcher and by the services below it alike.

use std::path::Path;

/// A host process's parent pid and start time (clock ticks since boot), read
/// from `/proc/<pid>/stat`. The start time tells a process from a later one
/// that reused its pid.
#[cfg(target_os = "linux")]
pub(crate) fn proc_stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may contain spaces/parentheses; the final ')'
    // is the only safe place from which to index the remaining fields.
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `rest[0]` is field 3 (state), so ppid=field 4 -> 1 and starttime=22 -> 19.
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

// Linux process ancestry is the authority for sandbox launch attribution.
// Other hosts must not infer equivalent authority from numeric PIDs.
#[cfg(not(target_os = "linux"))]
pub(crate) fn proc_stat(_pid: u32) -> Option<(u32, u64)> {
    None
}

/// Atomically replace a private file under the runtime directory: the parent
/// is created owner-only (`0700`, tightened when it exists) and the file lands
/// with mode `0600` before it is visible at `path`.
///
/// Nothing is synced. These files live in `$XDG_RUNTIME_DIR`, a tmpfs a crash
/// discards and the session regenerates, so a sync buys no durability; and
/// they are written on the compositor's loop (on every peer sync and launch),
/// where a sync under I/O pressure froze the desktop for seconds.
pub(crate) fn write_runtime_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tilo_files::native::atomic::create_private_directory(parent)?;
    }
    tilo_files::native::atomic::write_regenerable_with_mode(path, bytes, 0o600)
}
