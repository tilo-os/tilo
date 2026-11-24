//! The names the person's devices go by, the agent id a client works as for one, and the line a
//! `--connect --device` client asks its own host to carry it to one of them with ([`Forward`]).

#[cfg(not(target_arch = "wasm32"))]
use std::io::{self, Read as _, Write as _};

use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use tilo_socket::Stream;
use tilo_user_identity::DeviceCert;

/// The first four bytes of a device key, as hex: enough to tell two devices
/// of one person with the same label apart.
fn short_id(key: &[u8; 32]) -> String {
    hex::encode(key.get(..4).unwrap_or_default())
}

/// The agent id a client admitted for a device's key works as: its own id
/// (`claude-code`), then `@` and the device (`claude-code@studio-laptop-3f2a9c1b`),
/// so what it does is told apart from what the same client does for another key.
#[must_use]
pub fn agent_id(client: &str, device: &DeviceCert) -> String {
    let mut slug = String::new();
    for character in device.device_label.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let client: String = client.chars().take(64).collect();
    let short = short_id(&device.device_public_key);
    if slug.is_empty() {
        format!("{client}@{short}")
    } else {
        format!("{client}@{slug}-{short}")
    }
}

/// The longest answer to a [`Forward`] read.
#[cfg(not(target_arch = "wasm32"))]
const MAX_ANSWER: usize = 64 * 1024;

/// What a host answers a [`Forward`] with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum ForwardAnswer {
    /// The device works with the agent: MCP follows.
    Open,
    /// It does not, and why.
    Refused { reason: String },
}

/// Tell a forwarding client that MCP follows on `local`, toward the device it named.
///
/// # Errors
/// The client left.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_forward(local: &mut Stream) -> io::Result<()> {
    let mut line = serde_json::to_vec(&ForwardAnswer::Open).map_err(io::Error::other)?;
    line.push(b'\n');
    local.write_all(&line)
}

/// Tell a forwarding client its device's server does not open, and why.
#[cfg(not(target_arch = "wasm32"))]
pub fn refuse_forward(mut local: Stream, reason: String) {
    let answer = ForwardAnswer::Refused { reason };
    if let Ok(mut line) = serde_json::to_vec(&answer) {
        line.push(b'\n');
        let _ = local.set_write_timeout(Some(std::time::Duration::from_secs(1)));
        let _ = local.write_all(&line);
    }
    let _ = local.shutdown(std::net::Shutdown::Both);
}

/// The longest device name the person is shown, and a `--device` is matched
/// against.
const MAX_NAME: usize = 64;

/// A device as the person reads it: its label, or the start of its key when
/// it has none.
#[must_use]
pub fn device_name(device: &DeviceCert) -> String {
    let label: String = device
        .device_label
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_NAME)
        .collect();
    if label.is_empty() {
        short_id(&device.device_public_key)
    } else {
        label
    }
}

/// What a `--connect --device` client asks of its own host: carry this
/// connection to that device's MCP server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    /// The device, by its name or its endpoint id.
    pub device: String,
    /// The workspace on it to work in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardLine {
    forward: Forward,
}

/// The forward a connection's first line asks for, if that is what it is.
#[must_use]
pub fn forward_request(line: &[u8]) -> Option<Forward> {
    serde_json::from_slice::<ForwardLine>(line)
        .ok()
        .map(|line| line.forward)
}

/// Ask the host at the other end of `stream` to carry it to the server
/// `forward` names, and read whether that server opened. From `Ok` on, the
/// stream is that server's.
#[cfg(not(target_arch = "wasm32"))]
pub fn ask_forward(stream: &mut Stream, forward: &Forward) -> io::Result<()> {
    let mut line = serde_json::to_vec(&ForwardLine {
        forward: forward.clone(),
    })
    .map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut answer = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte)? {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "this device's host does not carry agents to another device: it closed \
                         without answering for {}",
                        forward.device
                    ),
                ));
            }
            _ if byte == *b"\n" => break,
            _ if answer.len() >= MAX_ANSWER => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "this device's host answered with a line too long to read",
                ));
            }
            _ => answer.extend_from_slice(&byte),
        }
    }
    match serde_json::from_slice::<ForwardAnswer>(&answer) {
        Ok(ForwardAnswer::Open) => Ok(()),
        Ok(ForwardAnswer::Refused { reason }) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} did not let this agent in: {reason}", forward.device),
        )),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "this device's host does not carry agents to another device: it answered {}",
                String::from_utf8_lossy(&answer)
            ),
        )),
    }
}
