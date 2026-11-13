//! WebTransport sessions: web-transport-quinn on the desktop, the browser's
//! `WebTransport` in a page that has one. A session carries streams and
//! datagrams to one `https:` URL; a server with a self-signed certificate is
//! trusted by pinning its SHA-256 in [`Options::cert_hashes`].

use bytes::Bytes;

use crate::NetError;

#[cfg(not(target_family = "wasm"))]
use crate::native::webtransport as backend;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use crate::web::webtransport as backend;

#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// Accept exactly the server certificates with these SHA-256 hashes instead
    /// of verifying against the system trust bundle.
    pub cert_hashes: Vec<[u8; 32]>,
    /// The subprotocols to offer, most preferred first. A server picks one or
    /// answers none.
    pub protocols: Vec<String>,
}

impl Options {
    #[must_use]
    pub fn pinned(cert_hashes: Vec<[u8; 32]>) -> Self {
        Self {
            cert_hashes,
            protocols: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_protocols(mut self, protocols: Vec<String>) -> Self {
        self.protocols = protocols;
        self
    }
}

pub fn connect(
    url: &str,
    options: Options,
) -> impl Future<Output = Result<Session, NetError>> + Send + use<> {
    let url = url::Url::parse(url)
        .map_err(|error| NetError::InvalidUrl(format!("{url}: {error}")))
        .and_then(|url| {
            if url.scheme() == "https" {
                Ok(url)
            } else {
                Err(NetError::InvalidUrl(format!(
                    "{url}: a WebTransport URL starts with https:"
                )))
            }
        });
    async move {
        Ok(Session {
            inner: backend::connect(url?, options).await?,
        })
    }
}

/// Methods take `&self`, so one task can accept streams while another opens
/// them.
#[derive(Clone)]
pub struct Session {
    inner: backend::Session,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn from_backend(inner: backend::Session) -> Self {
        Self { inner }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the written-out bound keeps the future `Send` on every target"
    )]
    pub fn open_bi(
        &self,
    ) -> impl Future<Output = Result<(SendStream, RecvStream), NetError>> + Send + '_ {
        async move {
            let (send, recv) = self.inner.open_bi().await?;
            Ok((SendStream(send), RecvStream(recv)))
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the written-out bound keeps the future `Send` on every target"
    )]
    pub fn accept_bi(
        &self,
    ) -> impl Future<Output = Result<(SendStream, RecvStream), NetError>> + Send + '_ {
        async move {
            let (send, recv) = self.inner.accept_bi().await?;
            Ok((SendStream(send), RecvStream(recv)))
        }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the written-out bound keeps the future `Send` on every target"
    )]
    pub fn open_uni(&self) -> impl Future<Output = Result<SendStream, NetError>> + Send + '_ {
        async move { Ok(SendStream(self.inner.open_uni().await?)) }
    }

    #[allow(
        clippy::manual_async_fn,
        reason = "the written-out bound keeps the future `Send` on every target"
    )]
    pub fn accept_uni(&self) -> impl Future<Output = Result<RecvStream, NetError>> + Send + '_ {
        async move { Ok(RecvStream(self.inner.accept_uni().await?)) }
    }

    pub fn send_datagram(
        &self,
        data: Bytes,
    ) -> impl Future<Output = Result<(), NetError>> + Send + '_ {
        self.inner.send_datagram(data)
    }

    pub fn recv_datagram(&self) -> impl Future<Output = Result<Bytes, NetError>> + Send + '_ {
        self.inner.recv_datagram()
    }

    pub fn close(&self, code: u32, reason: &str) {
        self.inner.close(code, reason);
    }

    #[must_use]
    pub fn protocol(&self) -> Option<&str> {
        self.inner.protocol()
    }

    #[must_use]
    pub fn max_datagram_size(&self) -> usize {
        self.inner.max_datagram_size()
    }
}

impl web_transport_trait::poll::Session for Session {
    type SendStream = SendStream;
    type RecvStream = RecvStream;
    type Error = NetError;

