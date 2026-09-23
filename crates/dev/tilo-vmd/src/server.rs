//! The newline-delimited JSON-RPC loop over a byte stream.

use std::io::{BufRead, Write};

use tilo_vm_protocol::{RequestEnvelope, ResponseEnvelope};

use crate::daemon::Daemon;
use crate::error::DaemonError;

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
        let line = line?;
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
    let value: serde_json::Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => return reply(0, Err(DaemonError::InvalidParams(format!("not JSON: {e}")))),
    };
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    match serde_json::from_value::<RequestEnvelope>(value) {
        Ok(envelope) => reply(id, daemon.handle(envelope.request)),
        Err(e) => reply(id, Err(DaemonError::InvalidParams(e.to_string()))),
    }
}
