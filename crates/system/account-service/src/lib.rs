//! The accounts service: what a sandboxed app may ask of an online account
//! without ever holding its credential.
//!
//! The service runs outside every sandbox (`tilo-account-service`, in
//! account-custody) and holds the account vault. An app declares
//! `accounts = "required@1"` in `[sandbox.services]`; the launcher connects
//! the service's socket for it, names the app in a first frame
//! ([`handoff`]) before the app holds the descriptor, and hands the
//! descriptor over as `TILO_SVC_ACCOUNTS_FD`. The app therefore cannot claim
//! to be another app, and never reaches the network or the vault itself:
//! it names an account and an operation, and gets back the outcome.
//!
//! The one operation so far files a forge issue for the feedback triage
//! agent. Filing is idempotent by a marker the caller chooses (the approved
//! draft's id): the issue carries it ([`marker_line`]), and the service looks
//! for an issue that carries it before it files a new one, so a caller that
//! lost the answer and asks again gets the issue it already filed.

use std::io::Write as _;
use std::os::fd::{FromRawFd as _, RawFd};
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};

/// The service's manifest name, and its socket's name in the session's
/// service directory.
pub const SERVICE_NAME: &str = "accounts";
/// Where the launcher tells the app its descriptor is.
pub const SERVICE_FD_ENV: &str = "TILO_SVC_ACCOUNTS_FD";
/// Version of the launcher's [`handoff`] frame.
pub const HANDOFF_PROTOCOL: u16 = 1;
/// The handoff: a little-endian `u32` length, then JSON.
pub const HANDOFF_FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(4 * 1024);
/// Every request and response: a little-endian `u32` length, then JSON.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(64 * 1024);
/// Version carried by every request frame.
pub const RPC_VERSION: u16 = 1;

/// The apps that may file forge issues: the feedback triage agent.
pub const FORGE_APPS: &[&str] = &["so.tilo.agent"];

/// Longest issue title the service files.
pub const MAX_TITLE_BYTES: usize = 256;
/// Longest issue body the service files.
pub const MAX_BODY_BYTES: usize = 32 * 1024;

/// An issue to file. `marker` makes filing idempotent: the filed issue
/// carries it, and an issue that already carries it is returned instead of
/// filing another.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeIssue {
    pub title: String,
    pub body: String,
    pub marker: String,
}

impl ForgeIssue {
    /// Whether the issue fits what the service files.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.title.trim().is_empty()
            && self.title.len() <= MAX_TITLE_BYTES
            && !self.title.contains(['\n', '\r'])
            && self.body.len() <= MAX_BODY_BYTES
            && valid_marker(&self.marker)
    }

    /// The body as filed: the caller's text and the marker line.
    #[must_use]
    pub fn filed_body(&self) -> String {
        format!("{}\n\n{}", self.body.trim_end(), marker_line(&self.marker))
    }
}

/// Where an issue was filed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiledIssue {
    pub number: u64,
    /// The issue's page on the forge.
    pub url: String,
}

/// Whether `marker` may mark an issue: 1–96 of `[A-Za-z0-9_.:-]`.
#[must_use]
pub fn valid_marker(marker: &str) -> bool {
    !marker.is_empty()
        && marker.len() <= 96
        && marker
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// The line that marks an issue as filed for `marker`. Invisible in the
/// rendered issue, and exact, so a search for it finds only that issue.
#[must_use]
pub fn marker_line(marker: &str) -> String {
    format!("<!-- tilo-filed-for: {marker} -->")
}

/// One request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// The issue already filed for `marker` in forge account `account`'s
    /// repository, if there is one.
    FindForgeIssue { account: String, marker: String },
    /// File `issue` in forge account `account`'s repository, unless an
    /// issue carrying its marker exists; then that one is the answer.
    FileForgeIssue { account: String, issue: ForgeIssue },
}

