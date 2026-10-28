//! `AirPlay` sessions, driven through an external sender process.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use tilo_projector::control::{ConnectStage, Credential};
use tilo_projector::{DisplayStreamPolicy, ProjectionDevice, ProjectionOptions};

use super::context::SessionContext;
use super::receiver_address;
use crate::error::ProjectorError;

/// How often a running sender is checked for having exited. Output and stop
/// requests wake the session at once; only an exit that leaves the sender's
/// pipes open (a capture child holding them) waits for this tick.
const SENDER_EXIT_CHECK: Duration = Duration::from_millis(500);

pub(crate) fn run_airplay_session(
    device: &ProjectionDevice,
    options: &ProjectionOptions,
    credential: Option<Credential>,
    context: &SessionContext,
) -> Result<(), ProjectorError> {
    let address = receiver_address(device)?;
    let credentials = airplay_credentials_path()?;
    let command_args = airplay_command_args(
        address,
        device.port,
        options.audio,
        options.stream_policy,
        &credentials,
    );
    let pairing_credential = credential.filter(|value| !value.expose().is_empty());
    run_airplay_process(
        &context.helpers.doubletake,
        &command_args,
        context,
        pairing_credential.as_ref().map(Credential::expose),
    )
}

fn airplay_credentials_path() -> Result<PathBuf, ProjectorError> {
    airplay_credentials_path_from(
        std::env::var_os("TILO_PROJECTOR_STATE_DIR"),
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn airplay_credentials_path_from(
    override_root: Option<std::ffi::OsString>,
    xdg_state_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, ProjectorError> {
    let state_root = override_root
        .map(PathBuf::from)
        .or_else(|| {
            tilo_paths::xdg::Base::State
                .resolve_from(xdg_state_home, home)
                .map(|dir| dir.join("tilo-projector"))
        })
        .ok_or_else(|| {
            ProjectorError::Unavailable(
                "cannot resolve persistent AirPlay pairing storage".to_owned(),
            )
        })?;
    fs::create_dir_all(&state_root)
        .map_err(ProjectorError::io("cannot create AirPlay pairing storage"))?;
    fs::set_permissions(&state_root, fs::Permissions::from_mode(0o700))
        .map_err(ProjectorError::io("cannot secure AirPlay pairing storage"))?;
    Ok(state_root.join("airplay-credentials.json"))
}

fn airplay_command_args(
    address: &str,
    port: u16,
    audio: bool,
    stream_policy: DisplayStreamPolicy,
    credentials: &Path,
) -> Vec<String> {
    let profile = stream_policy.ceiling(None);
    let mut args = vec![
        "-target".to_owned(),
        address.trim_matches(['[', ']']).to_owned(),
        "-port".to_owned(),
        port.to_string(),
        "-fps".to_owned(),
        profile.fps.to_string(),
        "-bitrate".to_owned(),
        profile.bitrate_kbps.to_string(),
        "-hwaccel".to_owned(),
        "openh264".to_owned(),
        "-creds".to_owned(),
        credentials.display().to_string(),
    ];
    if !audio {
        args.push("-no-audio".to_owned());
    }
    args
}

fn run_airplay_process(
    executable: &str,
    args: &[String],
    context: &SessionContext,
    pairing_credential: Option<&str>,
) -> Result<(), ProjectorError> {
    run_external_process(
        executable,
        args,
        context,
        &["mirror session ready"],
        "AirPlay",
        // Doubletake reads the code only from its environment. This one
        // sender process is the only place the credential is an environment
        // variable: the broker and the shell carry it in-band.
        pairing_credential.map(|value| ("DOUBLETAKE_CODE", value)),
    )
}

/// What the session loop wakes for.
enum SenderOutput {
    /// A line of sender output, and whether it says the stream is up.
    Line { ready: bool, text: String },
    /// The service asked the session to stop.
    Stop,
}

fn run_external_process(
    executable: &str,
    args: &[String],
    context: &SessionContext,
    ready_messages: &[&str],
    transport_name: &str,
    credential_env: Option<(&str, &str)>,
) -> Result<(), ProjectorError> {
    let mut command = Command::new(executable);
    command
        .args(args)
        // Keep the sender and its GStreamer capture children in one group so
        // Disconnect cannot strand a camera/audio/capture process.
        .process_group(0)
        // Only the selected sender gets the explicitly supplied credential.
        // Ambient codes must not pair a receiver or reach an unrelated helper.
        .env_remove("TILO_PROJECTION_CREDENTIAL")
        .env_remove("DOUBLETAKE_CODE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((name, value)) = credential_env {
        command.env(name, value);
    }
    context.stage(ConnectStage::Handshake);
    let mut child = command.spawn().map_err(ProjectorError::io(format!(
        "cannot start {transport_name} sender"
    )))?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ProjectorError::Unavailable(format!("{transport_name} sender has no status stream"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProjectorError::Unavailable(format!("{transport_name} sender has no diagnostic stream"))
    })?;
    let (line_tx, line_rx) = mpsc::channel();
    let stdout_tx = line_tx.clone();
    let stop_tx = line_tx.clone();
    let secret = credential_env.map(|(_, value)| value.to_owned());
    let stdout_secret = secret.clone();
    let ready_messages = ready_messages
        .iter()
        .map(|s| (*s).to_owned())
        .collect::<Vec<_>>();
    let stdout_ready = ready_messages.clone();
    let stdout_reader = std::thread::Builder::new()
        .name("tilo-projection-status-out".to_owned())
        .spawn(move || {
            forward_sender_diagnostics(stdout, &stdout_tx, stdout_secret.as_deref(), &stdout_ready);
        })
        .map_err(ProjectorError::io(format!(
            "cannot monitor {transport_name} sender output"
        )))?;
    let stderr_reader = std::thread::Builder::new()
        .name("tilo-projection-status-err".to_owned())
        .spawn(move || {
            forward_sender_diagnostics(stderr, &line_tx, secret.as_deref(), &ready_messages);
        })
        .map_err(ProjectorError::io(format!(
            "cannot monitor {transport_name} diagnostics"
        )))?;
    // A stop request lands in the same channel as the sender's output, so
    // the loop below wakes for it at once.
    context.control().on_stop(move || {
        let _ = stop_tx.send(SenderOutput::Stop);
    });
    let mut diagnostics = Vec::new();
    let mut ready = false;
    loop {
        match line_rx.recv_timeout(SENDER_EXIT_CHECK) {
            Ok(SenderOutput::Line {
                ready: is_ready,
                text,
            }) => {
                if is_ready && !ready {
                    context.ready();
                    ready = true;
                }
                keep_tail(&mut diagnostics, text);
                continue;
            }
            Ok(SenderOutput::Stop) => {
                bounded_command::terminate_tree(&mut child, Duration::from_secs(1));
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Ok(());
            }
            Err(_) => {}
        }
        if let Some(status) = child.try_wait().map_err(ProjectorError::io(format!(
            "cannot inspect {transport_name} sender"
        )))? {
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            while let Ok(output) = line_rx.try_recv() {
                if let SenderOutput::Line { text, .. } = output {
                    keep_tail(&mut diagnostics, text);
                }
            }
            let detail = diagnostics.join("\n");
            return Err(ProjectorError::Receiver(if detail.is_empty() {
                format!("{transport_name} sender exited unexpectedly ({status})")
            } else if ready {
                format!("{transport_name} stream ended ({status}): {detail}")
            } else {
                detail
            }));
        }
    }
}

/// Keeps the last eight diagnostic lines: enough to explain a failure.
fn keep_tail(diagnostics: &mut Vec<String>, line: String) {
    diagnostics.push(line);
    if diagnostics.len() > 8 {
        diagnostics.remove(0);
    }
}

// Redact before queueing: helper diagnostics may repeat a rejected PIN or
// password on either stream, and later become user-visible session errors.
fn forward_sender_diagnostics(
    stream: impl Read,
    lines: &mpsc::Sender<SenderOutput>,
    credential: Option<&str>,
    ready_messages: &[String],
) {
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        // Classify before redaction: a password may coincide with a status word.
        let ready = ready_messages.iter().any(|message| line.contains(message));
        let text = match credential.filter(|value| !value.is_empty()) {
            Some(secret) => line.replace(secret, "[redacted]"),
            None => line,
        };
        if lines.send(SenderOutput::Line { ready, text }).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    use tilo_projector::DisplayResolutionLimit;
    use tilo_projector::control::ProjectorEvent;

    use crate::session::Helpers;
    use crate::session::paths::SessionPaths;

    fn fixture(name: &str) -> (PathBuf, SessionContext, Receiver<ProjectorEvent>) {
        let root = std::env::temp_dir().join(format!(
            "tilo-projector-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let paths = SessionPaths::for_root(root.clone()).expect("session paths");
        let (context, events) = SessionContext::for_test(paths, Helpers::from_env());
        (root, context, events)
    }

    fn write_sender(path: &Path, script: &[u8]) {
        fs::write(path, script).expect("write sender");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("make sender executable");
    }

    /// Waits for the session to report the receiver connected.
    fn connected(events: &Receiver<ProjectorEvent>) -> bool {
        let deadline = tilo_test_env::hang_deadline();
        while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
            match events.recv_timeout(remaining) {
                Ok(ProjectorEvent::Connected { .. }) => return true,
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    #[test]
    fn rejected_airplay_credential_is_private_and_can_be_retried() {
        let (root, context, events) = fixture("auth");
        let sender = root.join("sender");
        write_sender(
            &sender,
            b"#!/bin/sh\n[ -z \"$TILO_PROJECTION_CREDENTIAL\" ] || exit 8\n[ \"$DOUBLETAKE_CODE\" = \"receiver-password\" ] || exit 9\necho \"rejected code: $DOUBLETAKE_CODE\"\necho \"authentication failed: $DOUBLETAKE_CODE\" >&2\nexit 1\n",
        );
        let error = run_airplay_process(
            &sender.display().to_string(),
            &[],
            &context,
            Some("receiver-password"),
        )
        .expect_err("rejected authentication must fail")
        .to_string();
        assert!(!error.contains("receiver-password"));
        assert!(error.contains("rejected code: [redacted]"));
        assert!(error.contains("authentication failed: [redacted]"));
        assert!(
            !events
                .try_iter()
                .any(|event| matches!(event, ProjectorEvent::Connected { .. })),
            "rejection must not report streaming"
        );

        // A new explicitly supplied code can retry the same session. Readiness
        // is still reported when the sender includes that code in its message.
        write_sender(
            &sender,
            b"#!/bin/sh\n[ \"$DOUBLETAKE_CODE\" = \"mirror session ready\" ] || exit 9\necho \"mirror session ready: $DOUBLETAKE_CODE\"\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
        );
        let (root_retry, context, events) = fixture("auth-retry");
        let worker = context.clone();
        let sender_path = sender.display().to_string();
        let session = std::thread::spawn(move || {
            run_airplay_process(&sender_path, &[], &worker, Some("mirror session ready"))
        });
        let ready = connected(&events);
        context.control().request_stop(None);
        session
            .join()
            .expect("join retry")
            .expect("disconnect retry");
        assert!(ready, "authenticated retry must report readiness");
        fs::remove_dir_all(root).expect("remove fixture");
        fs::remove_dir_all(root_retry).expect("remove retry fixture");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn airplay_senders_run_without_hardware() {
        {
            let (root, context, events) = fixture("airplay");
            let sender = root.join("airplay-sender-simulator");
            write_sender(
                &sender,
                b"#!/bin/sh\n[ \"$DOUBLETAKE_CODE\" = \"4821\" ] || exit 7\ntrap 'exit 0' TERM INT\necho 'screen capture started' >&2\necho 'mirror session ready (data port: 7001)' >&2\nwhile :; do sleep 1; done\n",
            );
            let worker = context.clone();
            let sender_path = sender.display().to_string();
            let session = std::thread::spawn(move || {
                run_airplay_process(&sender_path, &[], &worker, Some("4821"))
            });
            assert!(connected(&events), "AirPlay readiness was not published");
            context.control().request_stop(None);
            session
                .join()
                .expect("join AirPlay session")
                .expect("clean AirPlay disconnect");
            fs::remove_dir_all(root).expect("remove AirPlay fixture directory");
        }
        {
            let root = std::env::temp_dir().join(format!(
                "tilo-projector-state-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let path =
                airplay_credentials_path_from(Some(root.clone().into_os_string()), None, None)
                    .expect("persistent credential path");
            assert_eq!(path, root.join("airplay-credentials.json"));
            assert_eq!(
                fs::metadata(&root)
                    .expect("state metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            fs::remove_dir_all(root).expect("remove state fixture");
        }
        {
            let airplay = airplay_command_args(
                "[2001:db8::7]",
                7000,
                false,
                DisplayStreamPolicy {
                    adaptive: true,
                    resolution_limit: DisplayResolutionLimit::P2160,
                    max_fps: 60,
                },
                Path::new("/state/pairing.json"),
            );
            assert!(airplay.windows(2).any(|a| a == ["-target", "2001:db8::7"]));
            assert!(airplay.windows(2).any(|a| a == ["-port", "7000"]));
            assert!(airplay.windows(2).any(|a| a == ["-hwaccel", "openh264"]));
            assert!(airplay.iter().any(|a| a == "-no-audio"));
        }
    }
}