    fn poll_accept_uni(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<RecvStream, NetError>> {
        self.inner
            .poll_accept_uni(cx)
            .map(|stream| stream.map(RecvStream))
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(SendStream, RecvStream), NetError>> {
        self.inner
            .poll_accept_bi(cx)
            .map(|pair| pair.map(|(send, recv)| (SendStream(send), RecvStream(recv))))
    }

    fn poll_open_uni(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<SendStream, NetError>> {
        self.inner
            .poll_open_uni(cx)
            .map(|stream| stream.map(SendStream))
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(SendStream, RecvStream), NetError>> {
        self.inner
            .poll_open_bi(cx)
            .map(|pair| pair.map(|(send, recv)| (SendStream(send), RecvStream(recv))))
    }

    fn poll_send_datagram(
        &mut self,
        cx: &mut std::task::Context<'_>,
        payload: &[u8],
    ) -> std::task::Poll<Result<(), NetError>> {
        self.inner.poll_send_datagram(cx, payload)
    }

    fn poll_recv_datagram(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<Bytes, NetError>> {
        self.inner.poll_recv_datagram(cx)
    }

    fn max_datagram_size(&self) -> usize {
        self.inner.max_datagram_size()
    }

    fn protocol(&self) -> Option<&str> {
        self.inner.protocol()
    }

    fn close(&mut self, code: u32, reason: &str) {
        self.inner.close(code, reason);
    }

    fn poll_closed(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<NetError> {
        self.inner.poll_closed(cx)
    }

    fn stats(&self) -> impl web_transport_trait::Stats {
        self.inner.stats()
    }
}

pub struct SendStream(backend::SendStream);

impl web_transport_trait::poll::SendStream for SendStream {
    type Error = NetError;

    fn poll_write(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, NetError>> {
        self.0.poll_write(cx, buf)
    }

    fn set_priority(&mut self, order: u8) {
        self.0.set_priority(order);
    }

    fn finish(&mut self) -> Result<(), NetError> {
        self.0.finish()
    }

    fn reset(&mut self, code: u32) {
        self.0.reset(code);
    }

    fn poll_closed(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), NetError>> {
        self.0.poll_closed(cx)
    }
}

impl std::fmt::Debug for SendStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendStream").finish_non_exhaustive()
    }
}

impl SendStream {
    pub fn write_all<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> impl Future<Output = Result<(), NetError>> + Send + 'a {
        self.0.write_all(data)
    }

    pub async fn finish(mut self) -> Result<(), NetError> {
        self.0.finish_stream().await
    }
}

pub struct RecvStream(backend::RecvStream);

impl web_transport_trait::poll::RecvStream for RecvStream {
    type Error = NetError;

    fn poll_read(
        &mut self,
        cx: &mut std::task::Context<'_>,
        dst: &mut [u8],
    ) -> std::task::Poll<Result<Option<usize>, NetError>> {
        self.0.poll_read(cx, dst)
    }

    fn stop(&mut self, code: u32) {
        self.0.stop(code);
    }

    fn poll_closed(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), NetError>> {
        self.0.poll_closed(cx)
    }
}

impl std::fmt::Debug for RecvStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecvStream").finish_non_exhaustive()
    }
}

impl RecvStream {
    pub fn read(&mut self) -> impl Future<Output = Result<Option<Bytes>, NetError>> + Send + '_ {
        self.0.read()
    }

    pub async fn read_to_end(mut self, limit: usize) -> Result<Vec<u8>, NetError> {
        let mut all = Vec::new();
        while let Some(chunk) = self.read().await? {
            if all.len().saturating_add(chunk.len()) > limit {
                return Err(NetError::TooLarge { limit });
            }
            all.extend_from_slice(&chunk);
        }
        Ok(all)
    }
}

impl tokio::io::AsyncWrite for SendStream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        web_transport_trait::poll::SendStream::poll_write(&mut *self, cx, buf)
            .map_err(std::io::Error::other)
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(
            web_transport_trait::poll::SendStream::finish(&mut *self)
                .map_err(std::io::Error::other),
        )
    }
}

impl tokio::io::AsyncRead for RecvStream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let unfilled = buf.initialize_unfilled();
        match web_transport_trait::poll::RecvStream::poll_read(&mut *self, cx, unfilled) {
            std::task::Poll::Ready(Ok(Some(read))) => {
                buf.advance(read);
                std::task::Poll::Ready(Ok(()))
            }
            // The peer finished the stream: no bytes is the end.
            std::task::Poll::Ready(Ok(None)) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Ready(Err(error)) => {
                std::task::Poll::Ready(Err(std::io::Error::other(error)))
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

#[cfg(not(target_family = "wasm"))]
pub use crate::native::webtransport::{Answer, Listener};
