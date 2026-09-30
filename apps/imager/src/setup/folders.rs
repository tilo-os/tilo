//! Non-authoritative folder discovery for setup estimates.

use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use ui::time::Duration;

use super::nofollow::{metadata_nofollow, open_directory_nofollow};
use cap_std::fs::Dir;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    SymbolicLink,
    ReadError(String),
    UnsupportedFileType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedEntry {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// A point-in-time enumeration used only for UI estimates.
///
/// Later migration must independently reopen and validate all entries. The
/// paths and sizes here are neither a snapshot nor authority to copy them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderScan {
    pub source: PathBuf,
    pub file_count: u64,
    pub bytes: u64,
    pub empty_directory_count: u64,
    pub empty_directories: Vec<PathBuf>,
    pub skipped_count: u64,
    pub skipped: Vec<SkippedEntry>,
}

const MAX_SCAN_ENTRIES: u64 = 250_000;
const MAX_SCAN_PATH_BYTES: usize = 16 * 1024 * 1024;
const MAX_SCAN_DEPTH: usize = 128;
const MAX_SCAN_DURATION: Duration = Duration::from_secs(30);
const MAX_DIAGNOSTIC_SAMPLES: usize = 64;

fn path_memory_cost(path: &Path) -> usize {
    path.to_string_lossy()
        .len()
        .saturating_mul(4)
        .saturating_add(std::mem::size_of::<PathBuf>())
}

fn record_skipped(scan: &mut FolderScan, entry: SkippedEntry) -> Result<(), String> {
    scan.skipped_count = scan
        .skipped_count
        .checked_add(1)
        .ok_or_else(|| "folder skipped-entry count overflowed".to_string())?;
    if scan.skipped.len() < MAX_DIAGNOSTIC_SAMPLES {
        scan.skipped.push(entry);
    }
    Ok(())
}

fn root_parent_and_name(root: &Path) -> (PathBuf, OsString) {
    match root.file_name() {
        Some(name) => {
            let parent = root.parent().filter(|path| !path.as_os_str().is_empty());
            (
                parent.unwrap_or_else(|| Path::new(".")).to_path_buf(),
                name.to_os_string(),
            )
        }
        None => (root.to_path_buf(), OsString::from(".")),
    }
}

fn pin_selected_root(root: &Path) -> Result<fs::File, String> {
    let (parent_path, name) = root_parent_and_name(root);
    let parent =
        cap_primitives::fs::open_ambient_dir(&parent_path, cap_primitives::ambient_authority())
            .map_err(|error| format!("cannot open parent of {}: {error}", root.display()))?;
    let name = Path::new(&name);
    let expected = metadata_nofollow(&parent, name)
        .map_err(|error| format!("cannot inspect {}: {error}", root.display()))?;
    if expected.is_symlink() {
        return Err(format!(
            "selected folder is a symbolic link: {}",
            root.display()
        ));
    }
    open_directory_nofollow(&parent, name, &expected)
        .map_err(|error| format!("cannot open selected folder {}: {error}", root.display()))
}

fn open_descendant(root: &fs::File, relative: &Path) -> std::io::Result<Dir> {
    let mut current = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "folder scan path contains a non-normal component",
            ));
        };
        let path = Path::new(name);
        let expected = metadata_nofollow(&current, path)?;
        current = open_directory_nofollow(&current, path, &expected)?;
    }
    Ok(Dir::from_std_file(current))
}

fn empty_scan(root: &Path) -> FolderScan {
    FolderScan {
        source: root.to_path_buf(),
        file_count: 0,
        bytes: 0,
        empty_directory_count: 0,
        empty_directories: Vec::new(),
        skipped_count: 0,
        skipped: Vec::new(),
    }
}

/// Enumerate regular files beneath `root` without following symbolic links.
///
/// # Errors
///
/// Returns an error when the root is missing, is not a real directory, cannot
/// be read, or when a count or byte total overflows. Errors below a readable
/// root are returned in [`FolderScan::skipped`] so the estimate remains useful.
pub fn scan_folder(root: &Path) -> Result<FolderScan, String> {
    scan_folder_cancellable(root, &AtomicBool::new(false))
}

