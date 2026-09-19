//! Folder picker and service-owned folder estimate data.

#[cfg(not(target_os = "linux"))]
use std::{path::PathBuf, process::Command};

pub use ui::folders::scan::{FolderScan, SkippedEntry, SkipReason, scan_folder};

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
    use super::*;

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
