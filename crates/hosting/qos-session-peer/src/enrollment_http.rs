//! Narrow loopback gateway. Credentials authorize enrollment, never document transport.
use crate::enrollment::EnrollmentError;
use anyhow::{Context, Result, bail};
use qos_peer_state::enrollment::{ChallengeRequest, CompleteRequest};
use rand::RngCore;
use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
};
const MAX_HEADERS: usize = 8192;
const MAX_BODY: usize = 16384;
#[derive(Clone)]
pub struct Authorization {
    expected_host: String,
    expected_origin: String,
    capability: [u8; 32],
}
impl Authorization {
    pub fn local(root: &Path, expected_host: String) -> Result<Self> {
        let path = root.join("enrollment-capability");
        let absent = match std::fs::symlink_metadata(&path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error.into()),
        };
        if absent {
            let mut capability = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut capability);
            atomic_file::write_with_mode(&path, hex::encode(capability).as_bytes(), 0o600)?;
            crate::store::sync_dir(root)?;
        }
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() {
            bail!("enrollment capability must be a regular private file");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!("enrollment capability must be owner-only");
            }
        }
        let raw = std::fs::read_to_string(&path)?;
        if raw.len() != 64 {
            bail!("enrollment capability must contain exactly64 hexadecimal characters");
        }
        let capability: [u8; 32] = hex::decode(raw)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid enrollment capability"))?;
        Ok(Self {
            expected_host,
            expected_origin: "http://127.0.0.1:9998".into(),
            capability,
        })
    }
    /// Dedicated local HTTPS credential check. The private capability is never
    /// returned to browser script or inferred from Origin/loopback reachability.
    pub(crate) fn authorizes_local_cookie(&self, value: &str) -> bool {
        if value.len() != 64
            || !value
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
        {
            return false;
        }
        let Ok(bytes) = hex::decode(value) else {
            return false;
        };
        bytes
            .iter()
            .zip(self.capability)
            .fold(0u8, |difference, (a, b)| difference | (*a ^ b))
            == 0
    }
    fn authorize(
        &self,
        headers: &HashMap<String, String>,
        enrollment: bool,
    ) -> Result<(), EnrollmentError> {
        if headers.get("host") != Some(&self.expected_host) {
            return Err(EnrollmentError::new(403, "native Host is not permitted"));
        }
        if enrollment && headers.get("origin") != Some(&self.expected_origin)
            || headers
                .get("origin")
                .is_some_and(|origin| origin != &self.expected_origin)
        {
            return Err(EnrollmentError::new(
                403,
                "enrollment Origin is not permitted",
            ));
        }
        if enrollment {
            let supplied = headers
                .get("authorization")
                .and_then(|value| value.strip_prefix("Bearer "))
                .filter(|value| value.len() == 64)
                .and_then(|value| hex::decode(value).ok());
            let valid = supplied.is_some_and(|bytes| {
                bytes.len() == 32
                    && bytes
                        .iter()
                        .zip(self.capability)
                        .fold(0u8, |difference, (a, b)| difference | (*a ^ b))
                        == 0
            });
            if !valid {
                return Err(EnrollmentError::new(
                    403,
                    "authorized local enrollment session required",
                ));
            }
        }
        Ok(())
    }
}
pub enum EnrollmentRequest {
    Challenge(ChallengeRequest),
    Complete(CompleteRequest),
}
pub struct EnrollmentCommand {
    pub request: EnrollmentRequest,
    pub reply: oneshot::Sender<Result<Vec<u8>, EnrollmentError>>,
    // Declared last so owned decoded input drops before its original charge.
    // Partial moves of request/reply leave custody alive through the owner turn.
    custody: Option<HttpsCustody>,
}
struct HttpsCustody {
    _lease: Arc<crate::https::Lease>,
    deadline: tokio::time::Instant,
    omit_governance: bool,
}
/// Request header by which a renewing local-owner browser, which already
/// holds the workspace history, asks for the enrollment reply without it.
pub(crate) const GOVERNANCE_HEADER: &str = "x-tilo-enrollment-governance";
impl EnrollmentCommand {
    pub(crate) fn from_https(
        request: EnrollmentRequest,
        reply: oneshot::Sender<Result<Vec<u8>, EnrollmentError>>,
        lease: Arc<crate::https::Lease>,
        deadline: tokio::time::Instant,
    ) -> Self {
        Self {
            request,
            reply,
            custody: Some(HttpsCustody {
                _lease: lease,
                deadline,
                omit_governance: false,
            }),
        }
    }
    /// Renewal only: the reply carries certificate, roster and delivery
    /// discovery but no governance history. Only reduces what is returned to
    /// the already authorized owner; HTTPS-only.
    pub(crate) fn omitting_governance(mut self, omit: bool) -> Self {
        if let Some(custody) = self.custody.as_mut() {
            custody.omit_governance = omit;
        }
        self
    }
    pub(crate) fn omits_governance(&self) -> bool {
        self.custody
            .as_ref()
            .is_some_and(|custody| custody.omit_governance)
    }
    /// Arrived through the installed HTTPS local-owner gateway, whose browser
    /// can fetch a governance handoff from the same origin.
    pub(crate) fn is_https(&self) -> bool {
        self.custody.is_some()
    }
    pub(crate) fn is_cancelled(&self) -> bool {
        self.reply.is_closed()
            || self
                .custody
                .as_ref()
                .is_some_and(|custody| tokio::time::Instant::now() >= custody.deadline)
    }
}
async fn read_request(
    socket: &mut TcpStream,
    authorization: &Authorization,
) -> Result<Option<EnrollmentRequest>, EnrollmentError> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 2048];
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|slice| slice == b"\r\n\r\n") {
            break index + 4;
        }
        if bytes.len() >= MAX_HEADERS {
            return Err(EnrollmentError::new(
                413,
                "request headers exceed local limit",
            ));
        }
        let count = socket
            .read(&mut chunk)
            .await
            .map_err(|error| EnrollmentError::new(400, error.to_string()))?;
        if count == 0 {
            return Err(EnrollmentError::new(400, "incomplete HTTP request"));
        }
        bytes.extend_from_slice(&chunk[..count]);
    };
    if header_end > MAX_HEADERS {
        return Err(EnrollmentError::new(
            413,
            "request headers exceed local limit",
        ));
    }
    let headers = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| EnrollmentError::new(400, "invalid HTTP headers"))?;
    let mut lines = headers.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split_whitespace();
    let method = request.next().unwrap_or_default();
    let path = request.next().unwrap_or_default();
    if request.next() != Some("HTTP/1.1") || request.next().is_some() {
        return Err(EnrollmentError::new(400, "HTTP/1.1 required"));
    }
    let mut values = HashMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| EnrollmentError::new(400, "malformed HTTP header"))?;
        if values
            .insert(key.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return Err(EnrollmentError::new(400, "duplicate HTTP header"));
        }
    }
    let is_status = method == "GET" && path == "/status";
    authorization.authorize(&values, !is_status)?;
    if is_status {
        return Ok(None);
    }
    if method != "POST" {
        return Err(EnrollmentError::new(405, "POST required"));
    }
    if path != "/enrollment/challenge" && path != "/enrollment/complete" {
        return Err(EnrollmentError::new(404, "enrollment route unavailable"));
    }
    if values.contains_key("transfer-encoding") {
        return Err(EnrollmentError::new(
            400,
            "chunked enrollment requests are not supported",
        ));
    }
    if values
        .get("content-type")
        .is_none_or(|value| value.split(';').next() != Some("application/json"))
    {
        return Err(EnrollmentError::new(415, "application/json required"));
    }
    let size = values
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .ok_or_else(|| EnrollmentError::new(400, "valid Content-Length required"))?;
    if size > MAX_BODY {
        return Err(EnrollmentError::new(
            413,
            "enrollment body exceeds local limit",
        ));
    }
    let path = path.to_owned();
    let total = header_end + size;
    while bytes.len() < total {
        let count = socket
            .read(&mut chunk)
            .await
            .map_err(|error| EnrollmentError::new(400, error.to_string()))?;
        if count == 0 {
            return Err(EnrollmentError::new(400, "incomplete enrollment body"));
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let body = &bytes[header_end..total];
    if path == "/enrollment/challenge" {
        serde_json::from_slice(body)
            .map(EnrollmentRequest::Challenge)
            .map(Some)
    } else {
        serde_json::from_slice(body)
            .map(EnrollmentRequest::Complete)
            .map(Some)
    }
    .map_err(|_| EnrollmentError::new(400, "invalid enrollment request schema"))
}
pub async fn serve(
    mut socket: TcpStream,
    authorization: Arc<Authorization>,
    commands: mpsc::Sender<EnrollmentCommand>,
    status: serde_json::Value,
) -> Result<()> {
    let request = tokio::time::timeout(
        Duration::from_secs(5),
        read_request(&mut socket, &authorization),
    )
    .await;
    let result = match request {
        Err(_) => Err(EnrollmentError::new(408, "enrollment request timed out")),
        Ok(Err(error)) => Err(error),
        Ok(Ok(None)) => Ok(serde_json::to_vec(&status)?),
        Ok(Ok(Some(request))) => {
            let (reply, result) = oneshot::channel();
            if commands
                .send(EnrollmentCommand {
                    request,
                    reply,
                    custody: None,
                })
                .await
                .is_err()
            {
                Err(EnrollmentError::new(503, "native owner unavailable"))
            } else {
                result
                    .await
                    .unwrap_or_else(|_| Err(EnrollmentError::new(503, "native owner unavailable")))
            }
        }
    };
    let (code, body) = match result {
        Ok(bytes) => (200, bytes),
        Err(error) => (
            error.status,
            serde_json::to_vec(&serde_json::json!({"error":error.message}))?,
        ),
    };
    socket.write_all(format!("HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await?;
    socket.write_all(&body).await?;
    socket
        .shutdown()
        .await
        .context("close enrollment response")?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_host_origin_and_private_capability_are_all_required() {
        let temp = tempfile::tempdir().unwrap();
        let auth = Authorization::local(temp.path(), "127.0.0.1:4567".into()).unwrap();
        let mut headers = HashMap::from([
            ("host".into(), "127.0.0.1:4567".into()),
            ("origin".into(), "http://127.0.0.1:9998".into()),
            (
                "authorization".into(),
                format!("Bearer {}", hex::encode(auth.capability)),
            ),
        ]);
        assert!(auth.authorize(&headers, true).is_ok());
        for (name, bad) in [
            ("host", "127.0.0.1:4567.evil.invalid"),
            ("host", "localhost:4567"),
            ("origin", "https://unrelated.invalid"),
            ("authorization", "Bearer invalid"),
        ] {
            let old = headers.insert(name.into(), bad.into()).unwrap();
            assert_eq!(auth.authorize(&headers, true).unwrap_err().status, 403);
            headers.insert(name.into(), old);
        }
        headers.remove("origin");
        assert!(auth.authorize(&headers, true).is_err());
        assert!(auth.authorize(&headers, false).is_ok());
        let again = Authorization::local(temp.path(), "127.0.0.1:4567".into()).unwrap();
        assert_eq!(auth.capability, again.capability);
    }
}
