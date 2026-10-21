//! Native text-client network I/O retains the host's original reviewed egress.
//! Endpoints are fixed; redirects, ambient proxies and HTTP retries are refused.
use super::{
    Arc, HttpClient, HttpRequest, HttpResponse, Network, Transport, TransportEvent,
    TransportFactory,
};
use crate::supervisor::AccountAuthority;
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use std::{future::Future, sync::Mutex, time::Duration};
use tokio::{net::TcpStream, task::JoinHandle};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};
use whatsapp_rust::{anyhow, async_channel, bytes::Bytes};

const HOST: &str = "web.whatsapp.com";
const SOCKET: &str = "wss://web.whatsapp.com/ws/chat";
const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_REQUEST: usize = 1024 * 1024;
const CAPACITY: usize = 32;
type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type Sink = SplitSink<Socket, Message>;
type Result<T> = anyhow::Result<T>;

fn unavailable() -> anyhow::Error {
    anyhow::anyhow!("reviewed messaging network access unavailable")
}
fn check(authority: &dyn AccountAuthority) -> Result<()> {
    authority.check().map_err(|_| unavailable())
}
async fn admitted<T>(
    authority: &dyn AccountAuthority,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::pin!(future);
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        check(authority)?;
        tokio::select! {
            result = &mut future => {
                check(authority)?;
                return result;
            }
            _ = tick.tick() => {}
        }
    }
}

struct Native {
    authority: Arc<dyn AccountAuthority>,
    http: reqwest::Client,
    tls: Arc<rustls::ClientConfig>,
    exchanges: tokio::sync::Semaphore,
}
impl Network {
    /// Construct native providers for the original approved account and egress.
    /// This performs no connection, login or permission change. The authority
    /// must retain both the account's original grant and protected egress consent.
    /// # Errors
    /// Refuses revoked authority or missing existing system certificate roots.
    pub fn native(
        authority: Arc<dyn AccountAuthority>,
    ) -> std::result::Result<Self, tilo_messaging::SendError> {
        let source = Native::new(authority).map_err(|_| tilo_messaging::SendError::Unavailable)?;
        let source = Arc::new(source);
        Ok(Self {
            http: source.clone(),
            transport: source,
        })
    }
}
impl Native {
    fn new(authority: Arc<dyn AccountAuthority>) -> Result<Self> {
        check(authority.as_ref())?;
        let certificates = rustls_native_certs::load_native_certs();
        check(authority.as_ref())?;
        let mut roots = rustls::RootCertStore::empty();
        for certificate in certificates.certs {
            roots.add(certificate).map_err(|_| unavailable())?;
        }
        if roots.is_empty() {
            return Err(unavailable());
        }
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| unavailable())?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let http = tilo_net::http::client()
            .tls_backend_preconfigured(tls.clone())
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| unavailable())?;
        check(authority.as_ref())?;
        Ok(Self {
            authority,
            http,
            tls: Arc::new(tls),
            exchanges: tokio::sync::Semaphore::new(8),
        })
    }
    async fn exchange(&self, request: HttpRequest) -> Result<HttpResponse> {
        if request.url.len() > 8192 || request.method.len() > 16 {
            return Err(unavailable());
        }
        let url = reqwest::Url::parse(&request.url).map_err(|_| unavailable())?;
        if url.scheme() != "https"
            || url.host_str() != Some(HOST)
            || url.port_or_known_default() != Some(443)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || request.headers.len() > 64
            || request
                .headers
                .iter()
                .any(|(k, v)| k.len() > 256 || v.len() > 8192)
            || request
                .body
                .as_ref()
                .is_some_and(|body| body.len() > MAX_REQUEST)
        {
            return Err(unavailable());
        }
        let method =
            reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| unavailable())?;
        let mut outgoing = self.http.request(method, url);
        for (name, value) in request.headers {
            // The reviewed endpoint alone chooses routing and TLS identity.
            if name.eq_ignore_ascii_case("host") {
                return Err(unavailable());
            }
            outgoing = outgoing.header(name, value);
        }
        if let Some(body) = request.body {
            outgoing = outgoing.body(body);
        }
        let mut incoming = outgoing.send().await.map_err(|_| unavailable())?;
        check(self.authority.as_ref())?;
        if incoming
            .content_length()
            .is_some_and(|length| length > MAX_BODY as u64)
        {
            return Err(unavailable());
        }
        let status_code = incoming.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = incoming.chunk().await.map_err(|_| unavailable())? {
            check(self.authority.as_ref())?;
            if chunk.len() > MAX_BODY.saturating_sub(body.len()) {
                return Err(unavailable());
            }
            body.extend_from_slice(&chunk);
        }
        check(self.authority.as_ref())?;
        Ok(HttpResponse { status_code, body })
    }
}

#[whatsapp_rust::async_trait]
impl HttpClient for Native {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse> {
        check(self.authority.as_ref())?;
        let _permit = self.exchanges.try_acquire().map_err(|_| unavailable())?;
        admitted(self.authority.as_ref(), self.exchange(request)).await
    }
}

