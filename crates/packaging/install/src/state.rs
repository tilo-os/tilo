//! The user's data, as distinct from the package's files.
//!
//! Uninstalling has two halves that must not be conflated. The payload
//! is the publisher's: bytes that came out of a signed `.tapp`, exactly
//! reproducible by reinstalling, and removing them costs nothing.
//! The app's *state* is the user's: notes they wrote, settings they
//! chose, whatever the app saved. It lives outside the payload, at
//! `<data home>/tilo-rooms/<room>/<package>` — one directory per room
//! per package, which is the partitioning `launcher::appdir` creates
//! and an app has no way to opt out of.
//!
//! Because the two are different in kind, they are removed by different
//! decisions. `tilo-install uninstall` always removes the payload. It
//! removes state only when told to, and this module exists so the
//! telling is informed: it can enumerate exactly which directories
//! would go, in which rooms, and how much they hold, before anything is
//! deleted.

use std::path::{Path, PathBuf};

/// One room's state directory for one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomState {
    /// Room slug as it appears on disk. `personal` is the room a launch
    /// with no workspace behind it gets.
    pub room: String,
    pub path: PathBuf,
    /// Bytes on disk, summed over the tree. Approximate by design —
    /// it is there to let a person judge "is this worth keeping", not
    /// to balance a ledger.
    pub bytes: u64,
    /// Files in the tree.
    pub files: usize,
}

/// The directory every room lives under. Mirrors
/// `launcher::appdir::ROOMS_DIR`; kept as its own constant rather than
/// a dependency because the installer must not link the launcher.
pub const ROOMS_DIR: &str = "tilo-rooms";

/// Every room in which `package_id` has state.
///
/// Sorted by room name so two runs print the same thing. A package
/// that was never launched has none, and that is not an error.
pub fn find(data_home: &Path, package_id: &str) -> Vec<RoomState> {
    let rooms_root = data_home.join(ROOMS_DIR);
    let Ok(rooms) = std::fs::read_dir(&rooms_root) else {
        return Vec::new();
    };
    let component = sanitise(package_id);
    let mut out = Vec::new();
    for room in rooms.flatten() {
        if !room.path().is_dir() {
            continue;
        }
        let path = room.path().join(&component);
        if !path.is_dir() {
            continue;
        }
        let (bytes, files) = measure(&path);
        out.push(RoomState {
            room: room.file_name().to_string_lossy().to_string(),
            path,
            bytes,
            files,
        });
    }
    out.sort_by(|a, b| a.room.cmp(&b.room));
    out
}

/// Delete the listed state directories, returning how many were
/// removed and the failures.
///
/// Failures are collected rather than aborting: a state directory that
/// cannot be removed must not leave the *package* half-uninstalled, and
/// the caller can report both outcomes.
pub fn remove(states: &[RoomState]) -> (usize, Vec<(PathBuf, std::io::Error)>) {
    let mut removed: usize = 0;
    let mut failures = Vec::new();
    for state in states {
        match std::fs::remove_dir_all(&state.path) {
            Ok(()) => removed = removed.saturating_add(1),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                removed = removed.saturating_add(1);
            }
            Err(e) => failures.push((state.path.clone(), e)),
        }
    }
    (removed, failures)
}

/// Total bytes across a list, for a one-line summary.
pub fn total_bytes(states: &[RoomState]) -> u64 {
    states.iter().map(|s| s.bytes).sum()
}

/// `1.4 MB`, for a prompt a person reads.
#[allow(
    clippy::cast_precision_loss,
    reason = "display only, rounded to one decimal place; precision beyond f64's 52-bit \
              mantissa is unobservable at that rounding and would only matter for byte counts \
              in the exabyte range"
)]
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("kB", 1_000),
        ("B", 1),
    ];
    for (label, scale) in UNITS {
        if bytes >= scale {
            if scale == 1 {
                return format!("{bytes} B");
            }
            return format!("{:.1} {label}", bytes as f64 / scale as f64);
        }
    }
    "0 B".to_string()
}

/// Matches `launcher::appdir`'s component sanitiser. The two have to
/// agree or the installer would look for state under a name the
/// launcher never wrote.
fn sanitise(raw: &str) -> String {
    let mapped: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if mapped.is_empty() || mapped.chars().all(|c| c == '.') {
        return format!("pkg_{}", fnv1a(raw));
    }
    mapped
}

