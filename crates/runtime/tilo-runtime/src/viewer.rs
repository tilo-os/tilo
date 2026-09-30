//! Partner viewer invitations: short-lived, role-scoped invitations into the
//! room a device host beside this runtime governs.
//!
//! A hosting provider's dashboard asks for one when a person opens the
//! computer. `watch` issues a `viewer` invitation, `control` an `editor`
//! invitation; the browser redeems it on the host's public origin as
//! `#/join/<invitation>`. The device host stays the only collaboration
//! authority: this module calls its loopback operator route and keeps a small
//! ledger of the invitations it issued, so it can only ever revoke, re-role or
//! expire those, never the session's own membership.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

/// The public HTTPS origin browsers open, e.g. `https://c1.viewer.example`.
pub const ORIGIN_ENV: &str = "TILO_RUNTIME_VIEWER_ORIGIN";
/// The ready file the hosting process writes once it serves.
pub const HOST_READY_ENV: &str = "TILO_RUNTIME_VIEWER_HOST_READY";

const MIN_TTL_SECONDS: u64 = 60;
const MAX_TTL_SECONDS: u64 = 900;
const MAX_LABEL_CHARS: usize = 80;
const DEFAULT_LABEL: &str = "Viewer";
const MAX_LEDGER_ENTRIES: usize = 256;
const HOST_TIMEOUT: Duration = Duration::from_secs(10);
/// How often redeemed viewers past their deadline are removed.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Why a viewer request failed, with the HTTP status it answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerError {
    pub status: u16,
    pub message: String,
}

impl ViewerError {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
    fn unavailable(message: impl Into<String>) -> Self {
        Self::new(503, message)
    }
}

/// The device host's collaboration administration, as its governing authority.
pub trait HostOperator: Send + Sync {
    /// Run one collaboration action (`invitation.issue`, `invitation.list`, …).
    ///
    /// # Errors
    ///
    /// Returns the host's refusal (409), a malformed request (400) or an
    /// unavailable host (503).
    fn call(&self, action: &str, params: Option<Value>) -> Result<Value, ViewerError>;
}

/// The host's loopback operator route, located through its ready file on
/// every call so a restarted host on a new port is found again.
pub struct LoopbackHost {
    ready: PathBuf,
}

impl LoopbackHost {
    #[must_use]
    pub const fn new(ready: PathBuf) -> Self {
        Self { ready }
    }

    fn locate(&self) -> Result<(String, String), ViewerError> {
        let unavailable = |error: &dyn std::fmt::Display| {
            ViewerError::unavailable(format!("viewer host unavailable: {error}"))
        };
        let bytes = std::fs::read(&self.ready).map_err(|error| unavailable(&error))?;
        let ready: Value = serde_json::from_slice(&bytes).map_err(|error| unavailable(&error))?;
        let address = text(&ready, "fixture_origin")
            .and_then(|origin| origin.strip_prefix("http://"))
            .ok_or_else(|| unavailable(&"ready file names no loopback origin"))?
            .to_owned();
        let state = text(&ready, "state_root")
            .ok_or_else(|| unavailable(&"ready file names no state root"))?;
        let capability = std::fs::read_to_string(Path::new(state).join("enrollment-capability"))
            .map_err(|error| unavailable(&error))?;
        Ok((address, capability.trim().to_owned()))
    }
}

