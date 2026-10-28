//! The protocol between Tilo and the agent inside a legacy VM guest.
//!
//! A legacy VM (`linux` or `windows`, one per user and guest OS) runs an agent
//! that listens on `AF_VSOCK` port [`AGENT_PORT`]. The host opens one
//! connection per operation:
//!
//! ```text
//! host  → Hello { protocol_major }        guest → Hello(AgentHello)
//! host  → Health | ListRunning | Mount | InstalledApps
//!                                         guest → one answer, then close
//! host  → Launch(Launch)                  guest → Started { pid }
//! host  → Stdin(..) | CloseStdin | Signal guest → Stdout(..) | Stderr(..)
//!                                         guest → Exited(ExitStatus), close
//! ```
//!
//! Every message is one [`FRAMING`] frame carrying a postcard value. The
//! guest is not trusted with anything the host did not already give it: the
//! agent reports, the host decides. Windows that a launched app opens never
//! cross this channel; they travel over `waypipe` to [`WAYPIPE_PORT`] (see
//! [`waypipe`]) and arrive at the compositor as ordinary Wayland clients,
//! named by [`window`].

#![allow(
    clippy::missing_errors_doc,
    reason = "errors are typed enums whose variants document themselves"
)]

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wire_frame::{FrameError, Framing};

pub mod client;
pub mod link;
#[cfg(target_os = "linux")]
pub mod vsock;
pub mod waypipe;
pub mod window;

/// Bumped on any incompatible change; both ends refuse a different major.
pub const PROTOCOL_MAJOR: u16 = 1;

/// The guest agent's vsock port.
pub const AGENT_PORT: u32 = 7110;

/// The host's vsock port for forwarded Wayland connections. One host-side
/// `waypipe client` per VM accepts every app's `waypipe server`.
pub const WAYPIPE_PORT: u32 = 7111;

/// `VMADDR_CID_HOST`: how a guest names the host.
pub const HOST_CID: u32 = 2;

/// The largest frame either side sends. Output is chunked well below it.
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

/// The largest stdio chunk carried by one frame.
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;

/// Little-endian length prefix, postcard payload, never empty (every message
/// has at least its variant tag).
pub const FRAMING: Framing = Framing::little_endian(MAX_FRAME_BYTES).reject_empty();

const MAX_ARGS: usize = 1024;
const MAX_ARGV_BYTES: usize = 256 * 1024;
const MAX_ENV: usize = 256;
const MAX_SHARES: usize = 64;
/// virtio-fs tags are at most 36 bytes.
const MAX_TAG_BYTES: usize = 36;

/// The guest operating system of a legacy VM.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum GuestOs {
    Linux,
    Windows,
}

impl GuestOs {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }

    /// What a person reads next to a legacy window's name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::Windows => "Windows",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "linux" => Some(Self::Linux),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// Host → guest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HostMessage {
    Hello {
        protocol_major: u16,
    },
    Health,
    ListRunning,
    /// The complete set of shares the guest should have mounted. Shares
    /// missing from it are unmounted.
    Mount {
        shares: Vec<GuestShare>,
    },
    InstalledApps,
    Launch(Launch),
    Stdin(Vec<u8>),
    CloseStdin,
    Signal(Signal),
}

/// Guest → host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GuestMessage {
    Hello(AgentHello),
    Health(Health),
    Running(Vec<RunningApp>),
    Mounted { shares: Vec<GuestShare> },
    InstalledApps(Vec<InstalledApp>),
    Started { pid: u32 },
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exited(ExitStatus),
    Error { message: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentHello {
    pub protocol_major: u16,
    pub os: GuestOs,
    /// Changes on every guest boot. A resumed snapshot keeps it.
    pub boot_id: String,
    pub agent_version: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub uptime_secs: u64,
    pub mem_total_kib: u64,
    pub mem_available_kib: u64,
    /// Apps started through this agent that are still running.
    pub running: u32,
    /// Of those, the ones whose windows are forwarded to the host.
    pub windowed: u32,
}

/// One app started through the agent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunningApp {
    pub pid: u32,
    pub argv: Vec<String>,
    pub windowed: bool,
    pub started_unix: u64,
}

/// An app the guest has installed (a Flatpak export), as its desktop entry
/// describes it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstalledApp {
    /// Desktop-file id without `.desktop`, e.g. `org.gnome.Calculator`.
    pub id: String,
    pub name: String,
    /// The Wayland app id or X11 class the app's windows report.
    pub window_class: Option<String>,
    pub comment: Option<String>,
    pub terminal: bool,
}

