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
    mut input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    loop {
        let mut line = zeroize::Zeroizing::new(String::new());
        if input.read_line(&mut line)? == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let (response, is_binary) = handle_request(daemon, &line);
        let binary = if is_binary {
            response.result.as_ref()
        } else {
            None
        }
        .and_then(|value| {
            serde_json::from_value::<tilo_vm_protocol::LayerUploadBeginResult>(value.clone()).ok()
        });
        if let Err(error) = reply(&mut output, &response) {
            if let Some(binary) = binary {
                daemon.cancel_upload(&binary.upload);
            }
            return Err(error);
        }
        if let Some(binary) = binary {
            let (result, boundary) = daemon.receive_binary(&binary.upload, &mut input);
            if !boundary {
                return Err(result
                    .err()
                    .unwrap_or_else(|| std::io::Error::other("binary upload lost framing")));
            }
            let (result, error) = match result {
                Ok(received) => (Some(serde_json::json!({"received":received})), None),
                Err(_) => (
                    None,
                    Some(
                        DaemonError::InvalidParams(
                            "binary upload cancelled or failed integrity verification".into(),
                        )
                        .to_rpc(),
                    ),
                ),
            };
            if let Err(error) = reply(
                &mut output,
                &ResponseEnvelope {
                    jsonrpc: "2.0".into(),
                    id: response.id,
                    result,
                    error,
                },
            ) {
                daemon.cancel_upload(&binary.upload);
                return Err(error);
            }
        }
    }
    Ok(())
}

fn reply(output: &mut impl Write, response: &ResponseEnvelope) -> std::io::Result<()> {
    serde_json::to_writer(&mut *output, response).map_err(std::io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}

fn handle_request(daemon: &mut Daemon, line: &str) -> (ResponseEnvelope, bool) {
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
        Ok(envelope) => {
            let binary = matches!(
                &envelope.request,
                tilo_vm_protocol::Request::LayerUploadBinary(_)
            );
            (reply(id, daemon.handle(envelope.request)), binary)
        }
        Err(_) => (
            reply(
                id,
                Err(DaemonError::InvalidParams(
                    "invalid request encoding or arguments".into(),
                )),
            ),
            false,
        ),
    }
}

#[cfg(test)]
fn handle_line(daemon: &mut Daemon, line: &str) -> ResponseEnvelope {
    handle_request(daemon, line).0
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "wire fixtures assert the authenticated production RPC loop"
)]
mod binary_tests {
    use super::*;
    fn input() -> Vec<u8> {
        let mut bytes = Vec::new();
        for request in [
            serde_json::json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":tilo_vm_protocol::PROTOCOL_VERSION,"client_version":"fixture"}}),
            serde_json::json!({"jsonrpc":"2.0","id":2,"method":"layer.upload.binary","params":{}}),
        ] {
            serde_json::to_writer(&mut bytes, &request).unwrap();
            bytes.push(b'\n');
        }
        bytes
    }
    fn daemon(root: &std::path::Path) -> Daemon {
        Daemon::new(
            root,
            Box::new(crate::apply::RecordingApply::default()),
            None,
            tilo_vm_protocol::BaseInfo::default(),
        )
    }
    #[test]
    fn binary_bad_hash_cancel_and_disconnect_remove_partial_uploads() {
        for mode in ["hash", "cancel", "disconnect", "oversize"] {
            let dir = tempfile::tempdir().unwrap();
            let mut daemon = daemon(dir.path());
            let mut bytes = input();
            let mut body = Vec::new();
            let mut encoder = tilo_vm_protocol::binary::Encoder::new(&mut body);
            encoder.write_all(&vec![19; 2 << 20]).unwrap();
            if mode == "cancel" {
                encoder.cancel().unwrap();
            } else {
                encoder.finish().unwrap();
            }
            if mode == "hash" {
                *body.last_mut().unwrap() ^= 1;
            }
            if mode == "disconnect" {
                body.truncate(body.len().saturating_sub(10));
            }
            if mode == "oversize" {
                body = u32::MAX.saturating_sub(1).to_be_bytes().to_vec();
            }
            bytes.extend(body);
            if matches!(mode, "hash" | "cancel") {
                bytes.extend(
                    b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"layer.list\",\"params\":{}}\n",
                );
            }
            let mut output = Vec::new();
            let result = serve(&mut daemon, bytes.as_slice(), &mut output);
            assert_eq!(
                std::fs::read_dir(dir.path().join("uploads"))
                    .unwrap()
                    .count(),
                0
            );
            if matches!(mode, "hash" | "cancel") {
                result.unwrap();
                let responses: Vec<ResponseEnvelope> = output
                    .split(|byte| *byte == b'\n')
                    .filter(|line| !line.is_empty())
                    .map(|line| serde_json::from_slice(line).unwrap())
                    .collect();
                assert_eq!(responses.len(), 4);
                assert!(responses[2].error.is_some());
                assert_eq!(responses[3].id, 3);
            } else {
                assert!(result.is_err());
            }
        }
    }
    #[test]
    fn binary_upgrade_still_requires_the_matching_hello() {
        let dir = tempfile::tempdir().unwrap();
        let mut daemon = daemon(dir.path());
        let request =
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"layer.upload.binary\",\"params\":{}}";
        assert!(handle_line(&mut daemon, request).error.is_some());
        assert!(!dir.path().join("uploads").exists());
    }
}
