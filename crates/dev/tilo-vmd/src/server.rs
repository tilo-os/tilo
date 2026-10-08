//! The newline-delimited JSON-RPC loop over a byte stream.

use std::io::{BufRead, Write};

use tilo_vm_protocol::{RequestEnvelope, ResponseEnvelope};

use crate::daemon::Daemon;
use crate::error::DaemonError;

#[derive(serde::Deserialize)]
struct Header {
    id: u64,
}

/// Serve requests until the input closes.
///
/// # Errors
/// Only transport failures; request failures become error responses.
pub fn serve(
    daemon: &mut Daemon,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = zeroize::Zeroizing::new(line?);
        if line.trim().is_empty() {
            continue;
        }
        let response = handle_line(daemon, &line);
        serde_json::to_writer(&mut output, &response).map_err(std::io::Error::other)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

fn handle_line(daemon: &mut Daemon, line: &str) -> ResponseEnvelope {
    let reply = |id, outcome: Result<serde_json::Value, DaemonError>| match outcome {
        Ok(result) => ResponseEnvelope {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => ResponseEnvelope {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(error.to_rpc()),
        },
    };
    let id = serde_json::from_str::<Header>(line).map_or(0, |header| header.id);
    match serde_json::from_str::<RequestEnvelope>(line) {
        Ok(envelope) => reply(id, daemon.handle(envelope.request)),
        Err(_) => reply(
            id,
            Err(DaemonError::InvalidParams(
                "invalid request encoding or arguments".into(),
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::RecordingApply;
    use tilo_vm_protocol::BaseInfo;

    #[test]
    fn enrollment_semantic_rpc_refuses_a_session_without_actual_owner_authority() {
        let root = tempfile::tempdir().unwrap();
        let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut apply = RecordingApply::with_login("fixture-account");
        apply.mirror = Some(mirror.clone());
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        let hello = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":tilo_vm_protocol::PROTOCOL_VERSION,"client_version":"fixture"}});
        assert!(handle_line(&mut daemon, &hello.to_string()).error.is_none());
        let args = serde_json::json!({"passphrase":"private-fixture-answer","confirm":true});
        let line = serde_json::json!({"op":"actions_invoke","provider":"device-unlock","id":"enroll@device-unlock","args":args}).to_string();
        let envelope = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"device.actions","params":{"line":line}});
        let response = handle_line(&mut daemon, &envelope.to_string());
        assert!(response.error.is_some());
        assert!(
            !serde_json::to_string(&response)
                .unwrap()
                .contains("private-fixture-answer")
        );
        assert!(
            mirror
                .lock()
                .unwrap()
                .iter()
                .all(|entry| !entry.contains("private-fixture-answer")
                    && !entry.contains("enroll_device_unlock"))
        );
    }

    #[test]
    fn malformed_private_arguments_are_not_reflected_in_rpc_errors() {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(
            root.path(),
            Box::new(RecordingApply::default()),
            None,
            BaseInfo::default(),
        );
        for line in [
            r#"{"jsonrpc":"2.0","id":12,"method":"device.unlock.enrol","params":{"passphrase":"private-fixture-answer","allow_unsealed":"private-fixture-answer"}}"#,
            r#"{"jsonrpc":"2.0","id":12,"method":"device.actions","params":{"line":false,"private-fixture-answer":1}}"#,
        ] {
            let response = handle_line(&mut daemon, line);
            assert_eq!(response.id, 12);
            assert!(response.error.is_some());
            assert!(
                !serde_json::to_string(&response)
                    .unwrap()
                    .contains("private-fixture-answer")
            );
        }
    }
}
