//! Closed trusted-host backend for native task inference.
//!
//! Wire invocation values identify custody; they never supply admission. The
//! launch-authenticated Session chooses this source, independently of RPC JSON.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use agent_provider_config::ProviderProfile;
use agent_provider_config::protocol::{ErrorKind, ModelError};
use bytes::Bytes;
use http_body_util::Full;
use hyper_util::rt::TokioIo;

use super::{Again, ProviderFailure};
use crate::{Request, Response};

/// Non-secret, complete selection and exact final HTTP bytes. Deliberately no
/// Debug implementation: the body contains the person's private instructions.
#[derive(Clone)]
pub struct FrozenNativeAttempt {
    pub profile: ProviderProfile,
    pub model: String,
    pub endpoint: String,
    pub body: Vec<u8>,
}

/// Host adapter to the typed owner MODEL reserve/claim/dispatch gate.
///
/// `prepare` performs all asynchronous custody waits. `dispatch` must recheck
/// live authority inside the owner mutation/fence turn and invoke `start`
/// synchronously in that turn. Returning success without calling it is refused.
/// Each invocation is one HTTP attempt; no permit or boolean is reusable.
///
/// The synchronous callback transfers the request to a ready, dedicated HTTP/1
/// connection under the fence turn. This is the dispatch linearization point:
/// later socket writes/response completion may occur after Stop, but no task
/// awaiting admission may submit a new request after Stop. No lock may be
/// retained after `dispatch` returns.
pub trait NativeModelDispatch: Send + Sync {
    /// Called solely by the protected source after a successful decoded response.
    /// Delivery is historical evidence, not renewed permission to dispatch.
    fn completed(&self, _proof: &crate::native_delivery::NativeModelSuccess) -> Result<(), String> {
        Ok(())
    }

    fn prepare<'a>(
        &'a self,
        invocation: &'a serde_json::Value,
        attempt: &'a FrozenNativeAttempt,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

    fn dispatch(
        &self,
        invocation: &serde_json::Value,
        attempt: &FrozenNativeAttempt,
        start: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String>;
}

/// Protected launcher role. Missing native custody is not an ordinary source.
pub enum GatewaySource<'a> {
    Ordinary,
    Native(&'a NativeGatewaySource),
    NativeUnavailable,
}

/// Constructed only by trusted host code, never deserialized or selected by a
/// caller. Uses the ordinary host configuration, vault and generic Rig codecs.
pub struct NativeGatewaySource {
    http: reqwest::Client,
    dispatch: Arc<dyn NativeModelDispatch>,
    dispatched: Arc<std::sync::atomic::AtomicBool>,
}

impl NativeGatewaySource {
    pub fn new(dispatch: Arc<dyn NativeModelDispatch>) -> Result<Self, String> {
        Ok(Self {
            http: super::provider_client()?,
            dispatch,
            dispatched: Arc::default(),
        })
    }

    pub(super) fn dispatched(&self) -> bool {
        self.dispatched.load(std::sync::atomic::Ordering::Acquire)
    }

