//! qOS hosted-model gateway protocol.
//!
//! The API key is accepted only by the per-user gateway and is never part of
//! Settings' CRDT/settings payload or an agent process. Sandboxed callers use
//! one launcher-passed connected Unix descriptor (`QOS_SVC_MODEL_GATEWAY_FD`).

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::Zeroizing;

pub const SERVICE_NAME: &str = "model-gateway";
pub const SERVICE_FD_ENV: &str = "QOS_SVC_MODEL_GATEWAY_FD";
pub const RPC_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// A request that can cross the sandbox boundary. `SetApiKey` is only sent by
/// Settings; a server also authenticates the launcher-provided app preamble.
/// It is deliberately not serializable by caller-owned settings types.
#[derive(Deserialize, Serialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    Plan {
        profile_id: String,
        model_id: String,
        body: serde_json::Value,
    },
    SecretStatus {
        profile_id: String,
    },
    SetApiKey {
        profile_id: String,
        api_key: String,
    },
    RemoveApiKey {
        profile_id: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Frame<T> {
    pub version: u16,
    #[serde(flatten)]
    pub value: T,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("qOS model gateway is unavailable")]
    Unavailable,
    #[error("model gateway protocol error: {0}")]
    Protocol(String),
    #[error("model gateway refused request: {0}")]
    Rejected(String),
}

/// Whether this process has the launcher-issued service descriptor. This does
/// not probe or connect to a pathname socket, so the sandbox cannot substitute
/// a different same-UID service.
pub fn available() -> bool {
    std::env::var(SERVICE_FD_ENV)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
        .is_some_and(|fd| fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1)
}

/// Send a request over the launcher-provided service fd. The fd is duplicated
/// for each RPC so a short-lived helper cannot close the parent app's channel.
///
/// # Errors
///
/// Returns an error when the service is unavailable, framing fails, or the
/// gateway rejects the request.
pub fn request(request: Request) -> Result<Response, ClientError> {
    let fd = std::env::var(SERVICE_FD_ENV)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
        .filter(|fd| *fd >= 3 && unsafe { libc::fcntl(*fd, libc::F_GETFD) } != -1)
        .ok_or(ClientError::Unavailable)?;
    // SAFETY: F_DUPFD_CLOEXEC duplicates a checked open descriptor; ownership
    // of the duplicate transfers immediately to UnixStream.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(ClientError::Unavailable);
    }
    let mut stream = unsafe { UnixStream::from_raw_fd(duplicate) };
    write_frame(
        &mut stream,
        &Frame {
            version: RPC_VERSION,
            value: request,
        },
    )?;
    let response: Response = read_frame(&mut stream)?;
    if response.ok {
        Ok(response)
    } else {
        Err(ClientError::Rejected(
            response
                .error
                .unwrap_or_else(|| "request rejected".to_string()),
        ))
    }
}

/// Requests a model-generated plan from the selected provider profile.
///
/// # Errors
///
/// Returns an error when the gateway request fails or contains no response.
pub fn plan(
    profile_id: &str,
    model_id: &str,
    body: serde_json::Value,
) -> Result<String, ClientError> {
    let response = request(Request::Plan {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
        body,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no provider response".into()))
}

/// Reports whether a secret is configured for a provider profile.
///
/// # Errors
///
/// Returns an error when the gateway request fails.
pub fn secret_configured(profile_id: &str) -> Result<bool, ClientError> {
    Ok(request(Request::SecretStatus {
        profile_id: profile_id.to_string(),
    })?
    .configured
    .unwrap_or(false))
}

/// Stores an API key through the privileged gateway.
///
/// # Errors
///
/// Returns an error when the gateway request fails or is rejected.
pub fn set_api_key(profile_id: &str, api_key: String) -> Result<(), ClientError> {
    request(Request::SetApiKey {
        profile_id: profile_id.to_string(),
        api_key,
    })?;
    Ok(())
}

/// Removes the API key associated with a provider profile.
///
/// # Errors
///
/// Returns an error when the gateway request fails or is rejected.
pub fn remove_api_key(profile_id: &str) -> Result<(), ClientError> {
    request(Request::RemoveApiKey {
        profile_id: profile_id.to_string(),
    })?;
    Ok(())
}

/// Writes one size-bounded JSON frame.
///
/// # Errors
///
/// Returns an error when serialization, size validation, or socket I/O fails.
pub fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), ClientError> {
    let body = Zeroizing::new(
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?,
    );
    if body.len() > MAX_FRAME_BYTES {
        return Err(ClientError::Protocol(
            "request exceeds gateway frame limit".into(),
        ));
    }
    let len = u32::try_from(body.len())
        .map_err(|_| ClientError::Protocol("request exceeds gateway frame limit".into()))?;
    stream
        .write_all(&len.to_le_bytes())
        .map_err(|_| ClientError::Unavailable)?;
    stream
        .write_all(&body)
        .map_err(|_| ClientError::Unavailable)?;
    stream.flush().map_err(|_| ClientError::Unavailable)
}

/// Reads and decodes one size-bounded JSON frame.
///
/// # Errors
///
/// Returns an error when socket I/O, size validation, or decoding fails.
pub fn read_frame<T: for<'a> Deserialize<'a>>(stream: &mut UnixStream) -> Result<T, ClientError> {
    let mut length = [0_u8; 4];
    stream
        .read_exact(&mut length)
        .map_err(|_| ClientError::Unavailable)?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| ClientError::Protocol("invalid frame length".into()))?;
    if length > MAX_FRAME_BYTES {
        return Err(ClientError::Protocol("gateway frame exceeds limit".into()));
    }
    let mut body = Zeroizing::new(vec![0_u8; length]);
    stream
        .read_exact(&mut body)
        .map_err(|_| ClientError::Unavailable)?;
    serde_json::from_slice(&body)
        .map_err(|_| ClientError::Protocol("invalid gateway response".into()))
}
