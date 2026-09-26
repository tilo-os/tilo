//! Desktop helper elevation and bounded progress transport.

use super::desktop_install::{DesktopInstallEvent, DesktopInstallRequest};
use std::sync::mpsc::{self, Receiver};

/// The worker owns the helper until its terminal exit even if the UI drops this
/// receiver. Closing the progress view is never transaction cancellation.
///
/// # Errors
/// Refuses unsupported platforms, invalid requests and thread creation failure.
pub fn spawn_desktop_install(
    request: &DesktopInstallRequest,
) -> Result<Receiver<DesktopInstallEvent>, String> {
    if !cfg!(target_os = "linux") {
        return Err("Desktop installation requires a supported Linux host.".into());
    }
    let executable =
        std::env::current_exe().map_err(|_| "Cannot locate Desktop helper executable.")?;
    let mut command = elevation_command(&executable, request)?;
    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name("desktop-install".into())
        .spawn(move || {
            let outcome = command
                .spawn()
                .map_err(|_| "Cannot start host authorization UI.".into())
                .and_then(|child| track_helper(child, &sender));
            if let Err(reason) = outcome {
                let _ = sender.send(DesktopInstallEvent::Failed(reason));
            }
        })
        .map_err(|_| "Cannot start Desktop installation worker.")?;
    Ok(receiver)
}

fn elevation_command(
    executable: &std::path::Path,
    request: &DesktopInstallRequest,
) -> Result<std::process::Command, String> {
    use super::desktop_install::{REQUEST_LIMIT, request_digest};
    use std::process::{Command, Stdio};
    let json = serde_json::to_string(request)
        .map_err(|_| "Cannot encode Desktop request without loss.")?;
    if json.len() > REQUEST_LIMIT || request.schema != 1 || !request.package_input.is_absolute() {
        return Err("Invalid Desktop request.".into());
    }
    let mut command = Command::new("/usr/bin/pkexec");
    command
        .arg(executable)
        .arg("--tilo-desktop-install-helper")
        .arg(&json)
        .arg(request_digest(json.as_bytes()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    Ok(command)
}

fn track_helper(
    mut child: std::process::Child,
    sender: &mpsc::Sender<DesktopInstallEvent>,
) -> Result<(), String> {
    use std::io::Read;
    let mut pipe = child
        .stdout
        .take()
        .ok_or("Missing Desktop helper protocol channel.")?;
    let mut buffer = [0; 4096];
    let mut frame = Vec::new();
    let mut invalid = false;
    let mut state = 0;
    let mut terminal = None;
    let mut total = 0usize;
    loop {
        let count = match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                invalid = true;
                break;
            }
        };
        total = total.saturating_add(count);
        if total > 256 * 1024 {
            invalid = true;
        }
        for byte in buffer
            .get(..count)
            .ok_or("Invalid Desktop protocol read length.")?
        {
            if invalid {
                continue;
            }
            if *byte != b'\n' {
                if frame.len() >= 64 * 1024 {
                    invalid = true;
                } else {
                    frame.push(*byte);
                }
                continue;
            }
            let Ok(event) = serde_json::from_slice::<DesktopInstallEvent>(&frame) else {
                invalid = true;
                continue;
            };
            frame.clear();
            match (&event, state) {
                (DesktopInstallEvent::Preparing, 0) => state = 1,
                (DesktopInstallEvent::Installing, 1) => state = 2,
                (DesktopInstallEvent::Verifying, 1 | 2) => state = 3,
                (DesktopInstallEvent::DiagnosticLog(path), 2) if path.is_absolute() => (),
                (DesktopInstallEvent::Complete(result), 3)
                    if !result.version.is_empty()
                        && result.session_entry
                            == std::path::Path::new(super::desktop_install::SESSION_ENTRY) =>
                {
                    state = 4;
                    terminal = Some(event.clone());
                }
                (DesktopInstallEvent::Failed(_), 0..=3) => {
                    state = 4;
                    terminal = Some(event.clone());
                }
                _ => {
                    invalid = true;
                    continue;
                }
            }
            // Success is withheld until the helper exits successfully.
            if !matches!(
                event,
                DesktopInstallEvent::Complete(_) | DesktopInstallEvent::Failed(_)
            ) {
                let _ = sender.send(event);
            }
        }
    }
    // Never kill the helper: it may own a package database transaction.
    let status = child
        .wait()
        .map_err(|_| "Cannot establish Desktop helper terminal status.")?;
    if total == 0 {
        match status.code() {
            Some(126) => {
                let _ = sender.send(DesktopInstallEvent::AuthorizationCancelled);
                return Ok(());
            }
            Some(127) => {
                let _ = sender.send(DesktopInstallEvent::AuthorizationRefused);
                return Ok(());
            }
            _ => (),
        }
    }
    if invalid || !frame.is_empty() {
        return Err("Desktop helper progress channel was lost or malformed; installation success is unconfirmed.".into());
    }
    match terminal {
        Some(DesktopInstallEvent::Complete(result)) if status.success() => { let _ = sender.send(DesktopInstallEvent::Complete(result)); Ok(()) }
        Some(DesktopInstallEvent::Failed(reason)) => Err(reason),
        _ => Err("Desktop helper exited without verified completion; inspect native package state before retrying.".into()),
    }
}

