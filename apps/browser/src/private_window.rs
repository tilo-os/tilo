//! Local process boundary for independent private windows.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Launch a separate browser process with isolated private storage.
///
/// # Errors
/// Returns an error if the executable cannot be located or the child/reaper cannot be started.
pub fn launch() -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    launch_executable(&executable)
}

fn launch_executable(executable: &Path) -> Result<(), String> {
    let child = Command::new(executable)
        .arg("--private")
        .env("QOS_BROWSER_PRIVATE", "1")
        .env_remove("QOS_BROWSER_APP_ROOT")
        .env_remove("QOS_CHROMIUM_PROFILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("Could not open a private window: {error}"))?;
    let child = Arc::new(Mutex::new(Some(child)));
    let reaper = child.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("private-window-reaper".into())
        .spawn(move || {
            if let Some(mut child) = reaper.lock().ok().and_then(|mut slot| slot.take()) {
                let _ = child.wait();
            }
        })
    {
        if let Some(mut child) = child.lock().ok().and_then(|mut slot| slot.take()) {
            let _ = child.kill();
            let _ = child.wait();
        }
        return Err(format!("Could not supervise the private window: {error}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn launch_uses_only_private_mode_and_reaps_the_child() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let executable = fixture.path().join("browser-fixture");
        let capture = fixture.path().join("capture");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$@\" \"$QOS_BROWSER_PRIVATE\" \"$QOS_BROWSER_APP_ROOT\" \"$QOS_CHROMIUM_PROFILE\" > '{capture}.tmp'\nmv '{capture}.tmp' '{capture}'\n",
                capture = capture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        launch_executable(&executable).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !capture.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let lines = std::fs::read_to_string(capture).unwrap();
        assert_eq!(lines, "1\n--private\n1\n\n\n");
    }
}
