//! Non-authoritative folder discovery for setup estimates.

use std::fs;
use std::path::{Path, PathBuf};

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
    pub empty_directories: Vec<PathBuf>,
    pub skipped: Vec<SkippedEntry>,
}

/// Enumerate regular files beneath `root` without following symbolic links.
///
/// # Errors
///
/// Returns an error when the root is missing, is not a real directory, cannot
/// be read, or when a count or byte total overflows. Errors below a readable
/// root are returned in [`FolderScan::skipped`] so the estimate remains useful.
pub fn scan_folder(root: &Path) -> Result<FolderScan, String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("cannot inspect {}: {error}", root.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "selected folder is a symbolic link: {}",
            root.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!("selected path is not a folder: {}", root.display()));
    }

    // Store paths rather than open iterators so a wide tree cannot retain one
    // descriptor for every discovered child directory.
    let mut pending = vec![(root.to_path_buf(), true)];
    let mut scan = FolderScan {
        source: root.to_path_buf(),
        file_count: 0,
        bytes: 0,
        empty_directories: Vec::new(),
        skipped: Vec::new(),
    };

    while let Some((directory, is_root)) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if is_root => {
                return Err(format!(
                    "cannot read selected folder {}: {error}",
                    directory.display()
                ));
            }
            Err(error) => {
                scan.skipped.push(SkippedEntry {
                    path: directory,
                    reason: SkipReason::ReadError(error.to_string()),
                });
                continue;
            }
        };
        let mut saw_entry = false;
        let mut read_failed = false;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    read_failed = true;
                    scan.skipped.push(SkippedEntry {
                        path: directory.clone(),
                        reason: SkipReason::ReadError(error.to_string()),
                    });
                    continue;
                }
            };
            saw_entry = true;
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    scan.skipped.push(SkippedEntry {
                        path,
                        reason: SkipReason::ReadError(error.to_string()),
                    });
                    continue;
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                scan.skipped.push(SkippedEntry {
                    path,
                    reason: SkipReason::SymbolicLink,
                });
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
                pending.push((path, false));
            } else {
                scan.skipped.push(SkippedEntry {
                    path,
                    reason: SkipReason::UnsupportedFileType,
                });
            }
        }
        if !saw_entry && !read_failed {
            scan.empty_directories.push(directory);
        }
    }

    scan.empty_directories.sort();
    scan.skipped
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(scan)
}

/// Open the host's folder chooser without blocking the UI thread.
#[cfg(target_os = "linux")]
pub fn pick_folder() -> ui::PortalChannel {
    ui::PickRequest::directory("Choose folder", "Choose").send()
}

/// Open the host's folder chooser without blocking the UI thread.
#[cfg(not(target_os = "linux"))]
pub fn pick_folder() -> ui::PortalChannel {
    if ui::portal::suppressed() {
        return ui::portal::inject_for_test(ui::PortalOutcome::Cancelled);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("imager-folder-picker".to_string())
        .spawn(move || {
            let outcome = native_folder_path().map_or_else(
                |error| ui::PortalOutcome::Failed(error.to_string()),
                |path| path.map_or(ui::PortalOutcome::Cancelled, ui::PortalOutcome::Opened),
            );
            let _ = tx.send(outcome);
        });
    rx
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
    fn a_scan_is_only_a_point_in_time_estimate() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("changing");
        fs::write(&file, b"old").unwrap();
        let scan = scan_folder(root.path()).unwrap();

        fs::write(&file, b"a different length").unwrap();

        assert_eq!(scan.bytes, 3);
        assert_eq!(fs::metadata(file).unwrap().len(), 18);
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