/// One host directory exposed to the guest at the same path.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct GuestShare {
    /// The virtio-fs tag the host attached the share under.
    pub tag: String,
    pub path: String,
    pub read_only: bool,
}

/// Start `argv` in the guest as the guest's user.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    pub argv: Vec<String>,
    /// Absolute guest path; the guest user's home when absent.
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub stdio: Stdio,
    pub display: Display,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Stdio {
    /// stdin, stdout and stderr travel over this connection.
    Relay,
    /// The app gets `/dev/null`; the connection only reports its exit.
    Null,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Display {
    /// No windows (a command-line tool).
    None,
    /// Run under a `waypipe server` connected to the host; X11 clients reach
    /// the guest's Xwayland through the same forwarder.
    Forwarded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Signal {
    Interrupt,
    Terminate,
    Kill,
}

impl Signal {
    #[must_use]
    pub const fn number(self) -> i32 {
        match self {
            Self::Interrupt => libc::SIGINT,
            Self::Terminate => libc::SIGTERM,
            Self::Kill => libc::SIGKILL,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExitStatus {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitStatus {
    /// The status a shell would report: the code, or 128 + the signal.
    #[must_use]
    pub fn shell_code(self) -> i32 {
        match (self.code, self.signal) {
            (Some(code), _) => code,
            (None, Some(signal)) => signal.saturating_add(128),
            (None, None) => 1,
        }
    }
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("agent connection: {0}")]
    Io(#[from] io::Error),
    #[error("agent frame: {0}")]
    Frame(#[from] FrameError),
    #[error("agent closed the connection")]
    Closed,
    #[error("agent speaks protocol {actual}, host speaks {expected}")]
    Version { expected: u16, actual: u16 },
    #[error("agent sent an unexpected message")]
    Unexpected,
    #[error("agent refused: {0}")]
    Refused(String),
    #[error("invalid {0}")]
    Invalid(&'static str),
}

pub fn send<W: Write + ?Sized, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> Result<(), ProtocolError> {
    FRAMING.write_value(writer, message)?;
    writer.flush()?;
    Ok(())
}

/// Read one message; `None` when the peer closed between messages.
pub fn recv<R: Read + ?Sized, T: serde::de::DeserializeOwned>(
    reader: &mut R,
) -> Result<Option<T>, ProtocolError> {
    Ok(FRAMING.read_value(reader)?)
}

impl Launch {
    /// Shape checks both ends apply before acting on a launch.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.argv.is_empty() || self.argv.len() > MAX_ARGS {
            return Err(ProtocolError::Invalid("argv length"));
        }
        if self.argv.first().is_some_and(String::is_empty) {
            return Err(ProtocolError::Invalid("empty program"));
        }
        let bytes: usize = self.argv.iter().map(String::len).sum();
        if bytes > MAX_ARGV_BYTES || self.argv.iter().any(|arg| arg.contains('\0')) {
            return Err(ProtocolError::Invalid("argv"));
        }
        if let Some(cwd) = &self.cwd
            && (!Path::new(cwd).is_absolute() || cwd.contains('\0'))
        {
            return Err(ProtocolError::Invalid("cwd"));
        }
        if self.env.len() > MAX_ENV
            || self
                .env
                .iter()
                .any(|(name, value)| !valid_env_name(name) || value.contains('\0'))
        {
            return Err(ProtocolError::Invalid("environment"));
        }
        Ok(())
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Guest paths a share may never cover: the guest's own system.
const GUEST_SYSTEM_DIRS: &[&str] = &[
    "/usr", "/etc", "/proc", "/sys", "/dev", "/run", "/boot", "/var/lib", "/sysroot", "/ostree",
    "/bin", "/sbin", "/lib", "/lib64",
];

impl GuestShare {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.tag.is_empty()
            || self.tag.len() > MAX_TAG_BYTES
            || !self
                .tag
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(ProtocolError::Invalid("share tag"));
        }
        if !share_path_allowed(Path::new(&self.path)) {
            return Err(ProtocolError::Invalid("share path"));
        }
        Ok(())
    }

    /// Validates a whole desired set: each share, and no duplicate tag or path.
    pub fn validate_set(shares: &[Self]) -> Result<(), ProtocolError> {
        if shares.len() > MAX_SHARES {
            return Err(ProtocolError::Invalid("share count"));
        }
        for (index, share) in shares.iter().enumerate() {
            share.validate()?;
            if shares
                .iter()
                .skip(index.saturating_add(1))
                .any(|other| other.tag == share.tag || other.path == share.path)
            {
                return Err(ProtocolError::Invalid("duplicate share"));
            }
        }
        Ok(())
    }
}

/// An absolute, normalised path outside the guest's own system directories.
#[must_use]
pub fn share_path_allowed(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    if !path.is_absolute() || text == "/" || text.contains('\0') || text.ends_with('/') {
        return false;
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return false;
    }
    !GUEST_SYSTEM_DIRS
        .iter()
        .any(|system| path == Path::new(system) || path.starts_with(system))
}

/// A connection both directions of which can be used from different threads.
pub trait Duplex: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> io::Result<Self>;
    fn shutdown_write(&self) -> io::Result<()>;
    fn shutdown_both(&self) -> io::Result<()>;
}

impl Duplex for UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        Self::try_clone(self)
    }
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Write)
    }
    fn shutdown_both(&self) -> io::Result<()> {
        self.shutdown(std::net::Shutdown::Both)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(argv: &[&str]) -> Launch {
        Launch {
            argv: argv.iter().map(ToString::to_string).collect(),
            cwd: Some("/var/home/you/src".into()),
            env: vec![("LANG".into(), "C.UTF-8".into())],
            stdio: Stdio::Relay,
            display: Display::Forwarded,
        }
    }

    #[test]
    fn messages_round_trip_and_reject_malformed_requests() {
        // phase: every message survives the wire unchanged
        {
            let messages = vec![
                HostMessage::Hello {
                    protocol_major: PROTOCOL_MAJOR,
                },
                HostMessage::Launch(launch(&["code", "."])),
                HostMessage::Stdin(b"input".to_vec()),
                HostMessage::Signal(Signal::Terminate),
                HostMessage::Mount {
                    shares: vec![GuestShare {
                        tag: "fs-0123".into(),
                        path: "/var/home/you/src".into(),
                        read_only: false,
                    }],
                },
            ];
            let mut wire = Vec::new();
            for message in &messages {
                send(&mut wire, message).unwrap();
            }
            let mut reader = wire.as_slice();
            for message in &messages {
                assert_eq!(
                    &recv::<_, HostMessage>(&mut reader).unwrap().unwrap(),
                    message
                );
            }
            assert!(recv::<_, HostMessage>(&mut reader).unwrap().is_none());
        }
        // phase: launches are bounded and well-formed
        {
            assert!(launch(&["code", "."]).validate().is_ok());
            assert!(launch(&[]).validate().is_err());
            assert!(launch(&[""]).validate().is_err());
            assert!(launch(&["a\0b"]).validate().is_err());
            let mut relative = launch(&["ls"]);
            relative.cwd = Some("src".into());
            assert!(relative.validate().is_err());
            let mut bad_env = launch(&["ls"]);
            bad_env.env = vec![("1X".into(), String::new())];
            assert!(bad_env.validate().is_err());
        }
        // phase: shares never reach the guest's own system
        {
            let share = |tag: &str, path: &str| GuestShare {
                tag: tag.into(),
                path: path.into(),
                read_only: true,
            };
            assert!(share("fs-1", "/var/home/you/Documents").validate().is_ok());
            for path in [
                "/",
                "/usr",
                "/usr/share",
                "/etc/x",
                "relative",
                "/a/../b",
                "/a/",
            ] {
                assert!(share("fs-1", path).validate().is_err(), "{path}");
            }
            assert!(share("Upper", "/srv/x").validate().is_err());
            assert!(share(&"a".repeat(37), "/srv/x").validate().is_err());
            assert!(
                GuestShare::validate_set(&[share("fs-1", "/srv/a"), share("fs-1", "/srv/b")])
                    .is_err()
            );
            assert!(
                GuestShare::validate_set(&[share("fs-1", "/srv/a"), share("fs-2", "/srv/b")])
                    .is_ok()
            );
        }
        // phase: exit status reads like a shell's
        {
            let exited = |code, signal| ExitStatus { code, signal };
            assert_eq!(exited(Some(3), None).shell_code(), 3);
            assert_eq!(exited(None, Some(9)).shell_code(), 137);
            assert_eq!(GuestOs::parse("linux"), Some(GuestOs::Linux));
            assert_eq!(GuestOs::parse("Linux"), None);
        }
    }
}