    pub async fn execute(
        &self,
        request: Request,
        emit: &mut (dyn FnMut(&Response) -> bool + Send),
    ) -> Response {
        use agent_provider_config::protocol::{Content, Role};
        let texts = if let Request::NativeComplete { request, .. } = &request {
            request
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .flat_map(|m| &m.content)
                .filter_map(|c| match c {
                    Content::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        let (invocation, ordinary) = match request {
            Request::NativePlan {
                invocation,
                profile_id,
                model_id,
                body,
                timeout_ms,
                stream,
                deltas,
                trace,
            } => (
                invocation,
                Request::Plan {
                    profile_id,
                    model_id,
                    body,
                    timeout_ms,
                    stream,
                    deltas,
                    trace,
                },
            ),
            Request::NativeTransform {
                invocation,
                profile_id,
                model_id,
                instructions,
                input,
                max_output_tokens,
                timeout_ms,
            } => (
                invocation,
                Request::Transform {
                    profile_id,
                    model_id,
                    instructions,
                    input,
                    max_output_tokens,
                    timeout_ms,
                },
            ),
            Request::NativeComplete {
                invocation,
                profile_id,
                model_id,
                request,
                timeout_ms,
                stream,
            } => (
                invocation,
                Request::Complete {
                    profile_id,
                    model_id,
                    request,
                    timeout_ms,
                    stream,
                },
            ),
            // In particular, legacy Plan/Transform/Complete cannot bypass the
            // native gate on a protected connection.
            _ => return super::permission_denied(),
        };
        let timeout = match &ordinary {
            Request::Plan { timeout_ms, .. } | Request::Complete { timeout_ms, .. } => {
                std::time::Duration::from_millis(*timeout_ms)
            }
            Request::Transform { timeout_ms, .. } => {
                std::time::Duration::from_millis(*timeout_ms).min(crate::MAX_TRANSFORM_TIMEOUT)
            }
            _ => unreachable!("only native inference is converted above"),
        };
        // One wall-clock budget starts BEFORE profile/cache/vault resolution,
        // DNS/TCP/TLS, and asynchronous custody preparation. Inner HTTP budgets
        // alone do not bound those waits. Dropping this future cannot undo a
        // transferred POST (nor a blocking vault read); dispatch still rechecks
        // the live Stop fence before any transfer, with no automatic replay.
        let completed = Arc::new(std::sync::Mutex::new(None));
        let result = tokio::time::timeout(
            timeout,
            CONTEXT.scope(
                NativeContext {
                    invocation: invocation.clone(),
                    dispatch: self.dispatch.clone(),
                    completed: completed.clone(),
                    dispatched: self.dispatched.clone(),
                },
                super::handle_streaming(super::AGENT_APP, ordinary, &self.http, emit),
            ),
        )
        .await;
        match result {
            Ok(response) => {
                if response.ok {
                    let attempt = completed.lock().ok().and_then(|mut a| a.take());
                    let Some(attempt) = attempt else {
                        return super::permission_denied();
                    };
                    let proof = crate::native_delivery::NativeModelSuccess {
                        invocation,
                        profile: attempt.profile,
                        model: attempt.model,
                        endpoint: attempt.endpoint,
                        body: attempt.body,
                        texts,
                    };
                    if self.dispatch.completed(&proof).is_err() {
                        return super::permission_denied();
                    }
                }
                response
            }
            Err(_) => {
                let mut response = Response::failure(ModelError::new(ErrorKind::TimedOut));
                // Cancellation after transfer does not prove non-acceptance.
                // Keep the typed diagnostic redacted; this additional fixed
                // text contains no provider-controlled content or secrets.
                response.error = Some(
                    "Provider request timed out; outcome may be unknown; request was not retried"
                        .into(),
                );
                response
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct NativeContext {
    invocation: serde_json::Value,
    dispatch: Arc<dyn NativeModelDispatch>,
    completed: Arc<std::sync::Mutex<Option<FrozenNativeAttempt>>>,
    dispatched: Arc<std::sync::atomic::AtomicBool>,
}

tokio::task_local! {
    static CONTEXT: NativeContext;
}

pub(super) fn context() -> Option<NativeContext> {
    CONTEXT.try_with(Clone::clone).ok()
}

fn denied() -> ProviderFailure {
    ProviderFailure {
        model_error: ModelError::new(ErrorKind::PermissionDenied),
        why: "native model admission refused".into(),
        again: Again::Never,
        outcome_unknown: true,
        unprocessed: false,
        status: None,
    }
}

impl NativeContext {
    pub(super) async fn send(
        &self,
        _client: &reqwest::Client,
        request: reqwest::Request,
        profile: &ProviderProfile,
        model: &str,
    ) -> Result<reqwest::Response, ProviderFailure> {
        let attempt = FrozenNativeAttempt {
            profile: profile.clone(),
            model: model.to_owned(),
            endpoint: request.url().to_string(),
            body: request
                .body()
                .and_then(reqwest::Body::as_bytes)
                .ok_or_else(denied)?
                .to_vec(),
        };
        // DNS, TCP, TLS and the HTTP readiness wait precede custody. A
        // reqwest future's first poll is NOT sufficient: it can still wait
        // for a connection and only enqueue the POST after a Stop fence.
        let io = connect(request.url()).await?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(io))
                .await
                .map_err(|_| transport_failure(false))?;
        let driver = Driver(Some(tokio::spawn(async move {
            let _ = connection.await;
        })));
        sender.ready().await.map_err(|_| transport_failure(false))?;
        let mut outgoing = hyper::Request::builder()
            .method(request.method().clone())
            .uri(&request.url()[url::Position::BeforePath..url::Position::AfterQuery]);
        let headers = outgoing.headers_mut().ok_or_else(denied)?;
        *headers = request.headers().clone();
        let authority = &request.url()[url::Position::BeforeHost..url::Position::AfterPort];
        headers.insert(
            hyper::header::HOST,
            hyper::header::HeaderValue::from_str(authority).map_err(|_| denied())?,
        );
        // A dedicated connection has no pooled work and no automatic retry.
        headers.insert(
            hyper::header::CONNECTION,
            hyper::header::HeaderValue::from_static("close"),
        );
        let outgoing = outgoing
            .body(Full::new(Bytes::copy_from_slice(&attempt.body)))
            .map_err(|_| denied())?;
        let mut outgoing = Some(outgoing);
        self.dispatch
            .prepare(&self.invocation, &attempt)
            .await
            .map_err(|_| denied())?;
        let mut response = None;
        self.dispatch
            .dispatch(&self.invocation, &attempt, &mut || {
                let request = outgoing.take().ok_or("HTTP attempt already submitted")?;
                // Hyper 1.8.1 client/conn/http1.rs calls dispatch.send(req)
                // BEFORE constructing its async response future; dispatch.rs
                // immediately sends the envelope to the ready connection.
                // Transfer happens HERE under the fence, not on a later poll.
                // The driver may physically write after Stop; that attempt has
                // already been transferred, so its outcome remains unknown.
                response = Some(sender.send_request(request));
                self.dispatched
                    .store(true, std::sync::atomic::Ordering::Release);
                Ok(())
            })
            .map_err(|_| denied())?;
        let response = response
            .ok_or_else(denied)?
            .await
            .map_err(|_| transport_failure(true))?;
        if response.status().is_success() {
            *self.completed.lock().map_err(|_| denied())? = Some(attempt);
        }
        // The response body owns the driver, so cancellation during SSE/body
        // decoding also closes the dedicated connection rather than leaking it.
        Ok(reqwest::Response::from(response.map(|body| {
            reqwest::Body::wrap(NativeBody {
                body,
                _driver: driver,
            })
        })))
    }
}

trait NativeIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> NativeIo for T {}

async fn connect(url: &url::Url) -> Result<Box<dyn NativeIo>, ProviderFailure> {
    let host = url
        .host_str()
        .ok_or_else(denied)?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = url.port_or_known_default().ok_or_else(denied)?;
    let tcp = tokio::net::TcpStream::connect((host, port))
        .await
        .map_err(|_| transport_failure(false))?;
    match url.scheme() {
        "http" => Ok(Box::new(tcp)),
        "https" => {
            let config = tilo_net::client_config().map_err(|_| transport_failure(false))?;
            let name = tokio_rustls::rustls::pki_types::ServerName::try_from(host.to_owned())
                .map_err(|_| denied())?;
            let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
                .connect(name, tcp)
                .await
                .map_err(|_| transport_failure(false))?;
            Ok(Box::new(tls))
        }
        _ => Err(denied()),
    }
}

fn transport_failure(submitted: bool) -> ProviderFailure {
    ProviderFailure {
        model_error: ModelError::new(ErrorKind::Transport),
        why: "native provider transport failed".into(),
        again: Again::Never,
        outcome_unknown: submitted,
        unprocessed: !submitted,
        status: None,
    }
}

// Keep connection ownership through response decoding and cancellation.
struct NativeBody {
    body: hyper::body::Incoming,
    _driver: Driver,
}

impl hyper::body::Body for NativeBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, hyper::Error>>> {
        Pin::new(&mut self.get_mut().body).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.body.size_hint()
    }
}

// Abort an idle/preparing connection when admission fails or the run future is
// cancelled. After response headers, NativeBody retains the same guard.
struct Driver(Option<tokio::task::JoinHandle<()>>);
impl Drop for Driver {
    fn drop(&mut self) {
        if let Some(driver) = self.0.take() {
            driver.abort();
        }
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
