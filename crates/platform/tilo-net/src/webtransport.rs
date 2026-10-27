//! WebTransport sessions: web-transport-quinn on the desktop, the browser's
//! `WebTransport` in a page that has one.
//!
//! A session carries bidirectional and unidirectional streams and
//! unreliable datagrams to one `https:` URL. A server with a short-lived
//! self-signed certificate (development hosts, peers) is trusted by pinning
//! its SHA-256 in [`Options::cert_hashes`], exactly as a page's
//! `serverCertificateHashes`; otherwise the system trust bundle applies.

use bytes::Bytes;

use crate::NetError;

#[cfg(not(target_family = "wasm"))]
use crate::native::webtransport as backend;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
use crate::web::webtransport as backend;

/// How to open a session.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Options {
    /// Accept exactly the server certificates with these SHA-256 hashes
    /// instead of verifying against the system trust bundle.
    pub cert_hashes: Vec<[u8; 32]>,
}

impl Options {
    /// Pin the server certificate by its SHA-256.
    #[must_use]
    pub fn pinned(cert_hashes: Vec<[u8; 32]>) -> Self {
        Self { cert_hashes }
    }
}

/// Open a session to an `https:` URL.
///
/// # Errors
///
/// [`NetError::Unsupported`] in a browser without WebTransport,
/// [`NetError::InvalidUrl`], [`NetError::Connect`].
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

/// An open WebTransport session. Methods take `&self`, so one task can
/// accept streams while another opens them.
pub struct Session {
    inner: backend::Session,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").finish_non_exhaustive()
    }
}

impl Session {
    /// Open a bidirectional stream.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] once the session has ended.
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

    /// Wait for the server to open a bidirectional stream.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] once the session has ended.
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

    /// Open a unidirectional stream to the server.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] once the session has ended.
    #[allow(
        clippy::manual_async_fn,
        reason = "the written-out bound keeps the future `Send` on every target"
    )]
    pub fn open_uni(&self) -> impl Future<Output = Result<SendStream, NetError>> + Send + '_ {
        async move { Ok(SendStream(self.inner.open_uni().await?)) }
    }

    /// Wait for the server to open a unidirectional stream.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] once the session has ended.
    #[allow(
        clippy::manual_async_fn,
        reason = "the written-out bound keeps the future `Send` on every target"
    )]
    pub fn accept_uni(&self) -> impl Future<Output = Result<RecvStream, NetError>> + Send + '_ {
        async move { Ok(RecvStream(self.inner.accept_uni().await?)) }
    }

    /// Send one unreliable datagram.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] once the session has ended, and
    /// [`NetError::InvalidRequest`] for a datagram larger than the path allows.
    pub fn send_datagram(
        &self,
        data: Bytes,
    ) -> impl Future<Output = Result<(), NetError>> + Send + '_ {
        self.inner.send_datagram(data)
    }

    /// The next datagram from the server.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] once the session has ended.
    pub fn recv_datagram(&self) -> impl Future<Output = Result<Bytes, NetError>> + Send + '_ {
        self.inner.recv_datagram()
    }

    /// End the session with an application `code` and `reason`.
    pub fn close(&self, code: u32, reason: &str) {
        self.inner.close(code, reason);
    }
}

pub struct SendStream(backend::SendStream);

impl std::fmt::Debug for SendStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SendStream").finish_non_exhaustive()
    }
}

impl SendStream {
    /// Write all of `data`.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] when the peer stopped the stream or the session
    /// ended.
    pub fn write_all<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> impl Future<Output = Result<(), NetError>> + Send + 'a {
        self.0.write_all(data)
    }

    /// End the stream cleanly after what was written.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] when the stream had already ended.
    pub async fn finish(mut self) -> Result<(), NetError> {
        self.0.finish().await
    }
}

/// The receiving half of a stream.
pub struct RecvStream(backend::RecvStream);

impl std::fmt::Debug for RecvStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecvStream").finish_non_exhaustive()
    }
}

impl RecvStream {
    /// The next piece of the stream; `None` at its end.
    ///
    /// # Errors
    ///
    /// [`NetError::Closed`] when the peer reset the stream or the session
    /// ended.
    pub fn read(&mut self) -> impl Future<Output = Result<Option<Bytes>, NetError>> + Send + '_ {
        self.0.read()
    }

    /// The rest of the stream, refusing more than `limit` bytes.
    ///
    /// # Errors
    ///
    /// [`NetError::TooLarge`], and those of [`read`](Self::read).
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
