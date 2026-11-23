//! What the session and an app keeper say on the keeper's control socket (`<dir>/control`).
//!
//! Every message is one JSON value in a frame with a little-endian length prefix ([`FRAMING`]).
//! The session speaks first, with [`ToKeeper::Hello`] naming [`INTERFACE_VERSION`]; a keeper at
//! another version answers [`FromKeeper::VersionMismatch`] and keeps running and displaying its
//! app. [`ToKeeper::Attach`] carries one descriptor beside its frame (`SCM_RIGHTS`): the session's
//! end of a fresh Wayland connection to the desktop.
//!
//! # The part that never changes
//!
//! A running keeper is never replaced by an update, so a session meets keepers of builds older and
//! newer than its own. Everything above is at [`INTERFACE_VERSION`], and a session at another
//! version speaks none of it. What follows is not versioned, and every build of the keeper and the
//! session keeps it exactly as written:
//!
//! - the first frame of a connection is a hello, read by [`hello_version`];
//! - a keeper at another version answers it with `{"version-mismatch":{"version":N}}`
//!   ([`mismatch_version`]), then the frame [`HOLDING`], and keeps the connection open: while a
//!   connection that said hello at another version stays open, its app is not ended for want of
//!   a desktop ([`crate::DESKTOP_GONE`]), as a session is there that cannot show it; the time counts
//!   from when the last such connection closed;
//! - `<dir>/app` holds the app's process once it started, as [`AppProcess`] in JSON, so a session
//!   that cannot speak to the keeper can still end exactly that process.
//!
//! A keeper from before this rule answers the mismatch and closes the connection: its app ends
//! once its desktop stayed away for [`crate::DESKTOP_GONE`].

use serde::{Deserialize, Serialize};

/// The one version of this interface (`crates/desktop/app-keeper/interface.toml`).
pub const INTERFACE_VERSION: u32 = 1;

/// The frames on the control socket.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(1024 * 1024);

/// What the session asks of a keeper.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToKeeper {
    /// The first message on every connection: the version the session speaks.
    Hello { version: u32 },
    /// Show the app on the desktop whose Wayland connection rides beside this frame.
    Attach,
    /// Stop showing the app: hold its windows until the next `Attach`.
    Detach,
    /// Answer with [`FromKeeper::Described`].
    Describe,
    /// Remember where the window `tag` stands, opaque to the keeper, to be read back later.
    SetPlacement { tag: String, placement: String },
    /// Send `signal` to the app's process group.
    End { signal: i32 },
}

/// What a keeper tells the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FromKeeper {
    /// The session's hello was at this keeper's version.
    Hello { version: u32 },
    /// The session's hello named another version than this keeper's, which it names; the
    /// connection closes.
    VersionMismatch { version: u32 },
    /// Everything the keeper holds, as asked by [`ToKeeper::Describe`].
    Described(Description),
    /// The app's windows changed (one opened or closed, a title, an app id or a placement).
    Windows { windows: Vec<Window> },
    /// The app ended.
    Ended { end: End },
    /// The keeper broke the desktop's protocol and holds the app's windows until the next
    /// `Attach`, which recreates them from scratch.
    Fault { fault: String },
}

/// One keeper and the app it keeps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    /// What the session named this launch on the keeper's command line, as given.
    pub launch: String,
    /// The app's process, once started.
    pub app_pid: Option<u32>,
    /// When that process started, in clock ticks since boot (`/proc/<pid>/stat`), so a pid the
    /// kernel reused is told apart.
    pub started_at: Option<u64>,
    /// The app's windows, oldest first.
    pub windows: Vec<Window>,
    /// Whether a desktop shows the windows now.
    pub attached: bool,
    /// How the app ended, once it did.
    pub ended: Option<End>,
    /// The last protocol fault, when the keeper had one.
    pub fault: Option<String>,
}

/// One of the app's windows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    /// Unique within the keeper, stable for the window's life (`w1`, `w2`, ...).
    pub tag: String,
    /// The app id the desktop is given, with the keeper's prefix.
    pub app_id: Option<String>,
    pub title: Option<String>,
    /// What the session last set with [`ToKeeper::SetPlacement`].
    pub placement: Option<String>,
}

