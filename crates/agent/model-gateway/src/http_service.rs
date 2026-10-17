//! Small HTTP/1 adapter for an authenticated host, not a second identity service.
//!
//! Mount behind the host's HTTPS endpoint (or pass its accepted TLS stream to
//! `serve_io`). Never expose a cleartext listener to the public network. The
//! host must validate session, origin/CSRF and audience before returning a grant.
//! No identity or permission is accepted from the model request JSON.
use crate::{RPC_VERSION, Request, Response};
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody, combinators::UnsyncBoxBody};
use hyper::{
    Request as HttpRequest, Response as HttpResponse,
    body::{Frame, Incoming},
};
use std::{
    convert::Infallible,
    future::Future,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc};

pub const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const QUEUE: usize = 8;
const MAX_EVENT_BYTES: usize = 2 * 1024 * 1024;
// A synchronous provider poll may emit a burst before yielding. Bound that
// burst independently of event count; queued batches obey the same byte cap.
const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
const MAX_CALL: Duration = Duration::from_secs(120);

pub use crate::caller::{Caller, Session};

/// Authentication must be local and bounded. Hosts can consult an already
/// authenticated relay/session store. Reject missing/ambiguous credentials,
/// cross-origin cookie requests and tokens intended for another service.
pub trait SessionAuthorizer: Send + Sync + 'static {
    fn authorize(&self, headers: &hyper::HeaderMap) -> Option<Arc<dyn Session>>;
}

type Body = UnsyncBoxBody<Bytes, Infallible>;

#[derive(Clone)]
pub struct HttpService {
    authorizer: Arc<dyn SessionAuthorizer>,
    gateway: crate::service::Gateway,
    slots: Arc<Semaphore>,
}
impl HttpService {
    /// # Errors
    /// Returns an error if the provider HTTP client cannot be initialized.
    pub fn new(authorizer: Arc<dyn SessionAuthorizer>, max_calls: usize) -> Result<Self, String> {
        Ok(Self {
            authorizer,
            gateway: crate::service::Gateway::new()?,
            slots: Arc::new(Semaphore::new(max_calls.clamp(1, 64))),
        })
    }

    /// Run a lightweight loopback HTTP service behind a trusted HTTPS reverse
    /// proxy. Authentication still comes exclusively from the injected host
    /// authorizer; binding publicly requires host TLS via `serve_io` instead.
    /// Dropping this future or resolving `shutdown` aborts every connection.
    /// # Errors
    /// Rejects non-loopback listeners or failures accepting connections.
    pub async fn serve_loopback<F>(
        &self,
        listener: tokio::net::TcpListener,
        shutdown: F,
    ) -> Result<(), String>
    where
        F: std::future::Future<Output = ()> + Send,
    {
        if !listener
            .local_addr()
            .map_err(|_| "listener unavailable")?
            .ip()
            .is_loopback()
        {
            return Err("cleartext model service must bind loopback".into());
        }
        tokio::pin!(shutdown);
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                () = &mut shutdown => return Ok(()),
                _ = connections.join_next(), if !connections.is_empty() => {},
                accepted = listener.accept(), if connections.len() < 64 => {
                    let (stream, _) = accepted.map_err(|_| "model listener failed")?;
                    let service = self.clone();
                    connections.spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(135), service.serve_io(stream)).await;
                    });
                }
            }
        }
    }

    /// Serve a host-accepted TCP or TLS stream. Connection admission and TLS
    /// are host-owned; active model calls have their own bounded admission.
    /// # Errors
    /// Returns HTTP protocol or stream I/O errors.
    pub async fn serve_io<I>(&self, io: I) -> Result<(), hyper::Error>
    where
        I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let service = self.clone();
        hyper::server::conn::http1::Builder::new()
            .max_buf_size(16 * 1024)
            .serve_connection(
                hyper_util::rt::TokioIo::new(io),
                hyper::service::service_fn(move |request| {
                    let service = service.clone();
                    async move { Ok::<_, Infallible>(service.handle(request).await) }
                }),
            )
            .await
    }

    pub async fn handle(&self, request: HttpRequest<Incoming>) -> HttpResponse<Body> {
        if request.method() != hyper::Method::POST || request.uri().path() != "/model/rpc" {
            return failure(404);
        }
        let Some(session) = self
            .authorizer
            .authorize(request.headers())
            .filter(|s| s.active())
        else {
            return failure(401);
        };
        if request
            .headers()
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            != Some("application/json")
        {
            return failure(415);
        }
        let Ok(slot) = self.slots.clone().try_acquire_owned() else {
            return failure(429);
        };
        let body = async {
            let mut body = request.into_body();
            let mut bytes = zeroize::Zeroizing::new(Vec::new());
            while let Some(frame) = body.frame().await {
                let frame = frame.map_err(|_| ())?;
                if let Ok(data) = frame.into_data() {
                    if bytes.len().saturating_add(data.len()) > MAX_REQUEST_BYTES {
                        return Err(());
                    }
                    bytes.extend_from_slice(&data);
                }
            }
            Ok(bytes)
        };
        let Ok(Ok(body)) = tokio::time::timeout(Duration::from_secs(10), body).await else {
            return failure(413);
        };
        let Ok(frame) = serde_json::from_slice::<crate::Frame<Request>>(&body) else {
            return failure(400);
        };
        if frame.version != RPC_VERSION {
            return failure(400);
        }
        if !crate::caller::permitted(session.as_ref(), &frame.value) {
            return failure(403);
        }
        let (sender, receiver) = mpsc::channel::<Bytes>(QUEUE);
        let gateway = self.gateway.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let events = EventBuffer::default();
            let mut emit = |response: &Response| events.emit(response);
            let handling = gateway.execute(session.as_ref(), frame.value, &mut emit);
            let forwarding = forward(handling, &events, &sender);
            let revoked = async {
                loop {
                    if !session.active() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            };
            // Dropping inference aborts its provider HTTP body/request. Durable
            // management jobs already dispatched to the blocking pool still
            // commit; disconnect/revocation is never a rollback acknowledgement.
            tokio::select! {
                _ = tokio::time::timeout(MAX_CALL, forwarding) => {}
                () = sender.closed() => {}
                () = revoked => {}
            }
        });
        let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|bytes| (Ok::<_, Infallible>(Frame::data(bytes)), receiver))
        });
        let mut response = HttpResponse::new(StreamBody::new(stream).boxed_unsync());
        response.headers_mut().insert(
            hyper::header::CONTENT_TYPE,
            hyper::header::HeaderValue::from_static("application/x-ndjson"),
        );
        response.headers_mut().insert(
            hyper::header::CACHE_CONTROL,
            hyper::header::HeaderValue::from_static("no-store"),
        );
        response
    }
}

