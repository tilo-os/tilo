//! The session services a launcher connects on an app's behalf, and what it says first on each
//! connection, before the app holds it.
//!
//! Each service listens on a socket of the session's own (`$XDG_RUNTIME_DIR/tilo/services/<name>.sock`).
//! Whatever starts the app (the launcher in a login, `tilolang dev` in a developer's session)
//! connects it, names the app it is for, and hands the app its end as the descriptor
//! [`fd_env`] names; [`crate::service_fd`] is the app's side of that.

use std::io::{Read, Write};

use wire_frame::Framing;

/// The session shell's authenticated file chooser (`tilo_dialogs::shell`), served alongside any
/// desktop portal.
pub const CHOOSER: &str = "chooser";
/// The person's clipboard, served by the session to the apps it starts, in either language: they
/// run with no display of their own, so the selection is not theirs to offer or read.
pub const CLIPBOARD: &str = "clipboard";
/// Hosted model planning is performed by this per-user, desktop-owned gateway.
pub const MODEL_GATEWAY: &str = "model-gateway";
/// The person's messaging accounts (`tilo-messaging`).
pub const MESSAGING: &str = "messaging";
/// Notifications, served by the session's notifications daemon: an app shows, replaces, withdraws
/// and schedules them here, and what the person does with one comes back on the same connection.
pub const NOTIFY: &str = "notify";

/// The version of the chooser handshake.
const CHOOSER_PROTOCOL: u16 = 1;

/// What a service that answers each app by its id reads first ([`MODEL_GATEWAY`], [`MESSAGING`]):
/// a little-endian `u32` length, then JSON.
pub const APP_ID_FRAMING: Framing = Framing::little_endian(4 * 1024);

/// The environment variable an app finds the descriptor of service `name` in:
/// `TILO_SVC_<NAME>_FD`.
#[must_use]
pub fn fd_env(name: &str) -> String {
    format!("TILO_SVC_{}_FD", name.to_uppercase())
}

/// The first line on a chooser connection: the app and the workspace it runs in, read by the
/// shell before anything the app sends.
///
/// # Errors
/// The handshake could not be encoded.
pub fn chooser_handshake(app_id: &str, workspace_id: &str) -> serde_json::Result<Vec<u8>> {
    let mut frame = serde_json::to_vec(&serde_json::json!({
        "protocol": CHOOSER_PROTOCOL,
        "app_id": app_id,
        "workspace_id": workspace_id,
    }))?;
    frame.push(b'\n');
    Ok(frame)
}

/// Name the app and workspace a chooser connection is for, as the first thing on it.
///
/// # Errors
/// The handshake could not be written.
pub fn write_chooser_handshake(
    stream: &mut impl Write,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    let frame = chooser_handshake(app_id, workspace_id)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    stream.write_all(&frame)?;
    stream.flush()
}

/// Name the app a connection to a service that answers each app by its id is for, as the first
/// thing on it.
///
/// # Errors
/// The app id could not be written.
pub fn write_app_id(stream: &mut impl Write, app_id: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "app_id": app_id,
    }))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    APP_ID_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

/// The app a connection to a service that answers each app by its id is for, as the launcher named
/// it ([`write_app_id`]).
///
/// # Errors
/// The handshake could not be read, or names no app.
pub fn read_app_id(stream: &mut impl Read) -> std::io::Result<String> {
    let frame = APP_ID_FRAMING
        .read(stream)
        .map_err(std::io::Error::from)?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the connection ended before it named its app",
            )
        })?;
    let handshake: serde_json::Value = serde_json::from_slice(&frame)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    handshake
        .get("app_id")
        .and_then(serde_json::Value::as_str)
        .filter(|app_id| !app_id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the connection names no app",
            )
        })
}
