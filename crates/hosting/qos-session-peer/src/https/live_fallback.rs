//! LIVE_V1 HTTPS fallback carrier routes (`qos_moq::live::fallback`), served
//! directly beside the owner queues on HTTP/1.1, HTTP/2 and HTTP/3.
//!
//! Authorization is the live grant itself (exactly as the WebTransport
//! Authorization Token): `open` admits the grant bytes through the installed
//! live relay's authority (signature, lifetime, audience, host boot, lease,
//! current roster). `send`/`resume` carry the session's random 128-bit id,
//! generated here from OS randomness. The ordinary origin checks of every
//! `/peer/` route apply; no delivery proof and no owner turn are involved, so
//! live traffic never queues behind durable work.
use super::*;
use qos_moq::live::fallback::{
    FALLBACK_OPEN_PATH, FALLBACK_RESUME_PATH, FALLBACK_SEND_PATH, FALLBACK_SESSION_HEADER,
    FallbackError, MAX_SEND_BYTES, session_id,
};
use rand::RngCore;

/// Host bytes held by one open stream (bounded chunks in its queue).
const STREAM_BYTES: usize = 320 * 1024;

pub(super) fn recognizes(path: &str) -> bool {
    path.starts_with("/peer/v3/live/fallback/")
}

fn refused(error: FallbackError) -> Rejection {
    match error {
        FallbackError::Denied => Rejection::denied(),
        FallbackError::Gone => Rejection {
            status: StatusCode::NOT_FOUND,
            code: "not-found",
        },
        FallbackError::Busy => Rejection::unavailable(),
        FallbackError::Invalid => Rejection::invalid(),
    }
}

fn stream_response(
    stream: qos_moq::live::fallback::FallbackStream,
    credit: crate::host_bytes::ByteCredit,
) -> Response<ResponseBody> {
    response(
        StatusCode::OK,
        ResponseBody {
            data: Data::Live {
                stream,
                _credit: credit,
            },
            lease: None,
        },
        "application/octet-stream",
    )
}

pub(super) async fn serve<B>(
    parts: Parts,
    mut body: B,
    ingress: &Ingress,
) -> Result<Response<ResponseBody>, Rejection>
where
    B: Body<Data = Bytes> + Unpin,
{
    let relay = ingress.live.get().cloned().ok_or(Rejection {
        status: StatusCode::NOT_FOUND,
        code: "not-found",
    })?;
    let path = parts.uri.path();
    let limit = match path {
        FALLBACK_OPEN_PATH => qos_moq::live::MAX_TOKEN_BYTES,
        FALLBACK_RESUME_PATH => 0,
        FALLBACK_SEND_PATH => MAX_SEND_BYTES,
        _ => return Err(Rejection::unsupported()),
    };
    if parts.method != Method::POST
        || parts.uri.query().is_some()
        || parts.headers.contains_key(PROOF_HEADER)
        || parts.headers.get("content-length").is_some_and(|v| {
            v.to_str()
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .is_none_or(|size| size > limit)
        })
    {
        return Err(Rejection::invalid());
    }
    let session = parts
        .headers
        .get(FALLBACK_SESSION_HEADER)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(session_id)
                .ok_or_else(Rejection::invalid)
        })
        .transpose()?;
    let credit = ingress
        .bytes
        .try_acquire_bulk(limit + HEADER_BYTES)
        .ok_or_else(Rejection::unavailable)?;
    let mut bytes = BytesMut::with_capacity(limit);
    timeout(DEADLINE, async {
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| Rejection::invalid())?;
            let chunk = frame.into_data().map_err(|_| Rejection::invalid())?;
            if chunk.len() > limit - bytes.len() {
                return Err(Rejection::invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok::<_, Rejection>(())
    })
    .await
    .map_err(|_| Rejection::invalid())??;
    match path {
        FALLBACK_OPEN_PATH => {
            if session.is_some() || bytes.is_empty() {
                return Err(Rejection::invalid());
            }
            let mut id = [0u8; 16];
            rand::rngs::OsRng
                .try_fill_bytes(&mut id)
                .map_err(|_| Rejection::unavailable())?;
            drop(credit);
            let stream_credit = ingress
                .bytes
                .try_acquire_bulk(STREAM_BYTES)
                .ok_or_else(Rejection::unavailable)?;
            let stream = relay.fallback_open(&bytes, id).await.map_err(refused)?;
            Ok(stream_response(stream, stream_credit))
        }
        FALLBACK_RESUME_PATH => {
            let id = session.ok_or_else(Rejection::invalid)?;
            drop(credit);
            let stream_credit = ingress
                .bytes
                .try_acquire_bulk(STREAM_BYTES)
                .ok_or_else(Rejection::unavailable)?;
            let stream = relay.fallback_resume(&id).map_err(refused)?;
            Ok(stream_response(stream, stream_credit))
        }
        _ => {
            let id = session.ok_or_else(Rejection::invalid)?;
            relay.fallback_send(&id, &bytes).map_err(refused)?;
            drop(credit);
            Ok(response(
                StatusCode::NO_CONTENT,
                ResponseBody::full(Bytes::new(), None),
                "application/octet-stream",
            ))
        }
    }
}