/// Enumerate with cooperative cancellation and hard resource budgets.
pub fn scan_folder_cancellable(root: &Path, cancelled: &AtomicBool) -> Result<FolderScan, String> {
    let pinned_root = pin_selected_root(root)?;
    // Store root-relative paths rather than handles or open iterators. Each
    // path is later walked from `pinned_root` with no-follow opens, keeping the
    // descriptor count bounded by the current depth while preventing escape.
    let mut pending = vec![PathBuf::new()];
    let mut pending_path_bytes = path_memory_cost(Path::new(""));
    let mut examined_entries = 0_u64;
    let started = ui::time::now();
    let mut scan = empty_scan(root);

    while let Some(relative_directory) = pending.pop() {
        pending_path_bytes =
            pending_path_bytes.saturating_sub(path_memory_cost(&relative_directory));
        if cancelled.load(Ordering::Relaxed) {
            return Err("folder scan was cancelled".into());
        }
        if started.elapsed() > MAX_SCAN_DURATION {
            return Err("folder scan exceeded its 30 second safety limit".into());
        }
        let directory = root.join(&relative_directory);
        let opened = match open_descendant(&pinned_root, &relative_directory) {
            Ok(opened) => opened,
            Err(error) if relative_directory.as_os_str().is_empty() => {
                return Err(format!(
                    "cannot read selected folder {}: {error}",
                    root.display()
                ));
            }
            Err(error) => {
                record_skipped(
                    &mut scan,
                    SkippedEntry {
                        path: directory,
                        reason: SkipReason::ReadError(error.to_string()),
                    },
                )?;
                continue;
            }
        };
        let entries = match opened.entries() {
            Ok(entries) => entries,
            Err(error) if relative_directory.as_os_str().is_empty() => {
                return Err(format!(
                    "cannot read selected folder {}: {error}",
                    root.display()
                ));
            }
            Err(error) => {
                record_skipped(
                    &mut scan,
                    SkippedEntry {
                        path: directory,
                        reason: SkipReason::ReadError(error.to_string()),
                    },
                )?;
                continue;
            }
        };
        let mut saw_entry = false;
        let mut read_failed = false;
        for entry in entries {
            if cancelled.load(Ordering::Relaxed) {
                return Err("folder scan was cancelled".into());
            }
            examined_entries = examined_entries
                .checked_add(1)
                .ok_or_else(|| "folder entry count overflowed".to_string())?;
            if examined_entries > MAX_SCAN_ENTRIES {
                return Err(format!(
                    "folder scan exceeded its {MAX_SCAN_ENTRIES} entry safety limit"
                ));
            }
            if started.elapsed() > MAX_SCAN_DURATION {
                return Err("folder scan exceeded its 30 second safety limit".into());
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    read_failed = true;
                    record_skipped(
                        &mut scan,
                        SkippedEntry {
                            path: directory.clone(),
                            reason: SkipReason::ReadError(error.to_string()),
                        },
                    )?;
                    continue;
                }
            };
            saw_entry = true;
            let name = entry.file_name();
            let path = directory.join(&name);
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    record_skipped(
                        &mut scan,
                        SkippedEntry {
                            path,
                            reason: SkipReason::ReadError(error.to_string()),
                        },
                    )?;
                    continue;
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                record_skipped(
                    &mut scan,
                    SkippedEntry {
                        path,
                        reason: SkipReason::SymbolicLink,
                    },
                )?;
            } else if file_type.is_file() {
                scan.file_count = scan
                    .file_count
                    .checked_add(1)
                    .ok_or_else(|| "folder file count overflowed".to_string())?;
                scan.bytes = scan
                    .bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| "folder byte estimate overflowed".to_string())?;
            } else if file_type.is_dir() {
                let descendant = relative_directory.join(name);
                if descendant.components().count() > MAX_SCAN_DEPTH {
                    return Err(format!(
                        "folder scan exceeded its {MAX_SCAN_DEPTH} level depth limit"
                    ));
                }
                let cost = path_memory_cost(&descendant);
                pending_path_bytes = pending_path_bytes
                    .checked_add(cost)
                    .ok_or_else(|| "folder scan path budget overflowed".to_string())?;
                if pending_path_bytes > MAX_SCAN_PATH_BYTES {
                    return Err("folder scan exceeded its 16 MiB pending-path safety limit".into());
                }
                pending.push(descendant);
            } else {
                record_skipped(
                    &mut scan,
                    SkippedEntry {
                        path,
                        reason: SkipReason::UnsupportedFileType,
                    },
                )?;
            }
        }
        if !saw_entry && !read_failed {
            scan.empty_directory_count = scan
                .empty_directory_count
                .checked_add(1)
                .ok_or_else(|| "folder empty-directory count overflowed".to_string())?;
            if scan.empty_directories.len() < MAX_DIAGNOSTIC_SAMPLES {
                scan.empty_directories.push(directory);
            }
        }
    }

    scan.empty_directories.sort();
    scan.skipped
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(scan)
}

