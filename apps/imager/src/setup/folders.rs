//! Non-authoritative folder discovery for setup estimates.

use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::nofollow::{metadata_nofollow, open_directory_nofollow};
use cap_std::fs::Dir;

#[cfg(not(target_os = "linux"))]
use std::process::Command;

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
    let started = Instant::now();
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

/// The file choosers off Linux, where the imager ships without the desktop
/// portal: the platform's own folder and disk-image dialogs, run on a worker
/// thread. It serves the two requests the imager makes — a folder, and one
/// disk image to open.
#[cfg(not(target_os = "linux"))]
pub struct PlatformDialogs;

#[cfg(not(target_os = "linux"))]
impl PlatformDialogs {
    fn answered<T: Send + 'static>(outcome: T) -> std::sync::mpsc::Receiver<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(outcome);
        rx
    }

    const UNSUPPORTED: &'static str = "only folder and disk-image choosing is available";
}

#[cfg(not(target_os = "linux"))]
impl ui::host::FilePicker for PlatformDialogs {
    fn pick(&self, request: ui::PickRequest) -> ui::PortalChannel {
        if request.is_multiple() || request.suggested_name().is_some() {
            return Self::answered(ui::PortalOutcome::Failed(Self::UNSUPPORTED.into()));
        }
        let folder = request.is_directory();
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new()
            .name(request.worker_name().to_string())
            .spawn(move || {
                let picked = if folder {
                    native_folder_path()
                } else {
                    crate::probe::native_image_path()
                };
                let outcome = picked.map_or_else(
                    |error| ui::PortalOutcome::Failed(error.to_string()),
                    |path| path.map_or(ui::PortalOutcome::Cancelled, ui::PortalOutcome::Opened),
                );
                let _ = tx.send(outcome);
            });
        rx
    }

    fn pick_multiple(&self, _request: ui::PickRequest) -> ui::portal::MultiChannel {
        Self::answered(ui::portal::MultiOutcome::Failed(Self::UNSUPPORTED.into()))
    }

    fn open_file(&self, _request: ui::PickRequest) -> ui::portal::OpenFileChannel {
        Self::answered(ui::portal::OpenFileOutcome::Failed(
            Self::UNSUPPORTED.into(),
        ))
    }

    fn launch(&self, _request: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        Self::answered(ui::portal::LaunchOutcome::Failed(Self::UNSUPPORTED.into()))
    }
}

#[cfg(target_os = "macos")]
fn native_folder_path() -> std::io::Result<Option<PathBuf>> {
    const SCRIPT: &str = r#"try
set picked to choose folder with prompt "Choose folder"
POSIX path of picked
on error number -128
return ""
end try"#;
    let output = Command::new("osascript").args(["-e", SCRIPT]).output()?;
    if !output.status.success() {
        return Err(std::io::Error::other("macOS folder dialog failed"));
    }
    path_from_dialog_stdout(&output.stdout)
}

#[cfg(target_os = "windows")]
fn native_folder_path() -> std::io::Result<Option<PathBuf>> {
    const SCRIPT: &str = r#"[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = 'Choose folder'
$result = $dialog.ShowDialog()
if ($result -eq [System.Windows.Forms.DialogResult]::OK) {
    [Console]::Out.Write($dialog.SelectedPath)
    exit 0
}
if ($result -eq [System.Windows.Forms.DialogResult]::Cancel) { exit 0 }
[Console]::Error.Write("Folder dialog returned $result")
exit 1"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-Command", SCRIPT])
        .output()?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = detail.trim_end_matches(['\r', '\n']);
        let message = if detail.is_empty() {
            "Windows folder dialog failed".to_string()
        } else {
            format!("Windows folder dialog failed: {detail}")
        };
        return Err(std::io::Error::other(message));
    }
    path_from_dialog_stdout(&output.stdout)
}

#[cfg(not(target_os = "linux"))]
fn path_from_dialog_stdout(stdout: &[u8]) -> std::io::Result<Option<PathBuf>> {
    let text = std::str::from_utf8(stdout).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "folder dialog returned invalid UTF-8",
        )
    })?;
    let text = text.trim_end_matches(['\r', '\n']);
    Ok((!text.is_empty()).then(|| PathBuf::from(text)))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn nested_files_and_empty_directories_are_enumerated() {
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

    #[test]
    fn missing_and_non_directory_roots_are_errors() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing");
        assert!(scan_folder(&missing).is_err());

        let file = root.path().join("ordinary-file");
        fs::write(&file, b"data").unwrap();
        assert!(scan_folder(&file).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_skipped_and_never_traversed() {
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

    #[test]
    fn cancelled_scan_stops_before_enumeration() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("one"), b"abc").unwrap();
        let cancelled = AtomicBool::new(true);

        assert_eq!(
            scan_folder_cancellable(root.path(), &cancelled).unwrap_err(),
            "folder scan was cancelled"
        );
    }

    #[cfg(unix)]
    #[test]
    fn skipped_entry_details_are_bounded_but_the_total_is_exact() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        for index in 0..(MAX_DIAGNOSTIC_SAMPLES + 7) {
            symlink("missing", root.path().join(format!("link-{index:03}"))).unwrap();
        }

        let scan = scan_folder(root.path()).unwrap();
        assert_eq!(scan.skipped_count, (MAX_DIAGNOSTIC_SAMPLES + 7) as u64);
        assert_eq!(scan.skipped.len(), MAX_DIAGNOSTIC_SAMPLES);
    }

    #[test]
    fn a_scan_is_only_a_point_in_time_estimate() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("changing");
        fs::write(&file, b"old").unwrap();
        let scan = scan_folder(root.path()).unwrap();

        fs::write(&file, b"a different length").unwrap();

        assert_eq!(scan.bytes, 3);
        assert_eq!(fs::metadata(file).unwrap().len(), 18);
    }

    #[cfg(unix)]
    #[test]
    fn nofollow_open_rejects_a_directory_replaced_by_a_symlink() {
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

        assert!(open_directory_nofollow(&parent_handle, Path::new("selected"), &expected).is_err());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn dialog_stdout_preserves_spaces_and_rejects_invalid_utf8() {
        assert_eq!(
            path_from_dialog_stdout(b"/folder/with trailing space \r\n").unwrap(),
            Some(PathBuf::from("/folder/with trailing space "))
        );
        assert_eq!(
            path_from_dialog_stdout(&[0xff]).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
