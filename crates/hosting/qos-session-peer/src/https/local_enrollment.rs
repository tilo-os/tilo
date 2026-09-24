//! Explicit local OS-owner enrollment gateway. Hosted application auth is absent.
use super::*;
use crate::enrollment_http::{Authorization, EnrollmentCommand, EnrollmentRequest};
pub(crate) const COOKIE_NAME: &str = "__Host-qos-local-owner";
pub(crate) const INPUT_BYTES: usize = crate::host_bytes::CONTROL_INPUT_BYTES;
pub(crate) struct LocalEnrollment {
    origin: String,
    authorization: Arc<Authorization>,
    commands: mpsc::Sender<EnrollmentCommand>,
}
impl LocalEnrollment {
    pub(crate) fn new(
        origin: &str,
        authorization: Arc<Authorization>,
        commands: mpsc::Sender<EnrollmentCommand>,
    ) -> io::Result<Self> {
        Origin::parse(origin)?;
        Ok(Self {
            origin: origin.into(),
            authorization,
            commands,
        })
    }
    pub(super) fn origin(&self) -> &str {
        &self.origin
    }
    pub(super) fn cookie(&self, parts: &Parts) -> Result<(), Rejection> {
        if parts.headers.get_all("cookie").iter().count() != 1 {
            return Err(Rejection::denied());
        }
        let value = parts
            .headers
            .get("cookie")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(Rejection::denied)?;
        let token = value
            .strip_prefix(COOKIE_NAME)
            .and_then(|v| v.strip_prefix('='))
            .ok_or_else(Rejection::denied)?;
        if !self.authorization.authorizes_local_cookie(token) {
            return Err(Rejection::denied());
        }
        Ok(())
    }
    pub(super) fn recognizes(path: &str) -> bool {
        matches!(
            path,
            "/qos-peer/enrollment/challenge" | "/qos-peer/enrollment/complete"
        )
    }
    fn authorize(&self, parts: &Parts) -> Result<(), Rejection> {
        let origin = Origin::parse(&self.origin).map_err(|_| Rejection::denied())?;
        check_origin_base(parts, &origin)?;
        self.cookie(parts)?;
        if parts.method != Method::POST
            || parts.uri.query().is_some()
            || !Self::recognizes(parts.uri.path())
            || parts
                .headers
                .get("origin")
                .is_none_or(|v| v.as_bytes() != self.origin.as_bytes())
            || parts
                .headers
                .get("content-type")
                .is_none_or(|v| v.as_bytes() != b"application/json")
            || parts.headers.contains_key(PROOF_HEADER)
        {
            return Err(Rejection::denied());
        }
        Ok(())
    }
    pub(super) async fn serve<B>(
        &self,
        parts: Parts,
        mut body: B,
        ingress: &Ingress,
    ) -> Result<Response<ResponseBody>, Rejection>
    where
        B: hyper::body::Body<Data = bytes::Bytes> + Unpin,
    {
        self.authorize(&parts)?;
        if parts.headers.get("content-length").is_some_and(|value| {
            value
                .to_str()
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .is_none_or(|n| n > INPUT_BYTES)
        }) {
            return Err(Rejection::invalid());
        }
        let deadline = Instant::now() + DEADLINE;
        let lease = ingress.reserve(INPUT_BYTES, Lane::Control)?;
        let mut bytes = BytesMut::with_capacity(INPUT_BYTES);
        timeout_at(deadline, async {
            while let Some(frame) = body.frame().await {
                let chunk = frame
                    .map_err(|_| Rejection::invalid())?
                    .into_data()
                    .map_err(|_| Rejection::invalid())?;
                if chunk.len() > INPUT_BYTES - bytes.len() {
                    return Err(Rejection::invalid());
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok::<_, Rejection>(())
        })
        .await
        .map_err(|_| Rejection::unavailable())??;
        let omit_governance = parts.uri.path() == "/qos-peer/enrollment/complete"
            && match parts
                .headers
                .get_all(crate::enrollment_http::GOVERNANCE_HEADER)
                .iter()
                .map(|value| value.as_bytes())
                .collect::<Vec<_>>()
                .as_slice()
            {
                [] => false,
                [b"omit"] => true,
                _ => return Err(Rejection::invalid()),
            };
        let request = if parts.uri.path() == "/qos-peer/enrollment/challenge" {
            EnrollmentRequest::Challenge(
                serde_json::from_slice(&bytes).map_err(|_| Rejection::invalid())?,
            )
        } else {
            EnrollmentRequest::Complete(
                serde_json::from_slice(&bytes).map_err(|_| Rejection::invalid())?,
            )
        };
        let (reply, completed) = oneshot::channel();
        self.commands
            .try_send(
                EnrollmentCommand::from_https(request, reply, lease.clone(), deadline)
                    .omitting_governance(omit_governance),
            )
            .map_err(|_| Rejection::unavailable())?;
        let result = timeout_at(deadline, completed)
            .await
            .map_err(|_| Rejection::unavailable())?
            .map_err(|_| Rejection::unavailable())?;
        let (status, body) = match result {
            Ok(body) => (StatusCode::OK, body),
            Err(error) => (
                StatusCode::from_u16(error.status)
                    .ok()
                    .filter(|v| v.is_client_error() || v.is_server_error())
                    .unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                serde_json::to_vec(&serde_json::json!({"error":error.message}))
                    .map_err(|_| Rejection::unavailable())?,
            ),
        };
        if body.capacity() > BODY_BYTES {
            return Err(Rejection::unavailable());
        }
        Ok(response(
            status,
            ResponseBody::full(leased_bytes(body, lease.clone()), Some(lease)),
            "application/json",
        ))
    }
}
#[cfg(test)]
mod tests;
