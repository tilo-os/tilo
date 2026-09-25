//! Tilo hosted-model gateway protocol.
//!
//! The API key is accepted only by the per-user gateway and is never part of
//! Settings' CRDT/settings payload or an agent process. Sandboxed callers use
//! one launcher-passed connected Unix descriptor (`TILO_SVC_MODEL_GATEWAY_FD`).

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(unix)]
use zeroize::Zeroizing;

pub const SERVICE_NAME: &str = "model-gateway";
pub const SERVICE_FD_ENV: &str = "TILO_SVC_MODEL_GATEWAY_FD";
pub const RPC_VERSION: u16 = 3;
/// Every gateway request and response: a little-endian `u32` length, then
/// JSON.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(2 * 1024 * 1024);
/// The only encoded-preview media types accepted by the local vision RPC.
/// Keeping this vocabulary closed means a caller cannot ask the trusted
/// gateway to fetch a URL or interpret an arbitrary document as an image.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct VisionImage {
    /// `image/jpeg`, `image/png`, or `image/webp`.
    pub media_type: String,
    /// Standard base64 of a caller-decoded, downscaled preview.
    pub data_base64: String,
    /// SHA-256 of the original photo bytes, not of `data_base64`.
    pub source_content_hash: String,
}

/// One deliberately small photograph reference supplied to the dedicated
/// local face runtime.  `photo_id` is an app address, not a pathname; the
/// runtime never receives an identity label or an instruction from Photos.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FaceImage {
    pub photo_id: String,
    pub media_type: String,
    pub data_base64: String,
    pub source_content_hash: String,
}

/// A request that can cross the sandbox boundary. `SetApiKey` is only sent by
/// Settings; a server also authenticates the launcher-provided app preamble.
/// Caller-owned settings types cannot serialize it.
#[derive(Deserialize, Serialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    Plan {
        profile_id: String,
        model_id: String,
        body: serde_json::Value,
        timeout_ms: u64,
    },
    /// Fixed-shape, device-local visual analysis for Photos. The gateway
    /// owns the prompt and refuses hosted, unpinned, or non-vision profiles;
    /// a sandboxed caller never obtains direct network access.
    AnalyzeImage {
        profile_id: String,
        model_id: String,
        image: VisionImage,
    },
    /// Fixed, consent-gated face detection and clustering.  The gateway owns
    /// the request schema and admits it only for the signed local face model.
    /// It returns opaque cluster/photo membership; no embedding, crop, face
    /// rectangle, identity name, or raw path can cross this boundary.
    ClusterFaces {
        profile_id: String,
        model_id: String,
        images: Vec<FaceImage>,
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
    #[error("Tilo model gateway is unavailable")]
    Unavailable,
    #[error("model gateway protocol error: {0}")]
    Protocol(String),
    #[error("model gateway refused request: {0}")]
    Rejected(String),
}

/// Whether this process has the launcher-issued service descriptor. This does
/// not probe or connect to a pathname socket, so the sandbox cannot substitute
/// a different same-UID service.
#[cfg(unix)]
pub fn available() -> bool {
    std::env::var(SERVICE_FD_ENV)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
        .is_some_and(|fd| fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1)
}

/// No launcher-issued gateway transport is implemented on this platform.
/// Environment variables cannot manufacture a supported capability.
#[cfg(not(unix))]
pub fn available() -> bool {
    false
}

/// Send a request over the launcher-provided service fd. The fd is duplicated
/// for each RPC so a short-lived helper cannot close the parent app's channel.
///
/// # Errors
///
/// Returns [`ClientError::Unavailable`] when the launcher service descriptor or
/// gateway connection is unavailable, [`ClientError::Protocol`] for malformed
/// frames, and [`ClientError::Rejected`] when the gateway refuses the request.
#[cfg(unix)]
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

/// Refuse gateway operations when the launcher capability transport is absent.
///
/// # Errors
///
/// Always returns [`ClientError::Unavailable`]. There is no direct-network or
/// pathname-socket fallback for an unsupported host.
#[cfg(not(unix))]
pub fn request(_request: Request) -> Result<Response, ClientError> {
    Err(ClientError::Unavailable)
}

