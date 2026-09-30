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
        .env("TILO_BROWSER_PRIVATE", "1")
        .env_remove("TILO_BROWSER_APP_ROOT")
        .env_remove("TILO_CHROMIUM_PROFILE")
        // Runtime sockets belong to the parent process. The SDK closes them
        // across exec; their environment values must not reach a new window.
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_RUNTIME_RESUME_LIVE")
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
        // Set parent-only runtime state in a subprocess; process-global env
        // mutation would race with other tests. A real attached parent has
        // these variables even though SDK sockets close during child exec.
        if std::env::var_os("TILO_TEST_PRIVATE_LAUNCH_PARENT").is_none() {
            let result = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "private_window::tests::launch_uses_only_private_mode_and_reaps_the_child",
                    "--nocapture",
                ])
                .env("TILO_TEST_PRIVATE_LAUNCH_PARENT", "1")
                .env("TILO_SVC_COLLAB_FD", "998")
                .env("TILO_SVC_IDENTITY_FD", "999")
                .env("TILO_RUNTIME_RESUME_LIVE", "so.tilo.browser")
                .env("TILO_RUNTIME_PROFILE", "/fixture/runtime")
                .env("TILO_RUNTIME_BINARY", "/fixture/tilo-runtime-mcp")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "private child launch failed: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr),
            );
            // libtest exits 0 when `--exact` selects nothing; require the child ran.
            assert!(
                String::from_utf8_lossy(&result.stdout).contains("test result: ok. 1 passed"),
                "the child ran no test: {}",
                String::from_utf8_lossy(&result.stdout)
            );
            return;
        }
        let fixture = tempfile::tempdir().unwrap();
        let executable = fixture.path().join("browser-fixture");
        let capture = fixture.path().join("capture");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$@\" \"$TILO_BROWSER_PRIVATE\" \"$TILO_BROWSER_APP_ROOT\" \"$TILO_CHROMIUM_PROFILE\" \"$TILO_SVC_COLLAB_FD\" \"$TILO_SVC_IDENTITY_FD\" \"$TILO_RUNTIME_RESUME_LIVE\" \"$TILO_RUNTIME_PROFILE\" \"$TILO_RUNTIME_BINARY\" > '{capture}.tmp'\nmv '{capture}.tmp' '{capture}'\n",
                capture = capture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        launch_executable(&executable).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        while !capture.exists() && ui::time::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let lines = std::fs::read_to_string(capture).unwrap();
        assert_eq!(
            lines,
            "1\n--private\n1\n\n\n\n\n\n/fixture/runtime\n/fixture/tilo-runtime-mcp\n"
        );
    }
}