/// Hidden narrow helper dispatch, before GUI startup. The request and its digest
/// are arguments, so elevation never writes through user-controlled file names.
///
/// # Errors
/// Returns malformed invocation errors; transaction failures use the protocol.
pub fn maybe_run_desktop_helper(args: &[String]) -> Result<Option<i32>, String> {
    if !args
        .iter()
        .any(|arg| arg == "--tilo-desktop-install-helper")
    {
        return Ok(None);
    }
    let [_, flag, request, digest] = args else {
        return Err("Malformed Desktop helper invocation.".into());
    };
    if flag != "--tilo-desktop-install-helper" {
        return Err("Malformed Desktop helper invocation.".into());
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Some(super::desktop_install::run_native_helper(
            request.as_bytes(),
            digest,
        )))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (request, digest);
        Err("Desktop helper requires Linux.".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::desktop_install::{DesktopInstallEvent, DesktopInstallRequest};
    use super::*;
    use std::process::{Command, Stdio};

    /// Elevation request, protocol validation, disconnection and log locator.
    #[test]
    fn desktop_worker_protocol_completes_only_on_exact_verified_exit() {
        // Phase `elevation_passes_only_exact_request_and_digest`.
        {
            let request = DesktopInstallRequest {
                schema: 1,
                release_id: "fedora".into(),
                package_input: "/tmp/package with spaces.rpm".into(),
            };
            let command =
                elevation_command(std::path::Path::new("/opt/tilo/imager"), &request).unwrap();
            assert_eq!(command.get_program(), "/usr/bin/pkexec");
            let args: Vec<_> = command.get_args().collect();
            assert_eq!(args.len(), 4);
            assert_eq!(args[0], "/opt/tilo/imager");
            assert_eq!(args[1], "--tilo-desktop-install-helper");
            assert_eq!(
                args[3],
                super::super::desktop_install::request_digest(args[2].to_str().unwrap().as_bytes())
                    .as_str()
            );
        }

        // Phase `malformed_or_nonzero_protocol_never_reports_complete`.
        {
            for script in [
                "printf '%s\\n' '{\"phase\":\"Complete\",\"detail\":{\"version\":\"1\",\"session_entry\":\"/usr/share/wayland-sessions/tilo.desktop\"}}'",
                "printf '%s\\n' '{\"phase\":\"Preparing\"}' '{\"phase\":\"Installing\"}' '{\"phase\":\"Verifying\"}' '{\"phase\":\"Complete\",\"detail\":{\"version\":\"1\",\"session_entry\":\"/usr/share/wayland-sessions/tilo.desktop\"}}'; exit 7",
                "printf '%s\\n' 'not json'",
            ] {
                let child = Command::new("/bin/sh")
                    .args(["-c", script])
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                let (tx, rx) = std::sync::mpsc::channel();
                assert!(track_helper(child, &tx).is_err());
                assert!(
                    !rx.try_iter()
                        .any(|event| matches!(event, DesktopInstallEvent::Complete(_)))
                );
            }
        }

        // Phase `disconnected_ui_does_not_kill_helper_and_terminal_exit_is_tracked`.
        {
            let directory = tempfile::tempdir().unwrap();
            let marker = directory.path().join("finished");
            let child = Command::new("/bin/sh").args(["-c", "printf '%s\\n' '{\"phase\":\"Preparing\"}' '{\"phase\":\"Installing\"}'; sleep 0.05; touch \"$1\"; printf '%s\\n' '{\"phase\":\"Verifying\"}' '{\"phase\":\"Failed\",\"detail\":\"fixture failure\"}'", "recording-helper"]).arg(&marker).stdout(Stdio::piped()).spawn().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            drop(rx);
            assert!(track_helper(child, &tx).is_err());
            assert!(marker.is_file());
        }

        // Phase `protected_log_locator_does_not_change_completion_order`.
        {
            let child = Command::new("/bin/sh").args(["-c", "printf '%s\\n' '{\"phase\":\"Preparing\"}' '{\"phase\":\"Installing\"}' '{\"phase\":\"DiagnosticLog\",\"detail\":\"/var/tmp/tilo-desktop-fixture/native.log\"}' '{\"phase\":\"Verifying\"}' '{\"phase\":\"Complete\",\"detail\":{\"version\":\"1\",\"session_entry\":\"/usr/share/wayland-sessions/tilo.desktop\"}}'"]).stdout(Stdio::piped()).spawn().unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            track_helper(child, &tx).unwrap();
            let events: Vec<_> = rx.try_iter().collect();
            assert!(matches!(
                events.get(2),
                Some(DesktopInstallEvent::DiagnosticLog(_))
            ));
            assert!(matches!(
                events.last(),
                Some(DesktopInstallEvent::Complete(_))
            ));
        }
    }
}
