//! The user's app data, as distinct from the package's payload: `uninstall` always removes the
//! payload and removes this only when told to, so it can be listed first with the workspaces it is
//! in and the bytes it takes.
//!
//! It is at `<data home>/tilo-workspaces/<workspace>/<package>`, the layout `launcher::appdir` creates.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceState {
    /// The workspace slug on disk; `personal` is the workspace of a launch with none behind it.
    pub workspace: String,
    pub path: PathBuf,
    /// Bytes on disk, approximate: a prompt shows them to judge whether it is worth keeping.
    pub bytes: u64,
    pub files: usize,
}

/// The directory every workspace is under, shared with `launcher::appdir`.
pub const WORKSPACES_DIR: &str = "tilo-workspaces";

/// Every workspace in which `package_id` has data, sorted by workspace name. A package that was
/// never launched has none, which is not an error.
pub fn find(data_home: &Path, package_id: &str) -> Vec<WorkspaceState> {
    let workspaces_root = data_home.join(WORKSPACES_DIR);
    let Ok(workspaces) = std::fs::read_dir(&workspaces_root) else {
        return Vec::new();
    };
    let component = sanitise(package_id);
    let mut out = Vec::new();
    for workspace in workspaces.flatten() {
        if !workspace.path().is_dir() {
            continue;
        }
        let path = workspace.path().join(&component);
        if !path.is_dir() {
            continue;
        }
        let (bytes, files) = measure(&path);
        out.push(WorkspaceState {
            workspace: workspace.file_name().to_string_lossy().to_string(),
            path,
            bytes,
            files,
        });
    }
    out.sort_by(|a, b| a.workspace.cmp(&b.workspace));
    out
}

/// Delete the listed state directories, returning how many were removed and the failures.
///
/// Failures are collected rather than aborting, so a state directory that cannot be removed does
/// not leave the package half-uninstalled.
pub fn remove(states: &[WorkspaceState]) -> (usize, Vec<(PathBuf, std::io::Error)>) {
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

pub fn total_bytes(states: &[WorkspaceState]) -> u64 {
    states.iter().map(|s| s.bytes).sum()
}

/// `1.4 MB`, for a prompt.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    tilo_formats::decimal_bytes(bytes)
}

/// Turn a package id into a single path component that cannot escape its parent.
///
/// Shared with `launcher::appdir` so the installer looks for data under the exact name the
/// launcher wrote.
#[must_use]
pub fn sanitise(raw: &str) -> String {
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

/// Short, stable FNV-1a hex digest for an id refused verbatim, shared with `launcher::appdir`.
#[must_use]
pub fn fnv1a(raw: &str) -> String {
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
        // Symlinks count as files and are never followed: an app that symlinked its data at $HOME
        // must not make the installer report the whole home directory as app data.
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

    fn seed(data_home: &Path, workspace: &str, package: &str, body: &[u8]) -> PathBuf {
        let dir = data_home.join(WORKSPACES_DIR).join(workspace).join(package);
        std::fs::create_dir_all(dir.join(".local/share")).unwrap();
        std::fs::write(dir.join(".local/share/state.bin"), body).unwrap();
        dir
    }

    tilo_test_env::scenario! {
        fn package_state_is_found_and_measured;

        phase package_state_is_found_per_workspace_measured_and_removed_in_isolation;
        {
            {
                let tmp = tempfile::tempdir().unwrap();
                seed(tmp.path(), "personal", "so.tilo.notes", b"aaaa");
                seed(
                    tmp.path(),
                    "e3f1-workspace-uuid",
                    "so.tilo.notes",
                    b"bbbbbbbb",
                );
                // Another package in the same workspace must not be swept in.
                seed(tmp.path(), "personal", "so.tilo.other", b"cccc");

                let found = find(tmp.path(), "so.tilo.notes");
                let workspaces: Vec<&str> = found.iter().map(|s| s.workspace.as_str()).collect();
                assert_eq!(workspaces, vec!["e3f1-workspace-uuid", "personal"]);
                assert_eq!(total_bytes(&found), 12);
                assert!(found.iter().all(|s| s.files == 1));
            }
            {
                let tmp = tempfile::tempdir().unwrap();
                assert!(find(tmp.path(), "so.tilo.never-run").is_empty());
            }
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

        phase state_paths_read_like_the_launchers;
        {
            // The installer and the launcher have to agree on the directory name, or "remove this app's
            // data" would silently miss it. These literals pin the on-disk names.
            for (package_id, expected) in [
                ("so.tilo.notes", "so.tilo.notes"),
                ("dev/tilo notes", "dev_tilo_notes"),
                ("", "pkg_cbf29ce484222325"),
                ("..", "pkg_07da1a07b4a03f2d"),
                ("...", "pkg_f7d93e17ec4b1219"),
                ("so.tilo.büro", "so.tilo.b_ro"),
            ] {
                assert_eq!(sanitise(package_id), expected, "package_id={package_id:?}");
            }
            assert_eq!(sanitise("dev/tilo/notes"), "dev_tilo_notes");
        }
    }

    #[test]
    fn human_bytes_formats_table() {
        let cases = [
            (0, "0 B"),
            (1, "1 B"),
            (999, "999 B"),
            (1_000, "1.0 kB"),
            (1_023, "1.0 kB"),
            (1_024, "1.0 kB"),
            (1_500, "1.5 kB"),
            (999_999, "1000.0 kB"),
            (1_000_000, "1.0 MB"),
            (1_048_576, "1.0 MB"),
            (1_500_000, "1.5 MB"),
            (1_073_741_824, "1.1 GB"),
            (5_000_000_000, "5.0 GB"),
            (u64::MAX, "18446744073.7 GB"),
        ];
        for (input, expected) in cases {
            assert_eq!(human_bytes(input), expected, "input={input}");
        }
    }
}