impl HostOperator for LoopbackHost {
    fn call(&self, action: &str, params: Option<Value>) -> Result<Value, ViewerError> {
        let (address, capability) = self.locate()?;
        let body = json!({"action": action, "params": params}).to_string();
        let exchange = || -> io::Result<Vec<u8>> {
            let target = address
                .parse::<std::net::SocketAddr>()
                .map_err(io::Error::other)?;
            if !target.ip().is_loopback() {
                return Err(io::Error::other("viewer host is not on loopback"));
            }
            let mut stream = TcpStream::connect_timeout(&target, HOST_TIMEOUT)?;
            stream.set_read_timeout(Some(HOST_TIMEOUT))?;
            stream.set_write_timeout(Some(HOST_TIMEOUT))?;
            write!(
                stream,
                "POST /admin/collaboration HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {capability}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )?;
            let mut response = Vec::new();
            stream.take(1024 * 1024).read_to_end(&mut response)?;
            Ok(response)
        };
        let response = exchange().map_err(|error| {
            ViewerError::unavailable(format!("viewer host unavailable: {error}"))
        })?;
        let response = String::from_utf8_lossy(&response);
        let (head, body) = response
            .split_once("\r\n\r\n")
            .ok_or_else(|| ViewerError::unavailable("viewer host answered no HTTP response"))?;
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| ViewerError::unavailable("viewer host answered no status"))?;
        let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        if status == 200 {
            return Ok(value);
        }
        let message = text(&value, "error")
            .unwrap_or("viewer host refused the request")
            .to_owned();
        Err(ViewerError::new(
            if status == 409 { 409 } else { 503 },
            message,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Watch,
    Control,
}

impl Mode {
    const fn role(self) -> &'static str {
        match self {
            Self::Watch => "viewer",
            Self::Control => "editor",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueRequest {
    mode: Mode,
    ttl_seconds: u64,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleRequest {
    mode: Mode,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenewRequest {
    ttl_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    invite_id: String,
    mode: Mode,
    expires_at_ms: u64,
    /// When a redeemed viewer is removed: the invitation's expiry plus its
    /// lifetime, moved later by renewal.
    remove_at_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Ledger {
    invitations: Vec<Entry>,
}

/// Viewer invitation administration for one computer.
pub struct Viewer {
    origin: String,
    host: Box<dyn HostOperator>,
    ledger_path: PathBuf,
    ledger: Mutex<()>,
}

impl Viewer {
    /// Configure from [`ORIGIN_ENV`] and [`HOST_READY_ENV`]; `None` unless
    /// both are set. The ledger lives in the private directory `state`.
    ///
    /// # Errors
    ///
    /// Rejects an origin that is not an exact `https://` origin.
    pub fn from_env(state: &Path) -> io::Result<Option<Self>> {
        let (Some(origin), Some(ready)) = (
            std::env::var_os(ORIGIN_ENV),
            std::env::var_os(HOST_READY_ENV),
        ) else {
            return Ok(None);
        };
        let origin = origin
            .into_string()
            .map_err(|_| io::Error::other(format!("{ORIGIN_ENV} must be UTF-8")))?;
        Self::new(
            origin,
            Box::new(LoopbackHost::new(PathBuf::from(ready))),
            state.join("viewer-invitations.json"),
        )
        .map(Some)
    }

    /// # Errors
    ///
    /// Rejects an origin that is not an exact `https://` origin.
    pub fn new(
        origin: String,
        host: Box<dyn HostOperator>,
        ledger_path: PathBuf,
    ) -> io::Result<Self> {
        let valid = origin.strip_prefix("https://").is_some_and(|authority| {
            !authority.is_empty()
                && !authority.contains(['/', '?', '#', '@'])
                && !authority.chars().any(char::is_whitespace)
        });
        if !valid {
            return Err(io::Error::other(format!(
                "{ORIGIN_ENV} must be an exact https:// origin"
            )));
        }
        Ok(Self {
            origin,
            host,
            ledger_path,
            ledger: Mutex::new(()),
        })
    }

    /// `POST /viewer/invitations`.
    ///
    /// # Errors
    ///
    /// 400 for a malformed body, else the host's refusal or unavailability.
    pub fn issue(&self, body: &[u8]) -> Result<Value, ViewerError> {
        let request: IssueRequest = parse(body)?;
        let ttl = ttl(request.ttl_seconds)?;
        let label = match request.label {
            None => DEFAULT_LABEL.to_owned(),
            Some(label) => {
                let label = label.trim();
                if label.is_empty()
                    || label.chars().count() > MAX_LABEL_CHARS
                    || label.chars().any(char::is_control)
                {
                    return Err(ViewerError::new(
                        400,
                        "label must be 1-80 printable characters",
                    ));
                }
                label.to_owned()
            }
        };
        let _turn = self.turn()?;
        let mut ledger = self.load()?;
        if ledger.invitations.len() >= MAX_LEDGER_ENTRIES {
            return Err(ViewerError::new(429, "too many live viewer invitations"));
        }
        let issued = self.host.call(
            "invitation.issue",
            Some(json!({
                "role": request.mode.role(),
                "expires_in_seconds": ttl,
                "invitee_name": label,
            })),
        )?;
        let (Some(invitation), Some(invite_id), Some(expires_at_ms)) = (
            text(&issued, "invite"),
            text(&issued, "invite_id"),
            issued.get("expires_at_ms").and_then(Value::as_u64),
        ) else {
            return Err(ViewerError::unavailable(
                "viewer host answered a malformed invitation",
            ));
        };
        ledger.invitations.push(Entry {
            invite_id: invite_id.to_owned(),
            mode: request.mode,
            expires_at_ms,
            remove_at_ms: expires_at_ms.saturating_add(ttl.saturating_mul(1000)),
        });
        self.store(&ledger)?;
        Ok(json!({
            "origin": self.origin,
            "invitation": invitation,
            "invite_id": invite_id,
            "role": request.mode.role(),
            "expires_at": rfc3339(expires_at_ms),
        }))
    }

    /// `DELETE /viewer/invitations/<id>`: cancel an unredeemed invitation, or
    /// remove the person who redeemed it.
    ///
    /// # Errors
    ///
    /// 404 for an invitation this endpoint did not issue.
    pub fn revoke(&self, invite_id: &str) -> Result<Value, ViewerError> {
        let _turn = self.turn()?;
        let mut ledger = self.load()?;
        let index = find(&ledger, invite_id)?;
        let outcome = self.end(invite_id)?;
        ledger.invitations.remove(index);
        self.store(&ledger)?;
        Ok(json!({"invite_id": invite_id, "status": outcome}))
    }

    /// `POST /viewer/invitations/<id>/role {"mode": …}` for a redeemed invitation.
    ///
    /// # Errors
    ///
    /// 404 for an unknown invitation, 409 before redemption.
    pub fn set_role(&self, invite_id: &str, body: &[u8]) -> Result<Value, ViewerError> {
        let request: RoleRequest = parse(body)?;
        let _turn = self.turn()?;
        let mut ledger = self.load()?;
        let index = find(&ledger, invite_id)?;
        let participant = match self.status(invite_id)? {
            (status, Some(participant)) if status == "redeemed" => participant,
            _ => {
                return Err(ViewerError::new(
                    409,
                    "the invitation has not been redeemed; issue a new one instead",
                ));
            }
        };
        self.host.call(
            "member.role.set",
            Some(json!({"participant_id": participant, "role": request.mode.role()})),
        )?;
        if let Some(entry) = ledger.invitations.get_mut(index) {
            entry.mode = request.mode;
        }
        self.store(&ledger)?;
        Ok(json!({"invite_id": invite_id, "role": request.mode.role()}))
    }

    /// `POST /viewer/invitations/<id>/renew {"ttl_seconds": …}`: keep a redeemed
    /// viewer at least `ttl_seconds` longer.
    ///
    /// # Errors
    ///
    /// 404 for an unknown invitation, 400 for a malformed body.
    pub fn renew(&self, invite_id: &str, body: &[u8], now_ms: u64) -> Result<Value, ViewerError> {
        let request: RenewRequest = parse(body)?;
        let ttl = ttl(request.ttl_seconds)?;
        let _turn = self.turn()?;
        let mut ledger = self.load()?;
        let index = find(&ledger, invite_id)?;
        let remove_at_ms = {
            let Some(entry) = ledger.invitations.get_mut(index) else {
                return Err(ViewerError::new(404, "unknown viewer invitation"));
            };
            entry.remove_at_ms = entry
                .remove_at_ms
                .max(now_ms.saturating_add(ttl.saturating_mul(1000)));
            entry.remove_at_ms
        };
        self.store(&ledger)?;
        Ok(json!({"invite_id": invite_id, "remove_at": rfc3339(remove_at_ms)}))
    }

    /// Remove every viewer past its deadline. Entries stay for a retry while
    /// the host is unavailable.
    ///
    /// # Errors
    ///
    /// Returns ledger I/O errors.
    pub fn sweep(&self, now_ms: u64) -> Result<usize, ViewerError> {
        let _turn = self.turn()?;
        let mut ledger = self.load()?;
        let before = ledger.invitations.len();
        let mut kept = Vec::with_capacity(before);
        for entry in std::mem::take(&mut ledger.invitations) {
            if entry.remove_at_ms > now_ms {
                kept.push(entry);
                continue;
            }
            match self.end(&entry.invite_id) {
                Ok(_) => {}
                Err(error) if error.status == 409 => {}
                Err(error) => {
                    tracing::warn!(invite = %entry.invite_id, error = %error.message, "viewer expiry deferred");
                    kept.push(entry);
                }
            }
        }
        ledger.invitations = kept;
        self.store(&ledger)?;
        Ok(before.saturating_sub(ledger.invitations.len()))
    }

    /// Cancel a pending invitation or remove the member who redeemed it.
    fn end(&self, invite_id: &str) -> Result<&'static str, ViewerError> {
        match self.status(invite_id)? {
            (status, Some(participant)) if status == "redeemed" => {
                self.host.call(
                    "member.remove",
                    Some(json!({"participant_id": participant})),
                )?;
                Ok("removed")
            }
            (status, _) if status == "pending" => {
                self.host
                    .call("invitation.cancel", Some(json!({"invite_id": invite_id})))?;
                Ok("revoked")
            }
            _ => Ok("revoked"),
        }
    }

    fn status(&self, invite_id: &str) -> Result<(String, Option<String>), ViewerError> {
        let listed = self.host.call("invitation.list", None)?;
        let entry = listed
            .get("invitations")
            .and_then(Value::as_array)
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| text(entry, "id") == Some(invite_id))
            })
            .ok_or_else(|| ViewerError::new(409, "the host no longer lists this invitation"))?;
        Ok((
            text(entry, "status").unwrap_or_default().to_owned(),
            text(entry, "participant_id").map(str::to_owned),
        ))
    }

    fn turn(&self) -> Result<std::sync::MutexGuard<'_, ()>, ViewerError> {
        self.ledger
            .lock()
            .map_err(|_| ViewerError::unavailable("viewer ledger unavailable"))
    }

    fn load(&self) -> Result<Ledger, ViewerError> {
        match std::fs::read(&self.ledger_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| ViewerError::unavailable(format!("viewer ledger: {error}"))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Ledger::default()),
            Err(error) => Err(ViewerError::unavailable(format!("viewer ledger: {error}"))),
        }
    }

    fn store(&self, ledger: &Ledger) -> Result<(), ViewerError> {
        let bytes = serde_json::to_vec(ledger)
            .map_err(|error| ViewerError::unavailable(format!("viewer ledger: {error}")))?;
        atomic_file::write_with_mode(&self.ledger_path, &bytes, 0o600)
            .map_err(|error| ViewerError::unavailable(format!("viewer ledger: {error}")))
    }
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn parse<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ViewerError> {
    serde_json::from_slice(body).map_err(|error| ViewerError::new(400, error.to_string()))
}

fn ttl(seconds: u64) -> Result<u64, ViewerError> {
    if (MIN_TTL_SECONDS..=MAX_TTL_SECONDS).contains(&seconds) {
        Ok(seconds)
    } else {
        Err(ViewerError::new(400, "ttl_seconds must be 60..900"))
    }
}

fn find(ledger: &Ledger, invite_id: &str) -> Result<usize, ViewerError> {
    ledger
        .invitations
        .iter()
        .position(|entry| entry.invite_id == invite_id)
        .ok_or_else(|| ViewerError::new(404, "unknown viewer invitation"))
}

/// [`Viewer::from_env`] with its expiry sweeper running: what an MCP
/// endpoint passes to [`crate::mcp_http::Endpoint::with_viewer`]. The ledger
/// lives in the private directory `state`.
///
/// # Errors
///
/// Rejects an invalid origin, or an app runtime that cannot start.
pub fn start_from_env(state: &Path) -> io::Result<Option<std::sync::Arc<Viewer>>> {
    let Some(viewer) = Viewer::from_env(state)? else {
        return Ok(None);
    };
    let viewer = std::sync::Arc::new(viewer);
    spawn_sweeper(std::sync::Arc::clone(&viewer))?;
    Ok(Some(viewer))
}

/// Remove expired viewers every [`SWEEP_INTERVAL`] on the app runtime.
///
/// # Errors
///
/// Returns an error when the app runtime cannot start.
pub fn spawn_sweeper(viewer: std::sync::Arc<Viewer>) -> io::Result<()> {
    let runtime = tilo_rt::try_app().map_err(io::Error::other)?;
    runtime.spawn(async move {
        let mut interval = tokio::time::interval(SWEEP_INTERVAL);
        loop {
            interval.tick().await;
            let viewer = std::sync::Arc::clone(&viewer);
            match tokio::task::spawn_blocking(move || viewer.sweep(now_ms())).await {
                Ok(Ok(0)) => {}
                Ok(Ok(removed)) => tracing::info!(removed, "expired viewers removed"),
                Ok(Err(error)) => tracing::warn!(error = %error.message, "viewer sweep failed"),
                Err(error) => tracing::warn!(%error, "viewer sweep stopped"),
            }
        }
    });
    Ok(())
}

/// Milliseconds since the epoch.
#[must_use]
pub fn now_ms() -> u64 {
    tilo_clock::unix_now_ms()
}

/// `YYYY-MM-DDTHH:MM:SSZ` for milliseconds since the epoch.
fn rfc3339(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::MAX_UTC)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
#[path = "viewer_tests.rs"]
mod tests;
