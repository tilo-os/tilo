//! What the shell and Settings say to the NTFS mount service, and its answers.
//!
//! One JSON request per connection, one JSON answer back, each a single line.
//! The service decides who may ask for what from the kernel's account of the
//! caller (see the service's `peer` module): opening a drive is reserved to
//! the shell, because only the shell draws the consent prompt, while listing,
//! closing and forgetting drives is open to the person at the seat.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where the service listens.
pub const SOCKET_PATH: &str = "/run/tilo-ntfs-mount/control.sock";

/// Bumped when a request or answer changes shape incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

/// The longest line either side accepts.
pub const MAX_LINE_BYTES: u64 = 256 * 1024;

/// How a drive is opened. Never named after the driver behind it: the person
/// chooses between speed and isolation, not between kernel modules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// "Fastest. For your own drives." The kernel's own NTFS driver.
    Open,
    /// "For drives you don't know. A bit slower." A confined userspace
    /// driver, so a hostile file system reaches a sandbox, not the kernel.
    OpenSafely,
}

/// Where a drive sits, as far as the person needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Built into this computer and present since it started.
    Internal,
    /// Anything else: USB, SD cards, Thunderbolt enclosures, a disk that
    /// appeared after start-up.
    External,
}

/// Why a drive opened read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyReason {
    /// Windows is hibernated or kept the drive for Fast Startup.
    WindowsHibernated,
    /// Windows did not finish writing its journal: it was not shut down.
    WindowsNotShutDown,
    /// Windows marked the drive for a disk check.
    NeedsCheck,
}

/// What a "Remember" in the prompt may keep for a drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RememberOffer {
    /// Nothing: the drive shows no identifier to recognise it by.
    Nothing,
    /// Only that it opens safely, and it is still asked about: an external
    /// drive, or an internal one missing an identifier.
    OpenSafely,
    /// Either way of opening, without asking again: an internal drive.
    AnyMode,
}

/// A remembered choice that applies to a drive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    /// The grant's id, which Settings uses to forget it or change its mode.
    pub grant: String,
    pub mode: Mode,
}

/// A drive that is open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mounted {
    pub path: String,
    pub mode: Mode,
    pub read_only: Option<ReadOnlyReason>,
}

/// One NTFS volume the service can see right now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drive {
    /// Names this volume for as long as it stays attached. A drive that is
    /// unplugged and plugged in again gets a new id.
    pub id: String,
    pub label: String,
    pub size_bytes: u64,
    pub placement: Placement,
    /// Appeared after this computer started: the shell asks about it as it
    /// arrives instead of waiting for the person to open it.
    pub hot_added: bool,
    pub remembered: Option<Remembered>,
    /// Whether opening it needs the person's answer first.
    pub needs_consent: bool,
    /// The choice the prompt offers first. Never a reason to skip it.
    pub preselect: Mode,
    /// What "Remember" may keep for this drive.
    pub can_remember: RememberOffer,
    pub mounted: Option<Mounted>,
}

/// A remembered drive, as Settings lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedDrive {
    pub grant: String,
    pub label: String,
    pub placement: Placement,
    pub mode: Mode,
    /// Where it is open now, if it is.
    pub mounted: Option<String>,
}

/// The person's answer in the shell's consent prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    pub mode: Mode,
    pub remember: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Every NTFS volume attached now, as the caller would be asked about it.
    Drives,
    /// Open `drive`. Without `consent` it opens only a drive whose remembered
    /// choice applies, and otherwise answers [`Response::NeedsConsent`].
    Open {
        drive: String,
        consent: Option<Consent>,
    },
    /// Close a drive the caller opened.
    Close { drive: String },
    /// The caller's remembered drives.
    Remembered,
    /// Close the drive if it is open and delete the remembered choice.
    Forget { grant: String },
    /// Change how a remembered drive opens, from its next opening on.
    SetMode { grant: String, mode: Mode },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol: u32,
    pub request: Request,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Drives {
        drives: Vec<Drive>,
    },
    Opened {
        mounted: Mounted,
    },
    NeedsConsent {
        drive: Drive,
    },
    Remembered {
        drives: Vec<RememberedDrive>,
    },
    Done,
    /// The caller may not ask this.
    Refused {
        reason: String,
    },
    /// The request was allowed and could not be carried out.
    Failed {
        message: String,
    },
}

/// Send one request to the service at `socket` and wait for its answer.
///
/// `timeout` bounds each read and write; opening a drive checks the volume
/// first, so callers that open give it tens of seconds.
///
/// # Errors
///
/// Returns the I/O error if the service is unreachable, does not answer in
/// time, or answers with something that is not a [`Response`].
pub fn call(socket: &Path, request: Request, timeout: Duration) -> io::Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(&Envelope {
        protocol: PROTOCOL_VERSION,
        request,
    })
    .map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut answer = String::new();
    BufReader::new(stream)
        .take(MAX_LINE_BYTES)
        .read_line(&mut answer)?;
    serde_json::from_str(&answer).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_answers_name_modes_not_drivers() {
        let line = serde_json::to_string(&Envelope {
            protocol: PROTOCOL_VERSION,
            request: Request::Open {
                drive: "sdb1:7".into(),
                consent: Some(Consent {
                    mode: Mode::OpenSafely,
                    remember: true,
                }),
            },
        })
        .unwrap();
        assert!(line.contains(r#""op":"open""#));
        assert!(line.contains(r#""mode":"open_safely""#));
        assert!(!line.contains("ntfs3") && !line.contains("ntfs-3g"));
        let back: Envelope = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back.request,
            Request::Open {
                consent: Some(_),
                ..
            }
        ));
    }
}