impl Request {
    /// Whether `app_id` may make this request.
    #[must_use]
    pub fn permitted_for(&self, app_id: &str) -> bool {
        match self {
            Request::FindForgeIssue { .. } | Request::FileForgeIssue { .. } => {
                FORGE_APPS.contains(&app_id)
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Frame<T> {
    pub version: u16,
    #[serde(flatten)]
    pub value: T,
}

/// One answer. `issue` is the filed or found issue.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<FiledIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    #[must_use]
    pub fn issue(issue: Option<FiledIssue>) -> Self {
        Self {
            ok: true,
            issue,
            error: None,
        }
    }

    #[must_use]
    pub fn refused(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            issue: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ClientError {
    #[error("the accounts service is unavailable")]
    Unavailable,
    #[error("accounts service protocol error: {0}")]
    Protocol(String),
    #[error("{0}")]
    Refused(String),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HandoffFrame {
    protocol: u16,
    principal: String,
    app_id: String,
}

/// The launcher's first frame on an app's connection, naming the app.
///
/// # Errors
///
/// When the frame cannot be encoded.
pub fn handoff(app_id: &str) -> Result<Vec<u8>, ClientError> {
    serde_json::to_vec(&HandoffFrame {
        protocol: HANDOFF_PROTOCOL,
        principal: "app".into(),
        app_id: app_id.into(),
    })
    .map_err(|error| ClientError::Protocol(error.to_string()))
}

/// The app a handoff names.
///
/// # Errors
///
/// When the frame is malformed, of another version, or names no app.
pub fn app_of_handoff(body: &[u8]) -> Result<String, ClientError> {
    let frame: HandoffFrame = serde_json::from_slice(body)
        .map_err(|_| ClientError::Protocol("malformed accounts handoff".into()))?;
    if frame.protocol != HANDOFF_PROTOCOL || frame.principal != "app" || frame.app_id.is_empty() {
        return Err(ClientError::Protocol(
            "accounts handoff names no app of this protocol".into(),
        ));
    }
    Ok(frame.app_id)
}

/// Write one frame.
///
/// # Errors
///
/// When the value cannot be encoded or the stream fails.
pub fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), ClientError> {
    let body =
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?;
    FRAMING.write(stream, &body).map_err(|error| match error {
        wire_frame::FrameError::Io(_) => ClientError::Unavailable,
        _ => ClientError::Protocol("frame exceeds the accounts service limit".into()),
    })?;
    stream.flush().map_err(|_| ClientError::Unavailable)
}

/// Read one frame; `None` when the peer closed the stream.
///
/// # Errors
///
/// When the stream fails or the frame does not decode.
pub fn read_frame<T: for<'a> Deserialize<'a>>(
    stream: &mut UnixStream,
) -> Result<Option<T>, ClientError> {
    let body = match FRAMING.read(stream) {
        Ok(Some(body)) => body,
        Ok(None) => return Ok(None),
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err(ClientError::Protocol(
                "frame exceeds the accounts service limit".into(),
            ));
        }
        Err(_) => return Err(ClientError::Unavailable),
    };
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|_| ClientError::Protocol("invalid accounts service frame".into()))
}

/// An app's connection to the accounts service.
pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// The descriptor the launcher handed this app, when it did.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let fd = std::env::var(SERVICE_FD_ENV)
            .ok()
            .and_then(|value| value.parse::<RawFd>().ok())
            // SAFETY: fcntl only checks whether the descriptor is open.
            .filter(|fd| *fd >= 3 && unsafe { libc::fcntl(*fd, libc::F_GETFD) } != -1)?;
        // SAFETY: duplicates a checked open descriptor; the duplicate is
        // owned by the stream from here, and the app's own copy stays open.
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        (duplicate >= 0).then(|| Self {
            // SAFETY: `duplicate` is a fresh descriptor nothing else owns.
            stream: unsafe { UnixStream::from_raw_fd(duplicate) },
        })
    }

    /// A client over a connection whose handoff was already written, for a
    /// host that connected the service itself (a test harness).
    #[must_use]
    pub fn from_stream(stream: UnixStream) -> Self {
        Self { stream }
    }

    fn request(&mut self, request: Request) -> Result<Option<FiledIssue>, ClientError> {
        write_frame(
            &mut self.stream,
            &Frame {
                version: RPC_VERSION,
                value: request,
            },
        )?;
        let response: Response = read_frame(&mut self.stream)?.ok_or(ClientError::Unavailable)?;
        if response.ok {
            Ok(response.issue)
        } else {
            Err(ClientError::Refused(
                response.error.unwrap_or_else(|| "refused".into()),
            ))
        }
    }

    /// The issue filed for `marker` in `account`'s repository, if any.
    ///
    /// # Errors
    ///
    /// When the service is unreachable or refuses.
    pub fn find_forge_issue(
        &mut self,
        account: &str,
        marker: &str,
    ) -> Result<Option<FiledIssue>, ClientError> {
        self.request(Request::FindForgeIssue {
            account: account.into(),
            marker: marker.into(),
        })
    }

    /// File `issue` in `account`'s repository, or return the issue already
    /// filed for its marker.
    ///
    /// # Errors
    ///
    /// When the service is unreachable or refuses.
    pub fn file_forge_issue(
        &mut self,
        account: &str,
        issue: &ForgeIssue,
    ) -> Result<FiledIssue, ClientError> {
        self.request(Request::FileForgeIssue {
            account: account.into(),
            issue: issue.clone(),
        })?
        .ok_or_else(|| ClientError::Protocol("the service filed nothing".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_named_apps_file_and_a_marker_is_exact() {
        let file = Request::FileForgeIssue {
            account: "forge".into(),
            issue: ForgeIssue {
                title: "t".into(),
                body: "b".into(),
                marker: "m_draft".into(),
            },
        };
        assert!(file.permitted_for("so.tilo.agent"));
        assert!(!file.permitted_for("so.tilo.notes"));
        assert_eq!(
            app_of_handoff(&handoff("so.tilo.agent").unwrap()).unwrap(),
            "so.tilo.agent"
        );
        assert!(app_of_handoff(br#"{"protocol":1,"principal":"session","app_id":""}"#).is_err());
        assert!(valid_marker("room:m_draft-1"));
        assert!(!valid_marker("-->"));
        let issue = ForgeIssue {
            title: "t".into(),
            body: "b\n".into(),
            marker: "m_1".into(),
        };
        assert!(issue.filed_body().ends_with("<!-- tilo-filed-for: m_1 -->"));
        assert!(
            !ForgeIssue {
                title: "a\nb".into(),
                ..issue
            }
            .is_valid()
        );
    }
}
