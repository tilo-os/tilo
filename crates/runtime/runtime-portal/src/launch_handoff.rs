//! What a process says first on its connection to its session's services:
//! the app and the room its launch was for.
//!
//! The connection is already the process's own — the launcher connected it,
//! or a surface app connected the socket its session named — and the broker
//! on the other end admits it only for a launch the session recorded for
//! that very process (`/proc` credentials and start time included). The
//! handoff therefore grants nothing by itself: it names which of the
//! launch's records this connection is, so a launcher-written statement can
//! never stand for a process the session did not start.

use std::io::Write;
use std::os::unix::net::UnixStream;

use wire_frame::Framing;

/// Length-prefixed launcher handoff consumed before the wire starts on a
/// collaboration descriptor.
pub const PROTOCOL_VERSION: u16 = 1;

/// Prefix that separates an authenticated connection from a raw diagnostic
/// client. It is deliberately not an authority: the broker still verifies
/// the peer process against the session's launch record before it accepts
/// the following frame.
pub const MAGIC: [u8; 4] = *b"QCL1";

/// After [`MAGIC`]: a little-endian `u32` length, then the JSON handoff. It
/// is deliberately tiny: it only binds a socket to the session's
/// already-created process record, while the certificate the app presents
/// still decides workspace membership and delegated-agent authority.
pub const FRAMING: Framing = Framing::little_endian(1024).reject_empty();

/// Name the app and room a collaboration connection is for, as the first
/// thing on it: what the broker attests against the session's launch record.
///
/// # Errors
/// The stream refuses the write.
pub fn write(stream: &mut UnixStream, app_id: &str, workspace_id: &str) -> std::io::Result<()> {
    write_launch(
        stream,
        &serde_json::json!({
            "protocol": PROTOCOL_VERSION,
            "app_id": app_id,
            "workspace_id": workspace_id,
        }),
    )
}

/// Name the app, room and window instance a collaboration connection the
/// session makes for one of its windows is for: the broker attests it
/// against the launch record of exactly that window, so two windows of one
/// app in one room never take each other's.
///
/// # Errors
/// The stream refuses the write.
pub fn write_window(
    stream: &mut UnixStream,
    app_id: &str,
    workspace_id: &str,
    window_instance: u64,
) -> std::io::Result<()> {
    write_launch(
        stream,
        &serde_json::json!({
            "protocol": PROTOCOL_VERSION,
            "app_id": app_id,
            "workspace_id": workspace_id,
            "window_instance": window_instance,
        }),
    )
}

/// Request exact executing-task custody on an existing collaboration
/// connection: the handoff names the launch as [`write`] does, and the
/// broker then binds the connection to the process executing the task
/// ([`crate::process_binding`]). Hosts without the custody protocol take it
/// as the plain handoff.
///
/// # Errors
/// The stream refuses the write.
pub fn write_executing(
    stream: &mut UnixStream,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    return write(stream, app_id, workspace_id);
    #[cfg(target_os = "linux")]
    {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "protocol": PROTOCOL_VERSION,
            "app_id": app_id,
            "workspace_id": workspace_id,
            "executing_peer": true,
        }))
        .map_err(std::io::Error::other)?;
        stream.write_all(&MAGIC)?;
        FRAMING.write(stream, &bytes)?;
        stream.flush()
    }
}

fn write_launch(stream: &mut UnixStream, value: &serde_json::Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let frame = FRAMING.encode(&bytes)?;
    stream.write_all(&MAGIC)?;
    stream.write_all(&frame)?;
    stream.flush()
}