/// Open the host's folder chooser without blocking the UI thread.
pub fn pick_folder() -> ui::PortalChannel {
    ui::PickRequest::directory("Choose folder", "Choose").send()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// Enumeration, errors, cancellation and estimates.
    #[test]
    fn folder_scan_enumerates_estimates_and_rejects_bad_roots() {
        // Phase `nested_files_and_empty_directories_are_enumerated`.
        {
            let root = tempfile::tempdir().unwrap();
            let nested = root.path().join("nested");
            let empty = root.path().join("empty");
            fs::create_dir(&nested).unwrap();
            fs::create_dir(&empty).unwrap();
            fs::write(root.path().join("one"), b"abc").unwrap();
            fs::write(nested.join("two"), b"12345").unwrap();

            let scan = scan_folder(root.path()).unwrap();

            assert_eq!(scan.file_count, 2);
            assert_eq!(scan.bytes, 8);
            assert_eq!(scan.empty_directories, vec![empty]);
            assert!(scan.skipped.is_empty());
        }

        // Phase `missing_and_non_directory_roots_are_errors`.
        {
            let root = tempfile::tempdir().unwrap();
            let missing = root.path().join("missing");
            assert!(scan_folder(&missing).is_err());

            let file = root.path().join("ordinary-file");
            fs::write(&file, b"data").unwrap();
            assert!(scan_folder(&file).is_err());
        }

        // Phase `cancelled_scan_stops_before_enumeration`.
        {
            let root = tempfile::tempdir().unwrap();
            fs::write(root.path().join("one"), b"abc").unwrap();
            let cancelled = AtomicBool::new(true);

            assert_eq!(
                scan_folder_cancellable(root.path(), &cancelled).unwrap_err(),
                "folder scan was cancelled"
            );
        }

        // Phase `a_scan_is_only_a_point_in_time_estimate`.
        {
            let root = tempfile::tempdir().unwrap();
            let file = root.path().join("changing");
            fs::write(&file, b"old").unwrap();
            let scan = scan_folder(root.path()).unwrap();

            fs::write(&file, b"a different length").unwrap();

            assert_eq!(scan.bytes, 3);
            assert_eq!(fs::metadata(file).unwrap().len(), 18);
        }
    }

    /// Symlink handling and skip details.
    #[test]
    #[cfg(unix)]
    fn folder_scan_never_follows_symlinks_and_bounds_skip_details() {
        // Phase `symbolic_links_are_skipped_and_never_traversed`.
        {
            use std::os::unix::fs::symlink;

            let root = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            fs::write(outside.path().join("secret"), b"not counted").unwrap();
            let link = root.path().join("outside-link");
            symlink(outside.path(), &link).unwrap();

            let scan = scan_folder(root.path()).unwrap();

            assert_eq!(scan.file_count, 0);
            assert_eq!(scan.bytes, 0);
            assert_eq!(scan.skipped.len(), 1);
            assert_eq!(scan.skipped[0].path, link);
            assert_eq!(scan.skipped[0].reason, SkipReason::SymbolicLink);
        }

        // Phase `skipped_entry_details_are_bounded_but_the_total_is_exact`.
        {
            use std::os::unix::fs::symlink;

            let root = tempfile::tempdir().unwrap();
            for index in 0..(MAX_DIAGNOSTIC_SAMPLES + 7) {
                symlink("missing", root.path().join(format!("link-{index:03}"))).unwrap();
            }

            let scan = scan_folder(root.path()).unwrap();
            assert_eq!(scan.skipped_count, (MAX_DIAGNOSTIC_SAMPLES + 7) as u64);
            assert_eq!(scan.skipped.len(), MAX_DIAGNOSTIC_SAMPLES);
        }

        // Phase `nofollow_open_rejects_a_directory_replaced_by_a_symlink`.
        {
            use std::os::unix::fs::symlink;

            let parent = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let selected = parent.path().join("selected");
            fs::create_dir(&selected).unwrap();
            fs::write(outside.path().join("secret"), b"outside").unwrap();
            let parent_handle = fs::File::open(parent.path()).unwrap();
            let expected = metadata_nofollow(&parent_handle, Path::new("selected")).unwrap();

            fs::remove_dir(&selected).unwrap();
            symlink(outside.path(), &selected).unwrap();

            assert!(
                open_directory_nofollow(&parent_handle, Path::new("selected"), &expected).is_err()
            );
        }
    }
}