/// The same FNV-1a the launcher uses for the same purpose.
fn fnv1a(raw: &str) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, raw.as_bytes());
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

fn measure(dir: &Path) -> (u64, usize) {
    let mut bytes: u64 = 0;
    let mut files: usize = 0;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (bytes, files);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Symlinks are counted as files but never followed: an app that
        // symlinked its state at $HOME must not make the installer
        // report the whole home directory as "app state".
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let (b, f) = measure(&path);
            bytes = bytes.saturating_add(b);
            files = files.saturating_add(f);
        } else {
            bytes = bytes.saturating_add(meta.len());
            files = files.saturating_add(1);
        }
    }
    (bytes, files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(data_home: &Path, room: &str, package: &str, body: &[u8]) -> PathBuf {
        let dir = data_home.join(ROOMS_DIR).join(room).join(package);
        std::fs::create_dir_all(dir.join(".local/share")).unwrap();
        std::fs::write(dir.join(".local/share/state.bin"), body).unwrap();
        dir
    }

    #[test]
    fn package_state_is_found_per_room_measured_and_removed_in_isolation() {
        // Phase `every_room_holding_state_for_a_package_is_found`.
        {
            let tmp = tempfile::tempdir().unwrap();
            seed(tmp.path(), "personal", "so.tilo.notes", b"aaaa");
            seed(tmp.path(), "e3f1-room-uuid", "so.tilo.notes", b"bbbbbbbb");
            // Another package in the same room must not be swept in.
            seed(tmp.path(), "personal", "so.tilo.other", b"cccc");

            let found = find(tmp.path(), "so.tilo.notes");
            let rooms: Vec<&str> = found.iter().map(|s| s.room.as_str()).collect();
            assert_eq!(rooms, vec!["e3f1-room-uuid", "personal"]);
            assert_eq!(total_bytes(&found), 12);
            assert!(found.iter().all(|s| s.files == 1));
        }
        // Phase `a_package_that_was_never_launched_has_no_state`.
        {
            let tmp = tempfile::tempdir().unwrap();
            assert!(find(tmp.path(), "so.tilo.never-run").is_empty());
        }
        // Phase `removing_state_leaves_the_other_packages_alone`.
        {
            let tmp = tempfile::tempdir().unwrap();
            seed(tmp.path(), "personal", "so.tilo.notes", b"aaaa");
            let other = seed(tmp.path(), "personal", "so.tilo.other", b"cccc");
            let found = find(tmp.path(), "so.tilo.notes");
            let (removed, failures) = remove(&found);
            assert_eq!(removed, 1);
            assert!(failures.is_empty());
            assert!(find(tmp.path(), "so.tilo.notes").is_empty());
            assert!(other.exists(), "another package's state must survive");
        }
        // Phase `a_symlinked_subtree_is_measured_but_not_followed`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let dir = seed(tmp.path(), "personal", "so.tilo.notes", b"aaaa");
            let elsewhere = tmp.path().join("big");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::fs::write(elsewhere.join("huge.bin"), vec![0u8; 100_000]).unwrap();
            std::os::unix::fs::symlink(&elsewhere, dir.join("link")).unwrap();

            let found = find(tmp.path(), "so.tilo.notes");
            assert_eq!(found.len(), 1);
            assert!(
                found[0].bytes < 1000,
                "a symlink must not drag an unrelated tree into the total: {}",
                found[0].bytes
            );
        }
    }

    #[test]
    fn state_paths_and_sizes_read_like_the_launchers() {
        // Phase `the_package_component_matches_the_launchers_sanitiser`.
        // The installer and the launcher have to agree on the directory
        // name, or "remove this app's state" would silently miss it.
        {
            assert_eq!(sanitise("so.tilo.notes"), "so.tilo.notes");
            assert_eq!(sanitise("dev/tilo/notes"), "dev_tilo_notes");
            assert_eq!(sanitise(".."), format!("pkg_{}", fnv1a("..")));
        }
        // Phase `human_bytes_reads_like_a_size`.
        {
            assert_eq!(human_bytes(0), "0 B");
            assert_eq!(human_bytes(512), "512 B");
            assert_eq!(human_bytes(1_500), "1.5 kB");
            assert_eq!(human_bytes(2_400_000), "2.4 MB");
        }
    }
}
