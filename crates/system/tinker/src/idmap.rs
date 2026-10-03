//! Entering the user namespace a layer's files live in.
//!
//! A layer needs more than one uid. When a package manager runs it
//! drops privilege to a service account — apt to `_apt`, rpm scriptlets to
//! whatever the package asks for — and a namespace that maps only the calling
//! user turns that into `seteuid 42: Invalid argument`. So a layer session maps
//! the user's whole subordinate id range, which makes every uid in
//! `/etc/passwd` inside the layer a distinct uid.
//!
//! The consequence is that files in the layer belong to subordinate ids that the
//! plain host user has no authority over: reading or deleting them from outside
//! the namespace fails with `EPERM`. Every command that touches a layer's
//! contents therefore re-executes itself inside the same mapping first.

use std::os::unix::process::CommandExt;
use std::process::Command;

/// Set in the environment of the re-executed child so it does not loop.
const MARKER: &str = "TILO_TINKER_IN_USERNS";

/// Whether this process is already running inside the layer id mapping.
pub fn inside_mapping() -> bool {
    std::env::var_os(MARKER).is_some()
}

/// The subordinate id range allocated to a user, as `(start, count)`.
///
/// Parsed from `/etc/subuid` / `/etc/subgid`. A user with no allocation can
/// still open a layer, but service accounts inside it will not work.
pub fn subordinate_range(file: &str, user: &str, uid: u32) -> Option<(u32, u32)> {
    let text = std::fs::read_to_string(file).ok()?;
    text.lines()
        .find_map(|line| parse_subid_line(line, user, uid))
}

/// One `owner:start:count` line, or `None` if it is a comment, blank, or
/// malformed. A single bad line must not hide a later good one, so this is
/// deliberately per-line rather than short-circuiting the whole file.
fn parse_subid_line(line: &str, user: &str, uid: u32) -> Option<(u32, u32)> {
    let mut parts = line.split(':');
    let owner = parts.next()?.trim();
    if owner != user && owner.parse::<u32>() != Ok(uid) {
        return None;
    }
    let start: u32 = parts.next()?.trim().parse().ok()?;
    let count: u32 = parts.next()?.trim().parse().ok()?;
    Some((start, count))
}

/// Re-execute the current binary inside the layer id mapping and never return.
///
/// `unshare(1)`'s `--map-auto` reads `/etc/subuid` and delegates the actual map
/// write to `newuidmap`, the setuid helper shipped for exactly this purpose.
/// Nothing here needs the caller to be root, and nothing here is a privilege
/// escalation: the mapping only ever contains ids the user was already
/// delegated.
pub fn reexec_mapped() -> anyhow::Error {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return anyhow::anyhow!("cannot locate own executable: {e}"),
    };
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut cmd = Command::new("unshare");
    cmd.args([
        "--user",
        "--map-root-user",
        "--map-auto",
        "--mount",
        "--propagation=private",
        "--",
    ])
    .arg(&exe)
    .args(&args)
    .env(MARKER, "1");

    let err = cmd.exec();
    anyhow::anyhow!(
        "could not enter the layer id mapping via unshare(1): {err}. \
         Work layers need unprivileged user namespaces and a subordinate id \
         range in /etc/subuid."
    )
}

/// Re-execute into the mapping if not already inside it.
///
/// # Errors
///
/// Returns `Ok(())` at once when already inside the mapping. Otherwise this
/// calls [`reexec_mapped`]: on success the process image is replaced and
/// this never returns to its caller at all; it returns an error only when
/// that re-exec itself fails — its own executable path can't be found, or
/// `unshare(1)` fails (no unprivileged user namespaces, or no subordinate
/// id range in `/etc/subuid`).
pub fn ensure_mapped() -> anyhow::Result<()> {
    if inside_mapping() {
        return Ok(());
    }
    Err(reexec_mapped())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn subuid_file(body: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn subordinate_id_ranges_are_read_by_name_or_owner_and_tolerate_damage() {
        // Phase `range_is_read_by_name`.
        {
            let f = subuid_file("other:100000:65536\nquinten:524288:65536\n");
            let path = f.path().to_str().unwrap();
            assert_eq!(
                subordinate_range(path, "quinten", 1000),
                Some((524_288, 65_536))
            );
            assert_eq!(subordinate_range(path, "nobody", 65534), None);
        }
        // Phase `range_is_also_read_by_numeric_owner`.
        {
            let f = subuid_file("1000:524288:65536\n");
            assert_eq!(
                subordinate_range(f.path().to_str().unwrap(), "someone", 1000),
                Some((524_288, 65_536))
            );
        }
        // Phase `a_missing_file_is_not_an_error`.
        {
            assert_eq!(subordinate_range("/nonexistent/subuid", "x", 0), None);
        }
        // Phase `malformed_lines_are_skipped_not_fatal`.
        {
            let f = subuid_file("garbage\nx:notanumber:1\nquinten:5:6\n");
            assert_eq!(
                subordinate_range(f.path().to_str().unwrap(), "quinten", 1000),
                Some((5, 6))
            );
        }
    }
}