/// Ask the isolated model gateway to plan against one configured provider.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway is unavailable, rejects the
/// request, returns a malformed response, or the provider request fails.
pub fn plan(
    profile_id: &str,
    model_id: &str,
    body: serde_json::Value,
    timeout: Duration,
) -> Result<String, ClientError> {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let response = request(Request::Plan {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
        body,
        timeout_ms,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no provider response".into()))
}

/// Request a fixed-schema visual analysis through the launcher-issued gateway
/// descriptor. This operation can only be authorized for Photos by the
/// gateway; callers cannot provide a URL, arbitrary prompt, or timeout.
///
/// # Errors
///
/// Returns [`ClientError`] if the capability is not passed by the launcher,
/// the gateway rejects the request, or the provider is unavailable.
pub fn analyze_image(
    profile_id: &str,
    model_id: &str,
    image: VisionImage,
) -> Result<String, ClientError> {
    let response = request(Request::AnalyzeImage {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
        image,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no image analysis".into()))
}

/// Run the dedicated local face model against one bounded batch.  This is not
/// a generic multimodal request and cannot be redirected to a hosted model.
///
/// # Errors
///
/// Returns [`ClientError`] if the launcher gateway is absent, the local model
/// refuses the fixed protocol, or its response is unavailable.
pub fn cluster_faces(
    profile_id: &str,
    model_id: &str,
    images: Vec<FaceImage>,
) -> Result<String, ClientError> {
    let response = request(Request::ClusterFaces {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
        images,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no face clusters".into()))
}

/// Query whether the profile has a secret stored by the isolated gateway.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway cannot be reached or rejects or
/// malforms the response.
pub fn secret_configured(profile_id: &str) -> Result<bool, ClientError> {
    Ok(request(Request::SecretStatus {
        profile_id: profile_id.to_string(),
    })?
    .configured
    .unwrap_or(false))
}

/// Store a profile API key in the isolated gateway.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway cannot be reached, rejects the
/// update, or returns a malformed response.
pub fn set_api_key(profile_id: &str, api_key: String) -> Result<(), ClientError> {
    request(Request::SetApiKey {
        profile_id: profile_id.to_string(),
        api_key,
    })?;
    Ok(())
}

/// Remove a profile API key from the isolated gateway.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway cannot be reached, rejects the
/// update, or returns a malformed response.
pub fn remove_api_key(profile_id: &str) -> Result<(), ClientError> {
    request(Request::RemoveApiKey {
        profile_id: profile_id.to_string(),
    })?;
    Ok(())
}

/// Write one size-bounded JSON frame to a gateway stream.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when serialization fails or the frame is
/// too large, and [`ClientError::Unavailable`] when writing fails.
#[cfg(unix)]
pub fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), ClientError> {
    let body = Zeroizing::new(
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?,
    );
    FRAMING.write(stream, &body).map_err(|error| match error {
        wire_frame::FrameError::Io(_) => ClientError::Unavailable,
        _ => ClientError::Protocol("request exceeds gateway frame limit".into()),
    })?;
    stream.flush().map_err(|_| ClientError::Unavailable)
}

/// Read and decode one size-bounded JSON frame from a gateway stream.
///
/// # Errors
///
/// Returns [`ClientError::Unavailable`] when reading fails and
/// [`ClientError::Protocol`] for an invalid length or payload.
#[cfg(unix)]
pub fn read_frame<T: for<'a> Deserialize<'a>>(stream: &mut UnixStream) -> Result<T, ClientError> {
    let mut body = Zeroizing::new(Vec::new());
    match FRAMING.read_into(stream, &mut body) {
        Ok(true) => {}
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err(ClientError::Protocol("gateway frame exceeds limit".into()));
        }
        Ok(false) | Err(_) => return Err(ClientError::Unavailable),
    }
    serde_json::from_slice(&body)
        .map_err(|_| ClientError::Protocol("invalid gateway response".into()))
}

#[cfg(all(test, unix))]
mod wire_tests {
    use super::*;

    /// The gateway socket's bytes: a little-endian `u32` length, then JSON.
    #[test]
    fn gateway_wire_bytes_are_pinned() {
        use std::io::Read as _;
        let (mut client, mut gateway) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Response {
                ok: true,
                output: Some("x".into()),
                configured: None,
                error: None,
            },
        )
        .unwrap();
        let mut sent = [0_u8; 28];
        gateway.read_exact(&mut sent).unwrap();
        assert_eq!(sent[..4], [24, 0, 0, 0]);
        assert_eq!(&sent[4..], br#"{"ok":true,"output":"x"}"#);

        gateway.write_all(&[12, 0, 0, 0]).unwrap();
        gateway.write_all(br#"{"ok":false}"#).unwrap();
        let response: Response = read_frame(&mut client).unwrap();
        assert!(!response.ok);
    }
}

#[cfg(all(test, not(unix)))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn photos_and_secret_operations_require_a_supported_gateway_capability() {
        assert!(!available());
        assert!(matches!(
            analyze_image(
                "local",
                "vision",
                VisionImage {
                    media_type: "image/png".into(),
                    data_base64: String::new(),
                    source_content_hash: String::new(),
                }
            ),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            cluster_faces("local", "faces", Vec::new()),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            plan(
                "hosted",
                "model",
                serde_json::Value::Null,
                Duration::from_secs(1)
            ),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            secret_configured("hosted"),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            set_api_key("hosted", "owned fixture".into()),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            remove_api_key("hosted"),
            Err(ClientError::Unavailable)
        ));
    }
}
