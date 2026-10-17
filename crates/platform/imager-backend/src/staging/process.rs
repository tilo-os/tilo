//! Owned elevated helper launch, progress transport and retirement.
//! Request authority remains with the elevated transaction, never this transport.

use super::protocol::{Outcome, Progress, STAGING_HELPER_ARG, codes};
use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A running elevated helper launched by the UI.
pub struct Running {
    pub progress_path: std::path::PathBuf,
    child: Option<std::process::Child>,
    /// Private job directory; removed when the helper is dropped.
    _work: Option<tempfile::TempDir>,
    offset: u64,
    partial: String,
    latest: Option<Progress>,
    outcome: Option<Outcome>,
}

/// Snapshot of a running helper, read from `progress.jsonl`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Poll {
    pub latest: Option<Progress>,
    /// `Some` once the helper has written its final line or exited.
    pub outcome: Option<Outcome>,
}

/// A progress-file line.
#[derive(Deserialize)]
#[serde(untagged)]
enum Line {
    Outcome(Outcome),
    Progress(Progress),
}

impl Running {
    /// Observe a helper someone else started (tests, harnesses).
    pub fn watch(progress_path: PathBuf, child: Option<std::process::Child>) -> Self {
        Self {
            progress_path,
            child,
            _work: None,
            offset: 0,
            partial: String::new(),
            latest: None,
            outcome: None,
        }
    }

    pub fn poll(&mut self) -> Poll {
        self.read_new_lines();
        if self.outcome.is_none() {
            let exited = match self.child.as_mut() {
                Some(child) => child.try_wait().ok().flatten().map(|s| s.success()),
                None => None,
            };
            if let Some(success) = exited {
                // Consume bytes written between the last read and exit.
                self.read_new_lines();
                if self.outcome.is_none() {
                    self.outcome = Some(Outcome {
                        ok: false,
                        code: Some(codes::HELPER_EXITED.into()),
                        message: Some(if success {
                            "The installer helper stopped without reporting a result.".into()
                        } else {
                            "Authorization was cancelled or the installer helper stopped.".into()
                        }),
                    });
                }
            }
        }
        Poll {
            latest: self.latest.clone(),
            outcome: self.outcome.clone(),
        }
    }

    fn read_new_lines(&mut self) {
        let Ok(mut file) = std::fs::File::open(&self.progress_path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if file.take(1 << 20).read_to_end(&mut bytes).is_err() {
            return;
        }
        self.offset += bytes.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            match serde_json::from_str::<Line>(line.trim_end()) {
                Ok(Line::Outcome(o)) => self.outcome = Some(o),
                Ok(Line::Progress(p)) => self.latest = Some(p),
                Err(_) => {}
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Never abandon an elevated writer: wait before the job directory goes.
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait();
        }
    }
}

/// The executable the elevated helper runs (the AppImage when packaged).
pub fn helper_executable() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("TILO_IMAGER_APPIMAGE") {
        let path = PathBuf::from(appimage);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
    }
    std::env::current_exe()
}

// `Start-Process` flattens `ArgumentList` into one command line, so each path
// is wrapped in literal double quotes (PowerShell `'"'`) to survive spaces.
#[cfg(any(test, windows))]
const WINDOWS_ELEVATION_SCRIPT: &str = r#"$p = Start-Process -FilePath $args[0] -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList @('--tilo-staging-helper', ('"' + $args[1] + '"'), ('"' + $args[2] + '"')); exit $p.ExitCode"#;

fn elevated_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
) -> Result<std::process::Command, String> {
    #[cfg(target_os = "linux")]
    {
        let mut command = std::process::Command::new("pkexec");
        command
            .arg(exe)
            .arg(STAGING_HELPER_ARG)
            .arg(request)
            .arg(progress);
        Ok(command)
    }
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                WINDOWS_ELEVATION_SCRIPT,
            ])
            .arg(exe)
            .arg(request)
            .arg(progress);
        Ok(command)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (exe, request, progress);
        Err("USB-free installation is available on Windows and Linux only".into())
    }
}

/// Write `request` to a private temp file and launch the elevated helper
/// (`pkexec` on Linux, `RunAs` on Windows).
pub fn launch(request: &[u8]) -> Result<Running, String> {
    use std::io::Write;
    let work = tempfile::Builder::new()
        .prefix("tilo-imager-staging-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let request_path = work.path().join("request.json");
    let progress_path = work.path().join("progress.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request_path)
        .map_err(|e| e.to_string())?;
    file.write_all(request).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let exe = helper_executable().map_err(|e| e.to_string())?;
    let child = elevated_command(&exe, &request_path, &progress_path)?
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut running = Running::watch(progress_path, Some(child));
    running._work = Some(work);
    Ok(running)
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_elevation_preserves_literal_path_quotes() {
        use super::WINDOWS_ELEVATION_SCRIPT;
        assert!(WINDOWS_ELEVATION_SCRIPT.contains("'--tilo-staging-helper'"));
        assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[1] + '\"')"));
        assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[2] + '\"')"));
        assert!(WINDOWS_ELEVATION_SCRIPT.contains("-Verb RunAs"));
    }
}