#[derive(Default)]
struct EventBuffer {
    state: Mutex<(usize, Vec<u8>)>,
}
impl EventBuffer {
    fn emit(&self, response: &Response) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.0 = state.0.saturating_add(1);
        if state.0 > crate::MAX_STREAM_EVENTS + crate::MAX_DELTA_EVENTS {
            return false;
        }
        let Some(bytes) = encode(response) else {
            return false;
        };
        if state.1.len().saturating_add(bytes.len()) > MAX_BATCH_BYTES {
            return false;
        }
        state.1.extend_from_slice(&bytes);
        true
    }
}

// Poll the producer only after its previous batch has been sent. The callback
// cannot await, so one poll coalesces NDJSON frames without changing their order
// or boundaries. At most QUEUE queued batches, one sending batch and one encoded
// event are retained; a stalled consumer never grows an unbounded event list.
async fn forward(
    handling: impl Future<Output = Response>,
    events: &EventBuffer,
    sender: &mpsc::Sender<Bytes>,
) {
    tokio::pin!(handling);
    loop {
        let (result, batch) = std::future::poll_fn(|cx| {
            let result = handling.as_mut().poll(cx);
            let batch = std::mem::take(
                &mut events
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .1,
            );
            if result.is_pending() && batch.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready((result, batch))
            }
        })
        .await;
        if !batch.is_empty() && sender.send(Bytes::from(batch)).await.is_err() {
            return;
        }
        if let Poll::Ready(response) = result {
            if let Some(bytes) = encode(&response) {
                let _ = sender.send(bytes).await;
            }
            return;
        }
    }
}

fn encode(response: &Response) -> Option<Bytes> {
    let mut bytes = serde_json::to_vec(response).ok()?;
    if bytes.len() > MAX_EVENT_BYTES {
        return None;
    }
    bytes.push(b'\n');
    Some(bytes.into())
}
#[cfg(test)]
#[path = "http_service_tests.rs"]
mod tests;

fn failure(status: u16) -> HttpResponse<Body> {
    let mut response = HttpResponse::new(http_body_util::Empty::<Bytes>::new().boxed_unsync());
    *response.status_mut() =
        hyper::StatusCode::from_u16(status).unwrap_or(hyper::StatusCode::BAD_REQUEST);
    response.headers_mut().insert(
        hyper::header::CACHE_CONTROL,
        hyper::header::HeaderValue::from_static("no-store"),
    );
    response
}