struct Pipe {
    authority: Arc<dyn AccountAuthority>,
    sink: Arc<tokio::sync::Mutex<Sink>>,
    reader: Mutex<Option<JoinHandle<()>>>,
}
impl Drop for Pipe {
    fn drop(&mut self) {
        if let Ok(reader) = self.reader.get_mut()
            && let Some(reader) = reader.take()
        {
            reader.abort();
        }
    }
}
#[whatsapp_rust::async_trait]
impl Transport for Pipe {
    async fn send(&self, data: Bytes) -> Result<()> {
        if data.len() > MAX_BODY {
            return Err(unavailable());
        }
        admitted(self.authority.as_ref(), async {
            let mut sink = self.sink.lock().await;
            check(self.authority.as_ref())?;
            sink.send(Message::Binary(data))
                .await
                .map_err(|_| unavailable())
        })
        .await
    }
    async fn disconnect(&self) {
        let reader = self.reader.lock().ok().and_then(|mut reader| reader.take());
        if let Some(reader) = reader {
            reader.abort();
        }
        // Revocation permits teardown only, bounded even for an unresponsive peer.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            self.sink.lock().await.close().await
        })
        .await;
    }
}

#[whatsapp_rust::async_trait]
impl TransportFactory for Native {
    async fn create_transport(
        &self,
    ) -> Result<(Arc<dyn Transport>, async_channel::Receiver<TransportEvent>)> {
        check(self.authority.as_ref())?;
        let mut request = SOCKET.into_client_request().map_err(|_| unavailable())?;
        request.headers_mut().insert(
            "Origin",
            "https://web.whatsapp.com"
                .parse()
                .map_err(|_| unavailable())?,
        );
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_BODY))
            .max_frame_size(Some(MAX_BODY));
        let (socket, _) = Box::pin(admitted(self.authority.as_ref(), async {
            tokio::time::timeout(
                Duration::from_secs(15),
                tokio_tungstenite::connect_async_tls_with_config(
                    request,
                    Some(config),
                    false,
                    Some(Connector::Rustls(self.tls.clone())),
                ),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())
        }))
        .await?;
        check(self.authority.as_ref())?;
        Ok(pipe(socket, self.authority.clone()))
    }
}
fn pipe(
    socket: Socket,
    authority: Arc<dyn AccountAuthority>,
) -> (Arc<dyn Transport>, async_channel::Receiver<TransportEvent>) {
    let (sink, mut source) = socket.split();
    let (events, receiver) = async_channel::bounded(CAPACITY);
    let pipe = Arc::new(Pipe {
        authority: authority.clone(),
        sink: Arc::new(tokio::sync::Mutex::new(sink)),
        reader: Mutex::new(None),
    });
    let weak = Arc::downgrade(&pipe);
    let task = tokio::spawn(async move {
        if events.try_send(TransportEvent::Connected).is_err() {
            return;
        }
        loop {
            let next = admitted(authority.as_ref(), async { Ok(source.next().await) }).await;
            match next {
                Ok(Some(Ok(Message::Binary(data)))) if data.len() <= MAX_BODY => {
                    if events.try_send(TransportEvent::DataReceived(data)).is_err() {
                        break;
                    }
                }
                Ok(Some(Ok(Message::Ping(data)))) => {
                    let Some(pipe) = weak.upgrade() else {
                        break;
                    };
                    let reply = admitted(authority.as_ref(), async {
                        let mut sink = pipe.sink.lock().await;
                        check(authority.as_ref())?;
                        sink.send(Message::Pong(data))
                            .await
                            .map_err(|_| unavailable())
                    })
                    .await;
                    if reply.is_err() {
                        break;
                    }
                }
                Ok(Some(Ok(Message::Pong(_)))) => {}
                _ => break,
            }
        }
        let _ = events.try_send(TransportEvent::Disconnected(
            whatsapp_rust::transport::DisconnectReason::Unknown,
        ));
        events.close();
        if let Some(pipe) = weak.upgrade() {
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                pipe.sink.lock().await.close().await
            })
            .await;
        }
    });
    match pipe.reader.lock() {
        Ok(mut reader) => *reader = Some(task),
        Err(_) => task.abort(),
    }
    (pipe, receiver)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio_tungstenite::tungstenite::protocol::Role;
    struct Grant(AtomicBool);
    impl AccountAuthority for Grant {
        fn check(&self) -> std::result::Result<(), tilo_messaging::SendError> {
            if self.0.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(tilo_messaging::SendError::Revoked)
            }
        }
    }
    async fn sockets() -> (Socket, Socket) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let connecting = tokio::spawn(async move { TcpStream::connect(address).await.unwrap() });
        let (server, _) = listener.accept().await.unwrap();
        let client = connecting.await.unwrap();
        (
            WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client), Role::Client, None)
                .await,
            WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(server), Role::Server, None)
                .await,
        )
    }
    #[tokio::test]
    async fn native_pipe_revocation_stops_receives_and_new_writes_and_closes_owned_stream() {
        let (client, mut server) = sockets().await;
        let grant = Arc::new(Grant(AtomicBool::new(true)));
        let (transport, events) = pipe(client, grant.clone());
        assert!(matches!(
            events.recv().await.unwrap(),
            TransportEvent::Connected
        ));
        transport
            .send(Bytes::from_static(b"fixture encrypted frame"))
            .await
            .unwrap();
        assert_eq!(
            server.next().await.unwrap().unwrap(),
            Message::Binary(Bytes::from_static(b"fixture encrypted frame"))
        );
        grant.0.store(false, Ordering::Release);
        assert!(
            transport
                .send(Bytes::from_static(b"refused frame"))
                .await
                .is_err()
        );
        let next = tilo_test_env::within("the transport's disconnect", events.recv())
            .await
            .unwrap();
        assert!(matches!(next, TransportEvent::Disconnected(_)));
        assert!(events.recv().await.is_err());
        let closing = tilo_test_env::within("the server side to close", server.next()).await;
        assert!(matches!(closing, Some(Ok(Message::Close(_))) | None));
        transport.disconnect().await;
    }
}