/// How the app's process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum End {
    /// It exited with this status.
    Exited { code: i32 },
    /// This signal ended it; `memory` when the kernel ended it for memory (its cgroup counted an
    /// OOM kill while it ran).
    Signalled {
        signal: i32,
        #[serde(default)]
        memory: bool,
    },
}

/// The frame a keeper sends after its version mismatch when it holds its app for the session at
/// the other end (see the module's docs): the JSON string `"holding"`. Never changes.
pub const HOLDING: &str = "holding";

/// Whether `frame` is [`HOLDING`].
#[must_use]
pub fn holding(frame: &[u8]) -> bool {
    serde_json::from_slice::<String>(frame).is_ok_and(|said| said == HOLDING)
}

/// The version a keeper's version-mismatch answer names, read without the rest of
/// [`FromKeeper`], so a keeper at any version is understood. Never changes.
#[must_use]
pub fn mismatch_version(frame: &[u8]) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_slice(frame).ok()?;
    value
        .get("version-mismatch")?
        .get("version")?
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
}

/// The app's process, as a keeper writes it to `<dir>/app` once it started. Never changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppProcess {
    pub pid: u32,
    /// When it started, in clock ticks since boot (`/proc/<pid>/stat`).
    pub started_at: u64,
}

/// The version a first frame says hello with, read without the rest of [`ToKeeper`], so a
/// session at any version is answered.
#[must_use]
pub fn hello_version(frame: &[u8]) -> Option<u32> {
    let value: serde_json::Value = serde_json::from_slice(frame).ok()?;
    value
        .get("hello")?
        .get("version")?
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
}

/// One message as a frame.
///
/// # Errors
/// When the message does not fit [`FRAMING`].
pub fn encode<T: Serialize>(message: &T) -> std::io::Result<Vec<u8>> {
    let payload = serde_json::to_vec(message)?;
    FRAMING.encode(&payload).map_err(std::io::Error::other)
}

/// The frames complete in `buffer`, taken out of it; a partial frame stays.
///
/// # Errors
/// When a prefix announces a frame larger than [`FRAMING`] allows.
pub fn take_frames(buffer: &mut Vec<u8>) -> std::io::Result<Vec<Vec<u8>>> {
    let mut frames = Vec::new();
    while let Some(prefix) = buffer.first_chunk::<{ wire_frame::PREFIX_BYTES }>() {
        let len = FRAMING
            .decode_prefix(*prefix)
            .map_err(std::io::Error::other)?;
        let end = len.saturating_add(wire_frame::PREFIX_BYTES);
        if buffer.len() < end {
            break;
        }
        let frame: Vec<u8> = buffer.drain(..end).skip(wire_frame::PREFIX_BYTES).collect();
        frames.push(frame);
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hello_is_read_at_any_version_and_frames_split_where_they_end() {
        let hello = serde_json::to_vec(&ToKeeper::Hello { version: 7 }).unwrap_or_default();
        assert_eq!(hello_version(&hello), Some(7));
        assert_eq!(hello_version(br#"{"attach":null}"#), None);
        let mut buffer = encode(&ToKeeper::Describe).unwrap_or_default();
        buffer.extend(encode(&ToKeeper::Detach).unwrap_or_default());
        buffer.extend([9, 0]);
        let frames = take_frames(&mut buffer).unwrap_or_default();
        assert_eq!(frames.len(), 2);
        assert_eq!(buffer, vec![9, 0]);
        let first: Option<ToKeeper> = frames
            .first()
            .and_then(|frame| serde_json::from_slice(frame).ok());
        assert_eq!(first, Some(ToKeeper::Describe));
    }

    #[test]
    fn the_unversioned_frames_read_as_they_are_written() {
        let mismatch = encode(&FromKeeper::VersionMismatch { version: 4 }).unwrap_or_default();
        let mut buffer = mismatch;
        buffer.extend(encode(&HOLDING).unwrap_or_default());
        let frames = take_frames(&mut buffer).unwrap_or_default();
        assert_eq!(frames.first().and_then(|f| mismatch_version(f)), Some(4));
        assert!(frames.get(1).is_some_and(|f| holding(f)));
        assert!(!holding(br#"{"hello":{"version":1}}"#));
        let process = AppProcess {
            pid: 7,
            started_at: 9,
        };
        assert_eq!(
            serde_json::to_string(&process).unwrap_or_default(),
            r#"{"pid":7,"started_at":9}"#
        );
    }
}
